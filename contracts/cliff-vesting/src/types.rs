//! Types for the cliff-vesting contract.

use soroban_sdk::{contracttype, Address};

// ---------------------------------------------------------------------------
// Role-based authorization model (issue #403)
//
// Roles are independent address slots, each governing a distinct set of
// operations.  No role shares another's authority.  Every role can be
// reassigned by the admin, two-step rotation available via `propose_admin`.
//
// | Role      | Operations permitted |
// |-----------|---------------------|
// | admin     | all setters, all role assignments |
// | creator   | create_schedule, batch_create |
// | revoker   | revoke_schedule (claws back unvested funds) |
// | pauser    | pause_schedule, resume_schedule |
//
// A single address may hold multiple roles simultaneously (e.g. admin and
// creator).  The contract enforces that a role address is never the zero
// address (which cannot sign) or the contract's own address.
// ---------------------------------------------------------------------------

/// Bitmask constants for the roles a schedule may grant to delegates.
/// These are the per-schedule delegation bits, separate from the contract-level
/// roles in `VestingConfig`.
pub mod role {
    /// Delegate may withdraw on behalf of the beneficiary.
    pub const WITHDRAW: u32 = 1 << 0;
    /// Delegate may revoke the schedule (sender-side).
    pub const REVOKE: u32 = 1 << 1;
    /// Delegate may pause/resume the schedule.
    pub const PAUSE: u32 = 1 << 2;
}

/// Lifecycle state of a vesting schedule.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScheduleStatus {
    /// Accruing normally.
    Active = 0,
    /// Accrual frozen by the pauser; beneficiary can still withdraw earned funds.
    Paused = 1,
    /// Sender revoked; unvested portion refunded; terminal.
    Revoked = 2,
    /// Fully claimed by beneficiary; terminal.
    Depleted = 3,
}

impl ScheduleStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, ScheduleStatus::Revoked | ScheduleStatus::Depleted)
    }
}

/// A single cliff-vesting schedule.
///
/// One entry per schedule in persistent storage under `DataKey::Schedule(id)`.
/// Accrual is linear from `start_time` to `end_time`; withdrawals are blocked
/// until `cliff_time`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VestingSchedule {
    /// Party whose tokens are streaming out.
    pub grantor: Address,
    /// Party who receives the vested tokens.
    pub beneficiary: Address,
    /// SEP-41 token contract.
    pub token: Address,
    /// Total tokens deposited (reduced to `vested` on revoke).
    pub deposited: i128,
    /// Cumulative tokens claimed by the beneficiary.
    pub withdrawn: i128,
    /// Schedule start in Unix seconds.
    pub start_time: u64,
    /// Schedule end in Unix seconds.  Strictly > `start_time`.
    pub end_time: u64,
    /// Cliff gate in Unix seconds.  Within `[start_time, end_time]`.
    /// Withdrawals are blocked until this point, but accrual runs from
    /// `start_time`.
    pub cliff_time: u64,
    /// Instant the schedule was last paused.  `None` while active.
    pub paused_at: Option<u64>,
    /// Cumulative seconds spent paused (excluding the in-progress pause).
    pub paused_total: u64,
    /// Lifecycle state.
    pub status: ScheduleStatus,
}

/// Contract-level role configuration (issue #403).
///
/// Stored as a single instance-storage record so setters update all roles
/// atomically.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VestingConfig {
    /// Full administrative authority.
    pub admin: Address,
    /// Address permitted to create new vesting schedules.
    pub creator: Address,
    /// Address permitted to revoke schedules (claw back unvested tokens).
    pub revoker: Address,
    /// Address permitted to pause and resume schedules.
    pub pauser: Address,
}

/// Storage keys.
///
/// `Config`, `PendingAdmin`, and `NextScheduleId` live in instance storage.
/// `Schedule(id)` entries live in persistent storage with their own TTLs.
#[contracttype]
#[repr(u32)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Instance storage. Role configuration.
    Config = 0,
    /// Instance storage. Pending next admin for two-step rotation.
    PendingAdmin = 1,
    /// Instance storage. Monotonic counter; next schedule ID to issue.
    NextScheduleId = 2,
    /// Persistent storage. One entry per vesting schedule.
    Schedule(u64) = 3,
}
