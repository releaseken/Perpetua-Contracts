use soroban_sdk::contracterror;

/// Every failure mode in the event-sink contract, as a typed, client-observable
/// error.
///
/// # Design rationale (issue #461)
///
/// Errors are split into four semantic groups so client tooling can branch on
/// categories without matching every individual variant:
///
/// | Group | Discriminants | Meaning |
/// |-------|--------------|---------|
/// | Initialization | 1–2 | Contract lifecycle |
/// | Authorization | 3–5 | Caller identity & permission |
/// | Payload validation | 6–9 | Structural problems with an `emit` call |
/// | Arithmetic / limits | 10–11 | Overflow guards and batch bounds |
///
/// Discriminants are part of the public ABI.  Never renumber an existing
/// variant; only append new ones at the end.
///
/// # Custom messages
///
/// Each variant carries a doc-comment that is both human-readable and
/// machine-extractable via ABI tooling.  The `impl Error` block below exposes
/// a `message()` method so clients that cannot inspect Rust docs at runtime
/// (e.g. TypeScript SDKs decoding a `u32` from the ledger) can recover the
/// canonical description from the numeric discriminant.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum SinkError {
    // -----------------------------------------------------------------------
    // Group A — Initialization
    // -----------------------------------------------------------------------
    /// Contract has not been initialized via `init`.
    NotInitialized = 1,
    /// `init` has already been called; re-initialization is not permitted.
    AlreadyInitialized = 2,

    // -----------------------------------------------------------------------
    // Group B — Authorization
    // -----------------------------------------------------------------------
    /// Caller is not the registered admin; admin-gated operation refused.
    Unauthorized = 3,
    /// Only a registered emitter contract may call `emit`; unknown caller rejected.
    CallerNotRegisteredEmitter = 4,
    /// `accept_admin` was called but no two-step rotation proposal is pending.
    NoPendingAdmin = 5,
    /// The proposed new admin is the zero address or the contract itself;
    /// installing either would permanently lock all admin-gated operations.
    InvalidNewAdmin = 6,

    // -----------------------------------------------------------------------
    // Group C — Payload validation
    // -----------------------------------------------------------------------
    /// The `topic` symbol is empty; every sink entry must carry a non-empty topic.
    EmptyTopic = 7,
    /// The `source_contract` field is the zero address; a valid contract address
    /// is required so entries can be correlated to their origin.
    InvalidSourceContract = 8,
    /// The `sequence` field on the entry is `0`; callers must supply a positive
    /// monotonic sequence number so clients can detect gaps and duplicates.
    InvalidSequence = 9,
    /// The `payload` byte-string exceeds `MAX_PAYLOAD_BYTES`; truncate or
    /// split the payload before calling `emit`.
    PayloadTooLarge = 10,

    // -----------------------------------------------------------------------
    // Group D — Arithmetic / limits
    // -----------------------------------------------------------------------
    /// The internal entry-counter would overflow `u64`; no more entries can be
    /// accepted until the contract is redeployed.
    EntryCountOverflow = 11,
    /// Batch call contained no entries.
    EmptyBatch = 12,
    /// Batch call exceeded `MAX_BATCH_SIZE`; split into smaller batches.
    BatchTooLarge = 13,
}

impl SinkError {
    /// Canonical human-readable message for each error variant.
    ///
    /// Intended for client SDKs that decode the numeric discriminant from the
    /// ledger and want to surface a descriptive string without embedding Rust
    /// docs.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let msg = SinkError::CallerNotRegisteredEmitter.message();
    /// // msg == "caller is not a registered emitter"
    /// ```
    pub const fn message(self) -> &'static str {
        match self {
            SinkError::NotInitialized => "contract has not been initialized",
            SinkError::AlreadyInitialized => "contract is already initialized",
            SinkError::Unauthorized => "caller is not the admin",
            SinkError::CallerNotRegisteredEmitter => "caller is not a registered emitter",
            SinkError::NoPendingAdmin => "no admin rotation proposal is pending",
            SinkError::InvalidNewAdmin => {
                "proposed admin is the zero address or the contract itself"
            }
            SinkError::EmptyTopic => "entry topic must not be empty",
            SinkError::InvalidSourceContract => "source_contract must be a valid non-zero address",
            SinkError::InvalidSequence => "entry sequence must be greater than zero",
            SinkError::PayloadTooLarge => "payload exceeds MAX_PAYLOAD_BYTES",
            SinkError::EntryCountOverflow => "entry counter overflow; no more entries accepted",
            SinkError::EmptyBatch => "batch must contain at least one entry",
            SinkError::BatchTooLarge => "batch exceeds MAX_BATCH_SIZE; split into smaller batches",
        }
    }

    /// Return the discriminant group letter for coarse client-side branching.
    ///
    /// | Return | Meaning |
    /// |--------|---------|
    /// | `'A'`  | Initialization error |
    /// | `'B'`  | Authorization error |
    /// | `'C'`  | Payload validation error |
    /// | `'D'`  | Arithmetic / limits error |
    pub const fn group(self) -> char {
        match self {
            SinkError::NotInitialized | SinkError::AlreadyInitialized => 'A',
            SinkError::Unauthorized
            | SinkError::CallerNotRegisteredEmitter
            | SinkError::NoPendingAdmin
            | SinkError::InvalidNewAdmin => 'B',
            SinkError::EmptyTopic
            | SinkError::InvalidSourceContract
            | SinkError::InvalidSequence
            | SinkError::PayloadTooLarge => 'C',
            SinkError::EntryCountOverflow | SinkError::EmptyBatch | SinkError::BatchTooLarge => 'D',
        }
    }
}
