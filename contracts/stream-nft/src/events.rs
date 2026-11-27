//! Events emitted by the Stream NFT contract.

use soroban_sdk::{symbol_short, Address, Env};

/// Emitted when a fractional or full withdrawal is executed via the NFT wrapper.
pub fn fractional_withdrawn(
    env: &Env,
    stream_id: u64,
    recipient: &Address,
    amount: i128,
    remainder: i128,
) {
    let topics = (symbol_short!("withdrawn"), stream_id, recipient.clone());
    env.events().publish(topics, (amount, remainder));
}
