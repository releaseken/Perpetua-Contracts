//! # Invariants and Formal Verification for Archival Probe
//!
//! This module documents and provides assertions for formal verification standards,
//! ledger footprint optimization, and Soroban host instruction conservation during
//! probe operations and fractional stream withdrawal calculations.
//!
//! ## Core Invariants
//!
//! * **I1 (Single Deserialization & Host Instruction Minimization)**:
//!   Persistent storage entries are deserialized at most once during batch probe or
//!   verification passes. In-memory representations (such as [`ProbeRecord`]) are
//!   held in local contiguous memory rather than repeatedly decoded across loop iterations.
//!
//! * **I2 (Persistence Without Extension)**:
//!   The archival canary entry [`Key::Canary`], partition probe entries [`Key::Probe`],
//!   and batch claim probe entries [`Key::ClaimProbe`] must NEVER have their TTL refreshed
//!   or extended by contract operations (`plant`, `read`, `batch_plant`, `batch_read`,
//!   `batch_claim_probe`, or `batch_get_claim_probes`). This ensures entries archive at
//!   `min_persistent_ttl` according to network protocol rules.
//!
//! * **I3 (Write-Once / Idempotence)**:
//!   Subsequent calls to `plant`, `batch_plant`, or `batch_claim_probe` must preserve
//!   the first-written note or claim record and must not overwrite existing keys or
//!   reset archival countdowns.
//!
//! * **I4 (Safe Arithmetic & Bounds)**:
//!   Batch sizes are capped by [`MAX_BATCH_PROBE_SIZE`] (or dynamic config bound) to
//!   strictly prevent Soroban host CPU and memory exhaustion. All counter and arithmetic
//!   operations use checked arithmetic.
//!
//! * **I5 (Cross-Contract Invocation Overhead Reduction)**:
//!   Batch processing operations (`batch_plant`, `batch_read`, `batch_claim_probe`,
//!   `batch_get_claim_probes`) consolidate multi-key probes into single-pass execution frames,
//!   eliminating repetitive cross-contract call overhead during high-frequency claims.
//!
//! * **I6 (Fractional Withdrawal Conservation Identity)**:
//!   For any fractional withdrawal split of amount $T$ with fraction $N/D$, the payout $P$
//!   and remaining balance $R$ strictly satisfy:
//!   $$P + R == T$$
//!   where $0 \le P \le T$, $0 \le R \le T$, and no intermediate integer overflow occurs
//!   for any $T \le \text{i128::MAX}$.
//!
//! * **I7 (Granular Role-Based Authorization)**:
//!   Configuration updates are partitioned into granular roles (`Admin`, `ConfigAdmin`,
//!   `EmergencyGuardian`, `Operator`). Administrative actions are strictly isolated and
//!   protected against unauthorized updates. Addresses cannot be zero or contract self.

use crate::math::FractionalSplit;
use crate::{ClaimProbeRecord, ProbeRecord, Role, MAX_BATCH_PROBE_SIZE};
use soroban_sdk::{Address, Symbol};

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

/// Assert that the claim probe record preserves original stream claim data across checks.
#[inline(always)]
pub fn assert_claim_probe_integrity(
    record: &ClaimProbeRecord,
    expected_stream_id: u64,
    expected_amount: i128,
    expected_claimant: &Address,
) -> bool {
    record.stream_id == expected_stream_id
        && record.amount == expected_amount
        && record.claimant == *expected_claimant
}

/// Formally assert the fractional withdrawal conservation invariant:
/// `split.payout + split.remainder == split.total`.
#[inline(always)]
pub fn assert_fractional_conservation(split: &FractionalSplit) -> bool {
    split.assert_conservation()
}

/// Check role partition validity.
#[inline(always)]
pub fn is_valid_role(role: Role) -> bool {
    matches!(
        role,
        Role::Admin | Role::ConfigAdmin | Role::EmergencyGuardian | Role::Operator
    )
}
