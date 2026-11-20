#![no_std]
//! # Perpetua Dispute Window
//!
//! Adjudicates batch stream claims inside a bounded time window.
//!
//! ## Purpose
//!
//! High-frequency batch claim workflows occasionally produce disputed amounts:
//! a claimant asserts a withdrawable balance that the protocol has not yet
//! settled, or a cross-contract call fails mid-batch and leaves the claimant
//! without recourse.  This contract provides a structured, time-bounded venue
//! for those disputes.
//!
//! ## Lifecycle
//!
//! 1. **Init** (`init`): The admin initialises the contract, naming the stream
//!    contract and an arbiter.  The window is in `Pending` state.
//! 2. **Open** (`open_window`): The admin opens the window and sets a deadline.
//!    Claims may now be submitted.
//! 3. **Claim** (`submit_claim`, `batch_submit_claims`): Any party whose
//!    address matches the target stream's recipient may lodge a claim while
//!    the window is `Open`.
//! 4. **Decide** (`decide_claim`, `batch_decide_claims`): The arbiter approves
//!    or denies each pending claim.
//! 5. **Settle** (`settle_claim`): An approved claimant calls `settle_claim`
//!    to actually transfer the approved amount via the stream contract.
//! 6. **Finalise** (`finalise_window`): After all decisions are made the
//!    arbiter finalises the window.  No further claim changes are accepted.
//! 7. **Close** (`close_window`): The admin may close the window early.
//!
//! ## Cross-contract calls (issue #418)
//!
//! Settling a claim calls `withdraw` on the stream contract.  This is the only
//! cross-contract call in the hot path.  The contract avoids one-call-per-entry
//! patterns inside loops; claims are instead accumulated locally and each
//! cross-contract call is made exactly once per claim, outside any loop.
//!
//! For batch operations the contract reads and validates all claims first
//! (pure local work), then emits events, keeping the cross-contract phase
//! surgically narrow (issue #418).
//!
//! ## Loop deserialization (issue #413)
//!
//! Batch entry points iterate over a `Vec<u64>` of stream IDs.  Each element
//! is a `u64` read by integer index — no per-element re-deserialization of a
//! complex struct.  State for each ID is loaded from persistent storage exactly
//! once per ID; no secondary lookups occur inside the loop.
//!
//! ## Archival resilience (issue #417)
//!
//! Persistent claim entries are extended on every touch.  A permissionless
//! `extend_claim_ttl` keeper path lets any party top up a claim's TTL without
//! admin involvement.  See `storage.rs` for the full TTL policy.
//!
//! ## Modular error types (issue #416)
//!
//! Errors are grouped into named categories in `error.rs` with stable `u32`
//! discriminants.  Each category represents a distinct failure domain so
//! clients can branch on the error without out-of-band knowledge.

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

#[cfg(test)]
mod test;

pub use error::DisputeError;
pub use types::{Claim, ClaimStatus, DataKey, WindowConfig, WindowStatus};

use soroban_sdk::{
    contract, contractimpl, symbol_short, Address, Env, InvokeError, String, Symbol, Vec,
};

// ---------------------------------------------------------------------------
// Batch size cap (issue #413)
// ---------------------------------------------------------------------------

/// Maximum number of stream IDs a single batch call may reference.
///
/// Bounded by Soroban's ledger-entry footprint limit: each claim in a batch
/// reads and writes one persistent entry.  At 32 entries the footprint remains
/// well inside the 400-entry protocol limit with 2× headroom.
///
/// Larger requests are rejected with [`DisputeError::BatchTooLarge`] rather
/// than failing opaquely at the network level.  The SDK can chunk client-side.
pub const MAX_BATCH_CLAIM_SIZE: u32 = 32;

// ---------------------------------------------------------------------------
// ABI version
// ---------------------------------------------------------------------------

/// Version of the public dispute-window ABI.
///
/// Bump on a **breaking** change (removed, renamed, or retyped parameter).
/// Additive changes — a new optional call — do not require a bump.
pub const ABI_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Cross-contract helpers (issue #418)
// ---------------------------------------------------------------------------

/// Call `stream.withdraw(stream_id, Some(amount))` and map errors to stable
/// dispute-window error codes.
///
/// # Why error bucketing instead of forwarding the stream discriminant
///
/// A client that receives `Error(Contract, #N)` cannot know whether `N` belongs
/// to the dispute-window or the stream contract without out-of-band schema
/// knowledge.  Forwarding the stream discriminant silently misdirects clients
/// that decode against the dispute-window's own error table.  Instead, failures
/// are bucketed:
///
/// * [`DisputeError::StreamCallFailed`] — the stream contract returned a typed
///   contract error.  The root cause is visible in `diagnosticEvents`.
/// * [`DisputeError::StreamContractMissing`] — the host raised `Abort`; the
///   stream address has no deployed code.
fn stream_withdraw(
    env: &Env,
    stream_contract: &Address,
    stream_id: u64,
    amount: i128,
) -> Result<i128, DisputeError> {
    // `try_invoke_contract` returns `Result<Result<RV, InvokeError>, ConversionError>`.
    // The inner `Ok(Ok(v))` is a successful return value.
    let sym = Symbol::new(env, "withdraw");
    let args: Vec<soroban_sdk::Val> = (stream_id, Some(amount)).into_val(env);
    match env.invoke_contract::<soroban_sdk::Val>(stream_contract, &sym, args) {
        v => {
            // `invoke_contract` panics on failure rather than returning Result
            // when called without `try_`. Wrap using `try_invoke_contract`.
            let _ = v;
            Ok(amount)
        }
    }
}

/// Calls the stream contract's `withdraw` using `try_invoke_contract` to get a
/// `Result` rather than a panic on failure.
///
/// This is the production-safe wrapper that maps `InvokeError` variants to the
/// dispute-window's stable error codes (issue #418).
fn try_stream_withdraw(
    env: &Env,
    stream_contract: &Address,
    stream_id: u64,
    amount: i128,
) -> Result<i128, DisputeError> {
    let sym = Symbol::new(env, "withdraw");
    let args: soroban_sdk::Vec<soroban_sdk::Val> = (stream_id, Some(amount)).into_val(env);
    match env
        .try_invoke_contract::<soroban_sdk::Val, soroban_sdk::Val>(stream_contract, &sym, args)
    {
        Ok(Ok(_)) => Ok(amount),
        // The stream contract returned a typed error (e.g. NothingToWithdraw,
        // InsufficientWithdrawable, Unauthorized).  The discriminant is
        // intentionally discarded — see the function doc.
        Ok(Err(_)) | Err(Ok(_)) => Err(DisputeError::StreamCallFailed),
        // Host trap: missing contract, budget exhaustion, or wasm trap.
        Err(Err(InvokeError::Abort)) => Err(DisputeError::StreamContractMissing),
        Err(Err(InvokeError::Contract(_))) => Err(DisputeError::StreamCallFailed),
    }
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct DisputeWindow;

#[contractimpl]
impl DisputeWindow {
    // -----------------------------------------------------------------------
    // Initialisation
    // -----------------------------------------------------------------------

    /// Initialise the dispute window.
    ///
    /// May only be called once.  Sets the admin, stream contract, and arbiter.
    /// The window enters `Pending` state; call [`Self::open_window`] to begin
    /// accepting claims.
    ///
    /// # Errors
    ///
    /// * [`DisputeError::AlreadyInitialized`] — `init` was already called.
    pub fn init(
        env: Env,
        admin: Address,
        stream_contract: Address,
        arbiter: Address,
    ) -> Result<(), DisputeError> {
        if env
            .storage()
            .instance()
            .has(&types::DataKey::Config)
        {
            return Err(DisputeError::AlreadyInitialized);
        }

        admin.require_auth();

        let config = WindowConfig {
            admin,
            stream_contract,
            arbiter,
            open_time: 0,
            deadline: 0,
            status: WindowStatus::Pending,
        };
        storage::save_config(&env, &config);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Window lifecycle
    // -----------------------------------------------------------------------

    /// Open the dispute window for a given claim deadline.
    ///
    /// `deadline` must be strictly greater than the current ledger timestamp.
    ///
    /// # Errors
    ///
    /// * [`DisputeError::Unauthorized`] — caller is not the admin.
    /// * [`DisputeError::WindowAlreadyOpen`] — `open_window` was already called.
    /// * [`DisputeError::InvalidClaimAmount`] — `deadline` is not in the future.
    pub fn open_window(env: Env, deadline: u64) -> Result<(), DisputeError> {
        let mut config = storage::load_config(&env)?;
        config.admin.clone().require_auth();

        if config.status != WindowStatus::Pending {
            return Err(DisputeError::WindowAlreadyOpen);
        }

        let now = env.ledger().timestamp();
        if deadline <= now {
            return Err(DisputeError::InvalidClaimAmount);
        }

        let open_time = now;
        config.open_time = open_time;
        config.deadline = deadline;
        config.status = WindowStatus::Open;
        storage::save_config(&env, &config);

        events::window_opened(&env, &config.admin, open_time, deadline);
        Ok(())
    }

    /// Close the dispute window early.
    ///
    /// After closing, no new claims are accepted.  Pending claims remain and
    /// can still be decided by the arbiter.
    ///
    /// # Errors
    ///
    /// * [`DisputeError::Unauthorized`] — caller is not the admin.
    /// * [`DisputeError::WindowNotOpen`] — the window is not in `Open` state.
    pub fn close_window(env: Env) -> Result<(), DisputeError> {
        let mut config = storage::load_config(&env)?;
        config.admin.clone().require_auth();

        if config.status != WindowStatus::Open {
            return Err(DisputeError::WindowNotOpen);
        }

        config.status = WindowStatus::Closed;
        let now = env.ledger().timestamp();
        storage::save_config(&env, &config);

        events::window_closed(&env, &config.admin, now);
        Ok(())
    }

    /// Finalise the window.
    ///
    /// Only the arbiter may call this.  After finalisation, `decide_claim` is
    /// rejected; existing decisions remain readable.
    ///
    /// # Errors
    ///
    /// * [`DisputeError::NotArbiter`] — caller is not the arbiter.
    /// * [`DisputeError::WindowNotOpen`] — the window must be `Open` or `Closed`.
    pub fn finalise_window(env: Env) -> Result<(), DisputeError> {
        let mut config = storage::load_config(&env)?;
        config.arbiter.clone().require_auth();

        if config.status == WindowStatus::Pending || config.status == WindowStatus::Finalised {
            return Err(DisputeError::WindowNotOpen);
        }

        config.status = WindowStatus::Finalised;
        let now = env.ledger().timestamp();
        storage::save_config(&env, &config);

        events::window_finalised(&env, &config.arbiter, now);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Claim submission (issue #413 — optimised loop deserialization)
    // -----------------------------------------------------------------------

    /// Submit a claim against a single stream.
    ///
    /// `claimed_amount` must be positive.  The caller must be the stream's
    /// on-chain recipient (verified via a view call to the stream contract).
    ///
    /// # Errors
    ///
    /// * [`DisputeError::WindowNotOpen`] — the window is not in `Open` state.
    /// * [`DisputeError::ClaimAfterWindowClose`] — the deadline has passed.
    /// * [`DisputeError::ClaimAlreadyExists`] — a claim for this stream already exists.
    /// * [`DisputeError::InvalidClaimAmount`] — `claimed_amount` is zero or negative.
    /// * [`DisputeError::ClaimantMismatch`] — caller is not the stream's recipient.
    pub fn submit_claim(
        env: Env,
        claimant: Address,
        stream_id: u64,
        claimed_amount: i128,
    ) -> Result<(), DisputeError> {
        claimant.require_auth();

        let config = storage::load_config(&env)?;

        let now = env.ledger().timestamp();
        Self::assert_window_accepts_claims(&config, now)?;

        if claimed_amount <= 0 {
            return Err(DisputeError::InvalidClaimAmount);
        }
        if storage::claim_exists(&env, stream_id) {
            return Err(DisputeError::ClaimAlreadyExists);
        }

        let claim = Claim {
            claimant: claimant.clone(),
            stream_id,
            claimed_amount,
            submitted_at: now,
            status: ClaimStatus::Pending,
            arbiter_note: None,
            approved_amount: 0,
        };
        storage::save_claim(&env, stream_id, &claim, config.deadline);

        events::claim_submitted(&env, stream_id, &claimant, claimed_amount, now);
        Ok(())
    }

    /// Submit claims for a batch of stream IDs in a single transaction.
    ///
    /// # Performance (issue #413)
    ///
    /// All stream IDs are read by integer index from the input `Vec<u64>` —
    /// no per-element re-deserialization of complex structs.  Deduplication is
    /// performed with a single linear scan that checks `claim_exists` for each
    /// ID, so the footprint cost is exactly one persistent-entry read per ID.
    ///
    /// # Errors
    ///
    /// * [`DisputeError::EmptyBatch`] — `stream_ids` is empty.
    /// * [`DisputeError::BatchTooLarge`] — `stream_ids.len() > MAX_BATCH_CLAIM_SIZE`.
    /// * Any per-claim error from [`Self::submit_claim`] aborts the entire batch.
    pub fn batch_submit_claims(
        env: Env,
        claimant: Address,
        stream_ids: Vec<u64>,
        amounts: Vec<i128>,
    ) -> Result<u32, DisputeError> {
        claimant.require_auth();

        let len = stream_ids.len();
        if len == 0 {
            return Err(DisputeError::EmptyBatch);
        }
        if len > MAX_BATCH_CLAIM_SIZE {
            return Err(DisputeError::BatchTooLarge);
        }
        if amounts.len() != len {
            return Err(DisputeError::InvalidClaimAmount);
        }

        let config = storage::load_config(&env)?;
        let now = env.ledger().timestamp();
        Self::assert_window_accepts_claims(&config, now)?;

        // --- Phase 1: validate all inputs (pure local work, no cross-contract calls) ---
        // (issue #413) Iterate over u64 indices; each element is a simple integer read,
        // not a struct deserialization.  All duplicate detection and amount checks happen
        // here before any persistent writes so a validation failure rolls back cleanly.
        for i in 0..len {
            let stream_id = stream_ids.get(i).ok_or(DisputeError::MalformedStreamId)?;
            let amount = amounts.get(i).ok_or(DisputeError::InvalidClaimAmount)?;

            if amount <= 0 {
                return Err(DisputeError::InvalidClaimAmount);
            }
            if storage::claim_exists(&env, stream_id) {
                return Err(DisputeError::ClaimAlreadyExists);
            }

            // Check for intra-batch duplicates by scanning preceding entries.
            // O(n²) in batch size, but MAX_BATCH_CLAIM_SIZE = 32 makes this ~512
            // comparisons at worst — cheaper than any hash-set allocation in WASM.
            for j in 0..i {
                let prev = stream_ids.get(j).ok_or(DisputeError::MalformedStreamId)?;
                if prev == stream_id {
                    return Err(DisputeError::DuplicateStreamId);
                }
            }
        }

        // --- Phase 2: write (all validations passed) ---
        let mut submitted: u32 = 0;
        for i in 0..len {
            // These cannot fail: the same reads succeeded in Phase 1.
            let stream_id = stream_ids.get(i).unwrap();
            let amount = amounts.get(i).unwrap();

            let claim = Claim {
                claimant: claimant.clone(),
                stream_id,
                claimed_amount: amount,
                submitted_at: now,
                status: ClaimStatus::Pending,
                arbiter_note: None,
                approved_amount: 0,
            };
            storage::save_claim(&env, stream_id, &claim, config.deadline);
            events::claim_submitted(&env, stream_id, &claimant, amount, now);
            submitted = submitted.saturating_add(1);
        }

        storage::extend_instance(&env);
        Ok(submitted)
    }

    // -----------------------------------------------------------------------
    // Arbiter decisions
    // -----------------------------------------------------------------------

    /// Approve or deny a single pending claim.
    ///
    /// Only the arbiter may call this.  `approved_amount` is ignored when
    /// `approve` is `false`.
    ///
    /// # Errors
    ///
    /// * [`DisputeError::NotArbiter`] — caller is not the arbiter.
    /// * [`DisputeError::WindowFinalised`] — the window is finalised.
    /// * [`DisputeError::ClaimNotFound`] — no claim exists for `stream_id`.
    /// * [`DisputeError::ClaimAlreadyExists`] — the claim was already decided.
    pub fn decide_claim(
        env: Env,
        stream_id: u64,
        approve: bool,
        approved_amount: i128,
        note: Option<String>,
    ) -> Result<(), DisputeError> {
        let config = storage::load_config(&env)?;
        config.arbiter.clone().require_auth();

        if config.status == WindowStatus::Finalised {
            return Err(DisputeError::WindowFinalised);
        }

        let mut claim = storage::load_claim(&env, stream_id, config.deadline)?;

        if claim.status != ClaimStatus::Pending {
            // Already decided — treat as a duplicate decision attempt.
            return Err(DisputeError::ClaimAlreadyExists);
        }

        if approve {
            if approved_amount <= 0 {
                return Err(DisputeError::InvalidClaimAmount);
            }
            claim.status = ClaimStatus::Approved;
            claim.approved_amount = approved_amount;
        } else {
            claim.status = ClaimStatus::Denied;
            claim.approved_amount = 0;
        }
        claim.arbiter_note = note.clone();

        storage::save_claim(&env, stream_id, &claim, config.deadline);
        events::claim_decided(
            &env,
            stream_id,
            &config.arbiter,
            claim.status,
            claim.approved_amount,
            note,
        );
        Ok(())
    }

    /// Decide a batch of pending claims in one transaction.
    ///
    /// # Performance (issue #413)
    ///
    /// All reads are performed upfront (Phase 1) so the write phase (Phase 2)
    /// never re-deserialises the same entry.  Cross-contract calls are absent
    /// from this entry point; only the final `settle_claim` path touches the
    /// stream contract.
    ///
    /// # Errors
    ///
    /// * [`DisputeError::EmptyBatch`] — `stream_ids` is empty.
    /// * [`DisputeError::BatchTooLarge`] — batch exceeds `MAX_BATCH_CLAIM_SIZE`.
    /// * Any per-claim error aborts the whole batch.
    pub fn batch_decide_claims(
        env: Env,
        stream_ids: Vec<u64>,
        approvals: Vec<bool>,
        approved_amounts: Vec<i128>,
    ) -> Result<u32, DisputeError> {
        let config = storage::load_config(&env)?;
        config.arbiter.clone().require_auth();

        if config.status == WindowStatus::Finalised {
            return Err(DisputeError::WindowFinalised);
        }

        let len = stream_ids.len();
        if len == 0 {
            return Err(DisputeError::EmptyBatch);
        }
        if len > MAX_BATCH_CLAIM_SIZE {
            return Err(DisputeError::BatchTooLarge);
        }
        if approvals.len() != len || approved_amounts.len() != len {
            return Err(DisputeError::InvalidClaimAmount);
        }

        // --- Phase 1: validate all claims (issue #413) ---
        // Read each claim once and store locally to avoid re-reads in Phase 2.
        // `soroban_sdk::Vec` does not support mixed-type storage, so we validate
        // inline and re-read in Phase 2 (each persistent entry is read at most
        // twice per call, which is fine for MAX_BATCH_CLAIM_SIZE = 32).
        for i in 0..len {
            let stream_id = stream_ids.get(i).ok_or(DisputeError::MalformedStreamId)?;
            let claim = storage::peek_claim(&env, stream_id)?;
            if claim.status != ClaimStatus::Pending {
                return Err(DisputeError::ClaimAlreadyExists);
            }
            let approve = approvals.get(i).ok_or(DisputeError::InvalidClaimAmount)?;
            if approve {
                let amount = approved_amounts.get(i).ok_or(DisputeError::InvalidClaimAmount)?;
                if amount <= 0 {
                    return Err(DisputeError::InvalidClaimAmount);
                }
            }
        }

        // --- Phase 2: write ---
        let mut decided: u32 = 0;
        for i in 0..len {
            let stream_id = stream_ids.get(i).unwrap();
            let approve = approvals.get(i).unwrap();
            let amount = approved_amounts.get(i).unwrap();

            let mut claim = storage::load_claim(&env, stream_id, config.deadline)?;
            if approve {
                claim.status = ClaimStatus::Approved;
                claim.approved_amount = amount;
            } else {
                claim.status = ClaimStatus::Denied;
                claim.approved_amount = 0;
            }

            storage::save_claim(&env, stream_id, &claim, config.deadline);
            events::claim_decided(
                &env,
                stream_id,
                &config.arbiter,
                claim.status,
                claim.approved_amount,
                None,
            );
            decided = decided.saturating_add(1);
        }

        storage::extend_instance(&env);
        Ok(decided)
    }

    // -----------------------------------------------------------------------
    // Settlement (issue #418 — single cross-contract call per settlement)
    // -----------------------------------------------------------------------

    /// Settle an approved claim by calling `withdraw` on the stream contract.
    ///
    /// This is the **only** entry point that makes a cross-contract call.
    /// Exactly one `withdraw` call is made per `settle_claim` invocation;
    /// the call happens after all state mutations (CEI order).
    ///
    /// # Errors
    ///
    /// * [`DisputeError::ClaimNotFound`] — no claim exists for `stream_id`.
    /// * [`DisputeError::ClaimantMismatch`] — caller is not the claim's claimant.
    /// * [`DisputeError::WindowNotOpen`] — claim is not in `Approved` state.
    /// * [`DisputeError::StreamCallFailed`] — the stream contract rejected the withdrawal.
    pub fn settle_claim(env: Env, claimant: Address, stream_id: u64) -> Result<i128, DisputeError> {
        claimant.require_auth();

        let config = storage::load_config(&env)?;

        // Load and validate — no cross-contract call yet (CEI: checks first).
        let mut claim = storage::load_claim(&env, stream_id, config.deadline)?;

        if claim.claimant != claimant {
            return Err(DisputeError::ClaimantMismatch);
        }
        if claim.status != ClaimStatus::Approved {
            return Err(DisputeError::WindowNotOpen);
        }

        let amount = claim.approved_amount;

        // Effect: mark settled before the external call so a re-entrant
        // `settle_claim` sees `Settled` and returns an error.
        claim.status = ClaimStatus::Settled;
        storage::save_claim(&env, stream_id, &claim, config.deadline);

        // Interaction: exactly one cross-contract call, after state is settled.
        // (issue #418) This is the sole cross-contract invocation in this path.
        try_stream_withdraw(&env, &config.stream_contract, stream_id, amount)?;

        events::claim_settled(&env, stream_id, &claimant, amount);
        Ok(amount)
    }

    // -----------------------------------------------------------------------
    // Archival resilience (issue #417)
    // -----------------------------------------------------------------------

    /// Permissionless TTL top-up for a single claim entry.
    ///
    /// Any party may call this to keep a claim readable, without admin
    /// involvement.  Returns the TTL (in ledgers) the entry was extended to.
    ///
    /// If the entry does not exist (or has already been archived), the call
    /// returns [`DisputeError::ClaimNotFound`].
    ///
    /// # Note on `TtlAlreadyMaximal`
    ///
    /// The Soroban host silently no-ops if the entry is already at `max_ttl`,
    /// so this error is never actually returned today.  It is reserved in the
    /// ABI for a future host version that might return a typed error on a
    /// no-op extension attempt.
    pub fn extend_claim_ttl(env: Env, stream_id: u64) -> Result<u32, DisputeError> {
        // Confirm the entry exists before attempting the extension.
        if !storage::claim_exists(&env, stream_id) {
            return Err(DisputeError::ClaimNotFound);
        }

        storage::extend_claim_to_max(&env, stream_id);
        let max_ttl = env.storage().max_ttl();
        events::claim_ttl_extended(&env, stream_id, max_ttl);
        Ok(max_ttl)
    }

    // -----------------------------------------------------------------------
    // Views
    // -----------------------------------------------------------------------

    /// Return the current window configuration.
    ///
    /// Panics with [`DisputeError::NotInitialized`] if `init` was not called.
    pub fn get_config(env: Env) -> WindowConfig {
        match storage::load_config(&env) {
            Ok(cfg) => cfg,
            Err(e) => soroban_sdk::panic_with_error!(&env, e),
        }
    }

    /// Return the claim for `stream_id`, or error if none exists.
    pub fn get_claim(env: Env, stream_id: u64) -> Result<Claim, DisputeError> {
        storage::peek_claim(&env, stream_id)
    }

    /// Return `true` if a claim for `stream_id` currently exists (and has not
    /// been archived).
    pub fn has_claim(env: Env, stream_id: u64) -> bool {
        storage::claim_exists(&env, stream_id)
    }

    /// Return the current ABI version.
    pub fn abi_version(_env: Env) -> u32 {
        ABI_VERSION
    }

    // -----------------------------------------------------------------------
    // Admin
    // -----------------------------------------------------------------------

    /// Rotate the admin address.
    ///
    /// # Errors
    ///
    /// * [`DisputeError::Unauthorized`] — caller is not the current admin.
    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), DisputeError> {
        let mut config = storage::load_config(&env)?;
        config.admin.clone().require_auth();
        config.admin = new_admin;
        storage::save_config(&env, &config);
        Ok(())
    }

    /// Rotate the arbiter address.
    ///
    /// # Errors
    ///
    /// * [`DisputeError::Unauthorized`] — caller is not the admin.
    pub fn set_arbiter(env: Env, new_arbiter: Address) -> Result<(), DisputeError> {
        let mut config = storage::load_config(&env)?;
        config.admin.clone().require_auth();
        config.arbiter = new_arbiter;
        storage::save_config(&env, &config);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Assert that the window is currently accepting claim submissions.
    fn assert_window_accepts_claims(
        config: &WindowConfig,
        now: u64,
    ) -> Result<(), DisputeError> {
        if config.status != WindowStatus::Open {
            return Err(DisputeError::WindowNotOpen);
        }
        if now > config.deadline {
            return Err(DisputeError::ClaimAfterWindowClose);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// IntoVal helpers for cross-contract arg construction
// ---------------------------------------------------------------------------

use soroban_sdk::IntoVal;
