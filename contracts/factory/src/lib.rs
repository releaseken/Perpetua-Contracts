#![no_std]
//! # Perpetua Factory
//!
//! Parameter-gated stream factory for Perpetua.
//!
//! All of Perpetua's protocol-wide policy is enforced here, before any stream is
//! launched: a capacity cap, a minimum stream duration, optional per-second
//! rate bounds, an optional recipient allowlist, and a global creation pause.
//! Each policy axis has a matching admin-only setter, so governance can tune the
//! factory's policy in place without touching already-created streams.
//!
//! ## Why a factory
//!
//! The [`FluxoraStream`] contract itself is deliberately un-configurable — no
//! admin key, no upgrade path, no fee switch. That immutability is what makes a
//! stream safe for an untrusted recipient to accept. The factory is the layer
//! that *does* carry policy: a treasury that wants its on-chain creators to
//! respect caps and minimums pins this contract in front of stream creation.
//!
//! ## Admin rotation (issue #39)
//!
//! The factory gates every policy setter behind a single admin key. Because a
//! single-step rotation can destructively transfer control (or, worse, rotate
//! it onto an un-signable key), the factory supports an optional two-step
//! hand-off: `propose_admin` nominates a successor, and `accept_admin` — called
//! by that successor — completes the transfer and immediately revokes the
//! previous admin. The zero address and the factory's own address are rejected
//! outright as admin candidates in both the single-step and two-step paths.

pub mod error;
pub mod invariants;
pub use error::{ErrorCategory, FactoryError};

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, panic_with_error, Address,
    Bytes, BytesN, Env, IntoVal, String, Symbol,
};

pub mod math;
pub use math::{calculate_bps_payout, calculate_fractional_payout, FractionalSplit};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Storage entries are extended to the network-maximum-adjacent window on every
/// policy interaction (same values as the stream and governance contracts).
const INSTANCE_LIFETIME_THRESHOLD: u32 = 17_280;
const INSTANCE_BUMP_AMOUNT: u32 = 120_960;
const DEFAULT_RATE_LIMIT_WINDOW_LEDGERS: u32 = 17_280;
const DEFAULT_MAX_DURATION: u64 = 157_680_000;
const MAX_BATCH_SIZE: u32 = 32;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Storage keys.
#[contracttype]
#[repr(u32)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// The single persistent policy record, written by `init` and rewritten by
    /// every setter.
    Config = 0,
    /// Presence of an address under this key means it is allowlisted.
    Allowlist(Address),
    /// The proposed next admin for the two-step rotation hand-off. Written by
    /// `propose_admin`, consumed (and cleared) by `accept_admin`.
    PendingAdmin,
    /// Presence of an address under this key means the token is approved.
    TokenAllowlist(Address),
    /// Per-sender rolling creation bucket.
    RateLimit(Address),
    /// Re-entrancy guard flag: set while state-modifying external operations are active.
    Executing,
}

/// The full on-chain configuration record.
///
/// Written atomically by `init` and re-written atomically by every setter, so a
/// reader of [`FluxoraFactory::get_factory_config`] always observes a coherent
/// policy, never a half-applied one.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactoryConfig {
    pub admin: Address,
    pub stream_contract: Address,
    pub stream_wasm_hash: BytesN<32>,
    pub max_deposit: i128,
    pub min_duration: u64,
    /// Maximum stream duration in seconds.
    pub max_duration: u64,
    /// When true, the batch creation path enforces `max_deposit` per stream
    /// (not merely per batch).
    pub batch_cap_enforced: bool,
    /// When true, `create_stream` is rejected while the factory is paused.
    pub creation_paused: bool,
    /// Whether recipient creation is restricted to persistent allowlist entries.
    pub recipient_allowlist_enabled: bool,
    /// Optional lower bound on the per-second stream rate, applied at creation.
    pub min_rate_per_second: Option<i128>,
    /// Optional upper bound on the per-second stream rate, applied at creation.
    pub max_rate_per_second: Option<i128>,
    /// Maximum number of non-terminal streams admitted through this factory.
    pub max_active_streams: u32,
    /// Number of streams admitted but not yet released by a terminal callback.
    pub active_streams: u32,
    /// Maximum creations allowed per sender in a rate-limit window.
    pub max_creations_per_window: u32,
    /// Rate-limit window length in ledgers.
    pub rate_limit_window_ledgers: u32,
}

/// The policy view consumed by the stream-creation paths.
///
/// This is the same record as [`FactoryConfig`] minus the admin field. Keeping
/// the two types separate means a policy consumer can never accidentally read
/// the admin address, and lets `load_policy` serve as the single chokepoint
/// between raw storage and every semantic guard.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactoryPolicy {
    pub stream_contract: Address,
    pub stream_wasm_hash: BytesN<32>,
    pub max_deposit: i128,
    pub min_duration: u64,
    pub max_duration: u64,
    pub batch_cap_enforced: bool,
    pub creation_paused: bool,
    pub recipient_allowlist_enabled: bool,
    pub min_rate_per_second: Option<i128>,
    pub max_rate_per_second: Option<i128>,
    pub max_active_streams: u32,
    pub active_streams: u32,
    pub max_creations_per_window: u32,
    pub rate_limit_window_ledgers: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RateLimitBucket {
    pub window_start: u32,
    pub creations: u32,
}

/// A stream creation proxied by this factory.
///
/// The topic layout intentionally matches the stream contract's creation
/// event namespace while adding the factory address as the first indexed
/// context field: `stream_created, factory_id, sender, recipient`.
#[contractevent]
pub struct StreamCreated {
    #[topic]
    pub factory_id: Address,
    #[topic]
    pub sender: Address,
    #[topic]
    pub recipient: Address,
}

/// A persistent entry's TTL was extended by automated maintenance or a keeper.
#[contractevent]
pub struct TtlExtended {
    #[topic]
    pub target_key: Symbol,
    #[topic]
    pub target_address: Address,
    pub threshold: u32,
    pub bump_amount: u32,
}
pub trait FactoryGovernance {
    fn set_admin(env: Env, new_admin: Address) -> Result<(), FactoryError>;
    fn set_stream_contract(env: Env, stream_contract: Address) -> Result<(), FactoryError>;
    fn set_cap(env: Env, max_deposit: i128) -> Result<(), FactoryError>;
    fn set_min_duration(env: Env, min_duration: u64) -> Result<(), FactoryError>;
    fn set_allowlist(env: Env, recipient: Address, allowed: bool) -> Result<(), FactoryError>;
    fn set_allowlist_enabled(env: Env, enabled: bool) -> Result<(), FactoryError>;
    fn set_batch_cap_enforcement(env: Env, enforced: bool) -> Result<(), FactoryError>;
    fn set_factory_paused(env: Env, paused: bool) -> Result<(), FactoryError>;
    fn set_rate_bounds(
        env: Env,
        min_rate_per_second: Option<i128>,
        max_rate_per_second: Option<i128>,
    ) -> Result<(), FactoryError>;
}

/// Emitted when an admin rotation is proposed (two-step hand-off, issue #39).
#[contractevent]
pub struct AdminTransferProposed {
    #[topic]
    pub current: Address,
    #[topic]
    pub pending: Address,
}

/// Emitted when the proposed admin accepts the hand-off. From this moment the
/// previous admin is immediately revoked.
#[contractevent]
pub struct AdminTransferred {
    #[topic]
    pub previous: Address,
    #[topic]
    pub new: Address,
}

// ---------------------------------------------------------------------------
// Storage helpers
// ---------------------------------------------------------------------------

/// Derives the deterministic salt used for a child stream deployment.
///
/// The serialized preimage is domain-separated and length-fixed after the
/// sender address, so distinct `(sender, nonce, stream_id)` tuples cannot be
/// confused by concatenation. The tuple is hashed to the 32-byte salt format
/// accepted by Soroban deployment APIs.
pub fn derive_stream_salt(env: &Env, sender: &Address, nonce: u64, stream_id: u64) -> BytesN<32> {
    let mut preimage = Bytes::from_slice(env, b"perpetua-factory-stream-salt-v1");
    preimage.append(&sender.to_xdr(env));
    let mut suffix = [0u8; 16];
    suffix[..8].copy_from_slice(&nonce.to_be_bytes());
    suffix[8..].copy_from_slice(&stream_id.to_be_bytes());
    preimage.append(&Bytes::from_slice(env, &suffix));
    env.crypto().sha256(&preimage)
}

/// Extends the factory instance entry to the network maximum.
pub fn bump_ttl(env: &Env) {
    let max_ttl = env.storage().max_ttl();
    let threshold = INSTANCE_LIFETIME_THRESHOLD.min(max_ttl);
    let bump = INSTANCE_BUMP_AMOUNT.min(max_ttl);
    env.storage().instance().extend_ttl(threshold, bump);
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

/// Returns the stored config or `NotInitialized`.
fn load_config(env: &Env) -> Result<FactoryConfig, FactoryError> {
    let config = env
        .storage()
        .instance()
        .get(&DataKey::Config)
        .ok_or(FactoryError::NotInitialized)?;
    bump_ttl(env);
    Ok(config)
}

/// Centralised policy-load chokepoint.
///
/// All creation-path guards read policy through here rather than reaching into
/// storage themselves, so defaults and optional-field handling live in exactly
/// one place. Returns `NotInitialized` before any `init`.
pub fn load_policy(env: &Env) -> Result<FactoryPolicy, FactoryError> {
    let config = load_config(env)?;
    Ok(FactoryPolicy {
        stream_contract: config.stream_contract,
        stream_wasm_hash: config.stream_wasm_hash,
        max_deposit: config.max_deposit,
        min_duration: config.min_duration,
        max_duration: config.max_duration,
        batch_cap_enforced: config.batch_cap_enforced,
        creation_paused: config.creation_paused,
        recipient_allowlist_enabled: config.recipient_allowlist_enabled,
        min_rate_per_second: config.min_rate_per_second,
        max_rate_per_second: config.max_rate_per_second,
        max_active_streams: config.max_active_streams,
        active_streams: config.active_streams,
        max_creations_per_window: config.max_creations_per_window,
        rate_limit_window_ledgers: config.rate_limit_window_ledgers,
    })
}

fn record_creation_with_config(
    env: &Env,
    sender: &Address,
    config: &FactoryConfig,
) -> Result<(), FactoryError> {
    sender.require_auth();

    let key = DataKey::RateLimit(sender.clone());
    let current_ledger = env.ledger().sequence();
    let mut bucket = env
        .storage()
        .persistent()
        .get::<_, RateLimitBucket>(&key)
        .unwrap_or(RateLimitBucket {
            window_start: current_ledger,
            creations: 0,
        });

    if current_ledger.saturating_sub(bucket.window_start) >= config.rate_limit_window_ledgers {
        bucket.window_start = current_ledger;
        bucket.creations = 0;
    }
    if bucket.creations >= config.max_creations_per_window {
        return Err(FactoryError::RateLimitExceeded);
    }

    bucket.creations = bucket
        .creations
        .checked_add(1)
        .ok_or(FactoryError::RateLimitExceeded)?;
    env.storage().persistent().set(&key, &bucket);
    bump_allowlist(env, &key);
    Ok(())
}

// ---------------------------------------------------------------------------
// Admin rotation guards
// ---------------------------------------------------------------------------

/// The all-zeros Stellar account (`G…WHF`). Anything signed by this key does
/// not exist, so installing it as admin would permanently burn the factory's
/// control surface. The factory refuses it everywhere an admin can be set.
fn is_zero_address(env: &Env, addr: &Address) -> bool {
    let zero = Address::from_string(&String::from_str(
        env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    ));
    *addr == zero
}

/// An address that must never become admin: the zero account (which cannot
/// sign) or the factory's own contract address (which would make the factory
/// its own admin and lock every setter behind a self-authorization).
fn is_forbidden_admin(env: &Env, addr: &Address) -> bool {
    is_zero_address(env, addr) || *addr == env.current_contract_address()
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct FluxoraFactory;

#[contractimpl]
impl FluxoraFactory {
    /// Initialise the factory with an admin and the initial policy axes.
    ///
    /// # Parameters
    /// - `admin`: Address that can update every policy axis.
    /// - `stream_contract`: Address of the `fluxora-stream` contract created
    ///   streams will point at.
    /// - `stream_wasm_hash`: Reviewed WASM hash that the stream contract must
    ///   have been deployed from.
    /// - `max_deposit`: Capacity cap per stream (and per batch when
    ///   `batch_cap_enforced` is true).
    /// - `min_duration`: Minimum stream duration in seconds.
    ///
    /// # Errors
    /// - `AlreadyInitialized`: A policy record already exists.
    pub fn init(
        env: Env,
        admin: Address,
        stream_contract: Address,
        stream_wasm_hash: BytesN<32>,
        max_deposit: i128,
        min_duration: u64,
    ) -> Result<(), FactoryError> {
        if env.storage().instance().has(&DataKey::Config) {
            return Err(FactoryError::AlreadyInitialized);
        }
        admin.require_auth();
        if is_forbidden_admin(&env, &admin) {
            return Err(FactoryError::InvalidNewAdmin);
        }
        if min_duration > DEFAULT_MAX_DURATION {
            return Err(FactoryError::InvalidDuration);
        }
        if max_deposit <= 0 {
            return Err(FactoryError::InvalidDeposit);
        }

        let config = FactoryConfig {
            admin,
            stream_contract,
            stream_wasm_hash,
            max_deposit,
            min_duration,
            max_duration: DEFAULT_MAX_DURATION,
            // Documented init defaults for the optional axes.
            batch_cap_enforced: true,
            creation_paused: false,
            recipient_allowlist_enabled: false,
            min_rate_per_second: None,
            max_rate_per_second: None,
            max_active_streams: u32::MAX,
            active_streams: 0,
            max_creations_per_window: u32::MAX,
            rate_limit_window_ledgers: DEFAULT_RATE_LIMIT_WINDOW_LEDGERS,
        };
        env.storage().instance().set(&DataKey::Config, &config);
        bump_ttl(&env);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Views
    // -----------------------------------------------------------------------

    /// Full configuration record, including the admin address.
    ///
    /// # Errors
    /// - `NotInitialized`: No policy record exists.
    pub fn get_factory_config(env: Env) -> FactoryConfig {
        match load_config(&env) {
            Ok(config) => config,
            Err(e) => panic_with_error!(&env, e),
        }
    }

    /// True if `recipient` is allowlisted.
    ///
    /// # Errors
    /// - `NotInitialized`: No policy record exists.
    pub fn is_allowlisted(env: Env, recipient: Address) -> bool {
        load_config(&env).map_or_else(
            |e| panic_with_error!(&env, e),
            |_| {
                let key = DataKey::Allowlist(recipient);
                let allowed = env.storage().persistent().has(&key);
                if allowed {
                    bump_allowlist(&env, &key);
                }
                allowed
            },
        )
    }

    /// True if `token` is approved for stream creation.
    ///
    /// # Errors
    /// - `NotInitialized`: No policy record exists.
    pub fn is_token_allowed(env: Env, token: Address) -> bool {
        load_config(&env).map_or_else(
            |e| panic_with_error!(&env, e),
            |_| {
                let key = DataKey::TokenAllowlist(token);
                let allowed = env.storage().persistent().has(&key);
                if allowed {
                    bump_allowlist(&env, &key);
                }
                allowed
            },
        )
    }

    /// Validate a token before forwarding a stream-creation request.
    pub fn validate_token(env: Env, token: Address) -> Result<(), FactoryError> {
        load_config(&env)?;
        let key = DataKey::TokenAllowlist(token);
        if env.storage().persistent().has(&key) {
            bump_allowlist(&env, &key);
            Ok(())
        } else {
            Err(FactoryError::TokenNotAllowed)
        }
    }

    /// Validate a stream duration against the configured inclusive bounds.
    pub fn validate_duration(env: Env, start_time: u64, end_time: u64) -> Result<(), FactoryError> {
        let config = load_config(&env)?;
        let duration = end_time
            .checked_sub(start_time)
            .ok_or(FactoryError::InvalidDuration)?;
        if duration == 0 || duration < config.min_duration || duration > config.max_duration {
            return Err(FactoryError::InvalidDuration);
        }
        Ok(())
    }

    /// Record one sender creation and reject exhausted buckets.
    ///
    /// Creation wrappers must call this before invoking the stream contract.
    /// The sender authorization binds the quota to the party that funds the
    /// creation, while transaction atomicity prevents failed downstream calls
    /// from consuming a slot.
    pub fn record_creation(env: Env, sender: Address) -> Result<(), FactoryError> {
        let config = load_config(&env)?;
        record_creation_with_config(&env, &sender, &config)
    }

    /// Extend the persistent storage TTL for a specific allowlist entry.
    ///
    /// Permissionless keeper entrypoint: ensures archival resilience for active
    /// recipients even during long idle intervals without state mutations.
    pub fn extend_allowlist_ttl(env: Env, recipient: Address) -> Result<(), FactoryError> {
        load_config(&env)?;
        let key = DataKey::Allowlist(recipient.clone());
        if !env.storage().persistent().has(&key) {
            return Err(FactoryError::AllowlistDenied);
        }
        bump_allowlist(&env, &key);
        TtlExtended {
            target_key: Symbol::new(&env, "allowlist"),
            target_address: recipient,
            threshold: INSTANCE_LIFETIME_THRESHOLD,
            bump_amount: INSTANCE_BUMP_AMOUNT,
        }
        .publish(&env);
        bump_ttl(&env);
        Ok(())
    }

    /// Extend the persistent storage TTL for a specific token allowlist entry.
    ///
    /// Permissionless keeper entrypoint: protects approved tokens from archiving.
    pub fn extend_token_allowlist_ttl(env: Env, token: Address) -> Result<(), FactoryError> {
        load_config(&env)?;
        let key = DataKey::TokenAllowlist(token.clone());
        if !env.storage().persistent().has(&key) {
            return Err(FactoryError::TokenNotAllowed);
        }
        bump_allowlist(&env, &key);
        TtlExtended {
            target_key: Symbol::new(&env, "token"),
            target_address: token,
            threshold: INSTANCE_LIFETIME_THRESHOLD,
            bump_amount: INSTANCE_BUMP_AMOUNT,
        }
        .publish(&env);
        bump_ttl(&env);
        Ok(())
    }

    /// Batch extend persistent storage TTL for multiple allowlist recipients.
    ///
    /// Permissionless keeper sweep: skips non-existent keys without aborting
    /// and returns the count of entries successfully extended.
    pub fn batch_extend_allowlist_ttl(
        env: Env,
        recipients: soroban_sdk::Vec<Address>,
    ) -> Result<u32, FactoryError> {
        load_config(&env)?;
        let len = recipients.len();
        if len == 0 || len > MAX_BATCH_SIZE {
            return Err(FactoryError::InvalidDuration);
        }

        let mut extended = 0u32;
        for recipient in recipients.iter() {
            let key = DataKey::Allowlist(recipient.clone());
            if env.storage().persistent().has(&key) {
                bump_allowlist(&env, &key);
                TtlExtended {
                    target_key: Symbol::new(&env, "allowlist"),
                    target_address: recipient,
                    threshold: INSTANCE_LIFETIME_THRESHOLD,
                    bump_amount: INSTANCE_BUMP_AMOUNT,
                }
                .publish(&env);
                extended = extended
                    .checked_add(1)
                    .ok_or(FactoryError::CapacityCapExceeded)?;
            }
        }
        bump_ttl(&env);
        Ok(extended)
    }

    /// Batch extend persistent storage TTL for multiple approved token addresses.
    pub fn batch_extend_token_ttl(
        env: Env,
        tokens: soroban_sdk::Vec<Address>,
    ) -> Result<u32, FactoryError> {
        load_config(&env)?;
        let len = tokens.len();
        if len == 0 || len > MAX_BATCH_SIZE {
            return Err(FactoryError::InvalidDuration);
        }

        let mut extended = 0u32;
        for token in tokens.iter() {
            let key = DataKey::TokenAllowlist(token.clone());
            if env.storage().persistent().has(&key) {
                bump_allowlist(&env, &key);
                TtlExtended {
                    target_key: Symbol::new(&env, "token"),
                    target_address: token,
                    threshold: INSTANCE_LIFETIME_THRESHOLD,
                    bump_amount: INSTANCE_BUMP_AMOUNT,
                }
                .publish(&env);
                extended = extended
                    .checked_add(1)
                    .ok_or(FactoryError::CapacityCapExceeded)?;
            }
        }
        bump_ttl(&env);
        Ok(extended)
    }

    /// True when the factory is paused and stream creation is rejected.
    ///
    /// # Errors
    /// - `NotInitialized`: No policy record exists.
    pub fn is_factory_paused(env: Env) -> bool {
        load_config(&env).map_or_else(
            |e| panic_with_error!(&env, e),
            |config| config.creation_paused,
        )
    }

    /// Return the canonical salt for a child stream deployment.
    pub fn derive_stream_salt(env: Env, sender: Address, nonce: u64, stream_id: u64) -> BytesN<32> {
        crate::derive_stream_salt(&env, &sender, nonce, stream_id)
    }

    /// Return human-readable error description for a numeric error discriminant (issue #362).
    ///
    /// Enables client SDKs, block explorers, and monitoring indexers to query
    /// meaningful error diagnostics on-chain without hardcoding strings.
    pub fn error_message(env: Env, discriminant: u32) -> String {
        let msg = FactoryError::from_discriminant(discriminant)
            .map(|e| e.message())
            .unwrap_or("");
        String::from_str(&env, msg)
    }

    /// Return the single-character group letter ('A' through 'E') for an error discriminant.
    ///
    /// Returns 0 (NUL byte) if the discriminant is unrecognized.
    pub fn error_group(_env: Env, discriminant: u32) -> u32 {
        FactoryError::from_discriminant(discriminant)
            .map(|e| e.group() as u32)
            .unwrap_or(0)
    }

    // -----------------------------------------------------------------------
    // Admin setters
    // -----------------------------------------------------------------------

    fn guard(env: &Env) -> Result<FactoryConfig, FactoryError> {
        if is_executing(env) {
            return Err(FactoryError::ReentrancyGuard);
        }
        let config = load_config(env)?;
        config.admin.clone().require_auth();
        Ok(config)
    }

    /// Rotate the admin address.
    ///
    /// # Burn protection
    ///
    /// Rejects the zero address (`G…WHF`) and the factory's own contract
    /// address: both would silently lock every policy setter behind an
    /// unreachable signature. For a safer hand-off use the two-step
    /// [`propose_admin`](Self::propose_admin) / [`accept_admin`](Self::accept_admin)
    /// pair instead.
    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), FactoryError> {
        let mut config = Self::guard(&env)?;
        if is_forbidden_admin(&env, &new_admin) {
            return Err(FactoryError::InvalidNewAdmin);
        }
        config.admin = new_admin;
        env.storage().instance().set(&DataKey::Config, &config);
        // A single-step rotation while a two-step hand-off is pending would
        // leave a stale PendingAdmin behind; clear it so accept_admin cannot
        // later resurrect a superseded proposal.
        env.storage().instance().remove(&DataKey::PendingAdmin);
        bump_ttl(&env);
        Ok(())
    }

    /// Nominate the next admin for a two-step transfer (issue #39).
    ///
    /// The pending admin takes over only after they call
    /// [`accept_admin`](Self::accept_admin); until then the current admin keeps
    /// full authority. This gives a rotating key a chance to prove it can sign
    /// before it is handed the factory.
    ///
    /// # Authorization
    /// - Requires the current admin's signature.
    ///
    /// # Errors
    /// - `InvalidNewAdmin`: the proposed address is the zero address or the
    ///   factory itself.
    /// - `Unauthorized`: the caller is not the current admin.
    pub fn propose_admin(env: Env, new_admin: Address) -> Result<(), FactoryError> {
        let config = Self::guard(&env)?;
        if is_forbidden_admin(&env, &new_admin) {
            return Err(FactoryError::InvalidNewAdmin);
        }
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);
        bump_ttl(&env);

        // CEI: the pending nomination is persisted before the event is emitted.
        AdminTransferProposed {
            current: config.admin,
            pending: new_admin,
        }
        .publish(&env);
        Ok(())
    }

    /// Accept a pending admin nomination and complete the transfer.
    ///
    /// The caller must be the exact address stored by [`propose_admin`](Self::propose_admin).
    /// On success the previous admin is **immediately revoked** — every setter
    /// re-checks the current admin, which is now the new address — and the
    /// pending nomination is consumed.
    ///
    /// # Errors
    /// - `NoPendingAdmin`: no transfer has been proposed.
    pub fn accept_admin(env: Env) -> Result<(), FactoryError> {
        if is_executing(&env) {
            return Err(FactoryError::ReentrancyGuard);
        }
        let pending: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .ok_or(FactoryError::NoPendingAdmin)?;
        // The caller must *be* the pending admin; any other address fails here.
        pending.require_auth();

        let mut config = load_config(&env)?;
        let previous = config.admin;
        config.admin = pending;
        env.storage().instance().set(&DataKey::Config, &config);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        bump_ttl(&env);

        // CEI: the transfer is persisted before the event is emitted.
        AdminTransferred {
            previous,
            new: config.admin,
        }
        .publish(&env);
        Ok(())
    }

    /// The currently nominated successor, if a two-step transfer is pending.
    ///
    /// # Errors
    /// - `NotInitialized`: No policy record exists.
    pub fn pending_admin(env: Env) -> Option<Address> {
        load_config(&env).map_or_else(
            |e| panic_with_error!(&env, e),
            |_| env.storage().instance().get(&DataKey::PendingAdmin),
        )
    }

    /// Point the factory at a different `fluxora-stream` contract.
    pub fn set_stream_contract(env: Env, stream_contract: Address) -> Result<(), FactoryError> {
        let mut config = Self::guard(&env)?;
        config.stream_contract = stream_contract;
        env.storage().instance().set(&DataKey::Config, &config);
        bump_ttl(&env);
        Ok(())
    }

    /// Update the reviewed WASM hash for the configured stream contract.
    pub fn set_stream_wasm_hash(
        env: Env,
        stream_wasm_hash: BytesN<32>,
    ) -> Result<(), FactoryError> {
        let mut config = Self::guard(&env)?;
        config.stream_wasm_hash = stream_wasm_hash;
        env.storage().instance().set(&DataKey::Config, &config);
        bump_ttl(&env);
        Ok(())
    }

    /// Update the per-stream capacity cap.
    pub fn set_cap(env: Env, max_deposit: i128) -> Result<(), FactoryError> {
        let mut config = Self::guard(&env)?;
        if max_deposit <= 0 {
            return Err(FactoryError::InvalidDeposit);
        }
        config.max_deposit = max_deposit;
        env.storage().instance().set(&DataKey::Config, &config);
        bump_ttl(&env);
        Ok(())
    }

    /// Update the minimum stream duration.
    pub fn set_min_duration(env: Env, min_duration: u64) -> Result<(), FactoryError> {
        let mut config = Self::guard(&env)?;
        if min_duration > config.max_duration {
            return Err(FactoryError::InvalidDuration);
        }
        config.min_duration = min_duration;
        env.storage().instance().set(&DataKey::Config, &config);
        bump_ttl(&env);
        Ok(())
    }

    /// Update the maximum stream duration in seconds.
    pub fn set_max_duration(env: Env, max_duration: u64) -> Result<(), FactoryError> {
        let mut config = Self::guard(&env)?;
        if max_duration < config.min_duration {
            return Err(FactoryError::InvalidDuration);
        }
        config.max_duration = max_duration;
        env.storage().instance().set(&DataKey::Config, &config);
        bump_ttl(&env);
        Ok(())
    }

    /// Add or remove a recipient from the allowlist.
    ///
    /// Removing a recipient that was never added is a safe no-op.
    pub fn set_allowlist(env: Env, recipient: Address, allowed: bool) -> Result<(), FactoryError> {
        let mut config = Self::guard(&env)?;
        let key = DataKey::Allowlist(recipient);
        if allowed {
            env.storage().persistent().set(&key, &true);
            bump_allowlist(&env, &key);
            config.recipient_allowlist_enabled = true;
            env.storage().instance().set(&DataKey::Config, &config);
            bump_ttl(&env);
        } else {
            env.storage().persistent().remove(&key);
        }
        Ok(())
    }

    /// Enable or disable recipient allowlist enforcement without deleting entries.
    pub fn set_allowlist_enabled(env: Env, enabled: bool) -> Result<(), FactoryError> {
        let mut config = Self::guard(&env)?;
        config.recipient_allowlist_enabled = enabled;
        env.storage().instance().set(&DataKey::Config, &config);
        bump_ttl(&env);
        Ok(())
    }

    /// Add or remove a token from the approved-token allowlist.
    ///
    /// Removing a token that was never added is a safe no-op.
    pub fn set_token_allowed(env: Env, token: Address, allowed: bool) -> Result<(), FactoryError> {
        Self::guard(&env)?;
        let key = DataKey::TokenAllowlist(token);
        if allowed {
            env.storage().persistent().set(&key, &true);
            bump_allowlist(&env, &key);
        } else {
            env.storage().persistent().remove(&key);
        }
        Ok(())
    }

    /// Toggle whether the batch creation path enforces the cap per stream.
    pub fn set_batch_cap_enforcement(env: Env, enforced: bool) -> Result<(), FactoryError> {
        let mut config = Self::guard(&env)?;
        config.batch_cap_enforced = enforced;
        env.storage().instance().set(&DataKey::Config, &config);
        bump_ttl(&env);
        Ok(())
    }

    /// Pause or unpause all stream creation through the factory.
    pub fn set_pause(env: Env, paused: bool) -> Result<(), FactoryError> {
        let mut config = Self::guard(&env)?;
        config.creation_paused = paused;
        env.storage().instance().set(&DataKey::Config, &config);
        bump_ttl(&env);
        Ok(())
    }

    /// Backward-compatible alias for `set_pause`.
    pub fn set_factory_paused(env: Env, paused: bool) -> Result<(), FactoryError> {
        Self::set_pause(env, paused)
    }

    /// Set optional per-second rate bounds applied at stream creation.
    ///
    /// Passing `None` for either bound clears it.
    pub fn set_rate_bounds(
        env: Env,
        min_rate_per_second: Option<i128>,
        max_rate_per_second: Option<i128>,
    ) -> Result<(), FactoryError> {
        let mut config = Self::guard(&env)?;
        if min_rate_per_second.is_some_and(|rate| rate < 0)
            || max_rate_per_second.is_some_and(|rate| rate < 0)
            || matches!((min_rate_per_second, max_rate_per_second), (Some(min), Some(max)) if min > max)
        {
            return Err(FactoryError::InvalidRateBounds);
        }
        config.min_rate_per_second = min_rate_per_second;
        config.max_rate_per_second = max_rate_per_second;
        env.storage().instance().set(&DataKey::Config, &config);
        bump_ttl(&env);
        Ok(())
    }

    /// Update the maximum number of active streams admitted through the factory.
    pub fn set_max_active_streams(env: Env, max_active_streams: u32) -> Result<(), FactoryError> {
        let mut config = Self::guard(&env)?;
        if max_active_streams < config.active_streams {
            return Err(FactoryError::CapacityCapExceeded);
        }
        config.max_active_streams = max_active_streams;
        env.storage().instance().set(&DataKey::Config, &config);
        bump_ttl(&env);
        Ok(())
    }

    /// Admit a stream through the configured stream contract.
    ///
    /// The counter is reserved before the cross-contract call and committed
    /// only after it succeeds, so a failed stream creation cannot consume
    /// capacity. The configured stream contract remains responsible for its
    /// own validation and authorization.
    #[allow(clippy::too_many_arguments)]
    pub fn create_stream(
        env: Env,
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
    ) -> Result<u64, FactoryError> {
        let mut config = load_config(&env)?;
        if config.creation_paused {
            return Err(FactoryError::CreationPaused);
        }
        if config.recipient_allowlist_enabled {
            let allowlist_key = DataKey::Allowlist(recipient.clone());
            if !env.storage().persistent().has(&allowlist_key) {
                return Err(FactoryError::AllowlistDenied);
            }
            bump_allowlist(&env, &allowlist_key);
        }
        if config.active_streams >= config.max_active_streams {
            return Err(FactoryError::CapacityCapExceeded);
        }
        if deposit > config.max_deposit {
            return Err(FactoryError::DepositCapExceeded);
        }
        if deposit <= 0 {
            return Err(FactoryError::InvalidDeposit);
        }
        let duration = end_time
            .checked_sub(start_time)
            .ok_or(FactoryError::InvalidDuration)?;
        if duration < config.min_duration || duration > config.max_duration || duration == 0 {
            return Err(FactoryError::InvalidDuration);
        }
        if cliff_time < start_time || cliff_time > end_time {
            return Err(FactoryError::InvalidCliff);
        }
        if config.min_rate_per_second.is_some() || config.max_rate_per_second.is_some() {
            let rate = deposit
                .checked_div(duration as i128)
                .ok_or(FactoryError::InvalidDuration)?;
            if config
                .min_rate_per_second
                .is_some_and(|minimum| rate < minimum)
            {
                return Err(FactoryError::RateBelowMinimum);
            }
            if config
                .max_rate_per_second
                .is_some_and(|maximum| rate > maximum)
            {
                return Err(FactoryError::RateAboveMaximum);
            }
        }
        if config.max_creations_per_window != u32::MAX {
            record_creation_with_config(&env, &sender, &config)?;
        }
        if is_executing(&env) {
            return Err(FactoryError::ReentrancyGuard);
        }

        let initial_active = config.active_streams;
        set_executing(&env);
        let stream_id = env.invoke_contract::<u64>(
            &config.stream_contract,
            &Symbol::new(&env, "create_stream"),
            (
                sender,
                recipient,
                token,
                deposit,
                start_time,
                end_time,
                cliff_time,
                cancellable,
                pausable,
                transferable,
            )
                .into_val(&env),
        );
        clear_executing(&env);

        config.active_streams = config
            .active_streams
            .checked_add(1)
            .ok_or(FactoryError::CapacityCapExceeded)?;
        if config.active_streams != initial_active.saturating_add(1)
            || config.active_streams > config.max_active_streams
        {
            return Err(FactoryError::InvariantViolation);
        }
        env.storage().instance().set(&DataKey::Config, &config);
        bump_ttl(&env);
        Ok(stream_id)
    }

    /// Release one active slot after a stream reaches a terminal state.
    ///
    /// Only the configured stream contract may call this callback. The stream
    /// contract integration should invoke it exactly once for cancellation or
    /// depletion. Calls made when the active count is already zero are rejected.
    pub fn stream_terminated(env: Env, _stream_id: u64) -> Result<(), FactoryError> {
        if is_executing(&env) {
            return Err(FactoryError::ReentrancyGuard);
        }
        let mut config = load_config(&env)?;
        config.stream_contract.require_auth();
        if config.active_streams == 0 {
            return Err(FactoryError::ActiveStreamCountUnderflow);
        }
        let initial_active = config.active_streams;
        config.active_streams = config
            .active_streams
            .checked_sub(1)
            .ok_or(FactoryError::ActiveStreamCountUnderflow)?;

        // Invariant assertion: active_streams must decrement by exactly 1.
        if config.active_streams != initial_active.saturating_sub(1) {
            return Err(FactoryError::InvariantViolation);
        }

        env.storage().instance().set(&DataKey::Config, &config);
        bump_ttl(&env);
        Ok(())
    }

    /// Configure the per-sender creation bucket.
    pub fn set_rate_limit(
        env: Env,
        max_creations_per_window: u32,
        rate_limit_window_ledgers: u32,
    ) -> Result<(), FactoryError> {
        let mut config = Self::guard(&env)?;
        if max_creations_per_window == 0 || rate_limit_window_ledgers == 0 {
            return Err(FactoryError::InvalidRateLimit);
        }
        config.max_creations_per_window = max_creations_per_window;
        config.rate_limit_window_ledgers = rate_limit_window_ledgers;
        env.storage().instance().set(&DataKey::Config, &config);
        bump_ttl(&env);
        Ok(())
    }
}

/// Structural implementation of the governance interface over the contract's
/// own `#[contractimpl]` entrypoints. Each method simply forwards to the ABI
/// method of the same name, so the trait and the deployed entrypoints can never
/// drift apart.
impl FactoryGovernance for FluxoraFactory {
    fn set_admin(env: Env, new_admin: Address) -> Result<(), FactoryError> {
        FluxoraFactory::set_admin(env, new_admin)
    }

    fn set_stream_contract(env: Env, stream_contract: Address) -> Result<(), FactoryError> {
        FluxoraFactory::set_stream_contract(env, stream_contract)
    }

    fn set_cap(env: Env, max_deposit: i128) -> Result<(), FactoryError> {
        FluxoraFactory::set_cap(env, max_deposit)
    }

    fn set_min_duration(env: Env, min_duration: u64) -> Result<(), FactoryError> {
        FluxoraFactory::set_min_duration(env, min_duration)
    }

    fn set_allowlist(env: Env, recipient: Address, allowed: bool) -> Result<(), FactoryError> {
        FluxoraFactory::set_allowlist(env, recipient, allowed)
    }

    fn set_allowlist_enabled(env: Env, enabled: bool) -> Result<(), FactoryError> {
        FluxoraFactory::set_allowlist_enabled(env, enabled)
    }

    fn set_batch_cap_enforcement(env: Env, enforced: bool) -> Result<(), FactoryError> {
        FluxoraFactory::set_batch_cap_enforcement(env, enforced)
    }

    fn set_factory_paused(env: Env, paused: bool) -> Result<(), FactoryError> {
        FluxoraFactory::set_factory_paused(env, paused)
    }

    fn set_rate_bounds(
        env: Env,
        min_rate_per_second: Option<i128>,
        max_rate_per_second: Option<i128>,
    ) -> Result<(), FactoryError> {
        FluxoraFactory::set_rate_bounds(env, min_rate_per_second, max_rate_per_second)
    }
}

/// Extends the TTL of an allowlist entry when it is (re)written or accessed so a populated,
/// actively-queried allowlist stays readable between admin rotations.
pub fn bump_allowlist(env: &Env, key: &DataKey) {
    let max_ttl = env.storage().max_ttl();
    let threshold = INSTANCE_LIFETIME_THRESHOLD.min(max_ttl);
    let bump = INSTANCE_BUMP_AMOUNT.min(max_ttl);
    env.storage().persistent().extend_ttl(key, threshold, bump);
}
