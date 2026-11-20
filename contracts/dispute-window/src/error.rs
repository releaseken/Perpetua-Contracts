use soroban_sdk::contracterror;

/// Every failure mode in the dispute-window is a typed, ABI-stable error.
///
/// # ABI stability rule
///
/// Discriminants are part of the public ABI.  Never renumber an existing
/// variant; only append.  Off-chain clients decode the raw `u32` to map a host
/// error to a human-readable category, so renumbering silently misdirects them.
///
/// # Client observability (issue #416)
///
/// Errors are grouped into named categories below.  Each category represents a
/// distinct failure domain, so a client can determine *what went wrong* without
/// inspecting chain diagnostics:
///
/// * **Lifecycle** — preconditions on the contract or window state.
/// * **Authorization** — who may perform an action.
/// * **Claim validation** — problems with the claim payload.
/// * **State-machine** — illegal transitions given the current window status.
/// * **Cross-contract** — problems calling the stream contract.
/// * **Resource limits** — batch size and ID constraints.
/// * **Arithmetic** — overflow/underflow that should never reach production but
///   is typed rather than a host trap.
///
/// # Zero-amount policy
///
/// A claim for zero tokens is rejected with [`DisputeError::InvalidClaimAmount`]
/// rather than silently accepted or silently dropped.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum DisputeError {
    // --- Lifecycle ---

    /// Contract has not been initialised via `init`.
    NotInitialized = 1,
    /// Contract is already initialised; `init` may not be called again.
    AlreadyInitialized = 2,

    // --- Authorization ---

    /// Caller is not the admin.
    Unauthorized = 3,
    /// Caller is not the dispute window's designated arbiter.
    NotArbiter = 4,
    /// The claimant address does not match the stream's recipient.
    ClaimantMismatch = 5,

    // --- Claim validation ---

    /// A batch contained a stream ID that was referenced more than once.
    DuplicateStreamId = 6,
    /// A batch was submitted with no stream IDs.
    EmptyBatch = 7,
    /// The requested claim amount was zero or negative.
    InvalidClaimAmount = 8,
    /// The claim arrival timestamp predates the dispute window's open time.
    ClaimBeforeWindowOpen = 9,
    /// The claim arrival timestamp is after the dispute window's deadline.
    ClaimAfterWindowClose = 10,
    /// A stream ID in a batch could not be decoded as a `u64`.
    MalformedStreamId = 11,

    // --- State machine ---

    /// The dispute window is not yet open; claims cannot be lodged.
    WindowNotOpen = 12,
    /// The dispute window has already been closed; no further claims accepted.
    WindowClosed = 13,
    /// A claim for this stream ID already exists in this window.
    ClaimAlreadyExists = 14,
    /// No claim for this stream ID was found in this window.
    ClaimNotFound = 15,
    /// The window was finalised; the arbiter cannot modify claims any further.
    WindowFinalised = 16,
    /// The window is already open; `open` may not be called again.
    WindowAlreadyOpen = 17,

    // --- Cross-contract (issue #418) ---

    /// The stream contract returned a typed error during a cross-contract call.
    ///
    /// The stream contract's internal discriminant is intentionally discarded.
    /// It is visible in the transaction's `diagnosticEvents` for off-chain
    /// tooling; this variant is what a dispute client should match on.
    StreamCallFailed = 18,
    /// The host raised an `Abort` trap when calling the stream contract,
    /// most commonly because the address has no deployed code.
    StreamContractMissing = 19,

    // --- Resource limits (issue #413) ---

    /// A batch exceeded `MAX_BATCH_CLAIM_SIZE`.  Chunk the IDs client-side.
    BatchTooLarge = 20,

    // --- Arithmetic ---

    /// A checked arithmetic operation overflowed or underflowed.
    Overflow = 21,

    // --- Archival (issue #417) ---

    /// The claim entry has already been extended to the maximum TTL for this
    /// ledger; no further extension is possible.
    TtlAlreadyMaximal = 22,
}

impl DisputeError {
    /// Alias for [`DisputeError::Overflow`] using arithmetic terminology.
    ///
    /// Provides a stable alias without touching the discriminant table.
    pub const ArithmeticOverflow: DisputeError = DisputeError::Overflow;
}
