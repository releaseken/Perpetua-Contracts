//! Direct unit tests for `FluxoraFactory` admin setters and config views.
//!
//! Covers issue #684: `set_admin`, `set_cap`, `set_min_duration`, `set_allowlist`,
//! token allowlisting, `is_allowlisted`, and `get_factory_config` — including auth enforcement,
//! `NotInitialized` / `AlreadyInitialized` branches, and admin rotation.

#![cfg(test)]

use fluxora_factory::{
    derive_stream_salt, load_policy, FactoryError, FactoryPolicy, FluxoraFactory,
    FluxoraFactoryClient,
};
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke},
    Address, BytesN, Env, IntoVal,
};
use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;

fn stream_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[7; 32])
}

#[test]
fn test_stream_salt_is_unique_across_ten_thousand_creations() {
    let env = Env::default();
    let sender = Address::generate(&env);
    let mut salts = BTreeSet::new();

    for stream_id in 0..10_000u64 {
        let salt = derive_stream_salt(&env, &sender, 0, stream_id);
        assert!(salts.insert(salt.to_array()), "salt collision at {stream_id}");
    }
}

#[test]
fn test_stream_salt_changes_when_any_tuple_component_changes() {
    let env = Env::default();
    let sender = Address::generate(&env);
    let other_sender = Address::generate(&env);
    let base = derive_stream_salt(&env, &sender, 1, 1);

    assert_ne!(base, derive_stream_salt(&env, &other_sender, 1, 1));
    assert_ne!(base, derive_stream_salt(&env, &sender, 2, 1));
    assert_ne!(base, derive_stream_salt(&env, &sender, 1, 2));
}

// ---------------------------------------------------------------------------
// init — happy path and error branches
// ---------------------------------------------------------------------------

/// `init` succeeds and persists all supplied parameters.
#[test]
fn test_init_happy_path() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);

    factory.init(&admin, &sc, &stream_hash(&env), &5_000, &200);

    let cfg = factory.get_factory_config();
    assert_eq!(cfg.admin, admin);
    assert_eq!(cfg.stream_contract, sc);
    assert_eq!(cfg.max_deposit, 5_000);
    assert_eq!(cfg.min_duration, 200);
    assert_eq!(cfg.stream_wasm_hash, stream_hash(&env));
}

/// The reviewed hash is part of the policy consumed by creation paths.
#[test]
fn test_load_policy_includes_stream_wasm_hash() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);

    factory.init(&admin, &sc, &stream_hash(&env), &5_000, &200);

    let policy = env
        .as_contract(&fid, || load_policy(&env))
        .expect("policy should load after init");
    assert_eq!(policy.stream_wasm_hash, stream_hash(&env));
}

/// Governance can rotate the reviewed hash through the admin setter.
#[test]
fn test_set_stream_wasm_hash_round_trip() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    let new_hash = BytesN::from_array(&env, &[9; 32]);

    factory.init(&admin, &sc, &stream_hash(&env), &5_000, &200);
    factory.set_stream_wasm_hash(&new_hash);

    assert_eq!(factory.get_factory_config().stream_wasm_hash, new_hash);
}

/// Calling `init` a second time returns `AlreadyInitialized`.
#[test]
fn test_init_double_returns_already_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);

    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);
    let result = factory.try_init(&admin, &sc, &stream_hash(&env), &1_000, &10);
    assert_eq!(result, Err(Ok(FactoryError::AlreadyInitialized)));
}

// ---------------------------------------------------------------------------
// Views before init — NotInitialized
// ---------------------------------------------------------------------------

/// `get_factory_config` before `init` returns `NotInitialized`.
#[test]
fn test_get_factory_config_before_init() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);

    assert_eq!(
        factory.try_get_factory_config().unwrap_err().unwrap(),
        soroban_sdk::Error::from(FactoryError::NotInitialized)
    );
}

/// Each admin-only setter before `init` returns `NotInitialized`.
#[test]
fn test_setters_before_init_return_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let addr = Address::generate(&env);

    assert_eq!(
        factory.try_set_admin(&addr),
        Err(Ok(FactoryError::NotInitialized))
    );
    assert_eq!(
        factory.try_set_stream_contract(&addr),
        Err(Ok(FactoryError::NotInitialized))
    );
    assert_eq!(
        factory.try_set_stream_wasm_hash(&stream_hash(&env)),
        Err(Ok(FactoryError::NotInitialized))
    );
    assert_eq!(
        factory.try_set_cap(&1_000),
        Err(Ok(FactoryError::NotInitialized))
    );
    assert_eq!(
        factory.try_set_min_duration(&100),
        Err(Ok(FactoryError::NotInitialized))
    );
    assert_eq!(
        factory.try_set_max_duration(&100),
        Err(Ok(FactoryError::NotInitialized))
    );
    assert_eq!(
        factory.try_set_allowlist(&addr, &true),
        Err(Ok(FactoryError::NotInitialized))
    );
    assert_eq!(
        factory.try_set_token_allowed(&addr, &true),
        Err(Ok(FactoryError::NotInitialized))
    );
    assert_eq!(
        factory.try_set_pause(&true),
        Err(Ok(FactoryError::NotInitialized))
    );
}

// ---------------------------------------------------------------------------
// set_admin — rotation and auth transfer
// ---------------------------------------------------------------------------

/// `set_admin` updates the stored admin; `get_factory_config` reflects the change.
#[test]
fn test_set_admin_updates_config() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    let new_admin = Address::generate(&env);
    factory.set_admin(&new_admin);
    assert_eq!(factory.get_factory_config().admin, new_admin);
}

/// After rotation, the new admin can call setters (mock_all_auths covers both).
#[test]
fn test_set_admin_new_admin_can_call_setters() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    let new_admin = Address::generate(&env);
    factory.set_admin(&new_admin);

    factory.set_cap(&3_000);
    assert_eq!(factory.get_factory_config().max_deposit, 3_000);
}

/// Setting admin to the same address is a no-op and does not error.
#[test]
fn test_set_admin_same_address_noop() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    factory.set_admin(&admin);
    assert_eq!(factory.get_factory_config().admin, admin);
}

// ---------------------------------------------------------------------------
// set_cap — round-trip
// ---------------------------------------------------------------------------

/// `set_cap` persists and is reflected by `get_factory_config`.
#[test]
fn test_set_cap_round_trip() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    factory.set_cap(&7_500);
    assert_eq!(factory.get_factory_config().max_deposit, 7_500);
}

// ---------------------------------------------------------------------------
// set_min_duration — round-trip
// ---------------------------------------------------------------------------

/// `set_min_duration` persists and is reflected by `get_factory_config`.
#[test]
fn test_set_min_duration_round_trip() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    factory.set_min_duration(&300);
    assert_eq!(factory.get_factory_config().min_duration, 300);
}

#[test]
fn test_duration_bounds_validate_and_update() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    factory.init(&admin, &sc, &10_000, &3_600);

    factory.set_max_duration(&10_000);
    factory.validate_duration(&0, &3_600);
    factory.validate_duration(&0, &10_000);
    assert_eq!(
        factory.try_validate_duration(&0, &3_599),
        Err(Ok(FactoryError::InvalidDuration))
    );
    assert_eq!(
        factory.try_validate_duration(&0, &10_001),
        Err(Ok(FactoryError::InvalidDuration))
    );
    assert_eq!(
        factory.try_validate_duration(&10, &9),
        Err(Ok(FactoryError::InvalidDuration))
    );

    assert_eq!(
        factory.try_set_max_duration(&3_599),
        Err(Ok(FactoryError::InvalidDuration))
    );
}

// ---------------------------------------------------------------------------
// set_allowlist / is_allowlisted
// ---------------------------------------------------------------------------

/// `is_allowlisted` returns false for an address never added.
#[test]
fn test_is_allowlisted_default_false() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    let recipient = Address::generate(&env);
    assert!(!factory.is_allowlisted(&recipient));
}

/// Adding a recipient flips `is_allowlisted` to true.
#[test]
fn test_set_allowlist_add() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    let recipient = Address::generate(&env);
    factory.set_allowlist(&recipient, &true);
    assert!(factory.is_allowlisted(&recipient));
}

/// Removing a recipient flips `is_allowlisted` back to false and removes the
/// underlying persistent key.
#[test]
fn test_set_allowlist_remove() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    let recipient = Address::generate(&env);
    factory.set_allowlist(&recipient, &true);
    factory.set_allowlist(&recipient, &false);
    assert!(!factory.is_allowlisted(&recipient));
}

/// Removing a recipient that was never added is a safe no-op.
#[test]
fn test_set_allowlist_remove_non_allowlisted_noop() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    let recipient = Address::generate(&env);
    factory.set_allowlist(&recipient, &false); // never added — should not panic
    assert!(!factory.is_allowlisted(&recipient));
}

// ---------------------------------------------------------------------------
// token allowlist / is_token_allowed
// ---------------------------------------------------------------------------

#[test]
fn test_token_allowlist_add_remove_and_validate() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    let token = Address::generate(&env);
    factory.init(&admin, &sc, &10_000, &100);

    assert!(!factory.is_token_allowed(&token));
    assert_eq!(
        factory.try_validate_token(&token),
        Err(Ok(FactoryError::TokenNotAllowed))
    );

    factory.set_token_allowed(&token, &true);
    assert!(factory.is_token_allowed(&token));
    factory.validate_token(&token);

    factory.set_token_allowed(&token, &false);
    assert!(!factory.is_token_allowed(&token));
}

// ---------------------------------------------------------------------------
// set_pause / is_factory_paused
// ---------------------------------------------------------------------------

#[test]
fn test_set_pause_updates_policy() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    factory.init(&admin, &sc, &10_000, &100);

    assert!(!factory.is_factory_paused());
    factory.set_pause(&true);
    assert!(factory.is_factory_paused());
    assert!(factory.get_factory_config().creation_paused);
    factory.set_pause(&false);
    assert!(!factory.is_factory_paused());
}

// ---------------------------------------------------------------------------
// per-sender creation rate limit
// ---------------------------------------------------------------------------

#[test]
fn test_record_creation_enforces_sender_window_and_resets() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    let sender = Address::generate(&env);
    factory.init(&admin, &sc, &10_000, &100);
    factory.set_rate_limit(&2, &10);
    assert_eq!(
        factory.try_set_rate_limit(&0, &10),
        Err(Ok(FactoryError::InvalidRateLimit))
    );

    factory.record_creation(&sender);
    factory.record_creation(&sender);
    assert_eq!(
        factory.try_record_creation(&sender),
        Err(Ok(FactoryError::RateLimitExceeded))
    );

    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 10);
    factory.record_creation(&sender);
}

// ---------------------------------------------------------------------------
// Negative auth tests — each admin-only setter must fail without admin auth
// ---------------------------------------------------------------------------

/// Helper: assert that a closure panics (Soroban testutils behaviour for
/// unauthorized `require_auth` calls).
fn assert_auth_fails<F: FnOnce()>(f: F) {
    let result = std::panic::catch_unwind(AssertUnwindSafe(f));
    assert!(
        result.is_err(),
        "expected auth failure (panic) but call succeeded"
    );
}

/// `set_admin` rejects a non-admin caller.
#[test]
fn test_set_admin_rejects_non_admin() {
    let env = Env::default();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let non_admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let sc = Address::generate(&env);

    env.mock_all_auths();
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    env.mock_auths(&[MockAuth {
        address: &non_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_admin",
            args: (&new_admin,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert_auth_fails(|| factory.set_admin(&new_admin));
}

/// `set_stream_contract` rejects a non-admin caller.
#[test]
fn test_set_stream_contract_rejects_non_admin() {
    let env = Env::default();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let non_admin = Address::generate(&env);
    let sc = Address::generate(&env);
    let new_sc = Address::generate(&env);

    env.mock_all_auths();
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    env.mock_auths(&[MockAuth {
        address: &non_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_stream_contract",
            args: (&new_sc,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert_auth_fails(|| factory.set_stream_contract(&new_sc));
}

/// `set_cap` rejects a non-admin caller.
#[test]
fn test_set_cap_rejects_non_admin() {
    let env = Env::default();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let non_admin = Address::generate(&env);
    let sc = Address::generate(&env);

    env.mock_all_auths();
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    env.mock_auths(&[MockAuth {
        address: &non_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_cap",
            args: (5_000i128,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert_auth_fails(|| factory.set_cap(&5_000));
}

/// `set_min_duration` rejects a non-admin caller.
#[test]
fn test_set_min_duration_rejects_non_admin() {
    let env = Env::default();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let non_admin = Address::generate(&env);
    let sc = Address::generate(&env);

    env.mock_all_auths();
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    env.mock_auths(&[MockAuth {
        address: &non_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_min_duration",
            args: (500u64,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert_auth_fails(|| factory.set_min_duration(&500));
}

/// `set_pause` rejects a non-admin caller.
#[test]
fn test_set_pause_rejects_non_admin() {
    let env = Env::default();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let non_admin = Address::generate(&env);
    let sc = Address::generate(&env);

    env.mock_all_auths();
    factory.init(&admin, &sc, &10_000, &100);

    env.mock_auths(&[MockAuth {
        address: &non_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_pause",
            args: (true,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert_auth_fails(|| factory.set_pause(&true));
    assert!(!factory.is_factory_paused());
}

/// `set_allowlist` rejects a non-admin caller.
#[test]
fn test_set_allowlist_rejects_non_admin() {
    let env = Env::default();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let non_admin = Address::generate(&env);
    let sc = Address::generate(&env);
    let recipient = Address::generate(&env);

    env.mock_all_auths();
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    env.mock_auths(&[MockAuth {
        address: &non_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_allowlist",
            args: (&recipient, true).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert_auth_fails(|| factory.set_allowlist(&recipient, &true));
}

// ---------------------------------------------------------------------------
// Instance storage TTL regression tests (issue #728)
// ---------------------------------------------------------------------------

/// Test that `init` bumps instance TTL, allowing config to survive idle periods.
///
/// This test simulates a long-idle factory by advancing the ledger clock
/// after initialization. The config should remain accessible if TTL was
/// properly bumped during `init`.
#[test]
fn test_init_bumps_instance_ttl() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);

    // Initialize the factory — this should bump instance TTL.
    factory.init(&admin, &sc, &stream_hash(&env), &5_000, &200);

    // Verify config is immediately accessible after init.
    let cfg = factory.get_factory_config();
    assert_eq!(cfg.admin, admin);

    // Advance ledger by a significant amount (simulating idle time).
    // The threshold is 17_280 ledgers; we advance less than that to show
    // that a single TTL bump keeps the entry alive past one idle window.
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 10_000);

    // Config should still be accessible after simulated idle time.
    let cfg = factory.get_factory_config();
    assert_eq!(cfg.admin, admin);
    assert_eq!(cfg.stream_contract, sc);
    assert_eq!(cfg.max_deposit, 5_000);
    assert_eq!(cfg.min_duration, 200);
}

/// Test that each admin setter bumps instance TTL.
///
/// This test verifies that calling each setter extends the instance TTL,
/// ensuring that repeated admin activity keeps the config alive.
#[test]
fn test_setters_bump_instance_ttl() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);

    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    // Test set_admin bumps TTL.
    let new_admin = Address::generate(&env);
    factory.set_admin(&new_admin);
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 5_000);
    assert_eq!(factory.get_factory_config().admin, new_admin);

    // Test set_stream_contract bumps TTL.
    let new_sc = Address::generate(&env);
    factory.set_stream_contract(&new_sc);
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 5_000);
    assert_eq!(factory.get_factory_config().stream_contract, new_sc);

    // Test set_cap bumps TTL.
    factory.set_cap(&7_500);
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 5_000);
    assert_eq!(factory.get_factory_config().max_deposit, 7_500);

    // Test set_min_duration bumps TTL.
    factory.set_min_duration(&250);
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 5_000);
    assert_eq!(factory.get_factory_config().min_duration, 250);

    // Test set_batch_cap_enforcement bumps TTL.
    factory.set_batch_cap_enforcement(&false);
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 5_000);
    assert_eq!(factory.get_factory_config().batch_cap_enforced, false);

    // Test set_factory_paused bumps TTL.
    factory.set_factory_paused(&true);
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 5_000);
    assert!(factory.is_factory_paused());

    // Test set_rate_bounds bumps TTL (by checking config still accessible).
    factory.set_rate_bounds(&Some(100), &Some(1_000));
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 5_000);
    assert_eq!(factory.get_factory_config().max_deposit, 7_500); // config still accessible
}

/// Test repeated setter calls near TTL threshold keep config alive.
///
/// This test simulates a factory receiving frequent admin updates (near
/// the TTL threshold window) and verifies that repeated bumps keep the
/// config from expiring.
#[test]
fn test_repeated_setter_calls_prevent_expiration() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);

    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    // Simulate a busy factory: repeatedly advance and call setters.
    for i in 0..5 {
        env.ledger()
            .set_sequence_number(env.ledger().sequence() + 3_000);
        factory.set_cap(&(10_000 + (i as i128 * 100)));
        let cfg = factory.get_factory_config();
        assert_eq!(cfg.max_deposit, 10_000 + (i as i128 * 100));
    }

    // After 5 updates spaced 3000 ledgers apart (15_000 total),
    // the config should still be accessible.
    let cfg = factory.get_factory_config();
    assert_eq!(cfg.max_deposit, 10_400); // last update
}

/// Test that policy reads refresh the factory instance TTL.
#[test]
fn test_policy_reads_refresh_instance_ttl() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);

    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);
    assert_eq!(factory.get_factory_config().max_deposit, 10_000);

    // Read operations refresh the instance entry before the next idle period.
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 10_000);

    // This read must extend the policy entry's lifetime.
    let paused = factory.is_factory_paused();
    assert!(!paused);

    // A second long interval remains safe because the read refreshed TTL.
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 10_000);
    assert_eq!(factory.get_factory_config().max_deposit, 10_000);
}

/// An allowlist read refreshes the corresponding persistent entry.
#[test]
fn test_allowlist_reads_refresh_persistent_ttl() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);
    let recipient = Address::generate(&env);

    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);
    factory.set_allowlist(&recipient, &true);
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 10_000);

    assert!(factory.is_allowlisted(&recipient));
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 10_000);
    assert!(factory.is_allowlisted(&recipient));
}

/// Test that set_rate_bounds bumps instance TTL.
#[test]
fn test_set_rate_bounds_bumps_instance_ttl() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);

    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    // Set rate bounds — this should bump TTL.
    factory.set_rate_bounds(&Some(50), &Some(5_000));

    // Advance ledger and verify config is still accessible.
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 5_000);
    let cfg = factory.get_factory_config();
    assert_eq!(cfg.max_deposit, 10_000); // config still accessible
}

// ---------------------------------------------------------------------------
// load_policy helper — centralised policy-load chokepoint
// ---------------------------------------------------------------------------

/// `load_policy` returns `NotInitialized` before any `init` call. All required
/// fields are absent, so the first required read fails.
#[test]
fn test_load_policy_before_init_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);

    let result = env.as_contract(&fid, || load_policy(&env));
    assert_eq!(result, Err(FactoryError::NotInitialized));
}

/// Immediately after `init`, `load_policy` reflects the init-supplied values
/// plus the documented defaults for optional fields (pause = false, rate
/// bounds = None, batch_cap = true).
#[test]
fn test_load_policy_reflects_initial_state() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);

    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    let policy = env
        .as_contract(&fid, || load_policy(&env))
        .expect("policy should load after init");
    assert_eq!(policy.stream_contract, sc);
    assert_eq!(policy.max_deposit, 10_000);
    assert_eq!(policy.min_duration, 100);
    assert!(policy.batch_cap_enforced, "init defaults batch-cap to true");
    assert!(!policy.creation_paused, "init defaults pause to false");
    assert_eq!(
        policy.min_rate_per_second, None,
        "no rate bounds by default"
    );
    assert_eq!(
        policy.max_rate_per_second, None,
        "no rate bounds by default"
    );
    assert_eq!(policy.max_active_streams, u32::MAX);
    assert_eq!(policy.active_streams, 0);
}

/// Applying each policy setter (cap / min_duration / stream_contract /
/// batch_cap / pause / rate bounds) is reflected by `load_policy`. This is
/// the primary regression test for the centralised load helper.
#[test]
fn test_load_policy_reflects_all_setters() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);

    // Initial cap/min_duration
    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    // Replace each policy axis through its setter and verify via load_policy.
    let new_sc = Address::generate(&env);
    factory.set_stream_contract(&new_sc);
    factory.set_cap(&7_500);
    factory.set_min_duration(&250);
    factory.set_batch_cap_enforcement(&false);
    factory.set_factory_paused(&true);
    factory.set_rate_bounds(&Some(50), &Some(1_000));

    let policy: FactoryPolicy = env
        .as_contract(&fid, || load_policy(&env))
        .expect("policy should load");

    assert_eq!(policy.stream_contract, new_sc);
    assert_eq!(policy.max_deposit, 7_500);
    assert_eq!(policy.min_duration, 250);
    assert!(!policy.batch_cap_enforced);
    assert!(policy.creation_paused);
    assert_eq!(policy.min_rate_per_second, Some(50));
    assert_eq!(policy.max_rate_per_second, Some(1_000));
}

/// When rate bounds are absent, `load_policy` reports `None` for both fields.
/// This guards against accidentally reading them as `0` (which would silently
/// turn into RateBelowMin / RateAboveMax failures on every stream creation).
#[test]
fn test_load_policy_defaults_rate_bounds_to_none() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);

    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);

    let policy = env
        .as_contract(&fid, || load_policy(&env))
        .expect("policy should load");
    assert_eq!(policy.min_rate_per_second, None);
    assert_eq!(policy.max_rate_per_second, None);

    // Toggling pause does not implicitly change rate-bound visibility.
    factory.set_factory_paused(&true);
    let policy = env
        .as_contract(&fid, || load_policy(&env))
        .expect("policy should load");
    assert_eq!(policy.min_rate_per_second, None);
    assert_eq!(policy.max_rate_per_second, None);
}

/// `set_batch_cap_enforcement` flips `batch_cap_enforced` in both directions,
/// which `load_policy` must surface verbatim for the batch path to honour.
#[test]
fn test_load_policy_reflects_batch_cap_toggle() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);

    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);
    assert!(
        env.as_contract(&fid, || load_policy(&env))
            .unwrap()
            .batch_cap_enforced
    );

    factory.set_batch_cap_enforcement(&false);
    assert!(
        !env.as_contract(&fid, || load_policy(&env))
            .unwrap()
            .batch_cap_enforced
    );

    factory.set_batch_cap_enforcement(&true);
    assert!(
        env.as_contract(&fid, || load_policy(&env))
            .unwrap()
            .batch_cap_enforced
    );
}

/// `set_factory_paused` flips `creation_paused`, which is the very first
/// semantic guard after the policy load on both creation paths.
#[test]
fn test_load_policy_reflects_pause_toggle() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);

    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);
    assert!(
        !env.as_contract(&fid, || load_policy(&env))
            .unwrap()
            .creation_paused
    );

    factory.set_factory_paused(&true);
    assert!(
        env.as_contract(&fid, || load_policy(&env))
            .unwrap()
            .creation_paused
    );

    factory.set_factory_paused(&false);
    assert!(
        !env.as_contract(&fid, || load_policy(&env))
            .unwrap()
            .creation_paused
    );
}

/// `FactoryPolicy` instances returned from `load_policy` comparing equal must
/// be a strict struct-equality (not e.g. partial memcmp) so callers can
/// rely on field-by-field exactness for snapshot tests.
#[test]
fn test_load_policy_equality_is_struct_equality() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sc = Address::generate(&env);

    factory.init(&admin, &sc, &stream_hash(&env), &10_000, &100);
    factory.set_rate_bounds(&Some(10), &Some(100));

    let p1 = env.as_contract(&fid, || load_policy(&env)).unwrap();
    let p2 = env.as_contract(&fid, || load_policy(&env)).unwrap();
    assert_eq!(p1, p2);

    let admin_addr = p1.stream_contract.clone();
    let mut different = p1.clone();
    different.stream_contract = admin_addr; // identical -> still equal
    assert_eq!(p1, different);

    // Flip a single field — equality must break.
    let mut different = p1.clone();
    different.max_deposit += 1;
    assert_ne!(p1, different);
}

// ---------------------------------------------------------------------------
// Active-stream capacity
// ---------------------------------------------------------------------------

#[test]
fn active_stream_cap_blocks_creation_until_terminal_release() {
    let env = Env::default();
    env.mock_all_auths();
    let stream_id = env.register_contract(None, StreamStub);
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);
    let token = Address::generate(&env);

    factory.init(&admin, &stream_id, &10_000, &100);
    factory.set_max_active_streams(&1);

    factory.create_stream(
        &sender,
        &recipient,
        &token,
        &1_000,
        &0,
        &100,
        &0,
        &true,
        &true,
        &true,
    );
    assert_eq!(factory.get_factory_config().active_streams, 1);

    assert_eq!(
        factory
            .try_create_stream(
                &sender,
                &recipient,
                &token,
                &1_000,
                &0,
                &100,
                &0,
                &true,
                &true,
                &true,
            )
            .unwrap_err()
            .unwrap(),
        FactoryError::CapacityCapExceeded
    );

    factory.stream_terminated(&0);
    assert_eq!(factory.get_factory_config().active_streams, 0);
    factory.create_stream(
        &sender,
        &recipient,
        &token,
        &1_000,
        &0,
        &100,
        &0,
        &true,
        &true,
        &true,
    );
}

#[test]
fn max_active_streams_is_admin_configurable() {
    let env = Env::default();
    env.mock_all_auths();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let admin = Address::generate(&env);
    let stream_contract = Address::generate(&env);

    factory.init(&admin, &stream_contract, &10_000, &100);
    factory.set_max_active_streams(&25);

    let config = factory.get_factory_config();
    assert_eq!(config.max_active_streams, 25);
    assert_eq!(config.active_streams, 0);
}

// ---------------------------------------------------------------------------
// Same-ledger admin rotation tests
// ---------------------------------------------------------------------------

/// After rotating admin, the old admin cannot use residual authorization for a
/// setter call issued immediately after rotation in the same ledger. The second
/// call should fail auth because it requires the new admin's authorization.
#[test]
fn test_set_admin_same_ledger_old_admin_fails() {
    let env = Env::default();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let old_admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let sc = Address::generate(&env);

    // Initialize with old_admin
    env.mock_auths(&[MockAuth {
        address: &old_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "init",
            args: (&old_admin, &sc, &stream_hash(&env), 10_000i128, 100u64).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    factory.init(&old_admin, &sc, &stream_hash(&env), &10_000, &100);

    // Rotate to new_admin using old_admin's auth
    env.mock_auths(&[MockAuth {
        address: &old_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_admin",
            args: (&new_admin,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    factory.set_admin(&new_admin);

    // Verify rotation succeeded
    assert_eq!(factory.get_factory_config().admin, new_admin);

    // Same ledger: attempt to call set_cap with only old_admin's authorization
    // This should fail because the contract now requires new_admin's auth
    env.mock_auths(&[MockAuth {
        address: &old_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_cap",
            args: (5_000i128,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert_auth_fails(|| factory.set_cap(&5_000));

    // Verify the cap was not changed
    assert_eq!(factory.get_factory_config().max_deposit, 10_000);
}

/// After rotating admin in the same ledger, the new admin can successfully
/// call a setter immediately without waiting for a new ledger.
#[test]
fn test_set_admin_same_ledger_new_admin_succeeds() {
    let env = Env::default();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let old_admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let sc = Address::generate(&env);

    // Initialize with old_admin
    env.mock_auths(&[MockAuth {
        address: &old_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "init",
            args: (&old_admin, &sc, &stream_hash(&env), 10_000i128, 100u64).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    factory.init(&old_admin, &sc, &stream_hash(&env), &10_000, &100);

    // Rotate to new_admin using old_admin's auth
    env.mock_auths(&[MockAuth {
        address: &old_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_admin",
            args: (&new_admin,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    factory.set_admin(&new_admin);

    // Verify rotation succeeded
    assert_eq!(factory.get_factory_config().admin, new_admin);

    // Same ledger: new_admin calls set_cap and should succeed
    env.mock_auths(&[MockAuth {
        address: &new_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_cap",
            args: (7_500i128,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    factory.set_cap(&7_500);

    // Verify the cap was updated successfully
    assert_eq!(factory.get_factory_config().max_deposit, 7_500);
}

/// After rotating admin, verify old admin cannot call multiple different setters
/// in the same ledger, while new admin can.
#[test]
fn test_set_admin_same_ledger_multiple_setters() {
    let env = Env::default();
    let fid = env.register_contract(None, FluxoraFactory);
    let factory = FluxoraFactoryClient::new(&env, &fid);
    let old_admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let sc = Address::generate(&env);

    // Initialize with old_admin
    env.mock_auths(&[MockAuth {
        address: &old_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "init",
            args: (&old_admin, &sc, &stream_hash(&env), 10_000i128, 100u64).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    factory.init(&old_admin, &sc, &stream_hash(&env), &10_000, &100);

    // Rotate to new_admin
    env.mock_auths(&[MockAuth {
        address: &old_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_admin",
            args: (&new_admin,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    factory.set_admin(&new_admin);

    // Old admin tries set_min_duration - should fail
    env.mock_auths(&[MockAuth {
        address: &old_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_min_duration",
            args: (500u64,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert_auth_fails(|| factory.set_min_duration(&500));

    // New admin calls set_min_duration - should succeed
    env.mock_auths(&[MockAuth {
        address: &new_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_min_duration",
            args: (300u64,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    factory.set_min_duration(&300);
    assert_eq!(factory.get_factory_config().min_duration, 300);

    // Old admin tries set_batch_cap_enforcement - should fail
    env.mock_auths(&[MockAuth {
        address: &old_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_batch_cap_enforcement",
            args: (false,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert_auth_fails(|| factory.set_batch_cap_enforcement(&false));

    // New admin calls set_batch_cap_enforcement - should succeed
    env.mock_auths(&[MockAuth {
        address: &new_admin,
        invoke: &MockAuthInvoke {
            contract: &fid,
            fn_name: "set_batch_cap_enforcement",
            args: (false,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    factory.set_batch_cap_enforcement(&false);
    assert_eq!(factory.get_factory_config().batch_cap_enforced, false);
}
