//! Shared test harness and helpers for fee-accrual tests.

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{Address, Env};

use crate::{FeeAccrualContract, FeeAccrualContractClient};

pub const T0: u64 = 1_700_000_000;

pub struct Harness<'a> {
    pub env: Env,
    pub client: FeeAccrualContractClient<'a>,
    pub contract_id: Address,
    pub admin: Address,
    pub fee_admin: Address,
    pub fee_collector: Address,
    pub emergency_admin: Address,
    pub fee_recipient: Address,
}

impl<'a> Harness<'a> {
    pub fn new() -> Self {
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

        // 100 bps protocol fee (1.00%), 2000 bps keeper split (20% of protocol fee)
        client.init(
            &admin,
            &fee_admin,
            &fee_collector,
            &emergency_admin,
            &fee_recipient,
            &100,
            &2000,
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
        }
    }
}
