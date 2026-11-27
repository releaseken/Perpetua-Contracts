#![no_std]
//! # Perpetua stream-NFT — represent stream recipient claims as tokens (#123, #370)
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
//! ## Invariant Assertions and Re-entrancy Protection (#370)
//!
//! To prevent state desync and cross-contract re-entrancy during external calls
//! into `FluxoraStream::transfer_recipient`, `StreamNft` enforces:
//! 1. **Re-entrancy Guard**: Instance-level lock (`DataKey::Executing`) preventing
//!    nested or re-entrant invocations of state-modifying operations.
//! 2. **Check-Effects-Interactions (CEI)**: State is updated or guarded before
//!    external invocations, and rollbacks on failure are strictly transactional.
//! 3. **Checked Arithmetic**: All balance and supply updates use checked operations
//!    (`checked_add`, `checked_sub`) panicking with explicit errors on overflow.
//! 4. **Self-Transfer & Repeated-Transfer Guard**: Rejecting transfers where `from == to`
//!    to prevent balance corruption and redundant external calls.
//! 5. **Invariant In-Flight Assertions**: Verifying non-zero supply bounds,
//!    non-underflowing balances, and ledger footprint cleanup.

#[cfg(all(target_family = "wasm", not(target_os = "none")))]
compile_error!("stream-NFT production WASM must be built for wasm32v1-none.");

#[cfg(test)]
extern crate std;

pub mod invariants;

#[cfg(test)]
mod test;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, Address, Env, IntoVal,
    String, Symbol, Val,
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
    /// Re-entrancy guard flag: set while state-modifying external operations are active.
    Executing = 4,
}

#[contracterror]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    AlreadyMinted = 3,
    TokenNotFound = 4,
    NotOwner = 5,
    StreamRejected = 6,
    ReentrancyGuard = 7,
    InvariantViolation = 8,
    Overflow = 9,
    RepeatedTransfer = 10,
}

const STREAM_TRANSFER_RECIPIENT: &str = "transfer_recipient";

fn get_config(env: &Env) -> Config {
    bump(env, &DataKey::Config);
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
}

fn bump(env: &Env, key: &DataKey) {
    let live = env.storage().max_ttl();
    match key {
        DataKey::Config | DataKey::Supply | DataKey::Executing => {
            env.storage().instance().extend_ttl(live, live)
        }
        _ => env.storage().persistent().extend_ttl(key, live, live),
    }
}

fn is_executing(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&DataKey::Executing)
        .unwrap_or(false)
}

fn set_executing(env: &Env) {
    env.storage().instance().set(&DataKey::Executing, &true);
}

fn clear_executing(env: &Env) {
    env.storage().instance().remove(&DataKey::Executing);
}

/// Invoke `FluxoraStream::transfer_recipient(stream_id, to)`, panicking with
/// [`Error::StreamRejected`] if the stream contract refuses (not the
/// recipient, or the stream is not `transferable`).
fn transfer_claim(env: &Env, config: &Config, stream_id: u64, to: &Address) {
    let result: Result<(), Val> = env.invoke_contract(
        &config.stream,
        &Symbol::new(env, STREAM_TRANSFER_RECIPIENT),
        (stream_id, to.clone()).into_val(env),
    );
    if result.is_err() {
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
        env.storage().instance().set(&DataKey::Supply, &0u64);
        bump(&env, &DataKey::Config);
        bump(&env, &DataKey::Supply);
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
        let balance = env
            .storage()
            .persistent()
            .get(&DataKey::Balance(owner.clone()))
            .unwrap_or(0);
        if balance > 0 {
            bump(&env, &DataKey::Balance(owner));
        }
        balance
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
        if is_executing(&env) {
            panic_with_error!(&env, Error::ReentrancyGuard);
        }
        set_executing(&env);

        let caller = env.caller();
        caller.require_auth();

        let _config = get_config(&env);

        if env.storage().persistent().has(&DataKey::Owner(stream_id)) {
            clear_executing(&env);
            panic_with_error!(&env, Error::AlreadyMinted);
        }
        let supply: u64 = env.storage().instance().get(&DataKey::Supply).unwrap_or(0);
        let new_supply = match supply.checked_add(1) {
            Some(s) => s,
            None => {
                clear_executing(&env);
                panic_with_error!(&env, Error::Overflow);
            }
        };

        env.storage()
            .persistent()
            .set(&DataKey::Owner(stream_id), &caller);
        bump(&env, &DataKey::Owner(stream_id));

        let balance: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::Balance(caller.clone()))
            .unwrap_or(0);
        let new_balance = match balance.checked_add(1) {
            Some(b) => b,
            None => {
                clear_executing(&env);
                panic_with_error!(&env, Error::Overflow);
            }
        };

        env.storage()
            .persistent()
            .set(&DataKey::Balance(caller.clone()), &new_balance);
        bump(&env, &DataKey::Balance(caller));

        env.storage()
            .instance()
            .set(&DataKey::Supply, &new_supply);
        bump(&env, &DataKey::Supply);

        clear_executing(&env);
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
        if is_executing(&env) {
            panic_with_error!(&env, Error::ReentrancyGuard);
        }
        set_executing(&env);

        let config = get_config(&env);
        let from = match env.storage().persistent().get(&DataKey::Owner(stream_id)) {
            Some(o) => o,
            None => {
                clear_executing(&env);
                panic_with_error!(&env, Error::TokenNotFound);
            }
        };

        if from == to {
            clear_executing(&env);
            panic_with_error!(&env, Error::RepeatedTransfer);
        }

        from.require_auth();

        let from_balance: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::Balance(from.clone()))
            .unwrap_or(0);
        if from_balance == 0 {
            clear_executing(&env);
            panic_with_error!(&env, Error::InvariantViolation);
        }

        let to_balance: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::Balance(to.clone()))
            .unwrap_or(0);
        let new_to_balance = match to_balance.checked_add(1) {
            Some(b) => b,
            None => {
                clear_executing(&env);
                panic_with_error!(&env, Error::Overflow);
            }
        };

        // Call out to external stream contract. If it fails, Soroban unwinds host state.
        transfer_claim(&env, &config, stream_id, &to);

        // Update ownership
        env.storage()
            .persistent()
            .set(&DataKey::Owner(stream_id), &to);
        bump(&env, &DataKey::Owner(stream_id));

        // Update from balance
        let new_from_balance = match from_balance.checked_sub(1) {
            Some(b) => b,
            None => {
                clear_executing(&env);
                panic_with_error!(&env, Error::InvariantViolation);
            }
        };

        if new_from_balance == 0 {
            env.storage().persistent().remove(&DataKey::Balance(from));
        } else {
            env.storage()
                .persistent()
                .set(&DataKey::Balance(from.clone()), &new_from_balance);
            bump(&env, &DataKey::Balance(from));
        }

        // Update to balance
        env.storage()
            .persistent()
            .set(&DataKey::Balance(to.clone()), &new_to_balance);
        bump(&env, &DataKey::Balance(to));

        clear_executing(&env);
    }
}

#[cfg(test)]
mod test;
