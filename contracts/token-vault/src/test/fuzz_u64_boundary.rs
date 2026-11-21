//! Issue #455 — fuzzing coverage for edge-case timestamps in token-vault
//! near the `u64` maximum boundary.
//!
//! # Why `u64::MAX` timestamps are a distinct risk
//!
//! All time fields (`start_time`, `end_time`, `cliff_time`, `paused_at`,
//! `paused_total`) are `u64`.  The `math` module performs several arithmetic
//! steps on these values:
//!
//! * `end_time - start_time` could overflow if either is near `u64::MAX`.
//! * `paused_at.saturating_sub(paused_total)` could underflow.
//! * `clock.min(end_time).saturating_sub(start_time)` must clamp cleanly.
//! * `deposited * elapsed` must be guarded by `checked_mul`.
//!
//! # Test structure
//!
//! 1. **Explicit boundary pinning** — hand-crafted timestamps at `u64::MAX`,
//!    `u64::MAX - 1`, `u64::MAX / 2`, and similar landmarks, exercising the
//!    pure `math` module directly (no Soroban host needed).
//!
//! 2. **Proptest-driven fuzzing** — arbitrary `u64` timestamps in the valid
//!    creation range, verifying conservation and monotonicity at every point
//!    sampled by proptest's input generator.
//!
//! 3. **Cliff-specific boundary probes** — streams where
//!    `cliff_time == end_time - 1`, `cliff_time == end_time`,
//!    `cliff_time == start_time`, `paused_total` near the pause instant, and
//!    back-to-back positions at `u64::MAX`.
//!
//! All pure-accrual tests bypass the Soroban host entirely and run in
//! microseconds.

use proptest::prelude::*;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Env};

use crate::invariants::{assert_conservation, assert_conservation_across_lifecycle};
use crate::math;
use crate::types::{PositionStatus, VaultPosition};

// Silence unused-import warning: Address::generate is used via trait.
#[allow(unused_imports)]
use soroban_sdk::testutils::Ledger as _;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAX_U64: u64 = u64::MAX;

// ---------------------------------------------------------------------------
// Position builder (pure, no Soroban host)
// ---------------------------------------------------------------------------

/// Build a `VaultPosition` directly for accrual-only tests.
fn position_at(
    deposited: i128,
    start: u64,
    end: u64,
    cliff: u64,
    paused_total: u64,
    paused_at: Option<u64>,
) -> VaultPosition {
    let env = Env::default();
    let status = if paused_at.is_some() {
        PositionStatus::Paused
    } else {
        PositionStatus::Active
    };
    VaultPosition::test_new(
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
        deposited,
        start,
        end,
        cliff,
        paused_total,
        paused_at,
        status,
    )
}

// ---------------------------------------------------------------------------
// §1  Pure-accrual: cliff at u64::MAX boundaries
// ---------------------------------------------------------------------------

/// A position whose `start_time` and `cliff_time` are both near `u64::MAX`
/// must never overflow.  Key path: `stream_time >= cliff_time` in
/// `cliff_reached`.
#[test]
fn cliff_at_u64_max_minus_one_does_not_overflow() {
    let start = MAX_U64 - 1_000;
    let end = MAX_U64 - 500;
    let cliff = MAX_U64 - 600; // in [start, end]
    let deposit = 10_000i128;
    let pos = position_at(deposit, start, end, cliff, 0, None);

    assert_eq!(math::duration(&pos), 500, "duration must be exact");

    // Cliff gate at all boundary instants.
    assert!(!math::cliff_reached(&pos, start), "gate closed at start");
    assert!(
        !math::cliff_reached(&pos, cliff - 1),
        "gate closed one before cliff"
    );
    assert!(math::cliff_reached(&pos, cliff), "gate open at cliff");
    assert!(math::cliff_reached(&pos, end), "gate open at end");
    assert!(math::cliff_reached(&pos, MAX_U64), "gate open at u64::MAX");

    // vested must be bounded at every probe point.
    for now in [start, cliff - 1, cliff, end, MAX_U64] {
        let v = math::vested(&pos, now).expect("overflow at u64::MAX region");
        assert!(
            v >= 0 && v <= deposit,
            "vested out of range at now={now}: {v}"
        );
    }

    // Conservation at every probe point.
    for now in [start, cliff - 1, cliff, end, MAX_U64] {
        assert_conservation(&pos, now);
    }
}

/// `cliff_time == end_time - 1` near `u64::MAX`: the cliff opens one second
/// before maturity.
#[test]
fn cliff_one_second_before_maturity_at_u64_max() {
    let start = MAX_U64 - 2_000;
    let end = MAX_U64 - 1_000;
    let cliff = end - 1;
    let deposit = 1_000i128;
    let pos = position_at(deposit, start, end, cliff, 0, None);

    // Before cliff: nothing vested.
    assert_eq!(
        math::vested(&pos, cliff - 1).expect("overflow"),
        0,
        "pre-cliff vested must be 0"
    );

    // At cliff: some accrual released.
    let v_at_cliff = math::vested(&pos, cliff).expect("overflow");
    assert!(v_at_cliff > 0, "cliff release must be positive");
    assert!(
        v_at_cliff < deposit,
        "cliff release must be less than full deposit"
    );

    // At maturity: full deposit.
    assert_eq!(
        math::vested(&pos, end).expect("overflow"),
        deposit,
        "at maturity, vested must equal deposited"
    );

    assert_conservation_across_lifecycle(&pos);
}

/// `cliff_time == end_time` (lump-sum at maturity) near `u64::MAX`.
#[test]
fn cliff_equals_end_at_u64_max() {
    let start = MAX_U64 - 1_000;
    let end = MAX_U64 - 500;
    let cliff = end;
    let deposit = 1_000i128;
    let pos = position_at(deposit, start, end, cliff, 0, None);

    // Everything before maturity: vested == 0.
    for now in [start, start + 100, end - 1] {
        assert_eq!(
            math::vested(&pos, now).expect("overflow"),
            0,
            "pre-maturity with cliff=end: vested must be 0 at now={now}"
        );
    }

    // At maturity: full deposit vests.
    assert_eq!(
        math::vested(&pos, end).expect("overflow"),
        deposit,
        "at cliff=end instant: full deposit vests"
    );

    assert_conservation_across_lifecycle(&pos);
}

/// `cliff_time == start_time` (no cliff) near `u64::MAX`: accrual begins
/// immediately.
#[test]
fn cliff_equals_start_at_u64_max_immediate_accrual() {
    let start = MAX_U64 - 1_000;
    let end = MAX_U64 - 500;
    let deposit = 500i128; // 1 stroop per second
    let pos = position_at(deposit, start, end, start, 0, None);

    // Gate open at start.
    assert!(math::cliff_reached(&pos, start));

    // One second in: exactly 1 stroop.
    let v = math::vested(&pos, start + 1).expect("overflow");
    assert_eq!(v, 1, "one stroop per second at u64::MAX region");

    assert_conservation_across_lifecycle(&pos);
}

// ---------------------------------------------------------------------------
// §2  `paused_total` near the u64 ceiling with a cliff
// ---------------------------------------------------------------------------

/// When `paused_total` is larger than `paused_at`, `stream_time` saturates to
/// 0 rather than wrapping.  The cliff gate must remain closed and `vested`
/// must return 0 without panicking.
#[test]
fn large_paused_total_saturates_stream_clock_does_not_wrap() {
    let start = 1_700_000_000u64;
    let end = start + 10_000;
    let cliff = start + 5_000;
    let paused_at_time = start + 100;

    // paused_total larger than paused_at — impossible through the public API
    // but the pure arithmetic must still not panic.
    let large_paused_total = paused_at_time + 1;
    let pos = position_at(
        10_000,
        start,
        end,
        cliff,
        large_paused_total,
        Some(paused_at_time),
    );

    let st = math::stream_time(&pos, paused_at_time + 1_000_000);
    assert_eq!(
        st, 0,
        "stream_time must saturate to 0 when paused_total > paused_at"
    );

    assert!(!math::cliff_reached(&pos, paused_at_time + 1_000_000));

    let v = math::vested(&pos, paused_at_time + 1_000_000).expect("overflow");
    assert_eq!(v, 0, "vested must be 0 when stream_time saturates to 0");

    assert_conservation(&pos, paused_at_time + 1_000_000);
}

/// `paused_total == paused_at` → `stream_time == 0`.
/// Must not divide by zero or panic.
#[test]
fn paused_total_equals_paused_at_stream_time_is_zero() {
    let start = 1_700_000_000u64;
    let end = start + 1_000;
    let cliff = start; // no cliff
    let paused_at_time = start + 500;
    let paused_total = paused_at_time; // exactly equal

    let pos = position_at(1_000, start, end, cliff, paused_total, Some(paused_at_time));

    let st = math::stream_time(&pos, paused_at_time + 100);
    assert_eq!(
        st, 0,
        "stream_time must be 0 when paused_total == paused_at"
    );
    assert_eq!(
        math::elapsed(&pos, paused_at_time + 100),
        0,
        "elapsed must be 0"
    );
    let v = math::vested(&pos, paused_at_time + 100).expect("overflow");
    assert_eq!(v, 0, "vested must be 0");
    assert_conservation(&pos, paused_at_time + 100);
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
    /// 4. Be monotonically non-decreasing in `now`.
    #[test]
    fn fuzz_timestamps_near_u64_max(
        // Duration in [1, 100_000] to keep deposit * duration in range.
        duration in 1u64..=100_000u64,
        // Start placed near the top of the u64 range.
        start_offset in 0u64..=1_000_000u64,
        // Cliff offset within [0, duration].
        cliff_offset in 0u64..=100_000u64,
        // Deposit: enough to clear DepositRateTooLow, small enough to not overflow.
        deposit in 1i128..=1_000_000i128,
        // Probe point sampled across a wide range.
        probe_offset in 0u64..=200_000u64,
    ) {
        let start = MAX_U64.saturating_sub(start_offset + duration + 1);
        let end = start.saturating_add(duration);
        prop_assume!(end > start);

        let cliff = start.saturating_add(cliff_offset.min(duration));
        prop_assume!(cliff >= start && cliff <= end);

        let actual_duration = end - start;
        prop_assume!(deposit.checked_mul(actual_duration as i128).is_some());
        prop_assume!(deposit >= actual_duration as i128);

        let pos = position_at(deposit, start, end, cliff, 0, None);

        // --- Probe 1: before start ---
        let now_before = start.saturating_sub(1);
        let v_before = math::vested(&pos, now_before)
            .unwrap_or_else(|e| panic!("overflow before start at now={now_before}: {e:?}"));
        prop_assert!(v_before >= 0 && v_before <= deposit,
            "vested out of range before start: {}", v_before);

        // --- Probe 2: at start ---
        let v_start = math::vested(&pos, start)
            .unwrap_or_else(|e| panic!("overflow at start: {e:?}"));
        prop_assert!(v_start >= 0 && v_start <= deposit);

        // --- Probe 3: at cliff ---
        let v_cliff = math::vested(&pos, cliff)
            .unwrap_or_else(|e| panic!("overflow at cliff: {e:?}"));
        prop_assert!(v_cliff >= 0 && v_cliff <= deposit);

        // Before cliff, vested must be 0.
        if cliff > start {
            let v_pre_cliff = math::vested(&pos, cliff - 1)
                .unwrap_or_else(|e| panic!("overflow pre-cliff: {e:?}"));
            prop_assert_eq!(v_pre_cliff, 0, "pre-cliff vested must be 0");
        }

        // --- Probe 4: at end ---
        let v_end = math::vested(&pos, end)
            .unwrap_or_else(|e| panic!("overflow at end: {e:?}"));
        prop_assert_eq!(v_end, deposit, "at maturity vested must equal deposited");

        // --- Probe 5: at u64::MAX ---
        let v_max = math::vested(&pos, MAX_U64)
            .unwrap_or_else(|e| panic!("overflow at u64::MAX: {e:?}"));
        prop_assert_eq!(v_max, deposit, "past maturity vested must equal deposited");

        // --- Probe 6: arbitrary probe point (monotonicity) ---
        let now_probe = start.saturating_add(probe_offset.min(duration));
        let v_probe = math::vested(&pos, now_probe)
            .unwrap_or_else(|e| panic!("overflow at probe: {e:?}"));
        prop_assert!(v_probe >= 0 && v_probe <= deposit);

        if now_probe >= start {
            prop_assert!(v_probe >= v_start,
                "I2: vested({})={} < vested({})={}", now_probe, v_probe, start, v_start);
        }

        // --- Conservation at all probes ---
        for now in [now_before, start, cliff, end, MAX_U64, now_probe] {
            let v = math::vested(&pos, now).expect("overflow in conservation probe");
            let r = math::refundable(&pos, now).expect("overflow in refundable probe");
            prop_assert_eq!(v + r, deposit,
                "I4 at now={}: vested({}) + refundable({}) != deposited({})", now, v, r, deposit);
        }
    }
}

proptest! {
    /// Paused positions near the u64 boundary: stream clock freezes correctly,
    /// cliff gate is not opened prematurely, and conservation holds.
    #[test]
    fn fuzz_paused_position_cliff_at_u64_boundary(
        duration in 1_000u64..=100_000u64,
        // cliff_offset must be >= 2 so paused_offset can be in [1, cliff_offset-1].
        cliff_offset in 2u64..=999u64,
        // paused_offset in [1, cliff_offset-1]: ensure paused before cliff.
        paused_offset_frac in 1u64..=998u64,
        paused_duration in 0u64..=1_000u64,
        deposit in 1_000i128..=1_000_000i128,
    ) {
        let start = MAX_U64 / 2;
        let end = start.saturating_add(duration);
        prop_assume!(end > start && end < MAX_U64);

        let cliff = start.saturating_add(cliff_offset);
        prop_assume!(cliff <= end);

        let actual_duration = end - start;
        prop_assume!(deposit.checked_mul(actual_duration as i128).is_some());
        prop_assume!(deposit >= actual_duration as i128);

        // Keep paused_offset in [1, cliff_offset - 1] to ensure we pause before cliff.
        let paused_offset = 1 + (paused_offset_frac % cliff_offset.saturating_sub(1).max(1));
        let paused_at_time = start + paused_offset;
        prop_assume!(paused_at_time < cliff);

        let pos = position_at(
            deposit,
            start,
            end,
            cliff,
            paused_duration.min(paused_at_time),
            Some(paused_at_time),
        );

        // stream_time must be < cliff because we paused before cliff.
        let st = math::stream_time(&pos, cliff + 100_000);
        prop_assert!(st < cliff,
            "stream_time must be < cliff when paused before cliff: st={} cliff={}", st, cliff);

        // Cliff gate must NOT open while paused before cliff.
        prop_assert!(!math::cliff_reached(&pos, cliff + 100_000),
            "cliff gate must be closed while paused before cliff_time");

        // vested must be 0 while paused before cliff.
        let v = math::vested(&pos, cliff + 100_000).expect("overflow");
        prop_assert_eq!(v, 0, "vested must be 0 while paused before cliff");

        assert_conservation(&pos, cliff + 100_000);
    }
}

// ---------------------------------------------------------------------------
// §4  Explicit boundary probes: specific problematic values
// ---------------------------------------------------------------------------

/// `start_time == 0`, `end_time == u64::MAX`, cliff near `u64::MAX`.
/// This is the longest possible position; `deposit * duration` must be guarded.
#[test]
fn zero_start_time_cliff_at_u64_max_region() {
    let start = 0u64;
    let end = MAX_U64;
    let cliff = MAX_U64 - 100;
    // deposit * duration must fit in i128: duration = u64::MAX, so deposit = 1.
    let deposit = 1i128;
    let pos = position_at(deposit, start, end, cliff, 0, None);

    // Before cliff: vested == 0.
    let v_before = math::vested(&pos, cliff - 1).expect("overflow before cliff");
    assert_eq!(v_before, 0, "nothing vested before cliff near u64::MAX");

    // vested at cliff is in [0, deposit].
    let v_at_cliff = math::vested(&pos, cliff).expect("overflow at cliff");
    assert!(
        v_at_cliff >= 0 && v_at_cliff <= deposit,
        "vested at cliff out of range: {v_at_cliff}"
    );

    // At maturity: full deposit.
    let v_end = math::vested(&pos, end).expect("overflow at end");
    assert_eq!(v_end, deposit, "at maturity vested == deposited");

    // At u64::MAX (== end): same.
    let v_max = math::vested(&pos, MAX_U64).expect("overflow at u64::MAX");
    assert_eq!(v_max, deposit);

    assert_conservation_across_lifecycle(&pos);
}

/// Two back-to-back positions near `u64::MAX`.
/// The second starts where the first ends; no shared state; no interference.
#[test]
fn back_to_back_positions_near_u64_max_no_interference() {
    let start1 = MAX_U64 - 2_000;
    let end1 = MAX_U64 - 1_000;
    let start2 = end1;
    let end2 = MAX_U64 - 500;
    let deposit = 1_000i128;

    let pos1 = position_at(deposit, start1, end1, start1, 0, None);
    let pos2 = position_at(deposit, start2, end2, start2, 0, None);

    // pos1 fully vested at the boundary.
    assert_eq!(
        math::vested(&pos1, end1).expect("overflow pos1"),
        deposit,
        "pos1 fully vested at boundary"
    );
    // pos2 not yet accrued at start.
    assert_eq!(
        math::vested(&pos2, start2).expect("overflow pos2"),
        0,
        "pos2 not yet accrued at start"
    );

    assert_conservation_across_lifecycle(&pos1);
    assert_conservation_across_lifecycle(&pos2);
}

/// `elapsed` at exactly `start_time` must be 0; at `end_time` must equal
/// duration; past `end_time` must clamp.  All at `u64::MAX` region.
#[test]
fn elapsed_clamps_correctly_near_u64_max() {
    let start = MAX_U64 - 1_000;
    let end = MAX_U64 - 200;
    let pos = position_at(800, start, end, start, 0, None);

    assert_eq!(math::elapsed(&pos, start), 0, "elapsed at start must be 0");
    assert_eq!(
        math::elapsed(&pos, end),
        end - start,
        "elapsed at end must equal duration"
    );
    assert_eq!(
        math::elapsed(&pos, MAX_U64),
        end - start,
        "elapsed past end must clamp to duration"
    );
}

/// When `now == u64::MAX` and `start_time + duration << u64::MAX`, elapsed
/// must clamp to `duration` and not overflow.
#[test]
fn now_at_u64_max_elapsed_clamps_to_duration() {
    let start = 1_700_000_000u64;
    let duration = 1_000u64;
    let pos = position_at(1_000, start, start + duration, start, 0, None);

    let e = math::elapsed(&pos, MAX_U64);
    assert_eq!(e, duration, "elapsed must clamp to duration at u64::MAX");

    let v = math::vested(&pos, MAX_U64).expect("overflow");
    assert_eq!(v, 1_000, "vested must equal deposited past maturity");
    assert_conservation(&pos, MAX_U64);
}

/// `start_time == end_time - 1` with deposit == 1 and cliff == start:
/// a one-second stream at the very top of the u64 range.
#[test]
fn one_second_stream_at_u64_max() {
    let start = MAX_U64 - 1;
    let end = MAX_U64;
    let deposit = 1i128;
    let pos = position_at(deposit, start, end, start, 0, None);

    assert_eq!(math::duration(&pos), 1, "duration must be 1");
    assert_eq!(math::vested(&pos, start).expect("overflow"), 0);
    assert_eq!(math::vested(&pos, end).expect("overflow"), 1);
    assert_eq!(math::vested(&pos, MAX_U64).expect("overflow"), 1);
    assert_conservation_across_lifecycle(&pos);
}

/// `start_time == u64::MAX - 500`, `end_time == u64::MAX - 400`,
/// `cliff_time == u64::MAX - 450` (midpoint).  Withdraw-after-cliff assertion.
#[test]
fn midpoint_cliff_near_u64_max_exact_amounts() {
    let start = MAX_U64 - 500;
    let end = MAX_U64 - 400;
    let cliff = MAX_U64 - 450; // 50 s into a 100 s stream
    let deposit = 100i128;
    let pos = position_at(deposit, start, end, cliff, 0, None);

    // At cliff: 50 s elapsed of 100 s → 50 stroops.
    let v = math::vested(&pos, cliff).expect("overflow");
    assert_eq!(v, 50, "50 % of deposit vests at midpoint cliff");

    // At end: full deposit.
    assert_eq!(math::vested(&pos, end).expect("overflow"), 100);

    assert_conservation_across_lifecycle(&pos);
}

/// A position paused immediately after creation, then queried at `u64::MAX`.
/// stream_time must equal 0 (paused at start, no prior paused_total).
#[test]
fn paused_at_start_queried_at_u64_max() {
    let start = MAX_U64 - 1_000;
    let end = MAX_U64 - 500;
    let cliff = start + 100;
    let paused_at_time = start; // paused immediately
    let pos = position_at(1_000, start, end, cliff, 0, Some(paused_at_time));

    let st = math::stream_time(&pos, MAX_U64);
    assert_eq!(
        st, start,
        "stream_time frozen at paused_at when paused_total == 0"
    );

    // cliff not yet reached (stream_time == start < cliff).
    assert!(!math::cliff_reached(&pos, MAX_U64));

    let v = math::vested(&pos, MAX_U64).expect("overflow");
    assert_eq!(v, 0, "nothing vested while paused before cliff");

    assert_conservation(&pos, MAX_U64);
}
