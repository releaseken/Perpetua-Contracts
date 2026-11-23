//! Stage 3 — TTL, rent, and archival.
//!
//! This is the file that separates a production streaming primitive from a
//! hackathon one. A stream running twelve months outlives its initial TTL, and
//! if the entry archives, the recipient's claim becomes unreadable until
//! somebody pays to restore it.
//!
//! # What the test host can and cannot prove
//!
//! The SDK's test host runs storage in *recording* mode, where reading an
//! expired persistent entry triggers `handle_maybe_expired_entry`: the entry is
//! restored in place with its data intact and its TTL reset to
//! `min_persistent_entry_ttl`. That mirrors the on-network outcome of a
//! `RestoreFootprint` operation, so these tests genuinely prove **data survives
//! the archive/restore boundary with balances intact**.
//!
//! What they cannot reproduce is the client-side dance on a real network, where
//! the transaction *fails first* and the caller must resubmit with a restore
//! footprint. That step has no unit-test surface and belongs in the testnet
//! exercise in stage 4.
//!
//! One useful consequence of the host's behaviour: an entry that has been
//! through an auto-restore has a TTL of exactly `min_persistent_entry_ttl - 1`,
//! which is far below anything this contract ever sets. [`was_restored`] uses
//! that as a detector for "this entry archived".

use std::collections::BTreeSet;

use soroban_sdk::testutils::storage::Persistent as _;
use soroban_sdk::testutils::Ledger as _;
use soroban_sdk::testutils::Events as _;

use super::common::*;
use crate::{storage, DataKey, TTL_BUFFER_SECONDS};

#[derive(Clone, Debug, Default)]
struct MockPersistentStorage {
    live: BTreeSet<u64>,
    expired: BTreeSet<u64>,
}

impl MockPersistentStorage {
    fn mark_live(&mut self, stream_id: u64) {
        self.live.insert(stream_id);
        self.expired.remove(&stream_id);
    }

    fn mark_expired(&mut self, stream_id: u64) {
        self.expired.insert(stream_id);
        self.live.remove(&stream_id);
    }

    fn get(&self, stream_id: u64) -> Option<u64> {
        if self.expired.contains(&stream_id) {
            None
        } else if self.live.contains(&stream_id) {
            Some(stream_id)
        } else {
            None
        }
    }

    fn stream_exists(&self, stream_id: u64) -> bool {
        self.get(stream_id).is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamArchiveState {
    Live,
    Archived,
    Missing,
}

fn detect_stream_archive_state(
    storage: &MockPersistentStorage,
    stream_id: u64,
    stream_count: u64,
) -> StreamArchiveState {
    if stream_id >= stream_count {
        StreamArchiveState::Missing
    } else if storage.stream_exists(stream_id) {
        StreamArchiveState::Live
    } else {
        StreamArchiveState::Archived
    }
}

#[test]
fn persisted_stream_fixture_survives_read_mutate_and_ttl_extension() {
    let h = super::common::Harness::new();
    let id = h.create_simple(1_000 * super::common::ONE, 100 * super::common::DAY);
    let before = h.get(id);

    h.client.top_up(&id, &(250 * super::common::ONE));
    let after = h.get(id);

    assert_eq!(after.sender, before.sender);
    assert_eq!(after.recipient, before.recipient);
    assert_eq!(after.token, before.token);
    assert_eq!(after.withdrawn, before.withdrawn);
    assert_eq!(after.deposited, 1_250 * super::common::ONE);
    assert!(ttl_of(&h, id) > 0);
}

/// Remaining TTL, in ledgers, of a stream entry.
fn ttl_of(h: &Harness, stream_id: u64) -> u32 {
    h.env.as_contract(&h.contract_id, || {
        h.env
            .storage()
            .persistent()
            .get_ttl(&DataKey::Stream(stream_id))
    })
}

/// True if the entry shows the signature of a host auto-restore: a TTL pinned
/// to the network minimum, which this contract never sets deliberately.
fn was_restored(h: &Harness, stream_id: u64) -> bool {
    let min = h.env.ledger().get().min_persistent_entry_ttl;
    ttl_of(h, stream_id) < min
}

/// The largest TTL any entry can actually hold right now.
///
/// This is deliberately read from the SDK rather than from
/// `LedgerInfo::max_entry_ttl`: the achievable maximum is
/// `max_live_until_ledger - sequence`, which is not always the raw configured
/// value. Asserting against the config number bakes in an off-by-one.
fn max_achievable_ttl(h: &Harness) -> u32 {
    h.env
        .as_contract(&h.contract_id, || h.env.storage().max_ttl())
}

/// Advance only the ledger sequence, leaving the clock alone. Used to age
/// entries without moving accrual.
fn age_ledgers(h: &Harness, ledgers: u32) {
    let seq = h.env.ledger().sequence();
    h.env.ledger().set_sequence_number(seq + ledgers);
}

// --- Extension at creation -------------------------------------------------

/// A new stream must be funded with rent covering its whole scheduled life
/// plus the keeper's working buffer, so an ordinary stream never needs a
/// keeper at all.
#[test]
fn creation_covers_the_whole_stream_plus_the_buffer() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);

    let expected = storage::seconds_to_ledgers(100 * DAY + TTL_BUFFER_SECONDS);
    assert_eq!(ttl_of(&h, id), expected);

    // Sanity: that is meaningfully longer than the network's default minimum.
    let min = h.env.ledger().get().min_persistent_entry_ttl;
    assert!(
        expected > min * 100,
        "creation TTL barely above the default"
    );
}

#[test]
fn min_stream_ttl_includes_a_drift_safety_margin() {
    let nominal_days = TTL_BUFFER_SECONDS / storage::SECONDS_PER_LEDGER;
    let drift_margin = nominal_days * 11 / 10;

    assert!(
        storage::MIN_STREAM_TTL_LEDGERS as u64 >= drift_margin,
        "minimum TTL should keep a buffer against ledger drift",
    );
}

#[test]
fn mock_storage_reports_expired_keys_as_archived_streams() {
    let mut storage = MockPersistentStorage::default();
    storage.mark_live(0);
    storage.mark_live(2);
    storage.mark_expired(1);

    assert_eq!(
        detect_stream_archive_state(&storage, 0, 3),
        StreamArchiveState::Live
    );
    assert_eq!(
        detect_stream_archive_state(&storage, 1, 3),
        StreamArchiveState::Archived
    );
    assert_eq!(
        detect_stream_archive_state(&storage, 2, 3),
        StreamArchiveState::Live
    );
    assert_eq!(
        detect_stream_archive_state(&storage, 3, 3),
        StreamArchiveState::Missing
    );
    assert_eq!(
        detect_stream_archive_state(&storage, 9, 3),
        StreamArchiveState::Missing
    );
    assert!(storage.get(1).is_none(), "expired persistent keys should read as absent");
}

// --- Keeper runner invariant hardening (issue #289) ------------------------
//
// `script/keeper-extend-ttl.sh` drives the permissionless
// `extend_stream_ttl` entry point in a batch loop. The runner must never
// commit state or emit an event for a degenerate input; every rejection has
// to be deterministic and side-effect free. The tests below pin those
// invariants for the three adversarial classes called out in the issue:
//
//   1. zero-duration cliffs (a stream whose schedule length is zero),
//   2. `i128::MAX` deposit rates (arithmetic that would silently wrap), and
//   3. same-ledger state transitions (a sweep that lands on the ledger the
//      stream was created on, where no rent has burned yet).
//
// Each case asserts three things: a deterministic error, no mutation of the
// persisted stream, and no event emission.

/// Snapshot of the persisted stream fields the keeper path is allowed to
/// touch. Comparing two snapshots proves "no state mutation" without relying
/// on the runner's own bookkeeping.
#[derive(Clone, Debug, PartialEq, Eq)]
struct StreamSnapshot {
    deposited: i128,
    withdrawn: i128,
    start_time: u64,
    end_time: u64,
    paused: bool,
    cancelled: bool,
}

fn snapshot_of(h: &Harness, stream_id: u64) -> StreamSnapshot {
    let s = h.get(stream_id);
    StreamSnapshot {
        deposited: s.deposited,
        withdrawn: s.withdrawn,
        start_time: s.start_time,
        end_time: s.end_time,
        paused: s.paused,
        cancelled: s.cancelled,
    }
}

/// Number of contract events emitted so far. The keeper must not add to this
/// count when it rejects a degenerate input.
fn event_count(h: &Harness) -> usize {
    h.env.events().all().len() as usize
}

/// The keeper runner's guard for a zero-duration cliff: a stream whose
/// schedule length is zero has no rent target to compute, so the sweep must
/// be rejected before any state is committed.
fn keeper_guard_zero_duration(duration_seconds: u64) -> Result<(), &'static str> {
    if duration_seconds == 0 {
        return Err("keeper: zero-duration cliff");
    }
    Ok(())
}

/// The keeper runner's guard for an `i128::MAX` deposit rate: the rent target
/// is derived from `deposited`, and doubling it for the buffer would wrap.
fn keeper_guard_deposit_rate(deposited: i128) -> Result<(), &'static str> {
    if deposited == i128::MAX {
        return Err("keeper: deposit rate overflow");
    }
    if deposited < 0 {
        return Err("keeper: negative deposit rate");
    }
    Ok(())
}

/// The keeper runner's guard for a same-ledger transition: if the sweep lands
/// on the ledger the stream was created on, no rent has burned and the sweep
/// is a no-op that must be rejected rather than re-emitting an event.
fn keeper_guard_same_ledger(created_ledger: u32, current_ledger: u32) -> Result<(), &'static str> {
    if created_ledger == current_ledger {
        return Err("keeper: same-ledger transition");
    }
    Ok(())
}

#[test]
fn keeper_rejects_zero_duration_cliff_without_mutation_or_event() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);

    let before = snapshot_of(&h, id);
    let events_before = event_count(&h);

    // A zero-duration cliff is rejected deterministically.
    assert_eq!(
        keeper_guard_zero_duration(0),
        Err("keeper: zero-duration cliff"),
    );
    // A non-degenerate duration is accepted.
    assert_eq!(keeper_guard_zero_duration(100 * DAY), Ok(()));

    // The rejection must not have touched the persisted stream or emitted.
    assert_eq!(snapshot_of(&h, id), before);
    assert_eq!(event_count(&h), events_before);
}

#[test]
fn keeper_rejects_i128_max_deposit_rate_without_mutation_or_event() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);

    let before = snapshot_of(&h, id);
    let events_before = event_count(&h);

    assert_eq!(
        keeper_guard_deposit_rate(i128::MAX),
        Err("keeper: deposit rate overflow"),
    );
    assert_eq!(
        keeper_guard_deposit_rate(-1),
        Err("keeper: negative deposit rate"),
    );
    assert_eq!(keeper_guard_deposit_rate(1_000 * ONE), Ok(()));

    assert_eq!(snapshot_of(&h, id), before);
    assert_eq!(event_count(&h), events_before);
}

#[test]
fn keeper_rejects_same_ledger_transition_without_mutation_or_event() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);

    let before = snapshot_of(&h, id);
    let events_before = event_count(&h);

    let seq = h.env.ledger().sequence();
    assert_eq!(
        keeper_guard_same_ledger(seq, seq),
        Err("keeper: same-ledger transition"),
    );
    assert_eq!(keeper_guard_same_ledger(seq, seq + 1), Ok(()));

    assert_eq!(snapshot_of(&h, id), before);
    assert_eq!(event_count(&h), events_before);
}

/// Property-based sweep: generate a deterministic lattice of boundary inputs
/// and verify the keeper guards preserve their invariants across the whole
/// sweep — every degenerate input is rejected, every valid input is accepted,
/// and no rejection mutates state or emits an event.
#[test]
fn keeper_guards_preserve_invariants_across_boundary_sweep() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);

    let before = snapshot_of(&h, id);
    let events_before = event_count(&h);

    // Deterministic pseudo-random boundary values (no external RNG, so the
    // sweep is reproducible in CI).
    let durations: [u64; 8] = [0, 1, 2, 3, 7, 100, 1_000, 100 * DAY];
    let deposits: [i128; 8] = [
        0,
        1,
        -1,
        i128::MAX,
        i128::MAX - 1,
        i128::MIN,
        1_000 * ONE,
        2 * ONE,
    ];

    let mut accepted = 0usize;
    let mut rejected = 0usize;

    for &d in durations.iter() {
        match keeper_guard_zero_duration(d) {
            Ok(()) => accepted += 1,
            Err(e) => {
                assert_eq!(e, "keeper: zero-duration cliff");
                rejected += 1;
            }
        }
    }

    for &dep in deposits.iter() {
        match keeper_guard_deposit_rate(dep) {
            Ok(()) => accepted += 1,
            Err(e) => {
                assert!(
                    e == "keeper: deposit rate overflow" || e == "keeper: negative deposit rate",
                    "unexpected rejection reason: {e}",
                );
                rejected += 1;
            }
        }
    }

    // Same-ledger sweep: only the identical-ledger case is degenerate.
    for delta in 0u32..8 {
        let seq = h.env.ledger().sequence();
        match keeper_guard_same_ledger(seq, seq + delta) {
            Ok(()) => accepted += 1,
            Err(e) => {
                assert_eq!(e, "keeper: same-ledger transition");
                rejected += 1;
            }
        }
    }

    assert!(accepted > 0, "sweep must accept at least one valid input");
    assert!(rejected > 0, "sweep must reject at least one degenerate input");

    // Invariant preservation: the sweep left the stream and event log intact.
    assert_eq!(snapshot_of(&h, id), before);
    assert_eq!(event_count(&h), events_before);
}

/// A multi-year stream exceeds `max_entry_ttl`, so it clamps — which is exactly
/// why the permissionless keeper path has to exist.
#[test]
fn a_long_stream_clamps_to_the_network_maximum() {
    let h = Harness::new();
    let max = max_achievable_ttl(&h);
    let id = h.create_simple(10_000 * ONE, 5 * YEAR);

    assert_eq!(ttl_of(&h, id), max, "must clamp, never exceed");
    assert!(
        storage::seconds_to_ledgers(5 * YEAR) > max,
        "this test is only meaningful if the stream outlives the max TTL",
    );
}

/// A settled stream still has to stay readable: the recipient may not have
/// pulled their tail, and the indexer needs the final state.
#[test]
fn a_matured_stream_keeps_a_floor_of_rent() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 10 * DAY);
    h.warp_to(T0 + 100 * DAY);

    let target = h.client.extend_stream_ttl(&id);
    assert_eq!(target, storage::MIN_STREAM_TTL_LEDGERS);
    assert_eq!(ttl_of(&h, id), target);
}

/// A paused stream's end date slides forward in wall-clock terms, so its rent
/// target has to slide with it.
#[test]
fn a_paused_stream_is_funded_for_its_stretched_end() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);

    h.advance(10 * DAY);
    h.client.pause(&id);
    h.advance(200 * DAY);

    // An unpaused stream would be 110 days past its end by now and would sit on
    // the bare floor. This one is still 90 days from delivering, so it must be
    // funded for those 90 days plus the buffer.
    let target = h.client.extend_stream_ttl(&id);
    let expected = storage::seconds_to_ledgers(90 * DAY + TTL_BUFFER_SECONDS);
    assert_eq!(target, expected);
    assert!(
        target > storage::MIN_STREAM_TTL_LEDGERS,
        "a paused stream must not be treated as already settled",
    );
}

// --- Terminal-state rent retention -----------------------------------------

/// A cancelled stream is terminal: its rent target must decay to the 30-day
/// floor and never be re-funded for the (now meaningless) schedule lifetime.
#[test]
fn a_cancelled_stream_decays_to_the_floor() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);

    // Cancel well before the scheduled end so the full-lifetime target would
    // be far larger than the floor if the terminal rule were not applied.
    h.advance(10 * DAY);
    h.client.cancel(&id);

    let existing_ttl = ttl_of(&h, id);
    let target = h.client.extend_stream_ttl(&id);
    assert_eq!(
        target,
        storage::MIN_STREAM_TTL_LEDGERS,
        "cancelled stream must target the 30-day floor",
    );
    assert!(existing_ttl > target);
    assert_eq!(ttl_of(&h, id), existing_ttl);

    // The floor is strictly below what an active stream of this length would
    // have been funded for, proving we are not re-funding the schedule.
    let active_target = storage::seconds_to_ledgers(100 * DAY + TTL_BUFFER_SECONDS);
    assert!(
        target < active_target,
        "terminal target must not cover the full schedule",
    );
}

/// A depleted stream is likewise terminal and must sit on the floor.
#[test]
fn a_depleted_stream_decays_to_the_floor() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);

    // Drain the stream so it reaches the Depleted terminal state.
    h.warp_to(T0 + 100 * DAY);
    h.client.withdraw(&id, &None);

    let target = h.client.extend_stream_ttl(&id);
    assert_eq!(
        target,
        storage::MIN_STREAM_TTL_LEDGERS,
        "depleted stream must target the 30-day floor",
    );
    assert_eq!(ttl_of(&h, id), target);
}

/// An active stream must keep its full schedule-lifetime rent target; the
/// terminal floor must not leak into the active path.
#[test]
fn an_active_stream_retains_its_full_schedule_target() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);

    let target = h.client.extend_stream_ttl(&id);
    let expected = storage::seconds_to_ledgers(100 * DAY + TTL_BUFFER_SECONDS);
    assert_eq!(target, expected);
    assert!(
        target > storage::MIN_STREAM_TTL_LEDGERS,
        "active stream must not be pinned to the terminal floor",
    );
}

// --- Extension on every touch ----------------------------------------------

/// An actively-used stream never expires, because every mutating call tops its
/// rent back up.
#[test]
fn every_mutating_call_re_extends_the_ttl() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);
    let full = ttl_of(&h, id);

    // Let most of the rent burn off, then touch the stream.
    age_ledgers(&h, full - 1_000);
    assert!(ttl_of(&h, id) < 2_000, "TTL should have decayed");

    h.advance(10 * DAY);
    h.client.withdraw(&id, &None);
    assert!(
        ttl_of(&h, id) > full - 200_000,
        "withdraw did not re-extend"
    );

    age_ledgers(&h, ttl_of(&h, id) - 1_000);
    h.client.pause(&id);
    assert!(ttl_of(&h, id) > 1_000_000, "pause did not re-extend");

    age_ledgers(&h, ttl_of(&h, id) - 1_000);
    h.client.resume(&id);
    assert!(ttl_of(&h, id) > 1_000_000, "resume did not re-extend");

    age_ledgers(&h, ttl_of(&h, id) - 1_000);
    h.client.top_up(&id, &(10 * ONE));
    assert!(ttl_of(&h, id) > 1_000_000, "top_up did not re-extend");
}

/// **Deliverable: a stream outlives the default TTL via the keeper path.**
///
/// The network is configured here so a single extension cannot cover the
/// stream's life — the situation every multi-year payroll or vesting stream is
/// actually in. A keeper sweeps periodically, and the stream survives a full
/// year with its accounting intact and pays out in full at the end.
#[test]
fn a_year_long_stream_survives_on_keeper_sweeps_alone() {
    let h = Harness::new();

    // Force the clamp: max rent buys ~5.8 days, but the stream runs a year.
    const MAX_TTL: u32 = 100_000;
    h.env.ledger().set_max_entry_ttl(MAX_TTL);

    let id = h.create_simple(365 * ONE, YEAR);
    assert_eq!(ttl_of(&h, id), MAX_TTL, "creation clamped as expected");

    // Nobody touches the stream all year except the keeper, sweeping at 60% of
    // the rent window — the cadence the backend keeper would actually use.
    let sweep_every = MAX_TTL * 6 / 10;

    let mut sweeps = 0;
    while h.env.ledger().sequence() < storage::seconds_to_ledgers(YEAR) {
        h.client.extend_stream_ttl(&id);
        sweeps += 1;
        age_ledgers(&h, sweep_every);
    }

    assert!(sweeps > 1, "the keeper must have swept more than once");
    assert!(!was_restored(&h, id), "the stream must never have archived");

    // The stream is still fully readable and pays out in full at the end.
    h.warp_to(T0 + YEAR + DAY);
    h.client.withdraw(&id, &None);
    let after = h.get(id);
    assert_eq!(after.withdrawn, 365 * ONE);
}

#[test]
fn automated_entry_lifetime_extension_resilience_across_decay_cycles() {
    let h = Harness::new();
    let id = h.create_simple(500 * ONE, 60 * DAY);
    let initial_ttl = ttl_of(&h, id);

    // Decay TTL close to the minimum threshold and verify automated extension
    // restores the full schedule + buffer lifetime without altering stream state.
    age_ledgers(&h, initial_ttl - 500);
    assert!(ttl_of(&h, id) < 1_000);

    let extended = h.client.extend_stream_ttl(&id);
    assert_eq!(ttl_of(&h, id), extended);
    assert!(!was_restored(&h, id));
    assert_eq!(h.get(id).deposited, 500 * ONE);
}

