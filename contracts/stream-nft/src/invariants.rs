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
