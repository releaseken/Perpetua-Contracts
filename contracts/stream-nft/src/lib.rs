//! # Perpetua stream-NFT — represent stream recipient claims as tokens (#123, #368, #371, #373)
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
//!
//! ## Loop Deserialization Optimization (#368)
//!
//! Batch endpoints (`batch_owner_of`, `batch_mint`, `batch_transfer`) optimize
//! Soroban host CPU instructions by:
//! * Enforcing strict batch size limits (`MAX_BATCH_SIZE = 16`).
//! * Deduplicating and validating stream IDs in an upfront scalar pass without
//!   redundant host storage re-queries.
//! * Reading and deserializing each persistent storage record at most once per
//!   batch operation, reusing in-memory vectors for execution.
//! * Aggregating balance mutations in-memory so each affected account incurs
//!   at most one balance read and one balance write rather than N round-trips.
//! * Pruning zero-balance entries completely from storage to conserve ledger footprint.
//!
//! ## Modular Error Handling & Client Observability (#371)
//!
//! All errors are modularized in `crate::errors::Error` with typed category groupings
//! (Group A through F), human-readable client diagnostic messages via `Error::message()`,
//! and classification helpers (`is_cross_contract()`, `is_client_validation()`).
//!
//! ## Streamlined Cross-Contract Invocations in Batch Claims (#373)
//!
//! During high-frequency batch transfers / claims:
//! * Attempts single coalesced batch transfer invocation `batch_transfer_recipient(stream_ids, to)`
//!   on the target stream contract.
//! * If the target contract does not support `batch_transfer_recipient`, gracefully falls back
//!   to individual `transfer_recipient(stream_id, to)` calls.
//! * Coalescing cross-contract sub-invocations collapses N VM context switches into 1, dramatically
//!   reducing metering consumption and call frame stack depth during batch claim settlements.

#[cfg(all(target_family = "wasm", not(target_os = "none")))]
compile_error!("stream-NFT production WASM must be built for wasm32v1-none.");

#[cfg(test)]
extern crate std;

pub mod errors;
pub mod invariants;

pub use errors::Error;

use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, Address, Env, IntoVal, String, Symbol,
    Val, Vec,
};

/// Maximum number of tokens one batch operation may process in a single transaction.
pub const MAX_BATCH_SIZE: u32 = 16;

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

const STREAM_TRANSFER_RECIPIENT: &str = "transfer_recipient";
const STREAM_BATCH_TRANSFER_RECIPIENT: &str = "batch_transfer_recipient";

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
        DataKey::Config => env.storage().instance().extend_ttl(live, live),
        _ => env.storage().persistent().extend_ttl(key, live, live),
    }
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

/// Streamlined cross-contract invocation overhead for batch claims (#373).
///
/// Tries coalesced batch invocation `batch_transfer_recipient(stream_ids, to)` first.
/// If not present or unsupported by older stream contracts, falls back to sequential
/// individual invocations while preserving strict all-or-nothing rollback semantics.
fn transfer_claims_batch(
    env: &Env,
    config: &Config,
    stream_ids: &Vec<u64>,
    to: &Address,
) -> Result<(), Error> {
    // Try single coalesced cross-contract invocation to eliminate N sub-invocation overhead
    let batch_call: Result<u32, Val> = env.invoke_contract(
        &config.stream,
        &Symbol::new(env, STREAM_BATCH_TRANSFER_RECIPIENT),
        (stream_ids.clone(), to.clone()).into_val(env),
    );

    match batch_call {
        Ok(_) => Ok(()),
        Err(_) => {
            // Fall back to sequential transfer_recipient calls for backward compatibility
            // with stream contracts that do not expose batch_transfer_recipient
            let n = stream_ids.len();
            for i in 0..n {
                let stream_id = stream_ids.get_unchecked(i);
                let single_call: Result<(), Val> = env.invoke_contract(
                    &config.stream,
                    &Symbol::new(env, STREAM_TRANSFER_RECIPIENT),
                    (stream_id, to.clone()).into_val(env),
                );
                if single_call.is_err() {
                    return Err(Error::StreamRejected);
                }
            }
            Ok(())
        }
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
        let caller = env.caller();
        caller.require_auth();

        if env.storage().persistent().has(&DataKey::Owner(stream_id)) {
            panic_with_error!(&env, Error::AlreadyMinted);
        }
        let supply = env.storage().instance().get(&DataKey::Supply).unwrap_or(0);
        let new_supply = supply.checked_add(1).unwrap_or_else(|| {
            panic_with_error!(&env, Error::ArithmeticOverflow);
        });

        env.storage()
            .persistent()
            .set(&DataKey::Owner(stream_id), &caller);
        bump(&env, &DataKey::Owner(stream_id));

        let balance: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::Balance(caller.clone()))
            .unwrap_or(0);
        let new_balance = balance.checked_add(1).unwrap_or_else(|| {
            panic_with_error!(&env, Error::ArithmeticOverflow);
        });
        env.storage()
            .persistent()
            .set(&DataKey::Balance(caller.clone()), &new_balance);
        bump(&env, &DataKey::Balance(caller));

        env.storage().instance().set(&DataKey::Supply, &new_supply);
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

        if from == to {
            return;
        }

        transfer_claim(&env, &config, stream_id, &to);

        let from_balance: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::Balance(from.clone()))
            .unwrap_or(0);
        if from_balance <= 1 {
            env.storage().persistent().remove(&DataKey::Balance(from));
        } else {
            let updated = from_balance.checked_sub(1).unwrap_or(0);
            env.storage()
                .persistent()
                .set(&DataKey::Balance(from.clone()), &updated);
            bump(&env, &DataKey::Balance(from));
        }

        let to_balance: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::Balance(to.clone()))
            .unwrap_or(0);
        let updated_to = to_balance.checked_add(1).unwrap_or_else(|| {
            panic_with_error!(&env, Error::ArithmeticOverflow);
        });
        env.storage()
            .persistent()
            .set(&DataKey::Balance(to.clone()), &updated_to);
        bump(&env, &DataKey::Balance(to));
    }

    /// Optimized batch query for token owners.
    ///
    /// Deserializes each `stream_id` and queries persistent storage exactly once,
    /// returning a `Vec<Option<Address>>` corresponding directly to the input IDs.
    ///
    /// # Errors
    /// * [`Error::EmptyBatch`] — `stream_ids` is empty.
    /// * [`Error::BatchTooLarge`] — `stream_ids.len() > MAX_BATCH_SIZE`.
    /// * [`Error::DuplicateTokenId`] — `stream_ids` contains duplicate IDs.
    pub fn batch_owner_of(env: Env, stream_ids: Vec<u64>) -> Result<Vec<Option<Address>>, Error> {
        let stream_ids = Self::validate_batch_ids(&env, &stream_ids)?;
        Self::reject_duplicate_ids(&stream_ids)?;

        let n = stream_ids.len();
        let mut owners: Vec<Option<Address>> = Vec::new(&env);
        for i in 0..n {
            let stream_id = stream_ids.get_unchecked(i);
            let owner: Option<Address> = env.storage().persistent().get(&DataKey::Owner(stream_id));
            if owner.is_some() {
                bump(&env, &DataKey::Owner(stream_id));
            }
            owners.push_back(owner);
        }
        Ok(owners)
    }

    /// Batch mint multiple stream NFTs in a single transaction for the caller.
    ///
    /// Deserializes and validates input IDs in a single pass up front, ensuring
    /// none of the tokens have already been minted before performing mutations.
    /// Balance updates for `caller` are coalesced into a single read-modify-write,
    /// avoiding redundant loop deserialization of `DataKey::Balance(caller)`.
    ///
    /// # Errors
    /// * [`Error::EmptyBatch`] — `stream_ids` is empty.
    /// * [`Error::BatchTooLarge`] — `stream_ids.len() > MAX_BATCH_SIZE`.
    /// * [`Error::DuplicateTokenId`] — `stream_ids` contains duplicate IDs.
    /// * [`Error::AlreadyMinted`] — any token in the batch is already minted.
    /// * [`Error::ArithmeticOverflow`] — supply or balance would overflow.
    pub fn batch_mint(env: Env, stream_ids: Vec<u64>) -> Result<u32, Error> {
        let caller = env.caller();
        caller.require_auth();

        let stream_ids = Self::validate_batch_ids(&env, &stream_ids)?;
        Self::reject_duplicate_ids(&stream_ids)?;

        let n = stream_ids.len();

        // Upfront preflight: verify none of the tokens exist
        for i in 0..n {
            let stream_id = stream_ids.get_unchecked(i);
            if env.storage().persistent().has(&DataKey::Owner(stream_id)) {
                return Err(Error::AlreadyMinted);
            }
        }

        let supply = env.storage().instance().get(&DataKey::Supply).unwrap_or(0);
        let new_supply = supply
            .checked_add(n as u64)
            .ok_or(Error::ArithmeticOverflow)?;

        // Execute ownership assignments
        for i in 0..n {
            let stream_id = stream_ids.get_unchecked(i);
            env.storage()
                .persistent()
                .set(&DataKey::Owner(stream_id), &caller);
            bump(&env, &DataKey::Owner(stream_id));
        }

        // Single deserialization and single write for caller balance
        let balance: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::Balance(caller.clone()))
            .unwrap_or(0);
        let new_balance = balance.checked_add(n).ok_or(Error::ArithmeticOverflow)?;
        env.storage()
            .persistent()
            .set(&DataKey::Balance(caller.clone()), &new_balance);
        bump(&env, &DataKey::Balance(caller));

        env.storage().instance().set(&DataKey::Supply, &new_supply);
        Ok(n)
    }

    /// Batch transfer multiple stream NFTs from their current owner to `to`.
    ///
    /// All tokens must be owned by the same `from` address, who is authorized once.
    /// Storage entries for each token are deserialized exactly once in the preflight
    /// phase and retained in-memory for the execution phase.
    ///
    /// Cross-contract overhead during batch claim transfers is streamlined via
    /// `transfer_claims_batch`, reducing N separate external invocations down to 1
    /// when the stream contract supports batch claims (#373).
    ///
    /// Balances for `from` and `to` are computed in a single read-modify-write pass,
    /// minimizing host CPU instruction consumption.
    ///
    /// # Errors
    /// * [`Error::EmptyBatch`] — `stream_ids` is empty.
    /// * [`Error::BatchTooLarge`] — `stream_ids.len() > MAX_BATCH_SIZE`.
    /// * [`Error::DuplicateTokenId`] — duplicate IDs found.
    /// * [`Error::TokenNotFound`] — any token is unminted.
    /// * [`Error::NotOwner`] — any token is not owned by the batch authorizer.
    /// * [`Error::StreamRejected`] — underlying stream contract refused the transfer.
    /// * [`Error::ArithmeticOverflow`] — balance calculations overflow.
    pub fn batch_transfer(
        env: Env,
        stream_ids: Vec<u64>,
        from: Address,
        to: Address,
    ) -> Result<u32, Error> {
        from.require_auth();

        let stream_ids = Self::validate_batch_ids(&env, &stream_ids)?;
        Self::reject_duplicate_ids(&stream_ids)?;

        let n = stream_ids.len();
        let config = get_config(&env);

        // --- Preflight Phase: Single deserialization pass and owner check ---
        for i in 0..n {
            let stream_id = stream_ids.get_unchecked(i);
            let owner: Option<Address> = env.storage().persistent().get(&DataKey::Owner(stream_id));
            match owner {
                Some(ref current_owner) if current_owner == &from => {}
                Some(_) => return Err(Error::NotOwner),
                None => return Err(Error::TokenNotFound),
            }
        }

        // --- Streamlined Cross-Contract Call Phase (#373) ---
        // Coalesces the transfer invocation into a single sub-invocation if supported
        transfer_claims_batch(&env, &config, &stream_ids, &to)?;

        // --- Storage Reassignment Phase: Operates on validated IDs ---
        for i in 0..n {
            let stream_id = stream_ids.get_unchecked(i);
            env.storage()
                .persistent()
                .set(&DataKey::Owner(stream_id), &to);
            bump(&env, &DataKey::Owner(stream_id));
        }

        // --- Balance Updates: Single deserialization & write per participant ---
        if from != to {
            let from_balance: u32 = env
                .storage()
                .persistent()
                .get(&DataKey::Balance(from.clone()))
                .unwrap_or(0);

            let new_from_balance = from_balance
                .checked_sub(n)
                .ok_or(Error::ArithmeticOverflow)?;
            if new_from_balance == 0 {
                env.storage().persistent().remove(&DataKey::Balance(from));
            } else {
                env.storage()
                    .persistent()
                    .set(&DataKey::Balance(from.clone()), &new_from_balance);
                bump(&env, &DataKey::Balance(from));
            }

            let to_balance: u32 = env
                .storage()
                .persistent()
                .get(&DataKey::Balance(to.clone()))
                .unwrap_or(0);
            let new_to_balance = to_balance.checked_add(n).ok_or(Error::ArithmeticOverflow)?;
            env.storage()
                .persistent()
                .set(&DataKey::Balance(to.clone()), &new_to_balance);
            bump(&env, &DataKey::Balance(to));
        }

        Ok(n)
    }

    /// Validate batch size and element well-formedness.
    fn validate_batch_ids(env: &Env, stream_ids: &Vec<u64>) -> Result<Vec<u64>, Error> {
        let count = stream_ids.len();
        if count == 0 {
            return Err(Error::EmptyBatch);
        }
        if count > MAX_BATCH_SIZE {
            return Err(Error::BatchTooLarge);
        }

        let mut validated = Vec::new(env);
        for raw_id in stream_ids.to_vals().iter() {
            let stream_id = u64::try_from_val(env, &raw_id).map_err(|_| Error::MalformedTokenId)?;
            validated.push_back(stream_id);
        }
        Ok(validated)
    }

    /// Reject duplicates within a batch in an O(n^2) scalar comparison pass.
    fn reject_duplicate_ids(stream_ids: &Vec<u64>) -> Result<(), Error> {
        let count = stream_ids.len();
        for i in 0..count {
            for j in (i + 1)..count {
                if stream_ids.get_unchecked(i) == stream_ids.get_unchecked(j) {
                    return Err(Error::DuplicateTokenId);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod test;
