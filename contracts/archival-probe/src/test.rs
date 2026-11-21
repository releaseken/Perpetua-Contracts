//! Comprehensive unit, fuzz, re-entrancy, and invariant test suite for archival-probe.

use super::*;
use proptest::prelude::*;
use soroban_sdk::symbol_short;
use soroban_sdk::testutils::storage::Instance as _;
use soroban_sdk::testutils::storage::Persistent as _;
use soroban_sdk::testutils::Ledger as _;
use soroban_sdk::Address;

pub struct TestHarness<'a> {
    pub env: Env,
    pub contract_id: Address,
    pub client: ArchivalProbeClient<'a>,
}

impl<'a> TestHarness<'a> {
    pub fn new() -> Self {
        let env = Env::default();
        let contract_id = env.register(ArchivalProbe, ());
        let client = ArchivalProbeClient::new(&env, &contract_id);
        TestHarness {
            env,
            contract_id,
            client,
        }
    }

    pub fn advance_ledgers(&self, ledgers: u32) {
        let current = self.env.ledger().sequence();
        self.env.ledger().set_sequence_number(current + ledgers);
    }
}

// ===========================================================================
// BASIC ARCHIVAL & PROBE ROUND TRIP
// ===========================================================================

#[test]
fn plant_and_read_round_trip() {
    let h = TestHarness::new();

    assert!(!h.client.planted());
    assert_eq!(
        h.client.try_read().unwrap_err().unwrap(),
        ProbeError::NotPlanted
    );
    assert_eq!(h.client.probe_count(), 0);

    h.client.plant(&symbol_short!("canary"));
    assert!(h.client.planted());
    assert_eq!(h.client.read(), symbol_short!("canary"));
    assert_eq!(h.client.probe_count(), 1);
}

#[test]
fn retry_does_not_replace_note_or_refresh_ttl() {
    let h = TestHarness::new();

    h.client.plant(&symbol_short!("first"));
    assert_eq!(h.client.probe_count(), 1);

    let planted_at = h.env.ledger().sequence();
    h.env.ledger().set_sequence_number(planted_at + 100);
    let ttl_before_retry = h.env.as_contract(&h.contract_id, || {
        h.env.storage().persistent().get_ttl(&Key::Canary)
    });

    h.client.plant(&symbol_short!("retry"));

    let ttl_after_retry = h.env.as_contract(&h.contract_id, || {
        h.env.storage().persistent().get_ttl(&Key::Canary)
    });
    assert_eq!(ttl_after_retry, ttl_before_retry);
    assert_eq!(h.client.read(), symbol_short!("first"));
    // Count remains 1 because retry was idempotent
    assert_eq!(h.client.probe_count(), 1);
}

#[test]
fn the_probe_does_not_extend_its_own_ttl() {
    let h = TestHarness::new();
    h.client.plant(&symbol_short!("canary"));

    let ttl = h.env.as_contract(&h.contract_id, || {
        h.env.storage().persistent().get_ttl(&Key::Canary)
    });
    let min = h.env.ledger().get().min_persistent_entry_ttl;

    assert!(
        ttl < min,
        "probe TTL {ttl} exceeds the network minimum {min}; something is \
         extending it and the probe will not archive on schedule",
    );
}

// ===========================================================================
// AUTOMATED ENTRY LIFETIME EXTENSION (ISSUE #381)
// ===========================================================================

#[test]
fn instance_lifetime_is_automatically_extended_on_mutations() {
    let h = TestHarness::new();
    h.client.plant(&symbol_short!("c1"));

    let instance_ttl = h
        .env
        .as_contract(&h.contract_id, || h.env.storage().instance().get_ttl());
    assert!(instance_ttl > 0, "Instance TTL should be refreshed");

    // Plant probe also refreshes instance TTL
    h.advance_ledgers(50);
    h.client.plant_probe(&42, &symbol_short!("p42"));
    let instance_ttl_after = h
        .env
        .as_contract(&h.contract_id, || h.env.storage().instance().get_ttl());
    assert!(instance_ttl_after >= instance_ttl);
}

#[test]
fn extend_probe_lifetime_refreshes_persistent_entry() {
    let h = TestHarness::new();
    h.client.plant_probe(&100, &symbol_short!("target"));

    let initial_ttl = h.env.as_contract(&h.contract_id, || {
        h.env.storage().persistent().get_ttl(&Key::Probe(100))
    });

    h.advance_ledgers(10_000);

    let aged_ttl = h.env.as_contract(&h.contract_id, || {
        h.env.storage().persistent().get_ttl(&Key::Probe(100))
    });
    assert!(aged_ttl < initial_ttl);

    // Call extend_probe_lifetime
    h.client.extend_probe_lifetime(&100);

    let extended_ttl = h.env.as_contract(&h.contract_id, || {
        h.env.storage().persistent().get_ttl(&Key::Probe(100))
    });
    assert!(
        extended_ttl > aged_ttl,
        "Persistent probe TTL should be extended"
    );

    // Check non-existent probe extension fails with NotPlanted
    let err = h
        .client
        .try_extend_probe_lifetime(&999)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ProbeError::NotPlanted);
}

#[test]
fn batch_extend_probe_lifetime_sweeps_resilient_entries() {
    let h = TestHarness::new();
    let mut ids = Vec::new(&h.env);
    let mut notes = Vec::new(&h.env);

    for i in 1..=5 {
        ids.push_back(i);
        notes.push_back(symbol_short!("probe"));
    }
    h.client.batch_plant(&ids, &notes);

    h.advance_ledgers(5_000);

    // Batch extend probe lifetime with valid and non-existent IDs
    let mut sweep_ids = Vec::new(&h.env);
    sweep_ids.push_back(1);
    sweep_ids.push_back(3);
    sweep_ids.push_back(999); // unplanted, should skip

    let extended = h.client.batch_extend_probe_lifetime(&sweep_ids);
    assert_eq!(extended, 2);

    // Empty batch returns EmptyBatch error
    let empty_vec = Vec::new(&h.env);
    let err = h
        .client
        .try_batch_extend_probe_lifetime(&empty_vec)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ProbeError::EmptyBatch);

    // Oversized batch returns BatchTooLarge error
    let mut oversized = Vec::new(&h.env);
    for i in 0..=MAX_BATCH_PROBE_SIZE {
        oversized.push_back(i);
    }
    let err_large = h
        .client
        .try_batch_extend_probe_lifetime(&oversized)
        .unwrap_err()
        .unwrap();
    assert_eq!(err_large, ProbeError::BatchTooLarge);
}

// ===========================================================================
// RE-ENTRANCY & CONCURRENCY INVARIANT ENFORCEMENT (ISSUE #379)
// ===========================================================================

#[test]
fn executing_flag_is_cleared_after_successful_plant() {
    let h = TestHarness::new();
    h.client.plant(&symbol_short!("item1"));

    let flag: Option<bool> = h.env.as_contract(&h.contract_id, || {
        h.env.storage().instance().get(&Key::Executing)
    });
    assert!(
        flag.is_none() || flag == Some(false),
        "Executing lock must be cleared after plant, got {:?}",
        flag
    );
    assert!(!h.client.is_executing());
}

#[test]
fn executing_flag_is_cleared_after_successful_plant_probe() {
    let h = TestHarness::new();
    h.client.plant_probe(&10, &symbol_short!("probe10"));

    let flag: Option<bool> = h.env.as_contract(&h.contract_id, || {
        h.env.storage().instance().get(&Key::Executing)
    });
    assert!(
        flag.is_none() || flag == Some(false),
        "Executing lock must be cleared after plant_probe, got {:?}",
        flag
    );
    assert!(!h.client.is_executing());
}

#[test]
fn executing_flag_is_cleared_after_successful_batch_plant() {
    let h = TestHarness::new();
    let mut ids = Vec::new(&h.env);
    let mut notes = Vec::new(&h.env);
    ids.push_back(1);
    notes.push_back(symbol_short!("n1"));

    h.client.batch_plant(&ids, &notes);

    let flag: Option<bool> = h.env.as_contract(&h.contract_id, || {
        h.env.storage().instance().get(&Key::Executing)
    });
    assert!(
        flag.is_none() || flag == Some(false),
        "Executing lock must be cleared after batch_plant, got {:?}",
        flag
    );
    assert!(!h.client.is_executing());
}

#[test]
fn reentrancy_guard_rejects_concurrent_plant_when_locked() {
    let h = TestHarness::new();

    // Manually simulate an active re-entrancy lock
    h.env.as_contract(&h.contract_id, || {
        h.env.storage().instance().set(&Key::Executing, &true);
    });

    assert!(h.client.is_executing());

    // Attempting to call plant while executing lock is held must fail with ReentrancyGuard
    let err = h
        .client
        .try_plant(&symbol_short!("reentrant"))
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ProbeError::ReentrancyGuard);

    // Attempting to call plant_probe must also fail
    let err_probe = h
        .client
        .try_plant_probe(&99, &symbol_short!("reentrant"))
        .unwrap_err()
        .unwrap();
    assert_eq!(err_probe, ProbeError::ReentrancyGuard);

    // Attempting to call batch_plant must also fail
    let mut ids = Vec::new(&h.env);
    let mut notes = Vec::new(&h.env);
    ids.push_back(1);
    notes.push_back(symbol_short!("n1"));
    let err_batch = h.client.try_batch_plant(&ids, &notes).unwrap_err().unwrap();
    assert_eq!(err_batch, ProbeError::ReentrancyGuard);

    // Attempting to call extend_probe_lifetime must also fail
    let err_ext = h.client.try_extend_probe_lifetime(&1).unwrap_err().unwrap();
    assert_eq!(err_ext, ProbeError::ReentrancyGuard);

    // Attempting to call batch_extend_probe_lifetime must also fail
    let err_batch_ext = h
        .client
        .try_batch_extend_probe_lifetime(&ids)
        .unwrap_err()
        .unwrap();
    assert_eq!(err_batch_ext, ProbeError::ReentrancyGuard);

    // Release lock; subsequent operation succeeds
    h.env.as_contract(&h.contract_id, || {
        h.env.storage().instance().remove(&Key::Executing);
    });
    assert!(!h.client.is_executing());

    h.client.plant(&symbol_short!("resumed"));
    assert_eq!(h.client.read(), symbol_short!("resumed"));
}

#[test]
fn executing_flag_cleared_on_batch_validation_error() {
    let h = TestHarness::new();
    let empty_ids = Vec::new(&h.env);
    let empty_notes = Vec::new(&h.env);

    let err = h
        .client
        .try_batch_plant(&empty_ids, &empty_notes)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ProbeError::EmptyBatch);

    assert!(!h.client.is_executing());
}

// ===========================================================================
// STATE DESYNC & CONSERVATION INVARIANTS (ISSUE #379)
// ===========================================================================

#[test]
fn state_sync_preserves_exact_counter_advancement() {
    let h = TestHarness::new();
    assert_eq!(h.client.probe_count(), 0);

    h.client.plant(&symbol_short!("c"));
    assert_eq!(h.client.probe_count(), 1);

    h.client.plant_probe(&1, &symbol_short!("p1"));
    assert_eq!(h.client.probe_count(), 2);

    h.client.plant_probe(&2, &symbol_short!("p2"));
    assert_eq!(h.client.probe_count(), 3);

    // Replay duplicate probe 1; counter must not change
    h.client.plant_probe(&1, &symbol_short!("dup"));
    assert_eq!(h.client.probe_count(), 3);

    // Batch plant 3 new items and 1 duplicate
    let mut ids = Vec::new(&h.env);
    let mut notes = Vec::new(&h.env);
    ids.push_back(2); // duplicate
    notes.push_back(symbol_short!("dup2"));
    ids.push_back(4);
    notes.push_back(symbol_short!("p4"));
    ids.push_back(5);
    notes.push_back(symbol_short!("p5"));
    ids.push_back(6);
    notes.push_back(symbol_short!("p6"));

    let planted = h.client.batch_plant(&ids, &notes);
    assert_eq!(planted, 3);
    assert_eq!(h.client.probe_count(), 6);
}

#[test]
fn invariant_violation_triggers_when_state_desync_detected() {
    // Test the invariants::assert_state_sync directly
    assert!(invariants::assert_state_sync(10, 15, 5).is_ok());
    assert_eq!(
        invariants::assert_state_sync(10, 14, 5).unwrap_err(),
        ProbeError::InvariantViolation
    );
    assert_eq!(
        invariants::assert_state_sync(10, 16, 5).unwrap_err(),
        ProbeError::InvariantViolation
    );
    assert_eq!(
        invariants::assert_state_sync(u32::MAX, u32::MAX, 1).unwrap_err(),
        ProbeError::ArithmeticOverflow
    );
}

// ===========================================================================
// BATCH BOUNDS & HIGH-FREQUENCY EFFICIENCY
// ===========================================================================

#[test]
fn batch_size_bounds_and_error_handling() {
    let h = TestHarness::new();

    // 1. Empty batch
    let empty_ids = Vec::new(&h.env);
    let empty_notes = Vec::new(&h.env);
    assert_eq!(
        h.client
            .try_batch_plant(&empty_ids, &empty_notes)
            .unwrap_err()
            .unwrap(),
        ProbeError::EmptyBatch
    );
    assert_eq!(
        h.client.try_batch_read(&empty_ids).unwrap_err().unwrap(),
        ProbeError::EmptyBatch
    );

    // 2. Length mismatch
    let mut ids = Vec::new(&h.env);
    ids.push_back(1);
    ids.push_back(2);
    let mut notes = Vec::new(&h.env);
    notes.push_back(symbol_short!("note1"));
    assert_eq!(
        h.client.try_batch_plant(&ids, &notes).unwrap_err().unwrap(),
        ProbeError::BatchTooLarge
    );

    // 3. Exactly MAX_BATCH_PROBE_SIZE (32) succeeds
    let mut max_ids = Vec::new(&h.env);
    let mut max_notes = Vec::new(&h.env);
    for i in 0..MAX_BATCH_PROBE_SIZE {
        max_ids.push_back(i);
        max_notes.push_back(symbol_short!("probe"));
    }
    let count = h.client.batch_plant(&max_ids, &max_notes);
    assert_eq!(count, MAX_BATCH_PROBE_SIZE);
    assert_eq!(h.client.probe_count(), MAX_BATCH_PROBE_SIZE);

    let read_records = h.client.batch_read(&max_ids);
    assert_eq!(read_records.len(), MAX_BATCH_PROBE_SIZE);

    // 4. Over MAX_BATCH_PROBE_SIZE (33) fails with BatchTooLarge
    let mut over_ids = Vec::new(&h.env);
    let mut over_notes = Vec::new(&h.env);
    for i in 0..=MAX_BATCH_PROBE_SIZE {
        over_ids.push_back(i);
        over_notes.push_back(symbol_short!("probe"));
    }
    assert_eq!(
        h.client
            .try_batch_plant(&over_ids, &over_notes)
            .unwrap_err()
            .unwrap(),
        ProbeError::BatchTooLarge
    );
    assert_eq!(
        h.client.try_batch_read(&over_ids).unwrap_err().unwrap(),
        ProbeError::BatchTooLarge
    );
}

// ===========================================================================
// ERROR OBSERVABILITY & GROUP TAXONOMY (ISSUE #380)
// ===========================================================================

#[test]
fn error_messages_and_group_taxonomies() {
    let h = TestHarness::new();

    let all_errors = [
        ProbeError::NotPlanted,
        ProbeError::EmptyBatch,
        ProbeError::BatchTooLarge,
        ProbeError::DuplicateEntry,
        ProbeError::ArithmeticOverflow,
        ProbeError::CorruptedRecord,
        ProbeError::InvariantViolation,
        ProbeError::ReentrancyGuard,
        ProbeError::EntryArchived,
        ProbeError::LifetimeExtensionFailed,
    ];

    for err in all_errors {
        assert!(!err.message().is_empty());
        let disc = err as u32;
        let queried_msg = h.client.error_message(&disc);
        assert!(!queried_msg.is_empty());

        let queried_grp = h.client.error_group(&disc);
        assert_eq!(queried_grp, err.group() as u32);
    }

    assert_eq!(ProbeError::ReentrancyGuard.group(), 'E');
    assert_eq!(ProbeError::InvariantViolation.group(), 'D');
    assert_eq!(ProbeError::EntryArchived.group(), 'A');
    assert_eq!(ProbeError::LifetimeExtensionFailed.group(), 'A');
}

// ===========================================================================
// PROPERTY-BASED FUZZ TESTING
// ===========================================================================

proptest! {
    #[test]
    fn fuzz_arbitrary_discriminants_never_panics(disc in 0u32..=10_000u32) {
        let parsed = ProbeError::from_discriminant(disc);
        if let Some(err) = parsed {
            prop_assert_eq!(err as u32, disc);
            prop_assert!(!err.message().is_empty());
            prop_assert!(err.group() >= 'A' && err.group() <= 'E');
        } else {
            prop_assert!(disc == 0 || disc > 10);
        }
    }

    #[test]
    fn fuzz_state_sync_assertions(
        init in 0u32..=1_000_000u32,
        delta in 0u32..=MAX_BATCH_PROBE_SIZE,
        skew in -5i32..=5i32,
    ) {
        let expected = init + delta;
        let actual_candidate = (expected as i64) + (skew as i64);

        if actual_candidate >= 0 && actual_candidate <= (u32::MAX as i64) {
            let actual = actual_candidate as u32;
            let result = invariants::assert_state_sync(init, actual, delta);
            if skew == 0 {
                prop_assert!(result.is_ok());
            } else {
                prop_assert_eq!(result.unwrap_err(), ProbeError::InvariantViolation);
            }
        }
    }
}
