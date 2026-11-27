//! Error types and client observability for the stream-NFT contract.
//!
//! # Modular Error Taxonomy (Issue #371)
//!
//! Errors are categorized into explicit functional groups to enhance client
//! observability, debugging, and off-chain routing:
//!
//! | Group | Category | Discriminants | Description |
//! |:---|:---|:---|:---|
//! | **Group A** | Lifecycle / Init | `AlreadyInitialized (0)`, `NotInitialized (1)` | Contract initialization bounds |
//! | **Group B** | Token Entity | `AlreadyMinted (2)`, `TokenNotFound (3)` | Token state & existence |
//! | **Group C** | Authorization | `NotOwner (4)`, `Unauthorized (11)`, `InvalidRoleAddress (17)`, `NoPendingAdmin (18)` | Ownership, role, & auth gating |
//! | **Group D** | Cross-Contract | `StreamRejected (5)`, `StreamCallFailed (12)` | Invocations against stream contract |
//! | **Group E** | Batch / Protocol | `EmptyBatch (6)`, `BatchTooLarge (7)`, `DuplicateTokenId (8)`, `MalformedTokenId (9)`, `MintPaused (19)` | Batch parameter & protocol state validation |
//! | **Group F** | Arithmetic | `ArithmeticOverflow (10)`, `Overflow (16)` | Checked math overflows |
//! | **Group G** | Execution Safety | `ReentrancyGuard (13)`, `InvariantViolation (14)`, `RepeatedTransfer (15)` | Re-entrancy and invariant guards |
//!
//! Discriminants are part of the frozen public ABI. Never renumber existing variants.

use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    // --- Group A: Initialization / Lifecycle ---
    /// Contract is already initialized.
    AlreadyInitialized = 0,
    /// Contract is not yet initialized.
    NotInitialized = 1,

    // --- Group B: Token Existence & State ---
    /// The stream NFT has already been minted.
    AlreadyMinted = 2,
    /// The requested token ID does not exist.
    TokenNotFound = 3,

    // --- Group C: Authorization & Permissions ---
    /// Caller does not own the requested token ID.
    NotOwner = 4,

    // --- Group D: Cross-Contract Sub-Invocations ---
    /// The underlying stream contract refused the transfer (e.g. non-transferable or not recipient).
    StreamRejected = 5,

    // --- Group E: Batch Validation & Protocol Bounds ---
    /// Batch operation was supplied an empty array of token IDs.
    EmptyBatch = 6,
    /// Batch operation exceeded `MAX_BATCH_SIZE`.
    BatchTooLarge = 7,
    /// Duplicate token ID detected within the batch request.
    DuplicateTokenId = 8,
    /// Token ID argument is malformed or unconvertible.
    MalformedTokenId = 9,

    // --- Group F: Arithmetic & Numerical Safety ---
    /// Checked arithmetic addition or subtraction overflowed.
    ArithmeticOverflow = 10,

    // --- Additional Observability Variants (Issue #371) ---
    /// Caller is not authorized to perform the requested operation.
    Unauthorized = 11,
    /// Cross-contract invocation to stream contract failed to return or terminated abnormally.
    StreamCallFailed = 12,

    // --- Group G: Execution Safety (from origin/main stream-NFT hardening) ---
    /// A state-modifying operation was re-entered while the guard was held.
    ReentrancyGuard = 13,
    /// A core ownership or balance invariant was violated.
    InvariantViolation = 14,
    /// A self-transfer or repeated transfer was rejected.
    RepeatedTransfer = 15,
    /// Checked supply or balance arithmetic overflowed.
    Overflow = 16,
    /// A role, stream, or contract address is forbidden (zero address or self).
    InvalidRoleAddress = 17,
    /// No pending admin nomination exists to accept.
    NoPendingAdmin = 18,
    /// Minting is paused by the manager.
    MintPaused = 19,
}

impl Error {
    /// Return the semantic category group letter for client branching.
    #[inline(always)]
    pub const fn group(self) -> char {
        match self {
            Self::AlreadyInitialized | Self::NotInitialized => 'A',
            Self::AlreadyMinted | Self::TokenNotFound => 'B',
            Self::NotOwner
            | Self::Unauthorized
            | Self::InvalidRoleAddress
            | Self::NoPendingAdmin => 'C',
            Self::StreamRejected | Self::StreamCallFailed => 'D',
            Self::EmptyBatch
            | Self::BatchTooLarge
            | Self::DuplicateTokenId
            | Self::MalformedTokenId
            | Self::MintPaused => 'E',
            Self::ArithmeticOverflow | Self::Overflow => 'F',
            Self::ReentrancyGuard | Self::InvariantViolation | Self::RepeatedTransfer => 'G',
        }
    }

    /// Return a human-readable diagnostic message for client logging and UI display.
    pub const fn message(self) -> &'static str {
        match self {
            Self::AlreadyInitialized => "stream-NFT contract is already initialized",
            Self::NotInitialized => "stream-NFT contract has not been initialized",
            Self::AlreadyMinted => "stream token has already been minted into an NFT",
            Self::TokenNotFound => "stream NFT token does not exist",
            Self::NotOwner => "caller is not the owner of the requested token",
            Self::StreamRejected => {
                "underlying stream contract rejected claim transfer (not transferable or unauthorized)"
            }
            Self::EmptyBatch => "batch request cannot be empty",
            Self::BatchTooLarge => "batch size exceeds MAX_BATCH_SIZE (16)",
            Self::DuplicateTokenId => "duplicate token ID detected in batch request",
            Self::MalformedTokenId => "token ID is malformed or invalid",
            Self::ArithmeticOverflow => "checked arithmetic overflow or underflow occurred",
            Self::Unauthorized => "caller is not authorized for this operation",
            Self::StreamCallFailed => "cross-contract invocation to stream contract failed",
            Self::ReentrancyGuard => "re-entrant state-modifying invocation was rejected",
            Self::InvariantViolation => "stream-NFT ownership or balance invariant was violated",
            Self::RepeatedTransfer => "self-transfer or repeated transfer was rejected",
            Self::Overflow => "checked supply or balance arithmetic overflowed",
            Self::InvalidRoleAddress => "role or contract address is forbidden (zero or self)",
            Self::NoPendingAdmin => "no pending admin nomination exists to accept",
            Self::MintPaused => "minting is currently paused by the manager",
        }
    }

    /// Return true if the error originated from an external cross-contract invocation.
    #[inline(always)]
    pub const fn is_cross_contract(self) -> bool {
        matches!(self, Self::StreamRejected | Self::StreamCallFailed)
    }

    /// Return true if the error is due to bad client input arguments.
    #[inline(always)]
    pub const fn is_client_validation(self) -> bool {
        matches!(
            self,
            Self::EmptyBatch
                | Self::BatchTooLarge
                | Self::DuplicateTokenId
                | Self::MalformedTokenId
                | Self::TokenNotFound
                | Self::NotOwner
                | Self::InvalidRoleAddress
            )
    }
}
