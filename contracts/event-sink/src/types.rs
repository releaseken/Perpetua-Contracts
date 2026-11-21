use soroban_sdk::{contracttype, Address, Bytes, Symbol};

/// Storage keys for the event-sink contract.
///
/// `Config` and `PendingAdmin` live in instance storage.
/// `EntryCount` is an instance-storage monotonic counter.
/// There are no persistent per-entry storage keys: entries are emitted as
/// ledger events, not stored on-chain, keeping the footprint minimal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Instance storage: main configuration record.
    Config,
    /// Instance storage: proposed next admin for two-step rotation.
    PendingAdmin,
    /// Instance storage: monotonic count of accepted entries.
    EntryCount,
}

/// On-chain configuration record for the event-sink.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SinkConfig {
    /// Address authorised to call admin-gated operations.
    pub admin: Address,
    /// Whether new `emit` calls are accepted.  When `false`, all `emit` and
    /// `batch_emit` calls return [`SinkError::Unauthorized`].
    pub accepting: bool,
}

/// A single structured audit-log entry passed to `emit`.
///
/// # Fields
///
/// | Field | Constraint | Purpose |
/// |-------|-----------|---------|
/// | `source_contract` | non-zero address | identifies the calling contract |
/// | `topic` | non-empty Symbol | machine-readable event category |
/// | `sequence` | > 0 | caller-assigned monotonic counter for gap detection |
/// | `payload` | ≤ `MAX_PAYLOAD_BYTES` | arbitrary encoded data |
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SinkEntry {
    /// The contract that is reporting this event.
    pub source_contract: Address,
    /// Short machine-readable category string (e.g. `"stream_created"`).
    pub topic: Symbol,
    /// Caller-assigned sequence number, must be > 0.
    pub sequence: u64,
    /// Arbitrary encoded payload bytes, length ≤ `MAX_PAYLOAD_BYTES`.
    pub payload: Bytes,
}
