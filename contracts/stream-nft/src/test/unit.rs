//! Basic unit tests for the stream-nft contract.

use soroban_sdk::testutils::Address as _;
use soroban_sdk::Address;

use super::common::*;
use crate::Error;

#[test]
fn test_init_and_metadata() {
    let h = Harness::new();
    assert_eq!(h.client.decimals(), 0);
    assert_eq!(h.client.stream_contract(), h.stream_contract);
    assert_eq!(h.client.total_supply(), 0);
}

#[test]
fn test_mint_tracks_balance_and_supply() {
    let h = Harness::new();
    let stream_id = 100u64;

    assert_eq!(h.client.owner_of(&stream_id), None);
    assert_eq!(h.client.balance_of(&h.user1), 0);

    h.client.mint(&stream_id);

    assert_eq!(h.client.owner_of(&stream_id), Some(h.user1.clone()));
    assert_eq!(h.client.balance_of(&h.user1), 1);
    assert_eq!(h.client.total_supply(), 1);

    // Duplicate mint should fail
    let res = h.client.try_mint(&stream_id);
    assert_eq!(res, Err(Ok(Error::AlreadyMinted)));
}

#[test]
fn test_multiple_mints_for_same_user() {
    let h = Harness::new();
    h.client.mint(&1u64);
    h.client.mint(&2u64);

    assert_eq!(h.client.balance_of(&h.user1), 2);
    assert_eq!(h.client.total_supply(), 2);
}

#[test]
fn test_owner_of_unminted() {
    let h = Harness::new();
    assert_eq!(h.client.owner_of(&999u64), None);
}

#[test]
fn test_mint_at_u64_max_boundary() {
    let h = Harness::new();
    let max_id = u64::MAX;
    h.client.mint(&max_id);

    assert_eq!(h.client.owner_of(&max_id), Some(h.user1.clone()));
    assert_eq!(h.client.total_supply(), 1);
}
