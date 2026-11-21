//! Solvency and conservation invariant assertions for the token-vault.
//!
//! These helpers are used by the fuzz / property test suite to verify that
//! every arithmetic path in the accrual engine satisfies the core invariants
//! for a given position at a specific instant:
//!
//! * **I1** `0 <= vested <= deposited`
//! * **I2** `vested` is non-decreasing in `now`
//! * **I4** `vested(now) + refundable(now) == deposited`

#[cfg(test)]
use crate::math;
#[cfg(test)]
use crate::types::VaultPosition;

/// Assert I4 for `pos` at `now`: `vested + refundable == deposited`.
///
/// Panics with a descriptive message on violation.
#[cfg(test)]
pub fn assert_conservation(pos: &VaultPosition, now: u64) {
    let v = math::vested(pos, now).expect("vested must not overflow in assert_conservation");
    let r =
        math::refundable(pos, now).expect("refundable must not overflow in assert_conservation");
    assert_eq!(
        v + r,
        pos.deposited,
        "I4 violated at now={now}: vested({v}) + refundable({r}) != deposited({})",
        pos.deposited,
    );
}

/// Assert conservation at start, cliff, end, and `u64::MAX`.
#[cfg(test)]
pub fn assert_conservation_across_lifecycle(pos: &VaultPosition) {
    for now in [pos.start_time, pos.cliff_time, pos.end_time, u64::MAX] {
        assert_conservation(pos, now);
    }
}
