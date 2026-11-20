//! Storage access and TTL management for the pause-clock contract.
//!
//! # Storage tiers
//!
//! | Key | Tier | TTL strategy |
//! |-----|------|-------------|
//! | `Config` | instance | always pinned to `max_ttl()` |
//! | `PendingAdmin` | instance | same instance TTL, cleared on accept/single-step rotate |
//! | `PausedTotal(stream_id)` | persistent | `MIN_ENTRY_TTL_LEDGERS`, extended on every touch and by keeper |
//!
//! # Why instance entries are pinned to maximum
//!
//! The `Config` entry carries the stream-contract reference.  If it archived,
//! every entry point that calls `load_config` would return `NotInitialized`,
//! making the contract entirely non-functional until someone restores it.
//! Pinning to `max_ttl()` on every mutation guarantees this never happens.
//!
//! # Why persistent entries have a floor TTL
//!
//! A per-stream `PausedTotal` accumulates the paused time for that stream.
//! Batch-claim callers may read it long after the global clock last moved, so
//! the entry must stay alive at least `MIN_ENTRY_TTL_LEDGERS` beyond its last
//! write.  The permissionless `extend_stream_ttl` keeper path re-extends it on
//! demand.

use soroban_sdk::{Address, Env};

use crate::error::Error;
use crate::types::{ClockConfig, DataKey};

/// Nominal Stellar ledger close time in seconds.
pub const SECONDS_PER_LEDGER: u64 = 5;

/// Minimum TTL floor for persistent entries, in ledgers.
/// Approximately 30 days at the nominal 5-second close time, with a 10%
/// headroom multiplier to absorb ledger-close drift.
pub const MIN_ENTRY_TTL_LEDGERS: u32 = {
    let buffer_seconds: u64 = 30 * 24 * 60 * 60;
    let with_headroom = buffer_seconds * 11 / 10;
    let ledgers = (with_headroom + SECONDS_PER_LEDGER - 1) / SECONDS_PER_LEDGER;
    if ledgers > u32::MAX as u64 {
        u32::MAX
    } else {
        ledgers as u32
    }
};

/// TTL threshold below which we trigger a bump for persistent entries.
/// Set equal to the floor so we always bump any entry that falls below
/// our minimum acceptable window.
pub const ENTRY_LIFETIME_THRESHOLD: u32 = MIN_ENTRY_TTL_LEDGERS;

// ---------------------------------------------------------------------------
// Instance TTL
// ---------------------------------------------------------------------------

/// Pin the instance entry to the network maximum.
///
/// Called on every mutating entry point.  The instance entry carries `Config`
/// and `PendingAdmin`, both of which are uninhabitable if the entry archives.
pub fn extend_instance(env: &Env) {
    let max_ttl = env.storage().max_ttl();
    env.storage().instance().extend_ttl(max_ttl, max_ttl);
}

// ---------------------------------------------------------------------------
// Config helpers
// ---------------------------------------------------------------------------

/// Load the config or return `NotInitialized`.
///
/// Also bumps the instance TTL, so a read on a mutating path keeps the
/// instance entry alive.
pub fn load_config(env: &Env) -> Result<ClockConfig, Error> {
    let config = env
        .storage()
        .instance()
        .get(&DataKey::Config)
        .ok_or(Error::NotInitialized)?;
    extend_instance(env);
    Ok(config)
}

/// Write the config and bump the instance TTL.
pub fn save_config(env: &Env, config: &ClockConfig) {
    env.storage().instance().set(&DataKey::Config, config);
    extend_instance(env);
}

// ---------------------------------------------------------------------------
// Per-stream paused-total helpers
// ---------------------------------------------------------------------------

/// Read the cumulative paused seconds for `stream_id`.
///
/// Returns `0` for a stream whose entry has never been written (i.e. a stream
/// with no recorded pause history).  Does not bump TTL — use `load_stream_total`
/// on a mutating path.
pub fn peek_stream_total(env: &Env, stream_id: u64) -> u64 {
    env.storage()
        .persistent()
        .get::<_, u64>(&DataKey::PausedTotal(stream_id))
        .unwrap_or(0)
}

/// Read the cumulative paused seconds for `stream_id` on a mutating path,
/// bumping its TTL.
pub fn load_stream_total(env: &Env, stream_id: u64) -> u64 {
    let key = DataKey::PausedTotal(stream_id);
    let total: u64 = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or(0);
    extend_stream_entry(env, stream_id);
    total
}

/// Write the cumulative paused seconds for `stream_id` and extend its TTL.
pub fn save_stream_total(env: &Env, stream_id: u64, total: u64) {
    let key = DataKey::PausedTotal(stream_id);
    env.storage().persistent().set(&key, &total);
    extend_stream_entry(env, stream_id);
}

/// Extend the persistent TTL for a stream's paused-total entry.
///
/// Targets `MIN_ENTRY_TTL_LEDGERS` or the network maximum, whichever is
/// smaller.  Called from `save_stream_total`, `load_stream_total`, and the
/// permissionless `extend_stream_ttl` keeper path.
pub fn extend_stream_entry(env: &Env, stream_id: u64) {
    let target = MIN_ENTRY_TTL_LEDGERS.min(env.storage().max_ttl());
    env.storage().persistent().extend_ttl(
        &DataKey::PausedTotal(stream_id),
        ENTRY_LIFETIME_THRESHOLD,
        target,
    );
}

/// Return `true` if a persistent entry exists for `stream_id`.
pub fn stream_total_exists(env: &Env, stream_id: u64) -> bool {
    env.storage()
        .persistent()
        .has(&DataKey::PausedTotal(stream_id))
}

// ---------------------------------------------------------------------------
// Zero-address guard
// ---------------------------------------------------------------------------

/// The all-zeros Stellar account (`G…WHF`) cannot sign; installing it as admin
/// would permanently lock every admin-gated operation.
pub fn is_zero_address(env: &Env, addr: &Address) -> bool {
    use soroban_sdk::String;
    let zero = Address::from_string(&String::from_str(
        env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    ));
    *addr == zero
}

/// An address that must never become admin: the zero account or the contract
/// itself.
pub fn is_forbidden_admin(env: &Env, addr: &Address) -> bool {
    is_zero_address(env, addr) || *addr == env.current_contract_address()
}
