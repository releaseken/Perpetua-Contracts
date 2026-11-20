use soroban_sdk::{token::TokenClient, Address, Env};
use soroban_sdk::token::StellarAssetClient;

use crate::{CliffVesting, CliffVestingClient, VestingSchedule};
use crate::accrual;

/// Non-zero epoch so tests never depend on `now == 0`.
pub const T0: u64 = 1_700_000_000;
pub const DAY: u64 = 86_400;
pub const YEAR: u64 = 365 * DAY;
/// One whole USDC token (7 decimals).
pub const ONE: i128 = 10_000_000;

pub struct Harness<'a> {
    pub env: Env,
    pub client: CliffVestingClient<'a>,
    pub contract_id: Address,
    pub admin: Address,
    pub creator: Address,
    pub revoker: Address,
    pub pauser: Address,
    pub grantor: Address,
    pub beneficiary: Address,
    pub other: Address,
    pub token: Address,
    pub token_client: TokenClient<'a>,
    pub token_admin: StellarAssetClient<'a>,
}

impl<'a> Harness<'a> {
    pub fn new() -> Harness<'a> {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(T0);

        let contract_id = env.register(CliffVesting, ());
        let client = CliffVestingClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let creator = Address::generate(&env);
        let revoker = Address::generate(&env);
        let pauser = Address::generate(&env);
        let grantor = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let other = Address::generate(&env);

        // Set up a SAC token.
        let issuer = Address::generate(&env);
        let asset = env.register_stellar_asset_contract_v2(issuer);
        let token = asset.address();
        let token_admin = StellarAssetClient::new(&env, &token);
        token_admin.mint(&grantor, &(1_000_000 * ONE));
        token_admin.mint(&other, &(1_000_000 * ONE));
        let token_client = TokenClient::new(&env, &token);

        client.init(&admin, &creator, &revoker, &pauser);

        Harness {
            client,
            contract_id,
            admin,
            creator,
            revoker,
            pauser,
            grantor,
            beneficiary,
            other,
            token,
            token_client,
            token_admin,
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

    pub fn now(&self) -> u64 {
        self.env.ledger().timestamp()
    }

    /// Create a simple schedule: no cliff, full revocability, starts now.
    pub fn create_simple(&self, deposit: i128, duration: u64) -> u64 {
        let start = self.now();
        self.client.create_schedule(
            &self.creator,
            &self.grantor,
            &self.beneficiary,
            &self.token,
            &deposit,
            &start,
            &(start + duration),
            &start, // no cliff
        )
        .unwrap()
    }

    /// Create a schedule with a cliff.
    pub fn create_with_cliff(
        &self,
        deposit: i128,
        duration: u64,
        cliff_offset: u64,
    ) -> u64 {
        let start = self.now();
        self.client.create_schedule(
            &self.creator,
            &self.grantor,
            &self.beneficiary,
            &self.token,
            &deposit,
            &start,
            &(start + duration),
            &(start + cliff_offset),
        )
        .unwrap()
    }

    pub fn get(&self, id: u64) -> VestingSchedule {
        self.client.get_schedule(&id).unwrap()
    }

    pub fn balance(&self, who: &Address) -> i128 {
        self.token_client.balance(who)
    }

    pub fn pool(&self) -> i128 {
        self.token_client.balance(&self.contract_id)
    }

    pub fn ttl_of(&self, id: u64) -> u32 {
        self.env.as_contract(&self.contract_id, || {
            self.env
                .storage()
                .persistent()
                .get_ttl(&crate::DataKey::Schedule(id))
        })
    }

    pub fn max_achievable_ttl(&self) -> u32 {
        self.env
            .as_contract(&self.contract_id, || self.env.storage().max_ttl())
    }

    // -----------------------------------------------------------------------
    // Invariant assertions
    // -----------------------------------------------------------------------

    /// Assert the core accrual invariants for every schedule at the current time.
    ///
    /// I1: `0 <= withdrawn <= vested <= deposited`
    /// I4: `vested + refundable == deposited` (conservation, exact)
    pub fn assert_invariants(&self) {
        let now = self.now();
        for id in 0..self.client.schedule_count() {
            let s = self.client.get_schedule(&id).unwrap();
            let v = accrual::vested(&s, now).expect("vested must not overflow");
            let r = accrual::refundable(&s, now).expect("refundable must not overflow");

            assert!(s.withdrawn >= 0, "schedule {id}: negative withdrawn");
            assert!(s.deposited >= 0, "schedule {id}: negative deposited");
            assert!(
                s.withdrawn <= v,
                "schedule {id}: I1 violated — withdrawn {} > vested {v}",
                s.withdrawn
            );
            assert!(
                v <= s.deposited,
                "schedule {id}: I1 violated — vested {v} > deposited {}",
                s.deposited
            );
            assert_eq!(
                v + r,
                s.deposited,
                "schedule {id}: I4 violated — conservation broken"
            );
        }
    }

    /// Assert the pool holds at least the total outstanding liability.
    pub fn assert_pool_invariant(&self) {
        self.assert_invariants();
        let mut total: i128 = 0;
        for id in 0..self.client.schedule_count() {
            let s = self.client.get_schedule(&id).unwrap();
            if s.token != self.token {
                continue;
            }
            total += accrual::liability(&s).expect("liability must not overflow");
        }
        let pool = self.pool();
        assert!(
            pool >= total,
            "pool invariant violated: pool {pool} < total liability {total}"
        );
    }
}
