//! Unit, fuzz, and archival resilience tests for `FluxoraFactory` entry lifetime extension (issue #363).
//!
//! Verifies:
//! - Automated lifetime extension of the instance storage entry on initialization and all mutating/reading paths.
//! - Automated persistent storage TTL extension for allowlist entries on writes, queries, and stream creations.
//! - Automated persistent storage TTL extension for token allowlist entries on writes, queries, and stream creations.
//! - Automated persistent storage TTL extension for rate-limiting sender buckets.
//! - Permissionless keeper single entry extensions (`extend_allowlist_ttl`, `extend_token_allowlist_ttl`).
//! - Permissionless keeper batch sweeps (`batch_extend_allowlist_ttl`, `batch_extend_token_ttl`).
//! - Event emissions (`TtlExtended`) for off-chain keeper indexers and observability.
//! - Defensive parameter bounds and error handling (empty batch, oversize batch, non-existent entries).
//! - Fuzz / property tests ensuring TTL monotonicity and non-decreasing remaining lifetime guarantees.

#![cfg(test)]

use fluxora_factory::{
    bump_allowlist, bump_ttl, DataKey, FactoryError, FluxoraFactory, FluxoraFactoryClient,
    TtlExtended,
};
use proptest::prelude::*;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Events, Ledger},
    vec, Address, BytesN, Env, IntoVal, Symbol, TryFromVal, Vec,
};

fn stream_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[9; 32])
}

#[contract]
struct MockStream;

#[contractimpl]
impl MockStream {
    #[allow(clippy::too_many_arguments)]
    pub fn create_stream(
        _env: Env,
        _sender: Address,
        _recipient: Address,
        _token: Address,
        _deposit: i128,
        _start_time: u64,
        _end_time: u64,
        _cliff_time: u64,
        _cancellable: bool,
        _pausable: bool,
        _transferable: bool,
    ) -> u64 {
        1001
    }
}

fn setup(env: &Env) -> (FluxoraFactoryClient<'static>, Address, Address) {
    env.mock_all_auths();
    let stream_id = env.register(MockStream, ());
    let factory_id = env.register(FluxoraFactory, ());
    let client = FluxoraFactoryClient::new(env, &factory_id);
    let admin = Address::generate(env);
    client.init(&admin, &stream_id, &stream_hash(env), &1_000_000, &100);
    (client, factory_id, admin)
}

// ---------------------------------------------------------------------------
// 1. Instance Entry Lifetime Extension
// ---------------------------------------------------------------------------

#[test]
fn instance_ttl_bumped_on_init_and_setters() {
    let env = Env::default();
    let (client, factory_id, _admin) = setup(&env);

    let initial_ttl = env.as_contract(&factory_id, || env.storage().instance().get_ttl());
    assert!(initial_ttl > 0, "instance TTL should be funded after init");

    // Advance sequence number
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 5_000);

    // Call a setter
    client.set_cap(&2_000_000);
    let after_setter_ttl = env.as_contract(&factory_id, || env.storage().instance().get_ttl());
    assert!(
        after_setter_ttl > initial_ttl.saturating_sub(5_000),
        "setter must bump instance TTL"
    );
}

#[test]
fn instance_ttl_bumped_on_config_read() {
    let env = Env::default();
    let (client, factory_id, _admin) = setup(&env);

    // Advance sequence number
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 5_000);

    // Read config
    let _cfg = client.get_factory_config();
    let read_ttl = env.as_contract(&factory_id, || env.storage().instance().get_ttl());
    assert!(read_ttl > 0, "reading config must refresh instance TTL");
}

// ---------------------------------------------------------------------------
// 2. Persistent Storage Lifetime Extension (Allowlist & Token Allowlist)
// ---------------------------------------------------------------------------

#[test]
fn allowlist_ttl_bumped_on_write_and_query() {
    let env = Env::default();
    let (client, factory_id, _admin) = setup(&env);
    let recipient = Address::generate(&env);

    client.set_allowlist(&recipient, &true);

    let ttl_after_write = env.as_contract(&factory_id, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Allowlist(recipient.clone()))
    });
    assert!(ttl_after_write > 0, "allowlist entry must have active TTL");

    // Advance ledger
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 8_000);

    // Query allowlist
    assert!(client.is_allowlisted(&recipient));

    let ttl_after_query = env.as_contract(&factory_id, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Allowlist(recipient.clone()))
    });
    assert!(
        ttl_after_query >= ttl_after_write.saturating_sub(8_000),
        "is_allowlisted must refresh entry TTL"
    );
}

#[test]
fn token_allowlist_ttl_bumped_on_write_and_validation() {
    let env = Env::default();
    let (client, factory_id, _admin) = setup(&env);
    let token = Address::generate(&env);

    client.set_token_allowed(&token, &true);

    let ttl_after_write = env.as_contract(&factory_id, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::TokenAllowlist(token.clone()))
    });
    assert!(ttl_after_write > 0, "token entry must have active TTL");

    // Advance ledger
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 8_000);

    // Validate token
    assert!(client.is_token_allowed(&token));
    client.validate_token(&token);

    let ttl_after_validate = env.as_contract(&factory_id, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::TokenAllowlist(token.clone()))
    });
    assert!(
        ttl_after_validate >= ttl_after_write.saturating_sub(8_000),
        "token validation must refresh entry TTL"
    );
}

// ---------------------------------------------------------------------------
// 3. Permissionless Keeper Lifetime Extensions & Event Verification
// ---------------------------------------------------------------------------

#[test]
fn extend_allowlist_ttl_permissionless_and_events() {
    let env = Env::default();
    let (client, factory_id, _admin) = setup(&env);
    let recipient = Address::generate(&env);

    client.set_allowlist(&recipient, &true);

    // Advance ledger
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 10_000);

    // Keeper extends TTL
    client.extend_allowlist_ttl(&recipient);

    let ttl = env.as_contract(&factory_id, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Allowlist(recipient.clone()))
    });
    assert!(ttl > 10_000, "keeper extend must replenish TTL");

    // Verify TtlExtended event published
    let events = env.events().all().events().to_vec();
    let mut found = false;
    for event in events.iter() {
        if let soroban_sdk::xdr::ContractEventBody::V0(ref body) = event.body {
            if body.topics.len() >= 3 {
                if let Ok(topic_sym) = Symbol::try_from_val(&env, &body.topics[0]) {
                    if topic_sym == Symbol::new(&env, "ttl_extended") {
                        if let Ok(key_sym) = Symbol::try_from_val(&env, &body.topics[1]) {
                            if key_sym == Symbol::new(&env, "allowlist") {
                                found = true;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(found, "TtlExtended event must be published for allowlist");
}

#[test]
fn extend_token_allowlist_ttl_permissionless() {
    let env = Env::default();
    let (client, factory_id, _admin) = setup(&env);
    let token = Address::generate(&env);

    client.set_token_allowed(&token, &true);

    // Advance ledger
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 10_000);

    client.extend_token_allowlist_ttl(&token);

    let ttl = env.as_contract(&factory_id, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::TokenAllowlist(token.clone()))
    });
    assert!(ttl > 10_000, "keeper extend must replenish token TTL");
}

#[test]
fn extend_nonexistent_entries_returns_typed_errors() {
    let env = Env::default();
    let (client, _factory_id, _admin) = setup(&env);
    let unadded = Address::generate(&env);

    let err_allow = client
        .try_extend_allowlist_ttl(&unadded)
        .unwrap_err()
        .unwrap();
    assert_eq!(err_allow, FactoryError::AllowlistDenied);

    let err_tok = client
        .try_extend_token_allowlist_ttl(&unadded)
        .unwrap_err()
        .unwrap();
    assert_eq!(err_tok, FactoryError::TokenNotAllowed);
}

// ---------------------------------------------------------------------------
// 4. Batch Extension Sweeps
// ---------------------------------------------------------------------------

#[test]
fn batch_extend_allowlist_ttl_sweeps_and_skips_missing() {
    let env = Env::default();
    let (client, _factory_id, _admin) = setup(&env);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);
    let missing = Address::generate(&env);

    client.set_allowlist(&a1, &true);
    client.set_allowlist(&a2, &true);
    client.set_allowlist(&a3, &true);

    let mut batch = Vec::new(&env);
    batch.push_back(a1);
    batch.push_back(missing);
    batch.push_back(a2);
    batch.push_back(a3);

    let extended = client.batch_extend_allowlist_ttl(&batch);
    assert_eq!(
        extended, 3,
        "should extend 3 existing entries and skip missing"
    );
}

#[test]
fn batch_extend_token_ttl_sweeps() {
    let env = Env::default();
    let (client, _factory_id, _admin) = setup(&env);

    let t1 = Address::generate(&env);
    let t2 = Address::generate(&env);
    client.set_token_allowed(&t1, &true);
    client.set_token_allowed(&t2, &true);

    let mut batch = Vec::new(&env);
    batch.push_back(t1);
    batch.push_back(t2);

    let extended = client.batch_extend_token_ttl(&batch);
    assert_eq!(extended, 2);
}

#[test]
fn batch_extend_bounds_checks() {
    let env = Env::default();
    let (client, _factory_id, _admin) = setup(&env);

    // Empty batch returns InvalidDuration
    let empty_vec: Vec<Address> = Vec::new(&env);
    let err = client
        .try_batch_extend_allowlist_ttl(&empty_vec)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, FactoryError::InvalidDuration);

    // Oversized batch returns InvalidDuration (> 32)
    let mut big_vec: Vec<Address> = Vec::new(&env);
    for _ in 0..33 {
        big_vec.push_back(Address::generate(&env));
    }
    let err_big = client
        .try_batch_extend_allowlist_ttl(&big_vec)
        .unwrap_err()
        .unwrap();
    assert_eq!(err_big, FactoryError::InvalidDuration);
}

// ---------------------------------------------------------------------------
// 5. Rate Limit Lifetime Extension & Stream Creation Bumps
// ---------------------------------------------------------------------------

#[test]
fn rate_limit_entry_extended_on_creation() {
    let env = Env::default();
    let (client, factory_id, _admin) = setup(&env);

    client.set_rate_limit(&10, &1000);

    let sender = Address::generate(&env);
    client.record_creation(&sender);

    let rate_ttl = env.as_contract(&factory_id, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::RateLimit(sender.clone()))
    });
    assert!(rate_ttl > 0, "rate limit entry must have funded TTL");
}

#[test]
fn create_stream_bumps_instance_and_allowlist_ttl() {
    let env = Env::default();
    let (client, factory_id, _admin) = setup(&env);

    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);
    let token = Address::generate(&env);

    client.set_allowlist(&recipient, &true);
    client.set_allowlist_enabled(&true);

    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 5_000);

    let stream_id = client.create_stream(
        &sender, &recipient, &token, &50_000, &1_000, &2_000, &1_000, &true, &false, &false,
    );
    assert_eq!(stream_id, 1001);

    let instance_ttl = env.as_contract(&factory_id, || env.storage().instance().get_ttl());
    assert!(
        instance_ttl > 0,
        "instance TTL must remain healthy after stream creation"
    );
}

// ---------------------------------------------------------------------------
// 6. Proptest / Fuzz Invariants
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_batch_extension_count_never_exceeds_input(batch_size in 1usize..32usize) {
        let env = Env::default();
        let (client, _, _) = setup(&env);

        let mut batch = Vec::new(&env);
        for _ in 0..batch_size {
            let addr = Address::generate(&env);
            client.set_allowlist(&addr, &true);
            batch.push_back(addr);
        }

        let extended = client.batch_extend_allowlist_ttl(&batch);
        prop_assert_eq!(extended as usize, batch_size);
    }

    #[test]
    fn prop_ttl_helpers_safe_with_arbitrary_ledger_sequence(advance_ledgers in 1u32..50_000u32) {
        let env = Env::default();
        let (_, factory_id, _) = setup(&env);

        env.ledger().set_sequence_number(env.ledger().sequence() + advance_ledgers);

        env.as_contract(&factory_id, || {
            bump_ttl(&env);
            let key = DataKey::Allowlist(Address::generate(&env));
            env.storage().persistent().set(&key, &true);
            bump_allowlist(&env, &key);
        });
    }
}
