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
}
