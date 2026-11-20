//! Issue #406 — Strict invariant assertions for cliff-vesting to protect
//! against re-entrancy and state desync.
//!
//! # What these tests cover
//!
//! Soroban does not have cross-contract re-entrancy in the EVM sense. What
//! *can* happen is state desync: a state-changing call that commits a partial
//! write (e.g. `deposited` updated but `status` not, or `paused_at` set but
//! `status` still `Active`) leaving the stream logically inconsistent.
//!
//! Each test below asserts one or more of the six formal invariants defined in
//! `accrual.rs` after every potentially vulnerable operation.  The harness
//! method `assert_invariants()` checks I1 + I4 + I5 + schedule sanity; the
//! monotonicity helper `assert_no_vested_regression` covers I3; the
//! `assert_conservation` helper from `test::invariants` covers I4 precisely.

use super::common::*;
use crate::{accrual, Error, StreamStatus};

// ---------------------------------------------------------------------------
// I1 — Bounds: 0 <= withdrawn <= vested(t) <= deposited
// ---------------------------------------------------------------------------

/// At no point in a full create → accrue → cliff → withdraw → deplete lifecycle
/// may `withdrawn` exceed `vested`, and `vested` may never exceed `deposited`.
#[test]
fn i1_withdrawn_never_exceeds_vested_across_full_lifecycle() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 60 * DAY;
    let end = start + 180 * DAY;
    let deposit = 1_800 * ONE;
    let id = h.create(deposit, start, end, cliff, true, true, true);

    let assert_i1 = |now: u64, label: &str| {
        let stream = h.get(id);
        let vested = accrual::vested(&stream, now).expect("vested must not overflow");
        assert!(stream.withdrawn >= 0, "I1 ({label}): withdrawn is negative");
        assert!(stream.deposited >= 0, "I1 ({label}): deposited is negative");
        assert!(
            stream.withdrawn <= vested,
            "I1 ({label}) violated at t={now}: withdrawn({}) > vested({})",
            stream.withdrawn,
            vested
        );
        assert!(
            vested <= stream.deposited,
            "I1 ({label}) violated at t={now}: vested({}) > deposited({})",
            vested,
            stream.deposited
        );
    };

    h.warp_to(cliff - 1);
    assert_i1(h.now(), "pre-cliff");

    h.warp_to(cliff);
    assert_i1(h.now(), "at cliff");
    h.client.withdraw(&id, &Some(400 * ONE));
    assert_i1(h.now(), "after partial cliff withdraw");

    h.warp_to(cliff + 60 * DAY);
    assert_i1(h.now(), "post-cliff mid");
    h.client.withdraw(&id, &None);
    assert_i1(h.now(), "after post-cliff withdraw");

    h.warp_to(end);
    assert_i1(h.now(), "at maturity");
    h.client.withdraw(&id, &None);
    assert_i1(h.now(), "after final withdraw");
    h.assert_pool_exact();
}

/// After cancellation with a prior partial withdrawal, `withdrawn` must still
/// be bounded above by `vested`.
#[test]
fn i1_holds_after_cancel_with_prior_partial_withdrawal() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 50 * DAY;
    let deposit = 1_000 * ONE;
    let id = h.create(deposit, start, start + 100 * DAY, cliff, true, true, true);

    h.warp_to(cliff);
    h.client.withdraw(&id, &Some(200 * ONE));

    h.advance(10 * DAY);
    h.client.cancel(&id);

    let stream = h.get(id);
    let now = h.now();
    let vested = accrual::vested(&stream, now).expect("no overflow post-cancel");
    assert!(
        stream.withdrawn <= vested,
        "I1 post-cancel: withdrawn({}) > vested({})",
        stream.withdrawn,
        vested
    );
    assert!(
        vested <= stream.deposited,
        "I1 post-cancel: vested({}) > deposited({})",
        vested,
        stream.deposited
    );
    h.assert_pool_exact();
}

// ---------------------------------------------------------------------------
// I4 — Conservation: vested(t) + refundable(t) == deposited
// ---------------------------------------------------------------------------

/// Conservation holds at every ledger across the cliff transition.
/// The cliff gate must not create or destroy value.
#[test]
fn i4_conservation_holds_across_cliff_transition() {
    use crate::test::invariants::assert_conservation;

    let h = Harness::new();
    let start = h.now();
    let cliff = start + 100 * DAY;
    let end = start + 400 * DAY;
    let deposit = 1_200 * ONE;
    let id = h.create(deposit, start, end, cliff, true, true, true);

    for now in [
        start,
        cliff - 1,
        cliff,
        cliff + 1,
        cliff + 100 * DAY,
        end - 1,
        end,
        end + YEAR,
    ] {
        h.warp_to(now);
        assert_conservation(&h.get(id), h.now());
    }
    h.assert_pool_exact();
}

/// Conservation holds through a pause-across-cliff lifecycle.
#[test]
fn i4_conservation_holds_through_pause_across_cliff() {
    use crate::test::invariants::assert_conservation;

    let h = Harness::new();
    let start = h.now();
    let cliff = start + 60 * DAY;
    let deposit = 900 * ONE;
    let id = h.create(deposit, start, start + 180 * DAY, cliff, true, true, true);

    h.advance(40 * DAY);
    h.client.pause(&id);
    assert_conservation(&h.get(id), h.now());

    h.advance(50 * DAY); // wall clock crosses cliff while paused
    assert_conservation(&h.get(id), h.now());

    h.client.resume(&id);
    assert_conservation(&h.get(id), h.now());

    h.advance(20 * DAY); // stream clock now at 60 days → cliff opens
    assert_conservation(&h.get(id), h.now());

    h.client.withdraw(&id, &None);
    assert_conservation(&h.get(id), h.now());
    h.assert_pool_exact();
}

/// A top-up must not reduce `vested(t)` for the same timestamp (I3) while
/// maintaining conservation (I4).
#[test]
fn i3_i4_top_up_preserves_vested_and_conservation() {
    use crate::test::invariants::assert_conservation;

    let h = Harness::new();
    let start = h.now();
    let cliff = start + 50 * DAY;
    let deposit = 500 * ONE;
    let id = h.create(deposit, start, start + 100 * DAY, cliff, true, true, true);

    h.warp_to(cliff);
    let now = h.now();

    let stream_before = h.get(id);
    let vested_before = accrual::vested(&stream_before, now).expect("no overflow");
    assert_conservation(&stream_before, now);

    h.client.top_up(&id, &(100 * ONE));

    let stream_after = h.get(id);
    let vested_after = accrual::vested(&stream_after, now).expect("no overflow");
    assert!(
        vested_after >= vested_before,
        "I3 violated by top_up: vested went from {} to {} at t={}",
        vested_before,
        vested_after,
        now
    );
    assert_conservation(&stream_after, now);
    h.assert_pool_exact();
}

// ---------------------------------------------------------------------------
// I5 — Pause coherence: paused_at ⟺ status == Paused
// ---------------------------------------------------------------------------

/// Every state-changing operation must leave `paused_at` and `status` in
/// agreement — no "half-paused" state where one is set and the other is not.
#[test]
fn i5_pause_coherence_across_all_state_transitions() {
    let check_i5 = |stream: &crate::Stream, label: &str| match stream.status {
        StreamStatus::Paused => assert!(
            stream.paused_at.is_some(),
            "I5 ({label}): status=Paused but paused_at=None"
        ),
        other => assert!(
            stream.paused_at.is_none(),
            "I5 ({label}): status={other:?} but paused_at={:?}",
            stream.paused_at
        ),
    };

    let h = Harness::new();
    let start = h.now();
    let cliff = start + 50 * DAY;
    let id = h.create(1_000 * ONE, start, start + 100 * DAY, cliff, true, true, true);

    check_i5(&h.get(id), "after create");

    h.advance(30 * DAY);
    h.client.pause(&id);
    check_i5(&h.get(id), "after pause");

    h.advance(10 * DAY);
    h.client.resume(&id);
    check_i5(&h.get(id), "after resume");

    h.warp_to(cliff);
    check_i5(&h.get(id), "at cliff before withdraw");
    h.client.withdraw(&id, &None);
    check_i5(&h.get(id), "after cliff withdraw");

    h.client.pause(&id);
    check_i5(&h.get(id), "after post-cliff pause");

    // Cancel while paused must clear paused_at.
    h.advance(5 * DAY);
    h.client.cancel(&id);
    check_i5(&h.get(id), "after cancel-while-paused");
    assert_eq!(
        h.get(id).paused_at,
        None,
        "cancel must clear paused_at unconditionally"
    );
    h.assert_pool_exact();
}

/// Depletion of a paused stream must close out the pause: `paused_at` must
/// be `None` on a `Depleted` stream.
#[test]
fn i5_depletion_clears_paused_at() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);
    h.warp_to(T0 + 100 * DAY);

    h.client.pause(&id);
    assert!(h.get(id).paused_at.is_some());

    h.client.withdraw(&id, &None);
    let s = h.get(id);
    assert_eq!(s.status, StreamStatus::Depleted);
    assert_eq!(
        s.paused_at,
        None,
        "I5: Depleted stream must not retain paused_at"
    );
    h.assert_pool_exact();
}

// ---------------------------------------------------------------------------
// I6 — Zero dust at settlement
// ---------------------------------------------------------------------------

/// At and after `end_time`, `vested == deposited` and `refundable == 0`
/// exactly, with no dust remaining in the pool.
#[test]
fn i6_zero_dust_at_settlement_across_multiple_deposit_sizes() {
    for &deposit in &[1 * ONE, 1_000 * ONE, 1_000_007i128, 999_999_999i128] {
        let h = Harness::new();
        let start = h.now();
        let duration = 101 * DAY; // prime multiple — maximises truncation residue
        let end = start + duration;
        let id = h.create(deposit, start, end, start, true, true, true);

        h.advance(37 * DAY);
        let available = h.client.withdrawable_of(&id);
        if available > 0 {
            h.client.withdraw(&id, &Some(available));
        }

        h.warp_to(end);
        assert_eq!(
            h.client.vested_of(&id),
            deposit,
            "I6: vested must equal deposited at settlement (deposit={deposit})"
        );
        assert_eq!(
            h.client.refundable_of(&id),
            0,
            "I6: refundable must be zero at settlement (deposit={deposit})"
        );
        h.client.withdraw(&id, &None);
        h.assert_pool_exact();
        assert_eq!(h.pool(), 0, "I6: pool empty after full withdrawal (deposit={deposit})");
    }
}

// ---------------------------------------------------------------------------
// State desync guards
// ---------------------------------------------------------------------------

/// An attempted withdraw with an explicit amount of zero must be rejected;
/// the stream state must remain unchanged.
#[test]
fn zero_amount_withdraw_is_rejected_state_unchanged() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 50 * DAY;
    let id = h.create(500 * ONE, start, start + 100 * DAY, cliff, true, true, true);

    h.warp_to(cliff);
    let before = h.get(id);

    let err = h.client.try_withdraw(&id, &Some(0)).unwrap_err().unwrap();
    assert_eq!(err, Error::InvalidAmount);
    assert_eq!(h.get(id), before, "stream state must be unchanged after rejected withdrawal");
    h.assert_pool_exact();
}

/// An explicit withdraw amount exceeding the withdrawable balance must be
/// rejected; no partial transfer must occur.
#[test]
fn overdraft_withdraw_is_rejected_no_partial_transfer() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 100 * DAY;
    let deposit = 1_000 * ONE;
    let id = h.create(deposit, start, start + 200 * DAY, cliff, true, true, true);

    h.warp_to(cliff);
    let available = h.client.withdrawable_of(&id);
    assert_eq!(available, 500 * ONE);

    let err = h
        .client
        .try_withdraw(&id, &Some(available + 1))
        .unwrap_err()
        .unwrap();
    assert_eq!(err, Error::InsufficientWithdrawable);
    assert_eq!(h.balance(&h.recipient), 0, "no tokens must have moved");
    h.assert_pool_exact();
}

/// Non-pausable streams must never enter the `Paused` state through any
/// call sequence; `paused_at` must always be `None`.
#[test]
fn non_pausable_stream_never_enters_paused_state() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 50 * DAY;
    let id = h.create(600 * ONE, start, start + 120 * DAY, cliff, true, false, true);

    for t in [start, cliff - 1, cliff, cliff + 10 * DAY] {
        h.warp_to(t);
        let err = h.client.try_pause(&id).unwrap_err().unwrap();
        assert_eq!(err, Error::NotPausable, "must be NotPausable at t={t}");
        let s = h.get(id);
        assert_ne!(s.status, StreamStatus::Paused, "status must not be Paused at t={t}");
        assert_eq!(s.paused_at, None, "paused_at must remain None at t={t}");
    }
    h.assert_pool_exact();
}

/// A cliff-vesting stream with `cancellable == false` must reject every cancel
/// attempt, leaving the stream state unmodified.
#[test]
fn non_cancellable_stream_never_transitions_to_cancelled() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 60 * DAY;
    let id = h.create(900 * ONE, start, start + 180 * DAY, cliff, false, true, true);

    for t in [start, cliff - 1, cliff, cliff + 30 * DAY] {
        h.warp_to(t);
        let before = h.get(id);
        let err = h.client.try_cancel(&id).unwrap_err().unwrap();
        assert_eq!(err, Error::NotCancellable, "must be NotCancellable at t={t}");
        assert_eq!(h.get(id), before, "stream state unchanged after rejected cancel at t={t}");
    }
    h.assert_pool_exact();
}

/// Schedule sanity: `end_time >= start_time` must hold after every mutation.
/// A cancel that collapses the schedule must set `end_time = start_time`, never
/// invert them.
#[test]
fn schedule_never_inverts_across_mutations() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 50 * DAY;
    let deposit = 1_000 * ONE;
    let id = h.create(deposit, start, start + 100 * DAY, cliff, true, true, true);

    // Cancel at start (pre-cliff, zero vested) collapses to start==end.
    h.client.cancel(&id);
    let s = h.get(id);
    assert!(
        s.end_time >= s.start_time,
        "schedule inverted after early cancel: start={} end={}",
        s.start_time,
        s.end_time
    );
    h.assert_pool_exact();
}

/// A sequence of pause → resume → cancel must leave all invariants intact;
/// particularly I3 (vested must not decrease) must hold at the fixed
/// pre-cancel timestamp.
#[test]
fn i3_vested_does_not_decrease_across_pause_resume_cancel_sequence() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 40 * DAY;
    let deposit = 800 * ONE;
    let id = h.create(deposit, start, start + 200 * DAY, cliff, true, true, true);

    // Accrue past the cliff.
    h.warp_to(cliff + 20 * DAY);
    let snapshot_time = h.now();

    // Record vested before the sequence.
    let before = h.vested_snapshot();

    // Pause (clock freezes — I3: vested at snapshot_time must not decrease).
    h.client.pause(&id);
    h.assert_no_vested_regression(&before, "after pause");

    // Resume.
    h.client.resume(&id);
    h.assert_no_vested_regression(&before, "after resume");

    // Cancel — settles at current stream time.
    h.client.cancel(&id);
    // After cancel at snapshot_time (no time advanced since snapshot), the
    // settled vested must equal the recorded figure.
    let stream = h.get(id);
    let vested_post_cancel = accrual::vested(&stream, snapshot_time).expect("no overflow");
    let vested_before_cancel = before[id as usize];
    assert!(
        vested_post_cancel >= vested_before_cancel,
        "I3 violated by cancel: vested at snapshot went from {} to {}",
        vested_before_cancel,
        vested_post_cancel
    );
    h.assert_pool_exact();
}

/// A batch withdraw on two streams where one is at the cliff boundary and one
/// is pre-cliff must pay the first and skip the second without corrupting either.
#[test]
fn batch_withdraw_at_cliff_boundary_does_not_corrupt_pre_cliff_stream() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 100 * DAY;
    let end = start + 200 * DAY;
    let deposit = 1_000 * ONE;

    let id_at_cliff = h.create(deposit, start, end, cliff, true, true, true);
    let id_pre_cliff = h.create(deposit, start, end, cliff + 50 * DAY, true, true, true);

    h.warp_to(cliff);

    // Batch withdraw: id_at_cliff has 500 ONE available; id_pre_cliff has 0.
    let total = h.client.batch_withdraw(&h.recipient, &h.ids(&[id_at_cliff, id_pre_cliff]));
    assert_eq!(total, 500 * ONE, "only the cliff-opened stream contributes");

    // Pre-cliff stream is unmodified.
    assert_eq!(
        h.client.withdrawable_of(&id_pre_cliff),
        0,
        "pre-cliff stream must still have nothing withdrawable"
    );
    assert_eq!(
        h.get(id_pre_cliff).withdrawn,
        0,
        "pre-cliff stream withdrawn must remain zero"
    );
    h.assert_pool_exact();
}

/// A transfer of recipient on a cliff-vesting stream must preserve all
/// invariants; the new recipient must be able to withdraw at the cliff.
#[test]
fn recipient_transfer_does_not_break_cliff_invariants() {
    use crate::test::invariants::assert_conservation;

    let h = Harness::new();
    let start = h.now();
    let cliff = start + 60 * DAY;
    let deposit = 600 * ONE;
    let id = h.create(deposit, start, start + 120 * DAY, cliff, true, true, true);

    // Transfer before the cliff opens.
    h.advance(30 * DAY);
    h.client.transfer_recipient(&id, &h.other);
    assert_conservation(&h.get(id), h.now());

    // New recipient claims at the cliff.
    h.warp_to(cliff);
    let vested = h.client.vested_of(&id);
    assert_eq!(vested, 300 * ONE, "half the deposit vested at the cliff");
    // The new recipient (h.other) needs to auth. We use mock_all_auths so this succeeds.
    // Update the client's withdraw call: the stream's recipient is now h.other.
    // We call with the client directly (all auths mocked).
    let payout = h.client.withdraw(&id, &None);
    assert_eq!(payout, 300 * ONE);
    assert_conservation(&h.get(id), h.now());
    h.assert_pool_exact();
}
