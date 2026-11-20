//! TTL and archival-resilience tests.

use super::common::{Harness, YEAR};
use crate::storage::MIN_SCHEDULE_TTL_LEDGERS;

#[test]
fn create_sets_ttl_at_least_to_floor() {
    let h = Harness::new();
    let id = h.create_simple(YEAR as i128, YEAR);
    let ttl = h.ttl_of(id);
    assert!(
        ttl >= MIN_SCHEDULE_TTL_LEDGERS,
        "TTL {ttl} must be >= floor {MIN_SCHEDULE_TTL_LEDGERS}"
    );
}

#[test]
fn instance_ttl_at_max_after_create() {
    let h = Harness::new();
    let max_ttl = h.max_achievable_ttl();
    h.create_simple(YEAR as i128, YEAR);
    let instance_ttl = h.env.as_contract(&h.contract_id, || {
        h.env.storage().instance().get_ttl()
    });
    assert_eq!(instance_ttl, max_ttl, "instance TTL must be max after create");
}

#[test]
fn keeper_extend_refreshes_ttl() {
    let h = Harness::new();
    let id = h.create_simple(YEAR as i128, YEAR);
    h.advance(100 * 86_400); // 100 days

    h.client.extend_schedule_ttl(&id).unwrap();
    let ttl = h.ttl_of(id);
    assert!(
        ttl >= MIN_SCHEDULE_TTL_LEDGERS,
        "TTL must be >= floor after keeper extension"
    );
}

#[test]
fn keeper_extend_unknown_id_returns_error() {
    let h = Harness::new();
    let result = h.client.try_extend_schedule_ttl(&9999_u64);
    assert!(result.is_err());
}

#[test]
fn ttl_bumped_on_every_withdrawal() {
    let h = Harness::new();
    let id = h.create_simple(YEAR as i128, YEAR);

    for step in 0..4 {
        h.advance(YEAR / 8);
        h.client.withdraw(&id, &None).unwrap();
        let ttl = h.ttl_of(id);
        assert!(
            ttl >= MIN_SCHEDULE_TTL_LEDGERS,
            "step {step}: TTL {ttl} must be >= floor after withdrawal"
        );
    }
}
