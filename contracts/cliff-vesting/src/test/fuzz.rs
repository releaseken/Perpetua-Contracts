//! Fuzz / property tests for the cliff-vesting accrual engine.
//!
//! These tests use `proptest` to verify that the core invariants hold across
//! a wide range of synthetic schedule parameters.  Running with
//! `PROPTEST_CASES=10000` on CI provides meaningful coverage depth without
//! inflating the default run time.

use proptest::prelude::*;

use crate::accrual;
use crate::types::{ScheduleStatus, VestingSchedule};

/// Build a minimal `VestingSchedule` for property testing.  All timing
/// parameters are expressed in seconds relative to a notional epoch of 0.
fn make_schedule(
    deposited: i128,
    start_time: u64,
    duration: u64,
    cliff_offset: u64,
    paused_total: u64,
) -> VestingSchedule {
    let end_time = start_time.saturating_add(duration);
    let cliff_time = start_time.saturating_add(cliff_offset.min(duration));
    VestingSchedule {
        grantor: soroban_sdk::Address::from_string(
            &soroban_sdk::Env::default(),
            &soroban_sdk::String::from_str(
                &soroban_sdk::Env::default(),
                "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
            ),
        ),
        beneficiary: soroban_sdk::Address::from_string(
            &soroban_sdk::Env::default(),
            &soroban_sdk::String::from_str(
                &soroban_sdk::Env::default(),
                "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
            ),
        ),
        token: soroban_sdk::Address::from_string(
            &soroban_sdk::Env::default(),
            &soroban_sdk::String::from_str(
                &soroban_sdk::Env::default(),
                "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
            ),
        ),
        deposited,
        withdrawn: 0,
        start_time,
        end_time,
        cliff_time,
        paused_at: None,
        paused_total: paused_total.min(start_time), // paused_total <= start_time keeps math sound
        status: ScheduleStatus::Active,
    }
}

proptest! {
    /// I1: `0 <= vested <= deposited`
    #[test]
    fn prop_vested_in_bounds(
        deposited in 1_i128..=i128::MAX / 2,
        start_time in 0_u64..=1_000_000_u64,
        duration in 1_u64..=10_000_000_u64,
        cliff_offset in 0_u64..=10_000_000_u64,
        now_offset in 0_u64..=10_000_001_u64,
    ) {
        let s = make_schedule(deposited, start_time, duration, cliff_offset, 0);
        // Guard: deposited * duration must fit in i128 (matches creation validation).
        if s.deposited.checked_mul(duration as i128).is_none() {
            return Ok(());
        }
        let now = start_time.saturating_add(now_offset);
        if let Ok(v) = accrual::vested(&s, now) {
            prop_assert!(v >= 0, "vested must be non-negative");
            prop_assert!(v <= deposited, "vested must not exceed deposited");
        }
    }

    /// I4: `vested + refundable == deposited`
    #[test]
    fn prop_conservation_exact(
        deposited in 1_i128..=i128::MAX / 2,
        start_time in 0_u64..=1_000_000_u64,
        duration in 1_u64..=10_000_000_u64,
        cliff_offset in 0_u64..=10_000_000_u64,
        now_offset in 0_u64..=10_000_001_u64,
    ) {
        let s = make_schedule(deposited, start_time, duration, cliff_offset, 0);
        if s.deposited.checked_mul(duration as i128).is_none() {
            return Ok(());
        }
        let now = start_time.saturating_add(now_offset);
        if let (Ok(v), Ok(r)) = (accrual::vested(&s, now), accrual::refundable(&s, now)) {
            prop_assert_eq!(
                v + r,
                deposited,
                "conservation identity broken: vested={v} refundable={r} deposited={deposited}"
            );
        }
    }

    /// Cliff gate: nothing withdrawable before cliff_time.
    #[test]
    fn prop_zero_before_cliff(
        deposited in 1_i128..=1_000_000_000_i128,
        start_time in 0_u64..=1_000_000_u64,
        duration in 100_u64..=10_000_000_u64,
        cliff_offset in 1_u64..=10_000_000_u64,
        pre_cliff_offset in 0_u64..=9_999_999_u64,
    ) {
        let s = make_schedule(deposited, start_time, duration, cliff_offset, 0);
        if s.deposited.checked_mul(duration as i128).is_none() {
            return Ok(());
        }
        // Now is strictly before the cliff.
        let cliff = s.cliff_time;
        if pre_cliff_offset >= cliff.saturating_sub(start_time) || cliff == start_time {
            // Can't construct a "before cliff" time — skip.
            return Ok(());
        }
        let now = start_time.saturating_add(pre_cliff_offset);
        if now >= cliff {
            return Ok(());
        }
        if let Ok(v) = accrual::vested(&s, now) {
            prop_assert_eq!(v, 0, "vested must be 0 before cliff");
        }
    }

    /// Monotonicity: `vested(t1) <= vested(t2)` for `t1 <= t2`.
    #[test]
    fn prop_vested_monotone(
        deposited in 1_i128..=1_000_000_000_i128,
        start_time in 0_u64..=1_000_000_u64,
        duration in 1_u64..=10_000_000_u64,
        cliff_offset in 0_u64..=10_000_000_u64,
        t1_offset in 0_u64..=5_000_000_u64,
        t2_extra in 0_u64..=5_000_000_u64,
    ) {
        let s = make_schedule(deposited, start_time, duration, cliff_offset, 0);
        if s.deposited.checked_mul(duration as i128).is_none() {
            return Ok(());
        }
        let t1 = start_time.saturating_add(t1_offset);
        let t2 = t1.saturating_add(t2_extra);
        if let (Ok(v1), Ok(v2)) = (accrual::vested(&s, t1), accrual::vested(&s, t2)) {
            prop_assert!(v2 >= v1, "vested is not monotone: vested({t1})={v1} > vested({t2})={v2}");
        }
    }
}
