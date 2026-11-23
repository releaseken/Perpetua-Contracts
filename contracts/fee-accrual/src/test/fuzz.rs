//! Deterministic fuzz and boundary tests for `fee-accrual` math, checked
//! arithmetic, and TTL ledger calculations (issue #390).

use crate::math::compute_fee_breakdown;
use crate::storage::{
    seconds_to_ledgers, ENTRY_LIFETIME_THRESHOLD, MIN_ENTRY_TTL_LEDGERS, SECONDS_PER_LEDGER,
    TTL_BUFFER_SECONDS,
};
use crate::FeeError;

#[test]
fn fuzz_fee_breakdown_conservation_and_bounds() {
    let amounts: [i128; 10] = [
        0,
        1,
        7,
        9_999,
        10_000,
        10_001,
        1_000_000_000,
        10_000_000_000_000_000_000,
        i128::MAX / 10_000,
        (i128::MAX / 10_000) - 7,
    ];
    let proto_rates: [u32; 7] = [0, 1, 50, 250, 1_000, 5_000, 10_000];
    let keeper_splits: [u32; 7] = [0, 1, 100, 2_500, 5_000, 9_999, 10_000];

    for &gross in &amounts {
        for &proto_bps in &proto_rates {
            for &keeper_bps in &keeper_splits {
                let res = compute_fee_breakdown(gross, proto_bps, keeper_bps)
                    .expect("valid domain must not overflow");

                assert_eq!(res.gross_amount, gross);
                assert!(res.total_fee >= 0 && res.total_fee <= gross);
                assert!(res.protocol_fee >= 0 && res.keeper_fee >= 0);
                assert_eq!(
                    res.protocol_fee + res.keeper_fee,
                    res.total_fee,
                    "protocol_fee + keeper_fee must equal total_fee"
                );
                assert_eq!(
                    res.net_amount + res.total_fee,
                    gross,
                    "net_amount + total_fee must equal gross_amount"
                );
            }
        }
    }
}

#[test]
fn fuzz_fee_breakdown_rejects_negative_and_overflow_inputs() {
    assert_eq!(
        compute_fee_breakdown(-1, 100, 1_000),
        Err(FeeError::ArithmeticOverflow)
    );
    assert_eq!(
        compute_fee_breakdown(i128::MIN, 100, 1_000),
        Err(FeeError::ArithmeticOverflow)
    );
    assert_eq!(
        compute_fee_breakdown(i128::MAX, 2, 1_000),
        Err(FeeError::ArithmeticOverflow)
    );
    assert_eq!(
        compute_fee_breakdown(1_000, 10_001, 1_000),
        Err(FeeError::InvalidBpsRate)
    );
    assert_eq!(
        compute_fee_breakdown(1_000, 1_000, 10_001),
        Err(FeeError::InvalidBpsRate)
    );
}

#[test]
fn fuzz_seconds_to_ledgers_ceiling_and_saturation_invariants() {
    assert_eq!(seconds_to_ledgers(0), 0);
    assert_eq!(seconds_to_ledgers(1), 1);
    assert_eq!(seconds_to_ledgers(SECONDS_PER_LEDGER), 1);
    assert_eq!(seconds_to_ledgers(SECONDS_PER_LEDGER + 1), 2);
    assert_eq!(seconds_to_ledgers(u64::MAX), u32::MAX);

    // Sample durations across [1, 365 days] and verify ceiling property:
    // ledgers * SECONDS_PER_LEDGER >= seconds.
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..256 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let secs = seed % (365 * 86_400);
        let ledgers = seconds_to_ledgers(secs) as u64;
        assert!(
            ledgers.saturating_mul(SECONDS_PER_LEDGER) >= secs,
            "seconds_to_ledgers({secs}) = {ledgers} underestimated wall-clock window"
        );
    }

    // Verify 30-day + 10% drift floor constant.
    assert_eq!(MIN_ENTRY_TTL_LEDGERS, 570_240);
    assert_eq!(ENTRY_LIFETIME_THRESHOLD, MIN_ENTRY_TTL_LEDGERS);
    assert!(
        (MIN_ENTRY_TTL_LEDGERS as u64) * SECONDS_PER_LEDGER >= TTL_BUFFER_SECONDS,
        "MIN_ENTRY_TTL_LEDGERS must cover at least TTL_BUFFER_SECONDS"
    );
}
