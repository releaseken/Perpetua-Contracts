use soroban_sdk::contracterror;

/// Every failure mode in the cliff-vesting contract is a typed error.
/// Nothing panics; all arithmetic uses checked operations.
///
/// Discriminants are part of the public ABI. Never renumber; only append.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    // --- Initialization ---
    /// Contract has not been initialized via `init`.
    NotInitialized = 1,
    /// `init` has already been called.
    AlreadyInitialized = 2,

    // --- Authorization / roles ---
    /// Caller does not hold the required role.
    Unauthorized = 3,
    /// The proposed new role holder is the zero address or the contract itself.
    InvalidRoleAddress = 4,

    // --- Admin rotation ---
    /// `accept_admin` called with no pending proposal.
    NoPendingAdmin = 5,

    // --- Vesting schedule creation ---
    /// `end_time <= start_time`.
    InvalidTimeRange = 6,
    /// `cliff_time` is outside `[start_time, end_time]`.
    InvalidCliff = 7,
    /// Deposit is zero or negative.
    InvalidDeposit = 8,
    /// Deposit is below one stroop per second; recipient would accrue nothing.
    DepositTooSmall = 9,
    /// Sender and beneficiary are the same address.
    SelfVest = 10,
    /// `deposit * duration` overflows `i128`.
    Overflow = 11,
    /// The vesting-schedule ID counter would overflow `u64`.
    ScheduleIdOverflow = 12,

    // --- Schedule lifecycle ---
    /// No schedule exists with the given ID.
    ScheduleNotFound = 13,
    /// Action requires an active (non-terminal) schedule.
    ScheduleTerminated = 14,
    /// Schedule has already been fully claimed.
    ScheduleDepleted = 15,

    // --- Withdrawal ---
    /// Withdrawable balance is zero.
    NothingToWithdraw = 16,
    /// Explicit amount exceeds the withdrawable balance.
    InsufficientWithdrawable = 17,
    /// Explicit amount is zero or negative.
    InvalidAmount = 18,

    // --- Token ---
    /// Token contract rejected the transfer.
    TokenTransferFailed = 19,
    /// Token contract address has no deployed code.
    TokenMissing = 20,

    // --- Batch ---
    /// Batch contained no IDs.
    EmptyBatch = 21,
    /// Batch exceeded `MAX_BATCH_SIZE`.
    BatchTooLarge = 22,
    /// Batch contained a duplicate schedule ID.
    DuplicateScheduleId = 23,
    /// A serialized vector element could not be decoded as a `u64`.
    MalformedScheduleId = 24,

    // --- Configuration ---
    /// A configuration value was out of its valid range.
    InvalidConfig = 25,
}
