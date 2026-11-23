//! Error definitions and categories for the factory contract.
//!
//! # Client Observability & Modular Error Architecture (Issue #362)
//!
//! Failure modes are modeled as typed, ABI-stable Soroban contract errors.
//! To provide enhanced client observability for SDKs, monitoring indexers,
//! and end-user frontends, error variants are categorized into distinct semantic
//! groups:
//!
//! | Group | Group Code | Category | Discriminants | Description |
//! |-------|------------|----------|---------------|-------------|
//! | **A** | `'A'` | Lifecycle | 1, 2 | Initialization and state lifetime |
//! | **B** | `'B'` | Authorization & Governance | 3, 5, 6 | Administrative privileges and two-step handoff |
//! | **C** | `'C'` | Parameter & Policy Validation | 4, 7, 8, 10, 15, 18, 19 | Creation parameters, durations, bounds, allowlists |
//! | **D** | `'D'` | Rate & Capacity Bounds | 9, 11, 14, 16, 17 | Throttling, deposit caps, active stream limits, velocity rates |
//! | **E** | `'E'` | Accounting & Safety | 12, 13, 20, 21 | State machine counters, pause controls, reentrancy, invariant assertions |
//!
//! Discriminants are part of the public ABI. Existing discriminant values
//! MUST remain stable and never be renumbered.

use soroban_sdk::contracterror;

/// Categorical error group representing failure domains for coarse client-side branching.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ErrorCategory {
    /// Contract lifecycle errors (initialization).
    Lifecycle,
    /// Authorization and administrative governance errors.
    Authorization,
    /// Parameter validation, policy bounds, and allowlist rules.
    Validation,
    /// Rate limits, capacity quotas, and velocity constraints.
    CapacityAndRates,
    /// Contract accounting, pause status, re-entrancy, and security invariants.
    AccountingAndSafety,
}

impl ErrorCategory {
    /// Returns the uppercase group code character ('A' through 'E').
    pub const fn code(self) -> char {
        match self {
            Self::Lifecycle => 'A',
            Self::Authorization => 'B',
            Self::Validation => 'C',
            Self::CapacityAndRates => 'D',
            Self::AccountingAndSafety => 'E',
        }
    }

    /// Returns human-readable name of the error category.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lifecycle => "Lifecycle",
            Self::Authorization => "Authorization & Governance",
            Self::Validation => "Parameter & Policy Validation",
            Self::CapacityAndRates => "Rate & Capacity Bounds",
            Self::AccountingAndSafety => "Accounting & Safety",
        }
    }
}

/// Error codes for the factory contract.
#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum FactoryError {
    // -----------------------------------------------------------------------
    // Group A — Lifecycle (1..=2)
    // -----------------------------------------------------------------------
    /// Contract has not been initialised.
    NotInitialized = 1,
    /// Contract is already initialised.
    AlreadyInitialized = 2,

    // -----------------------------------------------------------------------
    // Group B — Authorization & Governance (3, 5, 6)
    // -----------------------------------------------------------------------
    /// Caller is not the admin.
    Unauthorized = 3,
    /// The proposed admin cannot be set: it is the zero address or the factory
    /// contract itself, either of which would permanently lock the factory.
    InvalidNewAdmin = 5,
    /// `accept_admin` was called but no admin transfer has been proposed.
    NoPendingAdmin = 6,

    // -----------------------------------------------------------------------
    // Group C — Parameter & Policy Validation (4, 7, 8, 10, 15, 18, 19)
    // -----------------------------------------------------------------------
    /// The recipient is not allowlisted.
    AllowlistDenied = 4,
    /// Duration is zero, overflows, or violates configured min/max duration bounds.
    InvalidDuration = 7,
    /// The token address is not on the factory's approved-token allowlist.
    TokenNotAllowed = 8,
    /// Rate limit configuration parameters are invalid (e.g. zero creations or zero window).
    InvalidRateLimit = 10,
    /// Rate bounds configuration is invalid (e.g. negative rate or min_rate > max_rate).
    InvalidRateBounds = 15,
    /// Deposit amount is non-positive or exceeds maximum allowable integer bounds.
    InvalidDeposit = 18,
    /// Cliff timestamp is invalid (outside `[start_time, end_time]`).
    InvalidCliff = 19,

    // -----------------------------------------------------------------------
    // Group D — Rate & Capacity Bounds (9, 11, 14, 16, 17)
    // -----------------------------------------------------------------------
    /// Sender has exceeded maximum allowed stream creations in the rolling ledger window.
    RateLimitExceeded = 9,
    /// Maximum active streams capacity cap reached or capacity quota exceeded.
    CapacityCapExceeded = 11,
    /// Deposit amount exceeds the factory-configured maximum deposit cap.
    DepositCapExceeded = 14,
    /// Stream flow rate is below the configured minimum per-second rate.
    RateBelowMinimum = 16,
    /// Stream flow rate exceeds the configured maximum per-second rate.
    RateAboveMaximum = 17,

    // -----------------------------------------------------------------------
    // Group E — Accounting & Safety (12, 13, 20, 21)
    // -----------------------------------------------------------------------
    /// Attempted to decrement active stream count when active streams is already zero.
    ActiveStreamCountUnderflow = 12,
    /// Stream creation is currently paused by governance policy.
    CreationPaused = 13,
    /// A re-entrant call was detected; the factory is already executing a state-modifying call.
    ReentrancyGuard = 20,
    /// A post-condition invariant check failed after a state mutation.
    InvariantViolation = 21,
}

impl FactoryError {
    /// Canonical human-readable description for each error variant.
    ///
    /// Useful for SDKs, off-chain client tooling, and monitoring systems to
    /// decode numeric host errors into clear descriptive diagnostics.
    pub const fn message(self) -> &'static str {
        match self {
            FactoryError::NotInitialized => "factory contract has not been initialized",
            FactoryError::AlreadyInitialized => "factory contract is already initialized",
            FactoryError::Unauthorized => "caller is not authorized as factory admin",
            FactoryError::AllowlistDenied => "recipient address is not on the approved allowlist",
            FactoryError::InvalidNewAdmin => {
                "proposed admin cannot be the zero address or the factory contract itself"
            }
            FactoryError::NoPendingAdmin => "no admin transfer proposal is currently pending",
            FactoryError::InvalidDuration => {
                "stream duration violates factory min/max duration policy"
            }
            FactoryError::TokenNotAllowed => "token address is not on the approved token allowlist",
            FactoryError::RateLimitExceeded => {
                "sender creation rate limit exceeded for the current window"
            }
            FactoryError::InvalidRateLimit => {
                "rate limit window and creations-per-window must be greater than zero"
            }
            FactoryError::CapacityCapExceeded => "active stream count exceeds maximum capacity cap",
            FactoryError::ActiveStreamCountUnderflow => {
                "cannot decrement active stream count below zero"
            }
            FactoryError::CreationPaused => "stream creation is currently paused by governance",
            FactoryError::DepositCapExceeded => "deposit exceeds configured maximum stream deposit",
            FactoryError::InvalidRateBounds => {
                "minimum rate per second cannot exceed maximum rate per second"
            }
            FactoryError::RateBelowMinimum => {
                "stream rate per second is below configured minimum rate"
            }
            FactoryError::RateAboveMaximum => {
                "stream rate per second exceeds configured maximum rate"
            }
            FactoryError::InvalidDeposit => "deposit amount must be greater than zero",
            FactoryError::InvalidCliff => {
                "cliff timestamp must be between stream start and end time"
            }
            FactoryError::ReentrancyGuard => {
                "re-entrant call detected; factory is already executing"
            }
            FactoryError::InvariantViolation => {
                "factory state invariant check failed; state transition aborted"
            }
        }
    }

    /// Returns the semantic error category for this error variant.
    pub const fn category(self) -> ErrorCategory {
        match self {
            FactoryError::NotInitialized | FactoryError::AlreadyInitialized => {
                ErrorCategory::Lifecycle
            }
            FactoryError::Unauthorized
            | FactoryError::InvalidNewAdmin
            | FactoryError::NoPendingAdmin => ErrorCategory::Authorization,
            FactoryError::AllowlistDenied
            | FactoryError::InvalidDuration
            | FactoryError::TokenNotAllowed
            | FactoryError::InvalidRateLimit
            | FactoryError::InvalidRateBounds
            | FactoryError::InvalidDeposit
            | FactoryError::InvalidCliff => ErrorCategory::Validation,
            FactoryError::RateLimitExceeded
            | FactoryError::CapacityCapExceeded
            | FactoryError::DepositCapExceeded
            | FactoryError::RateBelowMinimum
            | FactoryError::RateAboveMaximum => ErrorCategory::CapacityAndRates,
            FactoryError::ActiveStreamCountUnderflow
            | FactoryError::CreationPaused
            | FactoryError::ReentrancyGuard
            | FactoryError::InvariantViolation => ErrorCategory::AccountingAndSafety,
        }
    }

    /// Returns the single character group letter ('A' through 'E') for coarse client branching.
    pub const fn group(self) -> char {
        self.category().code()
    }

    /// Map from numeric discriminant to `FactoryError` variant, if valid.
    pub const fn from_discriminant(discriminant: u32) -> Option<Self> {
        match discriminant {
            1 => Some(FactoryError::NotInitialized),
            2 => Some(FactoryError::AlreadyInitialized),
            3 => Some(FactoryError::Unauthorized),
            4 => Some(FactoryError::AllowlistDenied),
            5 => Some(FactoryError::InvalidNewAdmin),
            6 => Some(FactoryError::NoPendingAdmin),
            7 => Some(FactoryError::InvalidDuration),
            8 => Some(FactoryError::TokenNotAllowed),
            9 => Some(FactoryError::RateLimitExceeded),
            10 => Some(FactoryError::InvalidRateLimit),
            11 => Some(FactoryError::CapacityCapExceeded),
            12 => Some(FactoryError::ActiveStreamCountUnderflow),
            13 => Some(FactoryError::CreationPaused),
            14 => Some(FactoryError::DepositCapExceeded),
            15 => Some(FactoryError::InvalidRateBounds),
            16 => Some(FactoryError::RateBelowMinimum),
            17 => Some(FactoryError::RateAboveMaximum),
            18 => Some(FactoryError::InvalidDeposit),
            19 => Some(FactoryError::InvalidCliff),
            20 => Some(FactoryError::ReentrancyGuard),
            21 => Some(FactoryError::InvariantViolation),
            _ => None,
        }
    }
}
