use soroban_sdk::{Address, Env};

use crate::{PauseClock, PauseClockClient};

/// Non-zero epoch so tests never accidentally depend on `now == 0`.
pub const T0: u64 = 1_700_000_000;

pub struct Harness<'a> {
    pub env: Env,
    pub client: PauseClockClient<'a>,
    pub contract_id: Address,
    pub admin: Address,
    pub stream_contract: Address,
    pub other: Address,
}

impl<'a> Harness<'a> {
    pub fn new() -> Harness<'a> {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(T0);

        let contract_id = env.register(PauseClock, ());
        let client = PauseClockClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let stream_contract = Address::generate(&env);
        let other = Address::generate(&env);

        // Initialize the contract.
        client.init(&admin, &stream_contract);

        Harness {
            client,
            contract_id,
            admin,
            stream_contract,
            other,
            env,
        }
    }

    /// Advance both clock and sequence consistently.
    pub fn advance(&self, seconds: u64) {
        let info = self.env.ledger().get();
        self.env
            .ledger()
            .set_timestamp(info.timestamp + seconds);
        // 5 s / ledger nominal.
        let ledgers = ((seconds + 4) / 5) as u32;
        self.env
            .ledger()
            .set_sequence_number(info.sequence_number.saturating_add(ledgers));
    }

    pub fn now(&self) -> u64 {
        self.env.ledger().timestamp()
    }

    /// True when the persistent entry for `stream_id` exists in storage.
    pub fn entry_exists(&self, stream_id: u64) -> bool {
        self.env.as_contract(&self.contract_id, || {
            self.env
                .storage()
                .persistent()
                .has(&crate::DataKey::PausedTotal(stream_id))
        })
    }

    /// Current TTL of the persistent entry for `stream_id`, in ledgers.
    pub fn ttl_of(&self, stream_id: u64) -> u32 {
        self.env.as_contract(&self.contract_id, || {
            self.env
                .storage()
                .persistent()
                .get_ttl(&crate::DataKey::PausedTotal(stream_id))
        })
    }

    pub fn max_achievable_ttl(&self) -> u32 {
        self.env
            .as_contract(&self.contract_id, || self.env.storage().max_ttl())
    }
}
