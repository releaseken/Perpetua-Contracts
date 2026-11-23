use soroban_sdk::{contracttype, Address, Env, Symbol};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeAccruedEvent {
    pub stream_id: u64,
    pub gross_amount: i128,
    pub protocol_fee: i128,
    pub keeper_fee: i128,
    pub net_amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeClaimedEvent {
    pub stream_id: u64,
    pub recipient: Address,
    pub protocol_fee: i128,
    pub keeper_fee: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeConfigUpdatedEvent {
    pub fee_admin: Address,
    pub fee_recipient: Address,
    pub protocol_fee_bps: u32,
    pub keeper_split_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoleUpdatedEvent {
    pub role: Symbol,
    pub old_holder: Address,
    pub new_holder: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminProposedEvent {
    pub current_admin: Address,
    pub proposed_admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminTransferredEvent {
    pub old_admin: Address,
    pub new_admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PauseStatusEvent {
    pub paused: bool,
    pub caller: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TtlExtendedEvent {
    pub stream_id: u64,
    pub extended_to_ledgers: u32,
}

pub fn emit_fee_accrued(
    env: &Env,
    stream_id: u64,
    gross_amount: i128,
    protocol_fee: i128,
    keeper_fee: i128,
    net_amount: i128,
) {
    let topics = (Symbol::new(env, "fee_accrued"), stream_id);
    let event = FeeAccruedEvent {
        stream_id,
        gross_amount,
        protocol_fee,
        keeper_fee,
        net_amount,
    };
    env.events().publish(topics, event);
}

pub fn emit_fee_claimed(
    env: &Env,
    stream_id: u64,
    recipient: &Address,
    protocol_fee: i128,
    keeper_fee: i128,
) {
    let topics = (Symbol::new(env, "fee_claimed"), stream_id, recipient);
    let event = FeeClaimedEvent {
        stream_id,
        recipient: recipient.clone(),
        protocol_fee,
        keeper_fee,
    };
    env.events().publish(topics, event);
}

pub fn emit_config_updated(
    env: &Env,
    fee_admin: &Address,
    fee_recipient: &Address,
    protocol_fee_bps: u32,
    keeper_split_bps: u32,
) {
    let topics = (Symbol::new(env, "fee_config_updated"),);
    let event = FeeConfigUpdatedEvent {
        fee_admin: fee_admin.clone(),
        fee_recipient: fee_recipient.clone(),
        protocol_fee_bps,
        keeper_split_bps,
    };
    env.events().publish(topics, event);
}

pub fn emit_role_updated(env: &Env, role: Symbol, old_holder: &Address, new_holder: &Address) {
    let topics = (Symbol::new(env, "role_updated"), role);
    let event = RoleUpdatedEvent {
        role,
        old_holder: old_holder.clone(),
        new_holder: new_holder.clone(),
    };
    env.events().publish(topics, event);
}

pub fn emit_admin_proposed(env: &Env, current_admin: &Address, proposed_admin: &Address) {
    let topics = (Symbol::new(env, "admin_proposed"),);
    let event = AdminProposedEvent {
        current_admin: current_admin.clone(),
        proposed_admin: proposed_admin.clone(),
    };
    env.events().publish(topics, event);
}

pub fn emit_admin_transferred(env: &Env, old_admin: &Address, new_admin: &Address) {
    let topics = (Symbol::new(env, "admin_transferred"),);
    let event = AdminTransferredEvent {
        old_admin: old_admin.clone(),
        new_admin: new_admin.clone(),
    };
    env.events().publish(topics, event);
}

pub fn emit_pause_status(env: &Env, paused: bool, caller: &Address) {
    let topics = (Symbol::new(env, "fee_pause_status"),);
    let event = PauseStatusEvent {
        paused,
        caller: caller.clone(),
    };
    env.events().publish(topics, event);
}

pub fn emit_ttl_extended(env: &Env, stream_id: u64, extended_to_ledgers: u32) {
    let topics = (Symbol::new(env, "ttl_extended"), stream_id);
    let event = TtlExtendedEvent {
        stream_id,
        extended_to_ledgers,
    };
    env.events().publish(topics, event);
}
