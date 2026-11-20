//! Event definitions and emission for the dispute-window contract.
//!
//! # Contract
//!
//! * Every state change emits exactly one event.
//! * Events use `#[contractevent]`, so schemas appear in the contract's
//!   interface spec.  Tooling generates typed decoders from that spec.
//! * `topic[0]` is the struct name in `snake_case` (SDK-derived).
//! * `stream_id` is always a topic so indexers can filter by stream.
//! * Addresses that an indexer routes on are marked `#[topic]`.
//!
//! # Additive versioning rule
//!
//! Appending a field at the end of a struct is a compatible change.
//! Reordering, removing, or re-topicking a field, or renaming the struct,
//! is an **incompatible breaking change** that requires a `V2` struct.
//!
//! # Namespace uniqueness
//!
//! No two structs may share the same `snake_case` derivation.  The uniqueness
//! of `EVENT_TOPIC_NAMES` is enforced by
//! `test::events::test_all_event_topic_names_are_unique`.

use soroban_sdk::{contractevent, Address, Env, String};

use crate::types::ClaimStatus;

/// Canonical inventory of every event's `topic[0]` namespace symbol.
///
/// **Invariant**: every entry is unique.  Enforced by the events test.
pub const EVENT_TOPIC_NAMES: [&str; 7] = [
    "window_opened",
    "window_closed",
    "window_finalised",
    "claim_submitted",
    "claim_decided",
    "claim_settled",
    "claim_ttl_extended",
];

// ---------------------------------------------------------------------------
// Event structs
// ---------------------------------------------------------------------------

/// The dispute window was opened and is now accepting claims.
#[contractevent]
pub struct WindowOpened {
    #[topic]
    pub admin: Address,
    /// Ledger timestamp at which the window opened.
    pub open_time: u64,
    /// Ledger timestamp after which no new claims are accepted.
    pub deadline: u64,
}

/// The dispute window was closed (by the admin or deadline passage).
#[contractevent]
pub struct WindowClosed {
    #[topic]
    pub admin: Address,
    pub closed_at: u64,
}

/// The arbiter finalised the window; no further claim decisions are possible.
#[contractevent]
pub struct WindowFinalised {
    #[topic]
    pub arbiter: Address,
    pub finalised_at: u64,
}

/// A claimant submitted a claim against a stream.
#[contractevent]
pub struct ClaimSubmitted {
    #[topic]
    pub stream_id: u64,
    #[topic]
    pub claimant: Address,
    pub claimed_amount: i128,
    pub submitted_at: u64,
}

/// The arbiter approved or denied a claim.
#[contractevent]
pub struct ClaimDecided {
    #[topic]
    pub stream_id: u64,
    #[topic]
    pub arbiter: Address,
    pub status: ClaimStatus,
    pub approved_amount: i128,
    /// Free-text note from the arbiter; `None` when no note was provided.
    pub note: Option<String>,
}

/// The claimant settled an approved claim (funds transferred).
#[contractevent]
pub struct ClaimSettled {
    #[topic]
    pub stream_id: u64,
    #[topic]
    pub claimant: Address,
    pub amount: i128,
}

/// A keeper extended a claim entry's TTL.
#[contractevent]
pub struct ClaimTtlExtended {
    #[topic]
    pub stream_id: u64,
    pub extended_to_ledgers: u32,
}

// ---------------------------------------------------------------------------
// Emission helpers
// ---------------------------------------------------------------------------

pub fn window_opened(env: &Env, admin: &Address, open_time: u64, deadline: u64) {
    WindowOpened {
        admin: admin.clone(),
        open_time,
        deadline,
    }
    .publish(env);
}

pub fn window_closed(env: &Env, admin: &Address, closed_at: u64) {
    WindowClosed {
        admin: admin.clone(),
        closed_at,
    }
    .publish(env);
}

pub fn window_finalised(env: &Env, arbiter: &Address, finalised_at: u64) {
    WindowFinalised {
        arbiter: arbiter.clone(),
        finalised_at,
    }
    .publish(env);
}

pub fn claim_submitted(
    env: &Env,
    stream_id: u64,
    claimant: &Address,
    claimed_amount: i128,
    submitted_at: u64,
) {
    ClaimSubmitted {
        stream_id,
        claimant: claimant.clone(),
        claimed_amount,
        submitted_at,
    }
    .publish(env);
}

pub fn claim_decided(
    env: &Env,
    stream_id: u64,
    arbiter: &Address,
    status: ClaimStatus,
    approved_amount: i128,
    note: Option<soroban_sdk::String>,
) {
    ClaimDecided {
        stream_id,
        arbiter: arbiter.clone(),
        status,
        approved_amount,
        note,
    }
    .publish(env);
}

pub fn claim_settled(env: &Env, stream_id: u64, claimant: &Address, amount: i128) {
    if amount == 0 {
        return;
    }
    ClaimSettled {
        stream_id,
        claimant: claimant.clone(),
        amount,
    }
    .publish(env);
}

pub fn claim_ttl_extended(env: &Env, stream_id: u64, extended_to_ledgers: u32) {
    ClaimTtlExtended {
        stream_id,
        extended_to_ledgers,
    }
    .publish(env);
}
