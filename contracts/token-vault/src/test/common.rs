//! Shared test harness for the token-vault contract.

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::token::{Client as TokenClient, StellarAssetClient};
use soroban_sdk::{Address, Env};

use crate::math;
use crate::{TokenVault, TokenVaultClient, VaultPosition};

/// Non-zero epoch so tests never depend on `now == 0`.
pub const T0: u64 = 1_700_000_000;
pub const DAY: u64 = 86_400;
/// One whole token unit (7 decimals, same as Stellar native / USDC).
pub const ONE: i128 = 10_000_000;

pub struct Harness<'a> {
    pub env: Env,
    pub client: TokenVaultClient<'a>,
    pub contract_id: Address,
    pub admin: Address,
    pub sender: Address,
    pub beneficiary: Address,
    pub token: Address,
    pub token_client: TokenClient<'a>,
    pub _token_admin: StellarAssetClient<'a>,
}

impl<'a> Harness<'a> {
    pub fn new() -> Harness<'a> {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(T0);

        let contract_id = env.register(TokenVault, ());
        let client = TokenVaultClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let sender = Address::generate(&env);
        let beneficiary = Address::generate(&env);

        let issuer = Address::generate(&env);
        let asset = env.register_stellar_asset_contract_v2(issuer);
        let token = asset.address();
        let token_admin = StellarAssetClient::new(&env, &token);
        token_admin.mint(&sender, &(1_000_000 * ONE));
        let token_client = TokenClient::new(&env, &token);

        client.init(&admin);

        Harness {
            client,
            contract_id,
            admin,
            sender,
            beneficiary,
            token,
            token_client,
            _token_admin: token_admin,
            env,
        }
    }

    pub fn advance(&self, seconds: u64) {
        let info = self.env.ledger().get();
        self.env.ledger().set_timestamp(info.timestamp + seconds);
        let ledgers = crate::storage::seconds_to_ledgers(seconds);
        self.env
            .ledger()
            .set_sequence_number(info.sequence_number.saturating_add(ledgers));
    }

    pub fn now(&self) -> u64 {
        self.env.ledger().get().timestamp
    }

    pub fn warp_to(&self, ts: u64) {
        self.env.ledger().set_timestamp(ts);
    }

    /// Create a position: starts now, no cliff.
    pub fn create_simple(&self, deposit: i128, duration: u64) -> u64 {
        let start = self.now();
        self.client.create_position(
            &self.sender,
            &self.beneficiary,
            &self.token,
            &deposit,
            &start,
            &(start + duration),
            &start,
        )
    }

    /// Create a position with explicit start/end/cliff.
    pub fn create(&self, deposit: i128, start: u64, end: u64, cliff: u64) -> u64 {
        self.client.create_position(
            &self.sender,
            &self.beneficiary,
            &self.token,
            &deposit,
            &start,
            &end,
            &cliff,
        )
    }

    pub fn get(&self, id: u64) -> VaultPosition {
        self.client.get_position(&id)
    }

    pub fn pool(&self) -> i128 {
        self.token_client.balance(&self.contract_id)
    }

    /// Assert the pool holds at least the total outstanding liability.
    pub fn assert_pool_invariant(&self) {
        let mut total: i128 = 0;
        for id in 0..self.client.position_count() {
            let pos = self.get(id);
            if pos.token != self.token {
                continue;
            }
            total += math::liability(&pos).expect("liability must not overflow");
        }
        let pool = self.pool();
        assert!(
            pool >= total,
            "pool invariant violated: pool {pool} < total liability {total}"
        );
    }

    /// Assert conservation for every tracked position at the current time.
    pub fn assert_pool_exact(&self) {
        let now = self.now();
        for id in 0..self.client.position_count() {
            let pos = self.get(id);
            let v = math::vested(&pos, now).expect("vested must not overflow");
            let r = math::refundable(&pos, now).expect("refundable must not overflow");
            assert_eq!(
                v + r,
                pos.deposited,
                "position {id}: I4 violated at now={now}"
            );
        }
    }
}
