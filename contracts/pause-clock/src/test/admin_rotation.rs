//! Two-step and single-step admin rotation tests.

use soroban_sdk::Address;

use super::common::Harness;

#[test]
fn single_step_set_admin_works() {
    let h = Harness::new();
    let new_admin = Address::generate(&h.env);
    h.client.set_admin(&new_admin);
    let config = h.client.get_config().unwrap();
    assert_eq!(config.admin, new_admin);
}

#[test]
fn set_admin_rejects_zero_address() {
    let h = Harness::new();
    let zero = Address::from_string(
        &h.env,
        &soroban_sdk::String::from_str(
            &h.env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        ),
    );
    let result = h.client.try_set_admin(&zero);
    assert!(result.is_err(), "zero address must be rejected");
}

#[test]
fn set_admin_rejects_contract_itself() {
    let h = Harness::new();
    let result = h.client.try_set_admin(&h.contract_id);
    assert!(result.is_err(), "contract's own address must be rejected");
}

#[test]
fn two_step_rotation_completes() {
    let h = Harness::new();
    let nominee = Address::generate(&h.env);

    // Step 1: current admin proposes.
    h.client.propose_admin(&nominee);
    let pending = h.client.pending_admin().unwrap();
    assert_eq!(pending, Some(nominee.clone()));

    // Step 2: nominee accepts.
    h.client.accept_admin();
    let config = h.client.get_config().unwrap();
    assert_eq!(config.admin, nominee, "nominee must become admin");

    // Pending entry must be cleared.
    let pending_after = h.client.pending_admin().unwrap();
    assert!(pending_after.is_none(), "pending admin must be cleared after accept");
}

#[test]
fn single_step_clears_pending_proposal() {
    let h = Harness::new();
    let nominee = Address::generate(&h.env);
    let replacement = Address::generate(&h.env);

    h.client.propose_admin(&nominee);
    // A single-step rotation supersedes the proposal.
    h.client.set_admin(&replacement);
    let pending = h.client.pending_admin().unwrap();
    assert!(pending.is_none(), "proposal must be cleared by single-step rotation");
}

#[test]
fn accept_admin_without_proposal_returns_error() {
    let h = Harness::new();
    let result = h.client.try_accept_admin();
    assert!(result.is_err());
}
