#![no_std]
//! # Perpetua token-vault
//!
//! A token-vault / stream accounting contract for Soroban.  Tokens accrue
//! linearly from `start_time` to `end_time` but are withheld until the
//! beneficiary's `cliff_time` passes.
//!
//! ## Overflow safety
//!
//! All time fields are `u64`.  Every arithmetic path that touches timestamps
//! uses `saturating_sub`, `saturating_add`, or `checked_mul`/`checked_div`.
//! The creation guard `deposit * duration < i128::MAX` front-loads the only
//! multiplication that could overflow during accrual.

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

pub mod errors;
pub mod invariants;
pub mod math;
pub mod storage;
pub mod types;

pub use errors::VaultError;
pub use math::{cliff_reached, duration, elapsed, liability, refundable, vested, withdrawable};
pub use storage::{MIN_POSITION_TTL_LEDGERS, SECONDS_PER_LEDGER, TTL_BUFFER_SECONDS};
pub use types::{DataKey, PositionStatus, VaultConfig, VaultPosition};

use soroban_sdk::{
    contract, contractimpl, token, Address, Env, IntoVal, InvokeError, MuxedAddress,
};

// ---------------------------------------------------------------------------
// Token transfer helper
// ---------------------------------------------------------------------------

fn token_transfer(
    env: &Env,
    token: &Address,
    from: &Address,
    to: MuxedAddress,
    amount: &i128,
) -> Result<(), VaultError> {
    match token::TokenClient::new(env, token).try_transfer(from, &to, amount) {
        Ok(Ok(())) => Ok(()),
        Err(Err(InvokeError::Contract(_))) | Ok(Err(_)) | Err(Ok(_)) => {
            Err(VaultError::TokenTransferFailed)
        }
        Err(Err(InvokeError::Abort)) => Err(VaultError::TokenMissing),
    }
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct TokenVault;

#[contractimpl]
impl TokenVault {
    // -----------------------------------------------------------------------
    // Initialization
    // -----------------------------------------------------------------------

    /// Initialize the vault with an admin address.
    ///
    /// # Errors
    ///
    /// * [`VaultError::AlreadyInitialized`]
    pub fn init(env: Env, admin: Address) -> Result<(), VaultError> {
        if env.storage().instance().has(&DataKey::Config) {
            return Err(VaultError::AlreadyInitialized);
        }
        admin.require_auth();
        let config = VaultConfig { admin };
        storage::save_config(&env, &config);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Position creation
    // -----------------------------------------------------------------------

    /// Create a vesting position and lock `deposit` from `sender`.
    ///
    /// Returns the new position ID (monotonic, never reused).
    ///
    /// # Errors
    ///
    /// * [`VaultError::InvalidDeposit`]
    /// * [`VaultError::InvalidTimeRange`]
    /// * [`VaultError::InvalidCliff`]
    /// * [`VaultError::DepositRateTooLow`] — `deposit < duration`
    /// * [`VaultError::Overflow`] — `deposit * duration` overflows `i128`
    #[allow(clippy::too_many_arguments)]
    pub fn create_position(
        env: Env,
        sender: Address,
        beneficiary: Address,
        token: Address,
        deposit: i128,
        start_time: u64,
        end_time: u64,
        cliff_time: u64,
    ) -> Result<u64, VaultError> {
        sender.require_auth_for_args(
            (
                beneficiary.clone(),
                token.clone(),
                deposit,
                start_time,
                end_time,
                cliff_time,
            )
                .into_val(&env),
        );

        Self::validate_position_params(deposit, start_time, end_time, cliff_time)?;

        let position_id = storage::allocate_position_id(&env)?;
        let pos = VaultPosition {
            sender: sender.clone(),
            beneficiary,
            token: token.clone(),
            deposited: deposit,
            withdrawn: 0,
            start_time,
            end_time,
            cliff_time,
            paused_at: None,
            paused_total: 0,
            status: PositionStatus::Active,
        };

        // Pull deposit before persisting (CEI pattern).
        token_transfer(
            &env,
            &token,
            &sender,
            MuxedAddress::from(env.current_contract_address()),
            &deposit,
        )?;

        storage::save_position(&env, position_id, &pos);
        storage::extend_instance(&env);
        Ok(position_id)
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
    /// * [`VaultError::NothingToWithdraw`]
    /// * [`VaultError::InsufficientWithdrawable`]
    /// * [`VaultError::PositionTerminated`]
    pub fn withdraw(env: Env, position_id: u64, amount: Option<i128>) -> Result<i128, VaultError> {
        let mut pos = storage::load_position(&env, position_id)?;
        pos.beneficiary
            .require_auth_for_args((position_id, amount).into_val(&env));

        let now = env.ledger().timestamp();
        let available = math::withdrawable(&pos, now)?;

        if available == 0 {
            if pos.status.is_terminal() {
                return Err(VaultError::PositionTerminated);
            }
            return Err(VaultError::NothingToWithdraw);
        }

        let payout = match amount {
            None => available,
            Some(req) => {
                if req <= 0 {
                    return Err(VaultError::InvalidAmount);
                }
                if req > available {
                    return Err(VaultError::InsufficientWithdrawable);
                }
                req
            }
        };

        Self::apply_withdrawal(&env, position_id, &mut pos, payout)?;
        storage::extend_instance(&env);
        Ok(payout)
    }

    // -----------------------------------------------------------------------
    // Revoke — admin only
    // -----------------------------------------------------------------------

    /// Revoke an active position: refund unvested tokens to the sender.
    ///
    /// # Errors
    ///
    /// * [`VaultError::Unauthorized`]
    /// * [`VaultError::PositionTerminated`]
    pub fn revoke_position(env: Env, position_id: u64) -> Result<(), VaultError> {
        let config = storage::load_config(&env)?;
        config.admin.require_auth();

        let mut pos = storage::load_position(&env, position_id)?;
        if pos.status.is_terminal() {
            return Err(VaultError::PositionTerminated);
        }

        let now = env.ledger().timestamp();
        let vested_now = math::vested(&pos, now)?;
        let refund = math::refundable(&pos, now)?;

        let settle_at = math::stream_time(&pos, now).max(pos.start_time);
        let sender = pos.sender.clone();
        let token = pos.token.clone();

        pos.deposited = vested_now;
        pos.end_time = settle_at;
        pos.paused_at = None;
        pos.status = PositionStatus::Revoked;

        storage::save_position(&env, position_id, &pos);
        storage::extend_instance(&env);

        if refund > 0 {
            token_transfer(
                &env,
                &token,
                &env.current_contract_address(),
                MuxedAddress::from(sender),
                &refund,
            )?;
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Pause / resume — admin only
    // -----------------------------------------------------------------------

    /// Pause a position's accrual clock.
    pub fn pause_position(env: Env, position_id: u64) -> Result<(), VaultError> {
        let config = storage::load_config(&env)?;
        config.admin.require_auth();

        let mut pos = storage::load_position(&env, position_id)?;
        if pos.status.is_terminal() {
            return Err(VaultError::PositionTerminated);
        }
        if pos.paused_at.is_some() {
            return Err(VaultError::PositionTerminated);
        }

        let now = env.ledger().timestamp();
        pos.paused_at = Some(now);
        pos.status = PositionStatus::Paused;
        storage::save_position(&env, position_id, &pos);
        storage::extend_instance(&env);
        Ok(())
    }

    /// Resume a paused position.
    pub fn resume_position(env: Env, position_id: u64) -> Result<(), VaultError> {
        let config = storage::load_config(&env)?;
        config.admin.require_auth();

        let mut pos = storage::load_position(&env, position_id)?;
        if pos.status.is_terminal() {
            return Err(VaultError::PositionTerminated);
        }
        let paused_at = match pos.paused_at {
            Some(t) => t,
            None => return Err(VaultError::PositionTerminated),
        };

        let now = env.ledger().timestamp();
        let paused_duration = now.saturating_sub(paused_at);
        pos.paused_total = pos
            .paused_total
            .checked_add(paused_duration)
            .ok_or(VaultError::ArithmeticOverflow)?;
        pos.paused_at = None;
        pos.status = PositionStatus::Active;
        storage::save_position(&env, position_id, &pos);
        storage::extend_instance(&env);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // TTL keeper — permissionless
    // -----------------------------------------------------------------------

    /// Extend the TTL of a position entry.  Permissionless.
    pub fn extend_position_ttl(env: Env, position_id: u64) -> Result<(), VaultError> {
        let pos = storage::load_position(&env, position_id)?;
        storage::extend_instance(&env);
        let _ = storage::ttl_target_ledgers(&env, &pos);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Views — permissionless
    // -----------------------------------------------------------------------

    /// Return the vault configuration record.
    pub fn get_config(env: Env) -> Result<VaultConfig, VaultError> {
        storage::load_config(&env)
    }

    /// Return the vault position for `position_id`.
    pub fn get_position(env: Env, position_id: u64) -> Result<VaultPosition, VaultError> {
        storage::peek_position(&env, position_id)
    }

    /// Total number of issued position IDs.
    pub fn position_count(env: Env) -> u64 {
        storage::position_count(&env)
    }

    /// Whether a persistent entry exists for `position_id`.
    pub fn position_exists(env: Env, position_id: u64) -> bool {
        storage::position_exists(&env, position_id)
    }

    /// Current withdrawable balance for `position_id`.
    pub fn withdrawable_of(env: Env, position_id: u64) -> Result<i128, VaultError> {
        let pos = storage::peek_position(&env, position_id)?;
        math::withdrawable(&pos, env.ledger().timestamp())
    }

    /// Total vested for `position_id` at the current ledger time.
    pub fn vested_of(env: Env, position_id: u64) -> Result<i128, VaultError> {
        let pos = storage::peek_position(&env, position_id)?;
        math::vested(&pos, env.ledger().timestamp())
    }

    // -----------------------------------------------------------------------
    // Admin rotation
    // -----------------------------------------------------------------------

    /// Replace the admin.
    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), VaultError> {
        let config = storage::load_config(&env)?;
        config.admin.require_auth();
        let new_config = VaultConfig { admin: new_admin };
        storage::save_config(&env, &new_config);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn validate_position_params(
        deposit: i128,
        start_time: u64,
        end_time: u64,
        cliff_time: u64,
    ) -> Result<(), VaultError> {
        if deposit <= 0 {
            return Err(VaultError::InvalidDeposit);
        }
        if end_time <= start_time {
            return Err(VaultError::InvalidTimeRange);
        }
        if cliff_time < start_time || cliff_time > end_time {
            return Err(VaultError::InvalidCliff);
        }
        let total_duration = end_time - start_time;
        if deposit < total_duration as i128 {
            return Err(VaultError::DepositRateTooLow);
        }
        // Front-load overflow guard: `deposit * elapsed` can never overflow
        // for the life of this position if this product fits in i128.
        deposit
            .checked_mul(total_duration as i128)
            .ok_or(VaultError::Overflow)?;
        Ok(())
    }

    /// Update `pos.withdrawn`, mark `Depleted` if fully claimed, transfer tokens.
    ///
    /// CEI: storage written before token transfer.
    fn apply_withdrawal(
        env: &Env,
        position_id: u64,
        pos: &mut VaultPosition,
        payout: i128,
    ) -> Result<(), VaultError> {
        pos.withdrawn = pos
            .withdrawn
            .checked_add(payout)
            .ok_or(VaultError::ArithmeticOverflow)?;

        if pos.withdrawn >= pos.deposited {
            pos.status = PositionStatus::Depleted;
        }

        let beneficiary = pos.beneficiary.clone();
        let token = pos.token.clone();

        storage::save_position(env, position_id, pos);

        token_transfer(
            env,
            &token,
            &env.current_contract_address(),
            MuxedAddress::from(beneficiary),
            &payout,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod test;
