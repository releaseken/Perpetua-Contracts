//! Happy-path and edge-case tests for `emit` and `batch_emit`.

use soroban_sdk::{Bytes, Symbol, Vec};

use super::common::*;
use crate::SinkEntry;

#[test]
fn emit_returns_monotonically_increasing_entry_ids() {
    let h = Harness::new();
    let id0 = h.client.emit(&h.emitter, &h.entry(1));
    let id1 = h.client.emit(&h.emitter, &h.entry(2));
    let id2 = h.client.emit(&h.emitter, &h.entry(3));
    assert_eq!(id0, 0, "first entry_id must be 0");
    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
    assert_eq!(h.client.entry_count(), 3);
}

#[test]
fn emit_accepts_max_payload() {
    let h = Harness::new();
    let big = std::vec![0xABu8; crate::MAX_PAYLOAD_BYTES as usize];
    let entry = h.entry_with_payload(1, &big);
    let id = h.client.emit(&h.emitter, &entry);
    assert_eq!(id, 0);
}

#[test]
fn emit_accepts_single_byte_payload() {
    let h = Harness::new();
    let entry = h.entry_with_payload(1, &[42u8]);
    let id = h.client.emit(&h.emitter, &entry);
    assert_eq!(id, 0);
}

#[test]
fn emit_accepts_empty_payload() {
    let h = Harness::new();
    let entry = SinkEntry {
        source_contract: h.emitter.clone(),
        topic: Symbol::new(&h.env, "test_event"),
        sequence: 1,
        payload: Bytes::new(&h.env),
    };
    let id = h.client.emit(&h.emitter, &entry);
    assert_eq!(id, 0);
}

#[test]
fn batch_emit_returns_first_entry_id_and_advances_count() {
    let h = Harness::new();
    let mut entries = Vec::new(&h.env);
    entries.push_back(h.entry(1));
    entries.push_back(h.entry(2));
    entries.push_back(h.entry(3));

    let first_id = h.client.batch_emit(&h.emitter, &entries);
    assert_eq!(first_id, 0, "first entry_id in batch must be 0");
    assert_eq!(h.client.entry_count(), 3);
}

#[test]
fn batch_emit_ids_are_contiguous_with_previous_emits() {
    let h = Harness::new();
    // Single emit first.
    let id0 = h.client.emit(&h.emitter, &h.entry(1));
    assert_eq!(id0, 0);

    // Batch of 2 follows.
    let mut entries = Vec::new(&h.env);
    entries.push_back(h.entry(2));
    entries.push_back(h.entry(3));
    let first_batch_id = h.client.batch_emit(&h.emitter, &entries);
    assert_eq!(first_batch_id, 1);
    assert_eq!(h.client.entry_count(), 3);
}

#[test]
fn set_accepting_false_blocks_emit() {
    let h = Harness::new();
    h.client.set_accepting(&false);
    let err = h
        .client
        .try_emit(&h.emitter, &h.entry(1))
        .unwrap_err()
        .unwrap();
    assert_eq!(err, crate::SinkError::Unauthorized);
}

#[test]
fn re_enabling_accepting_allows_emit_again() {
    let h = Harness::new();
    h.client.set_accepting(&false);
    h.client.set_accepting(&true);
    let id = h.client.emit(&h.emitter, &h.entry(1));
    assert_eq!(id, 0);
}

#[test]
fn deregister_emitter_prevents_further_emits() {
    let h = Harness::new();
    // First emit succeeds.
    let id = h.client.emit(&h.emitter, &h.entry(1));
    assert_eq!(id, 0);

    h.client.deregister_emitter(&h.emitter);
    let err = h
        .client
        .try_emit(&h.emitter, &h.entry(2))
        .unwrap_err()
        .unwrap();
    assert_eq!(err, crate::SinkError::CallerNotRegisteredEmitter);
}

#[test]
fn multiple_emitters_interleave_ids_correctly() {
    let h = Harness::new();
    // Register a second emitter.
    use soroban_sdk::testutils::Address as _;
    let emitter2 = soroban_sdk::Address::generate(&h.env);
    h.client.register_emitter(&emitter2);

    let id0 = h.client.emit(&h.emitter, &h.entry(1));
    let id1 = h.client.emit(&emitter2, &h.entry(1));
    let id2 = h.client.emit(&h.emitter, &h.entry(2));
    assert_eq!(id0, 0);
    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
    assert_eq!(h.client.entry_count(), 3);
}
