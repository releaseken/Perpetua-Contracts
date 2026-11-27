# Stream-NFT Host CPU & Loop Deserialization Optimization Audit

## Executive Summary
This document addresses **Issue #368**: *perf(contracts/stream-nft): minimize Soroban host CPU instructions in stream-nft by optimizing loop deserialization*.

The `contracts/stream-nft` subsystem provides a lightweight SEP-11 non-fungible wrapper around Perpetua stream recipient claims, associating token IDs (`stream_id`) with on-chain NFT ownership and orchestrating claim reassignment via cross-contract calls to `FluxoraStream::transfer_recipient`. In multi-token queries and batch operations, repeatedly reading and deserializing storage entries across loop iterations incurs substantial Soroban host CPU instruction overhead and inflates ledger footprints.

## Technical Architecture & Optimization Analysis

### 1. Loop Deserialization Bottlenecks in Soroban SDK
In Soroban smart contracts, calls to `env.storage().persistent().get(&key)` or `env.storage().instance().get(&key)` trigger host function calls that:
1. Locate the entry in the host storage map.
2. Read raw XDR bytes for the storage entry.
3. Deserialize the XDR payload into guest memory representations (`Address`, `u32`, `u64`, or composite structures).

When multi-token queries or batch operations are executed sequentially in loops:
- Redundant calls to `has(&key)` followed by `get(&key)` incur duplicate host storage traversals.
- Repeated deserialization of identical account balances (e.g., in a batch mint where the caller's balance is incremented N times, or a batch transfer where the sender and recipient balances are mutated N times) causes unnecessary host CPU instruction consumption and creates redundant host entries in the transaction footprint.

### 2. Implemented Optimizations in `contracts/stream-nft`

1. **Batch Query Optimization (`batch_owner_of`):**
   - Implemented `batch_owner_of(stream_ids)` allowing off-chain clients and indexers to inspect multiple stream NFT owners in a single contract invocation.
   - Bounded by `MAX_BATCH_SIZE = 16` to strictly guarantee CPU execution remains within Soroban metering headroom limits.
   - Elements are deserialized and queried in a single linear pass; live entries are refreshed (`bump`) immediately without redundant round-trips.

2. **Coalesced Balance Deserialization & Mutations (`batch_mint`, `batch_transfer`):**
   - **Preflight Validation Pass:** In `batch_mint`, input stream IDs are validated and checked against `DataKey::Owner(stream_id)` in a single pass to confirm unminted status before performing any mutations.
   - **Single Balance Deserialization & Write:** Rather than reading, deserializing, incrementing, and writing `DataKey::Balance(caller)` $N$ times for a batch of size $N$, `batch_mint` performs a single `.get()`, adds $N$ with checked arithmetic, and performs a single `.set()`. This eliminates $2(N - 1)$ host storage operations.
   - In `batch_transfer`, the sender's balance is decremented by $N$ and the recipient's balance is incremented by $N$ in a single coalesced update.

3. **Ledger Footprint Pruning (Zero-Balance Storage Cleanup):**
   - When a token transfer or batch transfer reduces an account's balance to 0, `env.storage().persistent().remove(&DataKey::Balance(owner))` is invoked rather than storing `0`.
   - This prevents state bloat on the ledger, lowers the transaction write footprint, and minimizes future deserialization costs.

4. **Checked Arithmetic & Defensive Error Handling:**
   - All balance, supply, and count calculations strictly employ checked arithmetic (`checked_add`, `checked_sub`), returning explicit error discriminants on overflow:
     - `AlreadyInitialized = 0`
     - `NotInitialized = 1`
     - `AlreadyMinted = 2`
     - `TokenNotFound = 3`
     - `NotOwner = 4`
     - `StreamRejected = 5`
     - `EmptyBatch = 6`
     - `BatchTooLarge = 7`
     - `DuplicateTokenId = 8`
     - `MalformedTokenId = 9`
     - `ArithmeticOverflow = 10`

5. **Formal Verification Standards & Invariant Module:**
   - Added `contracts/stream-nft/src/invariants.rs` providing formal definitions and verification functions for:
     - **I1:** Single-pass storage deserialization & bound enforcement ($0 < N \le \text{MAX\_BATCH\_SIZE}$).
     - **I2:** Idempotency & uniqueness (duplicate detection upfront).
     - **I3:** Total supply and balance solvency conservation across mints and transfers.
     - **I4:** Zero storage leakage on zero balance.
     - **I5:** Atomic rollback on stream contract rejection.
