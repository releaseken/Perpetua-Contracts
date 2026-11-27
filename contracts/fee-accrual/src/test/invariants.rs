use super::common::Harness;
use crate::invariants::{
    assert_accrual_monotonicity, assert_batch_bounds, assert_breakdown_conservation,
    assert_record_solvency, assert_valid_config, MAX_BATCH_SIZE,
};
use crate::math::compute_fee_breakdown;
use crate::types::{AccruedFeeBreakdown, BatchAccrueItem, StreamFeeRecord};
use crate::FeeError;
use soroban_sdk::Vec;

#[test]
fn test_conservation_invariant_across_various_rates() {
    for gross in [0i128, 1, 10, 100, 10_000, 1_000_000, 10_000_000_000i128] {
        for proto_bps in [0u32, 1, 100, 500, 1000, 2500, 10_000] {
            for keeper_bps in [0u32, 500, 2000, 5000, 10_000] {
                let b = compute_fee_breakdown(gross, proto_bps, keeper_bps).unwrap();
                assert_breakdown_conservation(&b).expect("conservation must hold");
                assert_eq!(b.protocol_fee + b.keeper_fee, b.total_fee);
                assert_eq!(b.net_amount + b.total_fee, gross);
            }
        }
    }
}

#[test]
fn test_invariant_breakdown_conservation_rejects_corrupted() {
    let mut bad_b = AccruedFeeBreakdown {
        gross_amount: 10_000,
        total_fee: 100,
        protocol_fee: 70, // Corrupted: 70 + 20 != 100
        keeper_fee: 20,
        net_amount: 9_900,
    };
    assert_eq!(
        assert_breakdown_conservation(&bad_b),
        Err(FeeError::InvariantViolation)
    );

    bad_b.protocol_fee = 80;
    bad_b.net_amount = 9_899; // Corrupted: 9899 + 100 != 10000
    assert_eq!(
        assert_breakdown_conservation(&bad_b),
        Err(FeeError::InvariantViolation)
    );
}

#[test]
fn test_record_solvency_and_monotonicity_invariants() {
    let old_rec = StreamFeeRecord {
        stream_id: 1,
        total_processed: 10_000,
        protocol_fee_accrued: 80,
        keeper_fee_accrued: 20,
        protocol_fee_claimed: 0,
        keeper_fee_claimed: 0,
        is_settled: false,
    };
    assert_record_solvency(&old_rec).unwrap();

    let valid_next = StreamFeeRecord {
        stream_id: 1,
        total_processed: 20_000,
        protocol_fee_accrued: 160,
        keeper_fee_accrued: 40,
        protocol_fee_claimed: 50,
        keeper_fee_claimed: 10,
        is_settled: false,
    };
    assert_record_solvency(&valid_next).unwrap();
    assert_accrual_monotonicity(&old_rec, &valid_next).unwrap();

    // Insolvent: claimed > accrued
    let insolvent = StreamFeeRecord {
        protocol_fee_claimed: 200,
        ..valid_next.clone()
    };
    assert_eq!(
        assert_record_solvency(&insolvent),
        Err(FeeError::InvariantViolation)
    );

    // Non-monotonic regression
    let regressed = StreamFeeRecord {
        total_processed: 5_000,
        ..valid_next.clone()
    };
    assert_eq!(
        assert_accrual_monotonicity(&old_rec, &regressed),
        Err(FeeError::InvariantViolation)
    );
}

#[test]
fn test_batch_bounds_invariant() {
    assert_eq!(assert_batch_bounds(0), Err(FeeError::EmptyBatch));
    assert_eq!(
        assert_batch_bounds(MAX_BATCH_SIZE + 1),
        Err(FeeError::BatchTooLarge)
    );
    assert!(assert_batch_bounds(1).is_ok());
    assert!(assert_batch_bounds(MAX_BATCH_SIZE).is_ok());
}

#[test]
fn test_fuzz_monotonicity_under_sequential_accruals() {
    let h = Harness::new();
    let mut total_proto = 0i128;
    let mut total_keeper = 0i128;
    let stream_id = 42u64;

    for i in 1..=20 {
        let gross = i * 1_000;
        let b = h.client.accrue_fee(&h.fee_collector, &stream_id, &gross);
        total_proto += b.protocol_fee;
        total_keeper += b.keeper_fee;

        let rec = h.client.get_stream_fee_record(&stream_id);
        assert_eq!(rec.protocol_fee_accrued, total_proto);
        assert_eq!(rec.keeper_fee_accrued, total_keeper);
        assert!(rec.protocol_fee_claimed <= rec.protocol_fee_accrued);
        assert!(rec.keeper_fee_claimed <= rec.keeper_fee_accrued);

        let (glob_proto, glob_keeper) = h.client.get_total_fees();
        assert_eq!(glob_proto, total_proto);
        assert_eq!(glob_keeper, total_keeper);
    }
}
