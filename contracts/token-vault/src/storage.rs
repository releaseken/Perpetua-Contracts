//! Storage access and TTL management for the token-vault contract.
//!
//! # Storage tiers
//!
//! | Key | Tier | TTL strategy |
//! |-----|------|-------------|
//! | `Config` | instance | always pinned to `max_ttl()` |
//! | `NextPositionId` | instance | same instance TTL |
//! | `Position(id)` | persistent | remaining lifetime + 30-day buffer, floor |

use soroban_sdk::Env;

use crate::errors::VaultError;
use crate::types::{DataKey, VaultConfig, VaultPosition};

/// Nominal Stellar ledger close time in seconds.
pub const SECONDS_PER_LEDGER: u64 = 5;

/// 30-day buffer added to a position's remaining lifetime when targeting TTL.
pub const TTL_BUFFER_SECONDS: u64 = 30 * 24 * 60 * 60;

/// Minimum floor TTL for a position entry (~30 days + 10 % drift margin).
pub const MIN_POSITION_TTL_LEDGERS: u32 = {
    let with_headroom = TTL_BUFFER_SECONDS * 11 / 10;
    let ledgers = with_headroom.div_ceil(SECONDS_PER_LEDGER);
    if ledgers > u32::MAX as u64 {
        u32::MAX
    } else {
        ledgers as u32
    }
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Ceiling division: `ceil(seconds / SECONDS_PER_LEDGER)`.
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

/// Network's current maximum entry TTL.
pub fn max_entry_ttl(env: &Env) -> u32 {
    env.storage().max_ttl()
}

// ---------------------------------------------------------------------------
// Instance TTL
// ---------------------------------------------------------------------------

/// Pin the instance entry to the network maximum.
pub fn extend_instance(env: &Env) {
    let max_ttl = env.storage().max_ttl();
    env.storage().instance().extend_ttl(max_ttl, max_ttl);
}

// ---------------------------------------------------------------------------
// Config helpers
// ---------------------------------------------------------------------------

/// Load the config or return `NotInitialized`.
pub fn load_config(env: &Env) -> Result<VaultConfig, VaultError> {
    let config = env
        .storage()
        .instance()
        .get(&DataKey::Config)
        .ok_or(VaultError::NotInitialized)?;
    extend_instance(env);
    Ok(config)
}

/// Write the config and bump the instance TTL.
pub fn save_config(env: &Env, config: &VaultConfig) {
    env.storage().instance().set(&DataKey::Config, config);
    extend_instance(env);
}

// ---------------------------------------------------------------------------
// Position ID counter
// ---------------------------------------------------------------------------

/// Read the next position ID (defaults to 0 on a fresh contract).
pub fn next_position_id(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&DataKey::NextPositionId)
        .unwrap_or(0)
}

/// Allocate the next position ID, checking for `u64` overflow.
pub fn allocate_position_id(env: &Env) -> Result<u64, VaultError> {
    let current = next_position_id(env);
    let next = current
        .checked_add(1)
        .ok_or(VaultError::ArithmeticOverflow)?;
    env.storage()
        .instance()
        .set(&DataKey::NextPositionId, &next);
    extend_instance(env);
    Ok(current)
}

/// Number of issued position IDs.
pub fn position_count(env: &Env) -> u64 {
    next_position_id(env)
}

// ---------------------------------------------------------------------------
// Position persistence
// ---------------------------------------------------------------------------

/// How many ledgers to keep a position entry alive at `now`.
pub fn ttl_target_ledgers(env: &Env, pos: &VaultPosition) -> u32 {
    let now = env.ledger().timestamp();

    let effective_end = pos
        .end_time
        .saturating_add(pos.paused_total)
        .saturating_add(match pos.paused_at {
            Some(paused_at) => now.saturating_sub(paused_at),
            None => 0,
        });

    let remaining = effective_end.saturating_sub(now);
    let target = seconds_to_ledgers(remaining.saturating_add(TTL_BUFFER_SECONDS));
    let floored = target.max(MIN_POSITION_TTL_LEDGERS);
    floored.min(max_entry_ttl(env))
}

/// Read a position for a mutating call, extending its TTL.
pub fn load_position(env: &Env, position_id: u64) -> Result<VaultPosition, VaultError> {
    let key = DataKey::Position(position_id);
    let pos = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(VaultError::PositionNotFound)?;
    extend_position(env, position_id, &pos);
    Ok(pos)
}

/// Read a position without extending its TTL (views and preflight passes).
pub fn peek_position(env: &Env, position_id: u64) -> Result<VaultPosition, VaultError> {
    env.storage()
        .persistent()
        .get(&DataKey::Position(position_id))
        .ok_or(VaultError::PositionNotFound)
}

/// Write a position and extend its TTL.
pub fn save_position(env: &Env, position_id: u64, pos: &VaultPosition) {
    let key = DataKey::Position(position_id);
    env.storage().persistent().set(&key, pos);
    extend_position(env, position_id, pos);
}

/// Extend a position's persistent TTL.
pub fn extend_position(env: &Env, position_id: u64, pos: &VaultPosition) {
    let target = ttl_target_ledgers(env, pos);
    env.storage()
        .persistent()
        .extend_ttl(&DataKey::Position(position_id), target, target);
}

/// Return `true` if the persistent entry for `position_id` exists.
pub fn position_exists(env: &Env, position_id: u64) -> bool {
    env.storage()
        .persistent()
        .has(&DataKey::Position(position_id))
}
