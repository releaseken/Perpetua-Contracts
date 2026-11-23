# Archival Probe Invariant Assertions, Re-entrancy Protection & State Desync Audit

## Executive Summary
This document addresses **Issue #379**: *security(contracts/archival-probe): enforce strict invariant assertions in archival-probe to protect against re-entrancy and state desync*.

The `contracts/archival-probe` subsystem serves as a verification mechanism to test Soroban persistent storage archival dynamics against real network ledgers without artificial TTL refresh. In multi-entry / batch verification and probe scenarios, rigorous invariant assertions are required to protect against re-entrant calls, race conditions, state desynchronization, and silent data corruption while adhering to Soroban SDK best practices, ledger footprint optimization, and formal verification standards.

---

## Technical Scope & Architecture

### 1. Re-entrancy Protection Mechanism
Soroban contract calls can interact across contract boundaries. To enforce absolute mutual exclusion on state-modifying operations:
- An execution lock (`Key::Executing`) is managed in instance storage.
- At the start of mutating dispatches (`plant`, `plant_probe`, `batch_plant`), `invariants::acquire_executing(&env)` checks if the lock is held. If so, execution immediately aborts with `ProbeError::ReentrancyGuard = 8` (Group E).
- The lock is cleared unconditionally upon completion of the dispatch via `invariants::release_executing(&env)`.
- If validation fails prior to execution, the lock is never leaked.

### 2. State Desynchronization & Conservation Invariants
To prevent state desynchronization across batch probe operations, network partitions, or delayed replays:
- A global probe accounting counter (`Key::ProbeCount`) tracks the total number of uniquely planted probe entries.
- In both single-item and batch registration paths, the contract records `count_before = load_probe_count(env)`.
- During execution, only newly planted keys increment `planted_count`.
- Post-condition invariant assertion `invariants::assert_state_sync(count_before, count_final, planted_count)` strictly validates that:
  $$\text{count}_{\text{final}} == \text{count}_{\text{before}} + \text{planted\_delta}$$
  Any arithmetic overflow returns `ProbeError::ArithmeticOverflow = 5`.
  Any divergence between actual and expected count immediately aborts the transaction with `ProbeError::InvariantViolation = 7`.

### 3. Formal Invariants & Security Taxonomy
The archival-probe formal invariant suite enforces:
- **I1 (Single Deserialization & Host Instruction Minimization)**: Persistent storage entries are deserialized at most once during batch execution passes.
- **I2 (Persistence Without Extension)**: Neither `plant`, `read`, nor batch probe operations ever invoke `extend_ttl`, guaranteeing that persistent entries archive precisely at network `min_persistent_ttl`.
- **I3 (Write-Once / Idempotence)**: Duplicate submissions preserve the initial probe record without resetting the sequence timestamp or TTL countdown.
- **I4 (Safe Arithmetic & Bounds)**: Batch sizes are strictly capped by `MAX_BATCH_PROBE_SIZE = 32`, and all arithmetic paths utilize checked arithmetic.
- **I5 (Re-entrancy Immunity and CEI)**: All state changes precede external effects, and concurrent re-entrant calls are rejected with `ReentrancyGuard`.
- **I6 (State Conservation and Desync Resistance)**: The global probe counter strictly mirrors storage cardinality; any delta discrepancy triggers `InvariantViolation`.
- **I7 (Error Observability & Categorization)**: All failure conditions map to ABI-stable discriminants partitioned into distinct semantic domains ('A' through 'E').

### 4. Verification and Fuzz Testing
The test suite in `contracts/archival-probe/src/test.rs` validates:
1. Re-entrancy guard acquisition, rejection of concurrent/re-entrant calls, and reliable lock release.
2. Exact state synchronization across single, batch, and duplicate replay operations.
3. Negative tests proving `assert_state_sync` rejects desynchronized states.
4. Property-based fuzz tests verifying invariant assertions across random initial counters, batch deltas, and state skews.
