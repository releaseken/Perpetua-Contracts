use soroban_sdk::contracterror;

/// Every failure mode in the pause-clock contract is a typed error.
/// Nothing panics: all arithmetic uses checked operations and maps
/// edge-cases to [`Error::Overflow`].
///
/// Discriminants are part of the public ABI. Never renumber an existing
/// variant; only append new ones at the end.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    // --- Initialization ---
    /// Contract has not been initialized via `init`.
    NotInitialized = 1,
    /// `init` has already been called; cannot re-initialize.
    AlreadyInitialized = 2,

    // --- Authorization ---
    /// Caller is not the registered admin.
    Unauthorized = 3,
    /// Only the registered stream contract may call this entry point.
    CallerNotStreamContract = 4,

    // --- Admin rotation ---
    /// The proposed new admin is the zero address or the contract itself;
    /// installing either would permanently lock all admin-gated operations.
    InvalidNewAdmin = 5,
    /// `accept_admin` was called but no rotation proposal is pending.
    NoPendingAdmin = 6,

    // --- Clock state ---
    /// `record_pause` called while the global clock is already paused.
    AlreadyPaused = 7,
    /// `record_resume` called while the global clock is not paused.
    NotPaused = 8,
    /// The stream ID provided does not exist in the registry.
    StreamNotFound = 9,

    // --- Arithmetic ---
    /// A checked arithmetic operation overflowed or underflowed.
    Overflow = 10,

    // --- Batch ---
    /// Batch contained no stream IDs.
    EmptyBatch = 11,
    /// Batch exceeded `MAX_BATCH_SIZE`.
    BatchTooLarge = 12,
    /// Batch contained a duplicate stream ID.
    DuplicateStreamId = 13,
}
