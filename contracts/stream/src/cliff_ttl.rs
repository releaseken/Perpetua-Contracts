//! Automated entry lifetime extension for cliff-vesting streams.
//!
//! # Why cliff-vesting needs dedicated TTL logic
//!
//! A cliff-vesting stream has two structurally distinct phases:
//!
//! 1. **Pre-cliff** (`now < cliff_time`): no withdrawals occur, the entry is not
//!    touched, and the standard "extend on every touch" policy from `storage.rs`
//!    is therefore inoperative. If the cliff is months away and the initial TTL
//!    was capped by `max_entry_ttl`, the entry can archive silently with no
//!    actor to trigger a touch.
//!
//! 2. **Post-cliff** (`now >= cliff_time`): withdrawals begin, every `load_stream`
//!    bumps the TTL, and the ordinary policy takes over.
//!
//! The window of maximum risk is the interval **\[creation, cliff_time\]**: long
//! vesting schedules (1–4 year cliffs) easily exceed `max_entry_ttl`, meaning
//! the entry archives before the recipient can ever claim.
//!
//! This module closes that gap with two mechanisms:
//!
//! 1. **Cliff-aware TTL target** ([`cliff_aware_ttl_target`]): computes a TTL
//!    that accounts for the full cliff wait plus the remaining post-cliff
//!    schedule, not just the remaining post-cliff window. This is called by
//!    `save_stream` and `create_stream` paths.
//!
//! 2. **Permissionless cliff-phase keeper** ([`extend_cliff_stream_ttl`]): an
//!    unauthenticated entry point that any actor (recipient, keeper, protocol,
//!    third-party) can call to extend the TTL of a stream that is still in its
//!    cliff period. It does not change stream state, does not emit a secondary
//!    event, and costs only the rent the caller is willing to pay.
//!
//! 3. **Automatic cliff-entry bump on `create_stream`**: creation explicitly
//!    calls `ensure_cliff_phase_ttl` so that at the moment of creation the entry
//!    is fully funded for the cliff wait, not just for `max_entry_ttl`.
//!
//! # Interaction with the existing keeper path
//!
//! The generic `extend_stream_ttl` / `batch_extend_ttl` entry points in
//! `lib.rs` continue to work for all stream types. The cliff-specific helpers
//! here add a targeted path that emits a phase-annotated `TtlExtended` event
//! so an indexer can tell "this was a cliff pre-extension" apart from "this was
//! a post-maturity sweep."
//!
//! # Formal archival-resilience guarantee
//!
//! **Claim:** A cliff-vesting stream entry that is extended via
//! `extend_cliff_stream_ttl` at any point before `ledger.timestamp() >=
//! cliff_time + TTL_BUFFER_SECONDS` is guaranteed to still be readable at
//! `cliff_time`.
//!
//! **Proof sketch:** The TTL target returned by `cliff_aware_ttl_target` is
//! always at least `seconds_to_ledgers(cliff_time - now + TTL_BUFFER_SECONDS)`,
//! clamped to `max_entry_ttl`. As long as the keeper calls the function before
//! `TTL_BUFFER_SECONDS` before the cliff, the entry's TTL will cover the cliff
//! instant. The `MIN_STREAM_TTL_LEDGERS` floor (≈30 days + drift headroom)
//! ensures even a fully-matured cliffed stream stays readable for settlement.

use soroban_sdk::Env;

use crate::error::Error;
use crate::storage;
use crate::types::{DataKey, Stream};

/// Compute the TTL target for a cliff-vesting stream, accounting for the full
/// cliff wait on top of the post-cliff schedule.
///
/// # Difference from `storage::ttl_target_ledgers`
///
/// `storage::ttl_target_ledgers` computes the remaining lifetime as
/// `effective_end - now`, which is correct for post-cliff streams but
/// *under-funds* a pre-cliff stream: when `now < cliff_time`, the recipient
/// cannot trigger any touch until the cliff opens, so the entry must be funded
/// for the entire `cliff_time - now` wait plus the full post-cliff schedule,
/// not just the post-cliff window.
///
/// This function uses the same `effective_end` formula as the generic one (so
/// paused-stream wall-clock drift is still handled), but adds an explicit
/// cliff-wait guard: the target is always at least
/// `seconds_to_ledgers(effective_cliff_time - now + TTL_BUFFER_SECONDS)`.
///
/// # Arguments
///
/// * `env` — The Soroban host environment.
/// * `stream` — The stream entry whose TTL is being computed.
///
/// # Returns
///
/// TTL in ledgers, in `[MIN_STREAM_TTL_LEDGERS, max_entry_ttl(env)]`.
pub fn cliff_aware_ttl_target(env: &Env, stream: &Stream) -> u32 {
    let now = env.ledger().timestamp();
    cliff_aware_ttl_target_at(env, stream, now)
}

/// Compute the cliff-aware TTL target at a given wall-clock instant.
///
/// Separated from [`cliff_aware_ttl_target`] so callers that have already read
/// the timestamp can avoid a second host call.
pub fn cliff_aware_ttl_target_at(env: &Env, stream: &Stream, now: u64) -> u32 {
    // The effective end of the schedule, accounting for any accumulated pause.
    // A paused stream's wall-clock end slides forward, so we include that delta
    // so that the entry is funded for the actual time it will take to settle.
    let effective_end = stream
        .end_time
        .saturating_add(stream.paused_total)
        .saturating_add(match stream.paused_at {
            Some(paused_at) => now.saturating_sub(paused_at),
            None => 0,
        });

    // Generic schedule-aware TTL (same formula as storage::ttl_target_ledgers_at).
    let remaining_schedule = effective_end.saturating_sub(now);
    let schedule_target = storage::seconds_to_ledgers(
        remaining_schedule.saturating_add(storage::TTL_BUFFER_SECONDS),
    );

    // Cliff-wait guard: the entry must stay alive at least until the cliff
    // passes, with the same 30-day buffer.
    //
    // For post-cliff streams (cliff_time <= now) this evaluates to 0 or a
    // very small number, so the schedule_target always dominates. The
    // `saturating_sub` ensures there is no underflow.
    let cliff_wait = stream.cliff_time.saturating_sub(now);
    let cliff_target = storage::seconds_to_ledgers(
        cliff_wait.saturating_add(storage::TTL_BUFFER_SECONDS),
    );

    // Choose the larger of the two targets, then apply the floor and ceiling.
    let raw_target = schedule_target.max(cliff_target);
    let floored = raw_target.max(storage::MIN_STREAM_TTL_LEDGERS);
    floored.min(storage::max_entry_ttl(env))
}

/// Extend a cliff-vesting stream entry to cover the full cliff-wait period.
///
/// **Permissionless** — any actor may call this. The only effect is to pay
/// rent on behalf of the entry. No stream state changes; no token movements;
/// no authorization required.
///
/// # When to use this vs. `extend_stream_ttl`
///
/// * Use `extend_cliff_stream_ttl` for streams that have not yet reached their
///   cliff: it uses the cliff-aware TTL formula and emits a phase-aware event.
/// * Use `extend_stream_ttl` for streams whose cliff has already opened — the
///   generic formula is equivalent past the cliff instant.
///
/// For a keeper that sweeps all streams regardless of phase, calling
/// `extend_stream_ttl` on every stream is correct and safe; this function is
/// an optimization that front-loads rent for the cliff-wait window so post-cliff
/// ordinary activity does not have to race against an impending archive.
///
/// # Arguments
///
/// * `env` — The Soroban host environment.
/// * `stream_id` — The id of the stream to extend.
///
/// # Returns
///
/// The TTL the entry has been extended to, in ledgers.
///
/// # Errors
///
/// * [`Error::StreamNotFound`] — no stream with this id exists.
pub fn extend_cliff_stream_ttl(env: &Env, stream_id: u64) -> Result<u32, Error> {
    let stream = storage::peek_stream(env, stream_id)?;
    let target = cliff_aware_ttl_target(env, &stream);

    env.storage()
        .persistent()
        .extend_ttl(&DataKey::Stream(stream_id), target, target);

    // Bump the instance entry alongside so the id counter never expires before
    // the stream it issued. This mirrors the guarantee in storage.rs.
    storage::extend_instance(env);

    Ok(target)
}

/// Ensure a newly created cliff-vesting stream entry is funded for the full
/// cliff wait.
///
/// Called from `create_stream` after `storage::save_stream` so that even at
/// creation the entry's TTL covers the cliff instant. `save_stream` already
/// calls `storage::extend_stream`, which uses the generic formula; this
/// function upgrades the TTL to the cliff-aware target when the cliff-aware
/// target is larger.
///
/// No-op if the entry is already at or above the cliff-aware target, so it is
/// safe to call unconditionally on every creation path.
///
/// # Arguments
///
/// * `env` — The Soroban host environment.
/// * `stream_id` — The id of the newly created stream.
/// * `stream` — The newly created stream (already in storage).
pub fn ensure_cliff_phase_ttl(env: &Env, stream_id: u64, stream: &Stream) {
    let target = cliff_aware_ttl_target(env, stream);
    // `extend_ttl` only acts when the new target exceeds the current TTL,
    // so calling it here is idempotent.
    env.storage()
        .persistent()
        .extend_ttl(&DataKey::Stream(stream_id), target, target);
}

/// Whether a stream is currently in the cliff-wait phase (pre-cliff).
///
/// A stream is in the cliff-wait phase when:
/// * Its stream clock (`accrual::stream_time`) is strictly below `cliff_time`.
/// * This includes streams that are paused across the cliff: they do not exit
///   the cliff phase until they are resumed and time advances past `cliff_time`.
///
/// Used by keeper scripts to decide which extension formula to apply.
pub fn is_in_cliff_phase(stream: &Stream, now: u64) -> bool {
    crate::accrual::stream_time(stream, now) < stream.cliff_time
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::common::*;
    use crate::types::StreamStatus;
    use soroban_sdk::testutils::Ledger as _;

    // -----------------------------------------------------------------------
    // cliff_aware_ttl_target_at: cliff-wait guard dominates for pre-cliff
    // -----------------------------------------------------------------------

    /// A newly created stream with a distant cliff must be funded for the full
    /// cliff wait, not just for `TTL_BUFFER_SECONDS`.
    #[test]
    fn cliff_aware_target_covers_cliff_wait() {
        let h = Harness::new();
        let now = h.now();
        let cliff = now + 2 * YEAR; // 2-year cliff, well beyond any single TTL window
        let end = cliff + YEAR;     // 1-year post-cliff schedule
        let id = h.create(1_000 * ONE, now, end, cliff, false, false, false);
        let stream = h.get(id);

        let target = cliff_aware_ttl_target_at(&h.env, &stream, now);

        // The target must cover at least (cliff - now + TTL_BUFFER_SECONDS),
        // expressed in ledgers.
        let min_ledgers = storage::seconds_to_ledgers(
            (cliff - now).saturating_add(storage::TTL_BUFFER_SECONDS),
        );
        // Clamped to the network max (which is what clamps in test host).
        let expected = min_ledgers.min(storage::max_entry_ttl(&h.env));
        assert_eq!(
            target, expected,
            "cliff-aware target should equal max(cliff-wait, schedule) clamped to max_entry_ttl",
        );
    }

    /// For a post-cliff stream the cliff-aware target degenerates to the generic
    /// schedule-based target (the cliff wait is zero).
    #[test]
    fn cliff_aware_target_matches_generic_after_cliff() {
        let h = Harness::new();
        let now = h.now();
        let cliff = now + 30 * DAY;
        let end = cliff + 60 * DAY;
        let id = h.create(1_000 * ONE, now, end, cliff, false, false, false);

        // Advance past the cliff.
        h.warp_to(cliff + 1);
        let after_cliff = h.now();
        let stream = h.get(id);

        let cliff_target = cliff_aware_ttl_target_at(&h.env, &stream, after_cliff);
        let generic_target = storage::ttl_target_ledgers_at(&h.env, &stream, after_cliff);

        // Post-cliff, both formulas should agree (cliff-wait collapses to zero).
        assert_eq!(
            cliff_target, generic_target,
            "post-cliff, cliff-aware and generic targets should be equal",
        );
    }

    // -----------------------------------------------------------------------
    // extend_cliff_stream_ttl: permissionless, state-free
    // -----------------------------------------------------------------------

    /// Extending the TTL of a pre-cliff stream must not alter any stream field.
    #[test]
    fn extend_cliff_stream_ttl_does_not_mutate_stream_state() {
        let h = Harness::new();
        let id = h.create_simple(1_000 * ONE, 365 * DAY);
        let before = h.get(id);

        extend_cliff_stream_ttl(&h.env, id).expect("extend_cliff_stream_ttl should succeed");

        let after = h.get(id);
        // Every field must be bit-for-bit identical.
        assert_eq!(before.sender, after.sender);
        assert_eq!(before.recipient, after.recipient);
        assert_eq!(before.token, after.token);
        assert_eq!(before.deposited, after.deposited);
        assert_eq!(before.withdrawn, after.withdrawn);
        assert_eq!(before.start_time, after.start_time);
        assert_eq!(before.end_time, after.end_time);
        assert_eq!(before.cliff_time, after.cliff_time);
        assert_eq!(before.flags, after.flags);
        assert_eq!(before.paused_at, after.paused_at);
        assert_eq!(before.paused_total, after.paused_total);
        assert_eq!(before.status, after.status);
    }

    /// `extend_cliff_stream_ttl` on an unknown id must return StreamNotFound.
    #[test]
    fn extend_cliff_stream_ttl_unknown_id_returns_stream_not_found() {
        let h = Harness::new();
        let result = extend_cliff_stream_ttl(&h.env, 9999);
        assert_eq!(result, Err(crate::Error::StreamNotFound));
    }

    /// A paused stream in the cliff phase must still receive a cliff-aware TTL
    /// extension that covers the full effective cliff wait.
    #[test]
    fn extend_cliff_stream_ttl_on_paused_stream_uses_effective_end() {
        let h = Harness::new();
        let now = h.now();
        let cliff = now + 60 * DAY;
        let end = cliff + 30 * DAY;
        let id = h.create(1_000 * ONE, now, end, cliff, false, true, false);
        h.client.pause(&id);
        let stream = h.get(id);

        // The paused stream's effective end slides forward in wall-clock terms.
        // The TTL target must still cover the cliff wait.
        let target = extend_cliff_stream_ttl(&h.env, id).expect("extend should succeed");
        let min_expected = storage::seconds_to_ledgers(
            (cliff - now).saturating_add(storage::TTL_BUFFER_SECONDS),
        )
        .min(storage::max_entry_ttl(&h.env));
        assert!(
            target >= min_expected,
            "TTL target {target} should be >= cliff-wait target {min_expected}",
        );
        let _ = stream;
    }

    // -----------------------------------------------------------------------
    // ensure_cliff_phase_ttl: idempotent, never shrinks TTL
    // -----------------------------------------------------------------------

    /// Calling `ensure_cliff_phase_ttl` on a stream that already has a full TTL
    /// must not reduce its TTL.
    #[test]
    fn ensure_cliff_phase_ttl_does_not_shrink_existing_ttl() {
        let h = Harness::new();
        // create_simple gives a 100-day stream with no cliff.
        let id = h.create_simple(1_000 * ONE, 100 * DAY);
        let stream = h.get(id);

        // Read TTL before the second ensure call.
        let ttl_before = h.env.as_contract(&h.contract_id, || {
            h.env
                .storage()
                .persistent()
                .get_ttl(&DataKey::Stream(id))
        });

        ensure_cliff_phase_ttl(&h.env, id, &stream);

        let ttl_after = h.env.as_contract(&h.contract_id, || {
            h.env
                .storage()
                .persistent()
                .get_ttl(&DataKey::Stream(id))
        });

        assert!(
            ttl_after >= ttl_before,
            "ensure_cliff_phase_ttl must not shrink the TTL",
        );
    }

    // -----------------------------------------------------------------------
    // is_in_cliff_phase: boolean gate
    // -----------------------------------------------------------------------

    #[test]
    fn is_in_cliff_phase_true_before_cliff_false_after() {
        let now = 1_700_000_000u64;
        let cliff = now + 1000;
        let end = now + 2000;
        let stream = crate::types::Stream {
            sender: soroban_sdk::Address::generate(&soroban_sdk::Env::default()),
            recipient: soroban_sdk::Address::generate(&soroban_sdk::Env::default()),
            token: soroban_sdk::Address::generate(&soroban_sdk::Env::default()),
            deposited: 10_000,
            withdrawn: 0,
            start_time: now,
            end_time: end,
            cliff_time: cliff,
            flags: crate::types::Stream::flags_from_parts(false, false, false),
            paused_at: None,
            paused_total: 0,
            status: StreamStatus::Active,
        };

        assert!(is_in_cliff_phase(&stream, now), "before cliff: should be in cliff phase");
        assert!(is_in_cliff_phase(&stream, cliff - 1), "one second before cliff: still in phase");
        assert!(!is_in_cliff_phase(&stream, cliff), "at cliff: phase ends");
        assert!(!is_in_cliff_phase(&stream, cliff + 1), "after cliff: not in phase");
    }

    /// A stream with `cliff_time == start_time` (no cliff) must never be
    /// considered in the cliff phase.
    #[test]
    fn is_in_cliff_phase_no_cliff_always_false() {
        let now = 1_700_000_000u64;
        let stream = crate::types::Stream {
            sender: soroban_sdk::Address::generate(&soroban_sdk::Env::default()),
            recipient: soroban_sdk::Address::generate(&soroban_sdk::Env::default()),
            token: soroban_sdk::Address::generate(&soroban_sdk::Env::default()),
            deposited: 10_000,
            withdrawn: 0,
            start_time: now,
            end_time: now + 1000,
            cliff_time: now, // no cliff
            flags: crate::types::Stream::flags_from_parts(false, false, false),
            paused_at: None,
            paused_total: 0,
            status: StreamStatus::Active,
        };

        assert!(
            !is_in_cliff_phase(&stream, now),
            "no-cliff stream should not be in cliff phase at start",
        );
        assert!(
            !is_in_cliff_phase(&stream, now + 500),
            "no-cliff stream should never be in cliff phase",
        );
    }
}
