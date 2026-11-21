//! Tests for modular error types and custom error messages (issue #461).
//!
//! Verifies that:
//! 1. Each error variant has a unique, non-empty `message()`.
//! 2. Each error variant has the correct `group()` letter.
//! 3. The `error_message` contract view returns the right string for every
//!    discriminant, and an empty string for unknown discriminants.
//! 4. Each validation failure path in `emit` returns the specific error
//!    variant documented for that failure (not a generic catch-all).

use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Bytes, Symbol};

use super::common::*;
use crate::{SinkEntry, SinkError};

// ---------------------------------------------------------------------------
// §1  Error message table: every variant has a unique, non-empty message
// ---------------------------------------------------------------------------

#[test]
fn all_error_messages_are_non_empty() {
    let variants = [
        SinkError::NotInitialized,
        SinkError::AlreadyInitialized,
        SinkError::Unauthorized,
        SinkError::CallerNotRegisteredEmitter,
        SinkError::NoPendingAdmin,
        SinkError::InvalidNewAdmin,
        SinkError::EmptyTopic,
        SinkError::InvalidSourceContract,
        SinkError::InvalidSequence,
        SinkError::PayloadTooLarge,
        SinkError::EntryCountOverflow,
        SinkError::EmptyBatch,
        SinkError::BatchTooLarge,
    ];
    for v in variants {
        assert!(
            !v.message().is_empty(),
            "SinkError {:?} has an empty message",
            v
        );
    }
}

#[test]
fn all_error_messages_are_unique() {
    let variants = [
        SinkError::NotInitialized,
        SinkError::AlreadyInitialized,
        SinkError::Unauthorized,
        SinkError::CallerNotRegisteredEmitter,
        SinkError::NoPendingAdmin,
        SinkError::InvalidNewAdmin,
        SinkError::EmptyTopic,
        SinkError::InvalidSourceContract,
        SinkError::InvalidSequence,
        SinkError::PayloadTooLarge,
        SinkError::EntryCountOverflow,
        SinkError::EmptyBatch,
        SinkError::BatchTooLarge,
    ];
    let mut seen = std::collections::HashSet::new();
    for v in variants {
        assert!(
            seen.insert(v.message()),
            "duplicate message for SinkError {:?}: \"{}\"",
            v,
            v.message()
        );
    }
}

// ---------------------------------------------------------------------------
// §2  Group letter correctness
// ---------------------------------------------------------------------------

#[test]
fn error_groups_are_correct() {
    // Group A — Initialization
    assert_eq!(SinkError::NotInitialized.group(), 'A');
    assert_eq!(SinkError::AlreadyInitialized.group(), 'A');

    // Group B — Authorization
    assert_eq!(SinkError::Unauthorized.group(), 'B');
    assert_eq!(SinkError::CallerNotRegisteredEmitter.group(), 'B');
    assert_eq!(SinkError::NoPendingAdmin.group(), 'B');
    assert_eq!(SinkError::InvalidNewAdmin.group(), 'B');

    // Group C — Payload validation
    assert_eq!(SinkError::EmptyTopic.group(), 'C');
    assert_eq!(SinkError::InvalidSourceContract.group(), 'C');
    assert_eq!(SinkError::InvalidSequence.group(), 'C');
    assert_eq!(SinkError::PayloadTooLarge.group(), 'C');

    // Group D — Arithmetic / limits
    assert_eq!(SinkError::EntryCountOverflow.group(), 'D');
    assert_eq!(SinkError::EmptyBatch.group(), 'D');
    assert_eq!(SinkError::BatchTooLarge.group(), 'D');
}

// ---------------------------------------------------------------------------
// §3  `error_message` contract view
// ---------------------------------------------------------------------------

/// Helper: convert a `soroban_sdk::String` to a Rust `std::string::String`.
fn sdk_string_to_rust(s: soroban_sdk::String) -> std::string::String {
    // soroban_sdk::String stores UTF-8 bytes. We copy them out via `copy_into_slice`.
    let len = s.len() as usize;
    let mut buf = std::vec![0u8; len];
    s.copy_into_slice(&mut buf);
    std::string::String::from_utf8(buf).expect("soroban String must be valid UTF-8")
}

#[test]
fn error_message_view_returns_correct_string_for_all_discriminants() {
    let h = Harness::new();
    let pairs: &[(u32, &str)] = &[
        (1, SinkError::NotInitialized.message()),
        (2, SinkError::AlreadyInitialized.message()),
        (3, SinkError::Unauthorized.message()),
        (4, SinkError::CallerNotRegisteredEmitter.message()),
        (5, SinkError::NoPendingAdmin.message()),
        (6, SinkError::InvalidNewAdmin.message()),
        (7, SinkError::EmptyTopic.message()),
        (8, SinkError::InvalidSourceContract.message()),
        (9, SinkError::InvalidSequence.message()),
        (10, SinkError::PayloadTooLarge.message()),
        (11, SinkError::EntryCountOverflow.message()),
        (12, SinkError::EmptyBatch.message()),
        (13, SinkError::BatchTooLarge.message()),
    ];
    for &(disc, expected) in pairs {
        let got = h.client.error_message(&disc);
        let got_str = sdk_string_to_rust(got);
        assert_eq!(got_str.as_str(), expected, "error_message({disc}) mismatch");
    }
}

#[test]
fn error_message_view_returns_empty_for_unknown_discriminant() {
    let h = Harness::new();
    let got = h.client.error_message(&999u32);
    assert_eq!(sdk_string_to_rust(got).as_str(), "");
}

// ---------------------------------------------------------------------------
// §4  Each validation failure returns the specific error variant
// ---------------------------------------------------------------------------

#[test]
fn emit_rejects_unregistered_emitter_with_correct_error() {
    let h = Harness::new();
    let entry = h.entry(1);
    let err = h.client.try_emit(&h.other, &entry).unwrap_err().unwrap();
    assert_eq!(
        err,
        SinkError::CallerNotRegisteredEmitter,
        "expected CallerNotRegisteredEmitter, got {:?}",
        err
    );
    assert_eq!(err.message(), "caller is not a registered emitter");
}

#[test]
fn emit_rejects_zero_sequence_with_correct_error() {
    let h = Harness::new();
    let entry = SinkEntry {
        source_contract: h.emitter.clone(),
        topic: Symbol::new(&h.env, "test_event"),
        sequence: 0,
        payload: Bytes::new(&h.env),
    };
    let err = h.client.try_emit(&h.emitter, &entry).unwrap_err().unwrap();
    assert_eq!(err, SinkError::InvalidSequence);
    assert_eq!(err.message(), "entry sequence must be greater than zero");
    assert_eq!(err.group(), 'C');
}

#[test]
fn emit_rejects_oversized_payload_with_correct_error() {
    let h = Harness::new();
    let big = std::vec![0u8; (crate::MAX_PAYLOAD_BYTES + 1) as usize];
    let entry = h.entry_with_payload(1, &big);
    let err = h.client.try_emit(&h.emitter, &entry).unwrap_err().unwrap();
    assert_eq!(err, SinkError::PayloadTooLarge);
    assert_eq!(err.message(), "payload exceeds MAX_PAYLOAD_BYTES");
    assert_eq!(err.group(), 'C');
}

#[test]
fn emit_rejects_zero_address_source_contract_with_correct_error() {
    let h = Harness::new();
    let zero = Address::from_string(&soroban_sdk::String::from_str(
        &h.env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    ));
    let entry = SinkEntry {
        source_contract: zero,
        topic: Symbol::new(&h.env, "test_event"),
        sequence: 1,
        payload: Bytes::new(&h.env),
    };
    let err = h.client.try_emit(&h.emitter, &entry).unwrap_err().unwrap();
    assert_eq!(err, SinkError::InvalidSourceContract);
    assert_eq!(
        err.message(),
        "source_contract must be a valid non-zero address"
    );
    assert_eq!(err.group(), 'C');
}

#[test]
fn emit_when_sink_paused_returns_unauthorized_not_other_error() {
    let h = Harness::new();
    h.client.set_accepting(&false);
    let entry = h.entry(1);
    let err = h.client.try_emit(&h.emitter, &entry).unwrap_err().unwrap();
    assert_eq!(err, SinkError::Unauthorized);
    assert_eq!(err.message(), "caller is not the admin");
    assert_eq!(err.group(), 'B');
}

#[test]
fn batch_emit_rejects_empty_batch_with_correct_error() {
    let h = Harness::new();
    let empty: soroban_sdk::Vec<SinkEntry> = soroban_sdk::Vec::new(&h.env);
    let err = h
        .client
        .try_batch_emit(&h.emitter, &empty)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, SinkError::EmptyBatch);
    assert_eq!(err.message(), "batch must contain at least one entry");
    assert_eq!(err.group(), 'D');
}

#[test]
fn batch_emit_rejects_oversized_batch_with_correct_error() {
    let h = Harness::new();
    let mut entries: soroban_sdk::Vec<SinkEntry> = soroban_sdk::Vec::new(&h.env);
    for i in 0..(crate::MAX_BATCH_SIZE + 1) {
        entries.push_back(h.entry(i as u64 + 1));
    }
    let err = h
        .client
        .try_batch_emit(&h.emitter, &entries)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, SinkError::BatchTooLarge);
    assert_eq!(
        err.message(),
        "batch exceeds MAX_BATCH_SIZE; split into smaller batches"
    );
    assert_eq!(err.group(), 'D');
}

#[test]
fn double_init_returns_already_initialized() {
    let h = Harness::new();
    let err = h.client.try_init(&h.admin).unwrap_err().unwrap();
    assert_eq!(err, SinkError::AlreadyInitialized);
    assert_eq!(err.message(), "contract is already initialized");
    assert_eq!(err.group(), 'A');
}

#[test]
fn emit_rejects_empty_topic_with_correct_error() {
    let h = Harness::new();
    let entry = SinkEntry {
        source_contract: h.emitter.clone(),
        topic: Symbol::new(&h.env, ""),
        sequence: 1,
        payload: Bytes::new(&h.env),
    };
    let err = h.client.try_emit(&h.emitter, &entry).unwrap_err().unwrap();
    assert_eq!(err, SinkError::EmptyTopic);
    assert_eq!(err.message(), "entry topic must not be empty");
    assert_eq!(err.group(), 'C');
}
