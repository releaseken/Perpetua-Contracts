//! Storage access and TTL management.
//!
//! # Why TTL is the hard part
//!
//! Soroban persistent entries have a time-to-live measured in ledgers. When it
//! runs out the entry is archived and becomes unreadable until explicitly
//! restored. A stream running twelve months will outlive its default TTL.
//!
//! If a stream entry archives the tokens are *not* lost — they sit in the
//! contract's pooled balance — but the accounting entry saying who they belong to
//! is inaccessible until someone pays to restore it. For a payroll or grant
//! primitive that is unacceptable, so the contract engineers around it three
//! ways:
//!
//! 1. **Extend on every touch.** Every function that reads or writes a stream
//!    bumps that entry's TTL. An actively-used stream never expires.
//! 2. **Extend generously at creation**, targeting the stream's remaining
//!    lifetime plus a buffer, clamped to the network maximum.
//! 3. **Permissionless top-ups** via `extend_stream_ttl`, so a keeper —or the
//!    recipient, or any passer-by — can keep a claim readable without the
//!    sender's cooperation.
//!
//! # Instance vs persistent TTL policy
//!
//! The contract uses two Soroban storage lifetimes, and they are *not* managed
//! the same way:
//!
//! | Entry | Lifetime | TTL target | Who bumps it |
//! |-------|----------|-----------|--------------|
//! | [`DataKey::NextStreamId`] (id counter) | **instance** | always the network `max_ttl()` | every mutating call |
//! | [`DataKey::Stream(id)`] (a stream) | **persistent** | remaining life + [`TTL_BUFFER_SECONDS`], floored at [`MIN_STREAM_TTL_LEDGERS`] | every touch + keeper |
//!
//! **Instance entries are always kept at maximum rent.** They are tiny, and the
//! id counter carries the contract's monotonicity invariant: if `NextStreamId`
//! archived, the next `create_stream` would restart ids from zero and collide
//! with live streams. So [`extend_instance`] pins it to `max_ttl()` on *every*
//! mutating call — creation, every withdrawal, every pause, every keeper sweep.
//!
//! **Persistent entries target the stream's remaining lifetime.** They hold the
//! full accounting record, so they are extended to a window that covers the
//! stream's scheduled end plus a keeper buffer. A stream that is still settling
//! keeps a floor of [`MIN_STREAM_TTL_LEDGERS`] so final state stays readable.
//!
//! **Ordering guarantee.** [`DataKey::NextStreamId`] must *never* expire before
//! the streams it issued. Because the instance entry is always pinned to the
//! maximum while a persistent entry is only ever as long-lived as its target,
//! the instance entry is always at least as fresh as any stream — so a live
//! stream can never outlive its own id-counter, and a keeper sweep always
//! bumps both in the same transaction ([`extend_stream_ttl`] calls
//! [`extend_stream`] *and* [`extend_instance`] together).
//!
//! # Safe handling of each storage type
//!
//! - **Instance `NextStreamId`** is read with a fallback of `0` (a fresh
//!   contract), written monotonically, and always re-extended. A restored
//!   instance correctly resumes from the last persisted id, never reusing one.
//! - **Persistent `Stream(id)`** is read through [`load_stream`] (which bumps
//!   TTL) or [`peek_stream`] (view-only, no write). [`stream_exists`] reports
//!   `false` for an archived entry, which combined with the id counter lets the
//!   contract tell "never existed" apart from "needs restoring".
//!
//! The regression tests in `test/ttl.rs` exercise both lifetimes from seeded
//! ledger states and assert that the instance entry cannot expire before the
//! persistent entries it issued.

use soroban_sdk::{Address, Env};

use crate::error::Error;
use crate::types::{DataKey, DelegateGrant, Stream};

/// Nominal Stellar ledger close time, in seconds.
///
/// Ledger close time is a network property, not a protocol constant, and it
/// drifts. Using a deliberately conservative value means the ledger count we
/// derive from a wall-clock duration *over*-estimates how many ledgers that
/// duration spans, which errs toward keeping entries alive longer than needed.
/// That is the safe direction to be wrong in.
pub const SECONDS_PER_LEDGER: u64 = 5;

/// Extra headroom, in seconds, added on top of a stream's remaining lifetime
/// when computing its TTL target. 30 days.
///
/// This is what gives the keeper a wide window to act in: a stream only needs
/// sweeping once its TTL falls inside this buffer, not on the day it would
/// otherwise archive.
pub const TTL_BUFFER_SECONDS: u64 = 30 * 24 * 60 * 60;

/// Safety margin applied to the 30-day minimum TTL to absorb ledger-close drift.
///
/// The network does not close every ledger at exactly 5s; actual close time can
/// drift above or below that nominal value. Using a small headroom means a
/// nominal 30-day floor still remains well above 30 days in wall-clock terms if
/// the network slows to around 5.2s/ledger, while also giving the keeper a bit
/// of slack when the network is slightly faster than expected.
pub const LEDGER_DRIFT_SAFETY_MULTIPLIER_NUMERATOR: u64 = 11;
pub const LEDGER_DRIFT_SAFETY_MULTIPLIER_DENOMINATOR: u64 = 10;

/// Floor for any stream entry's TTL, in ledgers, regardless of how little
/// lifetime the stream has left. Roughly 30 days at the nominal close time,
/// with a 10% headroom to absorb drift.
///
/// A settled stream still has to stay readable: the recipient may not have
/// withdrawn their tail yet, and the indexer needs to see the final state.
pub const MIN_STREAM_TTL_LEDGERS: u32 = ((TTL_BUFFER_SECONDS
    * LEDGER_DRIFT_SAFETY_MULTIPLIER_NUMERATOR
    + (LEDGER_DRIFT_SAFETY_MULTIPLIER_DENOMINATOR - 1))
    / LEDGER_DRIFT_SAFETY_MULTIPLIER_DENOMINATOR
    / SECONDS_PER_LEDGER) as u32;

/// Convert a wall-clock duration into a ledger count, rounding up.
///
/// # Why ceiling, not floor
///
/// This only ever feeds the "how long should this entry live" side of the TTL
/// math (see [`ttl_target_ledgers`]), never the "how much has the stream
/// promised" side. Flooring here would trim a fraction of a ledger off of
/// every TTL target — which can only ever *shorten* the window before an
/// entry becomes eligible to archive, never lengthen it. Ceiling guarantees
/// the opposite: the ledger count returned, converted back to seconds, is
/// always at least the requested duration. That guarantee is exercised
/// directly by `seconds_to_ledgers_round_trip_never_undershoots`.
///
/// Saturates at `u32::MAX`; callers clamp to the network maximum anyway.
pub fn seconds_to_ledgers(seconds: u64) -> u32 {
    let ledgers = seconds
        .saturating_add(SECONDS_PER_LEDGER - 1)
        .saturating_div(SECONDS_PER_LEDGER);
    if ledgers > u32::MAX as u64 {
        u32::MAX
    } else {
        ledgers as u32
    }
}

/// The network's current maximum entry TTL, in ledgers, queried dynamically
/// from the Soroban host environment.
///
/// `max_entry_ttl` is a network parameter that can change on protocol upgrade,
/// so it must never be baked in as a compile-time constant. Every TTL target
/// is clamped against this value at call time, which keeps the rent math
/// correct even if the network raises or lowers the ceiling.
pub fn max_entry_ttl(env: &Env) -> u32 {
    env.storage().max_ttl()
}

/// How many ledgers this stream's entry should be kept alive for, given the
/// current time.
///
/// Targets the stream's remaining lifetime plus `[TTL_BUFFER_SECONDS]`, floored
/// at `[MIN_STREAM_TTL_LEDGERS] and clamped to the network's `max_entry_ttl`.
///
/// A future-dated stream is covered implicitly: `remaining` is measured from
/// now to `end_time`, so the pre-start wait is part of the target. A schedule
/// beyond one TTL window clamps here and is kept alive by the permissionless
/// keeper path — creation deliberately does not reject it (see
/// [`crate::FluxoraStream::create_stream`]).
///
/// The clamp is not optional: a multi-year stream will exceed the network
/// maximum, so it *will* need periodic extension over its life no matter how slowry
/// we extend at creation. That is precisely what the permissionless
/// keeper path exists for.
pub fn ttl_target_ledgers(env: &Env, stream: &Stream) -> u32 {
    let now = env.ledger().timestamp();
    ttl_target_ledgers_at(env, stream, now)
}

pub fn ttl_target_ledgers_at(env: &Env, stream: &Stream, now: u64) -> u32 {
    // A paused stream's end date slides forward in wall-clock terms, so include
    // the accumulated pause when working out how much longer it may run.
    let effective_end = stream
        .end_time
        .saturating_add(stream.paused_total)
        .saturating_add(match stream.paused_at {
            Some(paused_at) => now.saturating_sub(paused_at),
            None => 0,
        });

    let remaining = effective_end.saturating_sub(now);
    // `remaining` spans now → end_time, so for a future-dated stream the
    // pre-start wait is included in the rent target.
    let target = seconds_to_ledgers(remaining.saturating_add(TTL_BUFFER_SECONDS));
    let floored = target.max(MIN_STREAM_TTL_LEDGERS);

    // Query the network maximum at call time rather than assuming a static
    // constant, so a protocol upgrade that changes `max_entry_ttl` is honored.
    floored.min(max_entry_ttl(env))
}

/// Read the next stream id, defaulting to `0` on a fresh contract.
pub fn next_stream_id(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&DataKey::NextStreamId)
        .unwrap_or(0)
}

/// Allocate the next stream id, guarding against `u64` overflow.
///
/// The counter is read from instance storage, incremented with `checked_add`,
/// and written back. If the counter is already at `u64::MAX` the increment
/// would silently wrap to `0` and collide with live streams, so the overflow
/// is surfaced as [`Error::StreamIdOverflow`] instead.
///
/// The instance entry is re-extended to the network maximum on every
/// allocation so the id counter can never archive before the streams it
/// issued (see the module docs on the ordering guarantee).
pub fn allocate_stream_id(env: &Env) -> Result<u64, Error> {
    let current = next_stream_id(env);
    let next = current.checked_add(1).ok_or(Error::StreamIdOverflow)?;
    env.storage().instance().set(&DataKey::NextStreamId, &next);
    extend_instance(env);
    Ok(next)
}

/// Bump the instance entry. Tiny, and it carries the id counter, so it is
/// always extended to the network maximum.
pub fn extend_instance(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(env.storage().max_ttl(), env.storage().max_ttl());
}
