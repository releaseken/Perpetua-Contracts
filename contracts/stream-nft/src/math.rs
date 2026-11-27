//! Precision-safe fractional stream withdrawal mathematics and defensive
//! timestamp/duration arithmetic for stream-nft.
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
//! `amount + remainder == total`
//!
//! Defensive math and checked arithmetic for timestamps and durations in stream-nft.
//!
//! # Boundary safety
//!
//! Provides overflow-safe timestamp, duration, and claim calculations near
//! boundary limits (`u64::MAX`, `i128::MAX`).

use crate::Error;

/// Safely compute duration between `start_time` and `end_time`.
/// Returns `Err(Error::ArithmeticOverflow)` if `end < start`.
#[inline]
pub fn checked_duration(start: u64, end: u64) -> Result<u64, Error> {
    if end < start {
        return Err(Error::ArithmeticOverflow);
    }
    end.checked_sub(start).ok_or(Error::ArithmeticOverflow)
}

/// Safely compute effective stream time given a ledger timestamp, pause instant,
/// and cumulative paused total.
///
/// If `paused_at` is set, effective stream time freezes at `paused_at - paused_total`.
/// Otherwise, effective time is `now - paused_total`.
#[inline]
pub fn stream_time(now: u64, paused_at: Option<u64>, paused_total: u64) -> u64 {
    let reference = paused_at.unwrap_or(now);
    reference.saturating_sub(paused_total)
}

/// Safely compute elapsed vesting time from `start_time` up to `now`, clamping
/// at `end_time`.
#[inline]
pub fn elapsed(start: u64, end: u64, now: u64, paused_at: Option<u64>, paused_total: u64) -> u64 {
    let st = stream_time(now, paused_at, paused_total);
    if st <= start {
        0
    } else {
        st.min(end).saturating_sub(start)
    }
}

/// Check if a cliff gate has been reached.
#[inline]
pub fn cliff_reached(cliff_time: u64, now: u64, paused_at: Option<u64>, paused_total: u64) -> bool {
    let st = stream_time(now, paused_at, paused_total);
    st >= cliff_time
}

/// Compute vested amount under linear vesting with a cliff gate.
///
/// Uses checked arithmetic:
/// `vested = (deposited * elapsed) / duration`
///
/// Returns `Err(Error::ArithmeticOverflow)` on division by zero or integer overflow.
pub fn compute_vested(
    deposited: i128,
    start: u64,
    end: u64,
    cliff: u64,
    now: u64,
    paused_at: Option<u64>,
    paused_total: u64,
) -> Result<i128, Error> {
    if deposited <= 0 {
        return Ok(0);
    }
    let duration = checked_duration(start, end)?;
    if duration == 0 {
        return Err(Error::ArithmeticOverflow);
    }

    if !cliff_reached(cliff, now, paused_at, paused_total) {
        return Ok(0);
    }

    let el = elapsed(start, end, now, paused_at, paused_total);
    if el >= duration {
        return Ok(deposited);
    }

    let el_i128 = el as i128;
    let dur_i128 = duration as i128;

    let product = deposited
        .checked_mul(el_i128)
        .ok_or(Error::ArithmeticOverflow)?;
    let vested = product
        .checked_div(dur_i128)
        .ok_or(Error::ArithmeticOverflow)?;

    Ok(vested)
}

/// Compute refundable amount: `deposited - vested`.
pub fn compute_refundable(
    deposited: i128,
    start: u64,
    end: u64,
    cliff: u64,
    now: u64,
    paused_at: Option<u64>,
    paused_total: u64,
) -> Result<i128, Error> {
    let vested = compute_vested(deposited, start, end, cliff, now, paused_at, paused_total)?;
    deposited
        .checked_sub(vested)
        .ok_or(Error::ArithmeticOverflow)
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
) -> Result<FractionalSplit, Error> {
    if total < 0 {
        return Err(Error::InvalidAmount);
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

    let high = q.checked_mul(numerator).ok_or(Error::ArithmeticOverflow)?;
    let low = r.checked_mul(numerator).ok_or(Error::ArithmeticOverflow)?;
    let low_div = low / denominator;

    let payout = high.checked_add(low_div).ok_or(Error::ArithmeticOverflow)?;
    let remainder = total
        .checked_sub(payout)
        .ok_or(Error::ArithmeticOverflow)?;

    Ok(FractionalSplit { payout, remainder })
}

/// Calculate proportional split in basis points (1 bps = 0.01%, 10,000 bps = 100%).
pub fn calculate_bps_payout(total: i128, bps: u32) -> Result<FractionalSplit, Error> {
    if bps > 10_000 {
        return Err(Error::InvalidFraction);
    }
    calculate_fractional_payout(total, bps as i128, 10_000)
}
