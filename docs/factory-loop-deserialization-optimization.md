# Factory Subsystem Host CPU Instruction Minimization & Loop Deserialization Audit

## Executive Summary
This document addresses **Issue #359**: *perf(contracts/factory): minimize Soroban host CPU instructions in factory by optimizing loop deserialization*.

The `contracts/factory` subsystem provides parameter-gated creation and policy enforcement for the Perpetua streaming protocol. Because the factory enforces protocol-wide capacity caps, duration constraints, recipient allowlists, token approvals, and per-sender rate-limiting buckets across transactions, eliminating redundant Soroban host deserialization and optimizing iteration loops is vital for predictable, minimal CPU instruction consumption.

---

## Technical Scope & Architecture Analysis

### 1. Soroban Host Metering & Deserialization Dynamics
Under Soroban Protocol 27 metering:
- Each host storage read (`env.storage().instance().get(...)` or `env.storage().persistent().get(...)`) traverses the host storage map, reads raw XDR bytes, and deserializes them into guest memory representations.
- Redundant queries within loops or repeated reads across multiple subroutines in a single transaction incur compounding CPU instruction costs and inflate memory footprint.
- In-memory collection iteration over `soroban_sdk::Vec<T>` or `Map<K, V>` repeatedly calls host functions (`vec_get`, `val_to_address`, etc.) if keys or values are accessed multiple times within loop bodies.

### 2. Implemented Optimizations & Invariants

#### A. Single-Pass Storage & Configuration Hoisting
- **Centralized Policy Loading:** In creation and setter flows, `load_config` / `load_policy` is executed once per transaction context. Invariant checks and validation checks consume the already-deserialized in-memory struct without re-querying the storage subsystem.
- **Environment Context Hoisting:** Dynamic ledger properties such as `env.ledger().sequence()` and `env.ledger().timestamp()` are captured once before conditional branching or bucket adjustments rather than queried per check.

#### B. Direct Key Queries Without Pre-Check Probing
- Eliminates the redundant pattern of invoking `has()` immediately followed by `get()`. Storage lookups utilize single-pass `get()` operations, halving host storage map traversals.
- Dedicated TTL bumps (`bump_ttl` and `bump_allowlist`) are consolidated to run after state mutations or positive matches, avoiding redundant host TTL calculation loops.

#### C. Loop Deserialization Avoidance
- Any batch or multi-entry operations adhere to formal invariant **I7** (Host CPU Instruction Minimization & Loop Deserialization Optimization):
  - Pass compact, scalar keys (`u64`, `Address`) rather than complex nested structs to minimize guest-host argument marshaling.
  - Iterate collections exactly once; cache deserialized values in local variables for subsequent mathematical operations.
  - Arithmetic operations on rates and durations in `math.rs` execute as pure in-memory scalar arithmetic without re-entering the Soroban host runtime.

#### D. Checked Arithmetic & Defensive Error Handling
- All counters (such as `active_streams`, `creations`, and TTL ledger bounds) utilize checked arithmetic (`checked_add`, `checked_sub`, `checked_mul`, `checked_div`).
- Guard against underflow, overflow, and division by zero across all boundary states, mapping failures to typed, stable `FactoryError` codes (`CapacityCapExceeded`, `ActiveStreamCountUnderflow`, `InvalidDuration`, `InvalidDeposit`, `InvariantViolation`).

---

## Verification & Metering Regression Prevention
1. **Host Metering Safety:** Factory operations maintain CPU instruction usage well within the 10% policy-overhead threshold established in `tests/gas_benchmark.rs`.
2. **Formal Invariant Adherence:** Verified against Invariants I1 through I7 in `contracts/factory/src/invariants.rs`.
3. **No Storage Desync:** Ensures all state transitions preserve atomic CEI guarantees with re-entrancy locking.
