//! Storage access and TTL management for the event-sink contract.
//!
//! # Storage tiers
//!
//! | Key | Tier | TTL strategy |
//! |-----|------|-------------|
//! | `Config` | instance | always pinned to `max_ttl()` |
//! | `PendingAdmin` | instance | same instance TTL, cleared on accept/rotate |
//! | `EntryCount` | instance | same instance TTL |
//! | `Emitter(addr)` — allow-list flag | persistent | `MIN_ENTRY_TTL_LEDGERS` |
//!
//! No per-entry persistent storage: entries are emitted as ledger events only,
//! keeping the on-chain footprint constant regardless of throughput.

use soroban_sdk::{Address, Env};

use crate::error::SinkError;
use crate::types::{DataKey, SinkConfig};

/// Nominal Stellar ledger close time in seconds.
pub const SECONDS_PER_LEDGER: u64 = 5;

/// 30-day buffer for persistent allow-list entries.
const TTL_BUFFER_SECONDS: u64 = 30 * 24 * 60 * 60;

/// Minimum floor TTL for persistent entries (~30 days + 10 % drift margin).
pub const MIN_ENTRY_TTL_LEDGERS: u32 = {
    let with_headroom = TTL_BUFFER_SECONDS * 11 / 10;
    let ledgers = with_headroom.div_ceil(SECONDS_PER_LEDGER);
    if ledgers > u32::MAX as u64 {
        u32::MAX
    } else {
        ledgers as u32
    }
};

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

/// Load the config or return [`SinkError::NotInitialized`].
pub fn load_config(env: &Env) -> Result<SinkConfig, SinkError> {
    let config = env
        .storage()
        .instance()
        .get(&DataKey::Config)
        .ok_or(SinkError::NotInitialized)?;
    extend_instance(env);
    Ok(config)
}

/// Write the config and bump the instance TTL.
pub fn save_config(env: &Env, config: &SinkConfig) {
    env.storage().instance().set(&DataKey::Config, config);
    extend_instance(env);
}

// ---------------------------------------------------------------------------
// Entry counter
// ---------------------------------------------------------------------------

/// Read the current entry count (defaults to `0` on a fresh contract).
pub fn entry_count(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&DataKey::EntryCount)
        .unwrap_or(0)
}

/// Atomically read the current count and increment it, returning the value
/// *before* the increment (i.e. the ID assigned to the new entry).
///
/// # Errors
///
/// Returns [`SinkError::EntryCountOverflow`] if the counter is at `u64::MAX`.
pub fn allocate_entry_id(env: &Env) -> Result<u64, SinkError> {
    let current = entry_count(env);
    let next = current
        .checked_add(1)
        .ok_or(SinkError::EntryCountOverflow)?;
    env.storage().instance().set(&DataKey::EntryCount, &next);
    extend_instance(env);
    Ok(current)
}

// ---------------------------------------------------------------------------
// Emitter allow-list (persistent storage)
// ---------------------------------------------------------------------------

/// Storage key for a registered emitter address.
///
/// We use a free-standing `contracttype` enum variant that wraps an `Address`
/// so each emitter occupies its own persistent ledger entry, avoiding an
/// unbounded `Map` deserialization cost.
fn emitter_key(emitter: &Address) -> soroban_sdk::Val {
    // We store a boolean flag under a composite key built from a fixed prefix
    // symbol and the address, encoded as a tuple.  Using the raw Val approach
    // keeps compatibility with the `has` / `get` / `set` / `remove` API without
    // requiring an extra contracttype.
    use soroban_sdk::{IntoVal, Symbol};
    (Symbol::new(emitter.env(), "emitter"), emitter.clone()).into_val(emitter.env())
}

/// Return `true` if `emitter` is on the allow-list.
pub fn is_registered_emitter(env: &Env, emitter: &Address) -> bool {
    env.storage()
        .persistent()
        .get::<_, bool>(&emitter_key(emitter))
        .unwrap_or(false)
}

/// Add `emitter` to the allow-list.
pub fn register_emitter(env: &Env, emitter: &Address) {
    let key = emitter_key(emitter);
    env.storage().persistent().set(&key, &true);
    env.storage().persistent().extend_ttl(
        &key,
        MIN_ENTRY_TTL_LEDGERS,
        MIN_ENTRY_TTL_LEDGERS.min(env.storage().max_ttl()),
    );
}

/// Remove `emitter` from the allow-list.
pub fn deregister_emitter(env: &Env, emitter: &Address) {
    env.storage().persistent().remove(&emitter_key(emitter));
}

// ---------------------------------------------------------------------------
// Zero-address / forbidden-admin guards
// ---------------------------------------------------------------------------

/// Return `true` if `addr` is the all-zeros Stellar account (`G…WHF`), which
/// cannot sign and therefore must never become admin.
pub fn is_zero_address(env: &Env, addr: &Address) -> bool {
    use soroban_sdk::String;
    let zero = Address::from_string(&String::from_str(
        env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    ));
    *addr == zero
}

/// Return `true` for any address that must never be installed as admin.
pub fn is_forbidden_admin(env: &Env, addr: &Address) -> bool {
    is_zero_address(env, addr) || *addr == env.current_contract_address()
}
