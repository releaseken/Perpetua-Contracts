//! Issue #374 — fuzzing coverage for edge-case timestamps in stream-nft
//! near the `u64` maximum boundary limits.
//!
//! # Why `u64::MAX` timestamps and identifiers are a distinct risk
//!
//! Timestamps and lifecycle duration parameters in streams (`start_time`,
//! `end_time`, `cliff_time`, `paused_at`, `paused_total`), as well as the
//! token/stream identifiers `stream_id`, are typed as `u64`.
//!
//! At boundaries near `u64::MAX`:
//! * Subtractions such as `end_time - start_time` or `now - paused_total` could
//!   overflow or wrap without checked arithmetic.
//! * Clamped logic (`st.min(end).saturating_sub(start)`) must clamp cleanly to
//!   zero before start or to `duration` after maturity.
//! * Vested calculation `(deposited * elapsed) / duration` requires defensive checked
//!   multiplication and division to prevent panic traps or loss of funds.
//! * NFT minting and balance tracking over extreme token identifiers (`u64::MAX`,
//!   `u64::MAX - 1`) must succeed without corrupting storage maps or key prefixes.
//!
//! # Invariants under test
//!
//! * **I1 (Bounds)**: For any `now`, `0 <= vested <= deposited`.
//! * **I2 (Monotonicity)**: For any `t1 <= t2`, `vested(t1) <= vested(t2)`.
//! * **I3 (Conservation)**: `vested(t) + refundable(t) == deposited` holds exactly.
//! * **I4 (Cliff Step)**: If `stream_time < cliff_time`, `vested == 0`.
//! * **I5 (Maturity)**: At `stream_time >= end_time`, `vested == deposited`.
//! * **I6 (Boundary Safety)**: Calculations at `u64::MAX`, `u64::MAX - 1`, and `0`
//!   never overflow or wrap unexpectedly.

use proptest::prelude::*;

use super::common::*;
use crate::math;

const MAX_U64: u64 = u64::MAX;

// ---------------------------------------------------------------------------
// §1  Hand-crafted explicit boundary probes
// ---------------------------------------------------------------------------

#[test]
fn test_duration_at_u64_max_boundaries() {
    // Exact u64::MAX range
    let start = 0u64;
    let end = MAX_U64;
    assert_eq!(math::checked_duration(start, end).unwrap(), MAX_U64);

    // End == Start
    assert_eq!(math::checked_duration(MAX_U64, MAX_U64).unwrap(), 0);

    // End < Start returns ArithmeticOverflow
    assert!(math::checked_duration(MAX_U64, MAX_U64 - 1).is_err());
    assert!(math::checked_duration(1, 0).is_err());

    // High boundary interval
    let start_high = MAX_U64 - 1_000;
    let end_high = MAX_U64;
    assert_eq!(math::checked_duration(start_high, end_high).unwrap(), 1_000);
}

#[test]
fn test_stream_time_at_u64_max_boundaries() {
    // Unpaused at MAX_U64
    assert_eq!(math::stream_time(MAX_U64, None, 0), MAX_U64);
    assert_eq!(math::stream_time(MAX_U64, None, 100), MAX_U64 - 100);

    // Paused total exceeding now saturates to 0
    assert_eq!(math::stream_time(100, None, 200), 0);
    assert_eq!(math::stream_time(MAX_U64, None, MAX_U64), 0);

    // Paused at MAX_U64 freezes stream time
    assert_eq!(math::stream_time(100, Some(MAX_U64), 50), MAX_U64 - 50);
}

#[test]
fn test_elapsed_at_u64_max_boundaries() {
    let start = MAX_U64 - 1_000;
    let end = MAX_U64;

    // Before start
    assert_eq!(math::elapsed(start, end, start - 1, None, 0), 0);

    // Exactly at start
    assert_eq!(math::elapsed(start, end, start, None, 0), 0);

    // Partway through
    assert_eq!(math::elapsed(start, end, start + 400, None, 0), 400);

    // Exactly at end
    assert_eq!(math::elapsed(start, end, end, None, 0), 1_000);

    // Past end (saturates to duration)
    assert_eq!(math::elapsed(start, end, MAX_U64, None, 0), 1_000);
}

#[test]
fn test_cliff_reached_at_u64_max_boundaries() {
    let cliff = MAX_U64 - 100;

    assert!(!math::cliff_reached(cliff, cliff - 1, None, 0));
    assert!(math::cliff_reached(cliff, cliff, None, 0));
    assert!(math::cliff_reached(cliff, MAX_U64, None, 0));
}

#[test]
fn test_vesting_conservation_at_max_u64() {
    let start = MAX_U64 - 2_000;
    let end = MAX_U64;
    let cliff = MAX_U64 - 1_000;
    let deposit = 1_000_000i128;

    // Before cliff
    let v_pre = math::compute_vested(deposit, start, end, cliff, cliff - 1, None, 0).unwrap();
    let r_pre = math::compute_refundable(deposit, start, end, cliff, cliff - 1, None, 0).unwrap();
    assert_eq!(v_pre, 0);
    assert_eq!(r_pre, deposit);
    assert_eq!(v_pre + r_pre, deposit);

    // At cliff: 1000 / 2000 = 50%
    let v_cliff = math::compute_vested(deposit, start, end, cliff, cliff, None, 0).unwrap();
    let r_cliff = math::compute_refundable(deposit, start, end, cliff, cliff, None, 0).unwrap();
    assert_eq!(v_cliff, 500_000);
    assert_eq!(r_cliff, 500_000);
    assert_eq!(v_cliff + r_cliff, deposit);

    // At maturity
    let v_end = math::compute_vested(deposit, start, end, cliff, end, None, 0).unwrap();
    let r_end = math::compute_refundable(deposit, start, end, cliff, end, None, 0).unwrap();
    assert_eq!(v_end, deposit);
    assert_eq!(r_end, 0);
    assert_eq!(v_end + r_end, deposit);
}

// ---------------------------------------------------------------------------
// §2  Host-level NFT operations at u64 boundary timestamps
// ---------------------------------------------------------------------------

#[test]
fn test_host_nft_operations_at_u64_max_ledger_timestamps() {
    let h = Harness::new();

    // Advance ledger timestamp to MAX_U64 - 1000
    h.set_timestamp(MAX_U64 - 1_000);
    assert_eq!(h.now(), MAX_U64 - 1_000);

    // Mint token at stream_id near u64::MAX
    let max_stream_id = MAX_U64 - 42;
    h.client.mint(&max_stream_id);

    assert_eq!(h.client.owner_of(&max_stream_id), Some(h.user1.clone()));
    assert_eq!(h.client.balance_of(&h.user1), 1);
    assert_eq!(h.client.total_supply(), 1);

    // Advance clock to MAX_U64
    h.set_timestamp(MAX_U64);
    assert_eq!(h.now(), MAX_U64);

    // Query owner at maximum timestamp
    assert_eq!(h.client.owner_of(&max_stream_id), Some(h.user1.clone()));
    assert_eq!(h.client.balance_of(&h.user1), 1);
}

// ---------------------------------------------------------------------------
// §3  Proptest-driven fuzzing at the u64 boundary
// ---------------------------------------------------------------------------

proptest! {
    /// Fuzz arbitrary timestamps near `u64::MAX`.
    /// Verifies invariants I1, I2, I3, I4, I5 without panicking or arithmetic wraps.
    #[test]
    fn fuzz_timestamps_near_u64_max(
        duration in 1u64..=100_000u64,
        start_offset in 0u64..=1_000_000u64,
        cliff_offset in 0u64..=100_000u64,
        deposit in 1i128..=1_000_000i128,
        probe_offset in 0u64..=200_000u64,
    ) {
        let start = MAX_U64.saturating_sub(start_offset + duration + 1);
        let end = start.saturating_add(duration);
        prop_assume!(end > start);

        let cliff = start.saturating_add(cliff_offset.min(duration));
        prop_assume!(cliff >= start && cliff <= end);

        let actual_duration = end - start;
        prop_assume!(deposit.checked_mul(actual_duration as i128).is_some());

        // Probe 1: before start
        let now_before = start.saturating_sub(1);
        let v_before = math::compute_vested(deposit, start, end, cliff, now_before, None, 0)
            .expect("should not overflow before start");
        prop_assert_eq!(v_before, 0);

        // Probe 2: at start
        let v_start = math::compute_vested(deposit, start, end, cliff, start, None, 0)
            .expect("should not overflow at start");
        prop_assert!(v_start >= 0 && v_start <= deposit);

        // Probe 3: at cliff
        let v_cliff = math::compute_vested(deposit, start, end, cliff, cliff, None, 0)
            .expect("should not overflow at cliff");
        prop_assert!(v_cliff >= 0 && v_cliff <= deposit);
        if cliff > start {
            let v_pre_cliff = math::compute_vested(deposit, start, end, cliff, cliff - 1, None, 0)
                .expect("should not overflow pre-cliff");
            prop_assert_eq!(v_pre_cliff, 0, "I4: pre-cliff vested must be 0");
        }

        // Probe 4: at end
        let v_end = math::compute_vested(deposit, start, end, cliff, end, None, 0)
            .expect("should not overflow at end");
        prop_assert_eq!(v_end, deposit, "I5: at end vested must equal deposited");

        // Probe 5: at u64::MAX
        let v_max = math::compute_vested(deposit, start, end, cliff, MAX_U64, None, 0)
            .expect("should not overflow at u64::MAX");
        prop_assert_eq!(v_max, deposit, "I5: past end vested must equal deposited");

        // Probe 6: arbitrary probe point — monotonicity
        let now_probe = start.saturating_add(probe_offset.min(duration));
        let v_probe = math::compute_vested(deposit, start, end, cliff, now_probe, None, 0)
            .expect("should not overflow at probe");
        prop_assert!(v_probe >= 0 && v_probe <= deposit, "I1 bounds violated");

        if now_probe >= start {
            prop_assert!(v_probe >= v_start, "I2 monotonicity violated");
        }

        // I3 Conservation check at probe points
        for now in [now_before, start, cliff, end, MAX_U64, now_probe] {
            let v = math::compute_vested(deposit, start, end, cliff, now, None, 0)
                .expect("vested error");
            let r = math::compute_refundable(deposit, start, end, cliff, now, None, 0)
                .expect("refundable error");
            prop_assert_eq!(v + r, deposit, "I3 conservation broken: v={} r={} dep={}", v, r, deposit);
        }
    }

    /// Fuzz paused streams near the u64 boundary
    #[test]
    fn fuzz_paused_stream_at_u64_max_boundary(
        duration in 1_000u64..=100_000u64,
        cliff_offset in 1u64..=999u64,
        paused_offset in 1u64..=cliff_offset,
        paused_duration in 0u64..=1_000u64,
        deposit in 1_000i128..=1_000_000i128,
    ) {
        let start = MAX_U64 / 2;
        let end = start.saturating_add(duration);
        prop_assume!(end > start && end < MAX_U64);

        let cliff = start.saturating_add(cliff_offset);
        prop_assume!(cliff <= end);

        let paused_at_time = start + paused_offset.min(cliff_offset - 1);
        prop_assume!(paused_at_time < cliff);

        let paused_total = paused_duration.min(paused_at_time);

        // Effective stream time must be frozen before cliff
        let st = math::stream_time(cliff + 50_000, Some(paused_at_time), paused_total);
        prop_assert!(st < cliff);
        prop_assert!(!math::cliff_reached(cliff, cliff + 50_000, Some(paused_at_time), paused_total));

        let v = math::compute_vested(deposit, start, end, cliff, cliff + 50_000, Some(paused_at_time), paused_total)
            .expect("should not overflow");
        prop_assert_eq!(v, 0);

        let r = math::compute_refundable(deposit, start, end, cliff, cliff + 50_000, Some(paused_at_time), paused_total)
            .expect("should not overflow");
        prop_assert_eq!(r, deposit);
    }
}
