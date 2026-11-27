# Stream-NFT Modular Errors & Cross-Contract Streamlining Audit

## Executive Summary
This document provides technical verification, architecture analysis, and formal audit documentation for:
* **Issue #371**: *refactor(contracts/stream-nft): modularize error types and custom error messages in stream-nft for enhanced client observability*.
* **Issue #373**: *perf(contracts/stream-nft): streamline cross-contract invocation overhead in stream-nft during high-frequency batch claims*.

Together with **Issue #368** (*perf: loop deserialization optimization*), the `contracts/stream-nft` subsystem delivers an enterprise-grade, gas-efficient, and observable SEP-11 NFT wrapper for Perpetua stream recipient claims.

---

## 1. Modular Error Handling Architecture (Issue #371)

### 1.1 Design Rationale
In high-frequency decentralized applications and indexing pipelines, contract error discriminants alone are insufficient for client observability. When transactions fail, SDKs, indexers, and frontend applications must determine:
1. Did the transaction fail because of invalid client input (e.g. empty batch, invalid token ID, unauthorized caller)?
2. Did the transaction fail due to underlying protocol/external contract states (e.g. non-transferable stream, already depleted)?
3. Did the transaction encounter numerical or lifecycle errors?

### 1.2 Modular Taxonomy & Categorization
Errors are decoupled into `contracts/stream-nft/src/errors.rs` and categorized into 6 functional groups:

| Group | Category | Discriminants | Description |
|:---|:---|:---|:---|
| **Group A** | Lifecycle / Init | `AlreadyInitialized (0)`, `NotInitialized (1)` | Contract initialization state invariants |
| **Group B** | Token Entity | `AlreadyMinted (2)`, `TokenNotFound (3)` | NFT token existence and uniqueness |
| **Group C** | Authorization | `NotOwner (4)`, `Unauthorized (11)` | Ownership verification and authorization checks |
| **Group D** | Cross-Contract | `StreamRejected (5)`, `StreamCallFailed (12)` | Invocations against external `FluxoraStream` contracts |
| **Group E** | Batch / Protocol | `EmptyBatch (6)`, `BatchTooLarge (7)`, `DuplicateTokenId (8)`, `MalformedTokenId (9)` | Batch payload bounds and schema validations |
| **Group F** | Arithmetic | `ArithmeticOverflow (10)` | Checked arithmetic bounds enforcement |

### 1.3 Client Observability APIs
The `Error` enum exposes:
* `Error::group() -> char`: Enables client applications to branch on broad failure categories (`'A'` through `'F'`) without matching every single discriminant.
* `Error::message() -> &'static str`: Returns canonical, descriptive human-readable diagnostics for client telemetry and UI toasts.
* `Error::is_cross_contract() -> bool`: Identifies sub-invocation failures.
* `Error::is_client_validation() -> bool`: Identifies bad client input before external interactions.

---

## 2. Cross-Contract Invocation Overhead Streamlining (Issue #373)

### 2.1 The Cross-Contract Overhead Problem in Batch Claims
In Soroban smart contracts, invoking an external contract via `env.invoke_contract` incurs substantial host VM metering overhead:
- Call frame initialization and stack allocation.
- Host parameter serialization into XDR and deserialization in the target contract.
- Authentication context forwarding.
- Sub-invocation metering base fee.

When performing high-frequency batch transfers / claims of $N$ streams ($1 \le N \le 16$), invoking `transfer_recipient` sequentially $N$ times executes $N$ discrete cross-contract call frames. For $N = 16$, this wastes host CPU gas and approaches call depth and instruction ceilings.

### 2.2 Streamlined Coalesced Invocations
`StreamNft` introduces `transfer_claims_batch`:
```rust
fn transfer_claims_batch(
    env: &Env,
    config: &Config,
    stream_ids: &Vec<u64>,
    to: &Address,
) -> Result<(), Error>
```
1. **Primary Coalesced Path (`batch_transfer_recipient`):**
   - The contract calls `FluxoraStream::batch_transfer_recipient(stream_ids, to)` in a single sub-invocation.
   - For a batch of size 16, cross-contract overhead collapses from **16 external invocations to 1**, achieving an **approximate 93% reduction in cross-contract call overhead**.
2. **Backward-Compatible Fallback Path (`transfer_recipient`):**
   - If the target contract does not export `batch_transfer_recipient` (e.g. interacting with an unupgraded stream instance), the helper catches the call rejection and falls back to sequential invocations.
   - Transaction atomicity is strictly preserved: if any single stream rejects, the transaction aborts with `Error::StreamRejected`, rolling back all staged storage changes.

---

## 3. Formal Invariant Verification Suite

The invariant verification suite in `contracts/stream-nft/src/invariants.rs` and unit test suite in `contracts/stream-nft/src/test.rs` formally test:
1. **I1 (Single-Pass Storage Deserialization & Bound Enforcement):** Bounded by `0 < N <= MAX_BATCH_SIZE (16)`.
2. **I2 (Idempotency and Uniqueness):** Duplicate token rejection upfront.
3. **I3 (Supply and Solvency Conservation):** `verify_supply_increment` and `verify_transfer_balance_conservation`.
4. **I4 (Zero Storage Leakage on Zero Balance):** Removal of empty balance keys.
5. **I5 (Atomic Rollback on Stream Rejection):** Complete rollback on external claim rejection.
6. **I6 (Cross-Contract Sub-Invocation Coalescing):** `verify_cross_contract_coalescing(batch_supported, batch_size, actual_calls)` verifies $O(1)$ sub-invocation overhead.
7. **I7 (Deterministic Modular Error Categorization):** Exhaustive test of all error codes, group discriminants, and client messages.
