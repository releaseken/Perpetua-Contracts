use super::common::Harness;
use crate::{DataKey, FeeError};
use soroban_sdk::Address;

#[test]
fn executing_flag_is_cleared_after_accrue_fee() {
    let h = Harness::new();
    let breakdown = h.client.accrue_fee(&h.fee_collector, &1, &10_000);
    assert_eq!(breakdown.gross_amount, 10_000);
    assert_eq!(breakdown.total_fee, 100);
    assert_eq!(breakdown.keeper_fee, 20);
    assert_eq!(breakdown.protocol_fee, 80);
    assert_eq!(breakdown.net_amount, 9_900);

    let executing: Option<bool> = h.env.as_contract(&h.contract_id, || {
        h.env.storage().instance().get(&DataKey::Executing)
    });
    assert!(executing.is_none() || executing == Some(false));
}

#[test]
fn executing_flag_rejects_reentrant_call() {
    let h = Harness::new();

    // Manually set Executing to true in instance storage
    h.env.as_contract(&h.contract_id, || {
        h.env.storage().instance().set(&DataKey::Executing, &true);
    });

    let res = h.client.try_accrue_fee(&h.fee_collector, &1, &10_000);
    assert_eq!(res, Err(Ok(FeeError::ReentrancyGuard)));

    let claim_res = h.client.try_claim_protocol_fees(&h.fee_admin, &1, &50);
    assert_eq!(claim_res, Err(Ok(FeeError::ReentrancyGuard)));

    let keeper_res =
        h.client
            .try_claim_keeper_fees(&h.fee_collector, &1, &Address::generate(&h.env), &10);
    assert_eq!(keeper_res, Err(Ok(FeeError::ReentrancyGuard)));
}

#[test]
fn executing_flag_cleared_on_inner_failure() {
    let h = Harness::new();

    // Attempt invalid stream_id 0
    let res = h.client.try_accrue_fee(&h.fee_collector, &0, &10_000);
    assert_eq!(res, Err(Ok(FeeError::InvalidStreamId)));

    // Ensure Executing flag is cleared
    let executing: Option<bool> = h.env.as_contract(&h.contract_id, || {
        h.env.storage().instance().get(&DataKey::Executing)
    });
    assert!(executing.is_none() || executing == Some(false));

    // Next call succeeds
    let res2 = h.client.try_accrue_fee(&h.fee_collector, &1, &10_000);
    assert!(res2.is_ok());
}
