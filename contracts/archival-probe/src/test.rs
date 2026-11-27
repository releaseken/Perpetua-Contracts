//! Shared test harness and Byzantine fault tolerance test scenarios for archival probe.

use super::*;
use soroban_sdk::symbol_short;
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

#[test]
fn plant_and_read_round_trip() {
    let h = TestHarness::new();

    assert!(!h.client.planted());
    assert_eq!(h.client.try_read().unwrap_err().unwrap(), Error::NotPlanted);

    h.client.plant(&symbol_short!("canary"));
    assert!(h.client.planted());
    assert_eq!(h.client.read(), symbol_short!("canary"));
}

#[test]
fn retry_does_not_replace_note_or_refresh_ttl() {
    let h = TestHarness::new();

    h.client.plant(&symbol_short!("first"));
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
}

/// The probe must never extend its own TTL — that omission is what makes it
/// archive on the network's schedule instead of the contract's.
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
// BYZANTINE FAULT TOLERANCE & NETWORK PARTITION TEST SUITE (ISSUE #378)
// ===========================================================================

/// Scenario 1: Network Partition Split-Brain Concurrent Plant Attempts
/// Under network partition, two replicas or partitioned callers attempt to plant
/// divergent canary notes. The contract must guarantee absolute first-write-wins
/// deterministic consensus and never overwrite the original canary note.
#[test]
fn bft_network_partition_split_brain_plant_concurrency() {
    let h = TestHarness::new();

    let partition_a_note = symbol_short!("part_a");
    let partition_b_note = symbol_short!("part_b");
    let partition_c_note = symbol_short!("part_c");

    // Partition A successfully lands transaction on ledger
    h.client.plant(&partition_a_note);
    assert_eq!(h.client.read(), partition_a_note);

    // Network partitions heal; Partition B and Partition C broadcast delayed proposals
    h.advance_ledgers(50);
    h.client.plant(&partition_b_note);
    assert_eq!(
        h.client.read(),
        partition_a_note,
        "Partition B must not overwrite"
    );

    h.advance_ledgers(100);
    h.client.plant(&partition_c_note);
    assert_eq!(
        h.client.read(),
        partition_a_note,
        "Partition C must not overwrite"
    );
}

/// Scenario 2: Byzantine Replay Storm with Simulated Ledger Skew
/// An adversary captures and replays plant transactions across large ledger sequence gaps.
/// Verifies that TTL expiration countdown is preserved monotonically without being
/// refreshed or reset by replayed calls.
#[test]
fn bft_replay_storm_with_ledger_skew_preserves_ttl_decay() {
    let h = TestHarness::new();
    let original_note = symbol_short!("orig");

    h.client.plant(&original_note);
    let initial_ttl = h.env.as_contract(&h.contract_id, || {
        h.env.storage().persistent().get_ttl(&Key::Canary)
    });

    // Fast-forward ledger sequence (simulating network partition delay)
    h.advance_ledgers(250);

    let decayed_ttl = h.env.as_contract(&h.contract_id, || {
        h.env.storage().persistent().get_ttl(&Key::Canary)
    });
    assert!(decayed_ttl < initial_ttl, "TTL must decrease over ledgers");

    // Adversary executes repeated replay attacks
    for _ in 0..10 {
        h.client.plant(&symbol_short!("replay"));
    }

    let post_attack_ttl = h.env.as_contract(&h.contract_id, || {
        h.env.storage().persistent().get_ttl(&Key::Canary)
    });

    // Byzantine replays must strictly NOT refresh or bump the decayed TTL
    assert_eq!(
        post_attack_ttl, decayed_ttl,
        "Replayed transactions must not bump TTL or reset archival window"
    );
    assert_eq!(h.client.read(), original_note);
}

/// Scenario 3: Batch Plant Under Partition With Partial Byzantine Corruptions
/// A partitioned node buffers probe registrations and submits a batch. If duplicate
/// keys or re-submissions exist in the batch, existing keys remain untouched
/// and idempotence invariant I3 is upheld.
#[test]
fn bft_partitioned_batch_plant_idempotency_and_integrity() {
    let h = TestHarness::new();

    let mut ids1 = Vec::new(&h.env);
    let mut notes1 = Vec::new(&h.env);
    ids1.push_back(1);
    notes1.push_back(symbol_short!("node1"));
    ids1.push_back(2);
    notes1.push_back(symbol_short!("node2"));

    // First batch lands
    let planted = h.client.batch_plant(&ids1, &notes1);
    assert_eq!(planted, 2);

    // Network partition delays second batch containing a duplicate of node1 and a new node3
    h.advance_ledgers(15);
    let mut ids2 = Vec::new(&h.env);
    let mut notes2 = Vec::new(&h.env);
    ids2.push_back(1);
    notes2.push_back(symbol_short!("corrupt")); // conflicting note for probe 1
    ids2.push_back(3);
    notes2.push_back(symbol_short!("node3"));

    let planted2 = h.client.batch_plant(&ids2, &notes2);
    // Only node3 should be planted; node1 was already planted
    assert_eq!(planted2, 1);

    // Verify probe 1 preserved original note (Invariant I3)
    let p1 = h.client.read_probe(&1);
    assert_eq!(p1.note, symbol_short!("node1"));

    let p3 = h.client.read_probe(&3);
    assert_eq!(p3.note, symbol_short!("node3"));
}

/// Scenario 4: Boundary Defenses Against Malformed/Mismatched Byzantine Batches
/// Adversary submits asymmetric batch lengths or empty vectors to induce host panic.
#[test]
fn bft_byzantine_malformed_batch_defenses() {
    let h = TestHarness::new();

    // 1. Empty batch rejection
    let empty_ids = Vec::new(&h.env);
    let empty_notes = Vec::new(&h.env);
    assert_eq!(
        h.client
            .try_batch_plant(&empty_ids, &empty_notes)
            .unwrap_err()
            .unwrap(),
        Error::EmptyBatch
    );

    // 2. Length mismatch rejection (Byzantine desynchronized payloads)
    let mut ids = Vec::new(&h.env);
    ids.push_back(10);
    ids.push_back(11);
    let mut notes = Vec::new(&h.env);
    notes.push_back(symbol_short!("ten"));

    assert_eq!(
        h.client.try_batch_plant(&ids, &notes).unwrap_err().unwrap(),
        Error::BatchTooLarge
    );

    // 3. Batch exceeding MAX_BATCH_PROBE_SIZE (denial of service attempt)
    let mut big_ids = Vec::new(&h.env);
    let mut big_notes = Vec::new(&h.env);
    for i in 0..33 {
        big_ids.push_back(i);
        big_notes.push_back(symbol_short!("probe"));
    }
    assert_eq!(
        h.client
            .try_batch_plant(&big_ids, &big_notes)
            .unwrap_err()
            .unwrap(),
        Error::BatchTooLarge
    );
}

/// Scenario 5: Formal Invariant and Read Deserialization Verification
/// Tests that invariant module checks assert_record_integrity and verify_batch_bounds
/// uphold formal verification standards under partition conditions.
#[test]
fn bft_formal_invariants_and_deserialization_efficiency() {
    let h = TestHarness::new();

    assert!(invariants::verify_batch_bounds(1));
    assert!(invariants::verify_batch_bounds(MAX_BATCH_PROBE_SIZE));
    assert!(!invariants::verify_batch_bounds(0));
    assert!(!invariants::verify_batch_bounds(MAX_BATCH_PROBE_SIZE + 1));

    h.client.plant_probe(&42, &symbol_short!("val_42"));
    let rec = h.client.read_probe(&42);

    assert!(invariants::assert_record_integrity(
        &rec,
        &symbol_short!("val_42")
    ));
    assert!(!invariants::assert_record_integrity(
        &rec,
        &symbol_short!("wrong")
    ));
}
