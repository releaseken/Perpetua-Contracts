//! Unit and fuzz tests for fractional stream withdrawal precision in fee-accrual.

use proptest::prelude::*;

use crate::math::{calculate_bps_payout, calculate_fractional_payout, FractionalSplit};

#[test]
fn test_fractional_withdrawal_exact_conservation() {
    let total = 1000_i128;
    let split = calculate_fractional_payout(total, 1, 3).unwrap();
    assert_eq!(split.payout, 333);
    assert_eq!(split.remainder, 667);
    assert_eq!(split.payout + split.remainder, total);
}

#[test]
fn test_fractional_withdrawal_zero_and_full() {
    let total = 500_i128;
    let zero_split = calculate_fractional_payout(total, 0, 100).unwrap();
    assert_eq!(zero_split.payout, 0);
    assert_eq!(zero_split.remainder, total);

    let full_split = calculate_fractional_payout(total, 100, 100).unwrap();
    assert_eq!(full_split.payout, total);
    assert_eq!(full_split.remainder, 0);
}

#[test]
fn test_bps_payout() {
    let total = 10_000_i128;
    let split = calculate_bps_payout(total, 250).unwrap(); // 2.5%
    assert_eq!(split.payout, 250);
    assert_eq!(split.remainder, 9750);
}

proptest! {
    #[test]
    fn prop_fractional_payout_conservation(
        total in 0_i128..=1_000_000_000_000_000_i128,
        denominator in 1_i128..=10_000_i128,
        numerator_factor in 0_i128..=10_000_i128,
    ) {
        let numerator = (numerator_factor * denominator) / 10_000;
        let split = calculate_fractional_payout(total, numerator, denominator).unwrap();

        prop_assert_eq!(split.payout + split.remainder, total);
        prop_assert!(split.payout >= 0);
        prop_assert!(split.remainder >= 0);
        prop_assert!(split.payout <= total);
        prop_assert!(split.remainder <= total);
    }
}
