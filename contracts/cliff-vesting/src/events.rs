//! Event definitions for the cliff-vesting contract.
//!
//! Every state change emits exactly one event.  Topic layout and struct fields
//! are ABI: additive (new trailing field) is compatible; reordering or removal
//! is a breaking change.
//!
//! # Topic inventory (must remain unique)
//!
//! | struct                   | topic[0]                 |
//! |--------------------------|--------------------------|
//! | `ScheduleCreated`        | `schedule_created`       |
//! | `Withdrawn`              | `withdrawn`              |
//! | `Revoked`                | `revoked`                |
//! | `SchedulePaused`         | `schedule_paused`        |
//! | `ScheduleResumed`        | `schedule_resumed`       |
//! | `TtlExtended`            | `ttl_extended`           |
//! | `RoleUpdated`            | `role_updated`           |
//! | `AdminTransferProposed`  | `admin_transfer_proposed`|
//! | `AdminTransferred`       | `admin_transferred`      |

use soroban_sdk::{contractevent, symbol_short, Address, Env, Symbol};

use crate::types::{ScheduleStatus, VestingSchedule};

/// A new vesting schedule was created.
#[contractevent]
pub struct ScheduleCreated {
    #[topic]
    pub schedule_id: u64,
    #[topic]
    pub grantor: Address,
    #[topic]
    pub beneficiary: Address,
    pub token: Address,
    pub deposited: i128,
    pub start_time: u64,
    pub end_time: u64,
    pub cliff_time: u64,
}

/// The beneficiary claimed tokens.
#[contractevent]
pub struct Withdrawn {
    #[topic]
    pub schedule_id: u64,
    #[topic]
    pub beneficiary: Address,
    pub amount: i128,
    pub withdrawn: i128,
    pub deposited: i128,
    pub status: ScheduleStatus,
}

/// The grantor revoked an active schedule.
#[contractevent]
pub struct Revoked {
    #[topic]
    pub schedule_id: u64,
    #[topic]
    pub grantor: Address,
    #[topic]
    pub beneficiary: Address,
    pub refunded: i128,
    pub vested: i128,
    pub withdrawn: i128,
    pub end_time: u64,
}

/// A schedule was paused.
#[contractevent]
pub struct SchedulePaused {
    #[topic]
    pub schedule_id: u64,
    pub paused_at: u64,
}

/// A schedule was resumed.
#[contractevent]
pub struct ScheduleResumed {
    #[topic]
    pub schedule_id: u64,
    pub paused_duration: u64,
    pub paused_total: u64,
}

/// A schedule entry's TTL was extended.
#[contractevent]
pub struct TtlExtended {
    #[topic]
    pub schedule_id: u64,
    pub extended_to_ledgers: u32,
}

/// A contract-level role was updated (issue #403).
#[contractevent]
pub struct RoleUpdated {
    /// Short symbol identifying which role was updated: "admin", "creator",
    /// "revoker", or "pauser".
    #[topic]
    pub role: Symbol,
    #[topic]
    pub new_holder: Address,
}

/// Two-step admin rotation: a successor was nominated.
#[contractevent]
pub struct AdminTransferProposed {
    #[topic]
    pub current: Address,
    #[topic]
    pub pending: Address,
}

/// Two-step admin rotation: the successor accepted and took over.
#[contractevent]
pub struct AdminTransferred {
    #[topic]
    pub previous: Address,
    #[topic]
    pub new: Address,
}

// ---------------------------------------------------------------------------
// Emission helpers
// ---------------------------------------------------------------------------

pub fn schedule_created(env: &Env, schedule_id: u64, s: &VestingSchedule) {
    ScheduleCreated {
        schedule_id,
        grantor: s.grantor.clone(),
        beneficiary: s.beneficiary.clone(),
        token: s.token.clone(),
        deposited: s.deposited,
        start_time: s.start_time,
        end_time: s.end_time,
        cliff_time: s.cliff_time,
    }
    .publish(env);
}

pub fn withdrawn(env: &Env, schedule_id: u64, s: &VestingSchedule, amount: i128) {
    Withdrawn {
        schedule_id,
        beneficiary: s.beneficiary.clone(),
        amount,
        withdrawn: s.withdrawn,
        deposited: s.deposited,
        status: s.status,
    }
    .publish(env);
}

pub fn revoked(env: &Env, schedule_id: u64, s: &VestingSchedule, refunded: i128, vested_at_revoke: i128) {
    Revoked {
        schedule_id,
        grantor: s.grantor.clone(),
        beneficiary: s.beneficiary.clone(),
        refunded,
        vested: vested_at_revoke,
        withdrawn: s.withdrawn,
        end_time: s.end_time,
    }
    .publish(env);
}

pub fn schedule_paused(env: &Env, schedule_id: u64, paused_at: u64) {
    SchedulePaused {
        schedule_id,
        paused_at,
    }
    .publish(env);
}

pub fn schedule_resumed(
    env: &Env,
    schedule_id: u64,
    paused_duration: u64,
    paused_total: u64,
) {
    ScheduleResumed {
        schedule_id,
        paused_duration,
        paused_total,
    }
    .publish(env);
}

pub fn ttl_extended(env: &Env, schedule_id: u64, extended_to_ledgers: u32) {
    TtlExtended {
        schedule_id,
        extended_to_ledgers,
    }
    .publish(env);
}

pub fn role_updated(env: &Env, role: &str, new_holder: Address) {
    RoleUpdated {
        role: symbol_short!(role),
        new_holder,
    }
    .publish(env);
}
