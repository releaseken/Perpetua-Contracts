//! Precision-safe fractional stream withdrawal mathematics for factory and proxies.
//!
//! # Precision Guarantee
//!
//! Evaluates proportional fractional splits of withdrawable amounts:
//! `withdrawable * numerator / denominator`
//!
//! Using decomposition:
//! `floor(total * n / d) = (total / d) * n + floor((total % d) * n / d)`
//!
//! This eliminates intermediate integer overflow for large balances
//! while preserving the exact conservation property:
//! `payout + remainder == total`

use crate::FactoryError;

/// A precision-safe withdrawal split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FractionalSplit {
    /// The computed payout amount (floored towards zero to ensure contract solvency).
    pub payout: i128,
    /// The remaining withdrawable balance (`total - payout`).
    pub remainder: i128,
}

/// Calculate the payout amount and remainder for a fractional withdrawal.
///
/// Guaranteed properties:
/// 1. `payout + remainder == total` (exact conservation, no loss of dust/residue).
/// 2. `0 <= payout <= total`.
/// 3. `0 <= remainder <= total`.
/// 4. No intermediate overflow for `total <= i128::MAX`.
pub fn calculate_fractional_payout(
    total: i128,
    numerator: i128,
    denominator: i128,
) -> Result<FractionalSplit, FactoryError> {
    if total < 0 {
        return Err(FactoryError::InvalidDeposit);
    }
    if denominator <= 0 {
        return Err(FactoryError::InvalidDuration);
    }
    if numerator < 0 || numerator > denominator {
        return Err(FactoryError::InvalidDuration);
    }

    if total == 0 || numerator == 0 {
        return Ok(FractionalSplit {
            payout: 0,
            remainder: total,
        });
    }

    if numerator == denominator {
        return Ok(FractionalSplit {
            payout: total,
            remainder: 0,
        });
    }

    // Decomposition to prevent (total * numerator) overflow:
    // total * n / d = (total / d) * n + ((total % d) * n) / d
    let q = total / denominator;
    let r = total % denominator;

    let high = q
        .checked_mul(numerator)
        .ok_or(FactoryError::CapacityCapExceeded)?;
    let low = r
        .checked_mul(numerator)
        .ok_or(FactoryError::CapacityCapExceeded)?;
    let low_div = low / denominator;

    let payout = high
        .checked_add(low_div)
        .ok_or(FactoryError::CapacityCapExceeded)?;
    let remainder = total
        .checked_sub(payout)
        .ok_or(FactoryError::CapacityCapExceeded)?;

    Ok(FractionalSplit { payout, remainder })
}

/// Calculate proportional split in basis points (1 bps = 0.01%, 10,000 bps = 100%).
pub fn calculate_bps_payout(total: i128, bps: u32) -> Result<FractionalSplit, FactoryError> {
    if bps > 10_000 {
        return Err(FactoryError::InvalidDuration);
    }
    calculate_fractional_payout(total, bps as i128, 10_000)
}
