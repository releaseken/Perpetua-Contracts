//! Pause/resume tests: role enforcement, clock freeze, accrual semantics.

use super::common::{Harness, DAY, YEAR};

#[test]
fn pause_freezes_accrual() {
    let h = Harness::new();
    let id = h.create_simple(YEAR as i128, YEAR);

    h.advance(DAY * 30);
    let vested_before_pause = h.client.vested_of(&id).unwrap();

    h.client.pause_schedule(&h.pauser, &id).unwrap();

    // Advance while paused — accrual must not increase.
    h.advance(DAY * 30);
    let vested_while_paused = h.client.vested_of(&id).unwrap();
    assert_eq!(
        vested_before_pause, vested_while_paused,
        "paused schedule must not accrue"
    );
}

#[test]
fn pause_requires_pauser_role() {
    let h = Harness::new();
    let id = h.create_simple(YEAR as i128, YEAR);
    let result = h.client.try_pause_schedule(&h.other, &id);
    assert!(result.is_err());
}

#[test]
fn resume_requires_pauser_role() {
    let h = Harness::new();
    let id = h.create_simple(YEAR as i128, YEAR);
    h.client.pause_schedule(&h.pauser, &id).unwrap();
    let result = h.client.try_resume_schedule(&h.other, &id);
    assert!(result.is_err());
}

#[test]
fn pause_then_resume_accumulates_paused_total() {
    let h = Harness::new();
    let id = h.create_simple(YEAR as i128, YEAR);

    h.advance(DAY * 10);
    h.client.pause_schedule(&h.pauser, &id).unwrap();
    h.advance(DAY * 5); // pause interval
    h.client.resume_schedule(&h.pauser, &id).unwrap();

    let s = h.get(id);
    assert!(
        s.paused_total >= DAY * 5,
        "paused_total must include the paused interval"
    );
    assert!(s.paused_at.is_none(), "paused_at must be cleared after resume");
}

#[test]
fn paused_clock_does_not_pass_cliff() {
    let h = Harness::new();
    // 90-day cliff.
    let cliff = DAY * 90;
    let id = h.create_with_cliff(YEAR as i128, YEAR, cliff);

    // Pause before the cliff and advance past what would have been the cliff.
    h.advance(DAY * 30);
    h.client.pause_schedule(&h.pauser, &id).unwrap();
    h.advance(DAY * 100); // 130 days total wall time, but clock frozen at 30 days

    // Cliff is at stream_time == 90 days; with the clock frozen at 30 days,
    // the cliff has not been reached.
    let available = h.client.withdrawable_of(&id).unwrap();
    assert_eq!(available, 0, "paused clock must not pass cliff");
}

#[test]
fn double_pause_returns_error() {
    let h = Harness::new();
    let id = h.create_simple(YEAR as i128, YEAR);
    h.client.pause_schedule(&h.pauser, &id).unwrap();
    let result = h.client.try_pause_schedule(&h.pauser, &id);
    assert!(result.is_err());
}

#[test]
fn resume_without_pause_returns_error() {
    let h = Harness::new();
    let id = h.create_simple(YEAR as i128, YEAR);
    let result = h.client.try_resume_schedule(&h.pauser, &id);
    assert!(result.is_err());
}

#[test]
fn pause_resume_invariants_hold() {
    let h = Harness::new();
    let id = h.create_simple(YEAR as i128, YEAR);

    for _ in 0..3 {
        h.advance(DAY * 10);
        h.client.pause_schedule(&h.pauser, &id).unwrap();
        h.advance(DAY * 5);
        h.client.resume_schedule(&h.pauser, &id).unwrap();
        h.assert_invariants();
    }
}
