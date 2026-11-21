//! Shared test harness and utilities for the fee-accrual contract test suite.

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{Address, Env};

use crate::{FeeAccrual, FeeAccrualClient, FeeConfig};

/// Default nominal timestamp for test setup (Nov 2023 epoch).
pub const T0: u64 = 1_700_000_000;

pub struct Harness<'a> {
    pub env: Env,
    pub client: FeeAccrualClient<'a>,
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
        Self::with_params(100, 2000) // 1% protocol fee, 20% keeper split
    }

    pub fn with_params(protocol_fee_bps: u32, keeper_split_bps: u32) -> Harness<'a> {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(T0);

        let contract_id = env.register(FeeAccrual, ());
        let client = FeeAccrualClient::new(&env, &contract_id);

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
            &protocol_fee_bps,
            &keeper_split_bps,
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

    /// Advance ledger timestamp and sequence number consistently.
    pub fn advance(&self, seconds: u64) {
        let info = self.env.ledger().get();
        self.env
            .ledger()
            .set_timestamp(info.timestamp.saturating_add(seconds));
        let ledgers = ((seconds.saturating_add(4)) / 5).min(u32::MAX as u64) as u32;
        self.env
            .ledger()
            .set_sequence_number(info.sequence_number.saturating_add(ledgers));
    }

    /// Warp ledger timestamp directly to an absolute timestamp.
    pub fn warp_to(&self, timestamp: u64) {
        let current = self.env.ledger().timestamp();
        if timestamp >= current {
            self.advance(timestamp - current);
        } else {
            self.env.ledger().set_timestamp(timestamp);
        }
    }

    pub fn now(&self) -> u64 {
        self.env.ledger().timestamp()
    }

    pub fn config(&self) -> FeeConfig {
        self.client.get_config()
    }
}
