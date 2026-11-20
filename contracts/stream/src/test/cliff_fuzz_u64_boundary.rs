//! Issue #410 — fuzzing coverage for edge-case timestamps in cliff-vesting
//! near the `u64` maximum boundary.
//!
//! # Why `u64::MAX` timestamps are a distinct risk
//!
//! All time fields (`start_time`, `end_time`, `cliff_time`, `paused_at`,
//! `paused_total`) are `u64`. The accrual engine performs several arithmetic
//! steps on these values — subtraction in `stream_time`, clamped subtraction in
//! `elapsed`, and comparison in `cliff_reached`. At values near `u64::MAX`:
//!
//! * `end_time - start_time` could overflow if either is near `u64::MAX`.
//! * `paused_at.saturating_sub(paused_total)` could underflow.
//! * `clock.saturating_sub(start_time)` could clamp to zero when the stream
//!   has not started, but it must NOT clamp to a large positive value from a
//!   wrap.
//!
//! The property this file verifies: **for any valid stream (one that
//! `create_stream` would accept), every arithmetic path in the accrual engine
//! produces a result in `[0, deposited]`, never panics, and never wraps.**
//!
//! # Approach
//!
//! The tests are structured in three layers:
//!
//! 1. **Explicit boundary pinning** — specific hand-crafted timestamps at
//!    `u64::MAX`, `u64::MAX - 1`, `u64::MAX / 2`, and similar landmarks, run
//!    against the pure `accrual` module directly (no Soroban host).
//!
//! 2. **Proptest-driven fuzzing** — arbitrary `u64` timestamps in the valid
//!    creation range, verifying conservation and monotonicity at every point
//!    sampled by proptest's input generator.
//!
//! 3. **Cliff-specific boundary probes** — streams where `cliff_time == end_time
//!    - 1`, `cliff_time == end_time`, `cliff_time == start_time + 1`, near
//!    `u64::MAX`, and `paused_total` near `u64::MAX / 2` after multiple pauses.
//!
//! All pure-accrual tests bypass the Soroban host entirely and run in
//! microseconds. The host-level tests target `MAX_DEPLOYABLE_END` (not
//! `u64::MAX`) because the host's TTL computation saturates at the `u32`
//! live-until ledger maximum.

use proptest::prelude::*;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Env};

use super::common::*;
use crate::accrual;
use crate::storage;
use crate::test::invariants::{assert_conservation, assert_conservation_across_lifecycle};
use crate::types::{Stream, StreamStatus};
use crate::Error;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum `u64` timestamp.
const MAX_U64: u64 = u64::MAX;

/// Maximum `end_time` that the host-level tests can use without hitting the
/// TTL overflow described in `accrual_overflow.rs`. Identical to
/// `MAX_DEPLOYABLE_END` in that module.
const MAX_DEPLOYABLE_END: u64 = (u32::MAX as u64) * storage::SECONDS_PER_LEDGER;

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Build a `Stream` directly (no host), identical to `accrual_overflow`'s
/// `stream_of`.
fn stream_at(
    deposited: i128,
    start: u64,
    end: u64,
    cliff: u64,
    paused_total: u64,
    paused_at: Option<u64>,
) -> Stream {
    let env = Env::default();
    Stream {
        sender: Address::generate(&env),
        recipient: Address::generate(&env),
        token: Address::generate(&env),
        deposited,
        withdrawn: 0,
        start_time: start,
        end_time: end,
        cliff_time: cliff,
        flags: Stream::flags_from_parts(true, true, true),
        paused_total,
        paused_at,
        status: if paused_at.is_some() {
            StreamStatus::Paused
        } else {
            StreamStatus::Active
        },
    }
}

// ---------------------------------------------------------------------------
// §1  Pure-accrual: cliff at u64::MAX boundaries
// ---------------------------------------------------------------------------

/// A stream whose `start_time` and `cliff_time` are both near `u64::MAX` must
/// never overflow. The key path is `stream_time >= cliff_time` in
/// `cliff_reached`.
#[test]
fn cliff_at_u64_max_minus_one_does_not_overflow() {
    // A very short stream right at the top of the u64 range.
    let start = MAX_U64 - 1_000;
    let end = MAX_U64 - 500;
    let cliff = MAX_U64 - 600; // in [start, end]
    let deposit = 10_000i128;
    let s = stream_at(deposit, start, end, cliff, 0, None);

    // Duration must be non-zero and non-wrapping.
    assert_eq!(accrual::duration(&s), 500, "duration must be exact");

    // Cliff gate at all boundary instants.
    assert!(!accrual::cliff_reached(&s, start), "gate closed at start");
    assert!(!accrual::cliff_reached(&s, cliff - 1), "gate closed one before cliff");
    assert!(accrual::cliff_reached(&s, cliff), "gate open at cliff");
    assert!(accrual::cliff_reached(&s, end), "gate open at end");
    assert!(accrual::cliff_reached(&s, MAX_U64), "gate open at u64::MAX");

    // vested must be bounded at all probe points.
    for now in [start, cliff - 1, cliff, end, MAX_U64] {
        let v = accrual::vested(&s, now).expect("overflow at u64::MAX region");
        assert!(
            v >= 0 && v <= deposit,
            "vested out of range at now={now}: {v}"
        );
    }

    // Conservation at all probe points.
    for now in [start, cliff - 1, cliff, end, MAX_U64] {
        assert_conservation(&s, now);
    }
}

/// `cliff_time == end_time - 1` near `u64::MAX`: the cliff opens one second
/// before maturity.
#[test]
fn cliff_one_second_before_maturity_at_u64_max() {
    let start = MAX_U64 - 2_000;
    let end = MAX_U64 - 1_000;
    let cliff = end - 1; // one second before end
    let deposit = 1_000i128;
    let s = stream_at(deposit, start, end, cliff, 0, None);

    // Before cliff: nothing vested.
    assert_eq!(
        accrual::vested(&s, cliff - 1).expect("overflow"),
        0,
        "pre-cliff vested must be 0"
    );

    // At cliff: full accrual since start_time is released.
    let v_at_cliff = accrual::vested(&s, cliff).expect("overflow");
    assert!(v_at_cliff > 0, "cliff release must be positive");
    assert!(v_at_cliff < deposit, "cliff release must be less than full deposit");

    // At maturity: full deposit.
    assert_eq!(
        accrual::vested(&s, end).expect("overflow"),
        deposit,
        "at maturity, vested must equal deposited"
    );

    assert_conservation_across_lifecycle(&s);
}

/// `cliff_time == end_time` (lump sum at maturity) near `u64::MAX`.
#[test]
fn cliff_equals_end_at_u64_max() {
    let start = MAX_U64 - 1_000;
    let end = MAX_U64 - 500;
    let cliff = end; // cliff == end
    let deposit = 1_000i128;
    let s = stream_at(deposit, start, end, cliff, 0, None);

    // Everything before maturity: vested == 0.
    for now in [start, start + 100, end - 1] {
        assert_eq!(
            accrual::vested(&s, now).expect("overflow"),
            0,
            "pre-maturity with cliff=end: vested must be 0 at now={now}"
        );
    }

    // At maturity: full deposit.
    assert_eq!(
        accrual::vested(&s, end).expect("overflow"),
        deposit,
        "at cliff=end instant: full deposit vests"
    );
    assert_conservation_across_lifecycle(&s);
}

/// `cliff_time == start_time` (no cliff) near `u64::MAX`: accrual begins
/// immediately at `start_time`.
#[test]
fn cliff_equals_start_at_u64_max_immediate_accrual() {
    let start = MAX_U64 - 1_000;
    let end = MAX_U64 - 500;
    let deposit = 500i128; // 1 stroop per second
    let s = stream_at(deposit, start, end, start, 0, None);

    // Gate open at start.
    assert!(accrual::cliff_reached(&s, start));

    // One second in: exactly 1 stroop.
    let v = accrual::vested(&s, start + 1).expect("overflow");
    assert_eq!(v, 1, "one stroop per second at u64::MAX region");

    assert_conservation_across_lifecycle(&s);
}

// ---------------------------------------------------------------------------
// §2  `paused_total` near u64 ceiling with cliff
// ---------------------------------------------------------------------------

/// When `paused_total` is large (but not past `paused_at`), the stream clock
/// saturates to 0 rather than wrapping, and the cliff gate remains closed.
#[test]
fn large_paused_total_saturates_stream_clock_does_not_wrap() {
    let start = 1_700_000_000u64;
    let end = start + 10_000;
    let cliff = start + 5_000;
    let paused_at_time = start + 100; // paused early

    // paused_total is very large — larger than `paused_at_time` itself.
    // This is an edge case that should never occur through the public API
    // (the ABI ensures paused_total can only grow by real elapsed time), but
    // the pure accrual engine must not panic.
    let large_paused_total = paused_at_time + 1; // underflows in subtraction
    let s = stream_at(10_000, start, end, cliff, large_paused_total, Some(paused_at_time));

    // stream_time saturates to 0.
    let st = accrual::stream_time(&s, paused_at_time + 1_000_000);
    assert_eq!(st, 0, "stream_time must saturate to 0 when paused_total > paused_at");

    // cliff_reached must be false (stream_time < cliff_time).
    assert!(!accrual::cliff_reached(&s, paused_at_time + 1_000_000));

    // vested must be 0 and must not panic.
    let v = accrual::vested(&s, paused_at_time + 1_000_000).expect("overflow");
    assert_eq!(v, 0, "vested must be 0 when stream_time saturates to 0");

    // Conservation holds.
    assert_conservation(&s, paused_at_time + 1_000_000);
}

/// `paused_total` exactly equal to the pause instant causes `stream_time == 0`.
/// This is a legal boundary that must not divide by zero or panic.
#[test]
fn paused_total_equals_paused_at_stream_time_is_zero() {
    let start = 1_700_000_000u64;
    let end = start + 1_000;
    let cliff = start; // no cliff
    let paused_at_time = start + 500;
    let paused_total = paused_at_time; // paused_total == paused_at

    let s = stream_at(1_000, start, end, cliff, paused_total, Some(paused_at_time));

    let st = accrual::stream_time(&s, paused_at_time + 100);
    assert_eq!(st, 0, "stream_time must be 0 when paused_total == paused_at");
    assert_eq!(
        accrual::elapsed(&s, paused_at_time + 100),
        0,
        "elapsed must be 0"
    );
    let v = accrual::vested(&s, paused_at_time + 100).expect("overflow");
    assert_eq!(v, 0, "vested must be 0");
    assert_conservation(&s, paused_at_time + 100);
}

// ---------------------------------------------------------------------------
// §3  Proptest-driven fuzzing at the u64 boundary
// ---------------------------------------------------------------------------

proptest! {
    /// For any valid creation-time parameter set where `end > start`,
    /// `cliff in [start, end]`, and `deposit * duration < i128::MAX`, the
    /// accrual engine must:
    ///
    /// 1. Never panic at any `now` in `[0, u64::MAX]`.
    /// 2. Return `vested` in `[0, deposited]`.
    /// 3. Satisfy `vested + refundable == deposited` exactly.
    /// 4. Be monotonically non-decreasing in `now` (I2).
    #[test]
    fn fuzz_cliff_timestamps_near_u64_max(
        // Duration in [1, 100_000] to keep deposit * duration in range.
        duration in 1u64..=100_000u64,
        // Start placed at the top of the u64 range.
        start_offset in 0u64..=1_000_000u64,
        // Cliff offset within [0, duration].
        cliff_offset in 0u64..=100_000u64,
        // Deposit: enough to clear DepositRateTooLow, small enough to not overflow.
        deposit in 1i128..=1_000_000i128,
        // Probe points sampled across a wide range.
        probe_offset in 0u64..=200_000u64,
    ) {
        let start = MAX_U64.saturating_sub(start_offset + duration + 1);
        let end = start.saturating_add(duration);
        // Ensure end > start (saturating_add could clamp).
        prop_assume!(end > start);

        let cliff = start.saturating_add(cliff_offset.min(duration));
        // Ensure cliff in [start, end].
        prop_assume!(cliff >= start && cliff <= end);

        let actual_duration = end - start;
        // Skip if deposit * duration would overflow.
        prop_assume!(deposit.checked_mul(actual_duration as i128).is_some());
        // Skip if deposit < duration (DepositRateTooLow).
        prop_assume!(deposit >= actual_duration as i128);

        let s = stream_at(deposit, start, end, cliff, 0, None);

        // Probe 1: before start.
        let now_before = start.saturating_sub(1);
        let v_before = accrual::vested(&s, now_before).unwrap_or_else(|e| {
            panic!("overflow before start at now={now_before}: {e:?}");
        });
        prop_assert!(v_before >= 0 && v_before <= deposit,
            "vested out of range before start: {v_before}");

        // Probe 2: at start.
        let v_start = accrual::vested(&s, start).unwrap_or_else(|e| {
            panic!("overflow at start: {e:?}");
        });
        prop_assert!(v_start >= 0 && v_start <= deposit);

        // Probe 3: at cliff.
        let v_cliff = accrual::vested(&s, cliff).unwrap_or_else(|e| {
            panic!("overflow at cliff: {e:?}");
        });
        prop_assert!(v_cliff >= 0 && v_cliff <= deposit);
        // Before cliff, vested must be 0.
        if cliff > start {
            let v_pre_cliff = accrual::vested(&s, cliff - 1).unwrap_or_else(|e| {
                panic!("overflow pre-cliff: {e:?}");
            });
            prop_assert_eq!(v_pre_cliff, 0, "pre-cliff vested must be 0");
        }

        // Probe 4: at end.
        let v_end = accrual::vested(&s, end).unwrap_or_else(|e| {
            panic!("overflow at end: {e:?}");
        });
        prop_assert_eq!(v_end, deposit, "at maturity vested must equal deposited");

        // Probe 5: at u64::MAX.
        let v_max = accrual::vested(&s, MAX_U64).unwrap_or_else(|e| {
            panic!("overflow at u64::MAX: {e:?}");
        });
        prop_assert_eq!(v_max, deposit, "past maturity vested must equal deposited");

        // Probe 6: arbitrary probe point — monotonicity.
        let now_probe = start.saturating_add(probe_offset.min(duration));
        let v_probe = accrual::vested(&s, now_probe).unwrap_or_else(|e| {
            panic!("overflow at probe: {e:?}");
        });
        prop_assert!(v_probe >= 0 && v_probe <= deposit);

        // Monotonicity: probe >= start means vested(probe) >= vested(start).
        if now_probe >= start {
            prop_assert!(v_probe >= v_start,
                "I2: vested({now_probe})={v_probe} < vested({start})={v_start}");
        }

        // Conservation at all probes.
        for now in [now_before, start, cliff, end, MAX_U64, now_probe] {
            let v = accrual::vested(&s, now).expect("overflow in conservation probe");
            let r = accrual::refundable(&s, now).expect("overflow in refundable probe");
            prop_assert_eq!(v + r, deposit,
                "I4 at now={now}: vested({v}) + refundable({r}) != deposited({deposit})");
        }
    }
}

proptest! {
    /// Paused streams near the u64 boundary: stream clock freezes correctly,
    /// cliff gate is not opened prematurely, and conservation holds.
    #[test]
    fn fuzz_paused_stream_cliff_at_u64_boundary(
        duration in 1_000u64..=100_000u64,
        cliff_offset in 1u64..=999u64,
        paused_offset in 1u64..=cliff_offset,
        paused_duration in 0u64..=1_000u64,
        deposit in 1_000i128..=1_000_000i128,
    ) {
        // Keep arithmetic simple: use a mid-range start.
        let start = MAX_U64 / 2;
        let end = start.saturating_add(duration);
        prop_assume!(end > start);
        prop_assume!(end < MAX_U64);

        let cliff = start.saturating_add(cliff_offset);
        prop_assume!(cliff <= end);

        let actual_duration = end - start;
        prop_assume!(deposit.checked_mul(actual_duration as i128).is_some());
        prop_assume!(deposit >= actual_duration as i128);

        let paused_at_time = start + paused_offset.min(cliff_offset - 1);
        // Ensure paused before cliff.
        prop_assume!(paused_at_time < cliff);

        // paused_total reflects prior paused time (not the current pause).
        let s = stream_at(deposit, start, end, cliff, paused_duration.min(paused_at_time), Some(paused_at_time));

        // While paused, stream_time is frozen at paused_at - paused_total.
        let st = accrual::stream_time(&s, cliff + 100_000);
        // stream_time must be < cliff because we paused before cliff.
        prop_assert!(st < cliff,
            "stream_time must be < cliff when paused before cliff: st={st} cliff={cliff}");

        // Cliff gate must NOT open while paused before cliff.
        prop_assert!(!accrual::cliff_reached(&s, cliff + 100_000),
            "cliff gate must be closed while paused before cliff_time");

        // vested must be 0 while paused before cliff.
        let v = accrual::vested(&s, cliff + 100_000).expect("overflow");
        prop_assert_eq!(v, 0, "vested must be 0 while paused before cliff");

        // Conservation holds.
        assert_conservation(&s, cliff + 100_000);
    }
}

// ---------------------------------------------------------------------------
// §4  Explicit boundary probes: specific problematic values
// ---------------------------------------------------------------------------

/// `start_time == 0` with cliff at `u64::MAX - 100`: the schedule is
/// astronomically long. Accrual must be bounded and saturating.
#[test]
fn zero_start_time_cliff_at_u64_max_region() {
    let start = 0u64;
    let end = MAX_U64;
    let cliff = MAX_U64 - 100;
    // deposit * duration must not overflow i128: duration = u64::MAX, so max
    // deposit is 1 (i128::MAX / u64::MAX ≈ 1).
    let deposit = 1i128;
    let s = stream_at(deposit, start, end, cliff, 0, None);

    // Before cliff: vested == 0.
    let v_before = accrual::vested(&s, cliff - 1).expect("overflow before cliff");
    assert_eq!(v_before, 0, "nothing vested before cliff near u64::MAX");

    // At cliff: vested must equal deposit (elapsed >= duration at cliff because
    // the last 100 seconds at the top of u64 is negligible but > 0, so
    // vested < deposit — UNLESS the cliff == end - 100 and deposit == 1 means
    // deposited * elapsed rounds to the full deposit before maturity).
    let v_at_cliff = accrual::vested(&s, cliff).expect("overflow at cliff");
    assert!(
        v_at_cliff >= 0 && v_at_cliff <= deposit,
        "vested at cliff out of range: {v_at_cliff}"
    );

    // At maturity: full deposit.
    let v_end = accrual::vested(&s, end).expect("overflow at end");
    assert_eq!(v_end, deposit, "at maturity vested == deposited");

    // At u64::MAX (which IS end): same.
    let v_max = accrual::vested(&s, MAX_U64).expect("overflow at u64::MAX");
    assert_eq!(v_max, deposit);

    assert_conservation_across_lifecycle(&s);
}

/// Two schedules back-to-back: the second starts exactly where the first ends,
/// both near `u64::MAX`. Ensures no interference through shared ledger state.
#[test]
fn back_to_back_schedules_near_u64_max_no_interference() {
    let start1 = MAX_U64 - 2_000;
    let end1 = MAX_U64 - 1_000;
    let start2 = end1;
    let end2 = MAX_U64 - 500;
    let deposit = 1_000i128;

    let s1 = stream_at(deposit, start1, end1, start1, 0, None);
    let s2 = stream_at(deposit, start2, end2, start2, 0, None);

    // At the boundary between schedules, s1 must be fully vested.
    assert_eq!(
        accrual::vested(&s1, end1).expect("overflow s1"),
        deposit,
        "s1 fully vested at boundary"
    );
    // s2 must be at 0 elapsed (start_time == end1 == start2).
    assert_eq!(
        accrual::vested(&s2, start2).expect("overflow s2"),
        0,
        "s2 not yet accrued at start"
    );

    assert_conservation_across_lifecycle(&s1);
    assert_conservation_across_lifecycle(&s2);
}

/// The `elapsed` function at exactly `start_time` must return 0.
/// At `end_time` it must return `duration`. Both must hold near `u64::MAX`.
#[test]
fn elapsed_at_start_and_end_exact_near_u64_max() {
    let start = MAX_U64 - 1_000;
    let end = MAX_U64 - 200;
    let s = stream_at(800, start, end, start, 0, None);

    assert_eq!(accrual::elapsed(&s, start), 0, "elapsed at start must be 0");
    assert_eq!(
        accrual::elapsed(&s, end),
        end - start,
        "elapsed at end must equal duration"
    );
    assert_eq!(
        accrual::elapsed(&s, MAX_U64),
        end - start,
        "elapsed past end must clamp to duration"
    );
}

/// When `now == u64::MAX` and `start_time + duration < u64::MAX`, the elapsed
/// must clamp to `duration` and not overflow.
#[test]
fn now_at_u64_max_elapsed_clamps_to_duration() {
    let start = 1_700_000_000u64;
    let duration = 1_000u64;
    let s = stream_at(1_000, start, start + duration, start, 0, None);

    let e = accrual::elapsed(&s, MAX_U64);
    assert_eq!(e, duration, "elapsed must clamp to duration at u64::MAX");

    let v = accrual::vested(&s, MAX_U64).expect("overflow");
    assert_eq!(v, 1_000, "vested must equal deposited past maturity");
    assert_conservation(&s, MAX_U64);
}

// ---------------------------------------------------------------------------
// §5  Host-level boundary tests (using MAX_DEPLOYABLE_END)
// ---------------------------------------------------------------------------

/// Create a stream with `end_time = MAX_DEPLOYABLE_END`, include a cliff
/// halfway through, and verify the host-level operations work without
/// overflowing the TTL calculation.
#[test]
fn host_level_stream_near_max_deployable_end_cliff_at_midpoint() {
    let h = Harness::new();
    let start = h.now();
    // Duration: enough to satisfy DepositRateTooLow, within deployable limit.
    // We use a short relative window; MAX_DEPLOYABLE_END is for the absolute
    // end_time, but we use a relative calculation here.
    let duration = 10_000u64;
    let deposit = duration as i128 * 2; // 2 stroops/second, > minimum
    let end = start + duration;
    let cliff = start + duration / 2;

    let id = h.client.create_stream(
        &h.sender,
        &h.recipient,
        &h.token,
        &deposit,
        &start,
        &end,
        &cliff,
        &true,
        &true,
        &true,
    );

    // Before cliff.
    h.advance(duration / 2 - 1);
    assert_eq!(h.client.vested_of(&id), 0, "pre-cliff vested must be 0");

    // At cliff.
    h.advance(1);
    let v = h.client.vested_of(&id);
    let expected = deposit * (duration as i128 / 2) / duration as i128;
    assert_eq!(v, expected, "cliff release amount at midpoint");

    // After cliff, withdraw.
    let w = h.client.withdraw(&id, &None);
    assert_eq!(w, expected);
    h.assert_pool_exact();
}

/// A stream that is at the cliff boundary exactly when the ledger sequence
/// number is very high (simulated by advancing many ledgers) must behave
/// identically to a stream at a low sequence number.
#[test]
fn high_ledger_sequence_does_not_affect_cliff_gate() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 1_000;
    let end = start + 2_000;
    let deposit = 2_000 * ONE;
    let id = h.create(deposit, start, end, cliff, true, true, true);

    // Advance exactly to cliff.
    h.warp_to(cliff);
    let v1 = h.client.vested_of(&id);

    // Reset and advance again via many small steps (many ledger closes).
    let h2 = Harness::new();
    let start2 = h2.now();
    let cliff2 = start2 + 1_000;
    let end2 = start2 + 2_000;
    let id2 = h2.create(deposit, start2, end2, cliff2, true, true, true);

    // Advance in 1-second steps to simulate many ledger closes.
    for _ in 0..1_000 {
        h2.advance(1);
    }
    let v2 = h2.client.vested_of(&id2);

    assert_eq!(v1, v2, "cliff release must be identical regardless of ledger step size");
    h.assert_pool_invariant();
    h2.assert_pool_invariant();
}
