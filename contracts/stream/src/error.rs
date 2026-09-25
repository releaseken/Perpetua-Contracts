use soroban_sdk::contracterror;

/// Every failure mode in Perpetua is a typed error. Nothing panics on a numeric
/// edge case: all arithmetic is checked and maps to [`Error::Overflow`].
///
/// Zero-amount policy: zero or negative deposit, top-up, and explicit
/// withdrawal amounts are errors, not no-ops. No zero-value event is emitted
/// for a rejected operation, and balances are conserved.
///
/// Discriminants are part of the public ABI. Never renumber an existing
/// variant; only append.
///
/// ## Creation atomicity
/// Stream creation is transactional: `next_stream_id` and `stream_count` are
/// only mutated after all validation and the token transfer succeed. If any
/// phase fails, no ID is consumed and no count is incremented; stream IDs are
/// therefore contiguous with no gaps.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    // --- Lookup ---
    /// No stream exists with the given id.
    StreamNotFound = 1,

    // --- Creation validation ---
    /// `end_time <= start_time`. A zero or negative duration would divide by zero.
    InvalidTimeRange = 2,
    /// `cliff_time` is outside [start_time, end_time].
    InvalidCliff = 3,
    /// Deposit is zero or negative.
    InvalidDeposit = 4,
    /// `deposited < duration`, so the per-second rate truncates to zero and the
    /// recipient would accrue nothing. See `MIN_RATE_STROOPS_PER_SECOND`.
    DepositRateTooLow = 5,
    /// `deposited < (end_time - start_time)`, i.e. the deposit-to-duration
    /// ratio is below the minimum of 1 stroop per second. The per-second rate
    /// would truncate to zero under integer division, so the recipient would
    /// accrue nothing until very late in the schedule. Deposit at least one
    /// stroop per second of duration.
    DepositTooSmall = 32,
    /// Sender and recipient are the same address.
    SelfStream = 6,

    // --- Authorization / capability ---
    /// Caller is not the party allowed to perform this action.
    Unauthorized = 7,
    /// `cancel` called on a stream created with `cancellable == false`.
    NotCancellable = 8,
    /// `pause` called on a stream created with `pausable == false`.
    NotPausable = 9,
    /// `transfer_recipient` called on a stream created with `transferable == false`.
    NotTransferable = 10,

    // --- State machine ---
    /// Action requires an `Active` stream.
    ///
    /// Reserved in the frozen ABI; current entry points use the more specific
    /// [`Self::StreamNotPaused`] / [`Self::StreamAlreadyPaused`] /
    /// [`Self::StreamTerminated`] variants instead. Do not renumber.
    StreamNotActive = 11,
    /// `resume` called on a stream that is not `Paused`.
    StreamNotPaused = 12,
    /// `pause` called on a stream that is already `Paused`.
    StreamAlreadyPaused = 13,
    /// Action attempted on a `Cancelled` or `Depleted` stream.
    ///
    /// A stream is `Depleted` when the recipient withdraws the exact
    /// withdrawable balance; subsequent withdrawals return this error.
    StreamTerminated = 14,
    /// `top_up` on a stream whose accrual clock has already reached `end_time`,
    /// Topping up a matured stream would make the new funds instantly
    /// withdrawable; create a new stream instead.
    StreamMatured = 15,

    // --- Withdrawal ---
    /// Requested amount exceeds the currently withdrawable balance.
    InsufficientWithdrawable = 16,
    /// Withdrawable balance is zero.
    NothingToWithdraw = 17,
    /// Explicit withdraw amount was zero or negative.
    InvalidAmount = 18,

    // --- Resource limits ---
    /// Batch size exceeds `MAX_BATCH_SIZE`. Chunk client-side.
    BatchTooLarge = 19,
    /// Batch contained no stream ids.
    EmptyBatch = 20,
    /// A Batch referenced the same stream id more than once.
    DuplicateStreamId = 21,

    // --- Arithmetic ---
    /// A Checked arithmetic operation overflowed or underflowed.
    Overflow = 22,
    /// A positive `top_up` amount is smaller than one second of streaming at
    /// the current rate, so it cannot extend the duration at all and would
    /// instead vest retroactively. Top up by at least `deposited / duration`.
    /// Zero or negative amounts return [`Self::InvalidTopUp`].
    TopUpTooSmall = 23,

    // --- Identifier exhaustion ---
    /// The stream-id counter has reached `u64::MAX`; no further ids can be
    /// handed out. Ids are monotonic and never reused, so the counter never
    /// wraps — this error is terminal for new-stream creation.
    StreamIdExhausted = 24,

    /// The `next_stream_id` counter would overflow `u64` on increment. This is
    /// the checked-increment guard for the stream counter: rather than silently
    /// wrapping to 0 (which would reuse ids and corrupt lookups), the increment
    /// fails with this typed error. Practically unreachable at 1.8e19 streams,
    /// but enforced so wrapping can never occur.
    StreamIdOverflow = 33,

    // --- Token sub-invocation ---
    /// The token contract rejected the transfer (e.g. insufficient balance in
    /// the pool on a payout, insufficient sender balance on a deposit, or the
    /// token contract's own authorization rules refused the call).
    ///
    /// When this occurs while creating a stream, no stream ID was allocated
    /// and `stream_count` is unchanged.
    ///
    /// The token contract's internal error discriminant is **intentionally
    /// discarded** here. Forwarding it would produce a value that clients
    /// decode against Perpetua's own error table, yielding a silent
    /// misinterpretation. The raw diagnostic is visible on chain in the failed
    /// transaction's `diagnosticEvents`; this variant is what a stream client
    /// should match on.
    TokenTransferFailed = 25,

    /// The address stored as the stream's token does not resolve to a deployed
    /// contract. This indicates a misconfigured stream; no funds have moved.
    ///
    /// Surfaces when the token sub-invocation fails with an `Abort` (host
    /// trap) rather than a typed contract error, which is what the host
    /// produces when the callee contract does not exist.
    TokenMissing = 26,

    // --- Delegation ---
    /// The delegate grant does not permit this operation on this stream.
    DelegateNotPermitted = 27,
    /// The delegate grant has passed its `expires_at` timestamp.
    DelegateExpired = 28,

    // --- Batch validation ---
    /// A serialized vector element of a batch is not a `u64`.
    MalformedStreamId = 29,

    // --- Transfer ---
    /// `transfer_recipient` to the current recipient.
    RepeatedTransfer = 30,

    // --- Arithmetic (top-up) ---
    /// Zero or negative `top_up` amount.
    InvalidTopUp = 31,

    // --- Batch resource limits ---
    /// `batch_withdraw` received more than `MAX_BATCH_SIZE` (16) stream ids.
    ///
    /// The 16-stream ceiling is bounded by Soroban's contract event budget
    /// (16,384 bytes): each withdrawal emits both a contract and a token
    /// event (~512 bytes), so a larger batch would exceed event memory limits
    /// on heavy token contracts. Chunk the ids client-side.
    BatchSizeExceeded = 32,
}
