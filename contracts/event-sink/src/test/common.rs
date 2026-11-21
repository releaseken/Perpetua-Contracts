//! Shared test harness for the event-sink contract.

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{Address, Bytes, Env, Symbol};

use crate::{EventSink, EventSinkClient, SinkEntry};

pub const T0: u64 = 1_700_000_000;

pub struct Harness<'a> {
    pub env: Env,
    pub client: EventSinkClient<'a>,
    pub contract_id: Address,
    pub admin: Address,
    pub emitter: Address,
    pub other: Address,
}

impl<'a> Harness<'a> {
    pub fn new() -> Harness<'a> {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(T0);

        let contract_id = env.register(EventSink, ());
        let client = EventSinkClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let emitter = Address::generate(&env);
        let other = Address::generate(&env);

        client.init(&admin);
        client.register_emitter(&emitter);

        Harness {
            client,
            contract_id,
            admin,
            emitter,
            other,
            env,
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

    /// Build a minimal valid `SinkEntry`.
    pub fn entry(&self, seq: u64) -> SinkEntry {
        SinkEntry {
            source_contract: self.emitter.clone(),
            topic: Symbol::new(&self.env, "test_event"),
            sequence: seq,
            payload: Bytes::from_slice(&self.env, b"hello"),
        }
    }

    /// Build an entry with a custom payload.
    pub fn entry_with_payload(&self, seq: u64, payload: &[u8]) -> SinkEntry {
        SinkEntry {
            source_contract: self.emitter.clone(),
            topic: Symbol::new(&self.env, "test_event"),
            sequence: seq,
            payload: Bytes::from_slice(&self.env, payload),
        }
    }
}
