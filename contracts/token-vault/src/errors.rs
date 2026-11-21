use soroban_sdk::contracterror;

/// Every failure mode in the token-vault contract.
/// Nothing panics; all arithmetic uses checked operations.
///
/// Discriminants are part of the public ABI.  Never renumber; only append.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum VaultError {
    // --- Initialization ---
    /// Contract has not been initialized.
    NotInitialized = 1,
    /// `init` has already been called.
    AlreadyInitialized = 2,

    // --- Authorization ---
    /// Caller is not the admin.
    Unauthorized = 3,

    // --- Position creation ---
    /// `end_time <= start_time`.
    InvalidTimeRange = 4,
    /// `cliff_time` is outside `[start_time, end_time]`.
    InvalidCliff = 5,
    /// Deposit is zero or negative.
    InvalidDeposit = 6,
    /// `deposit < (end_time - start_time)`: accrual rate below 1 stroop/s.
    DepositRateTooLow = 7,
    /// `deposit * duration` overflows `i128`.
    Overflow = 8,

    // --- Position lifecycle ---
    /// No position exists for the given ID.
    PositionNotFound = 9,
    /// Action requires a non-terminal position.
    PositionTerminated = 10,
    /// Position has already been fully claimed.
    PositionDepleted = 11,

    // --- Withdrawal ---
    /// Withdrawable balance is zero (cliff not reached or nothing accrued).
    NothingToWithdraw = 12,
    /// Explicit amount exceeds the withdrawable balance.
    InsufficientWithdrawable = 13,
    /// Explicit amount is zero or negative.
    InvalidAmount = 14,

    // --- Token ---
    /// Token contract rejected the transfer.
    TokenTransferFailed = 15,
    /// Token contract address has no deployed code.
    TokenMissing = 16,

    // --- Arithmetic guard ---
    /// Checked arithmetic overflowed in an unexpected path.
    ArithmeticOverflow = 17,
}
