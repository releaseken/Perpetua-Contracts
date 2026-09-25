//! Conservation invariant checker for the stream contract.
//!
//! The core invariant of Perpetua is exact balance conservation:
//!
//! ```text
//! vested(t) + refundable(t) == deposited      for all t
//! ```
//!
//! Any violation means the contract's balance could drift from its actual
//! accounting liability, leading to insolvency or locked funds. This module
//! provides a single reusable checker that unit tests and proptests can call
//! *before and after* every state-modifying transaction, so a breach panics
//! immediately with a clear message rather than surfacing later as a mysterious
//! shortfall.
//!
//! The checker is deliberately pure and host-free: it drives [`crate::accrual`]
//! directly, so it can be invoked from the cheap property tests as well as from
//! full contract tests without paying for a host invocation.

use crate::accrual;
use crate::types::{Stream, StreamStatus};

/// Assert that `vested(t) + refundable(t) == deposited` holds for `stream` at
/// `now`.
///
/// Panics with a descriptive message if the invariant is ever breached. Call
/// this before and after every state-modifying transaction to catch drift at
/// the exact step that introduced it.
///
/// The check is valid in every stream state:
///
/// * `Active` — the ordinary accrual case.
/// * `Paused` — accrual is frozen at `paused_at`, so the same identity must
///   still hold against the frozen clock.
/// * `Cancelled` — accrual stops at cancellation; the complement is refundable.
/// * `Depleted` — everything has vested, so `refundable` must be exactly zero.
///
pub fn assert_conservation(stream: &Stream, now: u64) {
    let vested = accrual::vested(stream, now)
        .unwrap_or_else(|e| panic!("conservation: vested() failed at t={}: {:?}", now, e));
    let refundable = accrual::refundable(stream, now)
        .unwrap_or_else(|e| panic!("conservation: refundable() failed at t={}: {:?}", now, e));

    let total = vested
        .checked_add(refundable)
        .unwrap_or_else(|| panic!("conservation: vested + refundable overflowed at t={}", now));

    assert_eq!(
        total, stream.deposited,
        "conservation breached at t={} (status={:?}): vested({}) + refundable({}) = {} != deposited({})",
        now, stream.status, vested, refundable, total, stream.deposited,
    );
}

/// Assert conservation across a representative set of instants for `stream`:
/// before the start, at the start, at the cliff, mid-schedule, at the end, and
/// well past the end.
///
/// This is the "validate math across Active, Paused, Cancelled, and Depleted
/// states" entry point: it exercises the identity at every phase boundary so a
/// single call covers the whole lifecycle of the schedule.
pub fn assert_conservation_across_lifecycle(stream: &Stream) {
    let start = stream.start_time;
    let end = stream.end_time;
    let cliff = stream.cliff_time;

    // Before the start: nothing has vested, everything is refundable.
    assert_conservation(stream, start.saturating_sub(1));
    // At the start.
    assert_conservation(stream, start);
    // At the cliff (may equal start when there is no cliff).
    assert_conservation(stream, cliff);
    // Mid-schedule.
    assert_conservation(stream, start + (end - start) / 2);
    // At the end: fully vested, nothing refundable.
    assert_conservation(stream, end);
    // Well past the end: still fully vested, still nothing refundable.
    assert_conservation(stream, end.saturating_add(365 * 86_400));
}

/// Assert the state-specific consequences of conservation.
///
/// Beyond the raw identity, each terminal state pins down one side of the
/// equation, which is what makes the invariant meaningful rather than vacuous:
///
/// * `Depleted` — `refundable` must be exactly zero.
/// * `Cancelled` — accrual is frozen, so `vested` must not grow with time.
/// * `Paused` — accrual is frozen at `paused_at`.
pub fn assert_state_conservation(stream: &Stream, now: u64) {
    assert_conservation(stream, now);

    match stream.status {
        StreamStatus::Depleted => {
            let refundable = accrual::refundable(stream, now)
                .unwrap_or_else(|e| panic!("conservation: refundable() failed at t={}: {:?}", now, e));
            assert_eq!(
                refundable, 0,
                "conservation: depleted stream still has refundable={} at t={}",
                refundable, now,
            );
        }
        StreamStatus::Cancelled => {
            // Cancellation freezes accrual: the entitlement at any later instant
            // must equal the entitlement at the cancellation instant.
            let frozen = accrual::vested(stream, stream.end_time)
                .unwrap_or_else(|e| panic!("conservation: vested() failed: {:?}", e));
            let at_now = accrual::vested(stream, now)
                .unwrap_or_else(|e| panic!("conservation: vested() failed at t={}: {:?}", now, e));
            assert_eq!(
                at_now, frozen,
                "conservation: cancelled stream accrued after cancellation: {} != {} at t={}",
                at_now, frozen, now,
            );
        }
        StreamStatus::Paused => {
            // Paused accrual is frozen at `paused_at`.
            if let Some(paused_at) = stream.paused_at {
                let frozen = accrual::vested(stream, paused_at)
                    .unwrap_or_else(|e| panic!("conservation: vested() failed: {:?}", e));
                let at_now = accrual::vested(stream, now)
                    .unwrap_or_else(|e| panic!("conservation: vested() failed at t={}: {:?}", now, e));
                assert_eq!(
                    at_now, frozen,
                    "conservation: paused stream accrued past paused_at: {} != {} at t={}",
                    at_now, frozen, now,
                );
            }
        }
        StreamStatus::Active => {}
    }
}
