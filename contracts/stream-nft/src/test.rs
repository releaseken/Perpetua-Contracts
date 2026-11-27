//! Invariant assertions, re-entrancy protection, and property tests for stream-nft.
#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger, MockAuth, MockAuthInvoke},
    vec,
};

const ZERO_ACCOUNT: &str = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";

fn zero_address(env: &Env) -> Address {
    Address::from_string(&String::from_str(env, ZERO_ACCOUNT))
}

#[contract]
struct MockStreamContract;

#[contractimpl]
impl MockStreamContract {
    pub fn transfer_recipient(env: Env, stream_id: u64, to: Address) {
        // If stream_id == 999, simulate stream contract rejection
        if stream_id == 999 {
            panic_with_error!(&env, Error::StreamRejected);
        }
        let _ = to;
    }
}

/// Mock stream contract that supports optimized batch claims / recipient transfers (#373)
#[contract]
struct MockBatchStreamContract;

#[contractimpl]
impl MockBatchStreamContract {
    pub fn transfer_recipient(env: Env, stream_id: u64, to: Address) {
        if stream_id == 999 {
            panic_with_error!(&env, Error::StreamRejected);
        }
        let _ = to;
    }

    pub fn batch_transfer_recipient(env: Env, stream_ids: Vec<u64>, to: Address) -> u32 {
        let n = stream_ids.len();
        for i in 0..n {
            if stream_ids.get_unchecked(i) == 999 {
                panic_with_error!(&env, Error::StreamRejected);
            }
        }
        let _ = to;
        n
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

fn setup_nft<'a>() -> (Env, StreamNftClient<'a>, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let stream_id = env.register(MockStreamContract, ());
    let nft_id = env.register(StreamNft, ());
    let client = StreamNftClient::new(&env, &nft_id);

    let name = String::from_str(&env, "Perpetua Stream NFT");
    let symbol = String::from_str(&env, "PSTREAM");
    client.init(&stream_id, &name, &symbol);

    let user_a = Address::generate(&env);
    let user_b = Address::generate(&env);

    (env, client, stream_id, user_a, user_b)
}

fn setup_batch_stream_nft<'a>() -> (Env, StreamNftClient<'a>, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let stream_id = env.register(MockBatchStreamContract, ());
    let nft_id = env.register(StreamNft, ());
    let client = StreamNftClient::new(&env, &nft_id);

    let name = String::from_str(&env, "Perpetua Stream NFT");
    let symbol = String::from_str(&env, "PSTREAM");
    client.init(&stream_id, &name, &symbol);

    let user_a = Address::generate(&env);
    let user_b = Address::generate(&env);

    (env, client, stream_id, user_a, user_b)
}

#[test]
fn test_init_and_metadata() {
    let (env, client, stream_id, _, _) = setup_nft();
    assert_eq!(client.name(), String::from_str(&env, "Perpetua Stream NFT"));
    assert_eq!(client.symbol(), String::from_str(&env, "PSTREAM"));
    assert_eq!(client.decimals(), 0);
    assert_eq!(client.stream_contract(), stream_id);
    assert_eq!(client.total_supply(), 0);
}

#[test]
fn test_single_mint_and_transfer() {
    let (env, client, _, user_a, user_b) = setup_nft();

    env.ledger().set_timestamp(100);
    client.mint(&1);

    assert_eq!(client.owner_of(&1), Some(user_a.clone()));
    assert_eq!(client.balance_of(&user_a), 1);
    assert_eq!(client.total_supply(), 1);

    client.transfer(&1, &user_b);
    assert_eq!(client.owner_of(&1), Some(user_b.clone()));
    assert_eq!(client.balance_of(&user_a), 0);
    assert_eq!(client.balance_of(&user_b), 1);
}

#[test]
fn test_batch_owner_of_single_pass() {
    let (env, client, _, user_a, _) = setup_nft();

    client.mint(&1);
    client.mint(&2);
    client.mint(&3);

    let ids = vec![&env, 1u64, 2u64, 3u64, 4u64];
    let owners = client.batch_owner_of(&ids);

    assert_eq!(owners.len(), 4);
    assert_eq!(owners.get(0).unwrap(), Some(user_a.clone()));
    assert_eq!(owners.get(1).unwrap(), Some(user_a.clone()));
    assert_eq!(owners.get(2).unwrap(), Some(user_a));
    assert_eq!(owners.get(3).unwrap(), None);
}

#[test]
fn test_batch_mint_success_and_solvency() {
    let (env, client, _, user_a, _) = setup_nft();

    let ids = vec![&env, 10u64, 11u64, 12u64, 13u64];
    let count = client.batch_mint(&ids);
    assert_eq!(count, 4);

    assert_eq!(client.total_supply(), 4);
    assert_eq!(client.balance_of(&user_a), 4);
    assert_eq!(client.owner_of(&10), Some(user_a.clone()));
    assert_eq!(client.owner_of(&13), Some(user_a));
}

#[test]
fn test_batch_transfer_success_and_pruning_sequential_fallback() {
    let (env, client, _, user_a, user_b) = setup_nft();

    let ids = vec![&env, 100u64, 101u64, 102u64];
    client.batch_mint(&ids);

    assert_eq!(client.balance_of(&user_a), 3);
    assert_eq!(client.balance_of(&user_b), 0);

    let transferred = client.batch_transfer(&ids, &user_a, &user_b);
    assert_eq!(transferred, 3);

    assert_eq!(client.balance_of(&user_a), 0);
    assert_eq!(client.balance_of(&user_b), 3);
    assert_eq!(client.owner_of(&100), Some(user_b.clone()));
    assert_eq!(client.owner_of(&101), Some(user_b.clone()));
    assert_eq!(client.owner_of(&102), Some(user_b));
}

#[test]
fn test_batch_transfer_streamlined_cross_contract_coalescing() {
    // Issue #373: tests coalesced batch cross-contract invocation
    let (env, client, _, user_a, user_b) = setup_batch_stream_nft();

    let ids = vec![&env, 200u64, 201u64, 202u64, 203u64];
    client.batch_mint(&ids);

    assert_eq!(client.balance_of(&user_a), 4);
    assert_eq!(client.balance_of(&user_b), 0);

    let transferred = client.batch_transfer(&ids, &user_a, &user_b);
    assert_eq!(transferred, 4);

    assert_eq!(client.balance_of(&user_a), 0);
    assert_eq!(client.balance_of(&user_b), 4);
    assert_eq!(client.owner_of(&200), Some(user_b.clone()));
    assert_eq!(client.owner_of(&203), Some(user_b));
}

#[test]
fn test_batch_transfer_streamlined_rejection_aborts_all() {
    // Issue #373: tests rejection in coalesced batch invocation rolls back state
    let (env, client, _, user_a, user_b) = setup_batch_stream_nft();

    client.mint(&300);
    client.mint(&999);

    let ids = vec![&env, 300u64, 999u64];
    let res = client.try_batch_transfer(&ids, &user_a, &user_b);
    assert!(res.is_err());

    // Verified: token 300 must still be owned by user_a (transaction rolled back)
    assert_eq!(client.owner_of(&300), Some(user_a.clone()));
    assert_eq!(client.balance_of(&user_a), 2);
    assert_eq!(client.balance_of(&user_b), 0);
}

#[test]
fn test_batch_rejects_empty() {
    let (env, client, _, user_a, user_b) = setup_nft();
    let empty_ids = vec![&env];

    assert_eq!(
        client.try_batch_owner_of(&empty_ids).unwrap_err().unwrap(),
        Error::EmptyBatch
    );
    assert_eq!(
        client.try_batch_mint(&empty_ids).unwrap_err().unwrap(),
        Error::EmptyBatch
    );
    assert_eq!(
        client
            .try_batch_transfer(&empty_ids, &user_a, &user_b)
            .unwrap_err()
            .unwrap(),
        Error::EmptyBatch
    );
}

#[test]
fn test_batch_rejects_too_large() {
    let (env, client, _, user_a, user_b) = setup_nft();
    let mut oversized = vec![&env];
    for i in 0..=(MAX_BATCH_SIZE as u64) {
        oversized.push_back(i);
    }

    assert_eq!(
        client.try_batch_owner_of(&oversized).unwrap_err().unwrap(),
        Error::BatchTooLarge
    );
    assert_eq!(
        client.try_batch_mint(&oversized).unwrap_err().unwrap(),
        Error::BatchTooLarge
    );
    assert_eq!(
        client
            .try_batch_transfer(&oversized, &user_a, &user_b)
            .unwrap_err()
            .unwrap(),
        Error::BatchTooLarge
    );
}

#[test]
fn test_batch_rejects_duplicate_ids() {
    let (env, client, _, user_a, user_b) = setup_nft();
    let dup_ids = vec![&env, 1u64, 2u64, 1u64];

    assert_eq!(
        client.try_batch_owner_of(&dup_ids).unwrap_err().unwrap(),
        Error::DuplicateTokenId
    );
    assert_eq!(
        client.try_batch_mint(&dup_ids).unwrap_err().unwrap(),
        Error::DuplicateTokenId
    );
    assert_eq!(
        client
            .try_batch_transfer(&dup_ids, &user_a, &user_b)
            .unwrap_err()
            .unwrap(),
        Error::DuplicateTokenId
    );
}

#[test]
fn test_batch_transfer_rejects_not_owner_or_unminted() {
    let (env, client, _, user_a, user_b) = setup_nft();

    client.mint(&50);
    // 50 owned by user_a, 51 unminted
    let mixed_ids = vec![&env, 50u64, 51u64];
    assert_eq!(
        client
            .try_batch_transfer(&mixed_ids, &user_a, &user_b)
            .unwrap_err()
            .unwrap(),
        Error::TokenNotFound
    );

    let other_user = Address::generate(&env);
    assert_eq!(
        client
            .try_batch_transfer(&vec![&env, 50u64], &other_user, &user_b)
            .unwrap_err()
            .unwrap(),
        Error::NotOwner
    );
}

#[test]
fn test_batch_transfer_stream_rejection_aborts_all() {
    let (env, client, _, user_a, user_b) = setup_nft();

    // Stream 999 triggers rejection in MockStreamContract
    client.mint(&10);
    client.mint(&999);

    let ids = vec![&env, 10u64, 999u64];
    let res = client.try_batch_transfer(&ids, &user_a, &user_b);
    assert!(res.is_err());

    // Verified: token 10 must still be owned by user_a (transaction rolled back)
    assert_eq!(client.owner_of(&10), Some(user_a.clone()));
    assert_eq!(client.balance_of(&user_a), 2);
    assert_eq!(client.balance_of(&user_b), 0);
}

#[test]
fn test_modular_error_metadata_and_observability() {
    // Issue #371: verify error groups, messages, and classifications
    let all_errors = [
        Error::AlreadyInitialized,
        Error::NotInitialized,
        Error::AlreadyMinted,
        Error::TokenNotFound,
        Error::NotOwner,
        Error::StreamRejected,
        Error::EmptyBatch,
        Error::BatchTooLarge,
        Error::DuplicateTokenId,
        Error::MalformedTokenId,
        Error::ArithmeticOverflow,
        Error::Unauthorized,
        Error::StreamCallFailed,
        Error::ReentrancyGuard,
        Error::InvariantViolation,
        Error::RepeatedTransfer,
        Error::Overflow,
        Error::InvalidRoleAddress,
        Error::NoPendingAdmin,
        Error::MintPaused,
    ];

    for err in all_errors.iter() {
        // Assert valid group categorization
        let grp = err.group();
        assert!(matches!(grp, 'A' | 'B' | 'C' | 'D' | 'E' | 'F'));

        // Assert message is descriptive and non-empty
        let msg = err.message();
        assert!(!msg.is_empty());
        assert!(msg.len() > 10);
    }

    // Specific group verifications
    assert_eq!(Error::AlreadyInitialized.group(), 'A');
    assert_eq!(Error::NotInitialized.group(), 'A');
    assert_eq!(Error::AlreadyMinted.group(), 'B');
    assert_eq!(Error::TokenNotFound.group(), 'B');
    assert_eq!(Error::NotOwner.group(), 'C');
    assert_eq!(Error::Unauthorized.group(), 'C');
    assert_eq!(Error::InvalidRoleAddress.group(), 'C');
    assert_eq!(Error::NoPendingAdmin.group(), 'C');
    assert_eq!(Error::StreamRejected.group(), 'D');
    assert_eq!(Error::StreamCallFailed.group(), 'D');
    assert_eq!(Error::EmptyBatch.group(), 'E');
    assert_eq!(Error::BatchTooLarge.group(), 'E');
    assert_eq!(Error::DuplicateTokenId.group(), 'E');
    assert_eq!(Error::MalformedTokenId.group(), 'E');
    assert_eq!(Error::ArithmeticOverflow.group(), 'F');
    assert_eq!(Error::MintPaused.group(), 'E');

    // Classification helpers
    assert!(Error::StreamRejected.is_cross_contract());
    assert!(Error::StreamCallFailed.is_cross_contract());
    assert!(!Error::NotOwner.is_cross_contract());

    assert!(Error::EmptyBatch.is_client_validation());
    assert!(Error::BatchTooLarge.is_client_validation());
    assert!(Error::DuplicateTokenId.is_client_validation());
    assert!(Error::TokenNotFound.is_client_validation());
    assert!(Error::NotOwner.is_client_validation());
    assert!(Error::InvalidRoleAddress.is_client_validation());
    assert!(!Error::StreamRejected.is_client_validation());
}

#[test]
fn test_fuzz_batch_bounds_and_invariants() {
    use crate::invariants::*;

    // Bounds check
    assert!(!verify_batch_bounds(0));
    assert!(verify_batch_bounds(1));
    assert!(verify_batch_bounds(MAX_BATCH_SIZE));
    assert!(!verify_batch_bounds(MAX_BATCH_SIZE + 1));

    // Supply increment check
    assert!(verify_supply_increment(10, 15, 5));
    assert!(!verify_supply_increment(10, 16, 5));
    assert!(!verify_supply_increment(u64::MAX, 0, 1));

    // Transfer conservation
    assert!(verify_transfer_balance_conservation(5, 2, 3, 6, 3));
    assert!(!verify_transfer_balance_conservation(5, 1, 3, 6, 3));

    // Cross-contract call coalescing verification (#373)
    assert!(verify_cross_contract_coalescing(true, 16, 1));
    assert!(!verify_cross_contract_coalescing(true, 16, 16));
    assert!(verify_cross_contract_coalescing(false, 16, 16));
    assert!(!verify_cross_contract_coalescing(false, 16, 1));
}

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
