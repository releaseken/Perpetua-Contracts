use soroban_sdk::contracterror;

/// Every failure mode in the fee-accrual contract, as a typed, client-observable
/// error.
///
/// # Design rationale (issue #385, issue #389)
///
/// Errors are split into five semantic groups so client tooling and SDKs can
/// branch on failure domains without matching every individual discriminant:
///
/// | Group | Discriminants | Category | Meaning |
/// |-------|--------------|----------|---------|
/// | Initialization | 1–2 | Group A | Contract lifecycle |
/// | Authorization | 3–6, 19–20 | Group B | Caller identity & role permissions |
/// | Validation | 7–10, 21 | Group C | Fee configuration and parameter bounds |
/// | Accounting | 11–14, 22 | Group D | Accrual, claim, and balance states |
/// | Arithmetic / Limits | 15–18 | Group E | Arithmetic guards and batch bounds |
///
/// Discriminants are part of the public ABI.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum FeeError {
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
    /// Caller is not an authorized fee distributor or caller.
    CallerNotAuthorized = 4,
    /// `accept_admin` was called but no two-step rotation proposal is pending.
    NoPendingAdmin = 5,
    /// The proposed new admin is the zero address or the contract itself;
    /// installing either would permanently lock all admin-gated operations.
    InvalidNewAdmin = 6,

    // -----------------------------------------------------------------------
    // Group C — Configuration & Parameter Validation
    // -----------------------------------------------------------------------
    /// The fee split in basis points exceeds 10,000 (100%).
    InvalidBpsRate = 7,
    /// The recipient address is invalid, null, or zero.
    InvalidFeeRecipient = 8,
    /// The stream or reference identifier provided is invalid or zero.
    InvalidStreamId = 9,
    /// The token address is invalid or contract has no deployed code.
    InvalidTokenAddress = 10,

    // -----------------------------------------------------------------------
    // Group D — Accounting & Balances
    // -----------------------------------------------------------------------
    /// No accrued fee balance exists or nothing is available to sweep or withdraw.
    ZeroFeeAccrual = 11,
    /// The requested fee claim amount exceeds accrued uncollected balance.
    InsufficientAccruedFees = 12,
    /// No record found for the queried fee account or stream fee entry.
    FeeRecordNotFound = 13,
    /// Fee accrual for this stream has already settled or is in a terminal state.
    FeeAlreadySettled = 14,

    // -----------------------------------------------------------------------
    // Group E — Arithmetic & Batch Limits
    // -----------------------------------------------------------------------
    /// A checked arithmetic operation overflowed or underflowed.
    ArithmeticOverflow = 15,
    /// Batch fee accrual or distribution list was submitted empty.
    EmptyBatch = 16,
    /// Batch exceeds maximum allowed batch size limit.
    BatchTooLarge = 17,
    /// An internal accounting invariant was violated; transaction aborted.
    InvariantViolation = 18,

    // -----------------------------------------------------------------------
    // Role Authorization Additions (Issue #385)
    // -----------------------------------------------------------------------
    /// Caller is not authorized for the requested role-gated operation.
    UnauthorizedRole = 19,
    /// Proposed role address is invalid (zero address or contract itself).
    InvalidRoleAddress = 20,
    /// Contract is currently paused by emergency admin.
    ContractPaused = 21,
    /// Contract is already unpaused.
    ContractNotPaused = 22,

    // -----------------------------------------------------------------------
    // Cross-Contract Batch Claims & Coalescing (Issue #391)
    // -----------------------------------------------------------------------
    /// Batch contains duplicate stream IDs; rejected to prevent double-counting.
    DuplicateStreamId = 23,
    /// Parallel vectors passed to a batch operation have mismatched lengths.
    LengthMismatch = 24,
    /// Gross accrual amount must be strictly positive (> 0).
    InvalidAmount = 25,
}

impl FeeError {
    /// Canonical human-readable message for each error variant.
    pub const fn message(self) -> &'static str {
        match self {
            FeeError::NotInitialized => "contract has not been initialized",
            FeeError::AlreadyInitialized => "contract is already initialized",
            FeeError::Unauthorized => "caller is not the admin",
            FeeError::CallerNotAuthorized => "caller is not authorized to accrue or claim fees",
            FeeError::NoPendingAdmin => "no admin rotation proposal is pending",
            FeeError::InvalidNewAdmin => {
                "proposed admin is the zero address or the contract itself"
            }
            FeeError::InvalidBpsRate => "fee rate exceeds maximum allowed 10,000 basis points",
            FeeError::InvalidFeeRecipient => "fee recipient must be a valid non-zero address",
            FeeError::InvalidStreamId => "stream ID must be greater than zero",
            FeeError::InvalidTokenAddress => "token address must be valid and non-zero",
            FeeError::ZeroFeeAccrual => "accrued fee balance is zero; nothing to claim",
            FeeError::InsufficientAccruedFees => {
                "requested claim exceeds currently accrued fee balance"
            }
            FeeError::FeeRecordNotFound => "fee accrual record not found for the given stream",
            FeeError::FeeAlreadySettled => "fee record is already settled and terminal",
            FeeError::ArithmeticOverflow => "checked arithmetic overflow or underflow occurred",
            FeeError::EmptyBatch => "batch must contain at least one fee entry",
            FeeError::BatchTooLarge => "batch exceeds MAX_BATCH_SIZE; split into smaller batches",
            FeeError::InvariantViolation => {
                "post-condition invariant check failed; state desync detected"
            }
            FeeError::UnauthorizedRole => "caller does not possess required role authorization",
            FeeError::InvalidRoleAddress => {
                "role address cannot be zero address or contract address"
            }
            FeeError::ContractPaused => "fee-accrual contract is currently paused",
            FeeError::ContractNotPaused => "fee-accrual contract is not paused",
            FeeError::DuplicateStreamId => {
                "batch contains duplicate stream IDs; deduplicate before invoking"
            }
            FeeError::LengthMismatch => {
                "parallel batch input vectors must have identical lengths"
            }
            FeeError::InvalidAmount => "fee accrual gross amount must be strictly positive",
        }
    }

    /// Return the discriminant group letter for coarse client-side branching.
    pub const fn group(self) -> char {
        match self {
            FeeError::NotInitialized | FeeError::AlreadyInitialized => 'A',
            FeeError::Unauthorized
            | FeeError::CallerNotAuthorized
            | FeeError::NoPendingAdmin
            | FeeError::InvalidNewAdmin
            | FeeError::UnauthorizedRole => 'B',
            FeeError::InvalidBpsRate
            | FeeError::InvalidFeeRecipient
            | FeeError::InvalidStreamId
            | FeeError::InvalidTokenAddress
            | FeeError::InvalidRoleAddress
            | FeeError::InvalidAmount => 'C',
            FeeError::ZeroFeeAccrual
            | FeeError::InsufficientAccruedFees
            | FeeError::FeeRecordNotFound
            | FeeError::FeeAlreadySettled
            | FeeError::ContractPaused
            | FeeError::ContractNotPaused => 'D',
            FeeError::ArithmeticOverflow
            | FeeError::EmptyBatch
            | FeeError::BatchTooLarge
            | FeeError::InvariantViolation
            | FeeError::DuplicateStreamId
            | FeeError::LengthMismatch => 'E',
        }
    }
}
