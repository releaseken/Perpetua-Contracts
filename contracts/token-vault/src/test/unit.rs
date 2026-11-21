//! Basic unit tests for the token-vault contract.

use super::common::*;

#[test]
fn create_and_withdraw_simple() {
    let h = Harness::new();
    let duration = 1_000u64;
    let deposit = duration as i128; // 1 stroop/s
    let id = h.create_simple(deposit, duration);

    // Before any time passes: cliff == start, so withdrawable == 0.
    assert_eq!(h.client.withdrawable_of(&id), 0);

    // Advance halfway.
    h.advance(duration / 2);
    let w = h.client.withdrawable_of(&id);
    assert_eq!(w, deposit / 2);

    // Withdraw partial.
    let got = h.client.withdraw(&id, &None);
    assert_eq!(got, deposit / 2);
    h.assert_pool_invariant();

    // Advance to maturity.
    h.advance(duration / 2);
    let got2 = h.client.withdraw(&id, &None);
    assert_eq!(got2, deposit / 2);
    h.assert_pool_exact();
}

#[test]
fn cliff_blocks_early_withdrawal() {
    let h = Harness::new();
    let start = h.now();
    let duration = 2_000u64;
    let cliff_offset = 1_000u64;
    let deposit = duration as i128;

    let id = h.create(deposit, start, start + duration, start + cliff_offset);

    // Before cliff: nothing withdrawable.
    h.advance(cliff_offset - 1);
    assert_eq!(h.client.withdrawable_of(&id), 0);
    assert_eq!(h.client.vested_of(&id), 0);

    // At cliff: full accrual since start is released.
    h.advance(1); // now == cliff
    let v = h.client.vested_of(&id);
    assert!(v > 0, "cliff release must be positive");
    assert_eq!(v, deposit / 2, "half of deposit vests at cliff midpoint");

    h.assert_pool_exact();
}

#[test]
fn revoke_returns_unvested() {
    let h = Harness::new();
    let start = h.now();
    let duration = 1_000u64;
    let deposit = duration as i128;
    let id = h.create(deposit, start, start + duration, start);

    h.advance(250);
    let sender_before = h.token_client.balance(&h.sender);
    h.client.revoke_position(&id);

    // 25 % vested → 75 % refunded.
    let sender_after = h.token_client.balance(&h.sender);
    assert_eq!(sender_after - sender_before, deposit * 3 / 4);

    // Beneficiary can still withdraw the 25 % vested.
    let got = h.client.withdraw(&id, &None);
    assert_eq!(got, deposit / 4);
    h.assert_pool_exact();
}

#[test]
fn pause_freezes_accrual() {
    let h = Harness::new();
    let duration = 2_000u64;
    let deposit = duration as i128;
    let id = h.create_simple(deposit, duration);

    // Advance 500 s, then pause.
    h.advance(500);
    h.client.pause_position(&id);

    // Advance another 500 s while paused — accrual should stay frozen.
    h.advance(500);
    let v_paused = h.client.vested_of(&id);
    assert_eq!(v_paused, 500, "accrual frozen while paused");

    // Resume.
    h.client.resume_position(&id);
    h.advance(500); // 500 real seconds after resume
    let v_resumed = h.client.vested_of(&id);
    assert_eq!(
        v_resumed, 1_000,
        "1000 schedule-seconds elapsed after resume"
    );

    h.assert_pool_exact();
}
