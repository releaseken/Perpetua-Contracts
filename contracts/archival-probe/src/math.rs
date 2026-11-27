//! Precision-safe fractional stream withdrawal mathematics for archival-probe.
//!
//! # Precision Guarantee
//!
//! Evaluates proportional fractional splits of withdrawable amounts:
//! `withdrawable * numerator / denominator`
//!
//! Using decomposition:
//! `floor(total * n / d) = (total / d) * n + floor((total % d) * n / d)`
//!
//! This eliminates intermediate integer overflow for large balances (up to `i128::MAX`)
//! while preserving the exact conservation property:
//! `payout + remainder == total`

use crate::Error;

/// A precision-safe withdrawal split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FractionalSplit {
    /// The computed payout amount (floored towards zero to ensure solvency).
    pub payout: i128,
    /// The remaining withdrawable balance (`total - payout`).
    pub remainder: i128,
    /// The original total withdrawable amount.
    pub total: i128,
}

impl FractionalSplit {
    /// Asserts conservation: `payout + remainder == total`.
    #[inline(always)]
    pub fn assert_conservation(&self) -> bool {
        self.payout
            .checked_add(self.remainder)
            .map(|sum| sum == self.total)
            .unwrap_or(false)
    }
}

/// Calculate the payout amount and remainder for a fractional withdrawal.
///
/// Guaranteed properties:
/// 1. `payout + remainder == total` (exact conservation, zero dust lost).
/// 2. `0 <= payout <= total`.
/// 3. `0 <= remainder <= total`.
/// 4. No intermediate overflow for `total <= i128::MAX`.
pub fn calculate_fractional_withdrawal(
    total: i128,
    numerator: i128,
    denominator: i128,
) -> Result<FractionalSplit, Error> {
    if total < 0 {
        return Err(Error::ArithmeticOverflow);
    }
    if denominator <= 0 {
        return Err(Error::ZeroDenominator);
    }
    if numerator < 0 || numerator > denominator {
        return Err(Error::InvalidFraction);
    }

    if total == 0 || numerator == 0 {
        return Ok(FractionalSplit {
            payout: 0,
            remainder: total,
            total,
        });
    }

    if numerator == denominator {
        return Ok(FractionalSplit {
            payout: total,
            remainder: 0,
            total,
        });
    }

    // Decomposition to prevent (total * numerator) intermediate overflow:
    // floor(total * n / d) = (total / d) * n + floor(((total % d) * n) / d)
    let q = total / denominator;
    let r = total % denominator;

    let high = q.checked_mul(numerator).ok_or(Error::ArithmeticOverflow)?;
    let low = r.checked_mul(numerator).ok_or(Error::ArithmeticOverflow)?;
    let low_div = low / denominator;

    let payout = high.checked_add(low_div).ok_or(Error::ArithmeticOverflow)?;
    let remainder = total.checked_sub(payout).ok_or(Error::ArithmeticOverflow)?;

    let split = FractionalSplit {
        payout,
        remainder,
        total,
    };

    if !split.assert_conservation() {
        return Err(Error::InvariantViolation);
    }

    Ok(split)
}

/// Calculate proportional split in basis points (1 bps = 0.01%, 10,000 bps = 100%).
pub fn calculate_bps_withdrawal(total: i128, bps: u32) -> Result<FractionalSplit, Error> {
    if bps > 10_000 {
        return Err(Error::InvalidFraction);
    }
    calculate_fractional_withdrawal(total, bps as i128, 10_000)
}

/// Divide `total` into `n` equal shares, distributing any truncation residue
/// deterministically across the first `r = total % n` shares so that the sum
/// across all shares equals `total` exactly without precision loss.
pub fn batch_split_equal_shares(total: i128, n: u32) -> Result<([i128; 16], u32), Error> {
    const MAX_N: u32 = 16;
    if total < 0 {
        return Err(Error::ArithmeticOverflow);
    }
    if n == 0 {
        return Err(Error::ZeroDenominator);
    }
    if n > MAX_N {
        return Err(Error::BatchTooLarge);
    }

    let n_i128 = n as i128;
    let base = total / n_i128;
    let remainder = (total % n_i128) as usize;

    let mut shares = [0i128; 16];
    for (i, item) in shares.iter_mut().enumerate().take(n as usize) {
        let extra = if i < remainder { 1 } else { 0 };
        *item = base.checked_add(extra).ok_or(Error::ArithmeticOverflow)?;
    }

    // Verify conservation identity
    let mut sum: i128 = 0;
    for &share in &shares[..n as usize] {
        sum = sum.checked_add(share).ok_or(Error::ArithmeticOverflow)?;
    }
    if sum != total {
        return Err(Error::InvariantViolation);
    }

    Ok((shares, n))
}
