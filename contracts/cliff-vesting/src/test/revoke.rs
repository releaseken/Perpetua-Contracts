//! Revoke tests: role enforcement, schedule collapse, refund accounting.

use super::common::{Harness, DAY, ONE, YEAR};

#[test]
fn revoke_before_cliff_refunds_full_deposit() {
    let h = Harness::new();
    let deposit = 100 * ONE;
    let cliff = DAY * 90;
    let id = h.create_with_cliff(deposit, YEAR, cliff);

    let grantor_before = h.balance(&h.grantor);
    // Revoke before the cliff: beneficiary has zero vested.
    h.client.revoke_schedule(&h.revoker, &id).unwrap();

    let refunded = h.balance(&h.grantor) - grantor_before;
    assert_eq!(refunded, deposit, "full deposit must be refunded before cliff");

    let s = h.get(id);
    assert_eq!(s.status, crate::ScheduleStatus::Revoked);
    assert_eq!(s.deposited, 0, "deposited must be rewritten to vested (0)");
}

#[test]
fn revoke_after_cliff_keeps_vested_in_pool() {
    let h = Harness::new();
    let deposit = YEAR as i128;
    let cliff = DAY * 30;
    let id = h.create_with_cliff(deposit, YEAR, cliff);

    // 60 days in — past cliff.
    h.advance(DAY * 60);
    let vested_before_revoke = h.client.vested_of(&id).unwrap();

    let grantor_before = h.balance(&h.grantor);
    h.client.revoke_schedule(&h.revoker, &id).unwrap();

    // Refund = deposited - vested_before_revoke.
    let refunded = h.balance(&h.grantor) - grantor_before;
    assert_eq!(refunded, deposit - vested_before_revoke);
    // The vested amount must remain pooled (beneficiary can still claim it).
    assert_eq!(h.pool(), vested_before_revoke);

    h.assert_pool_invariant();
}

#[test]
fn revoke_requires_revoker_role() {
    let h = Harness::new();
    let id = h.create_simple(YEAR as i128, YEAR);
    let result = h.client.try_revoke_schedule(&h.other, &id);
    assert!(result.is_err());
}

#[test]
fn revoke_twice_returns_terminated() {
    let h = Harness::new();
    let id = h.create_simple(YEAR as i128, YEAR);
    h.client.revoke_schedule(&h.revoker, &id).unwrap();
    let result = h.client.try_revoke_schedule(&h.revoker, &id);
    assert!(result.is_err());
}

#[test]
fn beneficiary_can_withdraw_vested_after_revoke() {
    let h = Harness::new();
    let deposit = YEAR as i128;
    let id = h.create_simple(deposit, YEAR);

    h.advance(YEAR / 2);
    let vested = h.client.vested_of(&id).unwrap();
    h.client.revoke_schedule(&h.revoker, &id).unwrap();

    // After revoke, beneficiary can still pull what was vested.
    h.client.withdraw(&id, &None).unwrap();
    assert_eq!(h.balance(&h.beneficiary), vested);
    h.assert_pool_invariant();
}

#[test]
fn revoke_conservation_identity() {
    let h = Harness::new();
    let deposit = 365 * ONE;
    let id = h.create_simple(deposit, YEAR);

    h.advance(DAY * 100);
    let vested = h.client.vested_of(&id).unwrap();
    let grantor_before = h.balance(&h.grantor);
    h.client.revoke_schedule(&h.revoker, &id).unwrap();
    let refunded = h.balance(&h.grantor) - grantor_before;

    // refunded + vested == deposit (conservation, exact).
    assert_eq!(
        refunded + vested,
        deposit,
        "conservation identity broken: refunded {refunded} + vested {vested} != deposit {deposit}"
    );
}
