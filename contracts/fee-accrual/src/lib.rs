#![no_std]
//! # Perpetua fee-accrual
//!
//! Dedicated fee-accrual subsystem for protocol and keeper fee accounting in Perpetua.
//! Tracks protocol fee accrual, keeper fee splits, and fee distribution balances
//! with defensive checked arithmetic, modularized error types, and granular
//! role-based authorization to prevent unauthorized configuration updates.

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

pub mod error;
pub mod events;
pub mod math;
pub mod storage;
pub mod types;

pub use error::FeeError;
pub use math::{compute_fee_breakdown, BPS_DENOMINATOR};
pub use storage::{
    BUMP_LEDGERS_PERSISTENT, MIN_TTL_LEDGERS_PERSISTENT,
};
pub use types::{
    AccruedFeeBreakdown, BatchAccrueItem, DataKey, FeeConfig, StreamFeeRecord,
};

use soroban_sdk::{
    contract, contractimpl, Address, Env, Symbol, Vec,
};

/// Maximum batch items allowed in a single batch accrual or claim call.
pub const MAX_BATCH_SIZE: u32 = 32;

#[contract]
pub struct FeeAccrual;

#[contractimpl]
impl FeeAccrual {
    // -----------------------------------------------------------------------
    // Initialization
    // -----------------------------------------------------------------------

    /// Initialize the fee-accrual contract with designated authorities and fee parameters.
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

        if protocol_fee_bps > 10_000 || keeper_split_bps > 10_000 {
            return Err(FeeError::InvalidBpsRate);
        }
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

        let config = FeeConfig {
            admin,
            fee_admin,
            fee_collector,
            emergency_admin,
            fee_recipient,
            protocol_fee_bps,
            keeper_split_bps,
            paused: false,
        };

        storage::save_config(&env, &config);
        storage::set_total_protocol_fees(&env, 0);
        storage::set_total_keeper_fees(&env, 0);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Fee Accrual
    // -----------------------------------------------------------------------

    /// Accrue fees for a specific stream processed amount.
    /// Gated to `fee_collector` role.
    pub fn accrue_fee(
        env: Env,
        caller: Address,
        stream_id: u64,
        gross_amount: i128,
    ) -> Result<AccruedFeeBreakdown, FeeError> {
        caller.require_auth();
        let config = storage::load_config(&env)?;

        if config.paused {
            return Err(FeeError::ContractPaused);
        }
        if caller != config.fee_collector {
            return Err(FeeError::UnauthorizedRole);
        }
        if stream_id == 0 {
            return Err(FeeError::InvalidStreamId);
        }
        if gross_amount <= 0 {
            return Err(FeeError::ArithmeticOverflow);
        }

        let mut record = storage::get_stream_fee(&env, stream_id).unwrap_or(StreamFeeRecord {
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

        let breakdown = compute_fee_breakdown(
            gross_amount,
            config.protocol_fee_bps,
            config.keeper_split_bps,
        )?;

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

        storage::set_stream_fee(&env, &record);

        let total_proto = storage::get_total_protocol_fees(&env)
            .checked_add(breakdown.protocol_fee)
            .ok_or(FeeError::ArithmeticOverflow)?;
        let total_keeper = storage::get_total_keeper_fees(&env)
            .checked_add(breakdown.keeper_fee)
            .ok_or(FeeError::ArithmeticOverflow)?;

        storage::set_total_protocol_fees(&env, total_proto);
        storage::set_total_keeper_fees(&env, total_keeper);

        events::emit_fee_accrued(
            &env,
            stream_id,
            gross_amount,
            breakdown.protocol_fee,
            breakdown.keeper_fee,
            breakdown.net_amount,
        );

        Ok(breakdown)
    }

    /// Batch accrue fees across multiple streams in a single call.
    pub fn batch_accrue_fee(
        env: Env,
        caller: Address,
        items: Vec<BatchAccrueItem>,
    ) -> Result<i128, FeeError> {
        caller.require_auth();
        let config = storage::load_config(&env)?;

        if config.paused {
            return Err(FeeError::ContractPaused);
        }
        if caller != config.fee_collector {
            return Err(FeeError::UnauthorizedRole);
        }
        let len = items.len();
        if len == 0 {
            return Err(FeeError::EmptyBatch);
        }
        if len > MAX_BATCH_SIZE {
            return Err(FeeError::BatchTooLarge);
        }

        let mut total_batch_fees: i128 = 0;

        for item in items.iter() {
            let breakdown = Self::accrue_fee(
                env.clone(),
                caller.clone(),
                item.stream_id,
                item.amount,
            )?;
            total_batch_fees = total_batch_fees
                .checked_add(breakdown.total_fee)
                .ok_or(FeeError::ArithmeticOverflow)?;
        }

        Ok(total_batch_fees)
    }

    /// Claim accrued protocol or keeper fees.
    pub fn claim_stream_fee(
        env: Env,
        caller: Address,
        stream_id: u64,
        is_protocol_fee: bool,
    ) -> Result<i128, FeeError> {
        caller.require_auth();
        let config = storage::load_config(&env)?;

        if config.paused {
            return Err(FeeError::ContractPaused);
        }

        let mut record = storage::get_stream_fee(&env, stream_id)
            .ok_or(FeeError::FeeRecordNotFound)?;

        let claim_amount = if is_protocol_fee {
            if caller != config.admin && caller != config.fee_admin && caller != config.fee_recipient {
                return Err(FeeError::UnauthorizedRole);
            }
            let uncollected = record
                .protocol_fee_accrued
                .checked_sub(record.protocol_fee_claimed)
                .ok_or(FeeError::ArithmeticOverflow)?;
            if uncollected <= 0 {
                return Err(FeeError::ZeroFeeAccrual);
            }
            record.protocol_fee_claimed = record
                .protocol_fee_claimed
                .checked_add(uncollected)
                .ok_or(FeeError::ArithmeticOverflow)?;
            uncollected
        } else {
            let uncollected = record
                .keeper_fee_accrued
                .checked_sub(record.keeper_fee_claimed)
                .ok_or(FeeError::ArithmeticOverflow)?;
            if uncollected <= 0 {
                return Err(FeeError::ZeroFeeAccrual);
            }
            record.keeper_fee_claimed = record
                .keeper_fee_claimed
                .checked_add(uncollected)
                .ok_or(FeeError::ArithmeticOverflow)?;
            uncollected
        };

        storage::set_stream_fee(&env, &record);

        let proto_claimed = if is_protocol_fee { claim_amount } else { 0 };
        let keeper_claimed = if is_protocol_fee { 0 } else { claim_amount };

        events::emit_fee_claimed(
            &env,
            stream_id,
            &caller,
            proto_claimed,
            keeper_claimed,
        );

        Ok(claim_amount)
    }

    /// Settle stream fees, finalizing accounting for the stream.
    pub fn settle_stream_fee(
        env: Env,
        caller: Address,
        stream_id: u64,
    ) -> Result<(), FeeError> {
        caller.require_auth();
        let config = storage::load_config(&env)?;

        if caller != config.fee_collector && caller != config.fee_admin && caller != config.admin {
            return Err(FeeError::UnauthorizedRole);
        }

        let mut record = storage::get_stream_fee(&env, stream_id)
            .ok_or(FeeError::FeeRecordNotFound)?;

        if record.is_settled {
            return Err(FeeError::FeeAlreadySettled);
        }

        record.is_settled = true;
        storage::set_stream_fee(&env, &record);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Role Configuration Setters
    // -----------------------------------------------------------------------

    /// Update fee rates and recipient. Gated to `fee_admin`.
    pub fn update_fee_config(
        env: Env,
        protocol_fee_bps: u32,
        keeper_split_bps: u32,
        fee_recipient: Address,
    ) -> Result<(), FeeError> {
        let mut config = storage::load_config(&env)?;
        config.fee_admin.require_auth();

        if protocol_fee_bps > 10_000 || keeper_split_bps > 10_000 {
            return Err(FeeError::InvalidBpsRate);
        }
        if storage::is_zero_address(&env, &fee_recipient) {
            return Err(FeeError::InvalidFeeRecipient);
        }

        config.protocol_fee_bps = protocol_fee_bps;
        config.keeper_split_bps = keeper_split_bps;
        config.fee_recipient = fee_recipient.clone();

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

    /// Update specific role authority. Gated to `admin`.
    pub fn set_role(
        env: Env,
        role: Symbol,
        new_holder: Address,
    ) -> Result<(), FeeError> {
        let mut config = storage::load_config(&env)?;
        config.admin.require_auth();

        if storage::is_forbidden_admin(&env, &new_holder) {
            return Err(FeeError::InvalidRoleAddress);
        }

        let fee_admin_sym = Symbol::new(&env, "fee_admin");
        let fee_collector_sym = Symbol::new(&env, "fee_collector");
        let emergency_admin_sym = Symbol::new(&env, "emergency_admin");

        let old_holder = if role == fee_admin_sym {
            let old = config.fee_admin.clone();
            config.fee_admin = new_holder.clone();
            old
        } else if role == fee_collector_sym {
            let old = config.fee_collector.clone();
            config.fee_collector = new_holder.clone();
            old
        } else if role == emergency_admin_sym {
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

    /// Emergency pause switch. Gated to `emergency_admin` or `admin`.
    pub fn set_paused(env: Env, paused: bool) -> Result<(), FeeError> {
        let mut config = storage::load_config(&env)?;

        // Either emergency_admin or admin can toggle pause
        if !storage::is_forbidden_admin(&env, &config.emergency_admin) {
            config.emergency_admin.require_auth();
        } else {
            config.admin.require_auth();
        }

        if config.paused == paused {
            return if paused {
                Err(FeeError::ContractPaused)
            } else {
                Err(FeeError::ContractNotPaused)
            };
        }

        config.paused = paused;
        storage::save_config(&env, &config);

        events::emit_pause_status(&env, paused, &config.emergency_admin);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Admin Two-Step Rotation
    // -----------------------------------------------------------------------

    /// Propose a new admin. Gated to `admin`.
    pub fn propose_admin(env: Env, nominee: Address) -> Result<(), FeeError> {
        let config = storage::load_config(&env)?;
        config.admin.require_auth();

        if storage::is_forbidden_admin(&env, &nominee) {
            return Err(FeeError::InvalidNewAdmin);
        }

        storage::set_pending_admin(&env, &nominee);
        events::emit_admin_proposed(&env, &config.admin, &nominee);

        Ok(())
    }

    /// Accept admin nomination. Gated to nominated address.
    pub fn accept_admin(env: Env) -> Result<(), FeeError> {
        let nominee = storage::get_pending_admin(&env)
            .ok_or(FeeError::NoPendingAdmin)?;
        nominee.require_auth();

        let mut config = storage::load_config(&env)?;
        let old_admin = config.admin.clone();
        config.admin = nominee.clone();

        storage::save_config(&env, &config);
        storage::clear_pending_admin(&env);

        events::emit_admin_transferred(&env, &old_admin, &nominee);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Views
    // -----------------------------------------------------------------------

    pub fn get_config(env: Env) -> Result<FeeConfig, FeeError> {
        storage::load_config(&env)
    }

    pub fn get_stream_fee(env: Env, stream_id: u64) -> Option<StreamFeeRecord> {
        storage::get_stream_fee(&env, stream_id)
    }

    pub fn total_protocol_fees(env: Env) -> i128 {
        storage::get_total_protocol_fees(&env)
    }

    pub fn total_keeper_fees(env: Env) -> i128 {
        storage::get_total_keeper_fees(&env)
    }

    pub fn pending_admin(env: Env) -> Option<Address> {
        storage::get_pending_admin(&env)
    }
}

#[cfg(test)]
mod test;
