#![no_std]
//! # Perpetua event-sink
//!
//! A structured, permissioned audit-log sink for the Perpetua protocol.
//!
//! ## Purpose
//!
//! Protocol contracts (stream, cliff-vesting, pause-clock, …) call `emit` to
//! deposit typed [`SinkEntry`] records here.  The sink validates each entry,
//! assigns a globally-monotonic `entry_id`, and emits a structured
//! [`events::EntryAccepted`] ledger event.  Client tooling (indexers, SDKs,
//! dashboards) subscribes to this single contract instead of monitoring every
//! protocol contract individually.
//!
//! ## Issue #461 — modular error types with custom messages
//!
//! All failure modes are expressed as variants of [`SinkError`], organised
//! into four semantic groups (Initialization, Authorization, Payload
//! validation, Arithmetic/limits).  Each variant carries a `const fn message()`
//! so TypeScript/Python SDKs that decode a `u32` discriminant from the ledger
//! can surface the canonical human-readable description without embedding Rust
//! docs.  A `group()` helper allows coarse client-side branching on error
//! category.
//!
//! ## Authorization model
//!
//! | operation | who may call |
//! |-----------|-------------|
//! | `init` | admin (signs the call) |
//! | `emit`, `batch_emit` | registered emitter contracts only |
//! | `register_emitter`, `deregister_emitter` | admin |
//! | `set_accepting` | admin |
//! | `set_admin`, `propose_admin`, `accept_admin` | admin |
//! | `get_config`, `entry_count` | permissionless |
//!
//! ## Footprint
//!
//! Entries are **not stored on-chain**; they are emitted as ledger events only.
//! The on-chain footprint is constant: one instance entry (`Config` +
//! `EntryCount` + optional `PendingAdmin`) plus one persistent entry per
//! registered emitter.

#[cfg(all(target_family = "wasm", not(target_os = "none")))]
compile_error!(
    "Perpetua production WASM must be built for wasm32v1-none; \
     wasm32-unknown-unknown can emit unsupported features."
);
#[cfg(all(target_family = "wasm", debug_assertions))]
compile_error!("Perpetua production WASM must be built without debug assertions; use --release.");
#[cfg(all(target_family = "wasm", feature = "testutils"))]
compile_error!("Perpetua production WASM must not enable the testutils feature.");

#[cfg(test)]
extern crate std;

mod error;
mod events;
mod storage;
mod types;

pub use error::SinkError;
pub use types::{DataKey, SinkConfig, SinkEntry};

use soroban_sdk::{contract, contractimpl, Address, Env, Symbol, Vec};

/// Maximum byte length of a `SinkEntry` payload.
///
/// Bounded by the ledger event data budget and the instruction cost of
/// serialising large byte strings.  At 512 bytes, a payload comfortably holds
/// any reasonable structured log record while staying well inside protocol
/// limits.
pub const MAX_PAYLOAD_BYTES: u32 = 512;

/// Maximum number of entries a single `batch_emit` call may submit.
pub const MAX_BATCH_SIZE: u32 = 16;

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct EventSink;

#[contractimpl]
impl EventSink {
    // -----------------------------------------------------------------------
    // Initialization
    // -----------------------------------------------------------------------

    /// Initialize the event-sink with an admin address.
    ///
    /// The sink starts with `accepting = true` so the first registered emitter
    /// can call `emit` immediately after registration.
    ///
    /// # Errors
    ///
    /// * [`SinkError::AlreadyInitialized`] — `init` has already been called.
    pub fn init(env: Env, admin: Address) -> Result<(), SinkError> {
        if env.storage().instance().has(&DataKey::Config) {
            return Err(SinkError::AlreadyInitialized);
        }
        admin.require_auth();
        let config = SinkConfig {
            admin,
            accepting: true,
        };
        storage::save_config(&env, &config);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Emit — registered emitters only
    // -----------------------------------------------------------------------

    /// Validate and accept a single structured audit-log entry.
    ///
    /// Returns the globally-monotonic `entry_id` assigned to this entry.
    ///
    /// # Validation (issue #461 — custom error messages per failure mode)
    ///
    /// | Check | Error on failure |
    /// |-------|-----------------|
    /// | Sink is `accepting` | [`SinkError::Unauthorized`] |
    /// | Caller is a registered emitter | [`SinkError::CallerNotRegisteredEmitter`] |
    /// | `entry.topic` is non-empty | [`SinkError::EmptyTopic`] |
    /// | `entry.source_contract` is non-zero | [`SinkError::InvalidSourceContract`] |
    /// | `entry.sequence > 0` | [`SinkError::InvalidSequence`] |
    /// | `entry.payload.len() ≤ MAX_PAYLOAD_BYTES` | [`SinkError::PayloadTooLarge`] |
    ///
    /// # Errors
    ///
    /// * Any of the validation errors listed above.
    /// * [`SinkError::EntryCountOverflow`] — the entry counter reached `u64::MAX`.
    pub fn emit(env: Env, caller: Address, entry: SinkEntry) -> Result<u64, SinkError> {
        caller.require_auth();
        let config = storage::load_config(&env)?;

        if !config.accepting {
            return Err(SinkError::Unauthorized);
        }
        if !storage::is_registered_emitter(&env, &caller) {
            return Err(SinkError::CallerNotRegisteredEmitter);
        }

        Self::validate_entry(&env, &entry)?;

        // CEI: allocate ID and write counter before emitting the event.
        let entry_id = storage::allocate_entry_id(&env)?;
        events::entry_accepted(
            &env,
            entry_id,
            entry.source_contract,
            entry.topic,
            entry.sequence,
            entry.payload,
        );
        Ok(entry_id)
    }

    /// Validate and accept multiple entries in a single invocation.
    ///
    /// All entries are validated before any IDs are allocated; a single
    /// invalid entry reverts the entire batch.  Returns the first `entry_id`
    /// assigned; subsequent IDs are contiguous (`first_id + 1`, `first_id + 2`,
    /// …).
    ///
    /// # Errors
    ///
    /// * [`SinkError::EmptyBatch`], [`SinkError::BatchTooLarge`]
    /// * Any per-entry validation error (checked before execution).
    /// * [`SinkError::Unauthorized`] — sink is not accepting.
    /// * [`SinkError::CallerNotRegisteredEmitter`] — unknown caller.
    /// * [`SinkError::EntryCountOverflow`]
    pub fn batch_emit(
        env: Env,
        caller: Address,
        entries: Vec<SinkEntry>,
    ) -> Result<u64, SinkError> {
        caller.require_auth();
        let config = storage::load_config(&env)?;

        if !config.accepting {
            return Err(SinkError::Unauthorized);
        }
        if !storage::is_registered_emitter(&env, &caller) {
            return Err(SinkError::CallerNotRegisteredEmitter);
        }

        let n = entries.len();
        if n == 0 {
            return Err(SinkError::EmptyBatch);
        }
        if n > MAX_BATCH_SIZE {
            return Err(SinkError::BatchTooLarge);
        }

        // Pre-flight: validate every entry before mutating state.
        for i in 0..n {
            let entry = entries.get_unchecked(i);
            Self::validate_entry(&env, &entry)?;
        }

        // Execution: allocate IDs and emit events.
        let first_id = storage::allocate_entry_id(&env)?;
        events::entry_accepted(
            &env,
            first_id,
            entries.get_unchecked(0).source_contract,
            entries.get_unchecked(0).topic,
            entries.get_unchecked(0).sequence,
            entries.get_unchecked(0).payload,
        );
        for i in 1..n {
            let entry_id = storage::allocate_entry_id(&env)?;
            let entry = entries.get_unchecked(i);
            events::entry_accepted(
                &env,
                entry_id,
                entry.source_contract,
                entry.topic,
                entry.sequence,
                entry.payload,
            );
        }

        Ok(first_id)
    }

    // -----------------------------------------------------------------------
    // Emitter management — admin only
    // -----------------------------------------------------------------------

    /// Add `emitter` to the allow-list so it may call `emit`.
    ///
    /// # Errors
    ///
    /// * [`SinkError::Unauthorized`] — caller is not the admin.
    pub fn register_emitter(env: Env, emitter: Address) -> Result<(), SinkError> {
        let config = Self::require_admin(&env)?;
        let _ = config;
        storage::register_emitter(&env, &emitter);
        events::emitter_registered(&env, emitter);
        Ok(())
    }

    /// Remove `emitter` from the allow-list.
    ///
    /// A no-op if the emitter was never registered.
    ///
    /// # Errors
    ///
    /// * [`SinkError::Unauthorized`] — caller is not the admin.
    pub fn deregister_emitter(env: Env, emitter: Address) -> Result<(), SinkError> {
        let config = Self::require_admin(&env)?;
        let _ = config;
        storage::deregister_emitter(&env, &emitter);
        events::emitter_deregistered(&env, emitter);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Sink state — admin only
    // -----------------------------------------------------------------------

    /// Enable or disable the acceptance of new entries.
    ///
    /// When `accepting = false`, all `emit` / `batch_emit` calls return
    /// [`SinkError::Unauthorized`], allowing the admin to pause the sink for
    /// maintenance without de-registering emitters.
    ///
    /// # Errors
    ///
    /// * [`SinkError::Unauthorized`] — caller is not the admin.
    pub fn set_accepting(env: Env, accepting: bool) -> Result<(), SinkError> {
        let mut config = Self::require_admin(&env)?;
        config.accepting = accepting;
        storage::save_config(&env, &config);
        events::sink_toggled(&env, accepting);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Views — permissionless
    // -----------------------------------------------------------------------

    /// Return the full configuration record.
    ///
    /// # Errors
    ///
    /// * [`SinkError::NotInitialized`]
    pub fn get_config(env: Env) -> Result<SinkConfig, SinkError> {
        storage::load_config(&env)
    }

    /// Return the total number of entries accepted so far.
    pub fn entry_count(env: Env) -> u64 {
        storage::entry_count(&env)
    }

    /// Return the canonical error message for a numeric discriminant.
    ///
    /// This is a view-only helper for client SDKs that decode a `u32` error
    /// code from the ledger and want to surface the description without
    /// embedding Rust docs.  Returns an empty string for unknown discriminants.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let msg = client.error_message(&4u32); // "caller is not a registered emitter"
    /// ```
    pub fn error_message(env: Env, discriminant: u32) -> soroban_sdk::String {
        let msg = match discriminant {
            1 => SinkError::NotInitialized.message(),
            2 => SinkError::AlreadyInitialized.message(),
            3 => SinkError::Unauthorized.message(),
            4 => SinkError::CallerNotRegisteredEmitter.message(),
            5 => SinkError::NoPendingAdmin.message(),
            6 => SinkError::InvalidNewAdmin.message(),
            7 => SinkError::EmptyTopic.message(),
            8 => SinkError::InvalidSourceContract.message(),
            9 => SinkError::InvalidSequence.message(),
            10 => SinkError::PayloadTooLarge.message(),
            11 => SinkError::EntryCountOverflow.message(),
            12 => SinkError::EmptyBatch.message(),
            13 => SinkError::BatchTooLarge.message(),
            _ => "",
        };
        soroban_sdk::String::from_str(&env, msg)
    }

    /// Return the pending admin nominee, if any.
    ///
    /// # Errors
    ///
    /// * [`SinkError::NotInitialized`]
    pub fn pending_admin(env: Env) -> Result<Option<Address>, SinkError> {
        storage::load_config(&env)?;
        Ok(env.storage().instance().get(&DataKey::PendingAdmin))
    }

    // -----------------------------------------------------------------------
    // Admin rotation
    // -----------------------------------------------------------------------

    fn require_admin(env: &Env) -> Result<SinkConfig, SinkError> {
        let config = storage::load_config(env)?;
        config.admin.clone().require_auth();
        Ok(config)
    }

    /// Rotate the admin address (single-step).
    ///
    /// Clears any stale two-step proposal to prevent a superseded nominee from
    /// completing a hand-off.
    ///
    /// # Errors
    ///
    /// * [`SinkError::Unauthorized`] — caller is not the current admin.
    /// * [`SinkError::InvalidNewAdmin`] — `new_admin` is the zero address or
    ///   the contract itself.
    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), SinkError> {
        let mut config = Self::require_admin(&env)?;
        if storage::is_forbidden_admin(&env, &new_admin) {
            return Err(SinkError::InvalidNewAdmin);
        }
        config.admin = new_admin;
        storage::save_config(&env, &config);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        storage::extend_instance(&env);
        Ok(())
    }

    /// Nominate a successor admin for a two-step hand-off.
    ///
    /// # Errors
    ///
    /// * [`SinkError::Unauthorized`], [`SinkError::InvalidNewAdmin`]
    pub fn propose_admin(env: Env, new_admin: Address) -> Result<(), SinkError> {
        let config = Self::require_admin(&env)?;
        if storage::is_forbidden_admin(&env, &new_admin) {
            return Err(SinkError::InvalidNewAdmin);
        }
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);
        storage::extend_instance(&env);

        events::AdminTransferProposed {
            current: config.admin,
            pending: new_admin,
        }
        .publish(&env);
        Ok(())
    }

    /// Accept a pending admin nomination.
    ///
    /// # Errors
    ///
    /// * [`SinkError::NoPendingAdmin`] — no rotation is pending.
    pub fn accept_admin(env: Env) -> Result<(), SinkError> {
        let pending: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .ok_or(SinkError::NoPendingAdmin)?;
        pending.require_auth();

        let mut config = storage::load_config(&env)?;
        let previous = config.admin.clone();
        config.admin = pending;
        env.storage().instance().remove(&DataKey::PendingAdmin);
        storage::save_config(&env, &config);

        events::AdminTransferred {
            previous,
            new: config.admin,
        }
        .publish(&env);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Validate the fields of a [`SinkEntry`], returning the specific
    /// [`SinkError`] variant (with its custom message) for each failure mode.
    ///
    /// This is the core of issue #461: every structural problem in the payload
    /// maps to a *distinct* error code with a *distinct* human-readable message,
    /// so clients can distinguish e.g. "topic is empty" from "sequence is zero"
    /// without inspecting the payload themselves.
    fn validate_entry(env: &Env, entry: &SinkEntry) -> Result<(), SinkError> {
        // Rule 1: source_contract must not be the zero address.
        if storage::is_zero_address(env, &entry.source_contract) {
            return Err(SinkError::InvalidSourceContract);
        }

        // Rule 2: topic must be non-empty.
        // A Symbol produced by `Symbol::new(env, "")` compares equal to
        // another empty Symbol.  This check works in both WASM and native
        // contexts without requiring std.
        {
            let empty_sym = Symbol::new(env, "");
            if entry.topic == empty_sym {
                return Err(SinkError::EmptyTopic);
            }
        }

        // Rule 3: sequence must be > 0.
        if entry.sequence == 0 {
            return Err(SinkError::InvalidSequence);
        }

        // Rule 4: payload length must not exceed the cap.
        if entry.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(SinkError::PayloadTooLarge);
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod test {
    mod admin;
    mod common;
    mod emit;
    mod errors;
}
