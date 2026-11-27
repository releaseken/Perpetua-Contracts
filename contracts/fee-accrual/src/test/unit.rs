use super::common::Harness;
use crate::types::BatchAccrueItem;
use crate::FeeError;
use soroban_sdk::{Address, Vec};

#[test]
fn test_init_and_config() {
    let h = Harness::new();
    let config = h.client.get_config();
    assert_eq!(config.admin, h.admin);
    assert_eq!(config.protocol_fee_bps, 100);
    assert_eq!(config.keeper_split_bps, 2000);
    assert_eq!(config.paused, false);
}

#[test]
fn test_accrue_and_claims_flow() {
    let h = Harness::new();
    let stream_id = 101u64;
    let gross = 100_000i128; // 1% total fee = 1,000. keeper 20% = 200. protocol 80% = 800.

    let b = h.client.accrue_fee(&h.fee_collector, &stream_id, &gross);
    assert_eq!(b.total_fee, 1_000);
    assert_eq!(b.protocol_fee, 800);
    assert_eq!(b.keeper_fee, 200);
    assert_eq!(b.net_amount, 99_000);

    // Claim protocol fee partially
    let claimed_proto = h.client.claim_protocol_fees(&h.fee_admin, &stream_id, &500);
    assert_eq!(claimed_proto, 500);

    let rec = h.client.get_stream_fee_record(&stream_id);
    assert_eq!(rec.protocol_fee_accrued, 800);
    assert_eq!(rec.protocol_fee_claimed, 500);

    // Overclaim protocol fee fails
    let err_claim = h
        .client
        .try_claim_protocol_fees(&h.fee_admin, &stream_id, &301);
    assert_eq!(err_claim, Err(Ok(FeeError::InsufficientAccruedFees)));

    // Claim keeper fees
    let keeper_recipient = Address::generate(&h.env);
    let claimed_keeper =
        h.client
            .claim_keeper_fees(&h.fee_collector, &stream_id, &keeper_recipient, &200);
    assert_eq!(claimed_keeper, 200);

    let rec2 = h.client.get_stream_fee_record(&stream_id);
    assert_eq!(rec2.keeper_fee_claimed, 200);
}

#[test]
fn test_batch_accrue_fee_flow() {
    let h = Harness::new();
    let mut items = Vec::new(&h.env);
    items.push_back(BatchAccrueItem {
        stream_id: 1,
        amount: 10_000,
    });
    items.push_back(BatchAccrueItem {
        stream_id: 2,
        amount: 20_000,
    });
    items.push_back(BatchAccrueItem {
        stream_id: 3,
        amount: 30_000,
    });

    let res = h.client.batch_accrue_fee(&h.fee_collector, &items);
    assert_eq!(res.len(), 3);

    let rec1 = h.client.get_stream_fee_record(&1);
    assert_eq!(rec1.total_processed, 10_000);
    assert_eq!(rec1.protocol_fee_accrued, 80);

    let rec2 = h.client.get_stream_fee_record(&2);
    assert_eq!(rec2.total_processed, 20_000);
    assert_eq!(rec2.protocol_fee_accrued, 160);

    let rec3 = h.client.get_stream_fee_record(&3);
    assert_eq!(rec3.total_processed, 30_000);
    assert_eq!(rec3.protocol_fee_accrued, 240);
}

#[test]
fn test_settle_stream_blocks_further_accruals() {
    let h = Harness::new();
    let stream_id = 999u64;
    h.client.accrue_fee(&h.fee_collector, &stream_id, &10_000);

    h.client.settle_stream(&h.admin, &stream_id);
    let rec = h.client.get_stream_fee_record(&stream_id);
    assert!(rec.is_settled);

    let err = h
        .client
        .try_accrue_fee(&h.fee_collector, &stream_id, &5_000);
    assert_eq!(err, Err(Ok(FeeError::FeeAlreadySettled)));
}

#[test]
fn test_pause_unpause_lifecycle() {
    let h = Harness::new();
    h.client.set_paused(&h.emergency_admin, &true);

    let err = h.client.try_accrue_fee(&h.fee_collector, &1, &10_000);
    assert_eq!(err, Err(Ok(FeeError::ContractPaused)));

    h.client.set_paused(&h.emergency_admin, &false);
    let ok = h.client.try_accrue_fee(&h.fee_collector, &1, &10_000);
    assert!(ok.is_ok());
}
