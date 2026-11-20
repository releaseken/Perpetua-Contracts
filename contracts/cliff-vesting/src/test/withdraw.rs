//! Withdrawal tests: cliff gate, accrual math, pool conservation.

use super::common::{Harness, DAY, ONE, YEAR};

#[test]
fn nothing_withdrawable_before_cliff() {
    let h = Harness::new();
    let cliff = DAY * 90;
    let id = h.create_with_cliff(YEAR as i128, YEAR, cliff);

    // 10 days in — still before the cliff.
    h.advance(DAY * 10);
    let available = h.client.withdrawable_of(&id).unwrap();
    assert_eq!(available, 0, "cliff not reached: nothing withdrawable");
}

#[test]
fn full_accrual_since_start_withdrawable_at_cliff() {
    let h = Harness::new();
    let cliff = DAY * 90;
    let deposit = YEAR as i128;
    let id = h.create_with_cliff(deposit, YEAR, cliff);

    // Jump exactly to cliff.
    h.advance(cliff);
    let available = h.client.withdrawable_of(&id).unwrap();
    // Expected: deposit * elapsed / duration = deposit * 90days / 365days (rounded down)
    let expected = deposit * (cliff as i128) / (YEAR as i128);
    assert_eq!(available, expected, "cliff release amount must match formula");
}

#[test]
fn withdraw_full_at_maturity() {
    let h = Harness::new();
    let deposit = 100 * ONE;
    let id = h.create_simple(deposit, YEAR);

    h.advance(YEAR + 1);
    h.client.withdraw(&id, &None).unwrap();

    let s = h.get(id);
    assert_eq!(s.withdrawn, deposit);
    assert_eq!(s.status, crate::ScheduleStatus::Depleted);
    assert_eq!(h.pool(), 0, "pool must be empty after full withdrawal");
}

#[test]
fn partial_withdraw_respects_available() {
    let h = Harness::new();
    let deposit = 100 * ONE;
    let id = h.create_simple(deposit, YEAR);

    h.advance(YEAR / 2);
    let available = h.client.withdrawable_of(&id).unwrap();
    // Withdraw half of what is available.
    let take = available / 2;
    h.client.withdraw(&id, &Some(take)).unwrap();

    let s = h.get(id);
    assert_eq!(s.withdrawn, take);
    h.assert_pool_invariant();
}

#[test]
fn withdraw_more_than_available_returns_error() {
    let h = Harness::new();
    let id = h.create_simple(YEAR as i128, YEAR);
    h.advance(YEAR / 4);
    let available = h.client.withdrawable_of(&id).unwrap();
    let result = h.client.try_withdraw(&id, &Some(available + 1));
    assert!(result.is_err());
}

#[test]
fn withdraw_zero_returns_error() {
    let h = Harness::new();
    let id = h.create_simple(YEAR as i128, YEAR);
    h.advance(YEAR / 2);
    let result = h.client.try_withdraw(&id, &Some(0_i128));
    assert!(result.is_err());
}

#[test]
fn nothing_to_withdraw_before_start() {
    let h = Harness::new();
    // Schedule starts 1 day in the future.
    let start = h.now() + DAY;
    let result = h.client.try_create_schedule(
        &h.creator,
        &h.grantor,
        &h.beneficiary,
        &h.token,
        &(YEAR as i128),
        &start,
        &(start + YEAR),
        &start,
    );
    // This should succeed (future-dated schedule is valid).
    assert!(result.is_ok());
    let id = result.unwrap().unwrap();

    // Trying to withdraw before start returns NothingToWithdraw.
    let w = h.client.try_withdraw(&id, &None);
    assert!(w.is_err());
}

#[test]
fn pool_invariant_holds_after_multiple_withdrawals() {
    let h = Harness::new();
    let ids: std::vec::Vec<u64> = (0..4)
        .map(|_| h.create_simple(YEAR as i128, YEAR))
        .collect();

    for &id in &ids {
        h.advance(DAY * 30);
        h.client.withdraw(&id, &None).unwrap();
        h.assert_pool_invariant();
    }
}
