#![no_std]
//! # Perpetua cliff-vesting
//!
//! A cliff-vesting schedule registry for Soroban.  Tokens accrue linearly
//! from `start_time` to `end_time` but are withheld until the beneficiary's
//! `cliff_time` passes.  At the cliff, the entire accrued balance since
//! `start_time` becomes withdrawable at once.
//!
//! ## Issue #403 — granular role-based authorization
//!
//! Configuration updates are gated behind four independent role addresses:
//!
//! | Role      | Operations |
//! |-----------|-----------|
//! | `admin`   | all setters, role assignments, admin rotation |
//! | `creator` | `create_schedule`, `batch_create` |
//! | `revoker` | `revoke_schedule` (claws back unvested tokens) |
//! | `pauser`  | `pause_schedule`, `resume_schedule` |
//!
//! Each role is a distinct address stored in `VestingConfig`.  Any address can
//! hold multiple roles (e.g. admin == creator), but each role's check is
//! independent, so a governance upgrade can restrict creation without granting
//! revoke authority.  An admin rotation never implicitly grants or removes any
//! other role.
//!
//! ## Issue #404 — minimize Soroban host CPU instructions in batch paths
//!
//! The batch entry points (`batch_create`, `batch_withdraw`, `batch_revoke`)
//! use `peek_schedule` for read-only pre-flight validation and
//! `load_schedule` / `save_schedule` only on the mutating pass.  Each schedule
//! is deserialized from persistent storage exactly once per batch call,
//! regardless of how many computations run against it.  No re-serialization of
//! intermediate state occurs mid-batch.  All pure arithmetic is performed in
//! `accrual.rs` without further storage access, eliminating the per-item
//! deserialization overhead that otherwise dominates batch CPU costs.

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

pub mod accrual;
pub mod error;
pub mod events;
pub mod invariants;
pub mod math;
pub mod storage;
pub mod types;

pub use accrual::{cliff_reached, duration, elapsed, liability, refundable, vested, withdrawable};
pub use error::Error;
pub use math::{calculate_bps_payout, calculate_fractional_payout, FractionalSplit};
pub use storage::{MIN_SCHEDULE_TTL_LEDGERS, SECONDS_PER_LEDGER, TTL_BUFFER_SECONDS};
pub use types::{DataKey, ScheduleStatus, VestingConfig, VestingSchedule};

use soroban_sdk::{
    contract, contractimpl, token, Address, Env, IntoVal, InvokeError, MuxedAddress, Vec,
};

/// Maximum number of schedules one batch call may process.
pub const MAX_BATCH_SIZE: u32 = 16;

// ---------------------------------------------------------------------------
// Token transfer helper
// ---------------------------------------------------------------------------

fn token_transfer(
    env: &Env,
    token: &Address,
    from: &Address,
    to: MuxedAddress,
    amount: &i128,
) -> Result<(), Error> {
    match token::TokenClient::new(env, token).try_transfer(from, &to, amount) {
        Ok(Ok(())) => Ok(()),
        Err(Err(InvokeError::Contract(_))) | Ok(Err(_)) | Err(Ok(_)) => {
            Err(Error::TokenTransferFailed)
        }
        Err(Err(InvokeError::Abort)) => Err(Error::TokenMissing),
    }
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct CliffVesting;

#[contractimpl]
impl CliffVesting {
    // -----------------------------------------------------------------------
    // Initialization
    // -----------------------------------------------------------------------

    /// Initialize the contract with the four role addresses.
    ///
    /// # Errors
    ///
    /// * [`Error::AlreadyInitialized`]
    pub fn init(
        env: Env,
        admin: Address,
        creator: Address,
        revoker: Address,
        pauser: Address,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Config) {
            return Err(Error::AlreadyInitialized);
        }
        admin.require_auth();
        let config = VestingConfig {
            admin,
            creator,
            revoker,
            pauser,
        };
        storage::save_config(&env, &config);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Schedule creation — creator role required (#403)
    // -----------------------------------------------------------------------

    /// Create a cliff-vesting schedule and lock `deposit` from `grantor`.
    ///
    /// Returns the new schedule ID.  The ID is monotonic and never reused.
    ///
    /// # Cliff semantics
    ///
    /// `cliff_time` **gates** withdrawals; it does not delay accrual.  At the
    /// cliff instant the beneficiary becomes entitled to everything accrued
    /// continuously since `start_time`.  Pass `cliff_time == start_time` for
    /// a schedule with no cliff.
    ///
    /// # Errors
    ///
    /// * [`Error::Unauthorized`] — caller does not hold the `creator` role.
    /// * [`Error::SelfVest`] — grantor and beneficiary are the same address.
    /// * [`Error::InvalidDeposit`] — deposit is not positive.
    /// * [`Error::InvalidTimeRange`] — `end_time <= start_time`.
    /// * [`Error::InvalidCliff`] — cliff outside `[start_time, end_time]`.
    /// * [`Error::DepositTooSmall`] — `deposit < (end_time - start_time)`.
    /// * [`Error::Overflow`] — `deposit * duration` overflows `i128`.
    #[allow(clippy::too_many_arguments)]
    pub fn create_schedule(
        env: Env,
        creator: Address,
        grantor: Address,
        beneficiary: Address,
        token: Address,
        deposit: i128,
        start_time: u64,
        end_time: u64,
        cliff_time: u64,
    ) -> Result<u64, Error> {
        // Role check: caller must be the registered creator.
        let config = storage::load_config(&env)?;
        if creator != config.creator {
            return Err(Error::Unauthorized);
        }
        creator.require_auth_for_args(
            (
                grantor.clone(),
                beneficiary.clone(),
                token.clone(),
                deposit,
                start_time,
                end_time,
                cliff_time,
            )
                .into_val(&env),
        );

        Self::validate_schedule_params(
            &env,
            &grantor,
            &beneficiary,
            deposit,
            start_time,
            end_time,
            cliff_time,
        )?;

        let schedule_id = storage::allocate_schedule_id(&env)?;
        let schedule = VestingSchedule {
            grantor: grantor.clone(),
            beneficiary,
            token: token.clone(),
            deposited: deposit,
            withdrawn: 0,
            start_time,
            end_time,
            cliff_time,
            paused_at: None,
            paused_total: 0,
            status: ScheduleStatus::Active,
        };

        // Pull deposit before persisting the schedule (CEI).
        token_transfer(
            &env,
            &token,
            &grantor,
            MuxedAddress::from(env.current_contract_address()),
            &deposit,
        )?;

        storage::save_schedule(&env, schedule_id, &schedule);
        storage::extend_instance(&env);

        events::schedule_created(&env, schedule_id, &schedule);
        Ok(schedule_id)
    }

    /// Create multiple schedules in one call.
    ///
    /// All validations run before any storage is mutated or tokens moved.
    /// A single validation failure reverts the entire batch.  Returns the
    /// IDs of all created schedules, in input order.
    ///
    /// Issue #404: validation loops deserialize each schedule's parameters
    /// from the input vectors exactly once and perform pure-arithmetic checks
    /// without additional storage access.
    ///
    /// # Errors
    ///
    /// * [`Error::Unauthorized`] — caller does not hold the `creator` role.
    /// * [`Error::EmptyBatch`], [`Error::BatchTooLarge`]
    /// * Any per-schedule creation error (all validated before execution).
    #[allow(clippy::too_many_arguments)]
    pub fn batch_create(
        env: Env,
        creator: Address,
        grantors: Vec<Address>,
        beneficiaries: Vec<Address>,
        tokens: Vec<Address>,
        deposits: Vec<i128>,
        start_times: Vec<u64>,
        end_times: Vec<u64>,
        cliff_times: Vec<u64>,
    ) -> Result<Vec<u64>, Error> {
        let config = storage::load_config(&env)?;
        if creator != config.creator {
            return Err(Error::Unauthorized);
        }
        creator.require_auth_for_args(
            (
                grantors.clone(),
                beneficiaries.clone(),
                tokens.clone(),
                deposits.clone(),
                start_times.clone(),
                end_times.clone(),
                cliff_times.clone(),
            )
                .into_val(&env),
        );

        let n = grantors.len();
        if n == 0 {
            return Err(Error::EmptyBatch);
        }
        if n > MAX_BATCH_SIZE {
            return Err(Error::BatchTooLarge);
        }

        // --- Pre-flight validation (no storage mutations) ---
        // Issue #404: each parameter vector is iterated once; no deserialization
        // of persistent storage entries occurs here.
        for i in 0..n {
            let grantor = grantors.get_unchecked(i);
            let beneficiary = beneficiaries.get_unchecked(i);
            let deposit = deposits.get_unchecked(i);
            let start = start_times.get_unchecked(i);
            let end = end_times.get_unchecked(i);
            let cliff = cliff_times.get_unchecked(i);
            Self::validate_schedule_params(
                &env,
                &grantor,
                &beneficiary,
                deposit,
                start,
                end,
                cliff,
            )?;
        }

        // --- Execution (all validations passed) ---
        let mut ids = Vec::new(&env);
        for i in 0..n {
            let grantor = grantors.get_unchecked(i);
            let beneficiary = beneficiaries.get_unchecked(i);
            let token = tokens.get_unchecked(i);
            let deposit = deposits.get_unchecked(i);
            let start = start_times.get_unchecked(i);
            let end = end_times.get_unchecked(i);
            let cliff = cliff_times.get_unchecked(i);

            let schedule_id = storage::allocate_schedule_id(&env)?;
            let schedule = VestingSchedule {
                grantor: grantor.clone(),
                beneficiary,
                token: token.clone(),
                deposited: deposit,
                withdrawn: 0,
                start_time: start,
                end_time: end,
                cliff_time: cliff,
                paused_at: None,
                paused_total: 0,
                status: ScheduleStatus::Active,
            };
            token_transfer(
                &env,
                &token,
                &grantor,
                MuxedAddress::from(env.current_contract_address()),
                &deposit,
            )?;
            storage::save_schedule(&env, schedule_id, &schedule);
            events::schedule_created(&env, schedule_id, &schedule);
            ids.push_back(schedule_id);
        }

        storage::extend_instance(&env);
        Ok(ids)
    }

    // -----------------------------------------------------------------------
    // Withdrawal — beneficiary only
    // -----------------------------------------------------------------------

    /// Withdraw vested tokens.
    ///
    /// `amount == None` withdraws the full withdrawable balance.  Returns the
    /// amount actually transferred.
    ///
    /// # Errors
    ///
    /// * [`Error::NothingToWithdraw`] — cliff not reached or balance is zero.
    /// * [`Error::InsufficientWithdrawable`] — explicit amount exceeds balance.
    /// * [`Error::ScheduleTerminated`] — schedule is revoked or depleted.
    pub fn withdraw(
        env: Env,
        schedule_id: u64,
        amount: Option<i128>,
    ) -> Result<i128, Error> {
        let mut schedule = storage::load_schedule(&env, schedule_id)?;
        schedule
            .beneficiary
            .require_auth_for_args((schedule_id, amount).into_val(&env));

        let now = env.ledger().timestamp();
        let available = accrual::withdrawable(&schedule, now)?;

        if available == 0 {
            if schedule.status.is_terminal() {
                return Err(Error::ScheduleTerminated);
            }
            return Err(Error::NothingToWithdraw);
        }

        let payout = match amount {
            None => available,
            Some(req) => {
                if req <= 0 {
                    return Err(Error::InvalidAmount);
                }
                if req > available {
                    return Err(Error::InsufficientWithdrawable);
                }
                req
            }
        };

        Self::apply_withdrawal(&env, schedule_id, &mut schedule, payout)?;
        storage::extend_instance(&env);
        Ok(payout)
    }

    /// Withdraw the full available balance from several schedules in one call.
    ///
    /// All schedules must share the same `beneficiary`, who authorizes once.
    /// Schedules with nothing withdrawable are skipped (their TTL is still
    /// extended).  Returns the total transferred.
    ///
    /// Issue #404: the validation pass calls `peek_schedule` (read without TTL
    /// bump); the execution pass operates on the already-deserialized structs
    /// held in a local `Vec`, paying one deserialization per schedule rather
    /// than two.
    ///
    /// # Errors
    ///
    /// * [`Error::EmptyBatch`], [`Error::BatchTooLarge`], [`Error::DuplicateScheduleId`]
    /// * [`Error::ScheduleNotFound`] — unknown ID in batch.
    /// * [`Error::Unauthorized`] — a schedule belongs to a different beneficiary.
    pub fn batch_withdraw(
        env: Env,
        beneficiary: Address,
        schedule_ids: Vec<u64>,
    ) -> Result<i128, Error> {
        Self::reject_duplicate_ids(&schedule_ids)?;
        beneficiary.require_auth_for_args((schedule_ids.clone(),).into_val(&env));

        let n = schedule_ids.len();
        if n == 0 {
            return Err(Error::EmptyBatch);
        }
        if n > MAX_BATCH_SIZE {
            return Err(Error::BatchTooLarge);
        }

        let now = env.ledger().timestamp();

        // --- Pre-flight pass: deserialize each schedule exactly once (#404). ---
        let mut schedules: Vec<VestingSchedule> = Vec::new(&env);
        let mut payouts: Vec<i128> = Vec::new(&env);
        let mut total: i128 = 0;

        for schedule_id in schedule_ids.iter() {
            // peek_schedule: no TTL bump on the read-only pass.
            let schedule = storage::peek_schedule(&env, schedule_id)?;
            if schedule.beneficiary != beneficiary {
                return Err(Error::Unauthorized);
            }
            let available = accrual::withdrawable(&schedule, now)?;
            total = total.checked_add(available).ok_or(Error::Overflow)?;
            schedules.push_back(schedule);
            payouts.push_back(available);
        }

        // --- Execution pass: mutate storage using the already-loaded structs. ---
        for i in 0..n {
            let schedule_id = schedule_ids.get_unchecked(i);
            let mut schedule = schedules.get_unchecked(i);
            let payout = payouts.get_unchecked(i);

            if payout == 0 {
                // No tokens to move; still bump TTL so the entry stays alive.
                storage::extend_schedule(&env, schedule_id, &schedule);
            } else {
                Self::apply_withdrawal(&env, schedule_id, &mut schedule, payout)?;
            }
        }

        if total > 0 {
            storage::extend_instance(&env);
        }
        Ok(total)
    }

    // -----------------------------------------------------------------------
    // Revoke — revoker role required (#403)
    // -----------------------------------------------------------------------

    /// Revoke an active schedule: refund unvested tokens to the grantor.
    ///
    /// The beneficiary keeps everything vested up to this instant.  Revocation
    /// collapses the schedule: `deposited` is reduced to `vested_now` and
    /// `end_time` is set to the current point on the schedule clock.  Subsequent
    /// `withdraw` calls function normally against the reduced deposit.
    ///
    /// Only the address holding the `revoker` role may call this.
    ///
    /// # Errors
    ///
    /// * [`Error::Unauthorized`] — caller is not the `revoker`.
    /// * [`Error::ScheduleTerminated`] — already revoked or depleted.
    pub fn revoke_schedule(env: Env, revoker: Address, schedule_id: u64) -> Result<(), Error> {
        let config = storage::load_config(&env)?;
        if revoker != config.revoker {
            return Err(Error::Unauthorized);
        }
        revoker.require_auth_for_args((schedule_id,).into_val(&env));

        let mut schedule = storage::load_schedule(&env, schedule_id)?;
        if schedule.status.is_terminal() {
            return Err(Error::ScheduleTerminated);
        }

        let now = env.ledger().timestamp();
        let vested_now = accrual::vested(&schedule, now)?;
        let refund = accrual::refundable(&schedule, now)?;

        debug_assert_eq!(
            refund + vested_now,
            schedule.deposited,
            "revoke: conservation broken"
        );

        // Collapse the schedule onto the current clock position.
        let settle_at = accrual::stream_time(&schedule, now).max(schedule.start_time);
        let grantor = schedule.grantor.clone();
        let token = schedule.token.clone();

        schedule.deposited = vested_now;
        schedule.end_time = settle_at;
        schedule.paused_at = None;
        schedule.status = ScheduleStatus::Revoked;

        storage::save_schedule(&env, schedule_id, &schedule);
        storage::extend_instance(&env);

        if refund > 0 {
            token_transfer(
                &env,
                &token,
                &env.current_contract_address(),
                MuxedAddress::from(grantor),
                &refund,
            )?;
        }

        events::revoked(&env, schedule_id, &schedule, refund, vested_now);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Pause / resume — pauser role required (#403)
    // -----------------------------------------------------------------------

    /// Pause a schedule's accrual clock.
    ///
    /// Only the address holding the `pauser` role may call this.
    ///
    /// # Errors
    ///
    /// * [`Error::Unauthorized`] — caller is not the `pauser`.
    /// * [`Error::ScheduleTerminated`]
    pub fn pause_schedule(env: Env, pauser: Address, schedule_id: u64) -> Result<(), Error> {
        let config = storage::load_config(&env)?;
        if pauser != config.pauser {
            return Err(Error::Unauthorized);
        }
        pauser.require_auth_for_args((schedule_id,).into_val(&env));

        let mut schedule = storage::load_schedule(&env, schedule_id)?;
        if schedule.status.is_terminal() {
            return Err(Error::ScheduleTerminated);
        }
        if schedule.paused_at.is_some() {
            return Err(Error::ScheduleTerminated); // already paused — reuse terminal error slot
        }

        let now = env.ledger().timestamp();
        schedule.paused_at = Some(now);
        schedule.status = ScheduleStatus::Paused;
        storage::save_schedule(&env, schedule_id, &schedule);
        storage::extend_instance(&env);

        events::schedule_paused(&env, schedule_id, now);
        Ok(())
    }

    /// Resume a paused schedule.
    ///
    /// Only the address holding the `pauser` role may call this.
    ///
    /// # Errors
    ///
    /// * [`Error::Unauthorized`] — caller is not the `pauser`.
    /// * [`Error::ScheduleTerminated`]
    pub fn resume_schedule(env: Env, pauser: Address, schedule_id: u64) -> Result<(), Error> {
        let config = storage::load_config(&env)?;
        if pauser != config.pauser {
            return Err(Error::Unauthorized);
        }
        pauser.require_auth_for_args((schedule_id,).into_val(&env));

        let mut schedule = storage::load_schedule(&env, schedule_id)?;
        if schedule.status.is_terminal() {
            return Err(Error::ScheduleTerminated);
        }
        let paused_at = match schedule.paused_at {
            Some(t) => t,
            None => return Err(Error::ScheduleTerminated),
        };

        let now = env.ledger().timestamp();
        let paused_duration = now.saturating_sub(paused_at);
        schedule.paused_total = schedule
            .paused_total
            .checked_add(paused_duration)
            .ok_or(Error::Overflow)?;
        schedule.paused_at = None;
        schedule.status = ScheduleStatus::Active;
        storage::save_schedule(&env, schedule_id, &schedule);
        storage::extend_instance(&env);

        events::schedule_resumed(&env, schedule_id, paused_duration, schedule.paused_total);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // TTL keeper — permissionless
    // -----------------------------------------------------------------------

    /// Extend the TTL of a schedule entry.  Permissionless.
    ///
    /// # Errors
    ///
    /// * [`Error::ScheduleNotFound`]
    pub fn extend_schedule_ttl(env: Env, schedule_id: u64) -> Result<(), Error> {
        // load_schedule bumps the TTL and returns an error if not found.
        let schedule = storage::load_schedule(&env, schedule_id)?;
        storage::extend_instance(&env);
        let target = storage::ttl_target_ledgers(&env, &schedule);
        events::ttl_extended(&env, schedule_id, target);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Views — permissionless
    // -----------------------------------------------------------------------

    /// Return the full configuration record.
    pub fn get_config(env: Env) -> Result<VestingConfig, Error> {
        storage::load_config(&env)
    }

    /// Return the vesting schedule for `schedule_id`.
    pub fn get_schedule(env: Env, schedule_id: u64) -> Result<VestingSchedule, Error> {
        storage::peek_schedule(&env, schedule_id)
    }

    /// Total number of issued schedule IDs.
    pub fn schedule_count(env: Env) -> u64 {
        storage::schedule_count(&env)
    }

    /// Whether a persistent entry exists for `schedule_id`.
    pub fn schedule_exists(env: Env, schedule_id: u64) -> bool {
        storage::schedule_exists(&env, schedule_id)
    }

    /// Current withdrawable balance for `schedule_id`.
    pub fn withdrawable_of(env: Env, schedule_id: u64) -> Result<i128, Error> {
        let schedule = storage::peek_schedule(&env, schedule_id)?;
        accrual::withdrawable(&schedule, env.ledger().timestamp())
    }

    /// Total vested for `schedule_id` at the current ledger time.
    pub fn vested_of(env: Env, schedule_id: u64) -> Result<i128, Error> {
        let schedule = storage::peek_schedule(&env, schedule_id)?;
        accrual::vested(&schedule, env.ledger().timestamp())
    }

    // -----------------------------------------------------------------------
    // Role setters — admin only (#403)
    // -----------------------------------------------------------------------

    fn require_admin(env: &Env) -> Result<VestingConfig, Error> {
        let config = storage::load_config(env)?;
        config.admin.clone().require_auth();
        Ok(config)
    }

    /// Replace the `creator` role holder.
    ///
    /// # Errors
    ///
    /// * [`Error::Unauthorized`] — caller is not the admin.
    /// * [`Error::InvalidRoleAddress`] — zero address or the contract itself.
    pub fn set_creator(env: Env, new_creator: Address) -> Result<(), Error> {
        let mut config = Self::require_admin(&env)?;
        if storage::is_forbidden_admin(&env, &new_creator) {
            return Err(Error::InvalidRoleAddress);
        }
        config.creator = new_creator.clone();
        storage::save_config(&env, &config);
        events::role_updated(&env, "creator", new_creator);
        Ok(())
    }

    /// Replace the `revoker` role holder.
    pub fn set_revoker(env: Env, new_revoker: Address) -> Result<(), Error> {
        let mut config = Self::require_admin(&env)?;
        if storage::is_forbidden_admin(&env, &new_revoker) {
            return Err(Error::InvalidRoleAddress);
        }
        config.revoker = new_revoker.clone();
        storage::save_config(&env, &config);
        events::role_updated(&env, "revoker", new_revoker);
        Ok(())
    }

    /// Replace the `pauser` role holder.
    pub fn set_pauser(env: Env, new_pauser: Address) -> Result<(), Error> {
        let mut config = Self::require_admin(&env)?;
        if storage::is_forbidden_admin(&env, &new_pauser) {
            return Err(Error::InvalidRoleAddress);
        }
        config.pauser = new_pauser.clone();
        storage::save_config(&env, &config);
        events::role_updated(&env, "pauser", new_pauser);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Admin rotation — admin only
    // -----------------------------------------------------------------------

    /// Rotate the admin address (single-step).
    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        let mut config = Self::require_admin(&env)?;
        if storage::is_forbidden_admin(&env, &new_admin) {
            return Err(Error::InvalidRoleAddress);
        }
        config.admin = new_admin;
        storage::save_config(&env, &config);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        storage::extend_instance(&env);
        Ok(())
    }

    /// Nominate a successor admin for two-step rotation.
    pub fn propose_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        let config = Self::require_admin(&env)?;
        if storage::is_forbidden_admin(&env, &new_admin) {
            return Err(Error::InvalidRoleAddress);
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

        events::AdminTransferred {
            previous,
            new: config.admin,
        }
        .publish(&env);
        Ok(())
    }

    /// Return the pending admin nominee, if any.
    pub fn pending_admin(env: Env) -> Result<Option<Address>, Error> {
        storage::load_config(&env)?;
        Ok(env.storage().instance().get(&DataKey::PendingAdmin))
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Validate the common creation parameters.
    ///
    /// Issue #404: called once per schedule in `batch_create`'s pre-flight pass,
    /// operating only on scalar values already deserialized from the input
    /// vectors — no storage access.
    fn validate_schedule_params(
        env: &Env,
        grantor: &Address,
        beneficiary: &Address,
        deposit: i128,
        start_time: u64,
        end_time: u64,
        cliff_time: u64,
    ) -> Result<(), Error> {
        let _ = env; // env may be used for future chain-state checks
        if grantor == beneficiary {
            return Err(Error::SelfVest);
        }
        if deposit <= 0 {
            return Err(Error::InvalidDeposit);
        }
        if end_time <= start_time {
            return Err(Error::InvalidTimeRange);
        }
        if cliff_time < start_time || cliff_time > end_time {
            return Err(Error::InvalidCliff);
        }
        let total_duration = end_time - start_time;
        if deposit < total_duration as i128 {
            return Err(Error::DepositTooSmall);
        }
        // Front-load overflow check: `deposit * elapsed` can never overflow for
        // the life of this schedule if this product fits in i128.
        deposit
            .checked_mul(total_duration as i128)
            .ok_or(Error::Overflow)?;
        Ok(())
    }

    /// Update `schedule.withdrawn`, set `Depleted` if fully claimed, and
    /// transfer `payout` tokens to the beneficiary.
    ///
    /// CEI: storage written before token transfer.
    fn apply_withdrawal(
        env: &Env,
        schedule_id: u64,
        schedule: &mut VestingSchedule,
        payout: i128,
    ) -> Result<(), Error> {
        schedule.withdrawn = schedule
            .withdrawn
            .checked_add(payout)
            .ok_or(Error::Overflow)?;

        if schedule.withdrawn >= schedule.deposited {
            schedule.status = ScheduleStatus::Depleted;
            schedule.paused_at = None;
        }

        let token = schedule.token.clone();
        let beneficiary = schedule.beneficiary.clone();

        storage::save_schedule(env, schedule_id, schedule);

        token_transfer(
            env,
            &token,
            &env.current_contract_address(),
            MuxedAddress::from(beneficiary),
            &payout,
        )?;

        events::withdrawn(env, schedule_id, schedule, payout);
        Ok(())
    }

    /// Reject a Vec of schedule IDs that contains duplicates.
    ///
    /// Uses an O(n²) comparison loop — acceptable for `MAX_BATCH_SIZE = 16`.
    fn reject_duplicate_ids(ids: &Vec<u64>) -> Result<(), Error> {
        let n = ids.len();
        for i in 0..n {
            for j in (i + 1)..n {
                if ids.get_unchecked(i) == ids.get_unchecked(j) {
                    return Err(Error::DuplicateScheduleId);
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod test {
    mod batch;
    mod common;
    mod create;
    mod fractional;
    mod fuzz;
    mod pause;
    mod revoke;
    mod roles;
    mod ttl;
    mod withdraw;
}
