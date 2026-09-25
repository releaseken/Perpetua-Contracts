#![cfg(test)]

use fluxora_factory::StreamCreated;
use soroban_sdk::{
    testutils::Events as _,
    xdr::ContractEventBody,
    Address, Env, Symbol,
};

#[test]
fn stream_created_schema_matches_factory_topics() {
    let env = Env::default();
    let factory_id = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    env.as_contract(&factory_id, || {
        StreamCreated {
            factory_id: factory_id.clone(),
            sender: sender.clone(),
            recipient: recipient.clone(),
        }
        .publish(&env);
    });

    let events = env.events().all().events().to_vec();
    assert_eq!(events.len(), 1);
    let ContractEventBody::V0(event) = &events[0].body;
    let topics = &event.topics;
    assert_eq!(topics.len(), 4);
    assert_eq!(Symbol::try_from_val(&env, &topics[0]).unwrap(), Symbol::new(&env, "stream_created"));
    assert_eq!(Address::try_from_val(&env, &topics[1]).unwrap(), factory_id);
    assert_eq!(Address::try_from_val(&env, &topics[2]).unwrap(), sender);
    assert_eq!(Address::try_from_val(&env, &topics[3]).unwrap(), recipient);
}