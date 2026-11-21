use soroban_sdk::{Address, Env};

use crate::error::FeeError;
use crate::types::{DataKey, FeeConfig, StreamFeeRecord};

pub const BUMP_LEDGERS_PERSISTENT: u32 = 518_400; // ~30 days at 5s/ledger
pub const MIN_TTL_LEDGERS_PERSISTENT: u32 = 120_960; // ~7 days at 5s/ledger

pub fn load_config(env: &Env) -> Result<FeeConfig, FeeError> {
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .ok_or(FeeError::NotInitialized)
}

pub fn save_config(env: &Env, config: &FeeConfig) {
    env.storage().instance().set(&DataKey::Config, config);
}

pub fn get_stream_fee(env: &Env, stream_id: u64) -> Option<StreamFeeRecord> {
    let key = DataKey::StreamFee(stream_id);
    let record: Option<StreamFeeRecord> = env.storage().persistent().get(&key);
    if record.is_some() {
        env.storage().persistent().extend_ttl(
            &key,
            MIN_TTL_LEDGERS_PERSISTENT,
            BUMP_LEDGERS_PERSISTENT,
        );
    }
    record
}

pub fn set_stream_fee(env: &Env, record: &StreamFeeRecord) {
    let key = DataKey::StreamFee(record.stream_id);
    env.storage().persistent().set(&key, record);
    env.storage().persistent().extend_ttl(
        &key,
        MIN_TTL_LEDGERS_PERSISTENT,
        BUMP_LEDGERS_PERSISTENT,
    );
}

pub fn get_total_protocol_fees(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::TotalProtocolFees)
        .unwrap_or(0)
}

pub fn set_total_protocol_fees(env: &Env, amount: i128) {
    env.storage()
        .instance()
        .set(&DataKey::TotalProtocolFees, &amount);
}

pub fn get_total_keeper_fees(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::TotalKeeperFees)
        .unwrap_or(0)
}

pub fn set_total_keeper_fees(env: &Env, amount: i128) {
    env.storage()
        .instance()
        .set(&DataKey::TotalKeeperFees, &amount);
}

pub fn set_pending_admin(env: &Env, nominee: &Address) {
    env.storage()
        .instance()
        .set(&DataKey::PendingAdmin, nominee);
}

pub fn get_pending_admin(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::PendingAdmin)
}

pub fn clear_pending_admin(env: &Env) {
    env.storage().instance().remove(&DataKey::PendingAdmin);
}
