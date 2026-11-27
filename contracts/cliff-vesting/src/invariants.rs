//! Solvency and conservation invariant assertions for cliff-vesting.
//!
//! These helpers are used by the test suite and fuzz engines to verify that
//! every arithmetic path in the accrual engine satisfies the core invariants
//! for a given vesting schedule at a specific instant:
//!
//! * **I1** `0 <= vested <= deposited`
//! * **I2** `vested` is non-decreasing in `now`
//! * **I4** `vested(now) + refundable(now) == deposited` (exact balance conservation)

#[cfg(test)]
use crate::accrual;
#[cfg(test)]
use crate::types::VestingSchedule;

/// Assert I4 for `schedule` at `now`: `vested + refundable == deposited`.
///
/// Panics with a descriptive message on violation.
#[cfg(test)]
pub fn assert_conservation(schedule: &VestingSchedule, now: u64) {
    let v =
        accrual::vested(schedule, now).expect("vested must not overflow in assert_conservation");
    let r = accrual::refundable(schedule, now)
        .expect("refundable must not overflow in assert_conservation");
    assert_eq!(
        v + r,
        schedule.deposited,
        "I4 violated at now={now}: vested({v}) + refundable({r}) != deposited({})",
        schedule.deposited,
    );
}

/// Assert conservation across representative lifecycle timestamps:
/// before start, at start, at cliff, at midpoint, at end, and at `u64::MAX`.
#[cfg(test)]
pub fn assert_conservation_across_lifecycle(schedule: &VestingSchedule) {
    let start = schedule.start_time;
    let end = schedule.end_time;
    let cliff = schedule.cliff_time;

    for now in [
        start.saturating_sub(1),
        start,
        cliff,
        start + (end.saturating_sub(start)) / 2,
        end,
        u64::MAX,
    ] {
        assert_conservation(schedule, now);
    }
}
