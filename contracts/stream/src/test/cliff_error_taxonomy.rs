//! Integration tests for [`crate::cliff_error`]: error taxonomy and messages.
//!
//! The unit tests live inside `cliff_error.rs` itself (the module's `#[cfg(test)]`
//! block). These integration-level tests exercise the taxonomy from the
//! *contract* perspective — they drive real contract calls, let them fail with
//! typed errors, and assert that those errors classify correctly.

use super::common::*;
use crate::cliff_error::{cliff_error_message, CliffVestingCategory};
use crate::Error;

/// A `withdraw` call before the cliff fails with `NothingToWithdraw`, which
/// must classify as `CliffNotReached`.
#[test]
fn withdraw_before_cliff_gives_cliff_not_reached_category() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 90 * DAY;
    let id = h.create(
        1_200 * ONE,
        start,
        start + 360 * DAY,
        cliff,
        true,
        true,
        true,
    );

    h.warp_to(cliff - 1);

    let err = h
        .client
        .try_withdraw(&id, &None)
        .unwrap_err()
        .unwrap();

    assert_eq!(err, Error::NothingToWithdraw);
    assert_eq!(
        CliffVestingCategory::from_error(err),
        CliffVestingCategory::CliffNotReached,
        "pre-cliff NothingToWithdraw should classify as CliffNotReached",
    );
    let msg = cliff_error_message(err);
    assert!(msg.contains("cliff"), "message should mention cliff: {msg}");
}

/// `create_stream` with an invalid cliff classifies as `CliffNotReached`.
#[test]
fn invalid_cliff_at_creation_classifies_as_cliff_not_reached() {
    assert_eq!(
        CliffVestingCategory::from_error(Error::InvalidCliff),
        CliffVestingCategory::CliffNotReached,
    );
    let msg = cliff_error_message(Error::InvalidCliff);
    assert!(msg.contains("cliff_time"), "message should mention cliff_time: {msg}");
}

/// `cancel` on a non-cancellable stream classifies as `Authorization`.
#[test]
fn cancel_non_cancellable_classifies_as_authorization() {
    let h = Harness::new();
    let id = h.create(
        1_000 * ONE,
        h.now(),
        h.now() + 100 * DAY,
        h.now(),
        false, // not cancellable
        false,
        false,
    );

    let err = h.client.try_cancel(&id).unwrap_err().unwrap();
    assert_eq!(err, Error::NotCancellable);
    assert_eq!(
        CliffVestingCategory::from_error(err),
        CliffVestingCategory::Authorization,
    );
}

/// `StreamTerminated` classifies as `StreamLifecycle`.
#[test]
fn stream_terminated_classifies_as_lifecycle() {
    let h = Harness::new();
    let id = h.create(
        1_000 * ONE,
        h.now(),
        h.now() + 10 * DAY,
        h.now(),
        true,
        false,
        false,
    );
    h.advance(10 * DAY);
    h.client.withdraw(&id, &None);

    // Withdrawing from a depleted stream returns StreamTerminated.
    let err = h.client.try_withdraw(&id, &None).unwrap_err().unwrap();
    assert_eq!(err, Error::StreamTerminated);
    assert_eq!(
        CliffVestingCategory::from_error(err),
        CliffVestingCategory::StreamLifecycle,
    );
}

/// `BatchTooLarge` classifies as `ArithmeticOrResource`.
#[test]
fn batch_too_large_classifies_as_arithmetic_or_resource() {
    assert_eq!(
        CliffVestingCategory::from_error(Error::BatchTooLarge),
        CliffVestingCategory::ArithmeticOrResource,
    );
    let msg = cliff_error_message(Error::BatchTooLarge);
    assert!(!msg.is_empty());
}

/// Every error returned from a real contract call must have a non-empty message.
#[test]
fn every_observable_error_has_a_non_empty_message() {
    let errors_observed_in_contracts = [
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
        Error::TokenTransferFailed,
        Error::TokenMissing,
        Error::DelegateNotPermitted,
        Error::DelegateExpired,
        Error::MalformedStreamId,
        Error::RepeatedTransfer,
        Error::InvalidTopUp,
    ];

    for err in errors_observed_in_contracts {
        let msg = cliff_error_message(err);
        assert!(
            !msg.is_empty(),
            "empty message for {:?}",
            err,
        );
        // Every message must be sentence-like (at least 20 chars).
        assert!(
            msg.len() >= 20,
            "message too short for {:?}: {msg:?}",
            err,
        );
    }
}
