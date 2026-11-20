//! Pure accrual arithmetic for cliff-vesting schedules.
//!
//! All functions are free of side effects: no `Env`, no storage, no host
//! calls.  Every computation is derived fresh from the schedule at call time
//! (non-incremental) so conservation is exact and truncation errors never
//! accumulate.
//!
//! # Issue #404 — optimizing loop deserialization
//!
//! These functions are called from the batch entry points inside tight loops.
//! They operate only on the already-deserialized `VestingSchedule` struct,
//! meaning each batch item pays deserialization exactly once (at the
//! `load_schedule` / `peek_schedule` call site) and then executes pure integer
//! arithmetic in subsequent loop iterations.  No re-deserialization, no
//! redundant storage reads.
//!
//! # Rounding
//!
//! Integer division truncates toward zero (rounds down), always in the
//! beneficiary's disfavour.  The residue stays in the pool and returns to the
//! grantor on revoke, so the contract can never owe more than it holds.
//!
//! # Conservation identity
//!
//! `vested(now) + refundable(now) == deposited` is an algebraic identity, not
//! an approximation.  No dust term.

use crate::error::Error;
use crate::types::VestingSchedule;

/// Map wall-clock time to the schedule's own clock.
///
/// The schedule clock freezes while paused.  After a resume, `paused_total`
/// has absorbed the paused interval so the clock picks up exactly where it
/// stopped.
pub fn stream_time(schedule: &VestingSchedule, now: u64) -> u64 {
    let frozen_at = schedule.paused_at.unwrap_or(now);
    frozen_at.saturating_sub(schedule.paused_total)
}

/// Total schedule duration, in seconds.
pub fn duration(schedule: &VestingSchedule) -> u64 {
    schedule.end_time.saturating_sub(schedule.start_time)
}

/// Seconds of the schedule consumed at `now`, clamped to `[0, duration]`.
pub fn elapsed(schedule: &VestingSchedule, now: u64) -> u64 {
    let clock = stream_time(schedule, now);
    let capped = clock.min(schedule.end_time);
    capped.saturating_sub(schedule.start_time)
}

/// Whether the cliff gate has opened.
///
/// The cliff is evaluated against the **schedule clock**, not wall-clock time,
/// so a schedule paused across its cliff does not silently open while frozen.
pub fn cliff_reached(schedule: &VestingSchedule, now: u64) -> bool {
    stream_time(schedule, now) >= schedule.cliff_time
}

/// Total tokens vested at `now`.
///
/// # Formula
///
/// ```text
/// vested = 0                                          if cliff not reached
/// vested = deposited                                  if duration == 0  (post-revoke)
/// vested = deposited                                  if elapsed >= duration  (maturity)
/// vested = deposited * elapsed / duration             otherwise (round down)
/// ```
///
/// # Errors
///
/// Returns [`Error::Overflow`] if `deposited * elapsed` overflows `i128`.
/// `create_schedule` front-loads the same guard so a well-formed schedule in
/// storage cannot trigger this in practice.
pub fn vested(schedule: &VestingSchedule, now: u64) -> Result<i128, Error> {
    if !cliff_reached(schedule, now) {
        return Ok(0);
    }

    let total_duration = duration(schedule);
    if total_duration == 0 {
        // Post-revoke: schedule collapsed onto a single instant.
        // `deposited` has been rewritten to the vested amount at revoke time.
        return Ok(schedule.deposited);
    }

    let consumed = elapsed(schedule, now);
    if consumed >= total_duration {
        return Ok(schedule.deposited);
    }

    let numerator = schedule
        .deposited
        .checked_mul(consumed as i128)
        .ok_or(Error::Overflow)?;
    let raw = numerator
        .checked_div(total_duration as i128)
        .ok_or(Error::Overflow)?;

    // Clamp defensively.
    Ok(raw.min(schedule.deposited))
}

/// Tokens the beneficiary can withdraw right now: `vested(now) - withdrawn`.
///
/// Saturates at zero.
pub fn withdrawable(schedule: &VestingSchedule, now: u64) -> Result<i128, Error> {
    let v = vested(schedule, now)?;
    Ok(v.saturating_sub(schedule.withdrawn))
}

/// Unvested portion that returns to the grantor on revoke: `deposited - vested(now)`.
pub fn refundable(schedule: &VestingSchedule, now: u64) -> Result<i128, Error> {
    let v = vested(schedule, now)?;
    Ok(schedule.deposited.saturating_sub(v))
}

/// Outstanding contract liability for this schedule: `deposited - withdrawn`.
pub fn liability(schedule: &VestingSchedule) -> Result<i128, Error> {
    schedule
        .deposited
        .checked_sub(schedule.withdrawn)
        .ok_or(Error::Overflow)
}
