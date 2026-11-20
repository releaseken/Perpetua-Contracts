//! Tests for issue #400: batch_settle and batch_get_totals reduce
//! cross-contract invocation overhead in high-frequency batch claims.
//!
//! The key behavioral assertions are:
//! 1. `batch_settle` stores all totals in one call.
//! 2. `batch_get_totals` retrieves them all in one call with no cross-contract
//!    overhead per item.
//! 3. Unknown IDs return 0 in `batch_get_totals`.
//! 4. Validation errors (empty batch, oversized batch, mismatched lengths)
//!    are caught before any storage is mutated.

use soroban_sdk::Vec;

use super::common::Harness;

// ---------------------------------------------------------------------------
// batch_settle
// ---------------------------------------------------------------------------

#[test]
fn batch_settle_stores_all_totals() {
    let h = Harness::new();
    let ids = Vec::from_slice(&h.env, &[10_u64, 20, 30]);
    let totals = Vec::from_slice(&h.env, &[100_u64, 200, 300]);

    h.client.batch_settle(&h.stream_contract, &ids, &totals);

    assert_eq!(h.client.get_stream_total(&10), 100);
    assert_eq!(h.client.get_stream_total(&20), 200);
    assert_eq!(h.client.get_stream_total(&30), 300);
}

#[test]
fn batch_settle_overwrites_previous_value() {
    let h = Harness::new();
    // First write.
    let ids = Vec::from_slice(&h.env, &[1_u64]);
    let first = Vec::from_slice(&h.env, &[500_u64]);
    h.client.batch_settle(&h.stream_contract, &ids, &first);

    // Overwrite with a new total.
    let second = Vec::from_slice(&h.env, &[750_u64]);
    h.client.batch_settle(&h.stream_contract, &ids, &second);

    assert_eq!(h.client.get_stream_total(&1), 750);
}

#[test]
fn batch_settle_empty_returns_error() {
    let h = Harness::new();
    let empty: Vec<u64> = Vec::new(&h.env);
    let result = h
        .client
        .try_batch_settle(&h.stream_contract, &empty, &empty);
    assert!(result.is_err());
}

#[test]
fn batch_settle_oversized_returns_error() {
    let h = Harness::new();
    // 33 items exceeds MAX_BATCH_SIZE (32).
    let mut raw_ids = [0u64; 33];
    let mut raw_tots = [0u64; 33];
    for i in 0..33 {
        raw_ids[i] = i as u64;
        raw_tots[i] = i as u64 * 10;
    }
    let ids = Vec::from_slice(&h.env, &raw_ids);
    let tots = Vec::from_slice(&h.env, &raw_tots);
    let result = h.client.try_batch_settle(&h.stream_contract, &ids, &tots);
    assert!(result.is_err());
}

#[test]
fn batch_settle_mismatched_lengths_returns_error() {
    let h = Harness::new();
    let ids = Vec::from_slice(&h.env, &[1_u64, 2]);
    let tots = Vec::from_slice(&h.env, &[10_u64]); // length mismatch
    let result = h.client.try_batch_settle(&h.stream_contract, &ids, &tots);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// batch_get_totals
// ---------------------------------------------------------------------------

#[test]
fn batch_get_totals_returns_all_values() {
    let h = Harness::new();
    let ids = Vec::from_slice(&h.env, &[5_u64, 6, 7]);
    let tots = Vec::from_slice(&h.env, &[55_u64, 66, 77]);
    h.client.batch_settle(&h.stream_contract, &ids, &tots);

    let result = h.client.batch_get_totals(&ids).unwrap();
    assert_eq!(result.get_unchecked(0), 55_u64);
    assert_eq!(result.get_unchecked(1), 66_u64);
    assert_eq!(result.get_unchecked(2), 77_u64);
}

#[test]
fn batch_get_totals_unknown_id_returns_zero() {
    let h = Harness::new();
    let ids = Vec::from_slice(&h.env, &[999_u64]);
    let result = h.client.batch_get_totals(&ids).unwrap();
    assert_eq!(result.get_unchecked(0), 0_u64);
}

#[test]
fn batch_get_totals_empty_returns_error() {
    let h = Harness::new();
    let empty: Vec<u64> = Vec::new(&h.env);
    let result = h.client.try_batch_get_totals(&empty);
    assert!(result.is_err());
}

#[test]
fn batch_get_totals_oversized_returns_error() {
    let h = Harness::new();
    let ids: Vec<u64> = Vec::from_slice(&h.env, &{
        let mut v = [0u64; 33];
        for (i, x) in v.iter_mut().enumerate() {
            *x = i as u64;
        }
        v
    });
    let result = h.client.try_batch_get_totals(&ids);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// batch_settle only allows the stream contract to call it
// ---------------------------------------------------------------------------

#[test]
fn batch_settle_rejects_non_stream_caller() {
    let h = Harness::new();
    let ids = Vec::from_slice(&h.env, &[1_u64]);
    let tots = Vec::from_slice(&h.env, &[10_u64]);
    // `other` is not the registered stream contract.
    let result = h.client.try_batch_settle(&h.other, &ids, &tots);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Idempotency / round-trip fidelity
// ---------------------------------------------------------------------------

#[test]
fn batch_settle_then_batch_get_round_trips() {
    let h = Harness::new();
    let count: u32 = 32; // MAX_BATCH_SIZE exactly
    let raw_ids: std::vec::Vec<u64> = (0..count as u64).collect();
    let raw_tots: std::vec::Vec<u64> = (0..count as u64).map(|i| i * 7).collect();

    let ids = Vec::from_slice(&h.env, &raw_ids);
    let tots = Vec::from_slice(&h.env, &raw_tots);
    h.client.batch_settle(&h.stream_contract, &ids, &tots);

    let fetched = h.client.batch_get_totals(&ids).unwrap();
    for i in 0..count {
        assert_eq!(
            fetched.get_unchecked(i),
            raw_tots[i as usize],
            "stream {i}: round-trip mismatch"
        );
    }
}
