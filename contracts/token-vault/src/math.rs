//! Pure accrual arithmetic for token-vault positions.
//!
//! All functions are free of side effects: no `Env`, no storage, no host
//! calls.  Every computation is derived fresh from the position at call time
//! so conservation is exact and truncation errors never accumulate.
//!
//! # Overflow safety
//!
//! All time fields (`start_time`, `end_time`, `cliff_time`, `paused_at`,
//! `paused_total`) are `u64`.  The arithmetic paths that are most dangerous
//! near `u64::MAX`:
//!
//! * `end_time - start_time` → guarded by `saturating_sub`.
//! * `paused_at.saturating_sub(paused_total)` → saturating.
//! * `clock.min(end_time).saturating_sub(start_time)` → saturating.
//! * `deposited * elapsed` → `checked_mul`, returns `Overflow`.
//!
//! # Rounding
//!
//! Integer division truncates toward zero (rounds down in the beneficiary's
//! disfavour).  The residue stays in the pool and returns to the sender on
//! revoke, so the contract can never owe more than it holds.
//!
//! # Conservation identity
//!
//! `vested(now) + refundable(now) == deposited` is an algebraic identity.

use crate::errors::VaultError;
use crate::types::VaultPosition;

/// Map wall-clock time to the position's internal clock.
///
/// The clock freezes while paused.  After a resume, `paused_total` has
/// absorbed the pause interval so the clock continues from where it stopped.
#[inline]
pub fn stream_time(pos: &VaultPosition, now: u64) -> u64 {
    let frozen_at = pos.paused_at.unwrap_or(now);
    frozen_at.saturating_sub(pos.paused_total)
}

/// Total position duration, in seconds.
#[inline]
pub fn duration(pos: &VaultPosition) -> u64 {
    pos.end_time.saturating_sub(pos.start_time)
}

/// Seconds consumed at `now`, clamped to `[0, duration]`.
#[inline]
pub fn elapsed(pos: &VaultPosition, now: u64) -> u64 {
    let clock = stream_time(pos, now);
    // clamp to end_time before subtracting start_time to avoid any wrap
    let capped = clock.min(pos.end_time);
    capped.saturating_sub(pos.start_time)
}

/// Whether the cliff gate has opened, evaluated against the position clock.
///
/// A position paused across its cliff does not silently open while frozen.
#[inline]
pub fn cliff_reached(pos: &VaultPosition, now: u64) -> bool {
    stream_time(pos, now) >= pos.cliff_time
}

/// Total tokens vested at `now`.
///
/// # Formula
///
/// ```text
/// vested = 0                                      if cliff not reached
/// vested = deposited                              if duration == 0
/// vested = deposited                              if elapsed >= duration
/// vested = deposited * elapsed / duration         otherwise (truncated)
/// ```
///
/// # Errors
///
/// Returns [`VaultError::Overflow`] if `deposited * elapsed` overflows `i128`.
/// A well-formed position that passed creation validation can never trigger
/// this in practice.
pub fn vested(pos: &VaultPosition, now: u64) -> Result<i128, VaultError> {
    if !cliff_reached(pos, now) {
        return Ok(0);
    }

    let total_duration = duration(pos);
    if total_duration == 0 {
        return Ok(pos.deposited);
    }

    let consumed = elapsed(pos, now);
    if consumed >= total_duration {
        return Ok(pos.deposited);
    }

    let numerator = pos
        .deposited
        .checked_mul(consumed as i128)
        .ok_or(VaultError::Overflow)?;
    let raw = numerator
        .checked_div(total_duration as i128)
        .ok_or(VaultError::Overflow)?;

    Ok(raw.min(pos.deposited))
}

/// Tokens the beneficiary can withdraw right now: `vested(now) - withdrawn`.
///
/// Saturates at zero.
pub fn withdrawable(pos: &VaultPosition, now: u64) -> Result<i128, VaultError> {
    let v = vested(pos, now)?;
    Ok(v.saturating_sub(pos.withdrawn))
}

/// Unvested portion that returns to the sender on revoke: `deposited - vested(now)`.
pub fn refundable(pos: &VaultPosition, now: u64) -> Result<i128, VaultError> {
    let v = vested(pos, now)?;
    Ok(pos.deposited.saturating_sub(v))
}

/// Outstanding contract liability for this position: `deposited - withdrawn`.
pub fn liability(pos: &VaultPosition) -> Result<i128, VaultError> {
    pos.deposited
        .checked_sub(pos.withdrawn)
        .ok_or(VaultError::ArithmeticOverflow)
}
