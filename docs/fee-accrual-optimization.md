# Fee-Accrual Host CPU & Loop Deserialization Optimization Audit

## Executive Summary
This document addresses **Issue #386**: *perf(contracts/fee-accrual): minimize Soroban host CPU instructions in fee-accrual by optimizing loop deserialization*.

The `contracts/fee-accrual` subsystem handles protocol fee accruals, keeper fee allocations, fee claims, and settlement finalization. In multi-stream / batch accrual and settlement paths (`batch_accrue_fees`, `batch_settle_fees`), performing repeated storage queries or deserializing complex ledger structures across loop iterations heavily consumes Soroban host CPU instructions and inflates ledger footprints.

## Technical Architecture & Optimization Analysis

### 1. Loop Deserialization Bottlenecks in Soroban SDK
Calls to `env.storage().persistent().get(&key)` or `env.storage().instance().get(&key)` invoke host functions that:
1. Traverse the host storage tree.
2. Read raw XDR bytes for the storage entry.
3. Deserialize the XDR payload into guest memory representations (`FeeConfig`, `StreamFeeRecord`, or scalar amounts).

Naive implementations suffer from:
- Redundant `has()` existence checks before calling `get()`, doubling host storage round-trips.
- Repeatedly querying and writing global instance storage keys (such as `TotalProtocolFees` and `TotalKeeperFees`) inside the iteration loop.
- Deserializing full contract configuration repeatedly inside loops.

### 2. Implemented Optimizations

1. **Single-Pass Storage Retrieval:**
   - In `batch_accrue_fees` and `batch_settle_fees`, persistent records (`StreamFeeRecord`) are queried once per stream ID using direct `get()` with `unwrap_or` / default instantiation, cutting host storage lookups in half.
   - Eliminated redundant `has()` checks preceding `get()`.

2. **Loop Deserialization Avoidance:**
   - Global configuration (`FeeConfig`) is loaded and validated exactly once outside the processing loop.
   - Batch input items (`BatchAccrueItem`) are loaded by integer index, extracting flat scalar primitives (`stream_id: u64`, `amount: i128`) into local CPU registers.
   - Duplicate stream IDs are detected via intra-batch scalar index comparison without allocating dynamic heap collections.

3. **Batched State Commit (Deferred Aggregation):**
   - Global aggregates (`TotalProtocolFees`, `TotalKeeperFees`) are not updated inside each loop iteration. Instead, incremental fee deltas are accumulated in local 128-bit memory registers using checked arithmetic (`checked_add`).
   - The cumulative global sums are committed to instance storage in a single atomic update at the completion of the batch.

4. **Checked Arithmetic & Defensive Error Handling:**
   - All arithmetic operations throughout batch execution strictly enforce checked operations (`checked_add`, `checked_mul`, `checked_div`, `checked_sub`), returning typed `FeeError::ArithmeticOverflow` on failure.
   - Strict batch size bounds (`MAX_BATCH_SIZE = 32`) guard against CPU instruction budget exhaustion.
