//! Tests for schedule creation and validation.

use super::common::{Harness, DAY, ONE, YEAR};

#[test]
fn create_simple_schedule_stores_state() {
    let h = Harness::new();
    let id = h.create_simple(100 * ONE, YEAR);
    let s = h.get(id);

    assert_eq!(s.deposited, 100 * ONE);
    assert_eq!(s.withdrawn, 0);
    assert_eq!(s.cliff_time, s.start_time, "no-cliff: cliff == start");
    assert_eq!(s.status, crate::ScheduleStatus::Active);
    h.assert_pool_invariant();
}

#[test]
fn create_moves_deposit_to_pool() {
    let h = Harness::new();
    let initial = h.balance(&h.grantor);
    h.create_simple(50 * ONE, YEAR);
    assert_eq!(h.balance(&h.grantor), initial - 50 * ONE);
    assert_eq!(h.pool(), 50 * ONE);
}

#[test]
fn create_with_cliff() {
    let h = Harness::new();
    let cliff = DAY * 90; // 90-day cliff
    let id = h.create_with_cliff(365 * ONE, YEAR, cliff);
    let s = h.get(id);
    assert_eq!(s.cliff_time, s.start_time + cliff);
}

#[test]
fn create_rejects_self_vest() {
    let h = Harness::new();
    let start = h.now();
    let result = h.client.try_create_schedule(
        &h.creator,
        &h.grantor,
        &h.grantor, // beneficiary == grantor
        &h.token,
        &(YEAR as i128),
        &start,
        &(start + YEAR),
        &start,
    );
    assert!(result.is_err());
}

#[test]
fn create_rejects_zero_deposit() {
    let h = Harness::new();
    let start = h.now();
    let result = h.client.try_create_schedule(
        &h.creator,
        &h.grantor,
        &h.beneficiary,
        &h.token,
        &0_i128,
        &start,
        &(start + YEAR),
        &start,
    );
    assert!(result.is_err());
}

#[test]
fn create_rejects_inverted_range() {
    let h = Harness::new();
    let start = h.now();
    let result = h.client.try_create_schedule(
        &h.creator,
        &h.grantor,
        &h.beneficiary,
        &h.token,
        &(YEAR as i128),
        &(start + YEAR),
        &start, // end before start
        &start,
    );
    assert!(result.is_err());
}

#[test]
fn create_rejects_cliff_before_start() {
    let h = Harness::new();
    let start = h.now();
    let result = h.client.try_create_schedule(
        &h.creator,
        &h.grantor,
        &h.beneficiary,
        &h.token,
        &(YEAR as i128),
        &start,
        &(start + YEAR),
        &(start - 1), // cliff before start
    );
    assert!(result.is_err());
}

#[test]
fn create_rejects_cliff_after_end() {
    let h = Harness::new();
    let start = h.now();
    let result = h.client.try_create_schedule(
        &h.creator,
        &h.grantor,
        &h.beneficiary,
        &h.token,
        &(YEAR as i128),
        &start,
        &(start + YEAR),
        &(start + YEAR + 1), // cliff after end
    );
    assert!(result.is_err());
}

#[test]
fn create_rejects_deposit_below_duration() {
    let h = Harness::new();
    let start = h.now();
    // deposit = 1, duration = YEAR — clearly below the 1 stroop/second floor
    let result = h.client.try_create_schedule(
        &h.creator,
        &h.grantor,
        &h.beneficiary,
        &h.token,
        &1_i128,
        &start,
        &(start + YEAR),
        &start,
    );
    assert!(result.is_err());
}

#[test]
fn create_requires_creator_role() {
    let h = Harness::new();
    let start = h.now();
    // `other` does not hold the creator role.
    let result = h.client.try_create_schedule(
        &h.other,
        &h.grantor,
        &h.beneficiary,
        &h.token,
        &(YEAR as i128),
        &start,
        &(start + YEAR),
        &start,
    );
    assert!(result.is_err());
}

#[test]
fn schedule_ids_are_monotonic_contiguous() {
    let h = Harness::new();
    let id0 = h.create_simple(YEAR as i128, YEAR);
    let id1 = h.create_simple(YEAR as i128, YEAR);
    let id2 = h.create_simple(YEAR as i128, YEAR);
    assert_eq!(id0, 0);
    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
}
