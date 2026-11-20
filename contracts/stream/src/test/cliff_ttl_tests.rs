//! Integration tests for [`crate::cliff_ttl`]: cliff-phase TTL automation.
//!
//! The unit tests in [`crate::cliff_ttl`] cover the pure functions in
//! isolation. These integration tests drive the full contract and assert that
//! the cliff-phase TTL policy is enforced end-to-end.

use soroban_sdk::testutils::storage::Persistent as _;
use soroban_sdk::testutils::Ledger as _;

use super::common::*;
use crate::{cliff_ttl, storage, DataKey};

/// Helper: read the current TTL of a stream entry.
fn ttl_of(h: &Harness, stream_id: u64) -> u32 {
    h.env.as_contract(&h.contract_id, || {
        h.env
            .storage()
            .persistent()
            .get_ttl(&DataKey::Stream(stream_id))
    })
}

// -----------------------------------------------------------------------
// Issue #408 — Creation: cliff-aware TTL is applied at create_stream
// -----------------------------------------------------------------------

/// A stream with no cliff (cliff_time == start_time) must receive the same
/// TTL at creation as the generic formula produces — the cliff-aware upgrade
/// path must be a no-op for no-cliff streams.
#[test]
fn no_cliff_stream_ttl_equals_generic_at_creation() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);
    let stream = h.get(id);

    let generic = storage::ttl_target_ledgers(&h.env, &stream);
    let actual = ttl_of(&h, id);

    assert_eq!(
        actual, generic,
        "no-cliff stream: cliff-aware TTL must equal generic TTL at creation",
    );
}

/// A stream with a cliff must have a TTL that covers the cliff wait at
/// creation, even if the generic schedule-based formula would give a shorter
/// window.
#[test]
fn cliff_stream_ttl_covers_cliff_wait_at_creation() {
    let h = Harness::new();
    let now = h.now();
    // Create a stream with a 100-day cliff and 50-day post-cliff schedule.
    // The generic formula would only cover the schedule end (now + 150 days).
    // The cliff-aware formula targets at least (cliff + buffer) = 100 days + 30 days.
    let cliff = now + 100 * DAY;
    let end = cliff + 50 * DAY;
    let id = h.create(1_000 * ONE, now, end, cliff, false, false, false);

    let actual_ttl = ttl_of(&h, id);

    // The cliff-aware target: at least seconds_to_ledgers(cliff_wait + buffer).
    let cliff_wait = 100 * DAY + storage::TTL_BUFFER_SECONDS;
    let min_ttl = storage::seconds_to_ledgers(cliff_wait)
        .min(storage::max_entry_ttl(&h.env));

    assert!(
        actual_ttl >= min_ttl,
        "cliff-aware TTL {actual_ttl} should be >= cliff-wait target {min_ttl}",
    );
}

// -----------------------------------------------------------------------
// Issue #408 — extend_cliff_stream_ttl: permissionless entry point
// -----------------------------------------------------------------------

/// The `extend_cliff_stream_ttl` entry point must succeed and return a
/// positive TTL for a live stream.
#[test]
fn extend_cliff_stream_ttl_entry_point_returns_positive_ttl() {
    let h = Harness::new();
    let now = h.now();
    let cliff = now + 60 * DAY;
    let id = h.create(1_000 * ONE, now, now + 120 * DAY, cliff, false, false, false);

    let result = h.client.extend_cliff_stream_ttl(&id);
    assert!(result > 0, "extend_cliff_stream_ttl must return a positive TTL");
}

/// `extend_cliff_stream_ttl` on an unknown id must return `StreamNotFound`.
#[test]
fn extend_cliff_stream_ttl_unknown_id_returns_stream_not_found() {
    let h = Harness::new();
    let err = h.client.try_extend_cliff_stream_ttl(&9999u64).unwrap_err().unwrap();
    assert_eq!(err, crate::Error::StreamNotFound);
}

/// Calling `extend_cliff_stream_ttl` must not change any stream field.
#[test]
fn extend_cliff_stream_ttl_does_not_mutate_stream_state() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);
    let before = h.get(id);

    h.client.extend_cliff_stream_ttl(&id);

    let after = h.get(id);
    assert_eq!(before.deposited, after.deposited);
    assert_eq!(before.withdrawn, after.withdrawn);
    assert_eq!(before.start_time, after.start_time);
    assert_eq!(before.end_time, after.end_time);
    assert_eq!(before.cliff_time, after.cliff_time);
    assert_eq!(before.status, after.status);
    assert_eq!(before.paused_at, after.paused_at);
    assert_eq!(before.paused_total, after.paused_total);
}

// -----------------------------------------------------------------------
// Issue #408 — is_in_cliff_phase utility
// -----------------------------------------------------------------------

/// A stream with an active cliff phase must report `is_in_cliff_phase == true`.
#[test]
fn is_in_cliff_phase_true_for_pre_cliff_stream() {
    let h = Harness::new();
    let now = h.now();
    let cliff = now + 50 * DAY;
    let id = h.create(1_000 * ONE, now, cliff + 50 * DAY, cliff, false, false, false);
    let stream = h.get(id);
    assert!(cliff_ttl::is_in_cliff_phase(&stream, now));
}

/// A stream with `cliff_time == start_time` (no cliff) must never be in the
/// cliff phase — this confirms the no-op behaviour of `ensure_cliff_phase_ttl`.
#[test]
fn is_in_cliff_phase_false_for_no_cliff_stream() {
    let h = Harness::new();
    let now = h.now();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);
    let stream = h.get(id);
    // For create_simple, cliff_time == start_time.
    assert!(!cliff_ttl::is_in_cliff_phase(&stream, now));
}

// -----------------------------------------------------------------------
// Pool invariant: extend does not break accounting
// -----------------------------------------------------------------------

/// After `extend_cliff_stream_ttl`, the pool invariant must still hold.
#[test]
fn extend_cliff_stream_ttl_preserves_pool_invariant() {
    let h = Harness::new();
    let now = h.now();
    let cliff = now + 30 * DAY;
    let id = h.create(1_000 * ONE, now, cliff + 30 * DAY, cliff, false, false, false);

    h.client.extend_cliff_stream_ttl(&id);
    h.assert_pool_exact();
}
