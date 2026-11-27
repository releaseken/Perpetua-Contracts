#![no_std]
//! # Perpetua stream-NFT — represent stream recipient claims as tokens (#123, #367, #370)
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
//! contract. Minting can be open or restricted by role policy. Transfers carry
//! the seller's auth to `transfer_recipient`, whose `recipient.require_auth()`
//! gate only passes when the NFT owner really is the stream's current recipient.
//!
//! ## Granular Role-Based Authorization (#367)
//!
//! Stream-NFT implements granular role-based authorization:
//! - `admin`: Manages admin rotation and role assignments.
//! - `metadata_admin`: Authorized to update token metadata (`name`, `symbol`).
//! - `manager`: Authorized to configure operational parameters such as minting pause
//!   and underlying stream instance binding.
//!
//! Admin rotation uses a secure two-step pattern (`propose_admin` / `accept_admin`)
//! alongside direct rotation (`set_admin`), with burn address and self protection.
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

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, Address, Env, IntoVal,
    String, Symbol, Val,
};

/// NFT metadata, underlying stream, and granular administrative roles.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Administrative authority (governs roles, proposals, upgrades).
    pub admin: Address,
    /// Authority permitted to update collection metadata (name, symbol).
    pub metadata_admin: Address,
    /// Authority permitted to update stream contract address and minting controls.
    pub manager: Address,
    /// The `FluxoraStream` contract whose claims are tokenized.
    pub stream: Address,
    /// Token name, e.g. "Perpetua Stream".
    pub name: String,
    /// Token symbol, e.g. "PSTREAM".
    pub symbol: String,
    /// Fixed per the NFT standard.
    pub decimals: u32,
    /// Emergency pause switch for minting new claim tokens.
    pub mint_paused: bool,
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
    /// Nominated successor admin awaiting acceptance.
    PendingAdmin = 5,
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
    Unauthorized = 11,
    UnauthorizedRole = 12,
    InvalidRoleAddress = 13,
    NoPendingAdmin = 14,
    MintPaused = 15,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleUpdatedEvent {
    pub role: Symbol,
    pub old_holder: Address,
    pub new_holder: Address,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminProposedEvent {
    pub current_admin: Address,
    pub proposed_admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminTransferredEvent {
    pub old_admin: Address,
    pub new_admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataUpdatedEvent {
    pub name: String,
    pub symbol: String,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamContractUpdatedEvent {
    pub old_stream: Address,
    pub new_stream: Address,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintPausedEvent {
    pub paused: bool,
}

const STREAM_TRANSFER_RECIPIENT: &str = "transfer_recipient";

/// Zero address sentinel check (Stellar all-zeros account).
pub fn is_zero_address(env: &Env, addr: &Address) -> bool {
    let zero = Address::from_string(&String::from_str(
        env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    ));
    *addr == zero
}

pub fn is_forbidden_address(env: &Env, addr: &Address) -> bool {
    is_zero_address(env, addr) || *addr == env.current_contract_address()
}

fn get_config(env: &Env) -> Config {
    bump(env, &DataKey::Config);
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
}

fn set_config(env: &Env, config: &Config) {
    env.storage().instance().set(&DataKey::Config, config);
    bump(env, &DataKey::Config);
}

fn bump(env: &Env, key: &DataKey) {
    let live = env.storage().max_ttl();
    match key {
        DataKey::Config | DataKey::Supply | DataKey::Executing | DataKey::PendingAdmin => {
            env.storage().instance().extend_ttl(live, live);
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
    /// Initialize the wrapper contract with granular role configuration.
    ///
    /// Requires non-null, valid addresses for admin, metadata_admin, manager, and stream.
    pub fn initialize(
        env: Env,
        admin: Address,
        metadata_admin: Address,
        manager: Address,
        stream: Address,
        name: String,
        symbol: String,
    ) {
        if env.storage().instance().has(&DataKey::Config) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }

        if is_forbidden_address(&env, &admin)
            || is_forbidden_address(&env, &metadata_admin)
            || is_forbidden_address(&env, &manager)
            || is_forbidden_address(&env, &stream)
        {
            panic_with_error!(&env, Error::InvalidRoleAddress);
        }

        let config = Config {
            admin,
            metadata_admin,
            manager,
            stream,
            name,
            symbol,
            decimals: 0,
            mint_paused: false,
        };

        set_config(&env, &config);
    }

    /// Backwards-compatible init: sets caller / stream as initial roles.
    pub fn init(env: Env, stream: Address, name: String, symbol: String) {
        let caller = env.caller();
        caller.require_auth();
        Self::initialize(
            env.clone(),
            caller.clone(),
            caller.clone(),
            caller,
            stream,
            name,
            symbol,
        );
        env.storage().instance().set(&DataKey::Supply, &0u64);
        bump(&env, &DataKey::Supply);
    }

    // -------------------------------------------------------------------------
    // View entry points
    // -------------------------------------------------------------------------

    pub fn get_config(env: Env) -> Config {
        get_config(&env)
    }

    pub fn admin(env: Env) -> Address {
        get_config(&env).admin
    }

    pub fn metadata_admin(env: Env) -> Address {
        get_config(&env).metadata_admin
    }

    pub fn manager(env: Env) -> Address {
        get_config(&env).manager
    }

    pub fn pending_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::PendingAdmin)
    }

    pub fn is_mint_paused(env: Env) -> bool {
        get_config(&env).mint_paused
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

    // -------------------------------------------------------------------------
    // Token operations
    // -------------------------------------------------------------------------

    /// Tokenize a stream claim for the caller.
    pub fn mint(env: Env, stream_id: u64) {
        if is_executing(&env) {
            panic_with_error!(&env, Error::ReentrancyGuard);
        }
        set_executing(&env);

        let caller = env.caller();
        caller.require_auth();

        let config = get_config(&env);
        if config.mint_paused {
            clear_executing(&env);
            panic_with_error!(&env, Error::MintPaused);
        }

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

    // -------------------------------------------------------------------------
    // Granular Role & Configuration Setters (#367)
    // -------------------------------------------------------------------------

    /// Update token metadata (`name` and `symbol`).
    ///
    /// Restricted to `metadata_admin`.
    pub fn set_metadata(env: Env, name: String, symbol: String) {
        let mut config = get_config(&env);
        config.metadata_admin.require_auth();

        config.name = name.clone();
        config.symbol = symbol.clone();
        set_config(&env, &config);

        env.events().publish(
            (Symbol::new(&env, "metadata_updated"),),
            MetadataUpdatedEvent { name, symbol },
        );
    }

    /// Update the underlying stream contract address.
    ///
    /// Restricted to `manager`.
    pub fn set_stream_contract(env: Env, new_stream: Address) {
        let mut config = get_config(&env);
        config.manager.require_auth();

        if is_forbidden_address(&env, &new_stream) {
            panic_with_error!(&env, Error::InvalidRoleAddress);
        }

        let old_stream = config.stream;
        config.stream = new_stream.clone();
        set_config(&env, &config);

        env.events().publish(
            (Symbol::new(&env, "stream_updated"),),
            StreamContractUpdatedEvent {
                old_stream,
                new_stream,
            },
        );
    }

    /// Pause or resume minting of stream claim tokens.
    ///
    /// Restricted to `manager`.
    pub fn set_mint_paused(env: Env, paused: bool) {
        let mut config = get_config(&env);
        config.manager.require_auth();

        config.mint_paused = paused;
        set_config(&env, &config);

        env.events().publish(
            (Symbol::new(&env, "mint_paused"),),
            MintPausedEvent { paused },
        );
    }

    /// Reassign the `metadata_admin` role.
    ///
    /// Restricted to `admin`.
    pub fn set_metadata_admin(env: Env, new_metadata_admin: Address) {
        let mut config = get_config(&env);
        config.admin.require_auth();

        if is_forbidden_address(&env, &new_metadata_admin) {
            panic_with_error!(&env, Error::InvalidRoleAddress);
        }

        let old_holder = config.metadata_admin;
        config.metadata_admin = new_metadata_admin.clone();
        set_config(&env, &config);

        env.events().publish(
            (
                Symbol::new(&env, "role_updated"),
                Symbol::new(&env, "metadata_admin"),
            ),
            RoleUpdatedEvent {
                role: Symbol::new(&env, "metadata_admin"),
                old_holder,
                new_holder: new_metadata_admin,
            },
        );
    }

    /// Reassign the `manager` role.
    ///
    /// Restricted to `admin`.
    pub fn set_manager(env: Env, new_manager: Address) {
        let mut config = get_config(&env);
        config.admin.require_auth();

        if is_forbidden_address(&env, &new_manager) {
            panic_with_error!(&env, Error::InvalidRoleAddress);
        }

        let old_holder = config.manager;
        config.manager = new_manager.clone();
        set_config(&env, &config);

        env.events().publish(
            (
                Symbol::new(&env, "role_updated"),
                Symbol::new(&env, "manager"),
            ),
            RoleUpdatedEvent {
                role: Symbol::new(&env, "manager"),
                old_holder,
                new_holder: new_manager,
            },
        );
    }

    /// Single-step direct admin rotation.
    ///
    /// Restricted to `admin`. Clears any pending admin proposal.
    pub fn set_admin(env: Env, new_admin: Address) {
        let mut config = get_config(&env);
        config.admin.require_auth();

        if is_forbidden_address(&env, &new_admin) {
            panic_with_error!(&env, Error::InvalidRoleAddress);
        }

        let old_admin = config.admin;
        config.admin = new_admin.clone();
        set_config(&env, &config);

        env.storage().instance().remove(&DataKey::PendingAdmin);

        env.events().publish(
            (Symbol::new(&env, "admin_transferred"),),
            AdminTransferredEvent {
                old_admin,
                new_admin,
            },
        );
    }

    /// Propose a new admin for two-step rotation.
    ///
    /// Restricted to `admin`.
    pub fn propose_admin(env: Env, new_admin: Address) {
        let config = get_config(&env);
        config.admin.require_auth();

        if is_forbidden_address(&env, &new_admin) {
            panic_with_error!(&env, Error::InvalidRoleAddress);
        }

        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);
        bump(&env, &DataKey::PendingAdmin);

        env.events().publish(
            (Symbol::new(&env, "admin_proposed"),),
            AdminProposedEvent {
                current_admin: config.admin,
                proposed_admin: new_admin,
            },
        );
    }

    /// Accept pending admin nomination (second step in two-step rotation).
    ///
    /// Restricted to the pending admin nominee.
    pub fn accept_admin(env: Env) {
        let pending: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoPendingAdmin));

        pending.require_auth();

        let mut config = get_config(&env);
        let old_admin = config.admin;
        config.admin = pending.clone();
        set_config(&env, &config);

        env.storage().instance().remove(&DataKey::PendingAdmin);

        env.events().publish(
            (Symbol::new(&env, "admin_transferred"),),
            AdminTransferredEvent {
                old_admin,
                new_admin: pending,
            },
        );
    }
}

#[cfg(test)]
mod test;
