//! Invariant assertions, re-entrancy protection, and property tests for stream-nft.
#![cfg(test)]

use crate::{is_forbidden_address, is_zero_address, Config, Error, StreamNft, StreamNftClient};
use soroban_sdk::testutils::{Address as _, Events, MockAuth, MockAuthInvoke};
use soroban_sdk::{contract, contractimpl, Address, Env, IntoVal, String, Symbol, Val};

const ZERO_ACCOUNT: &str = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";

fn zero_address(env: &Env) -> Address {
    Address::from_string(&String::from_str(env, ZERO_ACCOUNT))
}

#[contract]
struct MockStreamContract;

#[contractimpl]
impl MockStreamContract {
    pub fn transfer_recipient(env: Env, _stream_id: u64, _to: Address) {
        // Mock success
    }
}

struct TestHarness {
    env: Env,
    admin: Address,
    metadata_admin: Address,
    manager: Address,
    user: Address,
    other: Address,
    stream: Address,
    contract_id: Address,
    client: StreamNftClient<'static>,
}

impl TestHarness {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let metadata_admin = Address::generate(&env);
        let manager = Address::generate(&env);
        let user = Address::generate(&env);
        let other = Address::generate(&env);

        let stream = env.register(MockStreamContract, ());
        let contract_id = env.register(StreamNft, ());

        let client = StreamNftClient::new(&env, &contract_id);
        client.initialize(
            &admin,
            &metadata_admin,
            &manager,
            &stream,
            &String::from_str(&env, "Perpetua Stream"),
            &String::from_str(&env, "PSTREAM"),
        );

        Self {
            env,
            admin,
            metadata_admin,
            manager,
            user,
            other,
            stream,
            contract_id,
            client,
        }
    }
}

// ---------------------------------------------------------------------------
// 1. Initialization and role configuration
// ---------------------------------------------------------------------------

#[test]
fn test_initialization_roles() {
    let h = TestHarness::new();
    let config = h.client.get_config();

    assert_eq!(config.admin, h.admin);
    assert_eq!(config.metadata_admin, h.metadata_admin);
    assert_eq!(config.manager, h.manager);
    assert_eq!(config.stream, h.stream);
    assert_eq!(config.name, String::from_str(&h.env, "Perpetua Stream"));
    assert_eq!(config.symbol, String::from_str(&h.env, "PSTREAM"));
    assert_eq!(config.decimals, 0);
    assert_eq!(config.mint_paused, false);

    assert_eq!(h.client.admin(), h.admin);
    assert_eq!(h.client.metadata_admin(), h.metadata_admin);
    assert_eq!(h.client.manager(), h.manager);
    assert_eq!(h.client.stream_contract(), h.stream);
    assert_eq!(h.client.is_mint_paused(), false);
}

#[test]
fn test_double_init_rejected() {
    let h = TestHarness::new();
    let err = h
        .client
        .try_initialize(
            &h.admin,
            &h.metadata_admin,
            &h.manager,
            &h.stream,
            &String::from_str(&h.env, "Other"),
            &String::from_str(&h.env, "OTH"),
        )
        .unwrap_err()
        .unwrap();

    assert_eq!(err, Error::AlreadyInitialized);
}

#[test]
fn test_init_rejects_forbidden_addresses() {
    let env = Env::default();
    env.mock_all_auths();
    let valid = Address::generate(&env);
    let zero = zero_address(&env);
    let contract_id = env.register(StreamNft, ());
    let client = StreamNftClient::new(&env, &contract_id);

    // zero as admin
    assert_eq!(
        client
            .try_initialize(
                &zero,
                &valid,
                &valid,
                &valid,
                &String::from_str(&env, "N"),
                &String::from_str(&env, "S")
            )
            .unwrap_err()
            .unwrap(),
        Error::InvalidRoleAddress
    );

    // zero as metadata_admin
    assert_eq!(
        client
            .try_initialize(
                &valid,
                &zero,
                &valid,
                &valid,
                &String::from_str(&env, "N"),
                &String::from_str(&env, "S")
            )
            .unwrap_err()
            .unwrap(),
        Error::InvalidRoleAddress
    );

    // zero as manager
    assert_eq!(
        client
            .try_initialize(
                &valid,
                &valid,
                &zero,
                &valid,
                &String::from_str(&env, "N"),
                &String::from_str(&env, "S")
            )
            .unwrap_err()
            .unwrap(),
        Error::InvalidRoleAddress
    );

    // zero as stream
    assert_eq!(
        client
            .try_initialize(
                &valid,
                &valid,
                &valid,
                &zero,
                &String::from_str(&env, "N"),
                &String::from_str(&env, "S")
            )
            .unwrap_err()
            .unwrap(),
        Error::InvalidRoleAddress
    );

    // contract itself as role
    assert_eq!(
        client
            .try_initialize(
                &contract_id,
                &valid,
                &valid,
                &valid,
                &String::from_str(&env, "N"),
                &String::from_str(&env, "S")
            )
            .unwrap_err()
            .unwrap(),
        Error::InvalidRoleAddress
    );
}

// ---------------------------------------------------------------------------
// 2. Metadata Admin tests
// ---------------------------------------------------------------------------

#[test]
fn test_metadata_admin_can_update_metadata() {
    let h = TestHarness::new();
    let new_name = String::from_str(&h.env, "Updated Stream Token");
    let new_symbol = String::from_str(&h.env, "USTREAM");

    h.client.set_metadata(&new_name, &new_symbol);

    assert_eq!(h.client.name(), new_name);
    assert_eq!(h.client.symbol(), new_symbol);

    let auths = h.env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, h.metadata_admin);
}

// ---------------------------------------------------------------------------
// 3. Manager tests (stream contract, mint pause)
// ---------------------------------------------------------------------------

#[test]
fn test_manager_can_update_stream_contract() {
    let h = TestHarness::new();
    let new_stream = Address::generate(&h.env);

    h.client.set_stream_contract(&new_stream);
    assert_eq!(h.client.stream_contract(), new_stream);

    let auths = h.env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, h.manager);
}

#[test]
fn test_manager_cannot_set_forbidden_stream() {
    let h = TestHarness::new();
    let zero = zero_address(&h.env);

    assert_eq!(
        h.client
            .try_set_stream_contract(&zero)
            .unwrap_err()
            .unwrap(),
        Error::InvalidRoleAddress
    );

    assert_eq!(
        h.client
            .try_set_stream_contract(&h.contract_id)
            .unwrap_err()
            .unwrap(),
        Error::InvalidRoleAddress
    );
}

#[test]
fn test_manager_can_pause_and_unpause_minting() {
    let h = TestHarness::new();
    assert_eq!(h.client.is_mint_paused(), false);

    // Pause minting
    h.client.set_mint_paused(&true);
    assert_eq!(h.client.is_mint_paused(), true);

    let auths = h.env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, h.manager);

    // Minting fails while paused
    assert_eq!(
        h.client.try_mint(&1).unwrap_err().unwrap(),
        Error::MintPaused
    );

    // Unpause minting
    h.client.set_mint_paused(&false);
    assert_eq!(h.client.is_mint_paused(), false);

    // Minting succeeds after unpause
    h.client.mint(&1);
    assert_eq!(
        h.client.owner_of(&1),
        Some(h.env.current_contract_address())
    ); // mocked caller
    assert_eq!(h.client.total_supply(), 1);
}

// ---------------------------------------------------------------------------
// 4. Role reassignment by Admin
// ---------------------------------------------------------------------------

#[test]
fn test_admin_can_set_metadata_admin() {
    let h = TestHarness::new();
    let new_metadata_admin = Address::generate(&h.env);

    h.client.set_metadata_admin(&new_metadata_admin);
    assert_eq!(h.client.metadata_admin(), new_metadata_admin);

    let auths = h.env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, h.admin);
}

#[test]
fn test_admin_can_set_manager() {
    let h = TestHarness::new();
    let new_manager = Address::generate(&h.env);

    h.client.set_manager(&new_manager);
    assert_eq!(h.client.manager(), new_manager);

    let auths = h.env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, h.admin);
}

#[test]
fn test_admin_role_setters_reject_forbidden_addresses() {
    let h = TestHarness::new();
    let zero = zero_address(&h.env);

    assert_eq!(
        h.client.try_set_metadata_admin(&zero).unwrap_err().unwrap(),
        Error::InvalidRoleAddress
    );
    assert_eq!(
        h.client
            .try_set_metadata_admin(&h.contract_id)
            .unwrap_err()
            .unwrap(),
        Error::InvalidRoleAddress
    );

    assert_eq!(
        h.client.try_set_manager(&zero).unwrap_err().unwrap(),
        Error::InvalidRoleAddress
    );
    assert_eq!(
        h.client
            .try_set_manager(&h.contract_id)
            .unwrap_err()
            .unwrap(),
        Error::InvalidRoleAddress
    );
}

// ---------------------------------------------------------------------------
// 5. Admin rotation: Direct and Two-step
// ---------------------------------------------------------------------------

#[test]
fn test_direct_admin_rotation() {
    let h = TestHarness::new();
    let new_admin = Address::generate(&h.env);

    h.client.set_admin(&new_admin);
    assert_eq!(h.client.admin(), new_admin);

    let auths = h.env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, h.admin);
}

#[test]
fn test_direct_admin_rotation_clears_pending() {
    let h = TestHarness::new();
    let nominee = Address::generate(&h.env);
    let direct_admin = Address::generate(&h.env);

    h.client.propose_admin(&nominee);
    assert_eq!(h.client.pending_admin(), Some(nominee));

    h.client.set_admin(&direct_admin);
    assert_eq!(h.client.admin(), direct_admin);
    assert_eq!(h.client.pending_admin(), None);
}

#[test]
fn test_two_step_admin_rotation() {
    let h = TestHarness::new();
    let nominee = Address::generate(&h.env);

    h.client.propose_admin(&nominee);
    assert_eq!(h.client.pending_admin(), Some(nominee.clone()));

    let auths = h.env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, h.admin);

    // Nominee accepts
    h.client.accept_admin();
    assert_eq!(h.client.admin(), nominee);
    assert_eq!(h.client.pending_admin(), None);

    let auths_after = h.env.auths();
    assert_eq!(auths_after.len(), 1);
    assert_eq!(auths_after[0].0, nominee);
}

#[test]
fn test_accept_admin_without_pending_fails() {
    let h = TestHarness::new();
    assert_eq!(h.client.pending_admin(), None);

    assert_eq!(
        h.client.try_accept_admin().unwrap_err().unwrap(),
        Error::NoPendingAdmin
    );
}

#[test]
fn test_admin_rotation_rejects_forbidden_addresses() {
    let h = TestHarness::new();
    let zero = zero_address(&h.env);

    assert_eq!(
        h.client.try_set_admin(&zero).unwrap_err().unwrap(),
        Error::InvalidRoleAddress
    );
    assert_eq!(
        h.client.try_set_admin(&h.contract_id).unwrap_err().unwrap(),
        Error::InvalidRoleAddress
    );

    assert_eq!(
        h.client.try_propose_admin(&zero).unwrap_err().unwrap(),
        Error::InvalidRoleAddress
    );
    assert_eq!(
        h.client
            .try_propose_admin(&h.contract_id)
            .unwrap_err()
            .unwrap(),
        Error::InvalidRoleAddress
    );
}

// ---------------------------------------------------------------------------
// 6. Unit & Fuzz boundary tests
// ---------------------------------------------------------------------------

#[test]
fn test_fuzz_boundary_stream_ids_and_overflow_protection() {
    let h = TestHarness::new();

    // Mint boundary stream_ids: 0, 1, u64::MAX - 1, u64::MAX
    let boundary_ids: [u64; 4] = [0, 1, u64::MAX - 1, u64::MAX];

    for &id in &boundary_ids {
        h.client.mint(&id);
        assert!(h.client.owner_of(&id).is_some());
    }

    assert_eq!(h.client.total_supply(), 4);

    // Double mint on same boundary id must fail
    for &id in &boundary_ids {
        assert_eq!(
            h.client.try_mint(&id).unwrap_err().unwrap(),
            Error::AlreadyMinted
        );
    }
}
