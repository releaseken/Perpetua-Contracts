//! Byzantine fault tolerance and network partition simulations for `fee-accrual`
//! (issue #387, #390).
//!
//! Verifies that:
//! 1. Malformed or duplicate-ID batches during partition recovery are atomic
//!    and leave zero partial state or TTL side effects.
//! 2. Emergency pause halts accrual and claims during an incident while still
//!    allowing permissionless keeper TTL sweeps so entries never archive while
//!    paused.

use soroban_sdk::Vec;

use crate::{BatchAccrueItem, FeeError};

use super::common::Harness;

#[test]
fn malformed_partition_batch_is_atomic_and_leaves_no_partial_state() {
    let h = Harness::new();

    // First valid accrual establishes state for stream 1.
    h.client.accrue_fee(&h.fee_collector, &1_u64, &10_000_i128);
    let proto_before = h.client.total_protocol_fees();
    let keeper_before = h.client.total_keeper_fees();

    // A faulty batch containing a duplicate stream ID (stream 2 twice) must revert atomically.
    let mut dup_batch: Vec<BatchAccrueItem> = Vec::new(&h.env);
    dup_batch.push_back(BatchAccrueItem {
        stream_id: 2,
        amount: 50_000,
    });
    dup_batch.push_back(BatchAccrueItem {
        stream_id: 2,
        amount: 50_000,
    });

    let err = h
        .client
        .try_batch_accrue_fees(&h.fee_collector, &dup_batch)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, FeeError::InvalidStreamId);
    assert!(!h.entry_exists(2), "stream 2 must not be written on batch rejection");
    assert_eq!(h.client.total_protocol_fees(), proto_before);
    assert_eq!(h.client.total_keeper_fees(), keeper_before);
}

#[test]
fn keeper_ttl_sweeps_succeed_while_contract_is_paused_during_incident() {
    let h = Harness::new();
    let expected_floor = h.expected_entry_ttl();

    h.client.accrue_fee(&h.fee_collector, &5_u64, &100_000_i128);

    // Emergency admin pauses the contract during a prolonged incident.
    h.client.pause(&h.emergency_admin);

    // Accruals and claims are blocked while paused.
    assert_eq!(
        h.client
            .try_accrue_fee(&h.fee_collector, &5_u64, &10_000_i128)
            .unwrap_err()
            .unwrap(),
        FeeError::ContractPaused
    );
    assert_eq!(
        h.client
            .try_claim_protocol_fee(&h.fee_recipient, &5_u64, &None)
            .unwrap_err()
            .unwrap(),
        FeeError::ContractPaused
    );

    // Even after 90 days of pause, keepers can still extend persistent entry TTLs
    // so accrued fee records never archive while the circuit breaker is engaged.
    h.advance(90 * 86_400);
    let target = h.client.extend_stream_fee_ttl(&5_u64);
    assert_eq!(target, expected_floor);
    assert!(h.ttl_of(5) >= expected_floor);

    // Once unpaused, accrued fees remain intact and claimable.
    h.client.unpause(&h.emergency_admin);
    let claimed = h
        .client
        .claim_protocol_fee(&h.fee_recipient, &5_u64, &None);
    assert_eq!(claimed, 1_500);
}
