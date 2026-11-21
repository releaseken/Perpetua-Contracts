//! Shared test harness for stream-nft.

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{Address, Env, String};

use crate::{StreamNft, StreamNftClient};

pub const T0: u64 = 1_700_000_000;
pub const DAY: u64 = 86_400;

pub struct Harness<'a> {
    pub env: Env,
    pub client: StreamNftClient<'a>,
    pub contract_id: Address,
    pub stream_contract: Address,
    pub user1: Address,
    pub user2: Address,
    pub user3: Address,
}

impl<'a> Harness<'a> {
    pub fn new() -> Harness<'a> {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(T0);

        let contract_id = env.register(StreamNft, ());
        let client = StreamNftClient::new(&env, &contract_id);

        let stream_contract = Address::generate(&env);
        let user1 = Address::generate(&env);
        let user2 = Address::generate(&env);
        let user3 = Address::generate(&env);

        let name = String::from_str(&env, "Perpetua Stream NFT");
        let symbol = String::from_str(&env, "PSTREAM");

        client.init(&stream_contract, &name, &symbol);

        Harness {
            env,
            client,
            contract_id,
            stream_contract,
            user1,
            user2,
            user3,
        }
    }

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

    pub fn set_timestamp(&self, timestamp: u64) {
        self.env.ledger().set_timestamp(timestamp);
    }

    pub fn now(&self) -> u64 {
        self.env.ledger().timestamp()
    }
}
