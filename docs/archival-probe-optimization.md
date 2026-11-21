# Archival Probe Host CPU & Loop Deserialization Optimization Audit

## Executive Summary
This document addresses **Issue #377**: *perf(contracts/archival-probe): minimize Soroban host CPU instructions in archival-probe by optimizing loop deserialization*.

The `contracts/archival-probe` subsystem proves the Soroban archival/restore flow against live network ledgers by deliberately not extending TTL on persistent entries. In multi-entry / batch verification and probe scenarios, repeatedly querying persistent storage across loop iterations re-enters the Soroban host deserialization pipeline, incurring substantial CPU instruction overhead and ledger read footprint costs.

## Technical Architecture & Optimization Analysis

### 1. Loop Deserialization Bottlenecks in Soroban SDK
In Soroban contracts, calls to `env.storage().persistent().get(&key)` invoke host functions that:
1. Traverse the host storage map.
2. Read raw XDR bytes for the storage entry.
3. Deserialize the XDR payload into guest memory representations (`Symbol`, `Val`, or struct types).

When multiple entries or checks are performed in a loop, naive implementations perform redundant `has()` followed by `get()`, or re-query identical storage keys across loop passes.

### 2. Implemented Optimizations
1. **Single-Pass Storage Retrieval:**
   - In `batch_read` and multi-probe operations, entries are retrieved and deserialized exactly once into in-memory structures ([`ProbeRecord`]).
   - Eliminated redundant `has()` checks preceding `get()`, using `get()` directly with `.is_some()` to cut host storage lookups in half.

2. **Loop Deserialization Avoidance:**
   - Verification loops consume already-deserialized `ProbeRecord` items rather than querying host storage per iteration.
   - Batch operations strictly enforce `MAX_BATCH_PROBE_SIZE = 32`, guarding against CPU budget exhaustion.

3. **Checked Arithmetic & Defensive Error Handling:**
   - All loop counting and index calculations utilize checked arithmetic (`checked_add`), preventing underflow/overflow under boundary conditions.
   - Comprehensive error taxonomy with stable discriminants: `NotPlanted = 1`, `EmptyBatch = 2`, `BatchTooLarge = 3`, `DuplicateEntry = 4`, `ArithmeticOverflow = 5`.

4. **Preservation of Archival Guarantees:**
   - Strict verification that no TTL extension (`extend_ttl`) is introduced into probe execution paths, preserving early archival at `min_persistent_ttl`.
