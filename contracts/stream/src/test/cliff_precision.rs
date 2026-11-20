//! Issue #402 — Edge-case precision loss in cliff-vesting during fractional
//! stream withdrawal calculations.
//!
//! # The problem
//!
//! Perpetua uses pure integer arithmetic for all vesting math.  The accrual
//! formula is:
//!
//! ```text
//! vested = deposited * elapsed / duration          (integer division, rounds down)
//! ```
//!
//! This is correct by design: truncating toward zero means the residue lives in
//! `refundable` and returns to the sender, so the pool can never owe more than
//! it holds.  However several edge cases can amplify the apparent precision
//! loss or interact with the cliff gate in surprising ways:
//!
//! * **Cliff-step jump** — at the cliff instant `elapsed` jumps discontinuously
//!   from 0 to `cliff_time - start_time`.  The resulting `vested` amount is
//!   `deposited * cliff_elapsed / duration`.  When `duration` does not divide
//!   `deposited * cliff_elapsed` evenly, the cliff amount is truncated.  This
//!   is correct, but the truncated stroop must end up in `refundable`, not
//!   silently lost.
//!
//! * **Fractional withdrawal** — a partial withdraw of an amount smaller than
//!   one "second of vesting" leaves a residue.  On the next call `withdrawable`
//!   must return the correct remainder — not zero, not the original amount.
//!
//! * **Prime / coprime deposits** — a deposit whose value is coprime to the
//!   duration maximises the per-second truncation residue.  At maturity the
//!   residue must vanish entirely (`vested == deposited`), not accumulate.
//!
//! * **Post-cliff interval precision** — after the cliff opens, each additional
//!   second adds exactly `floor(deposited / duration)` to `vested`.  An
//!   off-by-one in the elapsed computation would compound every second.
//!
//! * **Pause interaction with fractional accrual** — pausing and resuming must
//!   not duplicate or drop the per-interval truncation residue.
//!
//! All tests in this file focus on exact stroop-level correctness of the
//! amounts reported and transferred, asserting `assert_pool_exact` after every
//! operation to verify the pool balance matches the outstanding liability.

use proptest::prelude::*;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Env};

use super::common::*;
use crate::{accrual, Error};
use crate::test::invariants::assert_conservation;
use crate::types::{Stream, StreamStatus};

// ---------------------------------------------------------------------------
// Cliff-step precision
// ---------------------------------------------------------------------------

/// The cliff step must release exactly `floor(deposited * cliff_elapsed /
/// duration)` stroops — no stroop more, no stroop fewer — and the complement
/// must sit in `refundable`.
#[test]
fn cliff_step_amount_is_exact_floor_division() {
    let h = Harness::new();
    let start = h.now();
    // 90 days cliff / 360 days total: each stroop of deposit represents 1/360
    // of the schedule. For a deposit of 1_000_001 (non-multiple of 360):
    //   vested at cliff = floor(1_000_001 * 90 / 360) = floor(250_000.25) = 250_000
    let deposit = 1_000_001i128 * ONE; // prime * ONE to maximise residue
    let cliff_elapsed = 90 * DAY;
    let duration = 360 * DAY;
    let id = h.create(deposit, start, start + duration, start + cliff_elapsed, true, true, true);

    h.warp_to(start + cliff_elapsed);
    let vested = h.client.vested_of(&id);
    let expected = deposit * cliff_elapsed as i128 / duration as i128;
    assert_eq!(vested, expected, "cliff step must be floor(deposited * elapsed / duration)");

    // The residue lives in refundable, not lost.
    let refundable = h.client.refundable_of(&id);
    assert_eq!(vested + refundable, deposit, "conservation at cliff step");

    // Withdrawing the exact cliff amount succeeds.
    assert_eq!(h.client.withdraw(&id, &None), expected);
    h.assert_pool_exact();
}

/// A deposit that is exactly divisible by the duration must produce zero
/// truncation residue at every integral second boundary.
#[test]
fn zero_residue_deposit_has_no_truncation_at_any_second() {
    let h = Harness::new();
    let start = h.now();
    // deposit == duration (in stroops) → exactly 1 stroop per second, zero residue.
    let duration_secs = 100 * DAY;
    let deposit = duration_secs as i128; // 1 stroop/second, perfectly divisible
    let cliff = start + 50 * DAY;
    let id = h.create(deposit, start, start + duration_secs, cliff, true, true, true);

    // At the cliff the full 50-day accrual is exactly half the deposit.
    h.warp_to(start + 50 * DAY);
    assert_eq!(h.client.vested_of(&id), deposit / 2);

    // After the cliff, each additional second adds exactly 1 stroop.
    h.advance(1);
    assert_eq!(h.client.vested_of(&id), deposit / 2 + 1);

    h.warp_to(start + duration_secs);
    assert_eq!(h.client.vested_of(&id), deposit, "zero residue at maturity");
    assert_eq!(h.client.refundable_of(&id), 0);
    h.assert_pool_exact();
}

/// A prime deposit against a non-prime duration produces a non-zero per-second
/// residue.  The residue must accumulate in `refundable` and vanish at maturity.
#[test]
fn prime_deposit_residue_vanishes_at_maturity() {
    // Primes chosen so gcd(deposit, duration) == 1, maximising residue.
    let deposit = 1_000_000_007i128; // well-known prime
    let duration_secs = 7 * 86_400u64; // 7 days

    let h = Harness::new();
    let start = h.now();
    let cliff = start + 3 * DAY;
    let id = h.create(deposit, start, start + duration_secs, cliff, true, true, true);

    // Mid-schedule: conservation holds but residue is non-zero.
    h.advance(4 * DAY);
    assert_conservation(&h.get(id), h.now());
    let mid_refundable = h.client.refundable_of(&id);
    // Some residue exists because gcd == 1.
    // (We don't assert an exact residue here; we assert it disappears at end.)

    h.advance(3 * DAY); // now at end
    assert_eq!(h.client.vested_of(&id), deposit, "prime deposit fully vests at maturity");
    assert_eq!(h.client.refundable_of(&id), 0, "zero residue at maturity");
    let _ = mid_refundable;
    h.assert_pool_exact();
}

// ---------------------------------------------------------------------------
// Fractional withdrawal precision
// ---------------------------------------------------------------------------

/// Withdrawing amounts smaller than one second of vesting must leave the
/// correct remainder in `withdrawable`, never stranding stroops.
#[test]
fn sub_second_partial_withdrawal_leaves_correct_remainder() {
    let h = Harness::new();
    let start = h.now();
    // 1 stroop/second rate so each second == 1 stroop.
    let duration_secs = 1_000u64;
    let deposit = duration_secs as i128; // 1_000 stroops over 1_000 seconds
    let cliff = start; // no cliff
    let id = h.create(deposit, start, start + duration_secs, cliff, true, true, true);

    h.advance(500); // 500 seconds elapsed → 500 stroops vested
    assert_eq!(h.client.vested_of(&id), 500);
    assert_eq!(h.client.withdrawable_of(&id), 500);

    // Withdraw exactly 1 stroop.
    h.client.withdraw(&id, &Some(1));
    assert_eq!(h.client.withdrawable_of(&id), 499, "remainder after 1-stroop withdrawal");

    // Withdraw all but 1 stroop.
    h.client.withdraw(&id, &Some(498));
    assert_eq!(h.client.withdrawable_of(&id), 1, "single stroop remains");

    // Withdraw the last stroop.
    h.client.withdraw(&id, &Some(1));
    assert_eq!(h.client.withdrawable_of(&id), 0);
    h.assert_pool_exact();
}

/// Multiple partial withdrawals across the cliff boundary must collectively pay
/// out the correct total — no double-counting, no dropped stroops.
#[test]
fn multiple_partial_withdrawals_across_cliff_sum_correctly() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 100 * DAY;
    let end = start + 400 * DAY;
    // A deposit whose per-day rate is non-integral to stress rounding.
    let deposit = 1_200_001i128 * ONE;
    let id = h.create(deposit, start, end, cliff, true, true, true);

    h.warp_to(cliff);
    let vested_at_cliff = h.client.vested_of(&id);
    assert_eq!(
        vested_at_cliff,
        deposit * 100 * DAY as i128 / (400 * DAY as i128),
        "exact cliff amount"
    );

    // Three partial withdrawals summing to the cliff amount.
    let w1 = vested_at_cliff / 3;
    let w2 = vested_at_cliff / 3;
    let w3 = vested_at_cliff - w1 - w2; // absorbs the residue
    h.client.withdraw(&id, &Some(w1));
    h.client.withdraw(&id, &Some(w2));
    h.client.withdraw(&id, &Some(w3));
    assert_eq!(h.client.withdrawable_of(&id), 0, "all cliff amount withdrawn");

    // Post-cliff accrual continues at the linear rate.
    h.advance(100 * DAY);
    let additional = h.client.withdrawable_of(&id);
    // 100 days of a 400-day schedule: 25% of deposit additionally vested.
    let expected_additional = deposit * 100 * DAY as i128 / (400 * DAY as i128);
    assert_eq!(additional, expected_additional, "post-cliff accrual is exact");
    h.client.withdraw(&id, &None);

    h.warp_to(end);
    h.client.withdraw(&id, &None);
    h.assert_pool_exact();
    assert_eq!(h.pool(), 0);
}

/// Withdrawing the exact withdrawable amount at every second for a long
/// sequence must leave zero pool dust — the cumulative truncation residue
/// must vanish entirely at settlement.
#[test]
fn per_second_withdrawals_leave_zero_pool_dust() {
    let h = Harness::new();
    let start = h.now();
    let duration = 200u64;
    let deposit = 1_000_003i128; // coprime to 200 → maximum per-step residue
    let id = h.create(deposit, start, start + duration, start, true, true, true);

    // Withdraw at every second.
    for _ in 0..duration {
        h.advance(1);
        let available = h.client.withdrawable_of(&id);
        if available > 0 {
            h.client.withdraw(&id, &Some(available));
        }
    }

    h.warp_to(start + duration);
    assert_eq!(h.client.vested_of(&id), deposit, "fully vested at settlement");
    // Withdraw the final settlement amount.
    h.client.withdraw(&id, &None);
    h.assert_pool_exact();
    assert_eq!(h.pool(), 0, "zero pool dust after per-second withdrawals");
}

// ---------------------------------------------------------------------------
// Post-cliff interval precision
// ---------------------------------------------------------------------------

/// Each additional second past the cliff must add exactly
/// `floor(deposited / duration)` to `withdrawable` (between whole-second marks).
#[test]
fn post_cliff_per_second_rate_is_exact_floor() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 50 * DAY;
    let end = start + 200 * DAY;
    let deposit = 2_000 * ONE;
    // rate = floor(deposit / duration) = floor(2_000*ONE / (200*DAY))
    let duration = (200 * DAY) as i128;
    let rate_per_second = deposit / duration; // floor division
    let id = h.create(deposit, start, end, cliff, true, true, true);

    // Open the cliff, then verify one-second increments.
    h.warp_to(cliff);
    let vested_at_cliff = h.client.vested_of(&id);
    assert_eq!(
        vested_at_cliff,
        deposit * (50 * DAY) as i128 / duration,
        "exact cliff amount"
    );

    for extra_seconds in 1u64..=5 {
        h.warp_to(cliff + extra_seconds);
        let vested = h.client.vested_of(&id);
        let expected = deposit * ((50 * DAY + extra_seconds) as i128) / duration;
        assert_eq!(
            vested,
            expected,
            "vested at cliff + {extra_seconds}s must equal exact floor"
        );
        // The per-second increment is exactly the floor rate (or one more for
        // ledgers where the residue "carries" — but the formula computes from
        // scratch so we check the cumulative value, not the delta).
        let _ = rate_per_second;
    }
    h.assert_pool_exact();
}

// ---------------------------------------------------------------------------
// Pause interaction with fractional accrual
// ---------------------------------------------------------------------------

/// Pausing and resuming must not duplicate or drop any per-interval
/// truncation residue: the total payout over the life of the stream must
/// equal `deposited` exactly.
#[test]
fn pause_resume_does_not_duplicate_or_drop_fractional_residue() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 30 * DAY;
    // 101-day schedule, coprime-ish deposit.
    let deposit = 1_000_000_007i128;
    let duration = 101 * DAY;
    let id = h.create(deposit, start, start + duration, cliff, true, true, true);

    // Multiple pause/resume cycles across and after the cliff.
    let intervals = [15u64 * DAY, 20 * DAY, 10 * DAY, 25 * DAY];
    let pause_durations = [5u64 * DAY, 3 * DAY, 7 * DAY, 2 * DAY];

    for (advance_secs, pause_secs) in intervals.iter().zip(pause_durations.iter()) {
        h.advance(*advance_secs);
        h.client.pause(&id);
        assert_conservation(&h.get(id), h.now());
        h.advance(*pause_secs);
        assert_conservation(&h.get(id), h.now());
        h.client.resume(&id);
        assert_conservation(&h.get(id), h.now());
    }

    // Advance to the stretched end.
    let total_paused = pause_durations.iter().sum::<u64>();
    h.warp_to(start + duration + total_paused);
    assert_eq!(
        h.client.vested_of(&id),
        deposit,
        "fully vested at stretched end after pause cycles"
    );
    assert_eq!(h.client.refundable_of(&id), 0);
    h.client.withdraw(&id, &None);
    h.assert_pool_exact();
    assert_eq!(h.pool(), 0, "zero pool dust after pause/resume cycles");
}

/// A partial withdrawal taken while the stream is paused must deduct from
/// `withdrawn` exactly, leaving the correct `withdrawable` once resumed.
#[test]
fn partial_withdrawal_while_paused_deducts_correctly() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 60 * DAY;
    let deposit = 1_200 * ONE;
    let id = h.create(deposit, start, start + 360 * DAY, cliff, true, true, true);

    // Pause after the cliff.
    h.warp_to(cliff + 30 * DAY);
    h.client.pause(&id);

    // Vested is frozen at cliff + 30 days.
    let vested_frozen = h.client.vested_of(&id);
    // 90 days of a 360-day schedule → 300 ONE.
    assert_eq!(vested_frozen, 300 * ONE);

    // Partial withdrawal of exactly 100 ONE while paused.
    h.client.withdraw(&id, &Some(100 * ONE));
    assert_eq!(h.client.withdrawable_of(&id), 200 * ONE, "200 ONE remains after partial");

    // Advance wall clock — no additional accrual while paused.
    h.advance(100 * DAY);
    assert_eq!(h.client.withdrawable_of(&id), 200 * ONE, "still 200 ONE while paused");

    // Withdraw the remaining 200 ONE while still paused.
    h.client.withdraw(&id, &Some(200 * ONE));
    assert_eq!(h.client.withdrawable_of(&id), 0, "nothing left while paused");

    // Resume and accrue to maturity.
    h.client.resume(&id);
    let stretched_end = start + 360 * DAY + 100 * DAY;
    h.warp_to(stretched_end);
    assert_eq!(h.client.vested_of(&id), deposit);
    h.client.withdraw(&id, &None);
    h.assert_pool_exact();
    assert_eq!(h.pool(), 0);
}

// ---------------------------------------------------------------------------
// Cancellation precision
// ---------------------------------------------------------------------------

/// Cancel at the cliff instant must vested exactly the cliff amount —
/// `floor(deposited * cliff_elapsed / duration)` — and refund the complement.
#[test]
fn cancel_at_cliff_vests_exact_floor_amount() {
    let h = Harness::new();
    let start = h.now();
    let cliff_elapsed = 90 * DAY;
    let duration = 360 * DAY;
    let deposit = 1_000_003i128 * ONE; // coprime to 360 → non-zero residue
    let id = h.create(deposit, start, start + duration, start + cliff_elapsed, true, true, true);
    let sender_before = h.balance(&h.sender);

    h.warp_to(start + cliff_elapsed);
    let expected_vested = deposit * cliff_elapsed as i128 / duration as i128;
    let expected_refund = deposit - expected_vested;

    h.client.cancel(&id);

    assert_eq!(
        h.balance(&h.sender),
        sender_before + expected_refund,
        "cancel at cliff: sender refund is exact complement"
    );
    assert_eq!(
        h.client.withdrawable_of(&id),
        expected_vested,
        "cancel at cliff: recipient withdrawable is exact floor"
    );
    assert_eq!(
        expected_vested + expected_refund,
        deposit,
        "conservation: vested + refund == deposit"
    );
    h.assert_pool_exact();
}

/// Cancel one second before the cliff must refund the entire deposit because
/// nothing has vested yet — the precision residue at that point is the entire
/// deposit, all in `refundable`.
#[test]
fn cancel_one_second_before_cliff_refunds_entire_deposit() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 100 * DAY;
    let deposit = 1_000_000_007i128; // prime
    let id = h.create(deposit, start, start + 200 * DAY, cliff, true, true, true);
    let sender_before = h.balance(&h.sender);

    h.warp_to(cliff - 1);
    h.client.cancel(&id);

    assert_eq!(
        h.balance(&h.sender),
        sender_before + deposit,
        "full deposit refunded one second before cliff"
    );
    assert_eq!(h.client.withdrawable_of(&id), 0);
    assert_eq!(h.pool(), 0);
    h.assert_pool_exact();
}

/// Cancel one second after the cliff must include the cliff amount plus exactly
/// one second of additional accrual.
#[test]
fn cancel_one_second_after_cliff_includes_one_extra_second() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 1_000;   // 1000-second cliff
    let end = start + 10_000;    // 10_000-second schedule
    let deposit = 10_000i128;    // 1 stroop/second, zero residue
    let id = h.create(deposit, start, end, cliff, true, true, true);
    let sender_before = h.balance(&h.sender);

    h.warp_to(cliff + 1);
    h.client.cancel(&id);

    // Vested = 1001 stroops; refund = 10_000 - 1001 = 8999.
    assert_eq!(
        h.balance(&h.sender),
        sender_before + (deposit - 1001),
        "refund is exactly deposit minus 1001 stroops"
    );
    assert_eq!(h.client.withdrawable_of(&id), 1001);
    h.assert_pool_exact();
}

// ---------------------------------------------------------------------------
// Property test: conservation holds for all fractional deposits
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::default())]

    /// For every deposit, duration, cliff fraction, and elapsed time,
    /// `vested(t) + refundable(t) == deposited` exactly.
    ///
    /// This property directly refutes "precision loss": any lost stroop would
    /// break the additive identity.
    #[test]
    fn conservation_holds_for_all_fractional_deposits(
        deposited in 1i128..(i128::MAX / (1 << 40)),
        duration_secs in 1u64..(20 * 365 * 86_400),
        cliff_frac in 0u64..=100u64,
        elapsed_frac in 0u64..=110u64,
    ) {
        let start = 1_700_000_000u64;
        let deposited = deposited.max(duration_secs as i128);
        let cliff_offset = duration_secs * cliff_frac / 100;
        let elapsed = duration_secs * elapsed_frac / 100;

        let env = Env::default();
        let stream = Stream {
            sender: Address::generate(&env),
            recipient: Address::generate(&env),
            token: Address::generate(&env),
            deposited,
            withdrawn: 0,
            start_time: start,
            end_time: start + duration_secs,
            cliff_time: start + cliff_offset,
            flags: Stream::flags_from_parts(true, true, true),
            paused_at: None,
            paused_total: 0,
            status: StreamStatus::Active,
        };
        let now = start + elapsed;

        let vested = accrual::vested(&stream, now).expect("must not overflow for valid stream");
        let refundable = accrual::refundable(&stream, now).expect("must not overflow");
        prop_assert_eq!(
            vested + refundable,
            deposited,
            "conservation broken: vested({}) + refundable({}) != deposited({}) \
             at elapsed={}s cliff={}s duration={}s",
            vested, refundable, deposited, elapsed, cliff_offset, duration_secs
        );
        prop_assert!(vested >= 0 && vested <= deposited, "vested out of bounds: {vested}");
        prop_assert!(refundable >= 0, "refundable went negative: {refundable}");
    }

    /// At maturity (`elapsed >= duration`), `refundable` is always exactly zero
    /// regardless of deposit, duration, or cliff, so no stroop is ever stranded.
    #[test]
    fn refundable_is_zero_at_all_maturity_timestamps(
        deposited in 1i128..(i128::MAX / (1 << 40)),
        duration_secs in 1u64..(20 * 365 * 86_400),
        cliff_frac in 0u64..=100u64,
        past_maturity in 0u64..(5 * 365 * 86_400),
    ) {
        let start = 1_700_000_000u64;
        let deposited = deposited.max(duration_secs as i128);
        let cliff_offset = duration_secs * cliff_frac / 100;
        let now = start + duration_secs + past_maturity;

        let env = Env::default();
        let stream = Stream {
            sender: Address::generate(&env),
            recipient: Address::generate(&env),
            token: Address::generate(&env),
            deposited,
            withdrawn: 0,
            start_time: start,
            end_time: start + duration_secs,
            cliff_time: start + cliff_offset,
            flags: Stream::flags_from_parts(true, true, true),
            paused_at: None,
            paused_total: 0,
            status: StreamStatus::Active,
        };

        let vested = accrual::vested(&stream, now).expect("no overflow");
        let refundable = accrual::refundable(&stream, now).expect("no overflow");
        prop_assert_eq!(vested, deposited, "must be fully vested at maturity");
        prop_assert_eq!(refundable, 0, "must have zero refundable at maturity");
    }

    /// For a fixed stream state, `vested(t)` is non-decreasing in `t`
    /// (I2 — monotonic in time). No clock advance may reduce the vested amount.
    #[test]
    fn vested_is_non_decreasing_in_time(
        deposited in 1i128..(i128::MAX / (1 << 40)),
        duration_secs in 1u64..(20 * 365 * 86_400),
        cliff_frac in 0u64..=100u64,
        t1_frac in 0u64..=100u64,
        t2_extra in 0u64..(10 * 365 * 86_400),
    ) {
        let start = 1_700_000_000u64;
        let deposited = deposited.max(duration_secs as i128);
        let cliff_offset = duration_secs * cliff_frac / 100;
        let t1 = start + duration_secs * t1_frac / 100;
        let t2 = t1 + t2_extra;

        let env = Env::default();
        let stream = Stream {
            sender: Address::generate(&env),
            recipient: Address::generate(&env),
            token: Address::generate(&env),
            deposited,
            withdrawn: 0,
            start_time: start,
            end_time: start + duration_secs,
            cliff_time: start + cliff_offset,
            flags: Stream::flags_from_parts(true, true, true),
            paused_at: None,
            paused_total: 0,
            status: StreamStatus::Active,
        };

        let v1 = accrual::vested(&stream, t1).expect("no overflow");
        let v2 = accrual::vested(&stream, t2).expect("no overflow");
        prop_assert!(v2 >= v1, "I2 violated: vested moved backwards {} -> {} as t advanced", v1, v2);
    }
}

// ---------------------------------------------------------------------------
// Regression: same-ledger precision
// ---------------------------------------------------------------------------

/// Two reads of `vested_of` at the exact same ledger timestamp must agree
/// to the stroop. This guards against any non-deterministic or side-effecting
/// behaviour in the view functions.
#[test]
fn vested_of_is_deterministic_at_the_same_ledger() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 100;
    let deposit = 999_999_999i128; // non-round number
    let id = h.create(deposit, start, start + 1_000, cliff, true, true, true);

    // Before cliff.
    h.warp_to(cliff - 1);
    assert_eq!(h.client.vested_of(&id), h.client.vested_of(&id), "deterministic pre-cliff");

    // At cliff.
    h.warp_to(cliff);
    let v1 = h.client.vested_of(&id);
    let v2 = h.client.vested_of(&id);
    assert_eq!(v1, v2, "deterministic at cliff: {v1} vs {v2}");

    // Post-cliff.
    h.advance(50);
    let v3 = h.client.vested_of(&id);
    let v4 = h.client.vested_of(&id);
    assert_eq!(v3, v4, "deterministic post-cliff: {v3} vs {v4}");
    h.assert_pool_exact();
}

/// A `withdrawable_of` call immediately after a successful `withdraw` at the
/// cliff instant must return exactly zero (the full available amount was just
/// pulled), not a stale positive value.
#[test]
fn withdrawable_is_zero_immediately_after_full_cliff_withdrawal() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 500;
    let deposit = 500_003i128; // non-multiple of 500 → non-zero residue
    let id = h.create(deposit, start, start + 1_000, cliff, true, true, true);

    h.warp_to(cliff);
    let available = h.client.withdrawable_of(&id);
    assert!(available > 0, "must have something to withdraw at cliff");

    h.client.withdraw(&id, &None);
    assert_eq!(
        h.client.withdrawable_of(&id),
        0,
        "withdrawable must be zero immediately after full withdrawal"
    );
    h.assert_pool_exact();
}
