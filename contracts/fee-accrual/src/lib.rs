#![no_std]
//! # Perpetua Fee Accrual Contract
//!
//! Dedicated fee-accrual subsystem for protocol and keeper fee accounting in Perpetua.
//! Tracks protocol fee accrual, keeper fee splits, and fee distribution balances
//! with defensive checked arithmetic, modularized error types, granular
//! role-based authorization, re-entrancy protection, and strict invariant assertions.
//!
//! ## Issue #388 — Strict Invariant Assertions & Re-entrancy Protection
//!
//! This contract enforces strict mathematical and state conservation invariants
//! on all paths to protect against re-entrancy and state desync:
//!
//! 1. **Re-entrancy Protection**: State-mutating methods (`accrue_fee`, `batch_accrue_fee`,
//!    `claim_protocol_fees`, `claim_keeper_fees`) use an explicit `Executing` lock flag in
//!    instance storage following Checks-Effects-Interactions (CEI). Any recursive or concurrent
//!    invocation fails with [`FeeError::ReentrancyGuard`].
//! 2. **Conservation of Balance**: Fee splits satisfy `protocol_fee + keeper_fee == total_fee`
//!    and `net_amount + total_fee == gross_amount`.
//! 3. **Monotonicity & Solvency**: Accrued balances are non-decreasing; claims never exceed
//!    cumulative accruals (`protocol_fee_claimed <= protocol_fee_accrued`).
//! 4. **Batch Ledger Footprint**: Batch operations are bounded by [`MAX_BATCH_SIZE`] (32 items)
//!    to prevent host CPU and ledger footprint metering exhaustion.
//!
//! ## Granular Role-based Authorization (Issue #385)
//!
//! - `admin`: Administrative authority (governs roles, admin rotation).
//! - `fee_admin`: Configures protocol fee bps, keeper split bps, and fee recipient.
//! - `fee_collector`: Authorized to record accruals (called by stream contract).
//! - `emergency_admin`: Emergency pause switch for operations.

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

pub mod error;
pub mod events;
pub mod invariants;
pub mod math;
pub mod storage;
pub mod types;

#[cfg(test)]
mod test;

pub use error::FeeError;
pub use invariants::MAX_BATCH_SIZE;
pub use types::{AccruedFeeBreakdown, BatchAccrueItem, DataKey, FeeConfig, StreamFeeRecord};

use soroban_sdk::{contract, contractimpl, symbol_short, Address, Env, Symbol, Vec};

#[contract]
pub struct FeeAccrualContract;

#[contractimpl]
impl FeeAccrualContract {
    // -----------------------------------------------------------------------
    // Initialization
    // -----------------------------------------------------------------------

    /// Initialize the fee-accrual contract with designated role authorities and parameters.
    pub fn init(
        env: Env,
        admin: Address,
        fee_admin: Address,
        fee_collector: Address,
        emergency_admin: Address,
        fee_recipient: Address,
        protocol_fee_bps: u32,
        keeper_split_bps: u32,
    ) -> Result<(), FeeError> {
        if env.storage().instance().has(&DataKey::Config) {
            return Err(FeeError::AlreadyInitialized);
        }
        admin.require_auth();

        if storage::is_forbidden_admin(&env, &admin)
            || storage::is_forbidden_admin(&env, &fee_admin)
            || storage::is_forbidden_admin(&env, &fee_collector)
            || storage::is_forbidden_admin(&env, &emergency_admin)
        {
            return Err(FeeError::InvalidRoleAddress);
        }
        if storage::is_zero_address(&env, &fee_recipient) {
            return Err(FeeError::InvalidFeeRecipient);
        }
        if protocol_fee_bps > 10_000 || keeper_split_bps > 10_000 {
            return Err(FeeError::InvalidBpsRate);
        }

        let config = FeeConfig {
            admin,
            fee_admin: fee_admin.clone(),
            fee_collector,
            emergency_admin,
            fee_recipient: fee_recipient.clone(),
            protocol_fee_bps,
            keeper_split_bps,
            paused: false,
        };

        storage::save_config(&env, &config);
        storage::set_total_protocol_fees(&env, 0);
        storage::set_total_keeper_fees(&env, 0);

        events::emit_config_updated(
            &env,
            &fee_admin,
            &fee_recipient,
            protocol_fee_bps,
            keeper_split_bps,
        );

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Configuration & Role Management (Issue #385)
    // -----------------------------------------------------------------------

    /// Update fee rates and recipient. Gated by `fee_admin`.
    pub fn update_fee_config(
        env: Env,
        caller: Address,
        fee_recipient: Address,
        protocol_fee_bps: u32,
        keeper_split_bps: u32,
    ) -> Result<(), FeeError> {
        caller.require_auth();
        let mut config = storage::load_config(&env)?;

        if caller != config.fee_admin && caller != config.admin {
            return Err(FeeError::UnauthorizedRole);
        }
        if storage::is_zero_address(&env, &fee_recipient) {
            return Err(FeeError::InvalidFeeRecipient);
        }
        if protocol_fee_bps > 10_000 || keeper_split_bps > 10_000 {
            return Err(FeeError::InvalidBpsRate);
        }

        config.fee_recipient = fee_recipient.clone();
        config.protocol_fee_bps = protocol_fee_bps;
        config.keeper_split_bps = keeper_split_bps;

        invariants::assert_valid_config(&config)?;
        storage::save_config(&env, &config);

        events::emit_config_updated(
            &env,
            &config.fee_admin,
            &fee_recipient,
            protocol_fee_bps,
            keeper_split_bps,
        );

        Ok(())
    }

    /// Update assigned role address. Gated by `admin`.
    pub fn set_role(
        env: Env,
        caller: Address,
        role: Symbol,
        new_holder: Address,
    ) -> Result<(), FeeError> {
        caller.require_auth();
        let mut config = storage::load_config(&env)?;

        if caller != config.admin {
            return Err(FeeError::Unauthorized);
        }
        if storage::is_forbidden_admin(&env, &new_holder) {
            return Err(FeeError::InvalidRoleAddress);
        }

        let old_holder = if role == Symbol::new(&env, "fee_admin") {
            let old = config.fee_admin.clone();
            config.fee_admin = new_holder.clone();
            old
        } else if role == Symbol::new(&env, "fee_collector") {
            let old = config.fee_collector.clone();
            config.fee_collector = new_holder.clone();
            old
        } else if role == Symbol::new(&env, "emergency_admin") {
            let old = config.emergency_admin.clone();
            config.emergency_admin = new_holder.clone();
            old
        } else {
            return Err(FeeError::UnauthorizedRole);
        };

        storage::save_config(&env, &config);
        events::emit_role_updated(&env, role, &old_holder, &new_holder);

        Ok(())
    }

    /// Two-step admin transfer: propose next admin.
    pub fn propose_admin(env: Env, caller: Address, nominee: Address) -> Result<(), FeeError> {
        caller.require_auth();
        let config = storage::load_config(&env)?;

        if caller != config.admin {
            return Err(FeeError::Unauthorized);
        }
        if storage::is_forbidden_admin(&env, &nominee) {
            return Err(FeeError::InvalidNewAdmin);
        }

        storage::set_pending_admin(&env, &nominee);
        events::emit_admin_proposed(&env, &caller, &nominee);

        Ok(())
    }

    /// Two-step admin transfer: accept proposed admin.
    pub fn accept_admin(env: Env, caller: Address) -> Result<(), FeeError> {
        caller.require_auth();
        let mut config = storage::load_config(&env)?;

        let pending = storage::get_pending_admin(&env).ok_or(FeeError::NoPendingAdmin)?;
        if caller != pending {
            return Err(FeeError::Unauthorized);
        }

        let old_admin = config.admin.clone();
        config.admin = caller.clone();

        storage::save_config(&env, &config);
        storage::clear_pending_admin(&env);

        events::emit_admin_transferred(&env, &old_admin, &caller);

        Ok(())
    }

    /// Emergency pause switch. Gated by `emergency_admin` or `admin`.
    pub fn set_paused(env: Env, caller: Address, paused: bool) -> Result<(), FeeError> {
        caller.require_auth();
        let mut config = storage::load_config(&env)?;

        if caller != config.emergency_admin && caller != config.admin {
            return Err(FeeError::UnauthorizedRole);
        }
        if config.paused == paused {
            if paused {
                return Err(FeeError::ContractPaused);
            } else {
                return Err(FeeError::ContractNotPaused);
            }
        }

        config.paused = paused;
        storage::save_config(&env, &config);
        events::emit_pause_status(&env, paused, &caller);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Fee Accrual (Issue #388: Re-entrancy Guard & Invariant Assertions)
    // -----------------------------------------------------------------------

    /// Accrue protocol and keeper fees for a stream. Gated by `fee_collector`.
    /// Guarded against re-entrancy and state desync via explicit `Executing` lock flag.
    pub fn accrue_fee(
        env: Env,
        caller: Address,
        stream_id: u64,
        gross_amount: i128,
    ) -> Result<AccruedFeeBreakdown, FeeError> {
        caller.require_auth();

        if storage::is_executing(&env) {
            return Err(FeeError::ReentrancyGuard);
        }
        storage::set_executing(&env);

        let res = Self::accrue_fee_inner(&env, &caller, stream_id, gross_amount);

        storage::clear_executing(&env);
        res
    }

    fn accrue_fee_inner(
        env: &Env,
        caller: &Address,
        stream_id: u64,
        gross_amount: i128,
    ) -> Result<AccruedFeeBreakdown, FeeError> {
        let config = storage::load_config(env)?;
        if config.paused {
            return Err(FeeError::ContractPaused);
        }
        if *caller != config.fee_collector && *caller != config.admin {
            return Err(FeeError::CallerNotAuthorized);
        }
        if stream_id == 0 {
            return Err(FeeError::InvalidStreamId);
        }
        if gross_amount < 0 {
            return Err(FeeError::ArithmeticOverflow);
        }

        let breakdown = math::compute_fee_breakdown(
            gross_amount,
            config.protocol_fee_bps,
            config.keeper_split_bps,
        )?;
        invariants::assert_breakdown_conservation(&breakdown)?;

        let mut record = storage::get_stream_fee(env, stream_id).unwrap_or(StreamFeeRecord {
            stream_id,
            total_processed: 0,
            protocol_fee_accrued: 0,
            keeper_fee_accrued: 0,
            protocol_fee_claimed: 0,
            keeper_fee_claimed: 0,
            is_settled: false,
        });

        if record.is_settled {
            return Err(FeeError::FeeAlreadySettled);
        }

        let old_record = record.clone();

        record.total_processed = record
            .total_processed
            .checked_add(gross_amount)
            .ok_or(FeeError::ArithmeticOverflow)?;
        record.protocol_fee_accrued = record
            .protocol_fee_accrued
            .checked_add(breakdown.protocol_fee)
            .ok_or(FeeError::ArithmeticOverflow)?;
        record.keeper_fee_accrued = record
            .keeper_fee_accrued
            .checked_add(breakdown.keeper_fee)
            .ok_or(FeeError::ArithmeticOverflow)?;

        // Enforce invariants
        invariants::assert_record_solvency(&record)?;
        invariants::assert_accrual_monotonicity(&old_record, &record)?;

        storage::set_stream_fee(env, &record);

        // Update global totals with checked arithmetic
        let total_proto = storage::get_total_protocol_fees(env);
        let total_keeper = storage::get_total_keeper_fees(env);

        let new_total_proto = total_proto
            .checked_add(breakdown.protocol_fee)
            .ok_or(FeeError::ArithmeticOverflow)?;
        let new_total_keeper = total_keeper
            .checked_add(breakdown.keeper_fee)
            .ok_or(FeeError::ArithmeticOverflow)?;

        storage::set_total_protocol_fees(env, new_total_proto);
        storage::set_total_keeper_fees(env, new_total_keeper);

        events::emit_fee_accrued(
            env,
            stream_id,
            breakdown.gross_amount,
            breakdown.protocol_fee,
            breakdown.keeper_fee,
            breakdown.net_amount,
        );

        Ok(breakdown)
    }

    /// Batch accrue fees across multiple streams in a single atomic transaction.
    /// Strictly limits batch size to MAX_BATCH_SIZE to prevent host metering regression.
    pub fn batch_accrue_fee(
        env: Env,
        caller: Address,
        items: Vec<BatchAccrueItem>,
    ) -> Result<Vec<AccruedFeeBreakdown>, FeeError> {
        caller.require_auth();

        if storage::is_executing(&env) {
            return Err(FeeError::ReentrancyGuard);
        }
        storage::set_executing(&env);

        let res = Self::batch_accrue_fee_inner(&env, &caller, items);

        storage::clear_executing(&env);
        res
    }

    fn batch_accrue_fee_inner(
        env: &Env,
        caller: &Address,
        items: Vec<BatchAccrueItem>,
    ) -> Result<Vec<AccruedFeeBreakdown>, FeeError> {
        invariants::assert_batch_bounds(items.len())?;

        let mut breakdowns = Vec::new(env);
        for item in items.iter() {
            let b = Self::accrue_fee_inner(env, caller, item.stream_id, item.amount)?;
            breakdowns.push_back(b);
        }

        Ok(breakdowns)
    }

    // -----------------------------------------------------------------------
    // Fee Claiming & Distribution
    // -----------------------------------------------------------------------

    /// Claim accrued protocol fees for a specific stream. Gated by `fee_admin` or `admin`.
    pub fn claim_protocol_fees(
        env: Env,
        caller: Address,
        stream_id: u64,
        amount: i128,
    ) -> Result<i128, FeeError> {
        caller.require_auth();

        if storage::is_executing(&env) {
            return Err(FeeError::ReentrancyGuard);
        }
        storage::set_executing(&env);

        let res = Self::claim_protocol_fees_inner(&env, &caller, stream_id, amount);

        storage::clear_executing(&env);
        res
    }

    fn claim_protocol_fees_inner(
        env: &Env,
        caller: &Address,
        stream_id: u64,
        amount: i128,
    ) -> Result<i128, FeeError> {
        let config = storage::load_config(env)?;
        if config.paused {
            return Err(FeeError::ContractPaused);
        }
        if *caller != config.fee_admin && *caller != config.admin {
            return Err(FeeError::UnauthorizedRole);
        }
        if stream_id == 0 {
            return Err(FeeError::InvalidStreamId);
        }
        if amount <= 0 {
            return Err(FeeError::ZeroFeeAccrual);
        }

        let mut record =
            storage::get_stream_fee(env, stream_id).ok_or(FeeError::FeeRecordNotFound)?;

        let available = record
            .protocol_fee_accrued
            .checked_sub(record.protocol_fee_claimed)
            .ok_or(FeeError::ArithmeticOverflow)?;

        if amount > available {
            return Err(FeeError::InsufficientAccruedFees);
        }

        let old_record = record.clone();
        record.protocol_fee_claimed = record
            .protocol_fee_claimed
            .checked_add(amount)
            .ok_or(FeeError::ArithmeticOverflow)?;

        invariants::assert_record_solvency(&record)?;
        invariants::assert_accrual_monotonicity(&old_record, &record)?;

        storage::set_stream_fee(env, &record);

        events::emit_fee_claimed(env, stream_id, &config.fee_recipient, amount, 0);

        Ok(amount)
    }

    /// Claim accrued keeper fees for a specific stream and disburse to recipient.
    pub fn claim_keeper_fees(
        env: Env,
        caller: Address,
        stream_id: u64,
        recipient: Address,
        amount: i128,
    ) -> Result<i128, FeeError> {
        caller.require_auth();

        if storage::is_executing(&env) {
            return Err(FeeError::ReentrancyGuard);
        }
        storage::set_executing(&env);

        let res = Self::claim_keeper_fees_inner(&env, &caller, stream_id, &recipient, amount);

        storage::clear_executing(&env);
        res
    }

    fn claim_keeper_fees_inner(
        env: &Env,
        caller: &Address,
        stream_id: u64,
        recipient: &Address,
        amount: i128,
    ) -> Result<i128, FeeError> {
        let config = storage::load_config(env)?;
        if config.paused {
            return Err(FeeError::ContractPaused);
        }
        if *caller != config.fee_collector && *caller != config.admin {
            return Err(FeeError::CallerNotAuthorized);
        }
        if stream_id == 0 {
            return Err(FeeError::InvalidStreamId);
        }
        if storage::is_zero_address(env, recipient) {
            return Err(FeeError::InvalidFeeRecipient);
        }
        if amount <= 0 {
            return Err(FeeError::ZeroFeeAccrual);
        }

        let mut record =
            storage::get_stream_fee(env, stream_id).ok_or(FeeError::FeeRecordNotFound)?;

        let available = record
            .keeper_fee_accrued
            .checked_sub(record.keeper_fee_claimed)
            .ok_or(FeeError::ArithmeticOverflow)?;

        if amount > available {
            return Err(FeeError::InsufficientAccruedFees);
        }

        let old_record = record.clone();
        record.keeper_fee_claimed = record
            .keeper_fee_claimed
            .checked_add(amount)
            .ok_or(FeeError::ArithmeticOverflow)?;

        invariants::assert_record_solvency(&record)?;
        invariants::assert_accrual_monotonicity(&old_record, &record)?;

        storage::set_stream_fee(env, &record);

        events::emit_fee_claimed(env, stream_id, recipient, 0, amount);

        Ok(amount)
    }

    /// Mark a stream fee record as settled (terminal state).
    pub fn settle_stream(env: Env, caller: Address, stream_id: u64) -> Result<(), FeeError> {
        caller.require_auth();
        let config = storage::load_config(&env)?;

        if caller != config.admin && caller != config.fee_admin {
            return Err(FeeError::UnauthorizedRole);
        }

        let mut record =
            storage::get_stream_fee(&env, stream_id).ok_or(FeeError::FeeRecordNotFound)?;

        if record.is_settled {
            return Err(FeeError::FeeAlreadySettled);
        }

        record.is_settled = true;
        invariants::assert_record_solvency(&record)?;
        storage::set_stream_fee(&env, &record);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // View / Query Functions
    // -----------------------------------------------------------------------

    pub fn get_config(env: Env) -> Result<FeeConfig, FeeError> {
        storage::load_config(&env)
    }

    pub fn get_stream_fee_record(env: Env, stream_id: u64) -> Result<StreamFeeRecord, FeeError> {
        storage::get_stream_fee(&env, stream_id).ok_or(FeeError::FeeRecordNotFound)
    }

    pub fn get_total_fees(env: Env) -> (i128, i128) {
        (
            storage::get_total_protocol_fees(&env),
            storage::get_total_keeper_fees(&env),
        )
    }
}
