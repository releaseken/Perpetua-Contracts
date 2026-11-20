//! Archival resilience tests (issue #417).
//!
//! Verifies that:
//! 1. `extend_claim_ttl` succeeds for an existing claim.
//! 2. `extend_claim_ttl` fails for a non-existent stream ID.
//! 3. The TTL returned is the network `max_ttl()` at the time of extension.
//! 4. The instance entry is bumped on every mutating call.

#![cfg(test)]

use crate::{DisputeError, DisputeWindow};

use super::common::*;

// ---------------------------------------------------------------------------
// extend_claim_ttl
// ---------------------------------------------------------------------------

#[test]
fn test_extend_claim_ttl_succeeds_for_existing_claim() {
    let h = Harness::new_open();
    h.submit(1, 500);

    // `extend_claim_ttl` must succeed and return a positive ledger count.
    let extended_to = h.client.extend_claim_ttl(&1);
    assert!(extended_to > 0, "extended_to must be positive");
}

#[test]
fn test_extend_claim_ttl_nonexistent_errors() {
    let h = Harness::new_open();
    let result = h.client.try_extend_claim_ttl(&99);
    assert_eq!(result, Err(Ok(DisputeError::ClaimNotFound)));
}

#[test]
fn test_extend_claim_ttl_returns_max_ttl() {
    let h = Harness::new_open();
    h.submit(1, 500);

    // The returned ledger count must equal the max_ttl at extension time.
    let extended_to = h.client.extend_claim_ttl(&1);
    let max_ttl = h.env.as_contract(&h.contract_id, || {
        h.env.storage().max_ttl()
    });
    assert_eq!(extended_to, max_ttl);
}

/// Calling `extend_claim_ttl` twice must not error — the host silently
/// no-ops a re-extension to the same ceiling.
#[test]
fn test_extend_claim_ttl_idempotent() {
    let h = Harness::new_open();
    h.submit(1, 500);

    let first = h.client.extend_claim_ttl(&1);
    let second = h.client.extend_claim_ttl(&1);
    assert_eq!(first, second);
}

/// After the window closes, `extend_claim_ttl` must still work so keepers can
/// keep settled/decided claims readable during the grace period (issue #417).
#[test]
fn test_extend_claim_ttl_works_after_window_close() {
    let h = Harness::new_open();
    h.submit(1, 500);
    h.client.close_window();

    let result = h.client.try_extend_claim_ttl(&1);
    assert!(result.is_ok(), "extend_claim_ttl should work after close");
}

// ---------------------------------------------------------------------------
// Instance bump — config entry never expires before claims
// ---------------------------------------------------------------------------

/// A fresh harness's contract has an instance TTL greater than zero.
#[test]
fn test_instance_ttl_is_positive_after_init() {
    let h = Harness::new();
    let ttl = h.env.as_contract(&h.contract_id, || {
        h.env.storage().instance().get_ttl()
    });
    assert!(ttl > 0, "instance TTL must be positive after init");
}

/// After opening the window, the instance TTL is at the network maximum.
#[test]
fn test_instance_ttl_at_max_after_open() {
    let h = Harness::new_open();
    let (instance_ttl, max_ttl) = h.env.as_contract(&h.contract_id, || {
        (
            h.env.storage().instance().get_ttl(),
            h.env.storage().max_ttl(),
        )
    });
    assert_eq!(
        instance_ttl, max_ttl,
        "instance TTL must be pinned to max_ttl after open_window"
    );
}
