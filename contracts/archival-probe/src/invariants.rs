//! # Invariants and Formal Verification for Archival Probe
//!
//! This module documents and provides assertions for formal verification standards,
//! ledger footprint optimization, re-entrancy protection, state synchronization, and
//! automated lifetime extension across single-key and batch probe operations.
//!
//! ## Core Invariants
//!
//! * **I1 (Single Deserialization & Host Instruction Minimization)**:
//!   Persistent storage entries are deserialized at most once during batch probe or
//!   verification passes. In-memory representations (such as [`ProbeRecord`]) are
//!   held in local contiguous memory rather than repeatedly decoded across loop iterations.
//!
//! * **I2 (Persistence Without Extension)**:
//!   The archival canary entry [`Key::Canary`] and standard partition probe entries [`Key::Probe`]
//!   deliberately receive no TTL extension during `plant`, `read`, `batch_plant`, or `batch_read`.
//!   This guarantees that test entries archive early at `min_persistent_ttl` according to network protocol rules.
//!
//! * **I3 (Automated Instance & Entry Lifetime Resilience)**:
//!   Instance metadata (`Key::ProbeCount`, `Key::Executing`) and resilient probes are automatically
//!   extended up to host `max_ttl()` during mutating executions to ensure ledger persistence and
//!   prevent state desynchronization while maintaining bounded footprint overhead.
//!
//! * **I4 (Write-Once / Idempotence)**:
//!   Subsequent calls to `plant`, `plant_probe`, or `batch_plant` must preserve the
//!   first-written note and timestamp, and must not overwrite existing keys or reset
//!   archival countdowns.
//!
//! * **I5 (Safe Arithmetic & Bounds)**:
//!   Batch sizes are capped by [`MAX_BATCH_PROBE_SIZE`] to strictly prevent Soroban host CPU
//!   and memory exhaustion. All counter and arithmetic operations use checked arithmetic.
//!
//! * **I6 (Re-entrancy Immunity and CEI)**:
//!   During mutating operations (`plant`, `plant_probe`, `batch_plant`, `extend_probe_lifetime`),
//!   an instance execution lock ([`Key::Executing`]) enforces mutual exclusion. Any re-entrant or
//!   overlapping mutating call immediately aborts with [`ProbeError::ReentrancyGuard`]. The guard
//!   is guaranteed to be cleared on completion or failure.
//!
//! * **I7 (State Conservation and Desync Resistance)**:
//!   Post-condition assertions verify that `probe_count` increments by exactly the number of
//!   newly planted entries (`new_count == old_count + planted_delta`). Any divergence
//!   aborts execution with [`ProbeError::InvariantViolation`].
//!
//! * **I8 (Error Observability & Categorization)**:
//!   All failure modes must be mapped to stable, typed error discriminants with canonical
//!   human-readable messages and semantic error categories for client SDK observability.

use crate::{
    Key, ProbeError, ProbeRecord, BUMP_AMOUNT_LEDGERS, LIFETIME_THRESHOLD_LEDGERS,
    MAX_BATCH_PROBE_SIZE,
};
use soroban_sdk::{Env, Symbol};

/// Verify that a batch size does not exceed the allowed threshold.
#[inline(always)]
pub fn verify_batch_bounds(size: u32) -> bool {
    size > 0 && size <= MAX_BATCH_PROBE_SIZE
}

/// Assert that the probe record preserves the original note across checks.
#[inline(always)]
pub fn assert_record_integrity(record: &ProbeRecord, expected_note: &Symbol) -> bool {
    record.note == *expected_note
}

/// Check re-entrancy lock status.
#[inline(always)]
pub fn is_executing(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&Key::Executing)
        .unwrap_or(false)
}

/// Acquire the re-entrancy execution lock.
#[inline(always)]
pub fn acquire_executing(env: &Env) -> Result<(), ProbeError> {
    if is_executing(env) {
        return Err(ProbeError::ReentrancyGuard);
    }
    env.storage().instance().set(&Key::Executing, &true);
    Ok(())
}

/// Release the re-entrancy execution lock.
#[inline(always)]
pub fn release_executing(env: &Env) {
    env.storage().instance().remove(&Key::Executing);
}

/// Assert state synchronization post-condition:
/// Verify that the probe counter advanced by exactly `expected_delta` without overflow.
#[inline(always)]
pub fn assert_state_sync(
    initial_count: u32,
    final_count: u32,
    expected_delta: u32,
) -> Result<(), ProbeError> {
    let expected = initial_count
        .checked_add(expected_delta)
        .ok_or(ProbeError::ArithmeticOverflow)?;
    if final_count != expected {
        return Err(ProbeError::InvariantViolation);
    }
    Ok(())
}

/// Pin the contract instance entry lifetime to the network maximum to guarantee archival resilience.
#[inline(always)]
pub fn extend_instance_lifetime(env: &Env) {
    let max_ttl = env.storage().max_ttl();
    let threshold = LIFETIME_THRESHOLD_LEDGERS.min(max_ttl);
    let bump = BUMP_AMOUNT_LEDGERS.min(max_ttl);
    env.storage().instance().extend_ttl(threshold, bump);
}

/// Extend persistent storage entry lifetime for a keyed probe to guarantee archival resilience.
#[inline(always)]
pub fn extend_persistent_entry(env: &Env, key: &Key) {
    let max_ttl = env.storage().max_ttl();
    let threshold = LIFETIME_THRESHOLD_LEDGERS.min(max_ttl);
    let bump = BUMP_AMOUNT_LEDGERS.min(max_ttl);
    env.storage().persistent().extend_ttl(key, threshold, bump);
}
