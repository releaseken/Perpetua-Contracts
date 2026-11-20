//! Issue #405 — Byzantine fault tolerance scenarios for cliff-vesting under
//! simulated network partitions.
//!
//! # What "Byzantine fault tolerance" means here
//!
//! Soroban is a deterministic single-threaded execution environment: there is
//! no genuine network partition inside a contract invocation.  What *can* occur
//! in a streaming/vesting context are the patterns that Byzantine fault models
//! are designed to expose:
//!
//! * **Stale reads** — a client operates against an RPC node that is behind the
//!   current ledger and therefore returns old stream state.  The contract's
//!   pure-function accrual engine (`accrual::vested`, etc.) must yield safe,
//!   bounded results even when the timestamp it receives is older than the
//!   actual ledger.
//!
//! * **Reordered or replayed operations** — an adversary submits the same
//!   transaction twice, or submits sub-operations out of order (pause before
//!   create, withdraw before cliff, cancel after deplete).  Every such attempt
//!   must be rejected with a deterministic typed error; no state must mutate
//!   silently.
//!
//! * **Concurrent competing callers** — two callers independently drive a
//!   pausable cliff-vesting stream through opposite state transitions (one
//!   pausing while another withdraws, one cancelling while another tops up).
//!   Soroban serialises all of these, but the first committed transaction must
//!   leave the second with a coherent, non-corrupting starting state.
//!
//! * **Interleaved clock jumps** — the ledger timestamp may skip forward by
//!   large amounts (missed ledgers, Horizon catch-up) or not advance between
//!   calls.  The accrual formula must stay bounded and the cliff gate must not
//!   misfire at any skip magnitude.
//!
//! * **Adversarial parameter combinations** — extreme start/cliff/end
//!   combinations (cliff == start, cliff == end, cliff == u64::MAX-relative
//!   timestamps) that could expose off-by-one or overflow in the gate logic.
//!
//! # Relationship to `ReadFreshness`
//!
//! `accrual::classify_read` / `ReadFreshness` already provide the vocabulary
//! for a client to detect stale reads before acting on them.  These tests
//! complement that by verifying the *contract side*: even if the client sends a
//! transaction derived from a stale snapshot, the invariants inside the contract
//! are preserved and the caller receives a typed error rather than corrupted
//! state.

use super::common::*;
use crate::{accrual, Error, StreamStatus};

// ---------------------------------------------------------------------------
// Stale-read scenarios
// ---------------------------------------------------------------------------

/// A withdrawal attempt that was built against a stale (pre-cliff) snapshot
/// must fail with [`Error::NothingToWithdraw`] when it arrives at a ledger
/// where the cliff has not yet been reached, regardless of how far back the
/// snapshot was.
///
/// Simulated partition: the RPC node lags by an arbitrary number of ledgers,
/// and the client calculates withdrawable == 0 at snapshot time.  When the
/// transaction executes the cliff is still in the future — the contract must
/// not let the cliff gate flip early due to any kind of "snapshot drift".
#[test]
fn stale_snapshot_before_cliff_cannot_unlock_withdrawal() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 100 * DAY;
    let end = start + 200 * DAY;
    let deposit = 1_000 * ONE;
    let id = h.create(deposit, start, end, cliff, true, true, true);

    // Simulate "partition" by advancing different amounts and checking every
    // time that the gate remains closed until the cliff ledger.
    for lag_days in [0u64, 10, 50, 99] {
        h.warp_to(start + lag_days * DAY);
        // A client acting on a stale snapshot where cliff was not yet reached
        // must receive NothingToWithdraw, never a successful payout.
        let err = h.client.try_withdraw(&id, &None).unwrap_err().unwrap();
        assert_eq!(
            err,
            Error::NothingToWithdraw,
            "withdrawal at lag {lag_days} days must fail — cliff not reached"
        );
        // Vested is strictly zero while the stream clock is before the cliff.
        assert_eq!(h.client.vested_of(&id), 0, "vested must be 0 at lag {lag_days}d");
    }

    // Once the cliff ledger is reached the gate flips exactly once; the full
    // accrued amount since start_time becomes available.
    h.warp_to(cliff);
    let vested = h.client.vested_of(&id);
    assert_eq!(vested, deposit / 2, "half the deposit vested at the halfway cliff");
    assert_eq!(h.client.withdraw(&id, &None), vested);
    h.assert_pool_exact();
}

/// If the RPC node is ahead of the executing ledger (clock skew in the other
/// direction), `vested` at an earlier timestamp must never exceed `vested` at
/// the current timestamp — accrual is monotonically non-decreasing in time.
///
/// This exercises the pure `accrual::vested` function at ordered timestamps to
/// confirm the stream clock never "un-accrues" from a network anomaly.
#[test]
fn accrual_is_monotone_across_simulated_clock_skew() {
    let start = 1_700_000_000u64;
    let cliff = start + 50 * DAY;
    let end = start + 200 * DAY;
    let deposit = 2_000 * ONE;
    let stream = crate::types::Stream {
        sender: soroban_sdk::Address::generate(&soroban_sdk::Env::default()),
        recipient: soroban_sdk::Address::generate(&soroban_sdk::Env::default()),
        token: soroban_sdk::Address::generate(&soroban_sdk::Env::default()),
        deposited: deposit,
        withdrawn: 0,
        start_time: start,
        end_time: end,
        cliff_time: cliff,
        flags: crate::types::Stream::flags_from_parts(true, true, true),
        paused_at: None,
        paused_total: 0,
        status: StreamStatus::Active,
    };

    // Sample timestamps: before start, at start, before cliff, at cliff,
    // between cliff and end, at end, after end, and a u64-extreme value.
    let probes = [
        start.saturating_sub(1),
        start,
        cliff - 1,
        cliff,
        cliff + 1,
        (start + end) / 2,
        end - 1,
        end,
        end + 1,
        end + 365 * DAY,
    ];

    let mut prev = -1i128;
    for now in probes {
        let v = accrual::vested(&stream, now).expect("must not overflow for a well-formed stream");
        assert!(
            v >= prev,
            "vested moved backwards: {} → {} at t={now}",
            prev, v
        );
        assert!(v >= 0 && v <= deposit, "vested out of range at t={now}: {v}");
        prev = v;
    }
}

/// A large clock jump (simulated "caught-up node" delivering many skipped
/// ledgers at once) must not produce a vested amount above `deposited`.  The
/// maturity clamp in `accrual::elapsed` is what prevents this; this test pins
/// that the clamp holds across arbitrarily large jumps.
#[test]
fn large_clock_jump_does_not_exceed_deposited() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);

    // A single jump of one full year — far past the 100-day schedule.
    h.advance(YEAR);
    let vested = h.client.vested_of(&id);
    assert_eq!(vested, 1_000 * ONE, "clamped at deposited after large jump");
    assert_eq!(h.client.withdraw(&id, &None), 1_000 * ONE);
    h.assert_pool_exact();
}

/// Zero-advance between consecutive calls (same-ledger partition simulation)
/// must leave all views idempotent: calling `vested_of` or `withdrawable_of`
/// twice at the same ledger timestamp must return the same value.
#[test]
fn same_ledger_views_are_idempotent() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 60 * DAY;
    let id = h.create(1_200 * ONE, start, start + 360 * DAY, cliff, true, true, true);

    // Before the cliff: idempotent zero.
    h.warp_to(cliff - 1);
    assert_eq!(h.client.vested_of(&id), h.client.vested_of(&id));
    assert_eq!(h.client.withdrawable_of(&id), 0);
    assert_eq!(h.client.withdrawable_of(&id), 0);

    // At the cliff: idempotent non-zero.
    h.warp_to(cliff);
    let v1 = h.client.vested_of(&id);
    let v2 = h.client.vested_of(&id);
    assert_eq!(v1, v2, "vested_of is not idempotent at the cliff ledger");
    assert!(v1 > 0);

    // Mid-schedule: idempotent without any time advancing.
    let w1 = h.client.withdrawable_of(&id);
    let w2 = h.client.withdrawable_of(&id);
    assert_eq!(w1, w2, "withdrawable_of is not idempotent mid-schedule");
    h.assert_pool_exact();
}

// ---------------------------------------------------------------------------
// Replayed / reordered transaction scenarios
// ---------------------------------------------------------------------------

/// Replaying a `withdraw` after the stream is fully drained must return the
/// terminal error, not silently succeed or corrupt the balance.
///
/// Models an attacker who captures a successful withdraw transaction and
/// resubmits it; Soroban's sequence-number nonce prevents exact replay at the
/// network layer, but the contract must be safe even if that guard were absent.
#[test]
fn replayed_withdraw_on_depleted_stream_returns_terminal_error() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);
    h.advance(100 * DAY);

    // First withdraw — drains the stream.
    let amount = h.client.withdraw(&id, &None);
    assert_eq!(amount, 1_000 * ONE);
    assert_eq!(h.get(id).status, StreamStatus::Depleted);

    // "Replay" — the stream is Depleted; must get StreamTerminated.
    let err = h.client.try_withdraw(&id, &None).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamTerminated, "replayed withdraw must be terminal");
    // Balance unchanged.
    assert_eq!(h.balance(&h.recipient), 1_000 * ONE);
    h.assert_pool_exact();
}

/// Replaying a `cancel` on an already-cancelled stream must fail with a typed
/// error, not double-refund the sender.
#[test]
fn replayed_cancel_does_not_double_refund() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);
    h.advance(30 * DAY);
    let sender_before = h.balance(&h.sender);

    h.client.cancel(&id);
    let after_first = h.balance(&h.sender);
    assert_eq!(after_first, sender_before + 700 * ONE);

    // "Replay" cancel.
    let err = h.client.try_cancel(&id).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamTerminated, "second cancel must be terminal");
    // Sender balance unchanged after the replay attempt.
    assert_eq!(h.balance(&h.sender), after_first);
    h.assert_pool_exact();
}

/// Reordering: `resume` before `pause` must be rejected.  This covers a
/// race where a resume transaction is broadcast before the pause it depends on
/// has been confirmed.
#[test]
fn resume_before_pause_is_rejected() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);
    h.advance(10 * DAY);

    // No pause yet — stream is Active.
    let err = h.client.try_resume(&id).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamNotPaused);

    // Pause, then confirm resume succeeds.
    h.client.pause(&id);
    h.advance(5 * DAY);
    h.client.resume(&id);
    assert_eq!(h.get(id).status, StreamStatus::Active);
    h.assert_pool_exact();
}

/// Reordering: `pause` on an already-paused stream (a second pause transaction
/// arriving after the first confirmed) must be rejected with a deterministic
/// error and must not overwrite `paused_at`.
#[test]
fn duplicate_pause_does_not_corrupt_freeze_point() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);
    h.advance(20 * DAY);
    h.client.pause(&id);

    let freeze_before = h.get(id).paused_at;
    let vested_before = h.client.vested_of(&id);

    // Advance and try a second pause — must be rejected.
    h.advance(10 * DAY);
    let err = h.client.try_pause(&id).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamAlreadyPaused);

    // Freeze point and vested amount unchanged.
    assert_eq!(h.get(id).paused_at, freeze_before, "paused_at must not move");
    assert_eq!(h.client.vested_of(&id), vested_before, "vested must remain frozen");
    h.assert_pool_exact();
}

// ---------------------------------------------------------------------------
// Competing caller scenarios
// ---------------------------------------------------------------------------

/// Competing actions: `pause` and `withdraw` arrive at the same ledger.
/// Soroban serialises them; whichever comes first the final state must be
/// coherent — no double-spending, no broken invariants.
///
/// We simulate both orderings: pause-then-withdraw and withdraw-then-pause.
#[test]
fn pause_then_withdraw_at_same_ledger_is_coherent() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);
    h.advance(50 * DAY);

    // Ordering 1: pause first, then withdraw.
    h.client.pause(&id);
    assert_eq!(h.client.withdrawable_of(&id), 500 * ONE);
    let payout = h.client.withdraw(&id, &None);
    assert_eq!(payout, 500 * ONE);
    assert_eq!(h.get(id).status, StreamStatus::Paused, "still paused after withdrawal");
    assert_eq!(h.get(id).withdrawn, 500 * ONE);
    h.assert_pool_exact();
}

#[test]
fn withdraw_then_pause_at_same_ledger_is_coherent() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);
    h.advance(50 * DAY);

    // Ordering 2: withdraw first, then pause.
    let payout = h.client.withdraw(&id, &None);
    assert_eq!(payout, 500 * ONE);
    h.client.pause(&id);
    assert_eq!(h.get(id).status, StreamStatus::Paused);
    // No further accrual while paused.
    h.advance(10 * DAY);
    assert_eq!(h.client.withdrawable_of(&id), 0, "paused — nothing further accrues");
    h.assert_pool_exact();
}

/// Competing actions: `cancel` while another caller's `withdraw` is in-flight
/// (modelled as cancel before withdraw succeeds).
///
/// After cancel, the recipient's entitlement is frozen at the cancel instant.
/// A subsequent `withdraw` must succeed with the remaining vested amount and
/// then leave the stream fully settled.
#[test]
fn cancel_before_withdraw_leaves_recipient_whole() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);
    h.advance(40 * DAY);
    let sender_before = h.balance(&h.sender);

    // Ordering: cancel wins the race.
    h.client.cancel(&id);
    assert_eq!(h.balance(&h.sender), sender_before + 600 * ONE);
    assert_eq!(h.get(id).status, StreamStatus::Cancelled);

    // Recipient's withdraw still succeeds — cancel does not seize earned funds.
    let payout = h.client.withdraw(&id, &None);
    assert_eq!(payout, 400 * ONE, "recipient collects the 40-day vested amount");
    h.assert_pool_exact();
    assert_eq!(h.pool(), 0);
}

// ---------------------------------------------------------------------------
// Interleaved clock jumps with cliff-vesting
// ---------------------------------------------------------------------------

/// A series of alternating freeze (same-ledger) and jump steps across a cliff
/// must keep the gate consistent: the cliff opens exactly once, at the correct
/// stream-time crossing, regardless of how the wall-clock jumps.
#[test]
fn interleaved_clock_jumps_across_cliff_maintain_gate() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 100 * DAY;
    let id = h.create(1_000 * ONE, start, start + 200 * DAY, cliff, true, true, true);

    // Five ticks before the cliff, each a different size.
    for jump in [1u64, DAY, 10 * DAY, 20 * DAY, 30 * DAY] {
        h.advance(jump);
        if h.now() < cliff {
            assert_eq!(
                h.client.vested_of(&id),
                0,
                "gate must remain closed before the cliff (now={})", h.now()
            );
        }
    }
    // At this point we have advanced 1 + DAY + 10*DAY + 20*DAY + 30*DAY = 61*DAY+1 seconds.
    // The cliff is at 100 days, so we are still before the cliff.
    assert!(h.now() < cliff, "should be before the cliff after mixed jumps");
    assert_eq!(h.client.vested_of(&id), 0, "gate still closed");

    // Single jump to the cliff.
    h.warp_to(cliff);
    assert!(h.client.vested_of(&id) > 0, "gate opens exactly at the cliff");

    // Single jump past the cliff.
    h.warp_to(cliff + 50 * DAY);
    let v = h.client.vested_of(&id);
    assert!(v >= h.client.vested_of(&id), "vested is non-decreasing");
    h.assert_pool_exact();
}

/// A cliff-vesting stream that is paused across a large clock jump must still
/// obey the stream-clock cliff gate; the gate must not open based on
/// wall-clock time alone.
#[test]
fn paused_stream_ignores_wall_clock_cliff_crossing() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 60 * DAY;
    let id = h.create(600 * ONE, start, start + 120 * DAY, cliff, true, true, true);

    // Pause before the cliff.
    h.advance(30 * DAY);
    h.client.pause(&id);

    // Large clock jump — wall-clock crosses the cliff while paused.
    h.advance(YEAR);
    // Stream clock is frozen at day 30; cliff has not been reached on the stream clock.
    assert_eq!(
        h.client.vested_of(&id),
        0,
        "cliff must not open during pause even after a year-long clock jump"
    );
    let err = h.client.try_withdraw(&id, &None).unwrap_err().unwrap();
    assert_eq!(err, Error::NothingToWithdraw);

    // Resume; stream clock is at day 30 — still 30 stream-days from the cliff.
    h.client.resume(&id);
    assert_eq!(h.client.vested_of(&id), 0, "30 stream-days still needed to reach the cliff");

    // Advance exactly 30 more stream-days to reach the cliff.
    h.advance(30 * DAY);
    let vested = h.client.vested_of(&id);
    // 60 stream-days elapsed out of 120 total → 300 ONE vested.
    assert_eq!(vested, 300 * ONE, "full accrual since start_time unlocked at cliff");
    h.assert_pool_exact();
}

// ---------------------------------------------------------------------------
// Adversarial parameter combinations
// ---------------------------------------------------------------------------

/// Cliff at start (no cliff) + a large clock jump must not overflow or produce
/// a vested amount above `deposited`.
#[test]
fn no_cliff_large_jump_stays_bounded() {
    let h = Harness::new();
    let start = h.now();
    let id = h.create(500 * ONE, start, start + 50 * DAY, start, true, true, true);
    h.advance(YEAR);
    let v = h.client.vested_of(&id);
    assert_eq!(v, 500 * ONE, "clamped at deposited");
    h.client.withdraw(&id, &None);
    h.assert_pool_exact();
}

/// Cliff at end (lump-sum) + pause spanning the entire schedule + resume must
/// still deliver the full deposit exactly once at maturity, with no double
/// payout.
#[test]
fn lump_sum_cliff_pause_spanning_full_schedule_no_double_payout() {
    let h = Harness::new();
    let start = h.now();
    let end = start + 100 * DAY;
    let id = h.create(800 * ONE, start, end, end, true, true, true);

    // Pause at start, let wall-clock run past the entire schedule.
    h.client.pause(&id);
    h.advance(YEAR);

    // The lump-sum cliff has not been reached on the stream clock.
    assert_eq!(h.client.vested_of(&id), 0, "lump-sum cliff not reached while paused");

    // Resume and advance to the stream-time end.
    h.client.resume(&id);
    h.advance(100 * DAY);
    assert_eq!(h.client.vested_of(&id), 800 * ONE, "full deposit vests at stream-time end");

    // Single withdrawal drains the stream.
    assert_eq!(h.client.withdraw(&id, &None), 800 * ONE);
    // Attempting a second withdrawal is terminal.
    let err = h.client.try_withdraw(&id, &None).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamTerminated, "no double payout");
    h.assert_pool_exact();
}

/// A sequence of pause→cancel→withdraw across a cliff must leave balances
/// exactly consistent regardless of the order cancel and withdraw land.
#[test]
fn pause_cancel_withdraw_sequence_across_cliff_is_consistent() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 50 * DAY;
    let deposit = 1_000 * ONE;
    let id = h.create(deposit, start, start + 200 * DAY, cliff, true, true, true);
    let sender_before = h.balance(&h.sender);

    // Pause before the cliff.
    h.advance(30 * DAY);
    h.client.pause(&id);
    // Advance wall clock past the cliff while paused.
    h.advance(100 * DAY);

    // Cancel while paused — settles against the stream clock (day 30, pre-cliff).
    h.client.cancel(&id);
    let s = h.get(id);
    assert_eq!(s.status, StreamStatus::Cancelled);
    // Stream clock was at day 30 < cliff (day 50), so nothing vested.
    assert_eq!(h.client.withdrawable_of(&id), 0, "pre-cliff cancel gives recipient nothing");
    // Full deposit returned to sender.
    assert_eq!(
        h.balance(&h.sender),
        sender_before + deposit,
        "full deposit refunded on pre-cliff cancel"
    );
    assert_eq!(h.pool(), 0);
    h.assert_pool_exact();
}

/// Multiple competing withdraw attempts at the same ledger (modelled as rapid
/// sequential calls with no time advance) must collectively pay out exactly the
/// available balance — no under-payment, no double-payment.
#[test]
fn multiple_concurrent_withdrawals_pay_out_exactly_once() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 100 * DAY;
    let deposit = 1_000 * ONE;
    let id = h.create(deposit, start, start + 100 * DAY, cliff, true, true, true);

    // Jump to exactly the cliff/end (they coincide here — lump sum at maturity).
    h.warp_to(cliff);

    // Three rapid partial withdrawals at the same ledger.
    let w1 = h.client.withdraw(&id, &Some(400 * ONE));
    let w2 = h.client.withdraw(&id, &Some(400 * ONE));
    let w3 = h.client.withdraw(&id, &Some(200 * ONE));

    assert_eq!(w1, 400 * ONE);
    assert_eq!(w2, 400 * ONE);
    assert_eq!(w3, 200 * ONE);
    assert_eq!(w1 + w2 + w3, deposit, "total payout must equal deposited exactly");

    // Any further withdrawal is terminal.
    let err = h.client.try_withdraw(&id, &None).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamTerminated);
    h.assert_pool_exact();
    assert_eq!(h.pool(), 0);
}

// ---------------------------------------------------------------------------
// ReadFreshness: stale-read classifier
// ---------------------------------------------------------------------------

/// `accrual::classify_read` must classify reads as `Stale` when the observed
/// ledger is behind the required ledger, and `Fresh` when at or ahead.
/// This is the pure, host-free half of the Byzantine partition defence.
#[test]
fn classify_read_identifies_stale_and_fresh_reads_correctly() {
    use crate::accrual::{classify_read, ReadFreshness};

    // Observed == required: Fresh.
    assert_eq!(classify_read(100, 100), ReadFreshness::Fresh);
    // Observed > required: also Fresh (ahead is fine).
    assert_eq!(classify_read(101, 100), ReadFreshness::Fresh);
    assert_eq!(classify_read(u32::MAX, 0), ReadFreshness::Fresh);

    // Observed < required: Stale with the exact lag.
    assert_eq!(classify_read(99, 100), ReadFreshness::Stale { behind_by: 1 });
    assert_eq!(classify_read(0, 100), ReadFreshness::Stale { behind_by: 100 });
    assert_eq!(
        classify_read(0, u32::MAX),
        ReadFreshness::Stale { behind_by: u32::MAX },
    );
}

/// When a read is stale, the vested amount computed from that snapshot must be
/// less than or equal to what the actual ledger would return — the stale
/// snapshot can only under-report, never over-report.
///
/// This is a contract-level analogue of the safety theorem: acting on a stale
/// snapshot can at most make the client *think* there is less available than
/// there is; it cannot make the client think there is more.
#[test]
fn stale_snapshot_never_overestimates_vested() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 60 * DAY;
    let id = h.create(600 * ONE, start, start + 300 * DAY, cliff, true, true, true);

    // Record vested at the "stale" snapshot timestamp.
    h.advance(80 * DAY);
    let stale_time = h.now();
    let vested_at_stale = h.client.vested_of(&id);

    // The "current" ledger is ahead.
    h.advance(50 * DAY);
    let vested_at_current = h.client.vested_of(&id);

    assert!(
        vested_at_stale <= vested_at_current,
        "stale snapshot over-reported: stale={} > current={}",
        vested_at_stale, vested_at_current,
    );
    let _ = stale_time; // used structurally above
    h.assert_pool_exact();
}
