//! Batch submission and batch decision tests.
//!
//! Covers issues #413 (loop deserialization efficiency) and #418 (cross-contract overhead).
//! The batch paths do zero cross-contract calls; `settle_claim` is exercised in `settle.rs`.

#![cfg(test)]

use soroban_sdk::Vec;

use crate::{ClaimStatus, DisputeError, MAX_BATCH_CLAIM_SIZE};

use super::common::*;

// ---------------------------------------------------------------------------
// batch_submit_claims
// ---------------------------------------------------------------------------

#[test]
fn test_batch_submit_creates_all_claims() {
    let h = Harness::new_open();

    let ids = Vec::from_slice(&h.env, &[10u64, 20, 30]);
    let amounts = Vec::from_slice(&h.env, &[100i128, 200, 300]);

    let submitted = h.client.batch_submit_claims(&h.claimant, &ids, &amounts);
    assert_eq!(submitted, 3);

    for (id, expected) in [(10u64, 100i128), (20, 200), (30, 300)] {
        let claim = h.client.get_claim(&id).unwrap();
        assert_eq!(claim.claimed_amount, expected);
        assert_eq!(claim.status, ClaimStatus::Pending);
    }
}

#[test]
fn test_batch_submit_empty_errors() {
    let h = Harness::new_open();
    let ids: Vec<u64> = Vec::new(&h.env);
    let amounts: Vec<i128> = Vec::new(&h.env);
    let result = h.client.try_batch_submit_claims(&h.claimant, &ids, &amounts);
    assert_eq!(result, Err(Ok(DisputeError::EmptyBatch)));
}

#[test]
fn test_batch_submit_too_large_errors() {
    let h = Harness::new_open();
    let n = MAX_BATCH_CLAIM_SIZE + 1;
    let ids: Vec<u64> = Vec::from_slice(&h.env, &(0u64..n as u64).collect::<std::vec::Vec<_>>());
    let amounts: Vec<i128> = Vec::from_slice(&h.env, &vec![100i128; n as usize]);
    let result = h.client.try_batch_submit_claims(&h.claimant, &ids, &amounts);
    assert_eq!(result, Err(Ok(DisputeError::BatchTooLarge)));
}

#[test]
fn test_batch_submit_duplicate_in_batch_errors() {
    let h = Harness::new_open();
    let ids = Vec::from_slice(&h.env, &[1u64, 2, 1]); // duplicate: stream_id 1
    let amounts = Vec::from_slice(&h.env, &[100i128, 200, 300]);
    let result = h.client.try_batch_submit_claims(&h.claimant, &ids, &amounts);
    assert_eq!(result, Err(Ok(DisputeError::DuplicateStreamId)));
}

#[test]
fn test_batch_submit_duplicate_across_calls_errors() {
    let h = Harness::new_open();
    // First batch succeeds.
    let ids1 = Vec::from_slice(&h.env, &[1u64, 2]);
    let amounts1 = Vec::from_slice(&h.env, &[100i128, 200]);
    h.client.batch_submit_claims(&h.claimant, &ids1, &amounts1);

    // Second batch references stream 1 again.
    let ids2 = Vec::from_slice(&h.env, &[1u64, 3]);
    let amounts2 = Vec::from_slice(&h.env, &[100i128, 300]);
    let result = h.client.try_batch_submit_claims(&h.claimant, &ids2, &amounts2);
    assert_eq!(result, Err(Ok(DisputeError::ClaimAlreadyExists)));
}

#[test]
fn test_batch_submit_zero_amount_errors() {
    let h = Harness::new_open();
    let ids = Vec::from_slice(&h.env, &[1u64, 2]);
    let amounts = Vec::from_slice(&h.env, &[100i128, 0]); // second amount is 0
    let result = h.client.try_batch_submit_claims(&h.claimant, &ids, &amounts);
    assert_eq!(result, Err(Ok(DisputeError::InvalidClaimAmount)));
}

#[test]
fn test_batch_submit_mismatched_lengths_errors() {
    let h = Harness::new_open();
    let ids = Vec::from_slice(&h.env, &[1u64, 2, 3]);
    let amounts = Vec::from_slice(&h.env, &[100i128, 200]); // only 2 amounts for 3 ids
    let result = h.client.try_batch_submit_claims(&h.claimant, &ids, &amounts);
    assert_eq!(result, Err(Ok(DisputeError::InvalidClaimAmount)));
}

#[test]
fn test_batch_submit_before_open_errors() {
    let h = Harness::new(); // not opened
    let ids = Vec::from_slice(&h.env, &[1u64]);
    let amounts = Vec::from_slice(&h.env, &[100i128]);
    let result = h.client.try_batch_submit_claims(&h.claimant, &ids, &amounts);
    assert_eq!(result, Err(Ok(DisputeError::WindowNotOpen)));
}

// ---------------------------------------------------------------------------
// batch_decide_claims (issue #413: validate-all-then-write pattern)
// ---------------------------------------------------------------------------

#[test]
fn test_batch_decide_approves_all() {
    let h = Harness::new_open();
    let ids = Vec::from_slice(&h.env, &[10u64, 20, 30]);
    let amounts = Vec::from_slice(&h.env, &[100i128, 200, 300]);
    h.client.batch_submit_claims(&h.claimant, &ids, &amounts);

    let approvals = Vec::from_slice(&h.env, &[true, true, false]);
    let approved_amounts = Vec::from_slice(&h.env, &[80i128, 200, 0]);
    let decided = h
        .client
        .batch_decide_claims(&ids, &approvals, &approved_amounts);
    assert_eq!(decided, 3);

    let c10 = h.client.get_claim(&10).unwrap();
    assert_eq!(c10.status, ClaimStatus::Approved);
    assert_eq!(c10.approved_amount, 80);

    let c30 = h.client.get_claim(&30).unwrap();
    assert_eq!(c30.status, ClaimStatus::Denied);
}

#[test]
fn test_batch_decide_already_decided_aborts_all() {
    let h = Harness::new_open();
    let ids = Vec::from_slice(&h.env, &[1u64, 2]);
    let amounts = Vec::from_slice(&h.env, &[100i128, 200]);
    h.client.batch_submit_claims(&h.claimant, &ids, &amounts);

    // Approve stream 1 first.
    h.client.decide_claim(&1, &true, &100, &None);

    // Batch decision that includes stream 1 must abort.
    let approvals = Vec::from_slice(&h.env, &[true, true]);
    let approved_amounts = Vec::from_slice(&h.env, &[100i128, 200]);
    let result = h
        .client
        .try_batch_decide_claims(&ids, &approvals, &approved_amounts);
    assert_eq!(result, Err(Ok(DisputeError::ClaimAlreadyExists)));

    // Stream 2 must still be Pending (batch rolled back).
    let c2 = h.client.get_claim(&2).unwrap();
    assert_eq!(c2.status, ClaimStatus::Pending);
}

#[test]
fn test_batch_decide_empty_errors() {
    let h = Harness::new_open();
    let empty_ids: Vec<u64> = Vec::new(&h.env);
    let empty_bools: Vec<bool> = Vec::new(&h.env);
    let empty_amounts: Vec<i128> = Vec::new(&h.env);
    let result = h
        .client
        .try_batch_decide_claims(&empty_ids, &empty_bools, &empty_amounts);
    assert_eq!(result, Err(Ok(DisputeError::EmptyBatch)));
}

#[test]
fn test_batch_decide_too_large_errors() {
    let h = Harness::new_open();
    let n = MAX_BATCH_CLAIM_SIZE + 1;
    let ids: Vec<u64> = Vec::from_slice(&h.env, &(0u64..n as u64).collect::<std::vec::Vec<_>>());
    let bools: Vec<bool> = Vec::from_slice(&h.env, &vec![true; n as usize]);
    let amounts: Vec<i128> = Vec::from_slice(&h.env, &vec![1i128; n as usize]);
    let result = h
        .client
        .try_batch_decide_claims(&ids, &bools, &amounts);
    assert_eq!(result, Err(Ok(DisputeError::BatchTooLarge)));
}

/// Verifies that the validate-all-then-write pattern (issue #413) prevents
/// partial writes: if validation fails on the second element, the first
/// element must not have been written.
#[test]
fn test_batch_decide_validation_failure_leaves_no_partial_writes() {
    let h = Harness::new_open();
    let ids = Vec::from_slice(&h.env, &[1u64, 2]);
    let amounts = Vec::from_slice(&h.env, &[100i128, 200]);
    h.client.batch_submit_claims(&h.claimant, &ids, &amounts);

    // Approve=true with amount=0 is invalid on the second element.
    let approvals = Vec::from_slice(&h.env, &[true, true]);
    let approved_amounts = Vec::from_slice(&h.env, &[100i128, 0]); // zero is invalid
    let result = h
        .client
        .try_batch_decide_claims(&ids, &approvals, &approved_amounts);
    assert_eq!(result, Err(Ok(DisputeError::InvalidClaimAmount)));

    // Stream 1 must still be Pending.
    let c1 = h.client.get_claim(&1).unwrap();
    assert_eq!(c1.status, ClaimStatus::Pending);
}
