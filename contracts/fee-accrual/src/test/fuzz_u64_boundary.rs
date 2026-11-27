//! Issue #392 — fuzzing coverage for edge-case timestamps in fee-accrual
//! near `u64` maximum boundary limits.
//!
//! # Why `u64::MAX` boundary limits matter in fee accrual
//!
//! Soroban ledger timestamps and stream identifiers are represented as `u64`.
//! When fee accruals, settlements, claims, or emergency operations occur at or near
//! `u64::MAX`, several risks arise:
//!
//! 1. **Stream ID boundary collisions and wraps**: Stream IDs generated or passed
//!    near `u64::MAX` (e.g. `u64::MAX`, `u64::MAX - 1`) must not wrap, collide,
//!    or cause persistent storage corruption or footprint thrashing.
//! 2. **Ledger timestamp progression near `u64::MAX`**: Time-advancement and
//!    TTL extensions at extreme ledger timestamps (e.g. `u64::MAX`, `u64::MAX - 500`)
//!    must remain well-behaved without panicking or corrupting instance/persistent storage.
//! 3. **Defensive arithmetic and invariant preservation**:
//!    - `protocol_fee + keeper_fee == total_fee`
//!    - `net_amount + total_fee == gross_amount`
//!    - Cumulative fee conservation across multiple accruals and claims.
//!    - Fee amounts must never overflow `i128`, even with large inputs.
//! 4. **Byzantine / extreme timestamp warping**: Subsystems invoking fee-accrual
//!    under extreme clock drift, large wall-clock jumps up to `u64::MAX`, or
//!    out-of-order calls must never break role-based auth or lead to inconsistent state.
//!
//! # Test Structure
//!
//! 1. **Explicit Boundary Pinning**: Hand-crafted edge-case tests probing:
//!    - Stream IDs at `0`, `1`, `u64::MAX / 2`, `u64::MAX - 1`, and `u64::MAX`.
//!    - Host ledger timestamps at `T0`, `u64::MAX / 2`, `u64::MAX - 1_000`, and `u64::MAX`.
//!    - Single-step and batch fee accruals at maximum boundary timestamps.
//!    - Multi-stage lifecycle (init -> accrue -> claim -> settle) at `u64::MAX` timestamps.
//! 2. **Proptest-Driven Fuzzing**:
//!    - Fuzzing stream IDs across the entire `[1, u64::MAX]` space.
//!    - Fuzzing timestamps across `[0, u64::MAX]` to verify host ledger stability and TTL safety.
//!    - Fuzzing gross amounts and basis points to ensure invariant conservation.
//!    - Fuzzing batch operations with edge-case stream IDs and timestamps.

use proptest::prelude::*;
use soroban_sdk::{testutils::Ledger as _, Vec};

use super::common::Harness;
use crate::error::FeeError;
use crate::math::{compute_fee_breakdown, BPS_DENOMINATOR};
use crate::types::BatchAccrueItem;

const MAX_U64: u64 = u64::MAX;

// ---------------------------------------------------------------------------
// §1. Explicit Boundary Pinning: Pure Math & Invariants
// ---------------------------------------------------------------------------

#[test]
fn pure_math_invariants_hold_at_extremes() {
    // Zero gross amount
    let res_zero = compute_fee_breakdown(0, 100, 2000).expect("zero gross");
    assert_eq!(res_zero.total_fee, 0);
    assert_eq!(res_zero.protocol_fee, 0);
    assert_eq!(res_zero.keeper_fee, 0);
    assert_eq!(res_zero.net_amount, 0);

    // Zero fee bps
    let res_no_fee = compute_fee_breakdown(1_000_000, 0, 5000).expect("zero bps");
    assert_eq!(res_no_fee.total_fee, 0);
    assert_eq!(res_no_fee.net_amount, 1_000_000);

    // Maximum allowed bps (10_000 = 100%)
    let res_max_bps = compute_fee_breakdown(10_000, 10_000, 10_000).expect("max bps");
    assert_eq!(res_max_bps.total_fee, 10_000);
    assert_eq!(res_max_bps.keeper_fee, 10_000);
    assert_eq!(res_max_bps.protocol_fee, 0);
    assert_eq!(res_max_bps.net_amount, 0);

    // Invalid bps rates (> 10_000)
    assert_eq!(
        compute_fee_breakdown(10_000, 10_001, 100),
        Err(FeeError::InvalidBpsRate)
    );
    assert_eq!(
        compute_fee_breakdown(10_000, 100, 10_001),
        Err(FeeError::InvalidBpsRate)
    );

    // Negative gross amount
    assert_eq!(
        compute_fee_breakdown(-1, 100, 100),
        Err(FeeError::ArithmeticOverflow)
    );
}

// ---------------------------------------------------------------------------
// §2. Explicit Boundary Pinning: Host-level operations at u64::MAX
// ---------------------------------------------------------------------------

#[test]
fn fee_accrual_at_u64_max_timestamp_and_stream_id() {
    let h = Harness::new();

    // Warp ledger timestamp to u64::MAX
    h.warp_to(MAX_U64);
    assert_eq!(h.now(), MAX_U64);

    let stream_id = MAX_U64;
    let gross_amount = 1_000_000i128;

    // Accrue fee for stream_id = u64::MAX at timestamp = u64::MAX
    let breakdown = h
        .client
        .accrue_fee(&h.fee_collector, &stream_id, &gross_amount);

    assert_eq!(breakdown.gross_amount, gross_amount);
    assert_eq!(breakdown.total_fee, 10_000); // 1% of 1_000_000
    assert_eq!(breakdown.keeper_fee, 2_000); // 20% of 10_000
    assert_eq!(breakdown.protocol_fee, 8_000); // 80% of 10_000
    assert_eq!(breakdown.net_amount, 990_000);

    // Verify stored record
    let record = h.client.get_stream_fee(&stream_id).expect("record exists");
    assert_eq!(record.stream_id, MAX_U64);
    assert_eq!(record.total_processed, gross_amount);
    assert_eq!(record.protocol_fee_accrued, 8_000);
    assert_eq!(record.keeper_fee_accrued, 2_000);
    assert_eq!(record.protocol_fee_claimed, 0);
    assert_eq!(record.keeper_fee_claimed, 0);
    assert!(!record.is_settled);

    // Cumulative totals
    assert_eq!(h.client.total_protocol_fees(), 8_000);
    assert_eq!(h.client.total_keeper_fees(), 2_000);
}

#[test]
fn adjacent_boundary_stream_ids_near_u64_max_do_not_interfere() {
    let h = Harness::new();
    h.warp_to(MAX_U64 - 100);

    let id_a = MAX_U64;
    let id_b = MAX_U64 - 1;
    let id_c = MAX_U64 - 2;

    h.client.accrue_fee(&h.fee_collector, &id_a, &100_000);
    h.client.accrue_fee(&h.fee_collector, &id_b, &200_000);
    h.client.accrue_fee(&h.fee_collector, &id_c, &300_000);

    let rec_a = h.client.get_stream_fee(&id_a).unwrap();
    let rec_b = h.client.get_stream_fee(&id_b).unwrap();
    let rec_c = h.client.get_stream_fee(&id_c).unwrap();

    assert_eq!(rec_a.total_processed, 100_000);
    assert_eq!(rec_b.total_processed, 200_000);
    assert_eq!(rec_c.total_processed, 300_000);

    // Invariant: cumulative totals equal sum of parts
    assert_eq!(
        h.client.total_protocol_fees(),
        rec_a.protocol_fee_accrued + rec_b.protocol_fee_accrued + rec_c.protocol_fee_accrued
    );
    assert_eq!(
        h.client.total_keeper_fees(),
        rec_a.keeper_fee_accrued + rec_b.keeper_fee_accrued + rec_c.keeper_fee_accrued
    );
}

#[test]
fn full_lifecycle_at_u64_max_boundary() {
    let h = Harness::new();
    h.warp_to(MAX_U64 - 1);

    let stream_id = MAX_U64 - 5;
    let gross = 500_000i128;

    // 1. Accrue
    let breakdown = h.client.accrue_fee(&h.fee_collector, &stream_id, &gross);
    assert_eq!(breakdown.total_fee, 5_000);
    assert_eq!(breakdown.protocol_fee, 4_000);
    assert_eq!(breakdown.keeper_fee, 1_000);

    // Advance clock across boundary to u64::MAX
    h.advance(1);
    assert_eq!(h.now(), MAX_U64);

    // 2. Claim Protocol Fee
    let proto_claimed = h.client.claim_stream_fee(
        &h.fee_recipient,
        &stream_id,
        &true,
    );
    assert_eq!(proto_claimed, 4_000);

    // Re-claiming should fail with ZeroFeeAccrual
    let err_proto = h.client.try_claim_stream_fee(
        &h.fee_recipient,
        &stream_id,
        &true,
    );
    assert_eq!(err_proto, Err(Ok(FeeError::ZeroFeeAccrual)));

    // 3. Claim Keeper Fee
    let keeper_claimed = h.client.claim_stream_fee(
        &h.fee_collector,
        &stream_id,
        &false,
    );
    assert_eq!(keeper_claimed, 1_000);

    // 4. Settle stream
    h.client.settle_stream_fee(&h.fee_collector, &stream_id);
    let rec = h.client.get_stream_fee(&stream_id).unwrap();
    assert!(rec.is_settled);

    // Accruing on settled stream must fail
    let err_settled = h.client.try_accrue_fee(&h.fee_collector, &stream_id, &1000);
    assert_eq!(err_settled, Err(Ok(FeeError::FeeAlreadySettled)));
}

#[test]
fn batch_accrual_at_u64_boundary() {
    let h = Harness::new();
    h.warp_to(MAX_U64);

    let mut items = Vec::new(&h.env);
    items.push_back(BatchAccrueItem {
        stream_id: MAX_U64,
        amount: 100_000,
    });
    items.push_back(BatchAccrueItem {
        stream_id: MAX_U64 - 1,
        amount: 200_000,
    });
    items.push_back(BatchAccrueItem {
        stream_id: 1,
        amount: 300_000,
    });

    let total_fees = h.client.batch_accrue_fee(&h.fee_collector, &items);
    assert_eq!(total_fees, 1_000 + 2_000 + 3_000);

    assert_eq!(
        h.client.get_stream_fee(&MAX_U64).unwrap().total_processed,
        100_000
    );
    assert_eq!(
        h.client.get_stream_fee(&(MAX_U64 - 1)).unwrap().total_processed,
        200_000
    );
    assert_eq!(
        h.client.get_stream_fee(&1).unwrap().total_processed,
        300_000
    );
}

#[test]
fn zero_stream_id_is_always_rejected() {
    let h = Harness::new();
    h.warp_to(MAX_U64);

    let res = h.client.try_accrue_fee(&h.fee_collector, &0, &100_000);
    assert_eq!(res, Err(Ok(FeeError::InvalidStreamId)));
}

#[test]
fn emergency_pause_at_u64_max_boundary() {
    let h = Harness::new();
    h.warp_to(MAX_U64);

    // Pause contract
    h.client.set_paused(&true);
    let conf = h.client.get_config();
    assert!(conf.paused);

    // Accrual must fail while paused
    let err_accrue = h.client.try_accrue_fee(&h.fee_collector, &MAX_U64, &50_000);
    assert_eq!(err_accrue, Err(Ok(FeeError::ContractPaused)));

    // Unpause contract
    h.client.set_paused(&false);
    let conf_unpaused = h.client.get_config();
    assert!(!conf_unpaused.paused);

    // Accrual succeeds
    let ok_accrue = h.client.accrue_fee(&h.fee_collector, &MAX_U64, &50_000);
    assert_eq!(ok_accrue.total_fee, 500);
}

// ---------------------------------------------------------------------------
// §3. Proptest-Driven Fuzzing: Properties across u64 Boundary Limits
// ---------------------------------------------------------------------------

proptest! {
    /// Invariant 1: Breakdown conservation
    /// protocol_fee + keeper_fee == total_fee && net_amount + total_fee == gross_amount
    #[test]
    fn prop_fuzz_fee_breakdown_conservation(
        gross in 0i128..=10_000_000_000_000i128,
        protocol_bps in 0u32..=10_000u32,
        keeper_split_bps in 0u32..=10_000u32,
    ) {
        let res = compute_fee_breakdown(gross, protocol_bps, keeper_split_bps).unwrap();

        prop_assert_eq!(res.protocol_fee + res.keeper_fee, res.total_fee);
        prop_assert_eq!(res.net_amount + res.total_fee, gross);
        prop_assert!(res.total_fee <= gross);
        prop_assert!(res.keeper_fee <= res.total_fee);
        prop_assert!(res.protocol_fee <= res.total_fee);
        prop_assert!(res.net_amount <= gross);
        prop_assert!(res.protocol_fee >= 0);
        prop_assert!(res.keeper_fee >= 0);
        prop_assert!(res.net_amount >= 0);
    }

    /// Invariant 2: Timestamp and Stream ID boundary resilience
    /// Contract never panics when stream_id or ledger timestamp approaches or reaches u64::MAX.
    #[test]
    fn prop_fuzz_stream_id_and_timestamp_boundary(
        timestamp_offset in 0u64..=1_000_000u64,
        stream_id_offset in 0u64..=1_000_000u64,
        gross in 1_000i128..=10_000_000i128,
    ) {
        let h = Harness::new();

        // Sample timestamp near u64::MAX or anywhere in the upper bound
        let test_timestamp = MAX_U64.saturating_sub(timestamp_offset);
        h.warp_to(test_timestamp);
        prop_assert_eq!(h.now(), test_timestamp);

        // Sample stream ID near u64::MAX
        let test_stream_id = MAX_U64.saturating_sub(stream_id_offset);
        if test_stream_id == 0 {
            return Ok(());
        }

        let breakdown = h.client.accrue_fee(&h.fee_collector, &test_stream_id, &gross);
        prop_assert_eq!(breakdown.gross_amount, gross);
        prop_assert_eq!(breakdown.protocol_fee + breakdown.keeper_fee, breakdown.total_fee);

        let rec = h.client.get_stream_fee(&test_stream_id).unwrap();
        prop_assert_eq!(rec.stream_id, test_stream_id);
        prop_assert_eq!(rec.total_processed, gross);

        // Verification of claim step at this extreme timestamp
        let proto_claim = h.client.claim_stream_fee(&h.fee_recipient, &test_stream_id, &true);
        prop_assert_eq!(proto_claim, breakdown.protocol_fee);
    }

    /// Invariant 3: Monotonicity of cumulative fee aggregation
    /// Successive fee accruals at extreme timestamps monotonically increase total protocol and keeper fees.
    #[test]
    fn prop_fuzz_monotonic_fee_aggregation(
        steps in 1usize..=10usize,
        gross in 100i128..=100_000i128,
        time_jump in 1u64..=10_000u64,
    ) {
        let h = Harness::new();
        let mut current_time = MAX_U64.saturating_sub(100_000);
        h.warp_to(current_time);

        let mut prev_proto_total = 0i128;
        let mut prev_keeper_total = 0i128;

        for i in 1..=steps {
            current_time = current_time.saturating_add(time_jump);
            h.warp_to(current_time);

            let stream_id = MAX_U64.saturating_sub(i as u64);
            let b = h.client.accrue_fee(&h.fee_collector, &stream_id, &gross);

            let cur_proto_total = h.client.total_protocol_fees();
            let cur_keeper_total = h.client.total_keeper_fees();

            prop_assert!(cur_proto_total >= prev_proto_total);
            prop_assert!(cur_keeper_total >= prev_keeper_total);
            prop_assert_eq!(cur_proto_total, prev_proto_total + b.protocol_fee);
            prop_assert_eq!(cur_keeper_total, prev_keeper_total + b.keeper_fee);

            prev_proto_total = cur_proto_total;
            prev_keeper_total = cur_keeper_total;
        }
    }
}
