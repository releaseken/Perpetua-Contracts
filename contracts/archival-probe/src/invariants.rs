//! # Invariants and Formal Verification for Archival Probe
//!
//! This module documents and provides assertions for formal verification standards,
//! ledger footprint optimization, and Soroban host instruction conservation.
//!
//! ## Core Invariants
//!
//! * **I1 (Single Deserialization & Host Instruction Minimization)**:
//!   Persistent storage entries are deserialized at most once during batch probe or
//!   verification passes. In-memory representations (such as [`ProbeRecord`]) are
//!   held in local contiguous memory rather than repeatedly decoded across loop iterations.
//!
//! * **I2 (Persistence Without Extension)**:
//!   The archival canary entry [`Key::Canary`] must NEVER have its TTL refreshed
//!   or extended by contract operations (`plant`, `read`, or batch checks). This ensures
//!   the entry archives at `min_persistent_ttl` according to network protocol rules.
//!
//! * **I3 (Write-Once / Idempotence)**:
//!   Subsequent calls to `plant` or `batch_plant` must preserve the first-written note
//!   and must not overwrite existing keys or reset archival countdowns.
//!
//! * **I4 (Safe Arithmetic & Bounds)**:
//!   Batch sizes are capped by [`MAX_BATCH_PROBE_SIZE`] to strictly prevent Soroban host CPU
//!   and memory exhaustion. All counter and arithmetic operations use checked arithmetic.

use crate::{ProbeRecord, MAX_BATCH_PROBE_SIZE};
use soroban_sdk::Symbol;

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
