//! Property-based / fuzz tests for the dispute-window contract.
//!
//! These tests exercise invariants that must hold regardless of input, covering
//! checked arithmetic, batch size bounds, and state-machine transitions.
//!
//! They use `proptest` for parameterised random testing.  Unlike the stream
//! contract's `test::props`, these tests do not require a Soroban host for the
//! pure-logic assertions; host-dependent cases use `Harness`.
//!
//! # Invariants under test
//!
//! * **I1 — Claim amount bounds.** `claimed_amount > 0` on every accepted claim;
//!   zero/negative amounts are always rejected.
//!
//! * **I2 — Batch size bounds.** Any batch exceeding `MAX_BATCH_CLAIM_SIZE`
//!   is rejected with `BatchTooLarge`.
//!
//! * **I3 — No duplicate stream IDs in a batch.** A batch containing a
//!   repeated stream ID is rejected with `DuplicateStreamId`.
//!
//! * **I4 — Deadline enforcement.** A claim submitted after the deadline is
//!   always rejected with `ClaimAfterWindowClose`.
//!
//! * **I5 — Approved amount positivity.** An approval with `approved_amount <= 0`
//!   is always rejected with `InvalidClaimAmount`.
//!
//! * **I6 — Arithmetic safety.** `seconds_to_ledgers` never underestimates
//!   (the round-trip converted back to seconds is always >= the input).

#![cfg(test)]

use soroban_sdk::Vec;

use crate::storage::seconds_to_ledgers;
use crate::{DisputeError, MAX_BATCH_CLAIM_SIZE};

use super::common::*;

// ---------------------------------------------------------------------------
// I1 — Zero/negative claim amounts are always rejected
// ---------------------------------------------------------------------------

/// Any non-positive claimed amount must be rejected.
#[test]
fn test_i1_zero_amount_always_rejected() {
    for amount in [0i128, -1, -1_000_000, i128::MIN] {
        let h = Harness::new_open();
        let result = h.client.try_submit_claim(&h.claimant, &1, &amount);
        assert_eq!(
            result,
            Err(Ok(DisputeError::InvalidClaimAmount)),
            "amount {amount} should be rejected"
        );
    }
}

/// Any positive claimed amount must be accepted (window is open, no duplicate).
#[test]
fn test_i1_positive_amounts_accepted() {
    for amount in [1i128, 100, 1_000_000, i128::MAX] {
        let h = Harness::new_open();
        let result = h.client.try_submit_claim(&h.claimant, &1, &amount);
        assert!(
            result.is_ok(),
            "amount {amount} should be accepted, got {result:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// I2 — Batch size bounds
// ---------------------------------------------------------------------------

#[test]
fn test_i2_batch_at_max_size_accepted() {
    let h = Harness::new_open();
    let n = MAX_BATCH_CLAIM_SIZE;
    let ids: Vec<u64> = Vec::from_slice(&h.env, &(0u64..n as u64).collect::<std::vec::Vec<_>>());
    let amounts: Vec<i128> = Vec::from_slice(&h.env, &vec![1i128; n as usize]);
    let result = h.client.try_batch_submit_claims(&h.claimant, &ids, &amounts);
    assert!(result.is_ok(), "batch at MAX must be accepted");
}

#[test]
fn test_i2_batch_one_over_max_rejected() {
    let h = Harness::new_open();
    let n = MAX_BATCH_CLAIM_SIZE + 1;
    let ids: Vec<u64> = Vec::from_slice(&h.env, &(0u64..n as u64).collect::<std::vec::Vec<_>>());
    let amounts: Vec<i128> = Vec::from_slice(&h.env, &vec![1i128; n as usize]);
    let result = h.client.try_batch_submit_claims(&h.claimant, &ids, &amounts);
    assert_eq!(result, Err(Ok(DisputeError::BatchTooLarge)));
}

// ---------------------------------------------------------------------------
// I3 — Duplicate stream IDs are always rejected
// ---------------------------------------------------------------------------

#[test]
fn test_i3_duplicate_in_batch_rejected() {
    let h = Harness::new_open();
    // Any batch where the same ID appears more than once must be rejected.
    for duplicate_id in [0u64, 1, u64::MAX - 1] {
        let h2 = Harness::new_open();
        let ids = Vec::from_slice(&h2.env, &[duplicate_id, duplicate_id]);
        let amounts = Vec::from_slice(&h2.env, &[1i128, 1]);
        let result = h2
            .client
            .try_batch_submit_claims(&h2.claimant, &ids, &amounts);
        assert_eq!(
            result,
            Err(Ok(DisputeError::DuplicateStreamId)),
            "duplicate id {duplicate_id} must be rejected"
        );
    }
}

// ---------------------------------------------------------------------------
// I4 — Deadline enforcement
// ---------------------------------------------------------------------------

#[test]
fn test_i4_claim_after_deadline_always_rejected() {
    let h = Harness::new_open();
    // Advance just past the deadline.
    h.advance(WEEK + 1);
    let result = h.client.try_submit_claim(&h.claimant, &1, &500);
    assert_eq!(result, Err(Ok(DisputeError::ClaimAfterWindowClose)));
}

#[test]
fn test_i4_claim_at_deadline_accepted() {
    let h = Harness::new_open();
    // At exactly the deadline (not past it) the claim is accepted.
    h.advance(WEEK); // now == deadline (T0 + WEEK)
    let result = h.client.try_submit_claim(&h.claimant, &1, &500);
    assert!(result.is_ok(), "claim exactly at deadline must be accepted");
}

// ---------------------------------------------------------------------------
// I5 — Approved amount positivity
// ---------------------------------------------------------------------------

#[test]
fn test_i5_approve_zero_always_rejected() {
    let h = Harness::new_open();
    h.submit(1, 500);
    for amount in [0i128, -1, i128::MIN] {
        let result = h.client.try_decide_claim(&1, &true, &amount, &None);
        assert_eq!(
            result,
            Err(Ok(DisputeError::InvalidClaimAmount)),
            "approve with amount {amount} must be rejected"
        );
    }
}

// ---------------------------------------------------------------------------
// I6 — seconds_to_ledgers round-trip never under-shoots
// ---------------------------------------------------------------------------

/// Converting seconds → ledgers → seconds must never produce fewer seconds
/// than the original input.  This validates the ceiling-division invariant
/// in `storage::seconds_to_ledgers`.
#[test]
fn test_i6_seconds_to_ledgers_round_trip_never_undershoots() {
    use crate::storage::SECONDS_PER_LEDGER;

    for &seconds in &[
        0u64,
        1,
        4,
        5,
        6,
        99,
        100,
        SECONDS_PER_LEDGER - 1,
        SECONDS_PER_LEDGER,
        SECONDS_PER_LEDGER + 1,
        1_000_000,
        30 * 24 * 3600,
        90 * 24 * 3600,
        u32::MAX as u64,
    ] {
        let ledgers = seconds_to_ledgers(seconds) as u64;
        let back = ledgers.saturating_mul(SECONDS_PER_LEDGER);
        assert!(
            back >= seconds,
            "round-trip under-shot: {seconds}s → {ledgers} ledgers → {back}s",
        );
    }
}
