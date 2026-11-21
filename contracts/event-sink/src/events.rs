//! Event definitions for the event-sink contract.
//!
//! Every state change and accepted entry emits exactly one structured event.
//! Topics carry indexer-friendly filter keys; data carries the full schema.
//!
//! # Topic inventory (must remain unique)
//!
//! | struct name            | topic[0]                    |
//! |------------------------|-----------------------------|
//! | `EntryAccepted`        | `entry_accepted`            |
//! | `EmitterRegistered`    | `emitter_registered`        |
//! | `EmitterDeregistered`  | `emitter_deregistered`      |
//! | `SinkToggled`          | `sink_toggled`              |
//! | `AdminTransferProposed`| `admin_transfer_proposed`   |
//! | `AdminTransferred`     | `admin_transferred`         |

use soroban_sdk::{contractevent, Address, Bytes, Env, Symbol};

/// An entry was validated and accepted into the sink.
#[contractevent]
pub struct EntryAccepted {
    /// Globally monotonic entry index assigned by the sink.
    #[topic]
    pub entry_id: u64,
    /// The contract that submitted the entry.
    #[topic]
    pub source_contract: Address,
    /// Caller-assigned category.
    #[topic]
    pub topic: Symbol,
    /// Caller-assigned sequence number.
    pub sequence: u64,
    /// The raw payload bytes.
    pub payload: Bytes,
}

/// A new emitter contract was registered (admin operation).
#[contractevent]
pub struct EmitterRegistered {
    #[topic]
    pub emitter: Address,
}

/// An emitter contract was removed from the allow-list (admin operation).
#[contractevent]
pub struct EmitterDeregistered {
    #[topic]
    pub emitter: Address,
}

/// The sink's `accepting` flag was toggled.
#[contractevent]
pub struct SinkToggled {
    pub accepting: bool,
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

pub fn entry_accepted(
    env: &Env,
    entry_id: u64,
    source_contract: Address,
    topic: Symbol,
    sequence: u64,
    payload: Bytes,
) {
    EntryAccepted {
        entry_id,
        source_contract,
        topic,
        sequence,
        payload,
    }
    .publish(env);
}

pub fn emitter_registered(env: &Env, emitter: Address) {
    EmitterRegistered { emitter }.publish(env);
}

pub fn emitter_deregistered(env: &Env, emitter: Address) {
    EmitterDeregistered { emitter }.publish(env);
}

pub fn sink_toggled(env: &Env, accepting: bool) {
    SinkToggled { accepting }.publish(env);
}
