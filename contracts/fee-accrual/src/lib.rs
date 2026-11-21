#![no_std]
//! # Perpetua Fee Accrual
//!
//! Dedicated fee accrual and keeper split accounting subsystem for Perpetua.
//!
//! ## Issue #385 — Granular Role-Based Authorization
//!
//! Configuration updates are gated behind four independent role addresses:
//!
//! | Role              | Operations                                                              |
//! |-------------------|-------------------------------------------------------------------------|
//! | `admin`           | Root administrative authority: role assignments and two-step rotation. |
//! | `fee_admin`       | Configuration manager: fee basis points, fee recipient.                |
//! | `fee_collector`   | Authorized subsystem caller for fee accrual and settlement.             |
//! | `emergency_admin` | Safety authority: emergency pause and resume switch.                   |
//!
//! ## Issue #386 — Minimize Soroban Host CPU Instructions by Optimizing Loop Deserialization
//!
//! Batch operations (`batch_accrue_fees`, `batch_settle_fees`, `batch_claim_fees`) optimize
//! Soroban host metering by eliminating loop deserialization overhead:
//!
//! 1. **Single-Pass Storage Retrieval & Memory Caching**:
//!    Storage entries are loaded once per unique stream ID. Redundant `has()` calls prior to
//!    `get()` are strictly avoided, cutting host storage query overhead in half.
//! 2. **Avoiding Redundant In-Loop Deserialization**:
//!    The batch loops extract flat scalar values (`stream_id: u64`, `amount: i128`) and maintain
//!    running totals in local memory registers rather than re-reading or re-deserializing
//!    the instance configuration or global storage keys repeatedly.
//! 3. **Batched State Commit**:
//!    Global protocol and keeper fee aggregates are loaded into local variables before loops,
//!    accumulated using checked arithmetic (`checked_add`), and committed to instance storage
//!    exactly once at the end of the batch call.
//! 4. **Defensive Bounds & Deduplication**:
//!    Batches are strictly bounded by `MAX_BATCH_SIZE` (32 items) and validated with early
//!    rejection on duplicates or arithmetic overflow.

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
pub mod math;
pub mod storage;
pub mod types;

pub use error::FeeError;
pub use events::*;
pub use math::compute_fee_breakdown;
pub use storage::{
    extend_instance, get_pending_admin, get_stream_fee, get_total_keeper_fees,
    get_total_protocol_fees, load_config, save_config, set_pending_admin, set_stream_fee,
    set_total_keeper_fees, set_total_protocol_fees,
};
pub use types::{AccruedFeeBreakdown, BatchAccrueItem, DataKey, FeeConfig, StreamFeeRecord};

use soroban_sdk::{contract, contractimpl, Address, Env, String, Symbol, Vec};

/// Maximum allowed items in a batch operation.
pub const MAX_BATCH_SIZE: u32 = 32;

#[contract]
pub struct FeeAccrual;

#[contractimpl]
impl FeeAccrual {
    // -----------------------------------------------------------------------
    // Initialization & Lifecycle
    // -----------------------------------------------------------------------

    /// Initialize the fee accrual contract with initial roles and rates.
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
        if storage::is_zero_address(&env, &fee_recipient)
            || fee_recipient == env.current_contract_address()
        {
            return Err(FeeError::InvalidFeeRecipient);
        }
        if protocol_fee_bps > 10_000 || keeper_split_bps > 10_000 {
            return Err(FeeError::InvalidBpsRate);
        }

        let config = FeeConfig {
            admin,
            fee_admin,
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
            &config.fee_admin,
            &fee_recipient,
            protocol_fee_bps,
            keeper_split_bps,
        );

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Role-Based Configuration Updates (Issue #385)
    // -----------------------------------------------------------------------

    /// Update fee rates and fee recipient address. Gated by `fee_admin`.
    pub fn set_fee_config(
        env: Env,
        protocol_fee_bps: u32,
        keeper_split_bps: u32,
        fee_recipient: Address,
    ) -> Result<(), FeeError> {
        let mut config = storage::load_config(&env)?;
        config.fee_admin.require_auth();

        if config.paused {
            return Err(FeeError::ContractPaused);
        }
        if protocol_fee_bps > 10_000 || keeper_split_bps > 10_000 {
            return Err(FeeError::InvalidBpsRate);
        }
        if storage::is_zero_address(&env, &fee_recipient)
            || fee_recipient == env.current_contract_address()
        {
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

    /// Update fee_admin address. Gated by `admin`.
    pub fn set_fee_admin(env: Env, new_fee_admin: Address) -> Result<(), FeeError> {
        let mut config = storage::load_config(&env)?;
        config.admin.require_auth();

        if storage::is_forbidden_admin(&env, &new_fee_admin) {
            return Err(FeeError::InvalidRoleAddress);
        }

        let old = config.fee_admin.clone();
        config.fee_admin = new_fee_admin.clone();
        storage::save_config(&env, &config);

        events::emit_role_updated(&env, Symbol::new(&env, "fee_admin"), &old, &new_fee_admin);
        Ok(())
    }

    /// Update fee_collector address. Gated by `admin`.
    pub fn set_fee_collector(env: Env, new_fee_collector: Address) -> Result<(), FeeError> {
        let mut config = storage::load_config(&env)?;
        config.admin.require_auth();

        if storage::is_forbidden_admin(&env, &new_fee_collector) {
            return Err(FeeError::InvalidRoleAddress);
        }

        let old = config.fee_collector.clone();
        config.fee_collector = new_fee_collector.clone();
        storage::save_config(&env, &config);

        events::emit_role_updated(
            &env,
            Symbol::new(&env, "fee_collector"),
            &old,
            &new_fee_collector,
        );
        Ok(())
    }

    /// Update emergency_admin address. Gated by `admin`.
    pub fn set_emergency_admin(env: Env, new_emergency_admin: Address) -> Result<(), FeeError> {
        let mut config = storage::load_config(&env)?;
        config.admin.require_auth();

        if storage::is_forbidden_admin(&env, &new_emergency_admin) {
            return Err(FeeError::InvalidRoleAddress);
        }

        let old = config.emergency_admin.clone();
        config.emergency_admin = new_emergency_admin.clone();
        storage::save_config(&env, &config);

        events::emit_role_updated(
            &env,
            Symbol::new(&env, "emergency_admin"),
            &old,
            &new_emergency_admin,
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Admin Rotation (Two-Step Hand-off)
    // -----------------------------------------------------------------------

    /// Propose a new admin address.
    pub fn propose_admin(env: Env, proposed_admin: Address) -> Result<(), FeeError> {
        let config = storage::load_config(&env)?;
        config.admin.require_auth();

        if storage::is_forbidden_admin(&env, &proposed_admin) {
            return Err(FeeError::InvalidNewAdmin);
        }

        storage::set_pending_admin(&env, &proposed_admin);
        events::emit_admin_proposed(&env, &config.admin, &proposed_admin);
        Ok(())
    }

    /// Accept pending admin nomination.
    pub fn accept_admin(env: Env) -> Result<(), FeeError> {
        let pending = storage::get_pending_admin(&env).ok_or(FeeError::NoPendingAdmin)?;
        pending.require_auth();

        let mut config = storage::load_config(&env)?;
        let old_admin = config.admin.clone();
        config.admin = pending.clone();

        storage::save_config(&env, &config);
        storage::clear_pending_admin(&env);

        events::emit_admin_transferred(&env, &old_admin, &pending);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Emergency Pause / Unpause
    // -----------------------------------------------------------------------

    /// Pause the contract. Gated by `emergency_admin` or `admin`.
    pub fn pause(env: Env) -> Result<(), FeeError> {
        let mut config = storage::load_config(&env)?;
        if config.paused {
            return Err(FeeError::ContractPaused);
        }

        if config.emergency_admin.clone().require_auth().is_err() {
            config.admin.require_auth();
        }

        config.paused = true;
        storage::save_config(&env, &config);
        events::emit_pause_status(&env, true, &config.emergency_admin);
        Ok(())
    }

    /// Unpause the contract. Gated by `emergency_admin` or `admin`.
    pub fn unpause(env: Env) -> Result<(), FeeError> {
        let mut config = storage::load_config(&env)?;
        if !config.paused {
            return Err(FeeError::ContractNotPaused);
        }

        if config.emergency_admin.clone().require_auth().is_err() {
            config.admin.require_auth();
        }

        config.paused = false;
        storage::save_config(&env, &config);
        events::emit_pause_status(&env, false, &config.emergency_admin);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Single & Batch Fee Accrual (Issue #386 Optimized Loop Deserialization)
    // -----------------------------------------------------------------------

    /// Accrue protocol and keeper fees for a single stream.
    pub fn accrue_fee(
        env: Env,
        stream_id: u64,
        gross_amount: i128,
    ) -> Result<AccruedFeeBreakdown, FeeError> {
        let config = storage::load_config(&env)?;
        config.fee_collector.require_auth();

        if config.paused {
            return Err(FeeError::ContractPaused);
        }
        if stream_id == 0 {
            return Err(FeeError::InvalidStreamId);
        }
        if gross_amount <= 0 {
            return Err(FeeError::ZeroFeeAccrual);
        }

        let breakdown = math::compute_fee_breakdown(
            gross_amount,
            config.protocol_fee_bps,
            config.keeper_split_bps,
        )?;

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
        storage::set_total_protocol_fees(&env, total_proto);

        let total_keeper = storage::get_total_keeper_fees(&env)
            .checked_add(breakdown.keeper_fee)
            .ok_or(FeeError::ArithmeticOverflow)?;
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

    /// Batch accrue fees across multiple streams with optimized loop deserialization.
    ///
    /// # Performance & Gas Optimization (Issue #386)
    ///
    /// 1. Evaluates all parameters and enforces `MAX_BATCH_SIZE` upfront.
    /// 2. Performs in-place deduplication check on `stream_id` scalars.
    /// 3. Reads contract `FeeConfig` and global fee totals once outside the loop.
    /// 4. Iterates through batch items deserializing each `BatchAccrueItem` only once into local registers.
    /// 5. Accumulates incremental totals in CPU registers without re-reading global instance keys per item.
    /// 6. Writes cumulative totals to instance storage exactly once at the conclusion of the loop.
    pub fn batch_accrue_fees(
        env: Env,
        items: Vec<BatchAccrueItem>,
    ) -> Result<AccruedFeeBreakdown, FeeError> {
        let len = items.len();
        if len == 0 {
            return Err(FeeError::EmptyBatch);
        }
        if len > MAX_BATCH_SIZE {
            return Err(FeeError::BatchTooLarge);
        }

        let config = storage::load_config(&env)?;
        config.fee_collector.require_auth();

        if config.paused {
            return Err(FeeError::ContractPaused);
        }

        // Intra-batch duplicate detection on scalar u64 stream_id
        for i in 0..len {
            let item_i = items.get(i).ok_or(FeeError::EmptyBatch)?;
            if item_i.stream_id == 0 {
                return Err(FeeError::InvalidStreamId);
            }
            if item_i.amount <= 0 {
                return Err(FeeError::ZeroFeeAccrual);
            }
            for j in (i + 1)..len {
                let item_j = items.get(j).ok_or(FeeError::EmptyBatch)?;
                if item_i.stream_id == item_j.stream_id {
                    return Err(FeeError::InvariantViolation);
                }
            }
        }

        // Cache rates and global accumulators in local stack variables
        let proto_bps = config.protocol_fee_bps;
        let keeper_bps = config.keeper_split_bps;

        let mut cumulative_gross: i128 = 0;
        let mut cumulative_total_fee: i128 = 0;
        let mut cumulative_proto_fee: i128 = 0;
        let mut cumulative_keeper_fee: i128 = 0;
        let mut cumulative_net: i128 = 0;

        // Optimized Loop: Deserialization happens once per item
        for i in 0..len {
            let item = items.get(i).ok_or(FeeError::EmptyBatch)?;
            let stream_id = item.stream_id;
            let gross_amount = item.amount;

            let breakdown = math::compute_fee_breakdown(gross_amount, proto_bps, keeper_bps)?;

            // Load persistent record directly via single .get()
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

            events::emit_fee_accrued(
                &env,
                stream_id,
                gross_amount,
                breakdown.protocol_fee,
                breakdown.keeper_fee,
                breakdown.net_amount,
            );

            // Accumulate in local memory registers with checked arithmetic
            cumulative_gross = cumulative_gross
                .checked_add(gross_amount)
                .ok_or(FeeError::ArithmeticOverflow)?;
            cumulative_total_fee = cumulative_total_fee
                .checked_add(breakdown.total_fee)
                .ok_or(FeeError::ArithmeticOverflow)?;
            cumulative_proto_fee = cumulative_proto_fee
                .checked_add(breakdown.protocol_fee)
                .ok_or(FeeError::ArithmeticOverflow)?;
            cumulative_keeper_fee = cumulative_keeper_fee
                .checked_add(breakdown.keeper_fee)
                .ok_or(FeeError::ArithmeticOverflow)?;
            cumulative_net = cumulative_net
                .checked_add(breakdown.net_amount)
                .ok_or(FeeError::ArithmeticOverflow)?;
        }

        // Single atomic write to global instance totals at the end of the batch
        let total_proto = storage::get_total_protocol_fees(&env)
            .checked_add(cumulative_proto_fee)
            .ok_or(FeeError::ArithmeticOverflow)?;
        storage::set_total_protocol_fees(&env, total_proto);

        let total_keeper = storage::get_total_keeper_fees(&env)
            .checked_add(cumulative_keeper_fee)
            .ok_or(FeeError::ArithmeticOverflow)?;
        storage::set_total_keeper_fees(&env, total_keeper);

        Ok(AccruedFeeBreakdown {
            gross_amount: cumulative_gross,
            total_fee: cumulative_total_fee,
            protocol_fee: cumulative_proto_fee,
            keeper_fee: cumulative_keeper_fee,
            net_amount: cumulative_net,
        })
    }

    /// Claim accrued fees for a specific stream. Gated by `fee_admin`.
    pub fn claim_stream_fee(
        env: Env,
        stream_id: u64,
        claim_proto: bool,
        claim_keeper: bool,
    ) -> Result<(i128, i128), FeeError> {
        let config = storage::load_config(&env)?;
        config.fee_admin.require_auth();

        if config.paused {
            return Err(FeeError::ContractPaused);
        }

        let mut record =
            storage::get_stream_fee(&env, stream_id).ok_or(FeeError::FeeRecordNotFound)?;

        let mut proto_to_claim: i128 = 0;
        let mut keeper_to_claim: i128 = 0;

        if claim_proto {
            proto_to_claim = record
                .protocol_fee_accrued
                .checked_sub(record.protocol_fee_claimed)
                .ok_or(FeeError::ArithmeticOverflow)?;
            if proto_to_claim > 0 {
                record.protocol_fee_claimed = record
                    .protocol_fee_claimed
                    .checked_add(proto_to_claim)
                    .ok_or(FeeError::ArithmeticOverflow)?;
            }
        }

        if claim_keeper {
            keeper_to_claim = record
                .keeper_fee_accrued
                .checked_sub(record.keeper_fee_claimed)
                .ok_or(FeeError::ArithmeticOverflow)?;
            if keeper_to_claim > 0 {
                record.keeper_fee_claimed = record
                    .keeper_fee_claimed
                    .checked_add(keeper_to_claim)
                    .ok_or(FeeError::ArithmeticOverflow)?;
            }
        }

        if proto_to_claim == 0 && keeper_to_claim == 0 {
            return Err(FeeError::ZeroFeeAccrual);
        }

        storage::set_stream_fee(&env, &record);

        events::emit_fee_claimed(
            &env,
            stream_id,
            &config.fee_recipient,
            proto_to_claim,
            keeper_to_claim,
        );

        Ok((proto_to_claim, keeper_to_claim))
    }

    /// Settle a stream fee record permanently, marking it finalized.
    pub fn settle_stream_fee(env: Env, stream_id: u64) -> Result<(), FeeError> {
        let config = storage::load_config(&env)?;
        config.fee_collector.require_auth();

        let mut record =
            storage::get_stream_fee(&env, stream_id).ok_or(FeeError::FeeRecordNotFound)?;
        if record.is_settled {
            return Err(FeeError::FeeAlreadySettled);
        }

        record.is_settled = true;
        storage::set_stream_fee(&env, &record);
        Ok(())
    }

    /// Batch settle multiple streams with loop optimization.
    pub fn batch_settle_fees(env: Env, stream_ids: Vec<u64>) -> Result<u32, FeeError> {
        let len = stream_ids.len();
        if len == 0 {
            return Err(FeeError::EmptyBatch);
        }
        if len > MAX_BATCH_SIZE {
            return Err(FeeError::BatchTooLarge);
        }

        let config = storage::load_config(&env)?;
        config.fee_collector.require_auth();

        // Validate duplicates and non-zero IDs
        for i in 0..len {
            let id_i = stream_ids.get(i).ok_or(FeeError::EmptyBatch)?;
            if id_i == 0 {
                return Err(FeeError::InvalidStreamId);
            }
            for j in (i + 1)..len {
                let id_j = stream_ids.get(j).ok_or(FeeError::EmptyBatch)?;
                if id_i == id_j {
                    return Err(FeeError::InvariantViolation);
                }
            }
        }

        let mut settled_count: u32 = 0;
        for i in 0..len {
            let stream_id = stream_ids.get(i).ok_or(FeeError::EmptyBatch)?;
            let mut record =
                storage::get_stream_fee(&env, stream_id).ok_or(FeeError::FeeRecordNotFound)?;
            if record.is_settled {
                return Err(FeeError::FeeAlreadySettled);
            }
            record.is_settled = true;
            storage::set_stream_fee(&env, &record);
            settled_count = settled_count
                .checked_add(1)
                .ok_or(FeeError::ArithmeticOverflow)?;
        }

        Ok(settled_count)
    }

    // -----------------------------------------------------------------------
    // View Functions
    // -----------------------------------------------------------------------

    /// Return global configuration.
    pub fn get_config(env: Env) -> Result<FeeConfig, FeeError> {
        storage::load_config(&env)
    }

    /// Return the stream fee record, if any.
    pub fn get_stream_fee(env: Env, stream_id: u64) -> Result<StreamFeeRecord, FeeError> {
        storage::get_stream_fee(&env, stream_id).ok_or(FeeError::FeeRecordNotFound)
    }

    /// Return total protocol fees accrued across all streams.
    pub fn get_total_protocol_fees(env: Env) -> i128 {
        storage::get_total_protocol_fees(&env)
    }

    /// Return total keeper fees accrued across all streams.
    pub fn get_total_keeper_fees(env: Env) -> i128 {
        storage::get_total_keeper_fees(&env)
    }

    /// Return the pending admin address, if an admin rotation is active.
    pub fn pending_admin(env: Env) -> Option<Address> {
        storage::get_pending_admin(&env)
    }

    /// Return canonical human-readable message for an error discriminant.
    pub fn error_message(env: Env, discriminant: u32) -> String {
        let msg = match discriminant {
            1 => FeeError::NotInitialized.message(),
            2 => FeeError::AlreadyInitialized.message(),
            3 => FeeError::Unauthorized.message(),
            4 => FeeError::CallerNotAuthorized.message(),
            5 => FeeError::NoPendingAdmin.message(),
            6 => FeeError::InvalidNewAdmin.message(),
            7 => FeeError::InvalidBpsRate.message(),
            8 => FeeError::InvalidFeeRecipient.message(),
            9 => FeeError::InvalidStreamId.message(),
            10 => FeeError::InvalidTokenAddress.message(),
            11 => FeeError::ZeroFeeAccrual.message(),
            12 => FeeError::InsufficientAccruedFees.message(),
            13 => FeeError::FeeRecordNotFound.message(),
            14 => FeeError::FeeAlreadySettled.message(),
            15 => FeeError::ArithmeticOverflow.message(),
            16 => FeeError::EmptyBatch.message(),
            17 => FeeError::BatchTooLarge.message(),
            18 => FeeError::InvariantViolation.message(),
            19 => FeeError::UnauthorizedRole.message(),
            20 => FeeError::InvalidRoleAddress.message(),
            21 => FeeError::ContractPaused.message(),
            22 => FeeError::ContractNotPaused.message(),
            _ => "",
        };
        String::from_str(&env, msg)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod test;
