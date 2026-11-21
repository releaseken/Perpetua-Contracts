//! Types for the token-vault contract.

use soroban_sdk::{contracttype, Address};

/// Lifecycle state of a vault position.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionStatus {
    /// Accruing normally.
    Active = 0,
    /// Accrual frozen by the admin; beneficiary can still withdraw earned funds.
    Paused = 1,
    /// Sender revoked; unvested portion refunded; terminal.
    Revoked = 2,
    /// Fully claimed by beneficiary; terminal.
    Depleted = 3,
}

impl PositionStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, PositionStatus::Revoked | PositionStatus::Depleted)
    }
}

/// A single token-vault position.
///
/// Stored in persistent storage under `DataKey::Position(id)`.
/// Accrual is linear from `start_time` to `end_time`; withdrawals are blocked
/// until `cliff_time`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultPosition {
    /// Party whose tokens are being vested.
    pub sender: Address,
    /// Party who receives the vested tokens.
    pub beneficiary: Address,
    /// SEP-41 token contract.
    pub token: Address,
    /// Total tokens deposited (reduced to `vested` on revoke).
    pub deposited: i128,
    /// Cumulative tokens claimed by the beneficiary.
    pub withdrawn: i128,
    /// Start of the vesting window, Unix seconds.
    pub start_time: u64,
    /// End of the vesting window, Unix seconds.  Strictly > `start_time`.
    pub end_time: u64,
    /// Cliff gate, Unix seconds.  Within `[start_time, end_time]`.
    /// Withdrawals are blocked until this point; accrual runs from `start_time`.
    pub cliff_time: u64,
    /// Instant the position was last paused, `None` while active.
    pub paused_at: Option<u64>,
    /// Cumulative seconds spent paused (excluding the in-progress pause).
    pub paused_total: u64,
    /// Lifecycle state.
    pub status: PositionStatus,
}

impl VaultPosition {
    /// Test helper: construct a minimal position from raw parameters.
    #[cfg(test)]
    pub fn test_new(
        sender: Address,
        beneficiary: Address,
        token: Address,
        deposited: i128,
        start_time: u64,
        end_time: u64,
        cliff_time: u64,
        paused_total: u64,
        paused_at: Option<u64>,
        status: PositionStatus,
    ) -> Self {
        VaultPosition {
            sender,
            beneficiary,
            token,
            deposited,
            withdrawn: 0,
            start_time,
            end_time,
            cliff_time,
            paused_at,
            paused_total,
            status,
        }
    }
}

/// Vault-level configuration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultConfig {
    /// Administrative authority.
    pub admin: Address,
}

/// Storage keys.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Instance storage: vault configuration.
    Config,
    /// Instance storage: monotonic position ID counter.
    NextPositionId,
    /// Persistent storage: one entry per vault position.
    Position(u64),
}
