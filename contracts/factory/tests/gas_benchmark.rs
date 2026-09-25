use fluxora_factory::{FactoryError, FluxoraFactory, FluxoraFactoryClient};
use fluxora_stream::{FluxoraStream, FluxoraStreamClient};
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger as _},
    token::StellarAssetClient,
    vec, Address, Env, IntoVal, Symbol,
};

const START_TIME: u64 = 1_700_000_000;
const DURATION: u64 = 1_000;
const DEPOSIT: i128 = 1_000_000;

#[contract]
struct Forwarder;

#[contractimpl]
impl Forwarder {
    #[allow(clippy::too_many_arguments)]
    pub fn create_stream(
        env: Env,
        stream_contract: Address,
        sender: Address,
        recipient: Address,
        token: Address,
        deposit: i128,
        start_time: u64,
        end_time: u64,
        cliff_time: u64,
        cancellable: bool,
        pausable: bool,
        transferable: bool,
    ) -> u64 {
        sender.require_auth();
        let args = vec![
            &env,
            sender.into_val(&env),
            recipient.into_val(&env),
            token.into_val(&env),
            deposit.into_val(&env),
            start_time.into_val(&env),
            end_time.into_val(&env),
            cliff_time.into_val(&env),
            cancellable.into_val(&env),
            pausable.into_val(&env),
            transferable.into_val(&env),
        ];
        env.invoke_contract::<u64>(&stream_contract, &Symbol::new(&env, "create_stream"), args)
    }
}

enum Path {
    Direct,
    Forwarding,
    Factory,
}

struct Scenario {
    env: Env,
    factory_id: Address,
    forwarder_id: Address,
    stream_id: Address,
    token: Address,
    sender: Address,
    recipient: Address,
}

impl Scenario {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(START_TIME);

        let factory_id = env.register(FluxoraFactory, ());
        let forwarder_id = env.register(Forwarder, ());
        let stream_id = env.register(FluxoraStream, ());
        let issuer = Address::generate(&env);
        let asset = env.register_stellar_asset_contract_v2(issuer);
        let token = asset.address();
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        StellarAssetClient::new(&env, &token).mint(&sender, &DEPOSIT);

        let admin = Address::generate(&env);
        let factory = FluxoraFactoryClient::new(&env, &factory_id);
        factory.init(&admin, &stream_id, &(DEPOSIT * 2), &DURATION);
        Self {
            env,
            factory_id,
            forwarder_id,
            stream_id,
            token,
            sender,
            recipient,
        }
    }

    fn create(&self, path: Path) -> u64 {
        match path {
            Path::Direct => FluxoraStreamClient::new(&self.env, &self.stream_id).create_stream(
                &self.sender,
                &self.recipient,
                &self.token,
                &DEPOSIT,
                &START_TIME,
                &(START_TIME + DURATION),
                &START_TIME,
                &true,
                &true,
                &true,
            ),
            Path::Forwarding => ForwarderClient::new(&self.env, &self.forwarder_id).create_stream(
                &self.stream_id,
                &self.sender,
                &self.recipient,
                &self.token,
                &DEPOSIT,
                &START_TIME,
                &(START_TIME + DURATION),
                &START_TIME,
                &true,
                &true,
                &true,
            ),
            Path::Factory => FluxoraFactoryClient::new(&self.env, &self.factory_id).create_stream(
                &self.sender,
                &self.recipient,
                &self.token,
                &DEPOSIT,
                &START_TIME,
                &(START_TIME + DURATION),
                &START_TIME,
                &true,
                &true,
                &true,
            ),
        }
    }
}

fn measure(label: &str, path: Path) -> i64 {
    let scenario = Scenario::new();
    assert_eq!(scenario.create(path), 0);
    let resources = scenario.env.cost_estimate().resources();
    std::println!(
        "{label}: instructions={}, memory_bytes={}, disk_reads={}, memory_reads={}, writes={}, write_bytes={}, event_bytes={}",
        resources.instructions,
        resources.mem_bytes,
        resources.disk_read_entries,
        resources.memory_read_entries,
        resources.write_entries,
        resources.write_bytes,
        resources.contract_events_size_bytes,
    );
    resources.instructions
}

#[test]
fn factory_policy_overhead_stays_below_ten_percent() {
    let direct = measure("direct", Path::Direct);
    let forwarding = measure("forwarding", Path::Forwarding);
    let through_factory = measure("factory", Path::Factory);
    let policy_overhead = through_factory - forwarding;
    let wrapper_overhead = through_factory - direct;
    std::println!(
        "policy_overhead={policy_overhead}, wrapper_overhead={wrapper_overhead}, direct={direct}"
    );

    assert!(
        policy_overhead >= 0,
        "factory policy used fewer instructions than forwarding"
    );
    assert!(
        policy_overhead * 100 < forwarding * 10,
        "factory policy overhead {policy_overhead} instructions is not below 10% of forwarding {forwarding}"
    );
}

#[test]
fn paused_factory_rejects_before_cross_contract_call() {
    let scenario = Scenario::new();
    let factory = FluxoraFactoryClient::new(&scenario.env, &scenario.factory_id);
    factory.set_factory_paused(&true);

    let result = factory.try_create_stream(
        &scenario.sender,
        &scenario.recipient,
        &scenario.token,
        &DEPOSIT,
        &START_TIME,
        &(START_TIME + DURATION),
        &START_TIME,
        &true,
        &true,
        &true,
    );

    assert_eq!(result, Err(Ok(FactoryError::CreationPaused)));
}
