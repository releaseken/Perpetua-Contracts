//! Fault-tolerance scenarios for the pause-clock: retries after a lost
//! response (a network partition between the stream contract and the ledger),
//! callers that are not the stream contract, and batches retried after a
//! timeout. Each scenario checks that state is not double-counted.

use soroban_sdk::Vec;

use super::common::Harness;

#[test]
fn retried_pause_after_lost_response_keeps_a_single_cycle() {
    let h = Harness::new();
    h.client.record_pause(&h.stream_contract);

    // The first response was lost, so the stream contract retries the same call.
    let retry = h.client.try_record_pause(&h.stream_contract);
    assert!(retry.is_err());

    let config = h.client.get_config().unwrap();
    assert!(config.paused);
    assert_eq!(
        config.cycle_count, 1,
        "a rejected retry must not start a new cycle"
    );
}

#[test]
fn retried_resume_after_lost_response_does_not_double_count() {
    let h = Harness::new();
    h.client.record_pause(&h.stream_contract);
    h.advance(300);
    h.client.record_resume(&h.stream_contract);

    // The resume response was lost, so the stream contract retries it.
    let retry = h.client.try_record_resume(&h.stream_contract);
    assert!(retry.is_err());

    let config = h.client.get_config().unwrap();
    assert!(!config.paused);
    assert_eq!(
        config.paused_total, 300,
        "the paused interval must be counted once"
    );
}

#[test]
fn batch_settle_from_a_non_stream_caller_writes_nothing() {
    let h = Harness::new();
    let ids = Vec::from_slice(&h.env, &[7_u64]);
    let totals = Vec::from_slice(&h.env, &[70_u64]);

    let result = h.client.try_batch_settle(&h.other, &ids, &totals);
    assert!(result.is_err());
    assert_eq!(h.client.get_stream_total(&7), 0);
}

#[test]
fn batch_settle_retried_after_timeout_leaves_the_same_totals() {
    let h = Harness::new();
    let ids = Vec::from_slice(&h.env, &[1_u64, 2]);
    let totals = Vec::from_slice(&h.env, &[10_u64, 20]);

    h.client.batch_settle(&h.stream_contract, &ids, &totals);
    // The first attempt timed out at the client, so the same batch is sent again.
    h.client.batch_settle(&h.stream_contract, &ids, &totals);

    assert_eq!(h.client.get_stream_total(&1), 10);
    assert_eq!(h.client.get_stream_total(&2), 20);
}

#[test]
fn batch_settle_with_a_duplicate_id_writes_nothing() {
    let h = Harness::new();
    let ids = Vec::from_slice(&h.env, &[1_u64, 2, 1]);
    let totals = Vec::from_slice(&h.env, &[10_u64, 20, 30]);

    let result = h.client.try_batch_settle(&h.stream_contract, &ids, &totals);
    assert_eq!(result, Err(Ok(crate::Error::DuplicateStreamId)));
    assert_eq!(h.client.get_stream_total(&1), 0);
    assert_eq!(h.client.get_stream_total(&2), 0);
}
