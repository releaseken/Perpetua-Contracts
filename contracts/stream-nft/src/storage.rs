//! Storage access and automated entry lifetime extension for stream-NFT.
//!
//! # Storage tiers
//!
//! | Entry                | Tier       | Archival resilience & TTL strategy                                      |
//! |----------------------|------------|--------------------------------------------------------------------------|
//! | `DataKey::Config`    | instance   | pinned to network `max_ttl()` on every touch via `extend_instance`       |
//! | `DataKey::Supply`    | instance   | pinned to network `max_ttl()` on every touch                             |
//! | `DataKey::Owner(id)` | persistent | automated lifetime extension: `MIN_ENTRY_TTL_LEDGERS` floor on touch    |
//! | `DataKey::Balance(a)`| persistent | automated lifetime extension: `MIN_ENTRY_TTL_LEDGERS` floor on touch    |
//!
//! ## Automated Lifetime Extension & Archival Resilience
//!
//! Soroban state archival unloads persistent entries when their lifetime drops below
//! network minimums unless refreshed. To prevent stream NFT claims and balance records
//! from archiving during prolonged holding periods:
//!
//! 1. **Automated Touch Extension**: All read/write operations automatically bump the
//!    touched entries to at least `MIN_ENTRY_TTL_LEDGERS` (clamped to network `max_ttl()`).
//! 2. **Instance Entry Pinning**: The contract `Config` and `Supply` entries are
//!    automatically pinned to `max_ttl()`, ensuring contract metadata never drops offline.
//! 3. **Permissionless Keeper Sweep**: `extend_token_ttl` and `batch_extend_ttl` allow
//!    keepers, indexers, or owners to proactively refresh token records without requiring
//!    admin authorization or state modifications.

use soroban_sdk::{Address, Env};

use crate::error::Error;
use crate::types::{Config, DataKey};

/// Nominal Stellar ledger close time in seconds.
pub const SECONDS_PER_LEDGER: u64 = 5;

/// 30-day buffer in seconds for persistent entry survival.
pub const PERSISTENT_BUFFER_SECONDS: u64 = 30 * 24 * 60 * 60;

/// Floor for persistent entry TTL: ~30 days with a 10% drift margin.
/// Derived as: ceil(30d * 1.1 / 5s) = ceil(2_851_200 / 5) = 570_241 ledgers.
pub const MIN_ENTRY_TTL_LEDGERS: u32 =
    ((30 * 24 * 3600 * 11 + 9) / 10 / SECONDS_PER_LEDGER as u32) as u32;

/// Maximum batch size for batch keeper sweep operations to bound Soroban host CPU instructions.
pub const MAX_BATCH_SIZE: u32 = 32;

// ---------------------------------------------------------------------------
// Instance TTL helpers
// ---------------------------------------------------------------------------

/// Extend the instance entry to the network maximum lifetime.
pub fn extend_instance(env: &Env) {
    let max_ttl = env.storage().max_ttl();
    env.storage().instance().extend_ttl(max_ttl, max_ttl);
}

// ---------------------------------------------------------------------------
// Persistent TTL helpers
// ---------------------------------------------------------------------------

/// Calculate the target TTL ledgers clamped by network `max_ttl()`.
#[inline(always)]
pub fn target_persistent_ttl(env: &Env) -> u32 {
    let max_ttl = env.storage().max_ttl();
    MIN_ENTRY_TTL_LEDGERS.min(max_ttl)
}

/// Extend a persistent storage key to at least `MIN_ENTRY_TTL_LEDGERS`.
pub fn extend_persistent_key(env: &Env, key: &DataKey) {
    let target = target_persistent_ttl(env);
    env.storage().persistent().extend_ttl(key, target, target);
}

// ---------------------------------------------------------------------------
// Config helpers
// ---------------------------------------------------------------------------

pub fn has_config(env: &Env) -> bool {
    env.storage().instance().has(&DataKey::Config)
}

pub fn save_config(env: &Env, config: &Config) {
    env.storage().instance().set(&DataKey::Config, config);
    extend_instance(env);
}

pub fn load_config(env: &Env) -> Result<Config, Error> {
    extend_instance(env);
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .ok_or(Error::NotInitialized)
}

// ---------------------------------------------------------------------------
// Supply helpers
// ---------------------------------------------------------------------------

pub fn get_supply(env: &Env) -> u64 {
    extend_instance(env);
    env.storage().instance().get(&DataKey::Supply).unwrap_or(0)
}

pub fn set_supply(env: &Env, supply: u64) {
    env.storage().instance().set(&DataKey::Supply, &supply);
    extend_instance(env);
}

// ---------------------------------------------------------------------------
// Owner helpers
// ---------------------------------------------------------------------------

pub fn has_token(env: &Env, stream_id: u64) -> bool {
    let key = DataKey::Owner(stream_id);
    env.storage().persistent().has(&key)
}

pub fn get_owner(env: &Env, stream_id: u64) -> Option<Address> {
    let key = DataKey::Owner(stream_id);
    let owner: Option<Address> = env.storage().persistent().get(&key);
    if owner.is_some() {
        extend_persistent_key(env, &key);
    }
    owner
}

pub fn set_owner(env: &Env, stream_id: u64, owner: &Address) {
    let key = DataKey::Owner(stream_id);
    env.storage().persistent().set(&key, owner);
    extend_persistent_key(env, &key);
}

// ---------------------------------------------------------------------------
// Balance helpers
// ---------------------------------------------------------------------------

pub fn get_balance(env: &Env, owner: &Address) -> u32 {
    let key = DataKey::Balance(owner.clone());
    let balance: u32 = env.storage().persistent().get(&key).unwrap_or(0);
    if balance > 0 {
        extend_persistent_key(env, &key);
    }
    balance
}

pub fn set_balance(env: &Env, owner: &Address, balance: u32) {
    let key = DataKey::Balance(owner.clone());
    if balance == 0 {
        env.storage().persistent().remove(&key);
    } else {
        env.storage().persistent().set(&key, &balance);
        extend_persistent_key(env, &key);
    }
}
