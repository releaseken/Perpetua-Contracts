//! Event definitions for the stream-NFT contract.

use soroban_sdk::{contractevent, Address, Env};

/// A new stream NFT token was minted.
#[contractevent]
pub struct StreamNftMinted {
    #[topic]
    pub stream_id: u64,
    #[topic]
    pub owner: Address,
}

/// A stream NFT token was transferred.
#[contractevent]
pub struct StreamNftTransferred {
    #[topic]
    pub stream_id: u64,
    #[topic]
    pub from: Address,
    #[topic]
    pub to: Address,
}

/// A persistent or instance entry's TTL was extended by automated bump or keeper sweep.
#[contractevent]
pub struct TtlExtended {
    #[topic]
    pub stream_id: u64,
    pub extended_to_ledgers: u32,
}

pub fn mint(env: &Env, stream_id: u64, owner: &Address) {
    StreamNftMinted {
        stream_id,
        owner: owner.clone(),
    }
    .publish(env);
}

pub fn transfer(env: &Env, stream_id: u64, from: &Address, to: &Address) {
    StreamNftTransferred {
        stream_id,
        from: from.clone(),
        to: to.clone(),
    }
    .publish(env);
}

pub fn ttl_extended(env: &Env, stream_id: u64, extended_to_ledgers: u32) {
    TtlExtended {
        stream_id,
        extended_to_ledgers,
    }
    .publish(env);
}
