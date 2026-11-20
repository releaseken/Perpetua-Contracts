#![no_std]
//! # Perpetua pause-clock
//!
//! A lightweight registry that tracks the global pause state and per-stream
//! cumulative paused durations for the Perpetua streaming system.
//!
//! ## Why this contract exists
//!
//! During a high-frequency batch claim, the stream contract must know how long
//! each stream has been paused (globally and per-stream) to compute the
//! correct withdrawable balance.  Without a dedicated registry, each batch
//! item would trigger a separate cross-contract read of that stream's state —
//! O(N) round-trips at the cross-contract overhead rate.  The pause-clock
//! collapses that to **one** cross-contract call per batch by caching the
//! global pause state and exposing a batch-read entry point for per-stream
//! totals.  Batch callers amortize the invocation overhead across the entire
//! batch rather than paying it per item.  (#400)
//!
//! ## Automated archival resilience (#399)
//!
//! Persistent entries (`PausedTotal(stream_id)`) are extended on every read and
//! write.  A permissionless `extend_stream_ttl` entry point lets any keeper top
//! up a specific entry.  `batch_extend_ttl` lets a keeper sweep several entries
//! in one transaction.  The contract never lets an entry silently expire and
//! lose accumulated pause history.
//!
//! ## Authorization model
//!
//! | operation | who may call |
//! |-----------|-------------|
//! | `init` | admin (signs the call) |
//! | `record_pause`, `record_resume` | stream contract only |
//! | `settle_stream_total` | stream contract only |
//! | `batch_settle` | stream contract only |
//! | `extend_stream_ttl`, `batch_extend_ttl` | permissionless |
//! | `get_config`, `get_stream_total`, `batch_get_totals` | permissionless |
//! | admin setters, `propose_admin`, `accept_admin` | admin |
//!
//! ## Admin rotation
//!
//! Two-step hand-off via `propose_admin` / `accept_admin`, identical to the
//! factory contract pattern.  Single-step `set_admin` also exists and clears
//! any stale pending nomination.

#[cfg(all(target_family = "wasm", not(target_os = "none")))]
compile_error!(
    "Perpetua production WASM must be built for wasm32v1-none; \
     wasm32-unknown-unknown can emit unsupported features."
);
#[cfg(all(target_family = "wasm", debug_assertions))]
compile_error!(
    "Perpetua production WASM must be built without debug assertions; use --release."
);
#[cfg(all(target_family = "wasm", feature = "testutils"))]
compile_error!("Perpetua production WASM must not enable the testutils feature.");

#[cfg(test)]
extern crate std;

mod error;
mod events;
mod storage;
mod types;

pub use error::Error;
pub use types::{ClockConfig, DataKey};

use soroban_sdk::{contract, contractimpl, Address, Env, Vec};

/// Maximum number of stream IDs a single batch call may process.
///
/// Bounded by the cross-contract event budget and instruction ceiling.
/// Each batch item performs one persistent read/write pair; at 32 items the
/// ledger-entry footprint remains well inside the protocol limit.
pub const MAX_BATCH_SIZE: u32 = 32;

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct PauseClock;

#[contractimpl]
impl PauseClock {
    // -----------------------------------------------------------------------
    // Initialization
    // -----------------------------------------------------------------------

    /// Initialize the pause-clock with an admin and the stream contract it
    /// serves.
    ///
    /// The admin is the only address that can update configuration.  The stream
    /// contract is the only address that can call `record_pause`, `record_resume`,
    /// `settle_stream_total`, and `batch_settle`.
    ///
    /// # Errors
    ///
    /// * [`Error::AlreadyInitialized`] — `init` has already been called.
    pub fn init(
        env: Env,
        admin: Address,
        stream_contract: Address,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Config) {
            return Err(Error::AlreadyInitialized);
        }
        admin.require_auth();

        let config = ClockConfig {
            admin,
            stream_contract,
            paused: false,
            paused_at: None,
            paused_total: 0,
            cycle_count: 0,
        };
        storage::save_config(&env, &config);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Global clock — called by the stream contract
    // -----------------------------------------------------------------------

    /// Record that the global stream clock has been paused.
    ///
    /// Only the registered stream contract may call this.  The call is a no-op
    /// if the clock is already paused (idempotent, no error) so that an
    /// in-flight retry after a ledger timeout does not revert.
    ///
    /// # Errors
    ///
    /// * [`Error::CallerNotStreamContract`] — caller is not the registered
    ///   stream contract.
    /// * [`Error::AlreadyPaused`] — the global clock is already paused.
    /// * [`Error::Overflow`] — `cycle_count` overflowed `u64`.
    pub fn record_pause(env: Env, caller: Address) -> Result<(), Error> {
        caller.require_auth();
        let mut config = storage::load_config(&env)?;
        if caller != config.stream_contract {
            return Err(Error::CallerNotStreamContract);
        }
        if config.paused {
            return Err(Error::AlreadyPaused);
        }

        let now = env.ledger().timestamp();
        config.paused = true;
        config.paused_at = Some(now);
        config.cycle_count = config
            .cycle_count
            .checked_add(1)
            .ok_or(Error::Overflow)?;
        storage::save_config(&env, &config);

        events::clock_paused(&env, now, config.cycle_count);
        Ok(())
    }

    /// Record that the global stream clock has been resumed.
    ///
    /// Absorbs the paused interval into `paused_total`.  Only the registered
    /// stream contract may call this.
    ///
    /// # Errors
    ///
    /// * [`Error::CallerNotStreamContract`] — caller is not the stream contract.
    /// * [`Error::NotPaused`] — the global clock is not currently paused.
    /// * [`Error::Overflow`] — the accumulated `paused_total` overflowed `u64`.
    pub fn record_resume(env: Env, caller: Address) -> Result<(), Error> {
        caller.require_auth();
        let mut config = storage::load_config(&env)?;
        if caller != config.stream_contract {
            return Err(Error::CallerNotStreamContract);
        }
        if !config.paused {
            return Err(Error::NotPaused);
        }

        let now = env.ledger().timestamp();
        let paused_at = config.paused_at.unwrap_or(now);
        let paused_duration = now.saturating_sub(paused_at);
        config.paused_total = config
            .paused_total
            .checked_add(paused_duration)
            .ok_or(Error::Overflow)?;
        config.paused = false;
        config.paused_at = None;
        storage::save_config(&env, &config);

        events::clock_resumed(&env, now, paused_duration, config.paused_total, config.cycle_count);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Per-stream totals — called by the stream contract
    //
    // Issue #400: these entry points eliminate per-item cross-contract calls
    // from the stream contract's batch-claim path.  The stream contract calls
    // `batch_settle` once per batch, passing all stream IDs together.  The
    // pause-clock updates all per-stream totals in a single invocation,
    // amortizing the cross-contract overhead across the batch.
    // -----------------------------------------------------------------------

    /// Record a finalized paused-total for a single stream.
    ///
    /// `total` is the caller-computed cumulative seconds that stream was paused.
    /// The pause-clock stores it and extends the entry's TTL.  Only the
    /// registered stream contract may call this.
    ///
    /// # Errors
    ///
    /// * [`Error::CallerNotStreamContract`]
    pub fn settle_stream_total(
        env: Env,
        caller: Address,
        stream_id: u64,
        total: u64,
    ) -> Result<(), Error> {
        caller.require_auth();
        let config = storage::load_config(&env)?;
        if caller != config.stream_contract {
            return Err(Error::CallerNotStreamContract);
        }

        // Write the finalized total and extend the entry's TTL.
        storage::save_stream_total(&env, stream_id, total);
        events::stream_pause_totaled(&env, stream_id, total);
        Ok(())
    }

    /// Batch-settle paused totals for multiple streams in a single invocation.
    ///
    /// This is the primary optimization for issue #400.  Instead of the stream
    /// contract making one cross-contract call per stream during a batch claim,
    /// it passes the full list of `(stream_id, paused_total)` pairs here.  The
    /// pause-clock processes them in a tight loop over already-loaded storage,
    /// paying the cross-contract overhead exactly **once** for the entire batch.
    ///
    /// `totals` is a parallel `Vec` where `totals[i]` is the finalized paused
    /// seconds for `stream_ids[i]`.  The two vecs must have equal length.
    ///
    /// # Errors
    ///
    /// * [`Error::CallerNotStreamContract`]
    /// * [`Error::EmptyBatch`] — no entries provided.
    /// * [`Error::BatchTooLarge`] — more than `MAX_BATCH_SIZE` entries.
    /// * [`Error::Overflow`] — the lengths of `stream_ids` and `totals` differ
    ///   (treated as an arithmetic inconsistency to avoid a separate error
    ///   discriminant).
    pub fn batch_settle(
        env: Env,
        caller: Address,
        stream_ids: Vec<u64>,
        totals: Vec<u64>,
    ) -> Result<(), Error> {
        caller.require_auth();
        let config = storage::load_config(&env)?;
        if caller != config.stream_contract {
            return Err(Error::CallerNotStreamContract);
        }

        let n = stream_ids.len();
        if n == 0 {
            return Err(Error::EmptyBatch);
        }
        if n > MAX_BATCH_SIZE {
            return Err(Error::BatchTooLarge);
        }
        // Parallel vecs must have matching length.
        if totals.len() != n {
            return Err(Error::Overflow);
        }

        // Tight loop: one persistent write per stream, no additional
        // cross-contract calls.  All TTL bumps happen inside save_stream_total.
        for i in 0..n {
            let stream_id = stream_ids.get_unchecked(i);
            let total = totals.get_unchecked(i);
            storage::save_stream_total(&env, stream_id, total);
            events::stream_pause_totaled(&env, stream_id, total);
        }

        // Bump instance once for the entire batch.
        storage::extend_instance(&env);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Archival resilience — permissionless keeper path (#399)
    // -----------------------------------------------------------------------

    /// Extend the TTL of a single stream's paused-total entry.
    ///
    /// Permissionless: anyone may call this to prevent a stream's pause-history
    /// entry from archiving.  Emits `TtlExtended` so keepers can confirm the
    /// sweep landed.
    ///
    /// # Errors
    ///
    /// * [`Error::StreamNotFound`] — no entry exists for `stream_id` (the entry
    ///   may have already archived; it must be restored before it can be
    ///   extended).
    pub fn extend_stream_ttl(env: Env, stream_id: u64) -> Result<(), Error> {
        if !storage::stream_total_exists(&env, stream_id) {
            return Err(Error::StreamNotFound);
        }
        storage::extend_stream_entry(&env, stream_id);
        // Also keep the instance entry alive.
        storage::extend_instance(&env);

        let target = storage::MIN_ENTRY_TTL_LEDGERS.min(env.storage().max_ttl());
        events::ttl_extended(&env, stream_id, target);
        Ok(())
    }

    /// Extend the TTL of multiple stream entries in one sweep.
    ///
    /// Unknown stream IDs are silently skipped (the entry may have never been
    /// written or may have already archived).  Returns the number of entries
    /// actually extended.
    ///
    /// # Errors
    ///
    /// * [`Error::EmptyBatch`] — no IDs provided.
    /// * [`Error::BatchTooLarge`] — more than `MAX_BATCH_SIZE` IDs.
    pub fn batch_extend_ttl(env: Env, stream_ids: Vec<u64>) -> Result<u32, Error> {
        let n = stream_ids.len();
        if n == 0 {
            return Err(Error::EmptyBatch);
        }
        if n > MAX_BATCH_SIZE {
            return Err(Error::BatchTooLarge);
        }

        let target = storage::MIN_ENTRY_TTL_LEDGERS.min(env.storage().max_ttl());
        let mut extended: u32 = 0;
        for stream_id in stream_ids.iter() {
            if storage::stream_total_exists(&env, stream_id) {
                storage::extend_stream_entry(&env, stream_id);
                events::ttl_extended(&env, stream_id, target);
                extended = extended.saturating_add(1);
            }
        }
        storage::extend_instance(&env);
        Ok(extended)
    }

    // -----------------------------------------------------------------------
    // Views
    // -----------------------------------------------------------------------

    /// Return the full configuration record.
    ///
    /// # Errors
    ///
    /// * [`Error::NotInitialized`]
    pub fn get_config(env: Env) -> Result<ClockConfig, Error> {
        storage::load_config(&env)
    }

    /// Return the cumulative paused seconds recorded for `stream_id`.
    ///
    /// Returns `0` for a stream with no recorded history (never paused or
    /// entry not yet written).
    pub fn get_stream_total(env: Env, stream_id: u64) -> u64 {
        storage::peek_stream_total(&env, stream_id)
    }

    /// Return cumulative paused totals for a batch of stream IDs.
    ///
    /// This is the read counterpart to `batch_settle`.  A batch-claim caller
    /// calls this **once** to fetch all totals, then uses them locally to
    /// compute withdrawable balances — eliminating one cross-contract read per
    /// stream and collapsing N reads into one invocation.  (#400)
    ///
    /// Unknown stream IDs return `0` in the corresponding position.
    ///
    /// # Errors
    ///
    /// * [`Error::EmptyBatch`]
    /// * [`Error::BatchTooLarge`]
    pub fn batch_get_totals(env: Env, stream_ids: Vec<u64>) -> Result<Vec<u64>, Error> {
        let n = stream_ids.len();
        if n == 0 {
            return Err(Error::EmptyBatch);
        }
        if n > MAX_BATCH_SIZE {
            return Err(Error::BatchTooLarge);
        }

        let mut out = Vec::new(&env);
        for stream_id in stream_ids.iter() {
            out.push_back(storage::peek_stream_total(&env, stream_id));
        }
        Ok(out)
    }

    /// Return the currently pending admin nominee, if any two-step rotation is
    /// in progress.
    ///
    /// # Errors
    ///
    /// * [`Error::NotInitialized`]
    pub fn pending_admin(env: Env) -> Result<Option<Address>, Error> {
        storage::load_config(&env)?;
        Ok(env.storage().instance().get(&DataKey::PendingAdmin))
    }

    // -----------------------------------------------------------------------
    // Admin setters
    // -----------------------------------------------------------------------

    fn require_admin(env: &Env) -> Result<ClockConfig, Error> {
        let config = storage::load_config(env)?;
        config.admin.clone().require_auth();
        Ok(config)
    }

    /// Rotate the admin address (single-step).
    ///
    /// Also clears any stale two-step proposal to prevent a superseded nominee
    /// from later completing a hand-off.
    ///
    /// # Errors
    ///
    /// * [`Error::Unauthorized`] — caller is not the current admin.
    /// * [`Error::InvalidNewAdmin`] — `new_admin` is the zero address or the
    ///   contract itself.
    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        let mut config = Self::require_admin(&env)?;
        if storage::is_forbidden_admin(&env, &new_admin) {
            return Err(Error::InvalidNewAdmin);
        }
        config.admin = new_admin;
        storage::save_config(&env, &config);
        // Clear any stale pending nomination.
        env.storage().instance().remove(&DataKey::PendingAdmin);
        storage::extend_instance(&env);
        Ok(())
    }

    /// Nominate a successor admin for a two-step hand-off.
    ///
    /// The nominee takes over only after they call `accept_admin`.
    ///
    /// # Errors
    ///
    /// * [`Error::Unauthorized`]
    /// * [`Error::InvalidNewAdmin`]
    pub fn propose_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        let config = Self::require_admin(&env)?;
        if storage::is_forbidden_admin(&env, &new_admin) {
            return Err(Error::InvalidNewAdmin);
        }
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);
        storage::extend_instance(&env);

        // CEI: state written before event emitted.
        events::AdminTransferProposed {
            current: config.admin,
            pending: new_admin,
        }
        .publish(&env);
        Ok(())
    }

    /// Accept a pending admin nomination and complete the rotation.
    ///
    /// The caller must be the address stored by `propose_admin`.
    ///
    /// # Errors
    ///
    /// * [`Error::NoPendingAdmin`] — no rotation is pending.
    pub fn accept_admin(env: Env) -> Result<(), Error> {
        let pending: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .ok_or(Error::NoPendingAdmin)?;
        pending.require_auth();

        let mut config = storage::load_config(&env)?;
        let previous = config.admin.clone();
        config.admin = pending;
        env.storage().instance().remove(&DataKey::PendingAdmin);
        storage::save_config(&env, &config);

        // CEI: state written before event emitted.
        events::AdminTransferred {
            previous,
            new: config.admin,
        }
        .publish(&env);
        Ok(())
    }

    /// Update the stream contract address this clock trusts.
    ///
    /// # Errors
    ///
    /// * [`Error::Unauthorized`]
    pub fn set_stream_contract(env: Env, stream_contract: Address) -> Result<(), Error> {
        let mut config = Self::require_admin(&env)?;
        config.stream_contract = stream_contract;
        storage::save_config(&env, &config);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod test {
    mod common;
    mod archival;
    mod batch;
    mod auth;
    mod admin_rotation;
}
