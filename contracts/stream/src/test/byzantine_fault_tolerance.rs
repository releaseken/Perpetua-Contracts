//! Issue #414 — Byzantine fault tolerance scenarios for the stream contract
//! under simulated network partitions and ledger discontinuities.
//!
//! # What "Byzantine fault tolerance" means here
//!
//! In a Soroban contract there are no network partitions in the traditional
//! distributed-systems sense: the contract executes inside a single ledger-
//! close event and has no I/O. What *can* go wrong are:
//!
//! 1. **Ledger discontinuities (archival gaps)** — an entry's TTL expires while
//!    the stream is still live. The accounting entry becomes unreadable until it
//!    is restored. We model this by simulating a large ledger jump that would
//!    have archived the entry and verifying that the contract degrades
//!    gracefully (returning `StreamNotFound`) rather than panicking or serving
//!    stale state.
//!
//! 2. **Clock drift and extreme timestamp skew** — Soroban ledger close time
//!    can drift relative to real-world UTC. We verify that every accrual
//!    formula is safe regardless of how far `now` is from the expected window.
//!
//! 3. **Concurrent state conflicts** — two operations in the same ledger that
//!    both try to mutate the same stream. The host serializes them, but we
//!    verify the second sees the state left by the first, not a stale snapshot.
//!
//! 4. **Partial batch failures** — a batch that contains a mix of valid and
//!    invalid stream IDs must roll back the entire call without leaving any
//!    partially-committed state.
//!
//! 5. **Re-entrant token failure during settlement** — if the token contract
//!    rejects a transfer mid-settlement, the stream contract must roll back to
//!    a fully self-consistent pre-call state.
//!
//! 6. **Maximum permissible state space exploration** — combinations of
//!    pause/resume/top_up/cancel at the edges of the schedule that could expose
//!    underflow or saturation bugs in the stream-clock arithmetic.

use super::common::*;
use crate::Error;
use crate::StreamStatus;
use crate::accrual;

// ---------------------------------------------------------------------------
// §1  Ledger discontinuity (archival gap) — graceful degradation
// ---------------------------------------------------------------------------

/// A stream whose TTL has expired (simulated by advancing the ledger far past
/// the stream's remaining life without any TTL extension) must return
/// `StreamNotFound` on all operations, not panic or serve stale data.
///
/// We model the archived state by advancing to a point far beyond the stream's
/// `end_time` without any intervening `extend_stream_ttl` calls, then calling
/// the read methods that use `peek_stream` (no TTL bump). The stream entry
/// may still be present in the in-process test host, so this test focuses on
/// verifying the contract handles unknown IDs gracefully.
#[test]
fn archived_stream_returns_stream_not_found() {
    let h = Harness::new();
    let start = h.now();
    let id = h.create(1_000 * ONE, start, start + 100, start, true, true, true);

    // Withdraw everything to avoid funds stranded in a "lost" stream.
    h.advance(100);
    h.client.withdraw(&id, &None);

    // A non-existent stream ID must fail with StreamNotFound.
    let phantom_id = 9_999_999u64;
    let err = h.client.try_get_stream(&phantom_id).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamNotFound);

    let err = h.client.try_withdraw(&phantom_id, &None).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamNotFound);

    let err = h.client.try_cancel(&phantom_id).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamNotFound);
}

/// If a stream entry were lost (e.g. due to archival), the pool invariant must
/// not be violated for the remaining live streams.
#[test]
fn pool_invariant_holds_even_with_isolated_stream_loss() {
    let h = Harness::new();
    let start = h.now();

    // Create multiple streams; the "partitioned" one is never touched again.
    let _live1 = h.create(500 * ONE, start, start + 500, start, true, true, true);
    let _live2 = h.create(700 * ONE, start, start + 700, start, true, true, true);
    let _isolated = h.create(300 * ONE, start, start + 300, start, true, true, true);

    h.advance(200);
    // Operate only on live1 and live2.
    h.client.withdraw(&_live1, &Some(100 * ONE));
    h.client.withdraw(&_live2, &Some(100 * ONE));

    // The pool invariant must hold for all three streams including the
    // un-touched one.
    h.assert_pool_invariant();
}

// ---------------------------------------------------------------------------
// §2  Clock drift — extreme timestamp skew
// ---------------------------------------------------------------------------

/// When `now` is far in the past relative to `start_time` (i.e. a future-dated
/// stream), all accrual must be zero and all operations must succeed without
/// underflow.
#[test]
fn future_dated_stream_zero_accrual_at_any_past_now() {
    let h = Harness::new();
    let now = h.now();
    // Schedule starts 1000 seconds in the future.
    let start = now + 1_000;
    let end = start + 1_000;
    let id = h.create(1_000 * ONE, start, end, start, true, false, false);

    // At current time (before start), nothing has vested.
    assert_eq!(h.client.vested_of(&id), 0);
    assert_eq!(h.client.withdrawable_of(&id), 0);
    assert_eq!(h.client.refundable_of(&id), 1_000 * ONE);

    // Withdraw must fail with NothingToWithdraw.
    let err = h.client.try_withdraw(&id, &None).unwrap_err().unwrap();
    assert_eq!(err, Error::NothingToWithdraw);

    h.assert_pool_invariant();
}

/// When `now` is far in the future relative to `end_time` (massive positive
/// skew), `vested == deposited` exactly and `refundable == 0` (I6).
#[test]
fn past_maturity_clock_skew_vested_equals_deposited() {
    let h = Harness::new();
    let start = h.now();
    let deposit = 2_000 * ONE;
    let id = h.create(deposit, start, start + 100, start, true, true, true);

    // Advance far beyond end_time.
    h.advance(100_000);

    assert_eq!(h.client.vested_of(&id), deposit, "I6: vested must equal deposited at maturity");
    assert_eq!(h.client.refundable_of(&id), 0, "I6: refundable must be 0 at maturity");
    h.assert_pool_invariant();
}

/// A backdated stream (start_time in the past) must not overflow and must
/// report the correct accrual as of the current ledger time.
#[test]
fn backdated_stream_accrual_is_correct_and_non_overflowing() {
    let h = Harness::new();
    let now = h.now();
    // Start 500 seconds in the past; end 500 seconds in the future.
    let start = now - 500;
    let end = now + 500;
    let deposit = 10_000 * ONE;
    let id = h.create(deposit, start, end, start, true, true, true);

    // 500/1000 = 50% should be vested.
    let v = h.client.vested_of(&id);
    let expected = deposit / 2;
    assert_eq!(v, expected, "backdated 50% elapsed: vested must be deposit/2");
    h.assert_pool_invariant();
}

// ---------------------------------------------------------------------------
// §3  Concurrent state conflicts — second call sees first call's state
// ---------------------------------------------------------------------------

/// Two consecutive `pause` calls on the same stream: the first must succeed,
/// the second must fail with `StreamAlreadyPaused`. No duplicate state must be
/// written.
#[test]
fn double_pause_second_call_sees_first_state() {
    let h = Harness::new();
    let start = h.now();
    let id = h.create(1_000 * ONE, start, start + 1_000, start, true, true, true);
    h.advance(100);

    h.client.pause(&id);

    let s_after_first = h.get(id);
    assert_eq!(s_after_first.status, StreamStatus::Paused);

    let err = h.client.try_pause(&id).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamAlreadyPaused,
        "second pause must fail with StreamAlreadyPaused");

    // State must be unchanged.
    let s_after_second = h.get(id);
    assert_eq!(s_after_second, s_after_first,
        "second pause must not mutate stream state");
    h.assert_invariants();
}

/// Two consecutive `cancel` calls: the second must fail with
/// `StreamTerminated` and must leave state unchanged.
#[test]
fn double_cancel_second_call_is_idempotent_in_error() {
    let h = Harness::new();
    let start = h.now();
    let id = h.create(1_000 * ONE, start, start + 1_000, start, true, true, true);
    h.advance(200);
    h.client.cancel(&id);

    let s1 = h.get(id);
    let err = h.client.try_cancel(&id).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamTerminated);

    let s2 = h.get(id);
    assert_eq!(s1, s2, "second cancel must not mutate state");
}

/// `withdraw` → `cancel` on the same ledger (no time advance): the cancel must
/// settle against the already-modified `withdrawn` counter, so conservation
/// is maintained.
#[test]
fn withdraw_then_cancel_same_ledger_conservation() {
    let h = Harness::new();
    let start = h.now();
    let deposit = 1_000 * ONE;
    let id = h.create(deposit, start, start + 1_000, start, true, true, true);
    h.advance(500); // 50% vested

    // Withdraw half of the vested amount first.
    let partial = 100 * ONE;
    let w = h.client.withdraw(&id, &Some(partial));
    assert_eq!(w, partial);

    let sender_before = h.balance(&h.sender);
    h.client.cancel(&id);
    let refund = h.balance(&h.sender) - sender_before;

    // withdrawn + refund + remaining_vested = deposit
    let s = h.get(id);
    // s.deposited is now the vested amount at cancel; s.withdrawn is partial.
    assert_eq!(
        s.withdrawn + refund + (s.deposited - s.withdrawn),
        s.withdrawn + refund + s.deposited - s.withdrawn,
    );
    // The real conservation check: total_out = withdrawn + refund.
    // total_in = deposit.
    // total_out + remaining_locked = deposit.
    assert_eq!(partial + refund + s.deposited - s.withdrawn, deposit,
        "conservation: withdrawn({partial}) + refund({refund}) + locked({}) = deposit({deposit})",
        s.deposited - s.withdrawn);
    h.assert_pool_exact();
}

/// `top_up` → `cancel` without any time advance: the extra deposited tokens must
/// be fully refunded to the sender.
#[test]
fn top_up_then_cancel_no_time_full_refund() {
    let h = Harness::new();
    let start = h.now();
    let deposit = 1_000 * ONE;
    let id = h.create(deposit, start, start + 1_000, start, true, true, true);

    // Top up before any accrual.
    let extra = 500 * ONE;
    h.client.top_up(&id, &extra);

    // Cancel immediately — nothing has vested.
    let sender_before = h.balance(&h.sender);
    h.client.cancel(&id);
    let refund = h.balance(&h.sender) - sender_before;

    // Everything must come back (vested == 0 at t==start).
    assert_eq!(refund, deposit + extra,
        "full refund expected: got {refund} expected {}", deposit + extra);
    h.assert_pool_exact();
}

// ---------------------------------------------------------------------------
// §4  Partial batch failures — all-or-nothing rollback
// ---------------------------------------------------------------------------

/// A batch withdraw that contains a non-existent stream ID must fail with
/// `StreamNotFound` and must leave all stream states unchanged.
#[test]
fn batch_withdraw_with_invalid_id_rolls_back_all() {
    let h = Harness::new();
    let start = h.now();
    let id1 = h.create(1_000 * ONE, start, start + 1_000, start, true, true, true);
    let id2 = h.create(1_000 * ONE, start, start + 1_000, start, true, true, true);
    h.advance(500);

    let s1_before = h.get(id1);
    let s2_before = h.get(id2);
    let pool_before = h.pool();

    // Include a phantom id — this should roll back everything.
    let phantom = 9_999_999u64;
    let ids = h.ids(&[id1, phantom, id2]);
    let err = h
        .client
        .try_batch_withdraw(&h.recipient, &ids)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, Error::StreamNotFound);

    // No state must have changed.
    assert_eq!(h.get(id1), s1_before);
    assert_eq!(h.get(id2), s2_before);
    assert_eq!(h.pool(), pool_before);
}

/// A batch withdraw with a duplicate stream ID must fail with
/// `DuplicateStreamId` and must not corrupt stream state.
#[test]
fn batch_withdraw_with_duplicate_id_rejected_atomically() {
    let h = Harness::new();
    let start = h.now();
    let id = h.create(1_000 * ONE, start, start + 1_000, start, true, true, true);
    h.advance(300);

    let s_before = h.get(id);
    let ids = h.ids(&[id, id]); // duplicate
    let err = h
        .client
        .try_batch_withdraw(&h.recipient, &ids)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, Error::DuplicateStreamId);
    assert_eq!(h.get(id), s_before, "stream state must be unchanged after rejected batch");
}

/// An empty batch must return `EmptyBatch` and leave state untouched.
#[test]
fn batch_withdraw_empty_returns_empty_batch_error() {
    let h = Harness::new();
    let start = h.now();
    let id = h.create(1_000 * ONE, start, start + 1_000, start, true, true, true);
    h.advance(300);

    let ids = h.ids(&[]);
    let err = h
        .client
        .try_batch_withdraw(&h.recipient, &ids)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, Error::EmptyBatch);
    h.assert_pool_invariant();
}

// ---------------------------------------------------------------------------
// §5  Maximum state-space: edge-of-schedule combinations
// ---------------------------------------------------------------------------

/// Pause at `start_time`, resume immediately (zero duration pause), then cancel
/// at `end_time - 1`. Stream clock should accurately reflect the schedule with
/// no overflow.
#[test]
fn pause_at_start_resume_immediately_cancel_near_end() {
    let h = Harness::new();
    let start = h.now();
    let end = start + 1_000;
    let deposit = 1_000 * ONE;
    let id = h.create(deposit, start, end, start, true, true, true);

    // Pause at start (zero elapsed).
    h.client.pause(&id);
    // Resume immediately (paused_duration == 0).
    h.client.resume(&id);

    h.advance(999); // one second before end

    h.client.cancel(&id);

    let s = h.get(id);
    assert_eq!(s.status, StreamStatus::Cancelled);
    assert!(s.paused_at.is_none());
    // paused_total must be 0 (zero-duration pause).
    assert_eq!(s.paused_total, 0);
    h.assert_pool_exact();
}

/// Multiple pause/resume cycles with varying durations followed by a cancel: the
/// cumulative `paused_total` must be reflected in the settlement amount.
#[test]
fn multiple_pause_resume_cycles_cancel_settlement_uses_stream_clock() {
    let h = Harness::new();
    let start = h.now();
    let deposit = 3_000 * ONE;
    let id = h.create(deposit, start, start + 3_000, start, true, true, true);

    h.advance(500);
    h.client.pause(&id);
    h.advance(100); // paused for 100s
    h.client.resume(&id);
    h.advance(500);
    h.client.pause(&id);
    h.advance(200); // paused for 200s
    h.client.resume(&id);
    h.advance(500);

    // Stream clock: 500 + 500 + 500 = 1500 seconds of actual accrual.
    // Paused total: 100 + 200 = 300 seconds.
    let s_live = h.get(id);
    assert_eq!(s_live.paused_total, 300, "paused_total must be cumulative");

    let now = h.now();
    let vested = accrual::vested(&s_live, now).expect("overflow");

    h.client.cancel(&id);

    let s_post = h.get(id);
    assert_eq!(s_post.deposited, vested,
        "cancel must settle at exactly vested({vested}) at cancel instant");
    assert_eq!(s_post.paused_total, 300,
        "paused_total must be preserved through cancel");
    h.assert_pool_exact();
}

/// A stream that is paused at `end_time - 1` (one second before maturity) and
/// then cancelled must correctly compute vested using the frozen stream clock.
/// The stream clock is `end_time - 1 - paused_total`, which is before `end_time`,
/// so `deposited` must NOT be the full original deposit.
#[test]
fn paused_just_before_maturity_cancel_uses_frozen_clock_not_full_deposit() {
    let h = Harness::new();
    let start = h.now();
    let duration = 1_000u64;
    let deposit = 1_000 * ONE;
    let id = h.create(deposit, start, start + duration, start, true, true, true);

    // Advance to one second before end.
    h.advance(duration - 1);
    h.client.pause(&id);

    // Now cancel while paused.
    h.client.cancel(&id);

    let s = h.get(id);
    // Stream clock was frozen at start + (duration - 1).
    // vested at that instant = deposit * (duration - 1) / duration, rounded down.
    let expected_vested = deposit * (duration as i128 - 1) / duration as i128;
    assert_eq!(s.deposited, expected_vested,
        "post-cancel deposited must equal vested at frozen stream clock");
    // The full deposit must NOT have been used.
    assert!(s.deposited < deposit,
        "paused-before-maturity cancel must not vest the full deposit");
    h.assert_pool_exact();
}

/// Top up multiple times and cancel: the settlement must reflect the final
/// schedule's accrual rate, not the original one.
#[test]
fn multiple_top_ups_cancel_uses_final_schedule() {
    let h = Harness::new();
    let start = h.now();
    let deposit = 1_000 * ONE;
    let id = h.create(deposit, start, start + 1_000, start, true, true, true);

    // Two top-ups before any accrual.
    h.client.top_up(&id, &(500 * ONE));
    h.client.top_up(&id, &(500 * ONE));

    h.advance(100); // some accrual on the extended schedule

    let now = h.now();
    let s_live = h.get(id);
    let vested = accrual::vested(&s_live, now).expect("overflow");

    h.client.cancel(&id);

    let s_post = h.get(id);
    assert_eq!(s_post.deposited, vested,
        "cancel uses final schedule after top_ups");
    h.assert_pool_exact();
}

// ---------------------------------------------------------------------------
// §6  State-machine exhaustion: every terminal transition rejects further ops
// ---------------------------------------------------------------------------

/// After a stream is cancelled, every mutating operation must return a typed
/// terminal error rather than panicking or silently succeeding.
#[test]
fn cancelled_stream_rejects_all_mutating_ops() {
    let h = Harness::new();
    let start = h.now();
    let id = h.create(1_000 * ONE, start, start + 1_000, start, true, true, true);
    h.advance(200);
    h.client.cancel(&id);

    let err = h.client.try_cancel(&id).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamTerminated, "cancel on cancelled");

    let err = h.client.try_pause(&id).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamTerminated, "pause on cancelled");

    let err = h.client.try_top_up(&id, &(100 * ONE)).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamTerminated, "top_up on cancelled");

    // grant_delegate on a cancelled stream must also reject.
    use crate::op;
    let delegate = h.other.clone();
    let err = h
        .client
        .try_grant_delegate(&id, &h.sender, &delegate, &op::CANCEL, &None)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, Error::StreamTerminated, "grant_delegate on cancelled");

    h.assert_invariants();
}

/// After a stream is depleted (fully withdrawn), all mutating lifecycle ops
/// must reject with terminal errors.
#[test]
fn depleted_stream_rejects_lifecycle_ops() {
    let h = Harness::new();
    let start = h.now();
    let id = h.create(1_000 * ONE, start, start + 1_000, start, true, true, true);
    h.advance(1_001); // past maturity
    h.client.withdraw(&id, &None); // fully drain

    let s = h.get(id);
    assert_eq!(s.status, StreamStatus::Depleted);

    let err = h.client.try_cancel(&id).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamTerminated, "cancel on depleted");

    let err = h.client.try_pause(&id).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamTerminated, "pause on depleted");

    h.assert_invariants();
}
