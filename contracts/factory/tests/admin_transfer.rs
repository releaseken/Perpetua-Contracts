//! Two-step admin rotation tests for `FluxoraFactory` (issue #39).
//!
//! Covers `propose_admin` / `accept_admin` and the `pending_admin` view:
//! happy-path hand-off, immediate revocation of the previous admin, burn
//! protection (zero address and self), `NoPendingAdmin` on a stray accept, and
//! auth enforcement on both steps.

#![cfg(test)]

use fluxora_factory::{FactoryError, FluxoraFactory, FluxoraFactoryClient};
use soroban_sdk::testutils::{Address as _, Events, MockAuth, MockAuthInvoke};
use soroban_sdk::{Address, Env, IntoVal, String, Symbol, TryFromVal, Val};
use std::panic::AssertUnwindSafe;

/// The all-zeros Stellar account — the "burn" sentinel the factory refuses.
const ZERO_ACCOUNT: &str = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";

fn zero_address(env: &Env) -> Address {
    Address::from_string(&String::from_str(env, ZERO_ACCOUNT))
}

/// Fresh initialised factory. `mock_all_auths` covers the `init` call.
fn setup() -> (
    Env,
    Address,
    FluxoraFactoryClient<'static>,
    Address,
    Address,
) {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register(FluxoraFactory, ());
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    factory.init(&admin, &sc, &10_000, &100);
    (env, fid, factory, admin, sc)
}

/// True if the contract has emitted an event whose first topic is `topic`.
fn has_event_with_topic(env: &Env, contract_id: &Address, topic: &str) -> bool {
    use soroban_sdk::xdr::ContractEventBody;
    env.events()
        .all()
        .filter_by_contract(contract_id)
        .events()
        .iter()
        .any(|event| {
            let ContractEventBody::V0(body) = &event.body;
            body.topics
                .first()
                .map(|t| {
                    let val = Val::try_from_val(env, t).expect("topic converts");
                    Symbol::try_from_val(env, &val).expect("topic is a symbol")
                })
                .is_some_and(|s| s.to_string().as_str() == topic)
        })
}

fn assert_auth_fails<F: FnOnce()>(f: F) {
    let result = std::panic::catch_unwind(AssertUnwindSafe(f));
    assert!(
        result.is_err(),
        "expected auth failure (panic) but call succeeded"
    );
}

// ---------------------------------------------------------------------------
// propose_admin / accept_admin — happy path
// ---------------------------------------------------------------------------

/// Nomination does not change the admin; acceptance completes the transfer and
/// consumes the nomination.
#[test]
fn test_two_step_transfer_completes_on_accept() {
    let (_env, _fid, factory, admin, new_admin, _sc) = setup_with_new();
    assert_eq!(factory.get_factory_config().admin, admin);

    factory.propose_admin(&new_admin);
    assert_eq!(factory.pending_admin(), Some(new_admin.clone()));
    // Admin unchanged until acceptance.
    assert_eq!(factory.get_factory_config().admin, admin);

    factory.accept_admin();

    assert_eq!(factory.get_factory_config().admin, new_admin);
    assert_eq!(factory.pending_admin(), None);
}

/// The previous admin is revoked the instant the transfer is accepted: a
/// setter authorised only by the old admin must now fail, while the new admin
/// can call setters.
#[test]
fn test_previous_admin_revoked_immediately_on_accept() {
    let (env, fid, factory, admin, new_admin, _sc) = setup_with_new();

    factory.propose_admin(&new_admin);
    factory.accept_admin();

    // Old admin alone can no longer set policy.
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_cap",
            args: (5_000i128,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert_auth_fails(|| factory.set_cap(&5_000));

    // New admin can.
    env.mock_auths(&[MockAuth {
        address: &new_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_cap",
            args: (5_000i128,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    factory.set_cap(&5_000);
    assert_eq!(factory.get_factory_config().max_deposit, 5_000);
}

/// The transferred-in admin can in turn rotate onward via the same two-step
/// path — the rotation is not a one-shot capability.
#[test]
fn test_new_admin_can_rotate_further() {
    let (env, _fid, factory, _admin, new_admin, _sc) = setup_with_new();
    factory.propose_admin(&new_admin);
    factory.accept_admin();

    let third = Address::generate(&env);
    factory.propose_admin(&third);
    assert_eq!(factory.pending_admin(), Some(third.clone()));
    factory.accept_admin();
    assert_eq!(factory.get_factory_config().admin, third);
}

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

/// The zero account must be rejected as the proposed admin.
#[test]
fn test_propose_admin_rejects_zero_address() {
    let (env, _fid, factory, _admin, _new_admin, _sc) = setup_with_new();

    let result = factory.try_propose_admin(&zero_address(&env));
    assert_eq!(result, Err(Ok(FactoryError::InvalidNewAdmin)));
    assert_eq!(factory.pending_admin(), None);
}

/// The factory's own contract address must be rejected as the proposed admin.
#[test]
fn test_propose_admin_rejects_self_address() {
    let (_env, fid, factory, _admin, _new_admin, _sc) = setup_with_new();

    let result = factory.try_propose_admin(&fid);
    assert_eq!(result, Err(Ok(FactoryError::InvalidNewAdmin)));
    assert_eq!(factory.pending_admin(), None);
}

/// The single-step legacy path gets the same burn protection.
#[test]
fn test_set_admin_rejects_zero_and_self() {
    let (env, fid, factory, _admin, _new_admin, _sc) = setup_with_new();

    assert_eq!(
        factory.try_set_admin(&zero_address(&env)),
        Err(Ok(FactoryError::InvalidNewAdmin))
    );
    assert_eq!(
        factory.try_set_admin(&fid),
        Err(Ok(FactoryError::InvalidNewAdmin))
    );
}

/// Accepting a transfer that was never proposed is a typed error.
#[test]
fn test_accept_admin_without_pending_errors() {
    let (_env, _fid, factory, _admin, _new_admin, _sc) = setup_with_new();

    let result = factory.try_accept_admin();
    assert_eq!(result, Err(Ok(FactoryError::NoPendingAdmin)));
}

/// An address that is not the pending admin cannot complete the transfer.
#[test]
fn test_accept_admin_rejects_non_pending_caller() {
    let (env, fid, factory, _admin, new_admin, _sc) = setup_with_new();
    factory.propose_admin(&new_admin);

    let stranger = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "accept_admin",
            args: ().into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert_auth_fails(|| factory.accept_admin());
    // Admin and pending nomination unchanged.
    assert_eq!(factory.pending_admin(), Some(new_admin));
}

/// A re-nomination while a transfer is pending replaces the pending address.
#[test]
fn test_repropose_overwrites_pending() {
    let (env, _fid, factory, _admin, new_admin, _sc) = setup_with_new();
    factory.propose_admin(&new_admin);

    let second = Address::generate(&env);
    factory.propose_admin(&second);
    assert_eq!(factory.pending_admin(), Some(second.clone()));

    factory.accept_admin();
    assert_eq!(factory.get_factory_config().admin, second);
}

/// A single-step `set_admin` while a transfer is pending discards the stale
/// nomination so a later `accept_admin` cannot resurrect it.
#[test]
fn test_set_admin_clears_pending_nomination() {
    let (env, _fid, factory, _admin, new_admin, _sc) = setup_with_new();
    factory.propose_admin(&new_admin);

    // The current admin rotates directly, superseding the pending hand-off.
    let direct = Address::generate(&env);
    factory.set_admin(&direct);
    assert_eq!(factory.pending_admin(), None);
    assert_eq!(factory.get_factory_config().admin, direct);
}

/// `propose_admin` before `init` reports `NotInitialized`, consistent with the
/// other setters.
#[test]
fn test_propose_admin_before_init_errors() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register(FluxoraFactory, ());
    let factory = FluxoraFactoryClient::new(&env, &fid);

    let stranger = Address::generate(&env);
    let result = factory.try_propose_admin(&stranger);
    assert_eq!(result, Err(Ok(FactoryError::NotInitialized)));
}

// ---------------------------------------------------------------------------
// Auth enforcement
// ---------------------------------------------------------------------------

/// Only the current admin can nominate a successor.
#[test]
fn test_propose_admin_rejects_non_admin() {
    let (env, fid, factory, _admin, new_admin, _sc) = setup_with_new();
    let non_admin = Address::generate(&env);

    env.mock_auths(&[MockAuth {
        address: &non_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "propose_admin",
            args: (&new_admin,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert_auth_fails(|| factory.propose_admin(&new_admin));
    assert_eq!(factory.pending_admin(), None);
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Both halves of the hand-off are observable: `admin_transfer_proposed` then
/// `admin_transferred`.
#[test]
fn test_transfer_emits_proposed_and_transferred_events() {
    let (env, fid, factory, _admin, new_admin, _sc) = setup_with_new();

    factory.propose_admin(&new_admin);
    assert!(has_event_with_topic(&env, &fid, "admin_transfer_proposed"));

    factory.accept_admin();
    assert!(has_event_with_topic(&env, &fid, "admin_transferred"));
}

/// Convenience: same tuple as `setup()` plus a fresh successor address.
fn setup_with_new() -> (
    Env,
    Address,
    FluxoraFactoryClient<'static>,
    Address,
    Address,
    Address,
) {
    let (env, fid, factory, admin, sc) = setup();
    let new_admin = Address::generate(&env);
    (env, fid, factory, admin, new_admin, sc)
}
