//! Tests for issue #399: automated entry lifetime extension.
//!
//! Verifies that every mutating path that touches a persistent
//! `PausedTotal` entry extends its TTL, that the permissionless keeper
//! paths work, and that the instance entry is always pinned.

use soroban_sdk::Vec;

use crate::storage::MIN_ENTRY_TTL_LEDGERS;

use super::common::Harness;

// ---------------------------------------------------------------------------
// settle_stream_total extends TTL
// ---------------------------------------------------------------------------

#[test]
fn settle_extends_entry_ttl() {
    let h = Harness::new();
    h.client
        .settle_stream_total(&h.stream_contract, &42_u64, &1_000_u64);

    assert!(
        h.entry_exists(42),
        "entry must exist after settle_stream_total"
    );
    let ttl = h.ttl_of(42);
    assert!(
        ttl >= MIN_ENTRY_TTL_LEDGERS,
        "TTL {ttl} must be >= MIN_ENTRY_TTL_LEDGERS {MIN_ENTRY_TTL_LEDGERS}"
    );
}

// ---------------------------------------------------------------------------
// extend_stream_ttl (permissionless keeper)
// ---------------------------------------------------------------------------

#[test]
fn extend_stream_ttl_permissionless() {
    let h = Harness::new();
    // Write an entry first.
    h.client
        .settle_stream_total(&h.stream_contract, &7_u64, &500_u64);

    // Any caller can extend.
    h.client.extend_stream_ttl(&7_u64);
    let ttl = h.ttl_of(7);
    assert!(
        ttl >= MIN_ENTRY_TTL_LEDGERS,
        "keeper sweep must keep TTL >= floor"
    );
}

#[test]
fn extend_stream_ttl_unknown_id_returns_error() {
    let h = Harness::new();
    // Stream ID 999 has never been written.
    let result = h.client.try_extend_stream_ttl(&999_u64);
    assert!(
        result.is_err(),
        "extending an unknown entry must return an error"
    );
}

// ---------------------------------------------------------------------------
// batch_extend_ttl (permissionless keeper sweep)
// ---------------------------------------------------------------------------

#[test]
fn batch_extend_ttl_skips_unknown_ids() {
    let h = Harness::new();
    h.client
        .settle_stream_total(&h.stream_contract, &1_u64, &100_u64);
    h.client
        .settle_stream_total(&h.stream_contract, &2_u64, &200_u64);

    // ID 99 is unknown — should be silently skipped.
    let ids = soroban_sdk::Vec::from_slice(&h.env, &[1_u64, 2_u64, 99_u64]);
    let extended = h.client.batch_extend_ttl(&ids);
    assert_eq!(extended, 2, "only the two existing entries must be extended");
}

#[test]
fn batch_extend_ttl_empty_batch_errors() {
    let h = Harness::new();
    let ids: Vec<u64> = Vec::new(&h.env);
    let result = h.client.try_batch_extend_ttl(&ids);
    assert!(result.is_err());
}

#[test]
fn batch_extend_ttl_respects_max_batch_size() {
    let h = Harness::new();
    // 33 items > MAX_BATCH_SIZE (32).
    let ids: Vec<u64> = Vec::from_slice(&h.env, &{
        let mut v = [0u64; 33];
        for (i, x) in v.iter_mut().enumerate() {
            *x = i as u64;
        }
        v
    });
    let result = h.client.try_batch_extend_ttl(&ids);
    assert!(result.is_err(), "batch > MAX_BATCH_SIZE must be rejected");
}

// ---------------------------------------------------------------------------
// Instance entry stays alive across mutations
// ---------------------------------------------------------------------------

#[test]
fn instance_ttl_bumped_on_every_mutation() {
    let h = Harness::new();
    let max_ttl = h.max_achievable_ttl();

    // After init the instance TTL must already be at max.
    let instance_ttl_after_init = h.env.as_contract(&h.contract_id, || {
        h.env.storage().instance().get_ttl()
    });
    assert_eq!(
        instance_ttl_after_init, max_ttl,
        "instance TTL must be max after init"
    );

    // After settle the instance TTL must still be at max.
    h.client
        .settle_stream_total(&h.stream_contract, &5_u64, &0_u64);
    let instance_ttl_after_settle = h.env.as_contract(&h.contract_id, || {
        h.env.storage().instance().get_ttl()
    });
    assert_eq!(
        instance_ttl_after_settle, max_ttl,
        "instance TTL must remain max after settle_stream_total"
    );
}

// ---------------------------------------------------------------------------
// Persistent entry TTL floor is never violated
// ---------------------------------------------------------------------------

#[test]
fn persistent_entry_ttl_never_below_floor() {
    let h = Harness::new();
    // Write several entries.
    for id in 0_u64..8 {
        h.client
            .settle_stream_total(&h.stream_contract, &id, &(id * 1_000));
    }
    // Advance time significantly.
    h.advance(100 * 86_400); // 100 days

    // Keeper sweep — now all TTLs are refreshed.
    let ids = Vec::from_slice(&h.env, &[0_u64, 1, 2, 3, 4, 5, 6, 7]);
    h.client.batch_extend_ttl(&ids);

    for id in 0_u64..8 {
        let ttl = h.ttl_of(id);
        assert!(
            ttl >= MIN_ENTRY_TTL_LEDGERS,
            "stream {id}: TTL {ttl} < MIN_ENTRY_TTL_LEDGERS after keeper sweep"
        );
    }
}
