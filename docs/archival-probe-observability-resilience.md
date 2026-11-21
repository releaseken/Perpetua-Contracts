# Archival Probe Observability & Automated Lifetime Extension Audit

## Executive Summary
This document addresses:
- **Issue #380**: *refactor(contracts/archival-probe): modularize error types and custom error messages in archival-probe for enhanced client observability*.
- **Issue #381**: *feat(contracts/archival-probe): implement automated entry lifetime extension for archival-probe to guarantee archival resilience*.

The `contracts/archival-probe` subsystem serves as a verification mechanism to test Soroban persistent storage archival dynamics against real network ledgers without artificial TTL refresh. In multi-entry / batch verification and probe scenarios, rigorous error taxonomy and automated entry lifetime extensions guarantee host metering safety, ledger footprint optimization, and archival resilience.

---

## 1. Modular Error Architecture & Client Observability (Issue #380)

Failure modes in `archival-probe` are modeled as typed, ABI-stable Soroban contract errors with canonical human-readable diagnostics and group-based categorization.

### Error Taxonomy & Categories

| Group | Group Code | Category | Discriminants | Description |
|-------|------------|----------|---------------|-------------|
| **A** | `'A'` | Lifecycle & Availability | 1, 9, 10 | Archival status and entry lifecycle |
| **B** | `'B'` | Batch & Validation | 2, 3, 4 | Batch constraints, size limits, duplicate inputs |
| **C** | `'C'` | Arithmetic & Metering | 5 | Safe arithmetic guards and overflow protection |
| **D** | `'D'` | Storage & Integrity | 6, 7 | Partition corruption and invariant verification |
| **E** | `'E'` | Re-entrancy & Concurrency | 8 | Execution lock and cross-call concurrency |

### Discriminant Stability & Diagnostics
1. **ABI Stability:** All numeric discriminants (1..=10) are frozen and mapped to explicit `u32` enum representations.
2. **On-Chain & Off-Chain Diagnostics:**
   - Contract views `error_message(discriminant: u32) -> String` and `error_group(discriminant: u32) -> u32` allow client SDKs and indexers to introspect failure modes dynamically without hardcoding contract strings.
   - Comprehensive descriptions facilitate actionable debugging (e.g., distinguishing between "not planted" and "archived needing restoration").

---

## 2. Automated Entry Lifetime Extension & Archival Resilience (Issue #381)

In Soroban, persistent entries have bounded lifetimes (`TTL`). If un-extended, persistent entries expire and archive at `min_persistent_ttl` (~7 days).

### Lifecycle Extension Strategy
1. **Canary Isolation (Preservation of Test Dynamics):**
   - The primary test canary (`Key::Canary`) intentionally **omits** TTL extension on `plant` and `read`. This preserves the core purpose of the probe contract: allowing the entry to archive naturally on the network's schedule to test restoration flows.
2. **Instance State Resilience:**
   - Operational metadata in instance storage—specifically probe counters (`Key::ProbeCount`) and execution re-entrancy locks (`Key::Executing`)—must not expire while probe experiments run.
   - Mutating operations (`plant`, `plant_probe`, `batch_plant`, `extend_probe_lifetime`, `batch_extend_probe_lifetime`) automatically trigger `invariants::extend_instance_lifetime(&env)`, pinning instance TTL to `max_ttl()`.
3. **Selective Persistent Entry Refresh:**
   - Long-running multi-partition probe records can be refreshed via permissionless keeper functions:
     - `extend_probe_lifetime(env: Env, id: u32) -> Result<(), ProbeError>`
     - `batch_extend_probe_lifetime(env: Env, ids: Vec<u32>) -> Result<u32, ProbeError>`
   - Bumps entry TTL to `BUMP_AMOUNT_LEDGERS` (~30 days) whenever remaining TTL falls below `LIFETIME_THRESHOLD_LEDGERS` (~7 days).

---

## 3. Formal Invariants & Verification

The subsystem adheres to the following formal invariants:
- **I1 (Single Deserialization & Host Instruction Minimization)**: Persistent storage entries are deserialized at most once during batch execution passes.
- **I2 (Persistence Without Extension)**: `Key::Canary` never has its persistent TTL refreshed during plant or read, ensuring early archival.
- **I3 (Automated Instance & Entry Lifetime Resilience)**: Contract instance state and designated probe entries are resiliently refreshed against archival limits.
- **I4 (Write-Once / Idempotence)**: First-write-wins semantics prevent re-plant race conditions or countdown resets.
- **I5 (Safe Arithmetic & Bounds)**: All batch operations enforce `MAX_BATCH_PROBE_SIZE = 32` and use `checked_add` arithmetic.
- **I6 (Re-entrancy Immunity and CEI)**: All mutating paths acquire and release the execution lock `Key::Executing`.
- **I7 (State Conservation and Desync Resistance)**: Post-condition assertions verify that `probe_count` increments by exactly the newly planted delta.
- **I8 (Error Observability & Categorization)**: Stable discriminants provide rich diagnostic categorization.

---

## 4. Test Suite Coverage

The unit and fuzz test suite in `contracts/archival-probe/src/test.rs` provides full coverage:
- **`instance_lifetime_is_automatically_extended_on_mutations`**: Validates automated instance TTL bumping across mutations.
- **`extend_probe_lifetime_refreshes_persistent_entry`**: Verifies TTL refreshing on specific probe entries and validates `NotPlanted` handling.
- **`batch_extend_probe_lifetime_sweeps_resilient_entries`**: Verifies multi-entry sweep efficiency, graceful skipping of unplanted IDs, empty-batch handling, and bounds checking.
- **`error_messages_and_group_taxonomies`**: Asserts complete coverage and correctness of error messages and categorical groups across all variants.
- **Proptest fuzzing**: Validates discriminant decoding and state sync invariant assertions against arbitrary inputs.
