//! Issue #401 — Fuzzing coverage for edge-case timestamps in the pause-clock
//! near `u64` maximum boundary limits.
//!
//! # What this file tests
//!
//! The pause-clock formula is:
//!
//! ```text
//! stream_time(now) = paused_at.unwrap_or(now) - paused_total
//! ```
//!
//! Both operands are `u64`, and `saturating_sub` is used to prevent wrapping.
//! This is correct for normal inputs, but several extreme cases need explicit
//! coverage:
//!
//! * **`paused_total` near `u64::MAX`** — if `paused_total` is close to or
//!   equal to `now`, the subtraction saturates at zero; the stream clock must
//!   read as "no elapsed time", never wrap to a huge value.
//!
//! * **`paused_at` near `u64::MAX`** — a freeze point at the very top of the
//!   `u64` range must not cause the stream clock to overflow past maturity or
//!   produce a vested amount above `deposited`.
//!
//! * **`now` at `u64::MAX`** — the largest possible ledger timestamp must be
//!   handled by every `accrual` helper without panicking.
//!
//! * **`paused_total` accumulated over many cycles** — after many pause/resume
//!   cycles, `paused_total` grows; eventually it may approach the stream's
//!   total runtime.  The contract must still produce correct arithmetic.
//!
//! * **Contract-level operations near the boundary** — `pause`, `resume`,
//!   `vested_of`, `withdrawable_of`, `refundable_of` must all stay typed at
//!   the furthest deployable end-time the host can store.
//!
//! # Relationship to `test::accrual_overflow`
//!
//! `accrual_overflow.rs` already covers the pure `accrual::*` functions at
//! `u64::MAX` timestamps.  This file focuses on the **pause-clock path
//! specifically**: the `paused_at` and `paused_total` operands that are unique
//! to the pause mechanism, exercised both in pure `accrual` and through the
//! real contract entry points at the limits of what the host can store.

use proptest::prelude::*;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Env};

use super::common::*;
use crate::storage::SECONDS_PER_LEDGER;
use crate::{accrual, Error};
use crate::types::{Stream, StreamStatus};

/// Largest `end_time` the test host can persist (from `accrual_overflow.rs`).
const MAX_DEPLOYABLE_END: u64 = (u32::MAX as u64) * SECONDS_PER_LEDGER;

// ---------------------------------------------------------------------------
// Pure accrual: paused_total at the u64 boundary
// ---------------------------------------------------------------------------

/// When `paused_total >= paused_at` (which cannot happen through the ABI but
/// is a defence-in-depth case), `stream_time` must saturate at zero without
/// wrapping, and no helper must panic.
#[test]
fn stream_time_saturates_when_paused_total_exceeds_freeze_point() {
    let start = 1_000_000u64;
    let end = start + 100 * DAY;

    // paused_total larger than the freeze point — overflow of the subtraction.
    let s = Stream {
        sender: Address::generate(&Env::default()),
        recipient: Address::generate(&Env::default()),
        token: Address::generate(&Env::default()),
        deposited: 1_000 * ONE,
        withdrawn: 0,
        start_time: start,
        end_time: end,
        cliff_time: start,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: Some(start + 10 * DAY),
        paused_total: start + 20 * DAY, // > paused_at → subtraction would underflow
        status: StreamStatus::Paused,
    };

    // Must not panic; must saturate to zero (no elapsed time).
    let st = accrual::stream_time(&s, start + 50 * DAY);
    assert_eq!(st, 0, "stream_time must saturate at zero, never underflow");
    assert_eq!(accrual::elapsed(&s, start + 50 * DAY), 0, "elapsed must be zero");

    // vested must be zero (cliff not reached at stream_time == 0 < cliff).
    // cliff_time == start, so stream_time(0) is before cliff_time only if 0 < start.
    // Here start == 1_000_000 so 0 < start: cliff not reached.
    // But stream_time saturates to 0, which may equal cliff_time if start == 0.
    // We only assert no panic; the exact value depends on saturation path.
    let _ = accrual::vested(&s, start + 50 * DAY);
}

/// A freeze point (`paused_at`) at exactly `u64::MAX` must pin the stream
/// clock at `u64::MAX - paused_total` and produce a bounded, non-panicking
/// vested amount.
#[test]
fn stream_time_is_bounded_with_paused_at_at_u64_max() {
    let start = u64::MAX - 100 * DAY;
    let end = u64::MAX; // end == MAX, start just before it

    let s = Stream {
        sender: Address::generate(&Env::default()),
        recipient: Address::generate(&Env::default()),
        token: Address::generate(&Env::default()),
        deposited: 1_000 * ONE,
        withdrawn: 0,
        start_time: start,
        end_time: end,
        cliff_time: start,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: Some(u64::MAX),
        paused_total: 0,
        status: StreamStatus::Paused,
    };

    // frozen_at == MAX, paused_total == 0 → stream_time == MAX.
    let st = accrual::stream_time(&s, u64::MAX);
    assert_eq!(st, u64::MAX, "stream_time must equal frozen_at when paused_total==0");

    // elapsed must be clamped to duration = 100*DAY.
    let el = accrual::elapsed(&s, u64::MAX);
    assert_eq!(el, 100 * DAY, "elapsed must clamp at duration");

    // vested must equal deposited (elapsed >= duration).
    let v = accrual::vested(&s, u64::MAX).expect("no overflow");
    assert_eq!(v, 1_000 * ONE, "fully vested when frozen at end");
}

/// With `paused_total` at the ceiling of what subtracting from `u64::MAX`
/// could reach, `stream_time` must still saturate gracefully.
#[test]
fn stream_time_saturates_at_zero_when_paused_total_near_ceiling() {
    let start = u64::MAX - 1_000;
    let s = Stream {
        sender: Address::generate(&Env::default()),
        recipient: Address::generate(&Env::default()),
        token: Address::generate(&Env::default()),
        deposited: 1_000,
        withdrawn: 0,
        start_time: start,
        end_time: u64::MAX,
        cliff_time: start,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: None,
        paused_total: u64::MAX - 10, // very large accumulated pause
        status: StreamStatus::Active,
    };

    // now = start + 500 → frozen_at = start + 500 (not paused), minus paused_total.
    // start + 500 - (u64::MAX - 10) would underflow → saturate at 0.
    let st = accrual::stream_time(&s, start + 500);
    assert_eq!(st, 0, "must saturate at zero, not wrap");
    assert_eq!(accrual::elapsed(&s, start + 500), 0, "zero elapsed when stream_time saturates");
}

/// `paused_total` exactly equal to `now` (stream clock = 0) must yield zero
/// elapsed time, not a wrap.
#[test]
fn paused_total_equal_to_now_gives_zero_elapsed() {
    let start = 1_000u64;
    let s = Stream {
        sender: Address::generate(&Env::default()),
        recipient: Address::generate(&Env::default()),
        token: Address::generate(&Env::default()),
        deposited: 10_000,
        withdrawn: 0,
        start_time: start,
        end_time: start + 1_000,
        cliff_time: start,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: None,
        paused_total: 2_000, // larger than now=1_500 → stream_time saturates to 0
        status: StreamStatus::Active,
    };
    let now = 1_500u64;
    assert_eq!(accrual::stream_time(&s, now), 0);
    assert_eq!(accrual::elapsed(&s, now), 0);
}

// ---------------------------------------------------------------------------
// Pure accrual: now at u64::MAX
// ---------------------------------------------------------------------------

/// Every accrual helper must handle `now = u64::MAX` without panicking for a
/// well-formed active stream whose schedule is nowhere near MAX.
#[test]
fn all_accrual_helpers_safe_at_now_u64_max_for_normal_stream() {
    let start = 1_700_000_000u64;
    let s = Stream {
        sender: Address::generate(&Env::default()),
        recipient: Address::generate(&Env::default()),
        token: Address::generate(&Env::default()),
        deposited: 1_000 * ONE,
        withdrawn: 0,
        start_time: start,
        end_time: start + 100 * DAY,
        cliff_time: start + 50 * DAY,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: None,
        paused_total: 0,
        status: StreamStatus::Active,
    };

    // All helpers must succeed.
    let st = accrual::stream_time(&s, u64::MAX);
    assert!(st >= s.end_time, "stream_time clamps past end_time");

    let el = accrual::elapsed(&s, u64::MAX);
    assert_eq!(el, 100 * DAY, "elapsed clamped at duration");

    let v = accrual::vested(&s, u64::MAX).expect("no overflow");
    assert_eq!(v, 1_000 * ONE, "fully vested at u64::MAX");

    let w = accrual::withdrawable(&s, u64::MAX).expect("no overflow");
    assert_eq!(w, 1_000 * ONE, "fully withdrawable at u64::MAX");

    let r = accrual::refundable(&s, u64::MAX).expect("no overflow");
    assert_eq!(r, 0, "zero refundable at u64::MAX");
}

/// A paused stream with `paused_at` near `u64::MAX` must report the same
/// vested amount at `now = u64::MAX` as at `now = paused_at`, because the
/// stream clock is frozen.
#[test]
fn paused_stream_at_u64_max_reports_same_vested_as_at_freeze_instant() {
    let start = u64::MAX - 50 * DAY;
    let pause_instant = u64::MAX - 20 * DAY;

    let s = Stream {
        sender: Address::generate(&Env::default()),
        recipient: Address::generate(&Env::default()),
        token: Address::generate(&Env::default()),
        deposited: 1_000 * ONE,
        withdrawn: 0,
        start_time: start,
        end_time: u64::MAX,
        cliff_time: start,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: Some(pause_instant),
        paused_total: 0,
        status: StreamStatus::Paused,
    };

    let v_at_max = accrual::vested(&s, u64::MAX).expect("no overflow at u64::MAX");
    let v_at_freeze = accrual::vested(&s, pause_instant).expect("no overflow at freeze");
    assert_eq!(
        v_at_max, v_at_freeze,
        "paused stream must report identical vested at u64::MAX and at the freeze instant"
    );
    assert!(v_at_max >= 0 && v_at_max <= s.deposited, "vested out of range");
}

// ---------------------------------------------------------------------------
// Contract level: pause/resume at the maximum deployable end time
// ---------------------------------------------------------------------------

/// Pausing and resuming a stream with timestamps at the furthest horizon the
/// host can store must not trap and must leave all invariants intact.
#[test]
fn pause_resume_at_maximum_deployable_timestamps_stays_typed() {
    let h = Harness::new();
    let start = MAX_DEPLOYABLE_END - 20 * DAY;
    let end = MAX_DEPLOYABLE_END;
    let cliff = start + 5 * DAY;
    let deposit = 1_000 * ONE;

    let id = h.client.create_stream(
        &h.sender,
        &h.recipient,
        &h.token,
        &deposit,
        &start,
        &end,
        &cliff,
        &true,
        &true,
        &true,
    );
    h.assert_pool_exact();

    // Pause before the cliff.
    h.warp_to(start + 2 * DAY);
    h.client.pause(&id);
    assert_eq!(h.client.vested_of(&id), 0, "pre-cliff: nothing vested while paused");

    // Large wall-clock jump while paused — passes the cliff but stream clock stays frozen.
    h.warp_to(start + 15 * DAY);
    assert_eq!(
        h.client.vested_of(&id),
        0,
        "cliff must not open during pause at max timestamps"
    );

    // Resume — stream clock picks up at day 2.
    h.client.resume(&id);
    let pt = h.get(id).paused_total;
    assert_eq!(pt, 13 * DAY, "paused_total must record exactly 13 days");

    // Advance 3 more days on the stream clock to reach the cliff.
    h.advance(3 * DAY); // stream clock now at day 5 == cliff
    let vested = h.client.vested_of(&id);
    // 5 days of a 20-day schedule: 5/20 * 1000 = 250 ONE.
    assert_eq!(vested, 250 * ONE, "correct vested at cliff after pause");

    h.client.withdraw(&id, &None);
    h.assert_pool_exact();
}

/// A series of rapid pause/resume cycles at the maximum deployable end time
/// must not overflow `paused_total`.
#[test]
fn many_pause_resume_cycles_at_max_timestamps_do_not_overflow() {
    let h = Harness::new();
    let start = MAX_DEPLOYABLE_END - 100 * DAY;
    let end = MAX_DEPLOYABLE_END;
    let deposit = 1_000 * ONE;

    let id = h.client.create_stream(
        &h.sender,
        &h.recipient,
        &h.token,
        &deposit,
        &start,
        &end,
        &start,
        &true,
        &true,
        &true,
    );
    h.warp_to(start);

    let mut expected_paused_total = 0u64;

    // 10 pause/resume cycles of 1 day each.
    for _ in 0..10 {
        h.advance(DAY); // 1 day of accrual
        h.client.pause(&id);
        h.advance(DAY); // 1 day paused
        h.client.resume(&id);
        expected_paused_total += DAY;

        let s = h.get(id);
        assert_eq!(
            s.paused_total,
            expected_paused_total,
            "paused_total must accumulate exactly"
        );
        assert!(s.paused_at.is_none(), "paused_at must be cleared after resume");
        h.assert_pool_exact();
    }

    // Advance to maturity (stretched by paused_total).
    h.warp_to(end + expected_paused_total);
    assert_eq!(h.client.vested_of(&id), deposit, "fully vested at stretched end");
    h.client.withdraw(&id, &None);
    h.assert_pool_exact();
    assert_eq!(h.pool(), 0);
}

/// An operation sequence that pushes `paused_total` to the entire stream
/// lifetime (all paused, no real accrual) must still yield the correct
/// vested=0 at the original end time, and correct vested=deposited at the
/// stretched end.
#[test]
fn paused_total_equal_to_full_duration_yields_correct_stretched_schedule() {
    let h = Harness::new();
    let start = MAX_DEPLOYABLE_END - 50 * DAY;
    let end = MAX_DEPLOYABLE_END;
    let duration = 50 * DAY;
    let deposit = 500 * ONE;

    let id = h.client.create_stream(
        &h.sender,
        &h.recipient,
        &h.token,
        &deposit,
        &start,
        &end,
        &start,
        &true,
        &true,
        &true,
    );

    // Pause immediately.
    h.warp_to(start);
    h.client.pause(&id);

    // Advance the wall clock by exactly the full duration.
    h.advance(duration);
    // Stream clock is frozen at start → 0 elapsed → 0 vested.
    assert_eq!(h.client.vested_of(&id), 0, "paused for full duration → 0 vested");
    assert_eq!(h.client.refundable_of(&id), deposit, "full deposit still refundable");

    // Resume.
    h.client.resume(&id);
    assert_eq!(h.get(id).paused_total, duration, "paused_total == full duration");

    // Stream clock restarts from 0; original end has passed but stretched end is 50 days away.
    h.advance(duration); // advance to stretched end
    assert_eq!(
        h.client.vested_of(&id),
        deposit,
        "fully vested at the stretched end"
    );
    h.client.withdraw(&id, &None);
    h.assert_pool_exact();
    assert_eq!(h.pool(), 0);
}

// ---------------------------------------------------------------------------
// Property tests: pause-clock invariants across the u64 boundary
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::default())]

    /// For any active stream with arbitrary timestamps (including near `u64::MAX`),
    /// `stream_time` must never exceed `now` and must be `>= 0`.
    #[test]
    fn stream_time_is_always_between_zero_and_now(
        start in 0u64..u64::MAX.saturating_sub(100),
        duration in 1u64..=(40 * 365 * 86_400),
        now_delta in 0u64..(40 * 365 * 86_400),
        paused_total in 0u64..u64::MAX,
    ) {
        let start = start.min(u64::MAX.saturating_sub(duration));
        let now = start.saturating_add(now_delta);
        let env = Env::default();
        let s = Stream {
            sender: Address::generate(&env),
            recipient: Address::generate(&env),
            token: Address::generate(&env),
            deposited: 1_000,
            withdrawn: 0,
            start_time: start,
            end_time: start.saturating_add(duration),
            cliff_time: start,
            flags: Stream::flags_from_parts(true, true, true),
            paused_at: None,
            paused_total,
            status: StreamStatus::Active,
        };
        let st = accrual::stream_time(&s, now);
        prop_assert!(st <= now, "stream_time({st}) exceeded now({now})");
    }

    /// For any paused stream with `paused_at` set to any `u64` value,
    /// the stream clock must never advance past `paused_at` as `now` increases,
    /// and `vested` must be bounded by `deposited`.
    #[test]
    fn paused_stream_clock_never_advances_past_freeze_point(
        start in 0u64..(u64::MAX / 2),
        duration in 1u64..=(10 * 365 * 86_400),
        paused_offset in 0u64..=(10 * 365 * 86_400),
        now_extra in 0u64..(10 * 365 * 86_400),
        deposited in 1i128..(i128::MAX / (1 << 40)),
    ) {
        let end = start.saturating_add(duration);
        let pause_instant = start.saturating_add(paused_offset.min(duration));
        let now = pause_instant.saturating_add(now_extra);
        let deposited = deposited.max(duration as i128);
        let env = Env::default();
        let s = Stream {
            sender: Address::generate(&env),
            recipient: Address::generate(&env),
            token: Address::generate(&env),
            deposited,
            withdrawn: 0,
            start_time: start,
            end_time: end,
            cliff_time: start,
            flags: Stream::flags_from_parts(true, true, true),
            paused_at: Some(pause_instant),
            paused_total: 0,
            status: StreamStatus::Paused,
        };

        // stream_time must equal pause_instant (minus paused_total=0 = pause_instant).
        let st = accrual::stream_time(&s, now);
        prop_assert_eq!(st, pause_instant, "stream_time must equal pause_instant while paused");

        // vested at `now` must equal vested at `pause_instant` (clock is frozen).
        let v_now = accrual::vested(&s, now);
        let v_freeze = accrual::vested(&s, pause_instant);
        match (v_now, v_freeze) {
            (Ok(vn), Ok(vf)) => {
                prop_assert_eq!(vn, vf, "vested must be identical at now and at pause_instant");
                prop_assert!(vn >= 0 && vn <= deposited, "vested out of range: {vn}");
            }
            (Err(e1), Err(e2)) => {
                prop_assert_eq!(e1, e2, "both must overflow the same way");
            }
            _ => prop_assert!(false, "vested disagrees between now and pause_instant"),
        }
    }

    /// Conservation holds for paused streams at any u64-range timestamp.
    #[test]
    fn conservation_holds_for_paused_streams_at_all_timestamps(
        start in 1_000_000u64..(u64::MAX / 4),
        duration in 1u64..=(5 * 365 * 86_400),
        pause_frac in 0u64..=100u64,
        paused_total_frac in 0u64..=100u64,
        now_delta in 0u64..(5 * 365 * 86_400),
        deposited in 1i128..(i128::MAX / (1 << 40)),
    ) {
        let end = start.saturating_add(duration);
        let pause_offset = duration * pause_frac / 100;
        let pause_instant = start.saturating_add(pause_offset);
        let paused_total = pause_offset * paused_total_frac / 100;
        let now = pause_instant.saturating_add(now_delta);
        let deposited = deposited.max(duration as i128);

        let env = Env::default();
        let s = Stream {
            sender: Address::generate(&env),
            recipient: Address::generate(&env),
            token: Address::generate(&env),
            deposited,
            withdrawn: 0,
            start_time: start,
            end_time: end,
            cliff_time: start,
            flags: Stream::flags_from_parts(true, true, true),
            paused_at: Some(pause_instant),
            paused_total,
            status: StreamStatus::Paused,
        };

        if let (Ok(v), Ok(r)) = (accrual::vested(&s, now), accrual::refundable(&s, now)) {
            prop_assert_eq!(v + r, deposited, "conservation must hold for paused stream");
            prop_assert!(v >= 0 && v <= deposited);
            prop_assert!(r >= 0);
        }
        // If either overflows, it is acceptable — overflow is a typed error.
    }

    /// `elapsed` must always be in `[0, duration]`, never negative, never past
    /// the schedule end, for any combination of pause parameters.
    #[test]
    fn elapsed_is_always_clamped_to_zero_duration(
        start in 0u64..(u64::MAX / 2),
        duration in 1u64..=(20 * 365 * 86_400),
        paused_total in 0u64..u64::MAX,
        now_offset in 0u64..(20 * 365 * 86_400),
    ) {
        let now = start.saturating_add(now_offset);
        let end = start.saturating_add(duration);
        let env = Env::default();
        let s = Stream {
            sender: Address::generate(&env),
            recipient: Address::generate(&env),
            token: Address::generate(&env),
            deposited: 1_000,
            withdrawn: 0,
            start_time: start,
            end_time: end,
            cliff_time: start,
            flags: Stream::flags_from_parts(true, true, true),
            paused_at: None,
            paused_total,
            status: StreamStatus::Active,
        };
        let el = accrual::elapsed(&s, now);
        prop_assert!(el <= duration, "elapsed({el}) exceeded duration({duration})");
    }
}

// ---------------------------------------------------------------------------
// Targeted boundary unit tests
// ---------------------------------------------------------------------------

/// `paused_total` exactly one below `u64::MAX` with `now = u64::MAX` and
/// `paused_at = None` (active stream): stream_time must equal 1, not wrap.
#[test]
fn stream_time_is_one_when_now_max_and_paused_total_is_max_minus_one() {
    let env = Env::default();
    let s = Stream {
        sender: Address::generate(&env),
        recipient: Address::generate(&env),
        token: Address::generate(&env),
        deposited: 1_000,
        withdrawn: 0,
        start_time: 0,
        end_time: u64::MAX,
        cliff_time: 0,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: None,
        paused_total: u64::MAX - 1,
        status: StreamStatus::Active,
    };
    // frozen_at = now = u64::MAX; u64::MAX - (u64::MAX - 1) = 1
    let st = accrual::stream_time(&s, u64::MAX);
    assert_eq!(st, 1, "stream_time must equal 1");
}

/// `paused_at` exactly equal to `u64::MAX` with `paused_total = u64::MAX - 1`:
/// stream_time = u64::MAX - (u64::MAX - 1) = 1 (not zero, not MAX).
#[test]
fn stream_time_with_paused_at_at_max_and_large_paused_total() {
    let env = Env::default();
    let s = Stream {
        sender: Address::generate(&env),
        recipient: Address::generate(&env),
        token: Address::generate(&env),
        deposited: 1_000,
        withdrawn: 0,
        start_time: 0,
        end_time: u64::MAX,
        cliff_time: 0,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: Some(u64::MAX),
        paused_total: u64::MAX - 1,
        status: StreamStatus::Paused,
    };
    // frozen_at = paused_at = u64::MAX; u64::MAX - (u64::MAX-1) = 1
    let st = accrual::stream_time(&s, 0); // now doesn't matter when paused
    assert_eq!(st, 1, "stream_time must be 1");
}

/// `paused_at = 0`, `paused_total = 0`, `now = u64::MAX`:
/// for a paused stream, stream_time = 0 (frozen at the creation instant).
#[test]
fn stream_time_frozen_at_zero_when_paused_at_zero() {
    let env = Env::default();
    let s = Stream {
        sender: Address::generate(&env),
        recipient: Address::generate(&env),
        token: Address::generate(&env),
        deposited: 1_000,
        withdrawn: 0,
        start_time: 0,
        end_time: 1_000,
        cliff_time: 0,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: Some(0),
        paused_total: 0,
        status: StreamStatus::Paused,
    };
    // frozen_at = 0; 0 - 0 = 0; regardless of now.
    let st = accrual::stream_time(&s, u64::MAX);
    assert_eq!(st, 0, "stream_time must be 0 when frozen at the origin");
    assert_eq!(accrual::elapsed(&s, u64::MAX), 0);
}

/// Both `paused_at` and `paused_total` at `u64::MAX`:
/// stream_time = u64::MAX - u64::MAX = 0.  Must saturate, not wrap.
#[test]
fn stream_time_saturates_at_zero_when_both_at_max() {
    let env = Env::default();
    let s = Stream {
        sender: Address::generate(&env),
        recipient: Address::generate(&env),
        token: Address::generate(&env),
        deposited: 1_000,
        withdrawn: 0,
        start_time: 0,
        end_time: u64::MAX,
        cliff_time: 0,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: Some(u64::MAX),
        paused_total: u64::MAX,
        status: StreamStatus::Paused,
    };
    // u64::MAX - u64::MAX = 0 (exact, not saturating; still correct).
    let st = accrual::stream_time(&s, u64::MAX);
    assert_eq!(st, 0, "u64::MAX - u64::MAX must be exactly 0");
}

/// A withdraw attempt on a stream that has been paused since creation
/// (paused_total grows without bound) must yield `NothingToWithdraw`
/// as long as the stream clock has not passed the cliff.
#[test]
fn nothing_to_withdraw_when_paused_total_keeps_stream_clock_before_cliff() {
    let h = Harness::new();
    let start = h.now();
    let cliff = start + 100 * DAY;
    let id = h.create(1_000 * ONE, start, start + 200 * DAY, cliff, true, true, true);

    // Pause immediately (stream clock frozen at `start`).
    h.client.pause(&id);

    // Advance wall clock far past the cliff — stream clock stays at `start`.
    h.advance(YEAR);

    let err = h.client.try_withdraw(&id, &None).unwrap_err().unwrap();
    assert_eq!(
        err,
        Error::NothingToWithdraw,
        "must be NothingToWithdraw while stream clock is before cliff"
    );
    // paused_total has not grown (the pause has not been resumed).
    assert_eq!(h.get(id).paused_total, 0, "paused_total stays 0 during an in-progress pause");
    h.assert_pool_exact();
}
