//! Storage access and TTL management for the cliff-vesting contract.
//!
//! # Storage tiers
//!
//! | Key | Tier | TTL strategy |
//! |-----|------|-------------|
//! | `Config` | instance | always pinned to `max_ttl()` |
//! | `PendingAdmin` | instance | same instance TTL, cleared on rotation |
//! | `NextScheduleId` | instance | same instance TTL |
//! | `Schedule(id)` | persistent | remaining lifetime + 30-day buffer, floor |
//!
//! # Ordering guarantee
//!
//! `NextScheduleId` must never expire before the schedules it issued.  Because
//! the instance entry is always pinned to the network maximum and persistent
//! entries are bounded by `max_entry_ttl`, this invariant holds automatically.

use soroban_sdk::{Address, Env};

use crate::error::Error;
use crate::types::{DataKey, VestingConfig, VestingSchedule};

/// Nominal Stellar ledger close time in seconds.
pub const SECONDS_PER_LEDGER: u64 = 5;

/// 30-day buffer added to a schedule's remaining lifetime when targeting TTL.
pub const TTL_BUFFER_SECONDS: u64 = 30 * 24 * 60 * 60;

/// Minimum floor TTL for a schedule entry (~30 days + 10% drift margin).
pub const MIN_SCHEDULE_TTL_LEDGERS: u32 = {
    let with_headroom = TTL_BUFFER_SECONDS * 11 / 10;
    let ledgers = (with_headroom + SECONDS_PER_LEDGER - 1) / SECONDS_PER_LEDGER;
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
pub fn load_config(env: &Env) -> Result<VestingConfig, Error> {
    let config = env
        .storage()
        .instance()
        .get(&DataKey::Config)
        .ok_or(Error::NotInitialized)?;
    extend_instance(env);
    Ok(config)
}

/// Write the config and bump the instance TTL.
pub fn save_config(env: &Env, config: &VestingConfig) {
    env.storage().instance().set(&DataKey::Config, config);
    extend_instance(env);
}

// ---------------------------------------------------------------------------
// Schedule ID counter
// ---------------------------------------------------------------------------

/// Read the next schedule ID (defaults to 0 on a fresh contract).
pub fn next_schedule_id(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&DataKey::NextScheduleId)
        .unwrap_or(0)
}

/// Allocate the next schedule ID, checking for `u64` overflow.
pub fn allocate_schedule_id(env: &Env) -> Result<u64, Error> {
    let current = next_schedule_id(env);
    let next = current
        .checked_add(1)
        .ok_or(Error::ScheduleIdOverflow)?;
    env.storage()
        .instance()
        .set(&DataKey::NextScheduleId, &next);
    extend_instance(env);
    Ok(current)
}

/// Number of issued schedule IDs (== `NextScheduleId`).
pub fn schedule_count(env: &Env) -> u64 {
    next_schedule_id(env)
}

// ---------------------------------------------------------------------------
// Schedule persistence
// ---------------------------------------------------------------------------

/// How many ledgers to keep a schedule entry alive at `now`.
pub fn ttl_target_ledgers(env: &Env, schedule: &VestingSchedule) -> u32 {
    let now = env.ledger().timestamp();

    // Account for in-progress pause: the effective end slides forward.
    let effective_end = schedule
        .end_time
        .saturating_add(schedule.paused_total)
        .saturating_add(match schedule.paused_at {
            Some(paused_at) => now.saturating_sub(paused_at),
            None => 0,
        });

    let remaining = effective_end.saturating_sub(now);
    let target = seconds_to_ledgers(remaining.saturating_add(TTL_BUFFER_SECONDS));
    let floored = target.max(MIN_SCHEDULE_TTL_LEDGERS);
    floored.min(max_entry_ttl(env))
}

/// Read a schedule for a mutating call, extending its TTL.
pub fn load_schedule(env: &Env, schedule_id: u64) -> Result<VestingSchedule, Error> {
    let key = DataKey::Schedule(schedule_id);
    let schedule = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::ScheduleNotFound)?;
    extend_schedule(env, schedule_id, &schedule);
    Ok(schedule)
}

/// Read a schedule without extending its TTL (views and batch preflight).
///
/// Issue #404: `peek_schedule` is used throughout the batch deserialization
/// path so that each schedule is deserialized at most once per batch call.
/// Once loaded, all subsequent per-item computations operate on the already-
/// deserialized `VestingSchedule` value with no additional storage round-trips.
pub fn peek_schedule(env: &Env, schedule_id: u64) -> Result<VestingSchedule, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Schedule(schedule_id))
        .ok_or(Error::ScheduleNotFound)
}

/// Write a schedule and extend its TTL.
pub fn save_schedule(env: &Env, schedule_id: u64, schedule: &VestingSchedule) {
    let key = DataKey::Schedule(schedule_id);
    env.storage().persistent().set(&key, schedule);
    extend_schedule(env, schedule_id, schedule);
}

/// Extend a schedule's persistent TTL.
pub fn extend_schedule(env: &Env, schedule_id: u64, schedule: &VestingSchedule) {
    let target = ttl_target_ledgers(env, schedule);
    env.storage().persistent().extend_ttl(
        &DataKey::Schedule(schedule_id),
        target,
        target,
    );
}

/// Return `true` if the persistent entry for `schedule_id` exists.
pub fn schedule_exists(env: &Env, schedule_id: u64) -> bool {
    env.storage()
        .persistent()
        .has(&DataKey::Schedule(schedule_id))
}

// ---------------------------------------------------------------------------
// Zero-address / forbidden-admin guards
// ---------------------------------------------------------------------------

pub fn is_zero_address(env: &Env, addr: &Address) -> bool {
    use soroban_sdk::String;
    let zero = Address::from_string(&String::from_str(
        env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    ));
    *addr == zero
}

pub fn is_forbidden_admin(env: &Env, addr: &Address) -> bool {
    is_zero_address(env, addr) || *addr == env.current_contract_address()
}
