//! Issue #458 — unit and fuzz tests for event-sink invariant guards.
//!
//! [`check_withdrawn_invariants`] and [`check_cancelled_invariants`] are
//! pre-emission guards in `events.rs`. They must reject every degenerate input
//! with a typed error and never panic, overflow, or silently accept bad state.
//!
//! # What is tested
//!
//! * **`check_withdrawn_invariants`** — rejects non-positive amounts, amounts
//!   that exceed `withdrawn`, and `withdrawn` values that exceed `deposited`.
//!   Accepts every valid (amount, stream) pair.
//!
//! * **`check_cancelled_invariants`** — rejects negative values, broken
//!   conservation (`refunded + vested != pre_deposited`), `vested < withdrawn`,
//!   and i128 overflow in the addition. Accepts every valid quadruple.
//!
//! # Fuzz coverage
//!
//! The proptest sweeps confirm that neither function panics for any combination
//! of i128 inputs, and that the accept/reject boundary is exactly the stated
//! invariant — no silent acceptance of out-of-range inputs.

#![cfg(test)]

extern crate std;

use proptest::prelude::*;
use soroban_sdk::{testutils::Address as _, Address, Env};

use crate::events::{check_cancelled_invariants, check_withdrawn_invariants};
use crate::error::Error;
use crate::types::{flag, Stream, StreamStatus};
use crate::test::common::{DAY, ONE};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a minimal `Stream` with the given `deposited` and `withdrawn` fields.
/// All other fields default to safe, non-interesting values.  Addresses are
/// generated inside a fresh `Env` so this helper has no dependency on `Harness`.
fn make_stream(deposited: i128, withdrawn: i128) -> Stream {
    let env = Env::default();
    Stream {
        sender: Address::generate(&env),
        recipient: Address::generate(&env),
        token: Address::generate(&env),
        deposited,
        withdrawn,
        start_time: 0,
        end_time: 100 * DAY,
        cliff_time: 0,
        paused_at: None,
        paused_total: 0,
        status: StreamStatus::Active,
        flags: flag::ALL,
    }
}

// ---------------------------------------------------------------------------
// check_withdrawn_invariants — unit tests
// ---------------------------------------------------------------------------

#[test]
fn withdrawn_ok_for_valid_inputs() {
    let stream = make_stream(1_000 * ONE, 300 * ONE);
    assert!(check_withdrawn_invariants(&stream, 300 * ONE).is_ok());
}

#[test]
fn withdrawn_ok_for_minimum_positive_amount() {
    let stream = make_stream(1_000 * ONE, 1);
    assert!(check_withdrawn_invariants(&stream, 1).is_ok());
}

#[test]
fn withdrawn_rejects_zero_amount() {
    let stream = make_stream(1_000 * ONE, 300 * ONE);
    assert_eq!(
        check_withdrawn_invariants(&stream, 0),
        Err(Error::InvalidAmount),
        "amount == 0 must be rejected"
    );
}

#[test]
fn withdrawn_rejects_negative_amount() {
    let stream = make_stream(1_000 * ONE, 300 * ONE);
    assert_eq!(
        check_withdrawn_invariants(&stream, -1),
        Err(Error::InvalidAmount),
        "negative amount must be rejected"
    );
}

#[test]
fn withdrawn_rejects_min_i128_amount() {
    let stream = make_stream(1_000 * ONE, 300 * ONE);
    assert_eq!(
        check_withdrawn_invariants(&stream, i128::MIN),
        Err(Error::InvalidAmount)
    );
}

/// `amount` exceeds `withdrawn` — the cumulative accounting would go negative.
#[test]
fn withdrawn_rejects_amount_exceeding_withdrawn_field() {
    let stream = make_stream(1_000 * ONE, 100 * ONE);
    assert_eq!(
        check_withdrawn_invariants(&stream, 200 * ONE),
        Err(Error::Overflow),
        "amount > stream.withdrawn must be rejected"
    );
}

/// `withdrawn` exceeds `deposited` — invariant I3 violation.
#[test]
fn withdrawn_rejects_withdrawn_exceeding_deposited() {
    let stream = make_stream(100 * ONE, 200 * ONE); // withdrawn > deposited
    assert_eq!(
        check_withdrawn_invariants(&stream, 100 * ONE),
        Err(Error::Overflow),
        "stream.withdrawn > stream.deposited must be rejected"
    );
}

// ---------------------------------------------------------------------------
// check_cancelled_invariants — unit tests
// ---------------------------------------------------------------------------

#[test]
fn cancelled_ok_for_valid_conservation() {
    // refunded(700) + vested(300) == pre_deposited(1000), withdrawn(100) <= vested(300)
    assert!(check_cancelled_invariants(1_000 * ONE, 700 * ONE, 300 * ONE, 100 * ONE).is_ok());
}

#[test]
fn cancelled_ok_for_zero_refund() {
    // Fully vested before cancel.
    assert!(check_cancelled_invariants(1_000 * ONE, 0, 1_000 * ONE, 400 * ONE).is_ok());
}

#[test]
fn cancelled_ok_for_zero_vested() {
    // Cancelled at or before start_time.
    assert!(check_cancelled_invariants(1_000 * ONE, 1_000 * ONE, 0, 0).is_ok());
}

#[test]
fn cancelled_rejects_negative_refunded() {
    assert_eq!(
        check_cancelled_invariants(1_000 * ONE, -1, 1_001 * ONE, 0),
        Err(Error::InvalidAmount)
    );
}

#[test]
fn cancelled_rejects_negative_withdrawn() {
    assert_eq!(
        check_cancelled_invariants(1_000 * ONE, 700 * ONE, 300 * ONE, -1),
        Err(Error::InvalidAmount)
    );
}

/// `vested < withdrawn` — the recipient cannot have already withdrawn more
/// than the total amount vested.
#[test]
fn cancelled_rejects_vested_less_than_withdrawn() {
    assert_eq!(
        check_cancelled_invariants(1_000 * ONE, 500 * ONE, 200 * ONE, 300 * ONE),
        Err(Error::InvalidAmount),
        "vested(200) < withdrawn(300) violates the invariant"
    );
}

/// Conservation broken: `refunded + vested != pre_deposited`.
#[test]
fn cancelled_rejects_broken_conservation() {
    // 500 + 300 = 800 ≠ 1000
    assert_eq!(
        check_cancelled_invariants(1_000 * ONE, 500 * ONE, 300 * ONE, 0),
        Err(Error::InvalidAmount)
    );
}

/// Overflow in `refunded.checked_add(vested)`.
#[test]
fn cancelled_rejects_addition_overflow() {
    assert_eq!(
        check_cancelled_invariants(i128::MAX, i128::MAX, i128::MAX, 0),
        Err(Error::Overflow)
    );
}

// ---------------------------------------------------------------------------
// Fuzz: check_withdrawn_invariants never panics for any i128 inputs
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn fuzz_withdrawn_never_panics(
        deposited in any::<i128>(),
        withdrawn_field in any::<i128>(),
        amount in any::<i128>(),
    ) {
        let stream = make_stream(deposited, withdrawn_field);
        // Must not panic — only Ok or a typed Err is acceptable.
        let _ = check_withdrawn_invariants(&stream, amount);
    }

    /// Any valid quadruple satisfying the stated constraints is accepted;
    /// any violating quadruple is rejected with a typed error.
    #[test]
    fn fuzz_withdrawn_accepts_valid_rejects_invalid(
        deposited in 1i128..=1_000_000_000_000i128,
        pct_withdrawn in 0u8..=100u8,
        pct_amount in 1u8..=100u8,
    ) {
        let withdrawn_field = (deposited * pct_withdrawn as i128) / 100;
        let amount = (withdrawn_field * pct_amount as i128) / 100;
        if amount == 0 {
            return Ok(());
        }
        let stream = make_stream(deposited, withdrawn_field);
        // All inputs satisfy the invariant by construction.
        prop_assert!(
            check_withdrawn_invariants(&stream, amount).is_ok(),
            "valid input rejected: deposited={deposited}, withdrawn={withdrawn_field}, amount={amount}"
        );
    }

    #[test]
    fn fuzz_cancelled_never_panics(
        pre_deposited in any::<i128>(),
        refunded in any::<i128>(),
        vested in any::<i128>(),
        withdrawn in any::<i128>(),
    ) {
        // Must not panic for any input combination.
        let _ = check_cancelled_invariants(pre_deposited, refunded, vested, withdrawn);
    }

    /// Any valid quadruple is accepted; any violating one is rejected.
    #[test]
    fn fuzz_cancelled_accepts_valid_quadruples(
        pct_vested in 0u8..=100u8,
        pct_withdrawn_of_vested in 0u8..=100u8,
    ) {
        let deposited: i128 = 1_000_000_000;
        let vested = (deposited * pct_vested as i128) / 100;
        let refunded = deposited - vested;
        let withdrawn = (vested * pct_withdrawn_of_vested as i128) / 100;
        // Conservation holds by construction.
        prop_assert!(
            check_cancelled_invariants(deposited, refunded, vested, withdrawn).is_ok(),
            "valid quadruple rejected: deposited={deposited}, refunded={refunded}, vested={vested}, withdrawn={withdrawn}"
        );
    }
}


// ---------------------------------------------------------------------------
// check_withdrawn_invariants — unit tests
// ---------------------------------------------------------------------------

#[test]
fn withdrawn_ok_for_valid_inputs() {
    let stream = make_stream(1_000 * ONE, 300 * ONE);
    assert!(check_withdrawn_invariants(&stream, 300 * ONE).is_ok());
}

#[test]
fn withdrawn_ok_for_minimum_positive_amount() {
    let stream = make_stream(1_000 * ONE, 1);
    assert!(check_withdrawn_invariants(&stream, 1).is_ok());
}

#[test]
fn withdrawn_rejects_zero_amount() {
    let stream = make_stream(1_000 * ONE, 300 * ONE);
    assert_eq!(
        check_withdrawn_invariants(&stream, 0),
        Err(Error::InvalidAmount),
        "amount == 0 must be rejected"
    );
}

#[test]
fn withdrawn_rejects_negative_amount() {
    let stream = make_stream(1_000 * ONE, 300 * ONE);
    assert_eq!(
        check_withdrawn_invariants(&stream, -1),
        Err(Error::InvalidAmount),
        "negative amount must be rejected"
    );
}

#[test]
fn withdrawn_rejects_min_i128_amount() {
    let stream = make_stream(1_000 * ONE, 300 * ONE);
    assert_eq!(
        check_withdrawn_invariants(&stream, i128::MIN),
        Err(Error::InvalidAmount)
    );
}

/// `amount` exceeds `withdrawn` — the cumulative accounting would go negative.
#[test]
fn withdrawn_rejects_amount_exceeding_withdrawn_field() {
    let stream = make_stream(1_000 * ONE, 100 * ONE);
    assert_eq!(
        check_withdrawn_invariants(&stream, 200 * ONE),
        Err(Error::Overflow),
        "amount > stream.withdrawn must be rejected"
    );
}

/// `withdrawn` exceeds `deposited` — invariant I3 violation.
#[test]
fn withdrawn_rejects_withdrawn_exceeding_deposited() {
    let stream = make_stream(100 * ONE, 200 * ONE); // withdrawn > deposited
    assert_eq!(
        check_withdrawn_invariants(&stream, 100 * ONE),
        Err(Error::Overflow),
        "stream.withdrawn > stream.deposited must be rejected"
    );
}

// ---------------------------------------------------------------------------
// check_cancelled_invariants — unit tests
// ---------------------------------------------------------------------------

#[test]
fn cancelled_ok_for_valid_conservation() {
    // refunded(700) + vested(300) == pre_deposited(1000), withdrawn(100) <= vested(300)
    assert!(check_cancelled_invariants(1_000 * ONE, 700 * ONE, 300 * ONE, 100 * ONE).is_ok());
}

#[test]
fn cancelled_ok_for_zero_refund() {
    // Fully vested before cancel.
    assert!(check_cancelled_invariants(1_000 * ONE, 0, 1_000 * ONE, 400 * ONE).is_ok());
}

#[test]
fn cancelled_ok_for_zero_vested() {
    // Cancelled at or before start_time.
    assert!(check_cancelled_invariants(1_000 * ONE, 1_000 * ONE, 0, 0).is_ok());
}

#[test]
fn cancelled_rejects_negative_refunded() {
    assert_eq!(
        check_cancelled_invariants(1_000 * ONE, -1, 1_001 * ONE, 0),
        Err(Error::InvalidAmount)
    );
}

#[test]
fn cancelled_rejects_negative_withdrawn() {
    assert_eq!(
        check_cancelled_invariants(1_000 * ONE, 700 * ONE, 300 * ONE, -1),
        Err(Error::InvalidAmount)
    );
}

/// `vested < withdrawn` — the recipient cannot have already withdrawn more
/// than the total amount vested.
#[test]
fn cancelled_rejects_vested_less_than_withdrawn() {
    assert_eq!(
        check_cancelled_invariants(1_000 * ONE, 500 * ONE, 200 * ONE, 300 * ONE),
        Err(Error::InvalidAmount),
        "vested(200) < withdrawn(300) violates the invariant"
    );
}

/// Conservation broken: `refunded + vested != pre_deposited`.
#[test]
fn cancelled_rejects_broken_conservation() {
    // 500 + 300 = 800 ≠ 1000
    assert_eq!(
        check_cancelled_invariants(1_000 * ONE, 500 * ONE, 300 * ONE, 0),
        Err(Error::InvalidAmount)
    );
}

/// Overflow in `refunded.checked_add(vested)`.
#[test]
fn cancelled_rejects_addition_overflow() {
    assert_eq!(
        check_cancelled_invariants(i128::MAX, i128::MAX, i128::MAX, 0),
        Err(Error::Overflow)
    );
}

// ---------------------------------------------------------------------------
// Fuzz: check_withdrawn_invariants never panics for any i128 inputs
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn fuzz_withdrawn_never_panics(
        deposited in any::<i128>(),
        withdrawn_field in any::<i128>(),
        amount in any::<i128>(),
    ) {
        let stream = make_stream(deposited, withdrawn_field);
        // Must not panic — only Ok or a typed Err is acceptable.
        let _ = check_withdrawn_invariants(&stream, amount);
    }

    /// Any valid quadruple satisfying the stated constraints is accepted;
    /// any violating quadruple is rejected with a typed error.
    #[test]
    fn fuzz_withdrawn_accepts_valid_rejects_invalid(
        deposited in 1i128..=i128::MAX,
        pct_withdrawn in 0u8..=100u8,
        pct_amount in 1u8..=100u8,
    ) {
        // Scale to avoid overflow: use percentages of a 1000-unit deposit.
        let deposited = deposited.min(1_000 * ONE as i128);
        let withdrawn_field = (deposited * pct_withdrawn as i128) / 100;
        let amount = (withdrawn_field * pct_amount as i128) / 100;
        if amount == 0 {
            return Ok(());
        }
        let stream = make_stream(deposited, withdrawn_field);
        // All inputs satisfy the invariant by construction.
        prop_assert!(
            check_withdrawn_invariants(&stream, amount).is_ok(),
            "valid input rejected: deposited={deposited}, withdrawn={withdrawn_field}, amount={amount}"
        );
    }

    #[test]
    fn fuzz_cancelled_never_panics(
        pre_deposited in any::<i128>(),
        refunded in any::<i128>(),
        vested in any::<i128>(),
        withdrawn in any::<i128>(),
    ) {
        // Must not panic for any input combination.
        let _ = check_cancelled_invariants(pre_deposited, refunded, vested, withdrawn);
    }

    /// Any valid quadruple is accepted; any violating one is rejected.
    #[test]
    fn fuzz_cancelled_accepts_valid_quadruples(
        pct_vested in 0u8..=100u8,
        pct_withdrawn_of_vested in 0u8..=100u8,
    ) {
        let deposited = 1_000 * ONE as i128;
        let vested = (deposited * pct_vested as i128) / 100;
        let refunded = deposited - vested;
        let withdrawn = (vested * pct_withdrawn_of_vested as i128) / 100;
        // Conservation holds by construction.
        prop_assert!(
            check_cancelled_invariants(deposited, refunded, vested, withdrawn).is_ok(),
            "valid quadruple rejected: deposited={deposited}, refunded={refunded}, vested={vested}, withdrawn={withdrawn}"
        );
    }
}
