//! Invariants and formal safety definitions for `contracts/stream-nft`.
//!
//! # Protocol Invariants
//!
//! * **I1 (Conservation of Total Supply)**: `total_supply == sum(balance_of(owner))` across all active holders.
//! * **I2 (Token Uniqueness and Ownership Coherence)**: A token `stream_id` exists if and only if
//!   `owner_of(stream_id)` is `Some(addr)`. In that case, `balance_of(addr) >= 1`.
//! * **I3 (No-Dangling Balances)**: If `balance_of(addr) > 0`, there exist non-zero tokens owned by `addr`.
//!   When an address transfers its last token, its balance entry is cleaned up to preserve storage footprint.
//! * **I4 (Monotonic Supply Bounds)**: `total_supply <= u64::MAX`, incrementing exactly once per unique `mint`.
//! * **I5 (Re-entrancy Immunity and CEI)**: In-flight execution flags prevent re-entrancy and state desync
//!   during cross-contract calls into the stream contract (`transfer_recipient`).
//!
//! # Invariants and Formal Verification Standards for Stream-NFT
//!
//! This module documents and provides formal verification specifications and invariants
//! for the `stream-nft` subsystem, with particular focus on Soroban host CPU instruction
//! minimization, footprint conservation, modular error categorization, and cross-contract
//! invocation overhead reduction during high-frequency batch claims (#368, #371, #373).
//!
//! ## Core Invariants
//!
//! * **I1 (Single-Pass Storage Deserialization & Bound Enforcement)**:
//!   Persistent storage entries are deserialized at most once during batch operations
//!   (`batch_owner_of`, `batch_mint`, `batch_transfer`). Intermediate validations operate
//!   on in-memory structures or scalar identifiers rather than re-querying the host storage
//!   engine. All batch operations strictly enforce `0 < count <= MAX_BATCH_SIZE`.
//!
//! * **I2 (Idempotency and Uniqueness)**:
//!   Batch inputs reject duplicate stream/token IDs in a single validation pass prior to
//!   any storage mutations. A stream token cannot be minted more than once (`AlreadyMinted`).
//!
//! * **I3 (Supply and Balance Solvency Conservation)**:
//!   `total_supply` strictly tracks the number of distinct minted NFTs. For any mint
//!   or transfer, owner balances are updated atomically with checked arithmetic.
//!   Underflow and overflow conditions are precluded by checked arithmetic (`checked_add`, `checked_sub`).
//!
//! * **I4 (Zero Storage Leakage on Zero Balance)**:
//!   When an owner's balance transitions to 0 following a transfer, the corresponding
//!   `DataKey::Balance(owner)` key is removed from persistent storage rather than storing 0,
//!   minimizing ledger storage footprint and future deserialization costs.
//!
//! * **I5 (Atomic Rollback on Stream Contract Rejection)**:
//!   Any failure during underlying stream cross-contract invocation (`transfer_recipient` or
//!   `batch_transfer_recipient`) or validation failure rolls back the entire batch transaction,
//!   preserving strict consistency between stream state and NFT ownership records.
//!
//! * **I6 (Cross-Contract Sub-Invocation Coalescing)**:
//!   During batch claims / transfers, when the target stream contract supports batch recipient
//!   transfers, external invocation count is $O(1)$ rather than $O(N)$, minimizing host call frame
//!   overhead and host CPU gas metering consumption (#373).
//!
//! * **I7 (Deterministic Modular Error Categorization)**:
//!   All contract errors map to a valid functional category group ('A' through 'F') and provide
//!   non-empty descriptive diagnostic messages for client observability (#371).

use crate::MAX_BATCH_SIZE;

/// Formally check that a batch size satisfies boundary invariants.
#[inline(always)]
pub fn verify_batch_bounds(size: u32) -> bool {
    size > 0 && size <= MAX_BATCH_SIZE
}

/// Formally assert supply conservation invariant after a mint operation.
#[inline(always)]
pub fn verify_supply_increment(old_supply: u64, new_supply: u64, minted_count: u64) -> bool {
    match old_supply.checked_add(minted_count) {
        Some(expected) => expected == new_supply,
        None => false,
    }
}

/// Formally assert balance conservation across a transfer between `from` and `to`.
#[inline(always)]
pub fn verify_transfer_balance_conservation(
    from_before: u32,
    from_after: u32,
    to_before: u32,
    to_after: u32,
    count: u32,
) -> bool {
    let from_ok = match from_before.checked_sub(count) {
        Some(res) => res == from_after,
        None => false,
    };
    let to_ok = match to_before.checked_add(count) {
        Some(res) => res == to_after,
        None => false,
    };
    from_ok && to_ok
}

/// Formally assert cross-contract call reduction: coalesced calls must equal 1 when supported.
#[inline(always)]
pub fn verify_cross_contract_coalescing(
    batch_supported: bool,
    batch_size: u32,
    actual_calls: u32,
) -> bool {
    if batch_supported {
        actual_calls == 1
    } else {
        actual_calls == batch_size
    }
}
