//! Granular role-based authorization, admin rotation, and modular error tests
//! for `fee-accrual` (issue #385, #389).

use soroban_sdk::testutils::Address as _;
use soroban_sdk::Address;

use crate::FeeError;

use super::common::Harness;

#[test]
fn double_init_returns_already_initialized() {
    let h = Harness::new();
    let err = h
        .client
        .try_init(
            &h.admin,
            &h.fee_admin,
            &h.fee_collector,
            &h.emergency_admin,
            &h.fee_recipient,
            &100,
            &1_000,
        )
        .unwrap_err()
        .unwrap();
    assert_eq!(err, FeeError::AlreadyInitialized);
}

#[test]
fn role_isolation_enforced_across_all_subsystems() {
    let h = Harness::new();

    // Only fee_collector can accrue fees.
    assert_eq!(
        h.client
            .try_accrue_fee(&h.fee_admin, &1_u64, &10_000_i128)
            .unwrap_err()
            .unwrap(),
        FeeError::UnauthorizedRole
    );
    assert_eq!(
        h.client
            .try_accrue_fee(&h.other, &1_u64, &10_000_i128)
            .unwrap_err()
            .unwrap(),
        FeeError::UnauthorizedRole
    );

    // Only fee_admin can update fee parameters.
    assert_eq!(
        h.client
            .try_update_fee_config(&h.fee_collector, &h.fee_recipient, &150, &2_000)
            .unwrap_err()
            .unwrap(),
        FeeError::UnauthorizedRole
    );

    // Only emergency_admin or admin can pause.
    assert_eq!(
        h.client
            .try_pause(&h.fee_collector)
            .unwrap_err()
            .unwrap(),
        FeeError::UnauthorizedRole
    );

    // Only admin can rotate role holders.
    let replacement = Address::generate(&h.env);
    assert_eq!(
        h.client
            .try_set_fee_admin(&h.fee_admin, &replacement)
            .unwrap_err()
            .unwrap(),
        FeeError::Unauthorized
    );
    assert_eq!(
        h.client
            .try_set_fee_collector(&h.fee_admin, &replacement)
            .unwrap_err()
            .unwrap(),
        FeeError::Unauthorized
    );
    assert_eq!(
        h.client
            .try_set_emergency_admin(&h.fee_admin, &replacement)
            .unwrap_err()
            .unwrap(),
        FeeError::Unauthorized
    );
}

#[test]
fn admin_can_rotate_roles_and_rejects_forbidden_addresses() {
    let h = Harness::new();
    let new_fee_admin = Address::generate(&h.env);
    let new_collector = Address::generate(&h.env);
    let new_emergency = Address::generate(&h.env);

    h.client.set_fee_admin(&h.admin, &new_fee_admin);
    h.client.set_fee_collector(&h.admin, &new_collector);
    h.client.set_emergency_admin(&h.admin, &new_emergency);

    let cfg = h.client.get_config();
    assert_eq!(cfg.fee_admin, new_fee_admin);
    assert_eq!(cfg.fee_collector, new_collector);
    assert_eq!(cfg.emergency_admin, new_emergency);

    // Zero address and contract self address are forbidden for roles.
    let zero = h.zero_address();
    assert_eq!(
        h.client
            .try_set_fee_admin(&h.admin, &zero)
            .unwrap_err()
            .unwrap(),
        FeeError::InvalidRoleAddress
    );
    assert_eq!(
        h.client
            .try_set_fee_collector(&h.admin, &h.contract_id)
            .unwrap_err()
            .unwrap(),
        FeeError::InvalidRoleAddress
    );
}

#[test]
fn two_step_and_single_step_admin_rotation_work() {
    let h = Harness::new();
    let nominee = Address::generate(&h.env);

    h.client.propose_admin(&h.admin, &nominee);
    assert_eq!(h.client.pending_admin(), Some(nominee.clone()));

    h.client.accept_admin();
    assert_eq!(h.client.get_config().admin, nominee);
    assert_eq!(h.client.pending_admin(), None);

    // Single-step rotation clears any pending proposal.
    let pending_candidate = Address::generate(&h.env);
    let direct_admin = Address::generate(&h.env);
    h.client.propose_admin(&nominee, &pending_candidate);
    h.client.set_admin(&nominee, &direct_admin);
    assert_eq!(h.client.get_config().admin, direct_admin);
    assert_eq!(h.client.pending_admin(), None);
}

#[test]
fn error_messages_and_groups_cover_all_discriminants() {
    let h = Harness::new();
    let variants = [
        (1, FeeError::NotInitialized, 'A'),
        (2, FeeError::AlreadyInitialized, 'A'),
        (3, FeeError::Unauthorized, 'B'),
        (4, FeeError::CallerNotAuthorized, 'B'),
        (5, FeeError::NoPendingAdmin, 'B'),
        (6, FeeError::InvalidNewAdmin, 'B'),
        (7, FeeError::InvalidBpsRate, 'C'),
        (8, FeeError::InvalidFeeRecipient, 'C'),
        (9, FeeError::InvalidStreamId, 'C'),
        (10, FeeError::InvalidTokenAddress, 'C'),
        (11, FeeError::ZeroFeeAccrual, 'D'),
        (12, FeeError::InsufficientAccruedFees, 'D'),
        (13, FeeError::FeeRecordNotFound, 'D'),
        (14, FeeError::FeeAlreadySettled, 'D'),
        (15, FeeError::ArithmeticOverflow, 'E'),
        (16, FeeError::EmptyBatch, 'E'),
        (17, FeeError::BatchTooLarge, 'E'),
        (18, FeeError::InvariantViolation, 'E'),
        (19, FeeError::UnauthorizedRole, 'B'),
        (20, FeeError::InvalidRoleAddress, 'C'),
        (21, FeeError::ContractPaused, 'D'),
        (22, FeeError::ContractNotPaused, 'D'),
    ];

    for (code, err, expected_group) in variants {
        assert!(!err.message().is_empty());
        assert_eq!(err.group(), expected_group);
        let on_chain = h.client.error_message(&code);
        assert_eq!(on_chain.len() as usize, err.message().len());
    }
    assert_eq!(h.client.error_message(&999).len(), 0);
}
