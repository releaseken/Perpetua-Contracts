//! Storage access and automated entry lifetime (TTL) extension for the `fee-accrual` contract.
//!
//! # Storage tiers & archival resilience (issue #390)
//!
//! | Key | Tier | TTL strategy |
//! |-----|------|--------------|
//! | `Config` | instance | always pinned to `max_ttl()` on every mutation |
//! | `PendingAdmin` | instance | shares instance TTL, cleared on rotation |
//! | `TotalProtocolFees` | instance | shares instance TTL, pinned to `max_ttl()` on update |
//! | `TotalKeeperFees` | instance | shares instance TTL, pinned to `max_ttl()` on update |
//! | `StreamFee(stream_id)` | persistent | extended to `MIN_ENTRY_TTL_LEDGERS` (~30 days + 10% drift margin) on every read/write and via permissionless keeper sweeps |

use soroban_sdk::{Address, Env};

use crate::error::FeeError;
use crate::types::{DataKey, FeeConfig, StreamFeeRecord};

/// Nominal Stellar ledger close time in seconds.
pub const SECONDS_PER_LEDGER: u64 = 5;

/// 30-day buffer in seconds for persistent stream fee records.
pub const TTL_BUFFER_SECONDS: u64 = 30 * 24 * 60 * 60;

/// Minimum TTL floor for persistent entries, in ledgers.
/// Approximately 30 days at the nominal 5-second close time, with a 10%
/// headroom multiplier to absorb ledger-close drift (570,240 ledgers).
pub const MIN_ENTRY_TTL_LEDGERS: u32 = {
    let with_headroom = TTL_BUFFER_SECONDS * 11 / 10;
    let ledgers = with_headroom.div_ceil(SECONDS_PER_LEDGER);
    if ledgers > u32::MAX as u64 {
        u32::MAX
    } else {
        ledgers as u32
    }
};

/// TTL threshold below which a persistent entry bump is triggered.
pub const ENTRY_LIFETIME_THRESHOLD: u32 = MIN_ENTRY_TTL_LEDGERS;

/// Legacy constant alias for persistent bump ledgers (~30 days at 5s/ledger).
pub const BUMP_LEDGERS_PERSISTENT: u32 = 518_400;
/// Legacy constant alias for minimum persistent TTL ledgers (~7 days at 5s/ledger).
pub const MIN_TTL_LEDGERS_PERSISTENT: u32 = 120_960;

/// Convert a wall-clock duration in seconds to a ledger count, rounding up.
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

/// Effective persistent TTL target clamped to the network's `max_ttl()`.
pub fn target_entry_ttl(env: &Env) -> u32 {
    MIN_ENTRY_TTL_LEDGERS.min(env.storage().max_ttl())
}

/// Pin the instance storage entry to the network maximum TTL.
pub fn extend_instance(env: &Env) {
    let max_ttl = env.storage().max_ttl();
    env.storage().instance().extend_ttl(max_ttl, max_ttl);
}

/// Extend the persistent TTL for `DataKey::StreamFee(stream_id)` to the target floor.
/// Returns the target ledger horizon applied.
pub fn extend_stream_fee_ttl(env: &Env, stream_id: u64) -> u32 {
    let target = target_entry_ttl(env);
    let threshold = ENTRY_LIFETIME_THRESHOLD.min(target);
    env.storage().persistent().extend_ttl(
        &DataKey::StreamFee(stream_id),
        threshold,
        target,
    );
    target
}

/// Extend a persistent stream fee entry directly to the network's `max_ttl()`.
pub fn extend_stream_fee_to_max(env: &Env, stream_id: u64) -> u32 {
    let max_ttl = env.storage().max_ttl();
    env.storage()
        .persistent()
        .extend_ttl(&DataKey::StreamFee(stream_id), max_ttl, max_ttl);
    max_ttl
}

/// Return `true` if a persistent fee record exists for `stream_id`.
pub fn has_stream_fee(env: &Env, stream_id: u64) -> bool {
    env.storage()
        .persistent()
        .has(&DataKey::StreamFee(stream_id))
}

/// Load the configuration from instance storage and automatically refresh instance TTL.
pub fn load_config(env: &Env) -> Result<FeeConfig, FeeError> {
    let config: FeeConfig = env
        .storage()
        .instance()
        .get(&DataKey::Config)
        .ok_or(FeeError::NotInitialized)?;
    extend_instance(env);
    Ok(config)
}

/// Read the configuration without mutating TTL (for pure read-only queries).
pub fn peek_config(env: &Env) -> Result<FeeConfig, FeeError> {
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .ok_or(FeeError::NotInitialized)
}

/// Save configuration to instance storage and pin instance TTL to `max_ttl()`.
pub fn save_config(env: &Env, config: &FeeConfig) {
    env.storage().instance().set(&DataKey::Config, config);
    extend_instance(env);
}

/// Read a stream fee record without bumping its TTL (used in views and batch pre-flight).
pub fn peek_stream_fee(env: &Env, stream_id: u64) -> Option<StreamFeeRecord> {
    env.storage()
        .persistent()
        .get(&DataKey::StreamFee(stream_id))
}

/// Read a stream fee record and automatically extend its persistent TTL if present.
pub fn get_stream_fee(env: &Env, stream_id: u64) -> Option<StreamFeeRecord> {
    let key = DataKey::StreamFee(stream_id);
    let record: Option<StreamFeeRecord> = env.storage().persistent().get(&key);
    if record.is_some() {
        extend_stream_fee_ttl(env, stream_id);
    }
    record
}

/// Load a stream fee record on a mutating path, returning `FeeError::FeeRecordNotFound`
/// if absent, and automatically extending its persistent TTL.
pub fn load_stream_fee(env: &Env, stream_id: u64) -> Result<StreamFeeRecord, FeeError> {
    let record = get_stream_fee(env, stream_id).ok_or(FeeError::FeeRecordNotFound)?;
    Ok(record)
}

/// Persist a stream fee record and automatically extend its persistent TTL.
pub fn set_stream_fee(env: &Env, record: &StreamFeeRecord) {
    let key = DataKey::StreamFee(record.stream_id);
    env.storage().persistent().set(&key, record);
    extend_stream_fee_ttl(env, record.stream_id);
}

pub fn get_total_protocol_fees(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::TotalProtocolFees)
        .unwrap_or(0)
}

pub fn set_total_protocol_fees(env: &Env, amount: i128) {
    env.storage()
        .instance()
        .set(&DataKey::TotalProtocolFees, &amount);
    extend_instance(env);
}

pub fn get_total_keeper_fees(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::TotalKeeperFees)
        .unwrap_or(0)
}

pub fn set_total_keeper_fees(env: &Env, amount: i128) {
    env.storage()
        .instance()
        .set(&DataKey::TotalKeeperFees, &amount);
    extend_instance(env);
}

pub fn set_pending_admin(env: &Env, nominee: &Address) {
    env.storage()
        .instance()
        .set(&DataKey::PendingAdmin, nominee);
    extend_instance(env);
}

pub fn get_pending_admin(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::PendingAdmin)
}

pub fn clear_pending_admin(env: &Env) {
    env.storage().instance().remove(&DataKey::PendingAdmin);
    extend_instance(env);
}

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
