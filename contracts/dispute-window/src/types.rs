use soroban_sdk::{contracttype, Address};

// ---------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------

/// Lifecycle state of a [`Claim`] entry.
///
/// Discriminants are part of the XDR-serialised ABI.  Append new variants at
/// the end; never renumber or remove existing ones.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClaimStatus {
    /// Submitted by the claimant; not yet reviewed by the arbiter.
    Pending = 0,
    /// The arbiter approved the claim; the claimant may withdraw.
    Approved = 1,
    /// The arbiter denied the claim; no withdrawal is possible.
    Denied = 2,
    /// The claimant withdrew the approved amount; terminal.
    Settled = 3,
}

/// Lifecycle state of the dispute window itself.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WindowStatus {
    /// `init` was called but `open` has not yet been called.
    Pending = 0,
    /// The window is open and accepting claims.
    Open = 1,
    /// The deadline has passed or `close` was called by the admin.
    Closed = 2,
    /// The arbiter has finalised all decisions; no further changes allowed.
    Finalised = 3,
}

// ---------------------------------------------------------------------------
// Persistent record types
// ---------------------------------------------------------------------------

/// A single claim lodged against a stream inside the dispute window.
///
/// Stored as a persistent entry keyed by `DataKey::Claim(window_id, stream_id)`.
/// Every field except `status` and `arbiter_note` is set at claim submission and
/// is never mutated by later operations; only the arbiter's decision columns
/// change after submission.
#[contracttype]
#[derive(Clone, Debug)]
pub struct Claim {
    /// The claimant (expected to match the stream's recipient).
    pub claimant: Address,
    /// The stream being claimed against.
    pub stream_id: u64,
    /// Amount the claimant asserts is withdrawable.  Must be positive.
    pub claimed_amount: i128,
    /// Ledger timestamp at which the claim was submitted.
    pub submitted_at: u64,
    /// Current lifecycle state.
    pub status: ClaimStatus,
    /// Optional free-text note written by the arbiter when approving or
    /// denying.  `None` until the arbiter acts.  Stored on-chain for
    /// observability and indexer parity.
    pub arbiter_note: Option<soroban_sdk::String>,
    /// Amount the arbiter approved.  Zero until approved; may differ from
    /// `claimed_amount` if the arbiter partially approves.
    pub approved_amount: i128,
}

/// Per-window configuration record.
///
/// Written atomically by `init`/`open` and read by every entry-point guard.
/// Never partially updated: each setter rewrites the entire struct.
#[contracttype]
#[derive(Clone, Debug)]
pub struct WindowConfig {
    /// Admin address: the only party that may open/close the window and rotate
    /// the arbiter.
    pub admin: Address,
    /// Stream contract address used for all cross-contract calls.
    pub stream_contract: Address,
    /// The arbiter: the only party that may approve or deny claims.
    pub arbiter: Address,
    /// Ledger timestamp at which the window was opened.  Zero before `open`.
    pub open_time: u64,
    /// Deadline (ledger timestamp) after which no new claims are accepted.
    pub deadline: u64,
    /// Current lifecycle state.
    pub status: WindowStatus,
}

// ---------------------------------------------------------------------------
// Storage key enumeration
// ---------------------------------------------------------------------------

/// Every storage key used by the dispute-window contract.
///
/// Explicit integer discriminants prevent accidental slot collisions if Rust
/// ever reorders the variants during refactoring (the same precaution as the
/// governance contract's `DataKey`).
#[contracttype]
pub enum DataKey {
    /// The window configuration blob (instance storage).
    Config = 0,
    /// A single claim record (persistent storage, keyed by stream id).
    ///
    /// There is one claim per stream per window.  Using `stream_id` directly
    /// as part of the key is intentional: the dispute window is scoped to a
    /// single deployment, so no additional window-id dimension is needed in
    /// the key.
    Claim(u64) = 1,
}
