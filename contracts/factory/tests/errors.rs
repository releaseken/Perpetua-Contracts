//! Tests for modular error types and custom error messages in factory (issue #362).
//!
//! Verifies that:
//! 1. Each error variant has a unique, non-empty `message()`.
//! 2. Each error variant maps to the expected `ErrorCategory` and group letter ('A' - 'E').
//! 3. Numeric discriminants map bi-directionally to variants via `from_discriminant`.
//! 4. The contract view `error_message` returns canonical messages on-chain.
//! 5. The contract view `error_group` returns correct group byte on-chain.
//! 6. Fuzz / property testing with `proptest` ensures invariant robustness across arbitrary discriminants.

#![cfg(test)]

use fluxora_factory::{error::ErrorCategory, FactoryError, FluxoraFactory, FluxoraFactoryClient};
use proptest::prelude::*;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, IntoVal, String, Symbol};

fn stream_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[9; 32])
}

fn setup_factory(env: &Env) -> (FluxoraFactoryClient<'static>, Address, Address) {
    let fid = env.register(FluxoraFactory, ());
    let client = FluxoraFactoryClient::new(env, &fid);
    let admin = Address::generate(env);
    let stream_contract = Address::generate(env);
    client.init(
        &admin,
        &stream_contract,
        &stream_hash(env),
        &1_000_000,
        &100,
    );
    (client, admin, stream_contract)
}

fn sdk_string_to_rust(s: String) -> std::string::String {
    let len = s.len() as usize;
    let mut buf = std::vec![0u8; len];
    s.copy_into_slice(&mut buf);
    std::string::String::from_utf8(buf).expect("soroban String must be valid UTF-8")
}

const ALL_VARIANTS: [FactoryError; 21] = [
    FactoryError::NotInitialized,
    FactoryError::AlreadyInitialized,
    FactoryError::Unauthorized,
    FactoryError::AllowlistDenied,
    FactoryError::InvalidNewAdmin,
    FactoryError::NoPendingAdmin,
    FactoryError::InvalidDuration,
    FactoryError::TokenNotAllowed,
    FactoryError::RateLimitExceeded,
    FactoryError::InvalidRateLimit,
    FactoryError::CapacityCapExceeded,
    FactoryError::ActiveStreamCountUnderflow,
    FactoryError::CreationPaused,
    FactoryError::DepositCapExceeded,
    FactoryError::InvalidRateBounds,
    FactoryError::RateBelowMinimum,
    FactoryError::RateAboveMaximum,
    FactoryError::InvalidDeposit,
    FactoryError::InvalidCliff,
    FactoryError::ReentrancyGuard,
    FactoryError::InvariantViolation,
];

// ---------------------------------------------------------------------------
// 1. Error message table: completeness, non-emptiness, and uniqueness
// ---------------------------------------------------------------------------

#[test]
fn all_factory_error_messages_are_non_empty() {
    for &err in &ALL_VARIANTS {
        let msg = err.message();
        assert!(
            !msg.is_empty(),
            "FactoryError::{:?} message must not be empty",
            err
        );
        assert!(
            msg.len() > 10,
            "FactoryError::{:?} message is too short to be descriptive: '{}'",
            err,
            msg
        );
    }
}

#[test]
fn all_factory_error_messages_are_unique() {
    let mut seen = std::collections::HashSet::new();
    for &err in &ALL_VARIANTS {
        let msg = err.message();
        assert!(
            seen.insert(msg),
            "Duplicate error message detected for FactoryError::{:?}: '{}'",
            err,
            msg
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Error taxonomy and grouping correctness
// ---------------------------------------------------------------------------

#[test]
fn factory_error_categories_and_groups_match_specification() {
    // Group A — Lifecycle
    assert_eq!(
        FactoryError::NotInitialized.category(),
        ErrorCategory::Lifecycle
    );
    assert_eq!(FactoryError::NotInitialized.group(), 'A');
    assert_eq!(
        FactoryError::AlreadyInitialized.category(),
        ErrorCategory::Lifecycle
    );
    assert_eq!(FactoryError::AlreadyInitialized.group(), 'A');

    // Group B — Authorization & Governance
    assert_eq!(
        FactoryError::Unauthorized.category(),
        ErrorCategory::Authorization
    );
    assert_eq!(FactoryError::Unauthorized.group(), 'B');
    assert_eq!(
        FactoryError::InvalidNewAdmin.category(),
        ErrorCategory::Authorization
    );
    assert_eq!(FactoryError::InvalidNewAdmin.group(), 'B');
    assert_eq!(
        FactoryError::NoPendingAdmin.category(),
        ErrorCategory::Authorization
    );
    assert_eq!(FactoryError::NoPendingAdmin.group(), 'B');

    // Group C — Parameter & Policy Validation
    assert_eq!(
        FactoryError::AllowlistDenied.category(),
        ErrorCategory::Validation
    );
    assert_eq!(FactoryError::AllowlistDenied.group(), 'C');
    assert_eq!(
        FactoryError::InvalidDuration.category(),
        ErrorCategory::Validation
    );
    assert_eq!(FactoryError::InvalidDuration.group(), 'C');
    assert_eq!(
        FactoryError::TokenNotAllowed.category(),
        ErrorCategory::Validation
    );
    assert_eq!(FactoryError::TokenNotAllowed.group(), 'C');
    assert_eq!(
        FactoryError::InvalidRateLimit.category(),
        ErrorCategory::Validation
    );
    assert_eq!(FactoryError::InvalidRateLimit.group(), 'C');
    assert_eq!(
        FactoryError::InvalidRateBounds.category(),
        ErrorCategory::Validation
    );
    assert_eq!(FactoryError::InvalidRateBounds.group(), 'C');
    assert_eq!(
        FactoryError::InvalidDeposit.category(),
        ErrorCategory::Validation
    );
    assert_eq!(FactoryError::InvalidDeposit.group(), 'C');
    assert_eq!(
        FactoryError::InvalidCliff.category(),
        ErrorCategory::Validation
    );
    assert_eq!(FactoryError::InvalidCliff.group(), 'C');

    // Group D — Rate & Capacity Bounds
    assert_eq!(
        FactoryError::RateLimitExceeded.category(),
        ErrorCategory::CapacityAndRates
    );
    assert_eq!(FactoryError::RateLimitExceeded.group(), 'D');
    assert_eq!(
        FactoryError::CapacityCapExceeded.category(),
        ErrorCategory::CapacityAndRates
    );
    assert_eq!(FactoryError::CapacityCapExceeded.group(), 'D');
    assert_eq!(
        FactoryError::DepositCapExceeded.category(),
        ErrorCategory::CapacityAndRates
    );
    assert_eq!(FactoryError::DepositCapExceeded.group(), 'D');
    assert_eq!(
        FactoryError::RateBelowMinimum.category(),
        ErrorCategory::CapacityAndRates
    );
    assert_eq!(FactoryError::RateBelowMinimum.group(), 'D');
    assert_eq!(
        FactoryError::RateAboveMaximum.category(),
        ErrorCategory::CapacityAndRates
    );
    assert_eq!(FactoryError::RateAboveMaximum.group(), 'D');

    // Group E — Accounting & Safety
    assert_eq!(
        FactoryError::ActiveStreamCountUnderflow.category(),
        ErrorCategory::AccountingAndSafety
    );
    assert_eq!(FactoryError::ActiveStreamCountUnderflow.group(), 'E');
    assert_eq!(
        FactoryError::CreationPaused.category(),
        ErrorCategory::AccountingAndSafety
    );
    assert_eq!(FactoryError::CreationPaused.group(), 'E');
    assert_eq!(
        FactoryError::ReentrancyGuard.category(),
        ErrorCategory::AccountingAndSafety
    );
    assert_eq!(FactoryError::ReentrancyGuard.group(), 'E');
    assert_eq!(
        FactoryError::InvariantViolation.category(),
        ErrorCategory::AccountingAndSafety
    );
    assert_eq!(FactoryError::InvariantViolation.group(), 'E');
}

#[test]
fn error_category_metadata() {
    assert_eq!(ErrorCategory::Lifecycle.name(), "Lifecycle");
    assert_eq!(
        ErrorCategory::Authorization.name(),
        "Authorization & Governance"
    );
    assert_eq!(
        ErrorCategory::Validation.name(),
        "Parameter & Policy Validation"
    );
    assert_eq!(
        ErrorCategory::CapacityAndRates.name(),
        "Rate & Capacity Bounds"
    );
    assert_eq!(
        ErrorCategory::AccountingAndSafety.name(),
        "Accounting & Safety"
    );

    assert_eq!(ErrorCategory::Lifecycle.code(), 'A');
    assert_eq!(ErrorCategory::Authorization.code(), 'B');
    assert_eq!(ErrorCategory::Validation.code(), 'C');
    assert_eq!(ErrorCategory::CapacityAndRates.code(), 'D');
    assert_eq!(ErrorCategory::AccountingAndSafety.code(), 'E');
}

// ---------------------------------------------------------------------------
// 3. Discriminant bidirectional stability
// ---------------------------------------------------------------------------

#[test]
fn discriminant_round_trip() {
    for &err in &ALL_VARIANTS {
        let disc = err as u32;
        let mapped = FactoryError::from_discriminant(disc);
        assert_eq!(
            mapped,
            Some(err),
            "Discriminant {} did not round-trip to {:?}",
            disc,
            err
        );
    }

    // Invalid discriminants return None
    assert_eq!(FactoryError::from_discriminant(0), None);
    assert_eq!(FactoryError::from_discriminant(22), None);
    assert_eq!(FactoryError::from_discriminant(999), None);
}

// ---------------------------------------------------------------------------
// 4. On-chain client views: error_message and error_group
// ---------------------------------------------------------------------------

#[test]
fn test_error_message_and_group_views() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _, _) = setup_factory(&env);

    for &err in &ALL_VARIANTS {
        let disc = err as u32;
        let msg = client.error_message(&disc);
        let msg_str = sdk_string_to_rust(msg);
        assert_eq!(msg_str, err.message(), "error_message({}) failed", disc);

        let group_code = client.error_group(&disc);
        assert_eq!(
            group_code,
            err.group() as u32,
            "error_group({}) failed",
            disc
        );
    }

    // Unregistered discriminant views
    let unknown_msg = client.error_message(&999);
    assert_eq!(sdk_string_to_rust(unknown_msg), "");

    let unknown_group = client.error_group(&999);
    assert_eq!(unknown_group, 0);
}

// ---------------------------------------------------------------------------
// 5. Fuzz / Property tests with proptest
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_discriminant_mapping_consistency(discriminant in 0u32..1000u32) {
        match FactoryError::from_discriminant(discriminant) {
            Some(err) => {
                prop_assert_eq!(err as u32, discriminant);
                prop_assert!(!err.message().is_empty());
                let group = err.group();
                prop_assert!(group >= 'A' && group <= 'E');
            }
            None => {
                // Must be outside valid discriminant set: 1..=21
                prop_assert!(discriminant == 0 || discriminant > 21);
            }
        }
    }

    #[test]
    fn prop_error_group_contract_view(discriminant in 0u32..500u32) {
        let env = Env::default();
        let (client, _, _) = setup_factory(&env);

        let view_group = client.error_group(&discriminant);
        if let Some(err) = FactoryError::from_discriminant(discriminant) {
            prop_assert_eq!(view_group, err.group() as u32);
        } else {
            prop_assert_eq!(view_group, 0);
        }
    }
}
