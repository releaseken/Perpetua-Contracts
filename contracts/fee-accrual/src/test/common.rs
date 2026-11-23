//! Shared test harness for the `fee-accrual` contract.

use soroban_sdk::testutils::{
    storage::{Instance as _, Persistent as _},
    Address as _, Ledger as _,
};
use soroban_sdk::{Address, Env, String};

use crate::{DataKey, FeeAccrualContract, FeeAccrualContractClient};

/// Non-zero starting ledger timestamp.
pub const T0: u64 = 1_700_000_000;

/// Default protocol fee: 200 bps (2.00%).
pub const DEFAULT_PROTOCOL_FEE_BPS: u32 = 200;

/// Default keeper split: 2_500 bps (25.00% of total fee).
pub const DEFAULT_KEEPER_SPLIT_BPS: u32 = 2_500;

pub struct Harness<'a> {
    pub env: Env,
    pub client: FeeAccrualContractClient<'a>,
    pub contract_id: Address,
    pub admin: Address,
    pub fee_admin: Address,
    pub fee_collector: Address,
    pub emergency_admin: Address,
    pub fee_recipient: Address,
    pub other: Address,
}

impl<'a> Harness<'a> {
    pub fn new() -> Harness<'a> {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(T0);

        let contract_id = env.register(FeeAccrualContract, ());
        let client = FeeAccrualContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let fee_admin = Address::generate(&env);
        let fee_collector = Address::generate(&env);
        let emergency_admin = Address::generate(&env);
        let fee_recipient = Address::generate(&env);
        let other = Address::generate(&env);

        client.init(
            &admin,
            &fee_admin,
            &fee_collector,
            &emergency_admin,
            &fee_recipient,
            &DEFAULT_PROTOCOL_FEE_BPS,
            &DEFAULT_KEEPER_SPLIT_BPS,
        );

        Harness {
            env,
            client,
            contract_id,
            admin,
            fee_admin,
            fee_collector,
            emergency_admin,
            fee_recipient,
            other,
        }
    }

    /// Advance both ledger wall-clock timestamp and sequence number consistently.
    pub fn advance(&self, seconds: u64) {
        let info = self.env.ledger().get();
        self.env
            .ledger()
            .set_timestamp(info.timestamp.saturating_add(seconds));
        let ledgers = ((seconds.saturating_add(4)) / 5) as u32;
        self.env
            .ledger()
            .set_sequence_number(info.sequence_number.saturating_add(ledgers));
    }

    /// Advance ledger sequence directly by `ledgers`.
    pub fn advance_ledgers(&self, ledgers: u32) {
        let info = self.env.ledger().get();
        self.env
            .ledger()
            .set_sequence_number(info.sequence_number.saturating_add(ledgers));
        self.env
            .ledger()
            .set_timestamp(info.timestamp.saturating_add(ledgers as u64 * 5));
    }

    /// True if persistent `DataKey::StreamFee(stream_id)` exists in contract storage.
    pub fn entry_exists(&self, stream_id: u64) -> bool {
        self.env.as_contract(&self.contract_id, || {
            self.env
                .storage()
                .persistent()
                .has(&DataKey::StreamFee(stream_id))
        })
    }

    /// Current remaining TTL in ledgers for `DataKey::StreamFee(stream_id)`.
    pub fn ttl_of(&self, stream_id: u64) -> u32 {
        self.env.as_contract(&self.contract_id, || {
            self.env
                .storage()
                .persistent()
                .get_ttl(&DataKey::StreamFee(stream_id))
        })
    }

    /// Current remaining TTL in ledgers for the contract's instance storage.
    pub fn instance_ttl(&self) -> u32 {
        self.env.as_contract(&self.contract_id, || {
            self.env.storage().instance().get_ttl()
        })
    }

    /// Maximum allowable TTL on the current network configuration.
    pub fn max_ttl(&self) -> u32 {
        self.env
            .as_contract(&self.contract_id, || self.env.storage().max_ttl())
    }

    /// Target persistent TTL floor clamped to `max_ttl()`.
    pub fn expected_entry_ttl(&self) -> u32 {
        crate::storage::MIN_ENTRY_TTL_LEDGERS.min(self.max_ttl())
    }

    /// All-zero Stellar address for negative validation tests.
    pub fn zero_address(&self) -> Address {
        Address::from_string(&String::from_str(
            &self.env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        ))
    }
}
