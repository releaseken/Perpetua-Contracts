//! Test suite for the fee-accrual contract.

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{Address, Env, Vec};

use crate::{
    compute_fee_breakdown, BatchAccrueItem, FeeAccrual, FeeAccrualClient, FeeError, MAX_BATCH_SIZE,
};

pub const T0: u64 = 1_700_000_000;

pub struct Harness<'a> {
    pub env: Env,
    pub client: FeeAccrualClient<'a>,
    pub contract_id: Address,
    pub admin: Address,
    pub fee_admin: Address,
    pub fee_collector: Address,
    pub emergency_admin: Address,
    pub fee_recipient: Address,
}

impl<'a> Harness<'a> {
    pub fn new() -> Harness<'a> {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(T0);

        let contract_id = env.register(FeeAccrual, ());
        let client = FeeAccrualClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let fee_admin = Address::generate(&env);
        let fee_collector = Address::generate(&env);
        let emergency_admin = Address::generate(&env);
        let fee_recipient = Address::generate(&env);

        client.init(
            &admin,
            &fee_admin,
            &fee_collector,
            &emergency_admin,
            &fee_recipient,
            &100,  // 1% protocol fee
            &2000, // 20% keeper split
        );

        Harness {
            env,
            client,
            contract_id,
            admin,
            fee_admin,
            fee_collector,
            emergency_admin,
            fee_recipient,
        }
    }
}

#[test]
fn test_initialization() {
    let h = Harness::new();
    let config = h.client.get_config();
    assert_eq!(config.admin, h.admin);
    assert_eq!(config.fee_admin, h.fee_admin);
    assert_eq!(config.fee_collector, h.fee_collector);
    assert_eq!(config.emergency_admin, h.emergency_admin);
    assert_eq!(config.fee_recipient, h.fee_recipient);
    assert_eq!(config.protocol_fee_bps, 100);
    assert_eq!(config.keeper_split_bps, 2000);
    assert!(!config.paused);

    assert_eq!(h.client.get_total_protocol_fees(), 0);
    assert_eq!(h.client.get_total_keeper_fees(), 0);
}

#[test]
fn test_reinitialization_fails() {
    let h = Harness::new();
    let res = h.client.try_init(
        &h.admin,
        &h.fee_admin,
        &h.fee_collector,
        &h.emergency_admin,
        &h.fee_recipient,
        &100,
        &2000,
    );
    assert!(res.is_err());
}

#[test]
fn test_single_accrue_fee() {
    let h = Harness::new();
    let breakdown = h.client.accrue_fee(&1, &10_000);
    assert_eq!(breakdown.gross_amount, 10_000);
    assert_eq!(breakdown.total_fee, 100);
    assert_eq!(breakdown.keeper_fee, 20);
    assert_eq!(breakdown.protocol_fee, 80);
    assert_eq!(breakdown.net_amount, 9_900);

    let rec = h.client.get_stream_fee(&1);
    assert_eq!(rec.stream_id, 1);
    assert_eq!(rec.total_processed, 10_000);
    assert_eq!(rec.protocol_fee_accrued, 80);
    assert_eq!(rec.keeper_fee_accrued, 20);
    assert_eq!(rec.protocol_fee_claimed, 0);
    assert_eq!(rec.keeper_fee_claimed, 0);
    assert!(!rec.is_settled);

    assert_eq!(h.client.get_total_protocol_fees(), 80);
    assert_eq!(h.client.get_total_keeper_fees(), 20);
}

#[test]
fn test_batch_accrue_fees_optimized() {
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

    let breakdown = h.client.batch_accrue_fees(&items);
    assert_eq!(breakdown.gross_amount, 60_000);
    assert_eq!(breakdown.total_fee, 600);
    assert_eq!(breakdown.keeper_fee, 120);
    assert_eq!(breakdown.protocol_fee, 480);
    assert_eq!(breakdown.net_amount, 59_400);

    assert_eq!(h.client.get_total_protocol_fees(), 480);
    assert_eq!(h.client.get_total_keeper_fees(), 120);

    let rec1 = h.client.get_stream_fee(&1);
    assert_eq!(rec1.total_processed, 10_000);
    assert_eq!(rec1.protocol_fee_accrued, 80);
    assert_eq!(rec1.keeper_fee_accrued, 20);

    let rec2 = h.client.get_stream_fee(&2);
    assert_eq!(rec2.total_processed, 20_000);
    assert_eq!(rec2.protocol_fee_accrued, 160);
    assert_eq!(rec2.keeper_fee_accrued, 40);

    let rec3 = h.client.get_stream_fee(&3);
    assert_eq!(rec3.total_processed, 30_000);
    assert_eq!(rec3.protocol_fee_accrued, 240);
    assert_eq!(rec3.keeper_fee_accrued, 60);
}

#[test]
fn test_batch_accrue_empty_rejected() {
    let h = Harness::new();
    let items = Vec::new(&h.env);
    let res = h.client.try_batch_accrue_fees(&items);
    assert!(res.is_err());
}

#[test]
fn test_batch_accrue_duplicates_rejected() {
    let h = Harness::new();
    let mut items = Vec::new(&h.env);
    items.push_back(BatchAccrueItem {
        stream_id: 1,
        amount: 10_000,
    });
    items.push_back(BatchAccrueItem {
        stream_id: 1,
        amount: 5_000,
    });
    let res = h.client.try_batch_accrue_fees(&items);
    assert!(res.is_err());
}

#[test]
fn test_batch_accrue_max_batch_size_exceeded() {
    let h = Harness::new();
    let mut items = Vec::new(&h.env);
    for i in 1..=(MAX_BATCH_SIZE + 1) {
        items.push_back(BatchAccrueItem {
            stream_id: i as u64,
            amount: 1_000,
        });
    }
    let res = h.client.try_batch_accrue_fees(&items);
    assert!(res.is_err());
}

#[test]
fn test_batch_settle_fees() {
    let h = Harness::new();
    let mut items = Vec::new(&h.env);
    items.push_back(BatchAccrueItem {
        stream_id: 10,
        amount: 10_000,
    });
    items.push_back(BatchAccrueItem {
        stream_id: 20,
        amount: 10_000,
    });
    h.client.batch_accrue_fees(&items);

    let mut settle_ids = Vec::new(&h.env);
    settle_ids.push_back(10);
    settle_ids.push_back(20);

    let count = h.client.batch_settle_fees(&settle_ids);
    assert_eq!(count, 2);

    let rec1 = h.client.get_stream_fee(&10);
    assert!(rec1.is_settled);
    let rec2 = h.client.get_stream_fee(&20);
    assert!(rec2.is_settled);

    // Accruing to settled stream fails
    let res = h.client.try_accrue_fee(&10, &5_000);
    assert!(res.is_err());
}

#[test]
fn test_claim_stream_fee() {
    let h = Harness::new();
    h.client.accrue_fee(&1, &100_000); // 1,000 total fee: 800 proto, 200 keeper

    let claimed = h.client.claim_stream_fee(&1, &true, &true);
    assert_eq!(claimed, (800, 200));

    let rec = h.client.get_stream_fee(&1);
    assert_eq!(rec.protocol_fee_claimed, 800);
    assert_eq!(rec.keeper_fee_claimed, 200);

    // Repeated claim fails with ZeroFeeAccrual
    let res = h.client.try_claim_stream_fee(&1, &true, &true);
    assert!(res.is_err());
}

#[test]
fn test_pause_unpause() {
    let h = Harness::new();
    h.client.pause();
    assert!(h.client.get_config().paused);

    // Accrue while paused fails
    let res = h.client.try_accrue_fee(&1, &10_000);
    assert!(res.is_err());

    h.client.unpause();
    assert!(!h.client.get_config().paused);

    // Accrue succeeds after unpause
    let breakdown = h.client.accrue_fee(&1, &10_000);
    assert_eq!(breakdown.gross_amount, 10_000);
}

#[test]
fn test_admin_rotation() {
    let h = Harness::new();
    let new_admin = Address::generate(&h.env);

    h.client.propose_admin(&new_admin);
    assert_eq!(h.client.pending_admin(), Some(new_admin.clone()));

    h.client.accept_admin();
    assert_eq!(h.client.get_config().admin, new_admin);
    assert_eq!(h.client.pending_admin(), None);
}

#[test]
fn test_error_messages() {
    let env = Env::default();
    let contract_id = env.register(FeeAccrual, ());
    let client = FeeAccrualClient::new(&env, &contract_id);

    let msg1 = client.error_message(&1);
    assert_eq!(
        msg1,
        soroban_sdk::String::from_str(&env, FeeError::NotInitialized.message())
    );
    let msg17 = client.error_message(&17);
    assert_eq!(
        msg17,
        soroban_sdk::String::from_str(&env, FeeError::BatchTooLarge.message())
    );
}
