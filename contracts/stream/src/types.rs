use soroban_sdk::{contracttype, Address};

/// Bitmask constants for which operations a delegate is permitted to perform.
///
/// Pass one constant or OR several together when calling [`crate::FluxoraStream::grant_delegate`].
/// New bits may be added; existing values are stable ABI.
pub mod op {
    pub const WITHDRAW: u32 = 1 << 0;
    pub const CANCEL: u32 = 1 << 1;
    pub const PAUSE: u32 = 1 << 2;
    pub const RESUME: u32 = 1 << 3;
    pub const TOP_UP: u32 = 1 << 4;
    pub const TRANSFER_RECIPIENT: u32 = 1 << 5;
}

/// Bitmask constants for the immutable [`Stream`] behavior flags.
///
/// All three bits are fixed at creation and never mutate. Packing them into a
/// single SDK-supported integer keeps one bit for each of `cancellable`,
/// `pausable` and `transferable`, shrinking the serialized `Stream` entry in
/// persistent storage by two fields.
pub mod flag {
    /// Stream may be cancelled by its sender.
    pub const CANCELLABLE: u32 = 1 << 0;
    /// Stream may be paused and resumed by its sender.
    pub const PAUSABLE: u32 = 1 << 1;
    /// Stream recipient may be transferred by the current recipient.
    pub const TRANSFERABLE: u32 = 1 << 2;
}

/// A delegation grant stored in persistent storage.
///
/// Scoped to one `(stream_id, delegate)` pair. The grantor is implied by which
/// bits are set: sender-side ops (`CANCEL`, `PAUSE`, `RESUME`, `TOP_UP`) can
/// only be granted by the sender; recipient-side ops (`WITHDRAW`,
/// `TRANSFER_RECIPIENT`) can only be granted by the recipient.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegateGrant {
    /// Bitmask of [`Op`] constants the delegate may invoke.
    pub ops: u32,
    /// Unix seconds after which this grant is no longer valid.
    /// `None` means the grant never expires on its own.
    pub expires_at: Option<u64>,
}

/// Lifecycle state of a stream.
///
/// `Cancelled` and `Depleted` are both terminal and both imply
/// withdrawable == 0` will eventually hold, but they are kept distinct so the
/// indexer can tell "dan to completion" apart from "sender clawed back the
/// unvested remainder". `Cancelled` is sticky: a cancelled stream that is
/// subsequently drained to zero stays `Cancelled` rather than becoming
/// `Depleted`.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamStatus {
    Active = 0,
    Paused = 1,
    Cancelled = 2,
    Depleted = 3,
}

impl StreamStatus {
    /// Terminal states accept no further lifecycle transitions.
    pub fn is_terminal(&self) -> bool {
        matches!(self, StreamStatus::Cancelled | StreamStatus::Depleted)
    }
}

/// A single payment stream.
///
/// One entry per stream lives in persistent storage under
/// `crate::types::DataKey::Stream`. There is deliberately no per-user index
/// anywhere on chain — see the module docs on `lib.rs` for why.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stream {
    pub sender: Address,
    pub recipient: Address,
    /// SEP-41 token contract. One token per stream; never changes.
    pub token: Address,
    /// Total ever deposited, including top-ups. Reduced to `vested` on cancel.
    pub deposited: i128,
    /// Total ever withdrawn by the recipient.
    pub withdrawn: i128,
    /// Unix seconds. May be in the past (backdated vesting is legitimate) or
    /// in the future (a scheduled stream). No bound on skew: the ledger
    /// timestamp is the only clock on chain, and well-formedness (`end > start`,
    /// `cliff` within `[start, end]`) is the whole validation. See
    /// [`crate::FluxoraStream::create_stream`].
    pub start_time: u64,
    /// Unix seconds. Strictly greater than `start_time` at creation.
    pub end_time: u64,
    /// Unix seconds in `[start_time, end_time]`. Equals `start_time` when there
    /// is no cliff. Gates withdrawal; does not delay accrual.
    pub cliff_time: u64,
    /// Fixed at creation, never mutable. Packed bitmask of [`flag`]:
    /// `CANCELLABLE | PAUSABLE | TRANSFERABLE`. One SDK-supported integer for
    /// all three behavior flags instead of three bool fields. See [`Stream::cancellable`]
    /// / [`Stream::pausable`] / [`Stream::transferable`].
    pub flags: u32,
    /// `Some(t)` while paused: the instant the accrual clock froze.
    pub paused_at: Option<u64>,
    /// Cumulative seconds spent paused, excluding any in-progress pause.
    pub paused_total: u64,
    pub status: StreamStatus,
}

impl Stream {
    /// Whether the sender may cancel this stream.
    pub fn cancellable(&self) -> bool {
        self.flags & flag::CANCELLABLE != 0
    }

    /// Whether the sender may pause and resume this stream.
    pub fn pausable(&self) -> bool {
        self.flags & flag::PAUSABLE != 0
    }

    /// Whether the recipient may transfer its claim to a new recipient.
    pub fn transferable(&self) -> bool {
        self.flags & flag::TRANSFERABLE != 0
    }

    /// Build the packed flags integer from the three creation-time booleans.
    pub fn flags_from_parts(cancellable: bool, pausable: bool, transferable: bool) -> u32 {
        u32::from(cancellable) | (u32::from(pausable) << 1) | (u32::from(transferable) << 2)
    }

    /// Enforces the recipient-only withdrawal policy.
    ///
    /// This is the sole authorization gate for withdrawals. A stream has no
    /// first-class delegate: no one other than the recipient may authorize an
    /// outgoing payment. This method must be called at the top of `withdraw`
    /// (and any other recipient-only operation) before any state changes.
    ///
    /// The recipient is authenticated via Soroban's authentication framework.
    /// When the recipient is a contract, the recipient itself decides whether
    /// to allow the caller to proceed (for example, by implementing
    /// `__check_auth`); the stream contract does not define a separate
    /// delegation mechanism.
    pub fn require_recipient_auth(&self) {
        self.recipient.require_auth();
    }
}

/// Storage keys.
///
/// `NextStreamId` lives in instance storage (tiny, shares the contract's TTL).
/// `Stream(id)` entries live in persistent storage with independent TTLs.
/// `Delegate(stream_id, delegate)` entries live in persistent storage, scoped
/// to the stream they were issued for.
///
/// There is no `Config` key: with no admin, no fees and no upgradeability
/// (all explicit non-goals), the contract has nothing to configure.
#[contracttype]
#[repr(u32)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Instance storage. Monotonic counter, next id to hand out.
    /// Incremented only on successful stream creation.
    NextStreamId = 0,
    /// Instance storage. Number of streams successfully created.
    /// Incremented only in the same transaction as `NextStreamId` and the
    /// corresponding `Stream(id)` entry.
    StreamCount = 1,
    /// Persistent storage. One entry per stream.
    Stream(u64) = 2,
    /// Persistent storage. One entry per (stream_id, delegate) pair.
    Delegate(u64, Address) = 3,
}
