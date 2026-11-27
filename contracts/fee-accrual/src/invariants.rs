//! Strict invariant assertions and verification helpers for the fee-accrual subsystem.
//!
//! # Core Protocol Invariants (Issue #388)
//!
//! * **Invariant 1 (Non-Negative Accounting & Rate Bounds)**:
//!   - For all stream fee records, all balances must be non-negative:
//!     `total_processed >= 0`, `protocol_fee_accrued >= 0`, `keeper_fee_accrued >= 0`,
//!     `protocol_fee_claimed >= 0`, `keeper_fee_claimed >= 0`.
//!   - Fee basis points configuration rates must not exceed `10_000` (100%):
//!     `protocol_fee_bps <= 10_000` and `keeper_split_bps <= 10_000`.
//!
//! * **Invariant 2 (Fee Breakdown Conservation)**:
//!   - For every gross accrual computation:
//!     `protocol_fee + keeper_fee == total_fee`
//!     `net_amount + total_fee == gross_amount`
//!     `total_fee <= gross_amount`
//!     `keeper_fee <= total_fee`
//!     `protocol_fee <= total_fee`
//!
//! * **Invariant 3 (Cumulative Claim Monotonicity & Solvency)**:
//!   - Accrued balances can never be exceeded by claims:
//!     `protocol_fee_claimed <= protocol_fee_accrued`
//!     `keeper_fee_claimed <= keeper_fee_accrued`
//!   - Claims and accruals must be monotonically non-decreasing over time:
//!     `protocol_fee_accrued_new >= protocol_fee_accrued_old`
//!     `keeper_fee_accrued_new >= keeper_fee_accrued_old`
//!     `protocol_fee_claimed_new >= protocol_fee_claimed_old`
//!     `keeper_fee_claimed_new >= keeper_fee_claimed_old`
//!
//! * **Invariant 4 (Global Totals Consistency)**:
//!   - Cumulative contract-wide totals must equal or exceed individual record aggregations:
//!     `total_protocol_fees >= protocol_fee_accrued`
//!     `total_keeper_fees >= keeper_fee_accrued`
//!   - Invariant holds across concurrent batch updates.
//!
//! * **Invariant 5 (Re-entrancy Protection & Lifecycle Integrity)**:
//!   - Re-entrancy guard flag `DataKey::Executing` must prevent recursive entry into
//!     state-mutating paths (`accrue_fee`, `batch_accrue_fee`, `claim_protocol_fees`, `claim_keeper_fees`).
//!   - Guard must be cleanly restored to cleared state upon return or transaction abort.
//!   - Settled streams (`is_settled == true`) reject additional fee accruals.
//!
//! * **Invariant 6 (Batch Atomic Bounds & Footprint Protection)**:
//!   - Batch sizes must strictly satisfy `1 <= batch.len() <= MAX_BATCH_SIZE` (32 items)
//!     to preserve bounded ledger footprint and prevent Soroban host metering resource exhaustion.

use crate::error::FeeError;
use crate::types::{AccruedFeeBreakdown, FeeConfig, StreamFeeRecord};

pub const MAX_BATCH_SIZE: u32 = 32;

/// Assert Invariant 1: Valid configuration rates.
#[inline(always)]
pub fn assert_valid_config(config: &FeeConfig) -> Result<(), FeeError> {
    if config.protocol_fee_bps > 10_000 || config.keeper_split_bps > 10_000 {
        return Err(FeeError::InvalidBpsRate);
    }
    Ok(())
}

/// Assert Invariant 2: Conservation of fee breakdown.
#[inline(always)]
pub fn assert_breakdown_conservation(b: &AccruedFeeBreakdown) -> Result<(), FeeError> {
    if b.gross_amount < 0
        || b.total_fee < 0
        || b.protocol_fee < 0
        || b.keeper_fee < 0
        || b.net_amount < 0
    {
        return Err(FeeError::InvariantViolation);
    }

    let fee_sum = b
        .protocol_fee
        .checked_add(b.keeper_fee)
        .ok_or(FeeError::ArithmeticOverflow)?;
    if fee_sum != b.total_fee {
        return Err(FeeError::InvariantViolation);
    }

    let net_plus_fee = b
        .net_amount
        .checked_add(b.total_fee)
        .ok_or(FeeError::ArithmeticOverflow)?;
    if net_plus_fee != b.gross_amount {
        return Err(FeeError::InvariantViolation);
    }

    if b.total_fee > b.gross_amount || b.protocol_fee > b.total_fee || b.keeper_fee > b.total_fee {
        return Err(FeeError::InvariantViolation);
    }

    Ok(())
}

/// Assert Invariant 3: Record solvency and non-negative balances.
#[inline(always)]
pub fn assert_record_solvency(record: &StreamFeeRecord) -> Result<(), FeeError> {
    if record.total_processed < 0
        || record.protocol_fee_accrued < 0
        || record.keeper_fee_accrued < 0
        || record.protocol_fee_claimed < 0
        || record.keeper_fee_claimed < 0
    {
        return Err(FeeError::InvariantViolation);
    }

    if record.protocol_fee_claimed > record.protocol_fee_accrued
        || record.keeper_fee_claimed > record.keeper_fee_accrued
    {
        return Err(FeeError::InvariantViolation);
    }

    Ok(())
}

/// Assert Invariant 3 (monotonicity check across state transition).
#[inline(always)]
pub fn assert_accrual_monotonicity(
    old_rec: &StreamFeeRecord,
    new_rec: &StreamFeeRecord,
) -> Result<(), FeeError> {
    if new_rec.total_processed < old_rec.total_processed
        || new_rec.protocol_fee_accrued < old_rec.protocol_fee_accrued
        || new_rec.keeper_fee_accrued < old_rec.keeper_fee_accrued
        || new_rec.protocol_fee_claimed < old_rec.protocol_fee_claimed
        || new_rec.keeper_fee_claimed < old_rec.keeper_fee_claimed
    {
        return Err(FeeError::InvariantViolation);
    }
    Ok(())
}

/// Assert Invariant 6: Batch bounds checking.
#[inline(always)]
pub fn assert_batch_bounds(len: u32) -> Result<(), FeeError> {
    if len == 0 {
        return Err(FeeError::EmptyBatch);
    }
    if len > MAX_BATCH_SIZE {
        return Err(FeeError::BatchTooLarge);
    }
    Ok(())
}
