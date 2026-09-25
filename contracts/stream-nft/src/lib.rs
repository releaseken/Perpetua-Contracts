#![no_std]
//! # Perpetua stream-NFT — represent stream recipient claims as tokens (#123)
//!
//! Thin non-fungible wrapper over a single Perpetua stream instance. A stream
//! that was created with `transferable = true` can be minted into an NFT keyed
//! by its `stream_id`; the NFT owner is then the stream's effective claimant,
//! and transferring the NFT reassigns the underlying claim via the stream
//! contract's `transfer_recipient`.
//!
//! ## Trust model
//!
//! Ownership records live here; the authoritative claim lives in the stream
//! contract. Minting is permissionless recording — anyone may tokenize a
//! `stream_id` for themselves — but it can never steal a claim, because every
//! state-changing path runs *through the stream contract*:
//!
//! * `transfer` carries the seller's auth to `transfer_recipient`, whose
//!   `recipient.require_auth()` gate only passes when the NFT owner really is
//!   the stream's current recipient.
//! * Withdrawals, cancels and pauses are untouched here — they still act on the
//!   stream directly under its own auth rules, so a squatted token that cannot
//!   be transferred is harmless to the real recipient.
//!
//! ## Token interface
//!
//! The surface mirrors the standard Soroban NFT/SEP-11 shape: `owner_of`,
//! `balance_of`, `transfer`, plus ERC-721-style `name`/`symbol`/`decimals`
//! metadata. Approvals (`approve` / `transfer_from`), full owner-token
//! enumeration and an indexer-friendly event surface are follow-up work.
//!
//! `stream_id` doubles as the NFT token id; it is `u64` and never reused by the
//! stream contract, so it is a stable on-chain handle for marketplaces.

#[cfg(all(target_family = "wasm", not(target_os = "none")))]
compile_error!("stream-NFT production WASM must be built for wasm32v1-none.");

#[cfg(test)]
extern crate std;

use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, vec, Address, Env, FromVal, IntoVal,
    String, Symbol, Val, Vec,
};

/// NFT metadata and the Perpetua stream instance this wrapper fronts.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// The `FluxoraStream` contract whose claims are tokenized.
    pub stream: Address,
    /// Token name, e.g. "Perpetua Stream".
    pub name: String,
    /// Token symbol, e.g. "PSTREAM".
    pub symbol: String,
    /// Fixed per the NFT standard.
    pub decimals: u32,
}

#[contracttype]
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataKey {
    Config = 0,
    /// token id (`stream_id`) -> current NFT owner.
    Owner(u64) = 1,
    /// owner -> number of claim tokens held.
    Balance(Address) = 2,
    /// Number of distinct tokens minted.
    Supply = 3,
}

#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    AlreadyInitialized,
    NotInitialized,
    AlreadyMinted,
    TokenNotFound,
    NotOwner,
    StreamRejected,
}

const STREAM_TRANSFER_RECIPIENT: &str = "transfer_recipient";

fn get_config(env: &Env) -> Config {
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .expect("stream-NFT contract not initialized")
}

fn bump(env: &Env, key: &DataKey) {
    let live = env.storage().max_ttl();
    match key {
        DataKey::Config => env.storage().instance().extend_ttl(live, live),
        _ => env.storage().persistent().extend_ttl(key, live, live),
    }
}

/// Invoke `FluxoraStream::transfer_recipient(stream_id, to)`, panicking with
/// [`Error::StreamRejected`] if the stream contract refuses (not the
/// recipient, or the stream is not `transferable`).
fn transfer_claim(env: &Env, config: &Config, stream_id: u64, to: &Address) {
    let ret: Val = env.invoke_contract(
        &config.stream,
        &Symbol::new(env, STREAM_TRANSFER_RECIPIENT),
        vec![&env, (&stream_id).into_val(env), to.into_val(env)],
    );
    // `Result<(), Error>` encodes as ScVec [Symbol("Ok"), Void] | [Symbol("Err"), …].
    let parts: Vec<Val> = FromVal::from_val(env, ret);
    let ok = Symbol::new(env, "Ok").into_val(env);
    let accepted = parts.get(0).is_some_and(|tag| tag == ok);
    if !accepted {
        panic_with_error!(env, Error::StreamRejected);
    }
}

#[contract]
pub struct StreamNft;

#[contractimpl]
impl StreamNft {
    /// One-time initialization: bind this wrapper to a Perpetua stream
    /// instance and fix the token metadata.
    pub fn init(env: Env, stream: Address, name: String, symbol: String) {
        if env.storage().instance().has(&DataKey::Config) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        env.storage().instance().set(
            &DataKey::Config,
            &Config {
                stream,
                name,
                symbol,
                decimals: 0,
            },
        );
    }

    pub fn name(env: Env) -> String {
        get_config(&env).name
    }

    pub fn symbol(env: Env) -> String {
        get_config(&env).symbol
    }

    pub fn decimals(env: Env) -> u32 {
        get_config(&env).decimals
    }

    /// The Perpetua stream instance this wrapper fronts.
    pub fn stream_contract(env: Env) -> Address {
        get_config(&env).stream
    }

    /// Current owner of the token minted for `stream_id`, or `None`.
    pub fn owner_of(env: Env, stream_id: u64) -> Option<Address> {
        let owner = env.storage().persistent().get(&DataKey::Owner(stream_id));
        if owner.is_some() {
            bump(&env, &DataKey::Owner(stream_id));
        }
        owner
    }

    /// Number of claim tokens held by `owner`.
    pub fn balance_of(env: Env, owner: Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::Balance(owner))
            .unwrap_or(0)
    }

    /// Number of distinct claim tokens issued so far.
    pub fn total_supply(env: Env) -> u64 {
        env.storage().instance().get(&DataKey::Supply).unwrap_or(0)
    }

    /// Tokenize a stream claim for the caller.
    ///
    /// Records `caller` as the NFT owner for `stream_id`. This is deliberately
    /// not gated on the caller being the current recipient: the stream
    /// contract's auth is the real lock, exercised on `transfer` and on any
    /// withdrawal the recipient performs on-chain. A token minted over someone
    /// else's claim cannot be transferred (the stream rejects the auth), so it
    /// is inert.
    pub fn mint(env: Env, stream_id: u64) {
        let caller = env.caller();
        caller.require_auth();

        if env.storage().persistent().has(&DataKey::Owner(stream_id)) {
            panic_with_error!(&env, Error::AlreadyMinted);
        }
        let supply = env.storage().instance().get(&DataKey::Supply).unwrap_or(0);
        if supply == u64::MAX {
            panic_with_error!(&env, Error::TokenNotFound);
        }

        env.storage()
            .persistent()
            .set(&DataKey::Owner(stream_id), &caller);
        bump(&env, &DataKey::Owner(stream_id));
        let balance: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::Balance(caller.clone()))
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&DataKey::Balance(caller.clone()), &(balance + 1));
        bump(&env, &DataKey::Balance(caller));
        env.storage().instance().set(&DataKey::Supply, &(supply + 1));
    }

    /// Transfer the claim token for `stream_id`, and reassign the underlying
    /// stream to `to`.
    ///
    /// Authorizes the current owner and forwards their auth into the stream
    /// contract's `transfer_recipient` call, so the reassignment only lands if
    /// the owner really is the stream's current recipient (and the stream was
    /// created `transferable`). Storage moves are recorded after the external
    /// call returns `Ok`; a rejection unwinds the whole transaction.
    pub fn transfer(env: Env, stream_id: u64, to: Address) {
        let config = get_config(&env);
        let from = match env.storage().persistent().get(&DataKey::Owner(stream_id)) {
            Some(o) => o,
            None => panic_with_error!(&env, Error::TokenNotFound),
        };
        from.require_auth();

        transfer_claim(&env, &config, stream_id, &to);

        env.storage()
            .persistent()
            .set(&DataKey::Owner(stream_id), &to);
        bump(&env, &DataKey::Owner(stream_id));

        let from_balance: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::Balance(from.clone()))
            .unwrap_or(0);
        if from_balance > 0 {
            env.storage()
                .persistent()
                .set(&DataKey::Balance(from.clone()), &(from_balance - 1));
        }
        let to_balance: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::Balance(to.clone()))
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&DataKey::Balance(to.clone()), &(to_balance + 1));
        bump(&env, &DataKey::Balance(to));
    }
}