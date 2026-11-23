#![no_std]
//! # Perpetua fee-accrual
//!
//! Dedicated protocol and keeper fee accounting subsystem for the Perpetua
//! streaming protocol.
//!
//! ## Automated entry lifetime extension & archival resilience (issue #390)
//!
//! Persistent stream fee records (`DataKey::StreamFee(stream_id)`) and instance
//! configuration entries (`DataKey::Config`, `DataKey::TotalProtocolFees`,
//! `DataKey::TotalKeeperFees`, `DataKey::PendingAdmin`) are protected from
//! unexpected ledger archival through a three-tier lifetime management policy:
//!
//! 1. **Instance pinning**: Every mutating operation and configuration touch
//!    pins the contract instance entry to `env.storage().max_ttl()`, ensuring
//!    global role assignments, fee rates, and cumulative totals never expire
//!    before individual stream fee records.
//! 2. **Automatic touch extension**: Every read-modify-write operation on a
//!    stream fee record (`accrue_fee`, `batch_accrue_fees`, `claim_protocol_fee`,
//!    `claim_keeper_fee`, `settle_stream_fee`) automatically extends the
//!    persistent entry's TTL to at least `MIN_ENTRY_TTL_LEDGERS` (~30 days with
//!    a 10% ledger-drift safety margin, clamped to `max_ttl()`).
//! 3. **Permissionless keeper sweeps**: Anyone can call `extend_stream_fee_ttl`,
//!    `extend_stream_fee_to_max`, or `batch_extend_ttl` to top up persistent
//!    stream fee entries without admin intervention, emitting `TtlExtendedEvent`
//!    for off-chain keeper observability.
//!
//! ## Granular role-based authorization (issue #385)
//!
//! | Role | Responsibilities |
//! |------|------------------|
//! | `admin` | Role assignments (`set_fee_admin`, `set_fee_collector`, `set_emergency_admin`) and two-step / single-step admin rotation |
//! | `fee_admin` | Fee parameter updates (`update_fee_config`: protocol fee bps, keeper split bps, fee recipient) |
//! | `fee_collector` | Triggering fee accruals (`accrue_fee`, `batch_accrue_fees`) and finalizing stream fee settlement (`settle_stream_fee`) |
//! | `emergency_admin` | Emergency circuit-breaker (`pause`, `unpause`) |

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
pub use math::{compute_fee_breakdown, BPS_DENOMINATOR};
pub use storage::{
    BUMP_LEDGERS_PERSISTENT, ENTRY_LIFETIME_THRESHOLD, MIN_ENTRY_TTL_LEDGERS,
    MIN_TTL_LEDGERS_PERSISTENT, SECONDS_PER_LEDGER, TTL_BUFFER_SECONDS,
};
pub use types::{AccruedFeeBreakdown, BatchAccrueItem, DataKey, FeeConfig, StreamFeeRecord};

use soroban_sdk::{contract, contractimpl, Address, Env, String, Symbol, Vec};

/// Maximum number of stream items a single batch call may process.
///
/// Bounded by Soroban's ledger footprint and host CPU instruction budget.
pub const MAX_BATCH_SIZE: u32 = 32;

#[contract]
pub struct FeeAccrualContract;

#[contractimpl]
impl FeeAccrualContract {
    // -----------------------------------------------------------------------
    // Initialization
    // -----------------------------------------------------------------------

    /// Initialize the `fee-accrual` contract with granular roles and initial fee rates.
    ///
    /// Pins the instance entry to `max_ttl()` immediately upon initialization.
    #[allow(clippy::too_many_arguments)]
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

        if storage::is_forbidden_admin(&env, &admin) {
            return Err(FeeError::InvalidNewAdmin);
        }
        if storage::is_forbidden_admin(&env, &fee_admin)
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
    // Fee Accrual — fee_collector role required
    // -----------------------------------------------------------------------

    /// Accrue protocol and keeper fees for a single `stream_id` and `gross_amount`.
    ///
    /// Automatically extends both the persistent `StreamFee(stream_id)` entry TTL
    /// and the instance storage TTL.
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
            return Err(FeeError::ZeroFeeAccrual);
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

        // Invariant check: claimed amounts never exceed accrued amounts.
        if record.protocol_fee_claimed > record.protocol_fee_accrued
            || record.keeper_fee_claimed > record.keeper_fee_accrued
        {
            return Err(FeeError::InvariantViolation);
        }

        let total_proto = storage::get_total_protocol_fees(&env)
            .checked_add(breakdown.protocol_fee)
            .ok_or(FeeError::ArithmeticOverflow)?;
        let total_keeper = storage::get_total_keeper_fees(&env)
            .checked_add(breakdown.keeper_fee)
            .ok_or(FeeError::ArithmeticOverflow)?;

        // Persist and bump TTLs (persistent + instance).
        storage::set_stream_fee(&env, &record);
        storage::set_total_protocol_fees(&env, total_proto);
        storage::set_total_keeper_fees(&env, total_keeper);

        events::emit_fee_accrued(
            &env,
            stream_id,
            breakdown.gross_amount,
            breakdown.protocol_fee,
            breakdown.keeper_fee,
            breakdown.net_amount,
        );

        Ok(breakdown)
    }

    /// Batch-accrue fees for multiple streams in a single atomic transaction.
    ///
    /// Performs a pre-flight validation pass before mutating any state so a
    /// single malformed item leaves zero partial writes. Every touched stream
    /// entry has its persistent TTL automatically extended.
    pub fn batch_accrue_fees(
        env: Env,
        caller: Address,
        items: Vec<BatchAccrueItem>,
    ) -> Result<Vec<AccruedFeeBreakdown>, FeeError> {
        caller.require_auth();
        let config = storage::load_config(&env)?;

        if config.paused {
            return Err(FeeError::ContractPaused);
        }
        if caller != config.fee_collector {
            return Err(FeeError::UnauthorizedRole);
        }

        let n = items.len();
        if n == 0 {
            return Err(FeeError::EmptyBatch);
        }
        if n > MAX_BATCH_SIZE {
            return Err(FeeError::BatchTooLarge);
        }

        // Pre-flight validation pass: validate all items and reject duplicate stream_ids
        // in the same batch to guarantee single-deserialization per unique stream entry.
        for i in 0..n {
            let item = items.get_unchecked(i);
            if item.stream_id == 0 {
                return Err(FeeError::InvalidStreamId);
            }
            if item.amount <= 0 {
                return Err(FeeError::ZeroFeeAccrual);
            }
            for j in (i + 1)..n {
                if items.get_unchecked(j).stream_id == item.stream_id {
                    return Err(FeeError::InvalidStreamId);
                }
            }
        }

        let mut total_proto = storage::get_total_protocol_fees(&env);
        let mut total_keeper = storage::get_total_keeper_fees(&env);
        let mut updated_records: Vec<StreamFeeRecord> = Vec::new(&env);
        let mut breakdowns: Vec<AccruedFeeBreakdown> = Vec::new(&env);

        for i in 0..n {
            let item = items.get_unchecked(i);
            let mut record =
                storage::peek_stream_fee(&env, item.stream_id).unwrap_or(StreamFeeRecord {
                    stream_id: item.stream_id,
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
                item.amount,
                config.protocol_fee_bps,
                config.keeper_split_bps,
            )?;

            record.total_processed = record
                .total_processed
                .checked_add(item.amount)
                .ok_or(FeeError::ArithmeticOverflow)?;
            record.protocol_fee_accrued = record
                .protocol_fee_accrued
                .checked_add(breakdown.protocol_fee)
                .ok_or(FeeError::ArithmeticOverflow)?;
            record.keeper_fee_accrued = record
                .keeper_fee_accrued
                .checked_add(breakdown.keeper_fee)
                .ok_or(FeeError::ArithmeticOverflow)?;

            total_proto = total_proto
                .checked_add(breakdown.protocol_fee)
                .ok_or(FeeError::ArithmeticOverflow)?;
            total_keeper = total_keeper
                .checked_add(breakdown.keeper_fee)
                .ok_or(FeeError::ArithmeticOverflow)?;

            updated_records.push_back(record);
            breakdowns.push_back(breakdown);
        }

        // Execution pass: write all records and extend each persistent entry's TTL.
        for i in 0..n {
            let record = updated_records.get_unchecked(i);
            let breakdown = breakdowns.get_unchecked(i);
            storage::set_stream_fee(&env, &record);
            events::emit_fee_accrued(
                &env,
                record.stream_id,
                breakdown.gross_amount,
                breakdown.protocol_fee,
                breakdown.keeper_fee,
                breakdown.net_amount,
            );
        }

        storage::set_total_protocol_fees(&env, total_proto);
        storage::set_total_keeper_fees(&env, total_keeper);
        Ok(breakdowns)
    }

    // -----------------------------------------------------------------------
    // Fee Claims & Settlement
    // -----------------------------------------------------------------------

    /// Claim accrued protocol fees for `stream_id` to the configured `fee_recipient`.
    ///
    /// Automatically refreshes the persistent entry TTL on claim.
    pub fn claim_protocol_fee(
        env: Env,
        caller: Address,
        stream_id: u64,
        amount: Option<i128>,
    ) -> Result<i128, FeeError> {
        caller.require_auth();
        let config = storage::load_config(&env)?;

        if config.paused {
            return Err(FeeError::ContractPaused);
        }
        if caller != config.fee_recipient && caller != config.fee_admin && caller != config.admin {
            return Err(FeeError::CallerNotAuthorized);
        }
        if stream_id == 0 {
            return Err(FeeError::InvalidStreamId);
        }

        let mut record = storage::load_stream_fee(&env, stream_id)?;
        let unclaimed = record
            .protocol_fee_accrued
            .checked_sub(record.protocol_fee_claimed)
            .ok_or(FeeError::ArithmeticOverflow)?;

        if unclaimed == 0 {
            return Err(FeeError::ZeroFeeAccrual);
        }

        let claim_amount = match amount {
            None => unclaimed,
            Some(req) => {
                if req <= 0 {
                    return Err(FeeError::ZeroFeeAccrual);
                }
                if req > unclaimed {
                    return Err(FeeError::InsufficientAccruedFees);
                }
                req
            }
        };

        record.protocol_fee_claimed = record
            .protocol_fee_claimed
            .checked_add(claim_amount)
            .ok_or(FeeError::ArithmeticOverflow)?;

        if record.protocol_fee_claimed > record.protocol_fee_accrued {
            return Err(FeeError::InvariantViolation);
        }

        storage::set_stream_fee(&env, &record);
        storage::extend_instance(&env);

        events::emit_fee_claimed(&env, stream_id, &config.fee_recipient, claim_amount, 0);
        Ok(claim_amount)
    }

    /// Claim accrued keeper fees for `stream_id` to `keeper_recipient`.
    ///
    /// Authorized for `fee_collector`, `fee_admin`, or `admin`. Automatically
    /// refreshes the persistent entry TTL on claim.
    pub fn claim_keeper_fee(
        env: Env,
        caller: Address,
        stream_id: u64,
        keeper_recipient: Address,
        amount: Option<i128>,
    ) -> Result<i128, FeeError> {
        caller.require_auth();
        let config = storage::load_config(&env)?;

        if config.paused {
            return Err(FeeError::ContractPaused);
        }
        if caller != config.fee_collector && caller != config.fee_admin && caller != config.admin {
            return Err(FeeError::CallerNotAuthorized);
        }
        if stream_id == 0 {
            return Err(FeeError::InvalidStreamId);
        }
        if storage::is_zero_address(&env, &keeper_recipient) {
            return Err(FeeError::InvalidFeeRecipient);
        }

        let mut record = storage::load_stream_fee(&env, stream_id)?;
        let unclaimed = record
            .keeper_fee_accrued
            .checked_sub(record.keeper_fee_claimed)
            .ok_or(FeeError::ArithmeticOverflow)?;

        if unclaimed == 0 {
            return Err(FeeError::ZeroFeeAccrual);
        }

        let claim_amount = match amount {
            None => unclaimed,
            Some(req) => {
                if req <= 0 {
                    return Err(FeeError::ZeroFeeAccrual);
                }
                if req > unclaimed {
                    return Err(FeeError::InsufficientAccruedFees);
                }
                req
            }
        };

        record.keeper_fee_claimed = record
            .keeper_fee_claimed
            .checked_add(claim_amount)
            .ok_or(FeeError::ArithmeticOverflow)?;

        if record.keeper_fee_claimed > record.keeper_fee_accrued {
            return Err(FeeError::InvariantViolation);
        }

        storage::set_stream_fee(&env, &record);
        storage::extend_instance(&env);

        events::emit_fee_claimed(&env, stream_id, &keeper_recipient, 0, claim_amount);
        Ok(claim_amount)
    }

    /// Mark a stream's fee record as settled (terminal state for new accruals).
    ///
    /// Automatically extends the persistent entry TTL so settled audit records
    /// and any remaining unclaimed fees remain accessible across the archival buffer.
    pub fn settle_stream_fee(env: Env, caller: Address, stream_id: u64) -> Result<(), FeeError> {
        caller.require_auth();
        let config = storage::load_config(&env)?;

        if config.paused {
            return Err(FeeError::ContractPaused);
        }
        if caller != config.fee_collector && caller != config.fee_admin && caller != config.admin {
            return Err(FeeError::UnauthorizedRole);
        }
        if stream_id == 0 {
            return Err(FeeError::InvalidStreamId);
        }

        let mut record = storage::load_stream_fee(&env, stream_id)?;
        if record.is_settled {
            return Err(FeeError::FeeAlreadySettled);
        }

        record.is_settled = true;
        storage::set_stream_fee(&env, &record);
        storage::extend_instance(&env);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Automated Entry Lifetime Extension — Permissionless Keeper API (#390)
    // -----------------------------------------------------------------------

    /// Extend the persistent TTL of a single stream's fee record to the minimum
    /// archival-resilience floor (`MIN_ENTRY_TTL_LEDGERS` clamped to `max_ttl()`).
    ///
    /// Permissionless: any keeper, user, or indexer may call this to prevent a
    /// stream's fee accounting entry from archiving. Also refreshes the contract
    /// instance TTL and emits `TtlExtendedEvent`.
    ///
    /// Returns the target ledger TTL applied.
    pub fn extend_stream_fee_ttl(env: Env, stream_id: u64) -> Result<u32, FeeError> {
        if stream_id == 0 {
            return Err(FeeError::InvalidStreamId);
        }
        if !storage::has_stream_fee(&env, stream_id) {
            return Err(FeeError::FeeRecordNotFound);
        }

        let target = storage::extend_stream_fee_ttl(&env, stream_id);
        storage::extend_instance(&env);

        events::emit_ttl_extended(&env, stream_id, target);
        Ok(target)
    }

    /// Extend the persistent TTL of a single stream's fee record to the network's
    /// maximum allowable TTL (`env.storage().max_ttl()`).
    ///
    /// Permissionless: enables long-horizon archival protection for active or
    /// disputed fee records.
    pub fn extend_stream_fee_to_max(env: Env, stream_id: u64) -> Result<u32, FeeError> {
        if stream_id == 0 {
            return Err(FeeError::InvalidStreamId);
        }
        if !storage::has_stream_fee(&env, stream_id) {
            return Err(FeeError::FeeRecordNotFound);
        }

        let max_ttl = storage::extend_stream_fee_to_max(&env, stream_id);
        storage::extend_instance(&env);

        events::emit_ttl_extended(&env, stream_id, max_ttl);
        Ok(max_ttl)
    }

    /// Extend the persistent TTL of multiple stream fee entries in a single sweep.
    ///
    /// Unknown or already-archived stream IDs are silently skipped so a single
    /// missing entry in a keeper sweep batch does not abort the entire maintenance
    /// transaction. Stream ID `0` is rejected during pre-flight validation.
    ///
    /// Returns the number of existing entries actually extended.
    pub fn batch_extend_ttl(env: Env, stream_ids: Vec<u64>) -> Result<u32, FeeError> {
        let n = stream_ids.len();
        if n == 0 {
            return Err(FeeError::EmptyBatch);
        }
        if n > MAX_BATCH_SIZE {
            return Err(FeeError::BatchTooLarge);
        }

        for i in 0..n {
            if stream_ids.get_unchecked(i) == 0 {
                return Err(FeeError::InvalidStreamId);
            }
        }

        let mut extended: u32 = 0;
        for stream_id in stream_ids.iter() {
            if storage::has_stream_fee(&env, stream_id) {
                let target = storage::extend_stream_fee_ttl(&env, stream_id);
                events::emit_ttl_extended(&env, stream_id, target);
                extended = extended
                    .checked_add(1)
                    .ok_or(FeeError::ArithmeticOverflow)?;
            }
        }

        storage::extend_instance(&env);
        Ok(extended)
    }

    /// Permissionless heartbeat to pin the contract instance storage to `max_ttl()`.
    pub fn extend_instance_ttl(env: Env) -> Result<u32, FeeError> {
        storage::load_config(&env)?;
        Ok(env.storage().max_ttl())
    }

    // -----------------------------------------------------------------------
    // Configuration & Role Management (#385)
    // -----------------------------------------------------------------------

    /// Update fee basis points and recipient address. Gated to `fee_admin`.
    pub fn update_fee_config(
        env: Env,
        caller: Address,
        fee_recipient: Address,
        protocol_fee_bps: u32,
        keeper_split_bps: u32,
    ) -> Result<(), FeeError> {
        caller.require_auth();
        let mut config = storage::load_config(&env)?;

        if caller != config.fee_admin {
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
        storage::save_config(&env, &config);

        events::emit_config_updated(
            &env,
            &caller,
            &fee_recipient,
            protocol_fee_bps,
            keeper_split_bps,
        );
        Ok(())
    }

    /// Assign a new `fee_admin` role holder. Gated to `admin`.
    pub fn set_fee_admin(env: Env, caller: Address, new_fee_admin: Address) -> Result<(), FeeError> {
        caller.require_auth();
        let mut config = storage::load_config(&env)?;
        if caller != config.admin {
            return Err(FeeError::Unauthorized);
        }
        if storage::is_forbidden_admin(&env, &new_fee_admin) {
            return Err(FeeError::InvalidRoleAddress);
        }

        let old = config.fee_admin.clone();
        config.fee_admin = new_fee_admin.clone();
        storage::save_config(&env, &config);

        events::emit_role_updated(&env, Symbol::new(&env, "fee_admin"), &old, &new_fee_admin);
        Ok(())
    }

    /// Assign a new `fee_collector` role holder. Gated to `admin`.
    pub fn set_fee_collector(
        env: Env,
        caller: Address,
        new_fee_collector: Address,
    ) -> Result<(), FeeError> {
        caller.require_auth();
        let mut config = storage::load_config(&env)?;
        if caller != config.admin {
            return Err(FeeError::Unauthorized);
        }
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

    /// Assign a new `emergency_admin` role holder. Gated to `admin`.
    pub fn set_emergency_admin(
        env: Env,
        caller: Address,
        new_emergency_admin: Address,
    ) -> Result<(), FeeError> {
        caller.require_auth();
        let mut config = storage::load_config(&env)?;
        if caller != config.admin {
            return Err(FeeError::Unauthorized);
        }
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

    /// Pause fee accrual and claiming operations. Gated to `emergency_admin` or `admin`.
    pub fn pause(env: Env, caller: Address) -> Result<(), FeeError> {
        caller.require_auth();
        let mut config = storage::load_config(&env)?;
        if caller != config.emergency_admin && caller != config.admin {
            return Err(FeeError::UnauthorizedRole);
        }
        if config.paused {
            return Err(FeeError::ContractPaused);
        }

        config.paused = true;
        storage::save_config(&env, &config);
        events::emit_pause_status(&env, true, &caller);
        Ok(())
    }

    /// Unpause fee accrual and claiming operations. Gated to `emergency_admin` or `admin`.
    pub fn unpause(env: Env, caller: Address) -> Result<(), FeeError> {
        caller.require_auth();
        let mut config = storage::load_config(&env)?;
        if caller != config.emergency_admin && caller != config.admin {
            return Err(FeeError::UnauthorizedRole);
        }
        if !config.paused {
            return Err(FeeError::ContractNotPaused);
        }

        config.paused = false;
        storage::save_config(&env, &config);
        events::emit_pause_status(&env, false, &caller);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Admin Rotation
    // -----------------------------------------------------------------------

    /// Nominate a successor admin for two-step admin rotation.
    pub fn propose_admin(env: Env, caller: Address, new_admin: Address) -> Result<(), FeeError> {
        caller.require_auth();
        let config = storage::load_config(&env)?;
        if caller != config.admin {
            return Err(FeeError::Unauthorized);
        }
        if storage::is_forbidden_admin(&env, &new_admin) {
            return Err(FeeError::InvalidNewAdmin);
        }

        storage::set_pending_admin(&env, &new_admin);
        events::emit_admin_proposed(&env, &config.admin, &new_admin);
        Ok(())
    }

    /// Accept a pending admin nomination.
    pub fn accept_admin(env: Env) -> Result<(), FeeError> {
        let pending = storage::get_pending_admin(&env).ok_or(FeeError::NoPendingAdmin)?;
        pending.require_auth();

        let mut config = storage::load_config(&env)?;
        let old_admin = config.admin.clone();
        config.admin = pending.clone();
        storage::clear_pending_admin(&env);
        storage::save_config(&env, &config);

        events::emit_admin_transferred(&env, &old_admin, &pending);
        Ok(())
    }

    /// Single-step admin rotation. Clears any pending two-step proposal.
    pub fn set_admin(env: Env, caller: Address, new_admin: Address) -> Result<(), FeeError> {
        caller.require_auth();
        let mut config = storage::load_config(&env)?;
        if caller != config.admin {
            return Err(FeeError::Unauthorized);
        }
        if storage::is_forbidden_admin(&env, &new_admin) {
            return Err(FeeError::InvalidNewAdmin);
        }

        let old_admin = config.admin.clone();
        config.admin = new_admin.clone();
        storage::clear_pending_admin(&env);
        storage::save_config(&env, &config);

        events::emit_admin_transferred(&env, &old_admin, &new_admin);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Read-Only Views
    // -----------------------------------------------------------------------

    /// Return the global fee configuration.
    pub fn get_config(env: Env) -> Result<FeeConfig, FeeError> {
        storage::load_config(&env)
    }

    /// Return the fee record for `stream_id` without mutating storage, or
    /// `FeeError::FeeRecordNotFound` if no entry exists.
    pub fn get_stream_record(env: Env, stream_id: u64) -> Result<StreamFeeRecord, FeeError> {
        storage::peek_stream_fee(&env, stream_id).ok_or(FeeError::FeeRecordNotFound)
    }

    /// Return `true` if a persistent fee entry exists for `stream_id`.
    pub fn stream_fee_exists(env: Env, stream_id: u64) -> bool {
        storage::has_stream_fee(&env, stream_id)
    }

    /// Preview the fee breakdown for `gross_amount` under the current fee configuration.
    pub fn preview_fee(env: Env, gross_amount: i128) -> Result<AccruedFeeBreakdown, FeeError> {
        let config = storage::peek_config(&env)?;
        compute_fee_breakdown(
            gross_amount,
            config.protocol_fee_bps,
            config.keeper_split_bps,
        )
    }

    /// Return cumulative protocol fees accrued across all streams.
    pub fn total_protocol_fees(env: Env) -> i128 {
        storage::get_total_protocol_fees(&env)
    }

    /// Return cumulative keeper fees accrued across all streams.
    pub fn total_keeper_fees(env: Env) -> i128 {
        storage::get_total_keeper_fees(&env)
    }

    /// Return the pending admin nominee, if any.
    pub fn pending_admin(env: Env) -> Result<Option<Address>, FeeError> {
        storage::peek_config(&env)?;
        Ok(storage::get_pending_admin(&env))
    }

    /// Return the canonical human-readable error message for a `FeeError` discriminant.
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

#[cfg(test)]
mod test;
