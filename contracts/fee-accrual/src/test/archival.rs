//! Tests for issue #390: automated entry lifetime extension for `fee-accrual`
//! to guarantee archival resilience.
//!
//! Verifies that:
//! 1. Every mutating path that touches a persistent `StreamFee(stream_id)` entry
//!    (`accrue_fee`, `batch_accrue_fees`, `claim_protocol_fee`, `claim_keeper_fee`,
//!    `settle_stream_fee`) automatically extends its TTL to at least
//!    `MIN_ENTRY_TTL_LEDGERS` (clamped to `max_ttl()`).
//! 2. The contract instance entry (`Config`, `TotalProtocolFees`, `TotalKeeperFees`,
//!    `PendingAdmin`) is pinned to `max_ttl()` after initialization and after
//!    every mutating operation.
//! 3. Permissionless keeper endpoints (`extend_stream_fee_ttl`,
//!    `extend_stream_fee_to_max`, `batch_extend_ttl`, `extend_instance_ttl`)
//!    refresh TTLs reliably, emit `TtlExtendedEvent`, skip unknown IDs in batch
//!    sweeps, and enforce defensive validation bounds.

use soroban_sdk::Vec;

use crate::{BatchAccrueItem, FeeError, MAX_BATCH_SIZE};

use super::common::Harness;

// ---------------------------------------------------------------------------
// §1  Automated persistent TTL extension on mutating fee paths
// ---------------------------------------------------------------------------

#[test]
fn accrue_fee_extends_persistent_and_instance_ttl() {
    let h = Harness::new();
    let expected_floor = h.expected_entry_ttl();
    let max_ttl = h.max_ttl();

    let breakdown = h
        .client
        .accrue_fee(&h.fee_collector, &42_u64, &100_000_i128);
    assert_eq!(breakdown.total_fee, 2_000);
    assert_eq!(breakdown.keeper_fee, 500);
    assert_eq!(breakdown.protocol_fee, 1_500);
    assert_eq!(breakdown.net_amount, 98_000);

    assert!(h.entry_exists(42), "StreamFee(42) must exist after accrual");
    let ttl = h.ttl_of(42);
    assert!(
        ttl >= expected_floor,
        "persistent TTL {ttl} must be >= expected floor {expected_floor}"
    );
    assert_eq!(
        h.instance_ttl(),
        max_ttl,
        "instance TTL must be pinned to max_ttl after accrue_fee"
    );
}

#[test]
fn subsequent_accrual_after_ledger_advance_refreshes_ttl() {
    let h = Harness::new();
    let expected_floor = h.expected_entry_ttl();

    h.client.accrue_fee(&h.fee_collector, &10_u64, &50_000_i128);

    // Advance 60 days so remaining TTL decays below the floor.
    h.advance(60 * 86_400);

    // Second accrual touches the entry and must automatically re-extend TTL back to floor.
    h.client.accrue_fee(&h.fee_collector, &10_u64, &25_000_i128);
    let ttl_after = h.ttl_of(10);
    assert!(
        ttl_after >= expected_floor,
        "TTL {ttl_after} must be refreshed to >= {expected_floor} on subsequent accrual"
    );
    assert_eq!(h.instance_ttl(), h.max_ttl());
}

#[test]
fn batch_accrue_fees_extends_ttl_for_every_stream_in_batch() {
    let h = Harness::new();
    let expected_floor = h.expected_entry_ttl();

    let mut items: Vec<BatchAccrueItem> = Vec::new(&h.env);
    for id in 1_u64..=8 {
        items.push_back(BatchAccrueItem {
            stream_id: id,
            amount: (id as i128) * 10_000,
        });
    }

    let breakdowns = h.client.batch_accrue_fees(&h.fee_collector, &items);
    assert_eq!(breakdowns.len(), 8);

    for id in 1_u64..=8 {
        assert!(h.entry_exists(id), "stream {id} must exist after batch");
        let ttl = h.ttl_of(id);
        assert!(
            ttl >= expected_floor,
            "stream {id}: TTL {ttl} must be >= {expected_floor}"
        );
    }
    assert_eq!(h.instance_ttl(), h.max_ttl());
}

#[test]
fn claim_protocol_fee_and_claim_keeper_fee_refresh_entry_ttl() {
    let h = Harness::new();
    let expected_floor = h.expected_entry_ttl();

    h.client.accrue_fee(&h.fee_collector, &77_u64, &100_000_i128);

    // Advance 45 days and claim partial protocol fee.
    h.advance(45 * 86_400);
    let claimed_proto = h
        .client
        .claim_protocol_fee(&h.fee_recipient, &77_u64, &Some(500));
    assert_eq!(claimed_proto, 500);
    assert!(
        h.ttl_of(77) >= expected_floor,
        "claim_protocol_fee must refresh persistent entry TTL"
    );
    assert_eq!(h.instance_ttl(), h.max_ttl());

    // Advance another 45 days and claim full keeper fee.
    h.advance(45 * 86_400);
    let claimed_keeper =
        h.client
            .claim_keeper_fee(&h.fee_collector, &77_u64, &h.other, &None);
    assert_eq!(claimed_keeper, 500);
    assert!(
        h.ttl_of(77) >= expected_floor,
        "claim_keeper_fee must refresh persistent entry TTL"
    );
    assert_eq!(h.instance_ttl(), h.max_ttl());
}

#[test]
fn settle_stream_fee_extends_entry_ttl_for_post_settlement_queries() {
    let h = Harness::new();
    let expected_floor = h.expected_entry_ttl();

    h.client.accrue_fee(&h.fee_collector, &88_u64, &200_000_i128);
    h.advance(50 * 86_400);

    h.client.settle_stream_fee(&h.fee_collector, &88_u64);
    let ttl = h.ttl_of(88);
    assert!(
        ttl >= expected_floor,
        "settle_stream_fee must extend persistent TTL to >= {expected_floor}, got {ttl}"
    );
    assert_eq!(h.instance_ttl(), h.max_ttl());

    let record = h.client.get_stream_record(&88_u64);
    assert!(record.is_settled);
}

// ---------------------------------------------------------------------------
// §2  Permissionless keeper TTL extension endpoints
// ---------------------------------------------------------------------------

#[test]
fn extend_stream_fee_ttl_permissionless_refreshes_to_floor() {
    let h = Harness::new();
    let expected_floor = h.expected_entry_ttl();

    h.client.accrue_fee(&h.fee_collector, &7_u64, &50_000_i128);
    h.advance(90 * 86_400);

    // Permissionless call: no caller auth required.
    let target = h.client.extend_stream_fee_ttl(&7_u64);
    assert_eq!(target, expected_floor);
    assert!(
        h.ttl_of(7) >= expected_floor,
        "keeper extension must restore TTL to >= {expected_floor}"
    );
    assert_eq!(h.instance_ttl(), h.max_ttl());
}

#[test]
fn extend_stream_fee_to_max_pins_entry_to_network_max_ttl() {
    let h = Harness::new();
    let max_ttl = h.max_ttl();

    h.client.accrue_fee(&h.fee_collector, &15_u64, &10_000_i128);
    h.advance(30 * 86_400);

    let extended_to = h.client.extend_stream_fee_to_max(&15_u64);
    assert_eq!(extended_to, max_ttl);
    assert_eq!(h.ttl_of(15), max_ttl);
    assert_eq!(h.instance_ttl(), max_ttl);
}

#[test]
fn extend_stream_fee_ttl_rejects_unknown_and_zero_ids() {
    let h = Harness::new();

    let err_zero = h
        .client
        .try_extend_stream_fee_ttl(&0_u64)
        .unwrap_err()
        .unwrap();
    assert_eq!(err_zero, FeeError::InvalidStreamId);

    let err_missing = h
        .client
        .try_extend_stream_fee_ttl(&999_u64)
        .unwrap_err()
        .unwrap();
    assert_eq!(err_missing, FeeError::FeeRecordNotFound);

    let err_max_missing = h
        .client
        .try_extend_stream_fee_to_max(&999_u64)
        .unwrap_err()
        .unwrap();
    assert_eq!(err_max_missing, FeeError::FeeRecordNotFound);
}

#[test]
fn batch_extend_ttl_extends_existing_and_skips_unknown_ids() {
    let h = Harness::new();
    let expected_floor = h.expected_entry_ttl();

    h.client.accrue_fee(&h.fee_collector, &1_u64, &10_000_i128);
    h.client.accrue_fee(&h.fee_collector, &2_u64, &20_000_i128);
    h.client.accrue_fee(&h.fee_collector, &3_u64, &30_000_i128);

    h.advance(100 * 86_400);

    // Stream IDs 99 and 100 do not exist — they must be skipped without aborting.
    let ids = Vec::from_slice(&h.env, &[1_u64, 99_u64, 2_u64, 100_u64, 3_u64]);
    let extended_count = h.client.batch_extend_ttl(&ids);
    assert_eq!(extended_count, 3);

    for id in [1_u64, 2, 3] {
        let ttl = h.ttl_of(id);
        assert!(
            ttl >= expected_floor,
            "stream {id}: TTL {ttl} < expected floor {expected_floor} after batch_extend_ttl"
        );
    }
    assert_eq!(h.instance_ttl(), h.max_ttl());
}

#[test]
fn batch_extend_ttl_enforces_batch_bounds_and_valid_ids() {
    let h = Harness::new();

    // Empty batch rejected.
    let empty: Vec<u64> = Vec::new(&h.env);
    assert_eq!(
        h.client.try_batch_extend_ttl(&empty).unwrap_err().unwrap(),
        FeeError::EmptyBatch
    );

    // Batch with stream_id == 0 rejected before mutating.
    let with_zero = Vec::from_slice(&h.env, &[1_u64, 0_u64]);
    assert_eq!(
        h.client
            .try_batch_extend_ttl(&with_zero)
            .unwrap_err()
            .unwrap(),
        FeeError::InvalidStreamId
    );

    // Batch exceeding MAX_BATCH_SIZE (32) rejected.
    let mut oversized: Vec<u64> = Vec::new(&h.env);
    for i in 1..=(MAX_BATCH_SIZE + 1) {
        oversized.push_back(i as u64);
    }
    assert_eq!(
        h.client
            .try_batch_extend_ttl(&oversized)
            .unwrap_err()
            .unwrap(),
        FeeError::BatchTooLarge
    );
}

#[test]
fn instance_ttl_stays_pinned_across_all_admin_and_config_mutations() {
    let h = Harness::new();
    let max_ttl = h.max_ttl();
    assert_eq!(h.instance_ttl(), max_ttl);

    h.advance_ledgers(50_000);
    assert!(h.instance_ttl() < max_ttl);

    // Configuration update pins instance TTL back to max.
    h.client
        .update_fee_config(&h.fee_admin, &h.fee_recipient, &300, &2_000);
    assert_eq!(h.instance_ttl(), max_ttl);

    // Pause / unpause pins instance TTL back to max.
    h.advance_ledgers(25_000);
    h.client.pause(&h.emergency_admin);
    assert_eq!(h.instance_ttl(), max_ttl);

    h.advance_ledgers(25_000);
    h.client.unpause(&h.emergency_admin);
    assert_eq!(h.instance_ttl(), max_ttl);

    // Permissionless instance heartbeat pins instance TTL back to max.
    h.advance_ledgers(40_000);
    let pinned = h.client.extend_instance_ttl();
    assert_eq!(pinned, max_ttl);
    assert_eq!(h.instance_ttl(), max_ttl);
}
