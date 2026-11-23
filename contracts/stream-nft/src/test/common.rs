//! Shared test harness and mock contracts for stream-nft testing.

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{contract, contractimpl, Address, Env, String};

use crate::{Config, StreamNft, StreamNftClient};

pub const T0: u64 = 1_700_000_000;

#[contract]
pub struct MockStreamContract;

#[contractimpl]
impl MockStreamContract {
    pub fn transfer_recipient(env: Env, stream_id: u64, to: Address) {
        if stream_id == 999_999 {
            panic!("stream rejected: non-existent stream");
        }
        if stream_id == 888_888 {
            panic!("stream rejected: not transferable");
        }
        env.storage().persistent().set(&stream_id, &to);
    }
}

pub struct Harness<'a> {
    pub env: Env,
    pub client: StreamNftClient<'a>,
    pub contract_id: Address,
    pub stream_contract: Address,
    pub alice: Address,
    pub bob: Address,
    pub charlie: Address,
}

impl<'a> Harness<'a> {
    pub fn new() -> Harness<'a> {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(T0);

        let contract_id = env.register(StreamNft, ());
        let client = StreamNftClient::new(&env, &contract_id);

        let stream_contract = env.register(MockStreamContract, ());

        let alice = Address::generate(&env);
        let bob = Address::generate(&env);
        let charlie = Address::generate(&env);

        client.init(
            &stream_contract,
            &String::from_str(&env, "Perpetua Stream NFT"),
            &String::from_str(&env, "PSTREAM"),
        );

        Harness {
            env,
            client,
            contract_id,
            stream_contract,
            alice,
            bob,
            charlie,
        }
    }

    pub fn advance(&self, seconds: u64) {
        let info = self.env.ledger().get();
        self.env.ledger().set_timestamp(info.timestamp + seconds);
        let ledgers = ((seconds + 4) / 5) as u32;
        self.env
            .ledger()
            .set_sequence_number(info.sequence_number.saturating_add(ledgers));
    }

    pub fn now(&self) -> u64 {
        self.env.ledger().timestamp()
    }
}
