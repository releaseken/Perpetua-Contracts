//! Event definitions for the pause-clock contract.
//!
//! Every state change emits exactly one event. The static topic is derived by
//! the SDK from the struct name in snake_case form. `#[topic]` on a field puts
//! it into the indexed topic vector, allowing server-side filtering.
//!
//! Discriminants and topic layout are ABI. Additive change (a new field
//! appended at the end) is compatible; reordering, removing, or renaming a
//! field is a breaking change.
//!
//! # Topic inventory (must remain unique)
//!
//! | struct name            | topic[0]            |
//! |------------------------|---------------------|
//! | `ClockPaused`          | `clock_paused`      |
//! | `ClockResumed`         | `clock_resumed`     |
//! | `StreamPauseTotaled`   | `stream_pause_totaled` |
//! | `TtlExtended`          | `ttl_extended`      |
//! | `AdminTransferProposed`| `admin_transfer_proposed` |
//! | `AdminTransferred`     | `admin_transferred` |

use soroban_sdk::{contractevent, Address, Env};

/// The global clock was paused.
#[contractevent]
pub struct ClockPaused {
    #[topic]
    pub paused_at: u64,
    pub cycle_count: u64,
}

/// The global clock was resumed. `paused_duration` is the interval just
/// absorbed; `paused_total` is the cumulative figure after this resume.
#[contractevent]
pub struct ClockResumed {
    #[topic]
    pub resumed_at: u64,
    pub paused_duration: u64,
    pub paused_total: u64,
    pub cycle_count: u64,
}

/// A per-stream cumulative paused total was updated (called once per stream
/// per batch-claim settlement).
#[contractevent]
pub struct StreamPauseTotaled {
    #[topic]
    pub stream_id: u64,
    pub paused_total: u64,
}

/// A persistent entry's TTL was extended by the keeper path.
#[contractevent]
pub struct TtlExtended {
    #[topic]
    pub stream_id: u64,
    pub extended_to_ledgers: u32,
}

/// Two-step admin rotation: a successor was nominated.
#[contractevent]
pub struct AdminTransferProposed {
    #[topic]
    pub current: Address,
    #[topic]
    pub pending: Address,
}

/// Two-step admin rotation: the successor accepted and took over.
#[contractevent]
pub struct AdminTransferred {
    #[topic]
    pub previous: Address,
    #[topic]
    pub new: Address,
}

// ---------------------------------------------------------------------------
// Emission helpers
// ---------------------------------------------------------------------------

pub fn clock_paused(env: &Env, paused_at: u64, cycle_count: u64) {
    ClockPaused {
        paused_at,
        cycle_count,
    }
    .publish(env);
}

pub fn clock_resumed(
    env: &Env,
    resumed_at: u64,
    paused_duration: u64,
    paused_total: u64,
    cycle_count: u64,
) {
    ClockResumed {
        resumed_at,
        paused_duration,
        paused_total,
        cycle_count,
    }
    .publish(env);
}

pub fn stream_pause_totaled(env: &Env, stream_id: u64, paused_total: u64) {
    StreamPauseTotaled {
        stream_id,
        paused_total,
    }
    .publish(env);
}

pub fn ttl_extended(env: &Env, stream_id: u64, extended_to_ledgers: u32) {
    TtlExtended {
        stream_id,
        extended_to_ledgers,
    }
    .publish(env);
}
