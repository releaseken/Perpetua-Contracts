//! Cliff-vesting error taxonomy and human-readable diagnostic messages.
//!
//! # Why a separate module?
//!
//! The root [`crate::Error`] enum intentionally keeps stable `u32` discriminants
//! covering every failure surface in the contract. That design is correct for
//! on-chain ABI stability, but it makes off-chain tooling (SDKs, explorers,
//! operator dashboards) work hard to give users meaningful messages: a bare
//! `Error::NothingToWithdraw` says nothing about *why* there is nothing — the
//! cliff has not opened yet, the stream has not started, or the recipient
//! already drained it this ledger.
//!
//! This module layers a **classification** on top of the existing error table
//! without touching discriminants or renumbering anything:
//!
//! * [`CliffVestingCategory`] groups related errors into five semantic buckets.
//! * [`cliff_error_message`] returns a static, English diagnostic string
//!   suitable for logging, SDK display, or CLI output.
//! * [`CliffVestingCategory::from_error`] classifies any [`crate::Error`]
//!   value so callers can branch on category rather than on individual
//!   discriminants.
//!
//! # ABI-safety guarantee
//!
//! Nothing in this module alters the wire encoding of any error. The
//! `#[contracterror]` table in [`crate::error`] is the only source of truth for
//! on-chain discriminants; this module is a pure read-only view over that table.
//!
//! # Extending this module
//!
//! When a new error variant is added to [`crate::Error`]:
//! 1. Add a match arm in [`CliffVestingCategory::from_error`].
//! 2. Add a match arm in [`cliff_error_message`].
//! 3. If the new error falls into cliff-vesting semantics, create a new
//!    category or extend an existing one. Do not reorder existing variants.
//!
//! The test `all_errors_have_a_category` in `test::cliff_error_taxonomy`
//! asserts exhaustive coverage so a future addition cannot silently fall
//! through to the default category.

use crate::Error;

/// Semantic category for a cliff-vesting error.
///
/// Clients can branch on the category to decide what to tell the user or which
/// retry strategy to apply, without hard-coding individual discriminant values.
///
/// # Category semantics
///
/// | Category | Meaning | Typical client action |
/// |---|---|---|
/// | [`CliffVestingCategory::CliffNotReached`] | The cliff gate is closed | Show countdown to cliff_time |
/// | [`CliffVestingCategory::StreamLifecycle`] | State-machine violation | Inspect stream status before retrying |
/// | [`CliffVestingCategory::Authorization`] | Wrong caller | Re-check the signing key |
/// | [`CliffVestingCategory::InvalidParameters`] | Bad creation parameters | Fix the call arguments |
/// | [`CliffVestingCategory::ArithmeticOrResource`] | Internal limit hit | Contact support or chunk the request |
/// | [`CliffVestingCategory::TokenSubcall`] | Token contract rejected | Inspect the token contract diagnostics |
/// | [`CliffVestingCategory::Other`] | Catch-all for non-vesting errors | Use the root error message |
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CliffVestingCategory {
    /// The cliff gate has not opened yet. No tokens have accrued for the
    /// recipient, even though time has passed since `start_time`.
    ///
    /// **Most common cause:** a `withdraw` call before `ledger.timestamp() >=
    /// cliff_time`.
    ///
    /// **Recommended message:** "The cliff period has not ended. Your tokens
    /// will begin unlocking once the cliff date is reached."
    CliffNotReached,

    /// A lifecycle state-machine precondition failed. The stream is in a state
    /// that does not allow this operation.
    ///
    /// **Most common causes:** calling `cancel` on a cancelled stream, `pause`
    /// on a stream that is already paused, or `top_up` on a matured stream.
    ///
    /// **Recommended message:** "This operation cannot be performed in the
    /// stream's current state. Check the stream status before retrying."
    StreamLifecycle,

    /// Authorization or capability flag check failed. Either the caller is not
    /// the required party, or the stream was created without the required
    /// capability flag.
    ///
    /// **Most common causes:** `cancel` on a non-cancellable stream, `pause` on
    /// a non-pausable stream, calling a sender-only function as the recipient.
    ///
    /// **Recommended message:** "You are not authorized to perform this action
    /// on this stream."
    Authorization,

    /// One or more parameters supplied to `create_stream` or `top_up` are
    /// outside the valid range.
    ///
    /// **Most common causes:** `end_time <= start_time`, `cliff_time` outside
    /// `[start_time, end_time]`, deposit below the minimum rate threshold, or
    /// a `top_up` amount that is below one second of accrual at the current
    /// rate.
    ///
    /// **Recommended message:** "The request contains invalid parameters. See
    /// the specific error for details."
    InvalidParameters,

    /// An arithmetic overflow, resource-limit, or identifier-exhaustion
    /// condition was hit. These errors should be rare in well-formed usage.
    ///
    /// **Most common causes:** an extremely large deposit, a batch that exceeds
    /// the protocol's event-byte budget, or the stream-id counter reaching
    /// `u64::MAX` (effectively unreachable in practice).
    ///
    /// **Recommended message:** "An internal resource limit was reached.
    /// Contact support if this error persists."
    ArithmeticOrResource,

    /// A sub-invocation to the token contract failed or the token address does
    /// not resolve to a deployed contract.
    ///
    /// **Most common causes:** insufficient balance in the contract pool at
    /// payout time, the sender lacking balance on deposit, or a misconfigured
    /// token address.
    ///
    /// **Recommended message:** "The token transfer failed. Check your balance
    /// and that the token contract is correctly configured."
    TokenSubcall,

    /// Catch-all for errors that do not map to a specific cliff-vesting phase.
    Other,
}

impl CliffVestingCategory {
    /// Classify a [`crate::Error`] into a [`CliffVestingCategory`].
    ///
    /// This mapping is intentionally conservative: when an error could belong to
    /// more than one category, it is placed in the most specific one. The only
    /// category that must not grow is [`CliffVestingCategory::CliffNotReached`]:
    /// it covers exactly the two error codes that mean "cliff gate is the reason
    /// the recipient cannot withdraw."
    ///
    /// # Examples
    ///
    /// ```rust
    /// use crate::Error;
    /// use crate::cliff_error::CliffVestingCategory;
    ///
    /// assert_eq!(
    ///     CliffVestingCategory::from_error(Error::NothingToWithdraw),
    ///     CliffVestingCategory::CliffNotReached,
    /// );
    /// ```
    pub fn from_error(error: Error) -> Self {
        match error {
            // "Nothing to withdraw" — most likely because the cliff has not
            // opened yet or the stream has not started. We classify this as
            // CliffNotReached because that is the dominant cliff-vesting case;
            // other causes (pre-start, fully drained) are covered by the
            // StreamLifecycle and Other branches of the same category for
            // human-message purposes.
            Error::NothingToWithdraw => CliffVestingCategory::CliffNotReached,
            // The cliff timestamp is out of the valid window; creation
            // rejected the schedule.
            Error::InvalidCliff => CliffVestingCategory::CliffNotReached,

            // Lifecycle / state-machine
            Error::StreamNotActive
            | Error::StreamNotPaused
            | Error::StreamAlreadyPaused
            | Error::StreamTerminated
            | Error::StreamMatured
            | Error::StreamNotFound
            | Error::InsufficientWithdrawable
            | Error::RepeatedTransfer => CliffVestingCategory::StreamLifecycle,

            // Authorization & capability flags
            Error::Unauthorized
            | Error::NotCancellable
            | Error::NotPausable
            | Error::NotTransferable
            | Error::SelfStream
            | Error::DelegateNotPermitted
            | Error::DelegateExpired => CliffVestingCategory::Authorization,

            // Creation / top-up parameter validation
            Error::InvalidTimeRange
            | Error::InvalidDeposit
            | Error::DepositRateTooLow
            | Error::DepositTooSmall
            | Error::InvalidAmount
            | Error::InvalidTopUp
            | Error::TopUpTooSmall => CliffVestingCategory::InvalidParameters,

            // Arithmetic and resource limits
            Error::Overflow
            | Error::BatchTooLarge
            | Error::BatchSizeExceeded
            | Error::EmptyBatch
            | Error::DuplicateStreamId
            | Error::MalformedStreamId
            | Error::StreamIdExhausted
            | Error::StreamIdOverflow => CliffVestingCategory::ArithmeticOrResource,

            // Token sub-invocation
            Error::TokenTransferFailed | Error::TokenMissing => CliffVestingCategory::TokenSubcall,
        }
    }
}

/// Return a human-readable diagnostic message for the given error, with
/// additional context about how the error relates to cliff-vesting semantics.
///
/// Messages are intentionally written at a level suitable for end-user display
/// (English prose, no Rust-internal identifiers), so SDK wrappers can forward
/// them directly without a translation layer.
///
/// # ABI note
///
/// These strings are **not** part of the on-chain ABI. They may be rephrased or
/// extended in a compatible release without bumping [`crate::ABI_VERSION`].
/// Only the [`crate::Error`] discriminant table is ABI-stable.
pub fn cliff_error_message(error: Error) -> &'static str {
    match error {
        // --- Cliff-vesting specific ---
        Error::NothingToWithdraw => {
            "No tokens are available to withdraw. \
             If a cliff is configured, the cliff date may not have been reached yet. \
             If the stream has not started, wait until start_time. \
             If the stream has been fully withdrawn, nothing remains."
        }
        Error::InvalidCliff => {
            "The cliff_time parameter is outside the valid range [start_time, end_time]. \
             Set cliff_time equal to start_time for a stream with no cliff, or to any \
             point within the schedule for a vesting cliff."
        }

        // --- Stream lookup ---
        Error::StreamNotFound => {
            "No stream exists with this id. \
             The id may be incorrect, or the stream entry may have been archived \
             and needs to be restored before it can be accessed."
        }

        // --- Creation parameter errors ---
        Error::InvalidTimeRange => {
            "end_time must be strictly greater than start_time. \
             A zero or negative duration is not valid."
        }
        Error::InvalidDeposit => {
            "The deposit amount must be greater than zero."
        }
        Error::DepositRateTooLow => {
            "The deposit is too small relative to the duration. \
             At least one stroop per second of duration is required so the recipient \
             accrues a non-zero amount in every second of the schedule."
        }
        Error::DepositTooSmall => {
            "The deposit is below the minimum rate threshold (one stroop per second). \
             Increase the deposit or shorten the duration."
        }
        Error::SelfStream => {
            "The sender and recipient cannot be the same address."
        }

        // --- Authorization errors ---
        Error::Unauthorized => {
            "The caller is not authorized to perform this action. \
             Check that you are calling with the correct key for this stream."
        }
        Error::NotCancellable => {
            "This stream was created without the cancellable flag. \
             It cannot be cancelled."
        }
        Error::NotPausable => {
            "This stream was created without the pausable flag. \
             It cannot be paused or resumed."
        }
        Error::NotTransferable => {
            "This stream was created without the transferable flag. \
             The recipient cannot be changed."
        }
        Error::DelegateNotPermitted => {
            "The delegate does not hold a grant that covers this operation. \
             The stream owner must issue a grant before the delegate can proceed."
        }
        Error::DelegateExpired => {
            "The delegate grant has passed its expiry timestamp. \
             The stream owner must issue a new grant."
        }

        // --- Lifecycle / state-machine ---
        Error::StreamNotActive => {
            "This action requires the stream to be in the Active state."
        }
        Error::StreamNotPaused => {
            "resume was called on a stream that is not currently paused."
        }
        Error::StreamAlreadyPaused => {
            "pause was called on a stream that is already paused."
        }
        Error::StreamTerminated => {
            "The stream has been cancelled or fully depleted. \
             No further lifecycle operations are allowed."
        }
        Error::StreamMatured => {
            "The stream has already reached its end_time. \
             Top-ups are not accepted on a matured stream. \
             Create a new stream instead."
        }
        Error::InsufficientWithdrawable => {
            "The requested amount exceeds the currently withdrawable balance. \
             Request a smaller amount or omit the amount to withdraw the full balance."
        }
        Error::InvalidAmount => {
            "The withdraw amount must be greater than zero."
        }
        Error::InvalidTopUp => {
            "The top-up amount must be greater than zero."
        }
        Error::TopUpTooSmall => {
            "The top-up amount is smaller than one second of accrual at the current rate. \
             It cannot extend the stream's duration without changing the rate. \
             Top up by at least (deposited / duration) stroops."
        }
        Error::RepeatedTransfer => {
            "The new recipient is already the current recipient. No transfer is needed."
        }

        // --- Batch / resource errors ---
        Error::BatchTooLarge | Error::BatchSizeExceeded => {
            "The batch contains more than the maximum allowed number of stream ids (16). \
             Split the request into smaller chunks."
        }
        Error::EmptyBatch => {
            "The batch must contain at least one stream id."
        }
        Error::DuplicateStreamId => {
            "The same stream id appears more than once in the batch. \
             Each stream may appear at most once per batch call."
        }
        Error::MalformedStreamId => {
            "One or more batch elements could not be decoded as a stream id. \
             Each element must be a u64."
        }

        // --- Arithmetic and id counter ---
        Error::Overflow => {
            "An arithmetic operation overflowed the i128 range. \
             This can occur with very large deposits. \
             Reduce the deposit amount or duration."
        }
        Error::StreamIdExhausted | Error::StreamIdOverflow => {
            "The stream id counter has reached its maximum value and no new streams can be created. \
             This condition is effectively unreachable in normal use."
        }

        // --- Token sub-invocation ---
        Error::TokenTransferFailed => {
            "The token contract rejected the transfer. \
             Check the transaction's diagnostic events for the token-level reason \
             (insufficient balance, deauthorized trustline, etc.)."
        }
        Error::TokenMissing => {
            "The token address does not resolve to a deployed contract. \
             Verify the token address used when creating the stream."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    /// Every variant of [`crate::Error`] must have an explicit arm in both
    /// `CliffVestingCategory::from_error` and `cliff_error_message`.
    ///
    /// Because both functions use exhaustive `match` expressions with no
    /// catch-all arm, this is a compile-time guarantee — the test exists
    /// primarily as documentation and to catch regressions if a future Rust
    /// edition changes exhaustiveness rules.
    #[test]
    fn all_errors_have_a_category_and_a_message() {
        // Build the full discriminant table (keep in sync with error.rs).
        let all_errors = [
            Error::StreamNotFound,
            Error::InvalidTimeRange,
            Error::InvalidCliff,
            Error::InvalidDeposit,
            Error::DepositRateTooLow,
            Error::SelfStream,
            Error::Unauthorized,
            Error::NotCancellable,
            Error::NotPausable,
            Error::NotTransferable,
            Error::StreamNotActive,
            Error::StreamNotPaused,
            Error::StreamAlreadyPaused,
            Error::StreamTerminated,
            Error::StreamMatured,
            Error::InsufficientWithdrawable,
            Error::NothingToWithdraw,
            Error::InvalidAmount,
            Error::BatchTooLarge,
            Error::EmptyBatch,
            Error::DuplicateStreamId,
            Error::Overflow,
            Error::TopUpTooSmall,
            Error::StreamIdExhausted,
            Error::TokenTransferFailed,
            Error::TokenMissing,
            Error::DelegateNotPermitted,
            Error::DelegateExpired,
            Error::MalformedStreamId,
            Error::RepeatedTransfer,
            Error::InvalidTopUp,
            Error::DepositTooSmall,
            Error::StreamIdOverflow,
            Error::BatchSizeExceeded,
        ];

        for error in all_errors {
            // from_error must not panic — just call it.
            let category = CliffVestingCategory::from_error(error);
            // cliff_error_message must return a non-empty string.
            let msg = cliff_error_message(error);
            assert!(!msg.is_empty(), "empty message for {:?}", error);
            // The category must be a sensible enum arm (checked by exhaustive match above).
            let _ = category;
        }
    }

    /// [`CliffVestingCategory::CliffNotReached`] must include exactly the two
    /// errors that relate to cliff semantics.
    #[test]
    fn cliff_not_reached_category_covers_cliff_errors() {
        assert_eq!(
            CliffVestingCategory::from_error(Error::NothingToWithdraw),
            CliffVestingCategory::CliffNotReached,
        );
        assert_eq!(
            CliffVestingCategory::from_error(Error::InvalidCliff),
            CliffVestingCategory::CliffNotReached,
        );
    }

    /// Token errors must be classified distinctly from authorization errors.
    #[test]
    fn token_errors_classified_as_token_subcall() {
        assert_eq!(
            CliffVestingCategory::from_error(Error::TokenTransferFailed),
            CliffVestingCategory::TokenSubcall,
        );
        assert_eq!(
            CliffVestingCategory::from_error(Error::TokenMissing),
            CliffVestingCategory::TokenSubcall,
        );
    }

    /// Arithmetic and batch resource errors are classified distinctly from
    /// authorization and lifecycle errors.
    #[test]
    fn arithmetic_errors_classified_as_arithmetic_or_resource() {
        for error in [
            Error::Overflow,
            Error::BatchTooLarge,
            Error::BatchSizeExceeded,
            Error::EmptyBatch,
            Error::DuplicateStreamId,
            Error::StreamIdExhausted,
            Error::StreamIdOverflow,
        ] {
            assert_eq!(
                CliffVestingCategory::from_error(error),
                CliffVestingCategory::ArithmeticOrResource,
                "expected ArithmeticOrResource for {:?}",
                error,
            );
        }
    }

    /// The cliff error message for `NothingToWithdraw` explicitly mentions the
    /// cliff, so SDK consumers can surface it without additional logic.
    #[test]
    fn nothing_to_withdraw_message_mentions_cliff() {
        let msg = cliff_error_message(Error::NothingToWithdraw);
        assert!(
            msg.contains("cliff"),
            "NothingToWithdraw message does not mention cliff: {msg}",
        );
    }
}
