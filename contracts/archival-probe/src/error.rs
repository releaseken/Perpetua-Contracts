//! Modular error definitions, categorizations, and diagnostic messages for archival probe.
//!
//! # Client Observability & Error Architecture (Issues #379 & #380)
//!
//! Failure modes are modeled as typed, ABI-stable Soroban contract errors.
//! To provide enhanced client observability for SDKs, monitoring indexers,
//! and formal verification callers, error variants are categorized into distinct semantic
//! groups:
//!
//! | Group | Group Code | Category | Discriminants | Description |
//! |-------|------------|----------|---------------|-------------|
//! | **A** | `'A'` | Lifecycle & Availability | 1, 9, 10 | Archival status and entry lifecycle |
//! | **B** | `'B'` | Batch & Validation | 2, 3, 4 | Batch constraints, size limits, duplicate inputs |
//! | **C** | `'C'` | Arithmetic & Metering | 5 | Safe arithmetic guards and overflow protection |
//! | **D** | `'D'` | Storage & Integrity | 6, 7 | Partition corruption and invariant verification |
//! | **E** | `'E'` | Re-entrancy & Concurrency | 8 | Execution lock and cross-call concurrency |
//!
//! Discriminants are part of the public ABI. Existing discriminant values
//! MUST remain stable and never be renumbered.

use soroban_sdk::contracterror;

/// Categorical error group representing failure domains for coarse client-side branching.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ErrorCategory {
    /// Archival status and entry presence lifecycle errors.
    Lifecycle,
    /// Batch input parameters, bounds, and duplicate validation errors.
    BatchValidation,
    /// Host metering, bounds calculation, and checked arithmetic errors.
    Arithmetic,
    /// Storage integrity, state desynchronization, and formal invariant violations.
    Integrity,
    /// Re-entrancy guards and concurrent invocation conflicts.
    Reentrancy,
}

impl ErrorCategory {
    /// Returns the uppercase group code character ('A' through 'E').
    pub const fn code(self) -> char {
        match self {
            Self::Lifecycle => 'A',
            Self::BatchValidation => 'B',
            Self::Arithmetic => 'C',
            Self::Integrity => 'D',
            Self::Reentrancy => 'E',
        }
    }

    /// Returns the human-readable name of the error category.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lifecycle => "Lifecycle & Availability",
            Self::BatchValidation => "Batch & Validation",
            Self::Arithmetic => "Arithmetic & Metering",
            Self::Integrity => "Storage & Integrity",
            Self::Reentrancy => "Re-entrancy & Concurrency",
        }
    }
}

/// Error codes for the archival-probe contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ProbeError {
    // -----------------------------------------------------------------------
    // Group A — Lifecycle & Availability (1, 9, 10)
    // -----------------------------------------------------------------------
    /// The canary or probe record has never been planted or has archived and requires restore.
    NotPlanted = 1,
    /// The entry has reached archival state and requires explicit restoration before access.
    EntryArchived = 9,
    /// Entry lifetime extension failed due to invalid threshold or target parameters.
    LifetimeExtensionFailed = 10,

    // -----------------------------------------------------------------------
    // Group B — Batch & Validation (2..=4)
    // -----------------------------------------------------------------------
    /// Batch operation was submitted empty.
    EmptyBatch = 2,
    /// Batch size exceeds maximum allowable limit or parameter vector lengths mismatch.
    BatchTooLarge = 3,
    /// Duplicate entry encountered in batch or entry already planted.
    DuplicateEntry = 4,

    // -----------------------------------------------------------------------
    // Group C — Arithmetic & Metering (5)
    // -----------------------------------------------------------------------
    /// Checked arithmetic overflow occurred in counter or index calculation.
    ArithmeticOverflow = 5,

    // -----------------------------------------------------------------------
    // Group D — Storage & Integrity (6..=7)
    // -----------------------------------------------------------------------
    /// Queried probe record contains corrupted note or invalid metadata.
    CorruptedRecord = 6,
    /// Formal invariant assertion failed during storage operation or state desync detected.
    InvariantViolation = 7,

    // -----------------------------------------------------------------------
    // Group E — Re-entrancy & Concurrency (8)
    // -----------------------------------------------------------------------
    /// Re-entrant call detected; another mutating probe execution is already in progress.
    ReentrancyGuard = 8,
}

impl ProbeError {
    /// Canonical human-readable description for each error variant.
    ///
    /// Useful for SDKs, off-chain client tooling, and monitoring systems to
    /// decode numeric host errors into clear descriptive diagnostics.
    pub const fn message(self) -> &'static str {
        match self {
            ProbeError::NotPlanted => {
                "canary or probe entry has not been planted or has archived and requires restoration"
            }
            ProbeError::EntryArchived => {
                "persistent probe entry has archived; restore footprint required prior to reading"
            }
            ProbeError::LifetimeExtensionFailed => {
                "entry lifetime extension failed due to invalid TTL thresholds or host limits"
            }
            ProbeError::EmptyBatch => "batch operation must contain at least one probe entry",
            ProbeError::BatchTooLarge => {
                "batch size exceeds MAX_BATCH_PROBE_SIZE or vector lengths mismatch"
            }
            ProbeError::DuplicateEntry => {
                "duplicate probe entry detected; write-once invariant prevents overwrite"
            }
            ProbeError::ArithmeticOverflow => {
                "checked arithmetic overflow or underflow occurred during operation"
            }
            ProbeError::CorruptedRecord => {
                "probe record storage entry is corrupted or fails integrity check"
            }
            ProbeError::InvariantViolation => {
                "formal post-condition invariant check failed; state desync detected"
            }
            ProbeError::ReentrancyGuard => {
                "re-entrant call detected; another mutating probe execution is already in progress"
            }
        }
    }

    /// Returns the semantic error category for this error variant.
    pub const fn category(self) -> ErrorCategory {
        match self {
            ProbeError::NotPlanted
            | ProbeError::EntryArchived
            | ProbeError::LifetimeExtensionFailed => ErrorCategory::Lifecycle,
            ProbeError::EmptyBatch | ProbeError::BatchTooLarge | ProbeError::DuplicateEntry => {
                ErrorCategory::BatchValidation
            }
            ProbeError::ArithmeticOverflow => ErrorCategory::Arithmetic,
            ProbeError::CorruptedRecord | ProbeError::InvariantViolation => {
                ErrorCategory::Integrity
            }
            ProbeError::ReentrancyGuard => ErrorCategory::Reentrancy,
        }
    }

    /// Returns the single character group letter ('A' through 'E') for coarse client branching.
    pub const fn group(self) -> char {
        self.category().code()
    }

    /// Map from numeric discriminant to `ProbeError` variant, if valid.
    pub const fn from_discriminant(discriminant: u32) -> Option<Self> {
        match discriminant {
            1 => Some(ProbeError::NotPlanted),
            2 => Some(ProbeError::EmptyBatch),
            3 => Some(ProbeError::BatchTooLarge),
            4 => Some(ProbeError::DuplicateEntry),
            5 => Some(ProbeError::ArithmeticOverflow),
            6 => Some(ProbeError::CorruptedRecord),
            7 => Some(ProbeError::InvariantViolation),
            8 => Some(ProbeError::ReentrancyGuard),
            9 => Some(ProbeError::EntryArchived),
            10 => Some(ProbeError::LifetimeExtensionFailed),
            _ => None,
        }
    }
}
