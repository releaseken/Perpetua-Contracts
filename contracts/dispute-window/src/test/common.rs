//! Shared test harness for dispute-window tests.

use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Env};

use crate::{DisputeWindow, DisputeWindowClient};

/// Non-zero epoch so that timestamp=0 never masks underflow bugs.
pub const T0: u64 = 1_700_000_000;

/// 1 day in seconds.
pub const DAY: u64 = 86_400;

/// 7 days — a reasonable dispute window deadline in tests.
pub const WEEK: u64 = 7 * DAY;

pub struct Harness<'a> {
    pub env: Env,
    pub client: DisputeWindowClient<'a>,
    pub contract_id: Address,
    pub admin: Address,
    pub arbiter: Address,
    pub claimant: Address,
    pub other: Address,
    /// A mock address used as the stream contract.
    /// Tests that do not exercise actual cross-contract calls use this address.
    pub stream_contract: Address,
}

impl<'a> Harness<'a> {
    /// Fresh environment with all auths mocked, an initialised dispute window,
    /// and the ledger time set to [`T0`].
    pub fn new() -> Harness<'a> {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(T0);

        let contract_id = env.register(DisputeWindow, ());
        let client = DisputeWindowClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let arbiter = Address::generate(&env);
        let claimant = Address::generate(&env);
        let other = Address::generate(&env);
        let stream_contract = Address::generate(&env);

        client.init(&admin, &stream_contract, &arbiter);

        Harness {
            env,
            client,
            contract_id,
            admin,
            arbiter,
            claimant,
            other,
            stream_contract,
        }
    }

    /// Create a harness and immediately open the window with a 7-day deadline.
    pub fn new_open() -> Harness<'a> {
        let h = Self::new();
        h.client.open_window(&(T0 + WEEK));
        h
    }

    /// Advance the ledger clock by `seconds`.
    pub fn advance(&self, seconds: u64) {
        let info = self.env.ledger().get();
        self.env.ledger().set_timestamp(info.timestamp + seconds);
    }

    /// Current ledger timestamp.
    pub fn now(&self) -> u64 {
        self.env.ledger().timestamp()
    }

    /// Submit a simple claim and return success/failure.
    pub fn submit(&self, stream_id: u64, amount: i128) {
        self.client
            .submit_claim(&self.claimant, &stream_id, &amount);
    }

    /// Approve a claim.
    pub fn approve(&self, stream_id: u64, amount: i128) {
        self.client.decide_claim(&stream_id, &true, &amount, &None);
    }

    /// Deny a claim.
    pub fn deny(&self, stream_id: u64) {
        self.client.decide_claim(&stream_id, &false, &0, &None);
    }
}
