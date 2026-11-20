//! Single-claim submission and arbiter decision tests (issue #416: error observability).

#![cfg(test)]

use crate::{ClaimStatus, DisputeError, WindowStatus};

use super::common::*;

// ---------------------------------------------------------------------------
// submit_claim
// ---------------------------------------------------------------------------

#[test]
fn test_submit_claim_creates_pending_entry() {
    let h = Harness::new_open();
    h.submit(1, 500);

    let claim = h.client.get_claim(&1).unwrap();
    assert_eq!(claim.stream_id, 1);
    assert_eq!(claim.claimed_amount, 500);
    assert_eq!(claim.status, ClaimStatus::Pending);
    assert_eq!(claim.claimant, h.claimant);
}

#[test]
fn test_submit_claim_before_open_errors() {
    let h = Harness::new(); // not opened
    let result = h.client.try_submit_claim(&h.claimant, &1, &500);
    assert_eq!(result, Err(Ok(DisputeError::WindowNotOpen)));
}

#[test]
fn test_submit_claim_after_deadline_errors() {
    let h = Harness::new_open();
    h.advance(WEEK + 1); // past deadline
    let result = h.client.try_submit_claim(&h.claimant, &1, &500);
    assert_eq!(result, Err(Ok(DisputeError::ClaimAfterWindowClose)));
}

#[test]
fn test_submit_claim_zero_amount_errors() {
    let h = Harness::new_open();
    let result = h.client.try_submit_claim(&h.claimant, &1, &0);
    assert_eq!(result, Err(Ok(DisputeError::InvalidClaimAmount)));
}

#[test]
fn test_submit_claim_negative_amount_errors() {
    let h = Harness::new_open();
    let result = h.client.try_submit_claim(&h.claimant, &1, &-1);
    assert_eq!(result, Err(Ok(DisputeError::InvalidClaimAmount)));
}

#[test]
fn test_submit_claim_duplicate_errors() {
    let h = Harness::new_open();
    h.submit(1, 500);
    let result = h.client.try_submit_claim(&h.claimant, &1, &200);
    assert_eq!(result, Err(Ok(DisputeError::ClaimAlreadyExists)));
}

#[test]
fn test_has_claim_true_after_submit() {
    let h = Harness::new_open();
    assert!(!h.client.has_claim(&1));
    h.submit(1, 500);
    assert!(h.client.has_claim(&1));
}

// ---------------------------------------------------------------------------
// decide_claim
// ---------------------------------------------------------------------------

#[test]
fn test_approve_claim_sets_approved_status() {
    let h = Harness::new_open();
    h.submit(1, 500);
    h.approve(1, 400);

    let claim = h.client.get_claim(&1).unwrap();
    assert_eq!(claim.status, ClaimStatus::Approved);
    assert_eq!(claim.approved_amount, 400);
}

#[test]
fn test_deny_claim_sets_denied_status() {
    let h = Harness::new_open();
    h.submit(1, 500);
    h.deny(1);

    let claim = h.client.get_claim(&1).unwrap();
    assert_eq!(claim.status, ClaimStatus::Denied);
    assert_eq!(claim.approved_amount, 0);
}

#[test]
fn test_decide_nonexistent_claim_errors() {
    let h = Harness::new_open();
    let result = h.client.try_decide_claim(&99, &true, &500, &None);
    assert_eq!(result, Err(Ok(DisputeError::ClaimNotFound)));
}

#[test]
fn test_decide_claim_twice_errors() {
    let h = Harness::new_open();
    h.submit(1, 500);
    h.approve(1, 400);
    // Second decision attempt is rejected.
    let result = h.client.try_decide_claim(&1, &false, &0, &None);
    assert_eq!(result, Err(Ok(DisputeError::ClaimAlreadyExists)));
}

#[test]
fn test_approve_zero_amount_errors() {
    let h = Harness::new_open();
    h.submit(1, 500);
    let result = h.client.try_decide_claim(&1, &true, &0, &None);
    assert_eq!(result, Err(Ok(DisputeError::InvalidClaimAmount)));
}

#[test]
fn test_decide_after_finalise_errors() {
    let h = Harness::new_open();
    h.submit(1, 500);
    h.client.finalise_window();
    let result = h.client.try_decide_claim(&1, &true, &500, &None);
    assert_eq!(result, Err(Ok(DisputeError::WindowFinalised)));
}

#[test]
fn test_decide_with_note() {
    let h = Harness::new_open();
    h.submit(1, 500);
    let note = soroban_sdk::String::from_str(&h.env, "approved after review");
    h.client.decide_claim(&1, &true, &400, &Some(note.clone()));
    let claim = h.client.get_claim(&1).unwrap();
    assert_eq!(claim.arbiter_note, Some(note));
}
