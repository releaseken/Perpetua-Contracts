//! Storage access and TTL management for the dispute-window contract.
//!
//! # Storage tiers
//!
//! | Entry                 | Tier       | TTL policy                              |
//! |-----------------------|------------|-----------------------------------------|
//! | `DataKey::Config`     | instance   | always pinned to `max_ttl()`            |
//! | `DataKey::Claim(id)`  | persistent | extended to `CLAIM_TTL_LEDGERS` on each touch; bumped to `max_ttl()` for active windows (issue #417) |
//!
//! ## Why instance entries are pinned to max_ttl
//!
//! The config blob carries the admin, arbiter, and window lifecycle state.
//! Losing it to archival during an active dispute window would make the contract
//! permanently inoperable.  Pinning to `max_ttl()` on every mutating call is the
//! cheapest reliable guarantee.
//!
//! ## Persistent claim entries and archival resilience (issue #417)
//!
//! Claim records may need to remain readable for weeks after the window closes
//! (while settlements are processed and indexers replay state).  The contract
//! therefore:
//!
//! 1. **Extends on every touch** (`load_claim`, `save_claim`) so an actively
//!    disputed claim never expires during processing.
//! 2. **Extends generously at submission**, targeting at least
//!    `CLAIM_SETTLEMENT_BUFFER_SECONDS` beyond the window's deadline.
//! 3. **Provides a permissionless `extend_claim_ttl`** keeper path so any
//!    party — the claimant, an indexer, or a cron-style keeper — can top up a
//!    claim without the admin's cooperation (see `lib.rs`).
//!
//! The buffer is intentionally generous: a claim that expires before a claimant
//! can settle is a fund-access incident.

use soroban_sdk::Env;

use crate::error::DisputeError;
use crate::types::{Claim, DataKey, WindowConfig};

// ---------------------------------------------------------------------------
// TTL constants (issue #417)
// ---------------------------------------------------------------------------

/// Nominal Stellar ledger close time, in seconds.
///
/// Conservative (network can be slower), so ledger counts derived from
/// wall-clock durations always over-estimate, erring toward longer TTLs.
pub const SECONDS_PER_LEDGER: u64 = 5;

/// Buffer added on top of the window deadline when computing a claim's initial
/// TTL target: 90 days.  This covers the settlement phase that follows window
/// close and gives keepers a wide sweep window.
pub const CLAIM_SETTLEMENT_BUFFER_SECONDS: u64 = 90 * 24 * 60 * 60;

/// Floor for any persistent claim entry, regardless of remaining window
/// lifetime: roughly 30 days at nominal close time, with a 10% drift margin.
///
/// Computed as: ceil(30d * 1.1 / 5s) = ceil(2_851_200 / 5) = 570_241 ledgers.
pub const MIN_CLAIM_TTL_LEDGERS: u32 =
    ((30 * 24 * 3600 * 11 + 9) / 10 / SECONDS_PER_LEDGER as u32) as u32;

/// Threshold below which the instance entry is re-extended.  ~1 day.
pub const INSTANCE_LIFETIME_THRESHOLD: u32 = 17_280;
/// Amount added when the instance entry falls below threshold.  ~7 days.
pub const INSTANCE_BUMP_AMOUNT: u32 = 120_960;

// ---------------------------------------------------------------------------
// Helpers: instance TTL
// ---------------------------------------------------------------------------

/// Extend the instance entry.
///
/// Called on every mutating entry point.  Uses `max_ttl()` rather than a fixed
/// constant so protocol upgrades that raise the ceiling are automatically
/// honoured.
pub fn extend_instance(env: &Env) {
    let max_ttl = env.storage().max_ttl();
    env.storage().instance().extend_ttl(max_ttl, max_ttl);
}

// ---------------------------------------------------------------------------
// Helpers: config (instance storage)
// ---------------------------------------------------------------------------

/// Write the window configuration blob.
pub fn save_config(env: &Env, config: &WindowConfig) {
    env.storage().instance().set(&DataKey::Config, config);
    extend_instance(env);
}

/// Read the window configuration or return [`DisputeError::NotInitialized`].
pub fn load_config(env: &Env) -> Result<WindowConfig, DisputeError> {
    let cfg = env
        .storage()
        .instance()
        .get(&DataKey::Config)
        .ok_or(DisputeError::NotInitialized)?;
    extend_instance(env);
    Ok(cfg)
}

// ---------------------------------------------------------------------------
// Helpers: persistent claim entries (issue #417)
// ---------------------------------------------------------------------------

/// Convert a wall-clock duration in seconds to a ledger count, rounding up.
///
/// Ceiling ensures the resulting ledger count — when converted back to seconds —
/// is never less than the requested duration.  Saturates at `u32::MAX`.
pub fn seconds_to_ledgers(seconds: u64) -> u32 {
    let ledgers = seconds
        .saturating_add(SECONDS_PER_LEDGER - 1)
        .saturating_div(SECONDS_PER_LEDGER);
    ledgers.min(u32::MAX as u64) as u32
}

/// TTL target (ledgers) for a claim entry given the window's current deadline.
///
/// Targets the remaining time until `deadline` plus
/// `CLAIM_SETTLEMENT_BUFFER_SECONDS`, floored at `MIN_CLAIM_TTL_LEDGERS` and
/// clamped to the network's `max_entry_ttl`.
///
/// A claim submitted before the window opens targets the full deadline horizon.
/// After the window closes, the buffer alone keeps claims readable for the
/// settlement phase.
pub fn claim_ttl_target(env: &Env, deadline: u64) -> u32 {
    let now = env.ledger().timestamp();
    let remaining = deadline.saturating_sub(now);
    let with_buffer = remaining.saturating_add(CLAIM_SETTLEMENT_BUFFER_SECONDS);
    let ledgers = seconds_to_ledgers(with_buffer);
    let floored = ledgers.max(MIN_CLAIM_TTL_LEDGERS);
    floored.min(env.storage().max_ttl())
}

/// Write a claim entry and extend its TTL to the schedule-aware target.
pub fn save_claim(env: &Env, stream_id: u64, claim: &Claim, deadline: u64) {
    let key = DataKey::Claim(stream_id);
    env.storage().persistent().set(&key, claim);
    extend_claim(env, stream_id, deadline);
}

/// Load a claim and refresh its TTL.
///
/// Returns [`DisputeError::ClaimNotFound`] when the entry does not exist or
/// has been archived.
pub fn load_claim(env: &Env, stream_id: u64, deadline: u64) -> Result<Claim, DisputeError> {
    let key = DataKey::Claim(stream_id);
    let claim: Claim = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(DisputeError::ClaimNotFound)?;
    extend_claim(env, stream_id, deadline);
    Ok(claim)
}

/// Read a claim without changing its TTL.  Used by views and batch preflight.
pub fn peek_claim(env: &Env, stream_id: u64) -> Result<Claim, DisputeError> {
    env.storage()
        .persistent()
        .get(&DataKey::Claim(stream_id))
        .ok_or(DisputeError::ClaimNotFound)
}

/// Extend a claim entry to its schedule-aware TTL target.
///
/// This is the core of issue #417: every touch automatically renews the entry
/// so a claim cannot expire while actively being processed.
pub fn extend_claim(env: &Env, stream_id: u64, deadline: u64) {
    let target = claim_ttl_target(env, deadline);
    env.storage()
        .persistent()
        .extend_ttl(&DataKey::Claim(stream_id), target, target);
}

/// Extend a claim entry to the network's current `max_ttl`, regardless of the
/// deadline target.
///
/// Used by the permissionless `extend_claim_ttl` keeper path (issue #417).
/// If the entry is already at `max_ttl`, the host no-ops silently — the
/// contract does not need to guard against double-extension.
pub fn extend_claim_to_max(env: &Env, stream_id: u64) {
    let max_ttl = env.storage().max_ttl();
    env.storage()
        .persistent()
        .extend_ttl(&DataKey::Claim(stream_id), max_ttl, max_ttl);
}

/// True if a claim entry is currently present (not archived) in persistent storage.
pub fn claim_exists(env: &Env, stream_id: u64) -> bool {
    env.storage()
        .persistent()
        .has(&DataKey::Claim(stream_id))
}
