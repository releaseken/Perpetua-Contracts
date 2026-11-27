#![no_std]
//! # Archival probe — **not part of the product**
//!
//! This contract exists for exactly one reason: to prove, against a real
//! network, the one thing Perpetua's unit tests structurally cannot.
//!
//! ## What it is for
//!
//! The SDK's test host runs storage in recording mode, where reading an expired
//! persistent entry is silently auto-restored rather than failing. So the unit
//! suite proves that crossing the archive/restore boundary preserves accounting,
//! but it never sees the live-network sequence:
//!
//! ```text
//!   read archived entry  ->  transaction FAILS
//!   resubmit with RestoreFootprint
//!   read again           ->  succeeds, data intact
//! ```
//!
//! See `docs/KNOWN-LIMITATIONS.md` §1.
//!
//! ## Why a separate contract
//!
//! Perpetua floors every stream entry's TTL at 30 days, and the network floors
//! *any* persistent entry at `min_persistent_ttl` — 120,960 ledgers, about 7
//! days, on both testnet and local quickstart. A real Perpetua stream therefore
//! cannot archive for a month.
//!
//! This probe deliberately does the one thing Perpetua never does: it writes a
//! persistent canary entry and **never extends its TTL**. The entry then lives exactly
//! `min_persistent_ttl` and archives as early as the network permits. The
//! restore mechanism it exercises is identical for any persistent entry — it is
//! a property of the ledger, not of the contract — so proving it here proves it
//! for `DataKey::Stream(id)`.
//!
//! ## Precision-Safe Fractional Stream Withdrawal Calculations
//!
//! Probing contracts and client SDKs executing dry-run or live archival verification
//! simulate partial and fractional withdrawals against streaming positions.
//! When amounts are calculated using naive integer arithmetic `(total * numerator) / denominator`,
//! precision loss can truncate dust balances or trigger intermediate overflow when balances
//! are large.
//!
//! This contract integrates checked mathematical routines utilizing quotient-remainder
//! decomposition:
//!
//! ```text
//! floor(total * n / d) = (total / d) * n + floor(((total % d) * n) / d)
//! ```
//!
//! guaranteeing exact conservation `payout + remainder == total` without precision loss
//! or intermediate overflow.
//!
//! ## Granular Role-Based Authorization (#376)
//!
//! To prevent unauthorized configuration updates, configuration authority is
//! partitioned into orthogonal functional roles:
//!
//! | Role | Name | Scope & Authority |
//! |---|---|---|
//! | `0` | `Admin` | Governs role grants, revocations, and probe parameter management. |
//! | `1` | `ConfigAdmin` | Configures probe limits, batch size constraints, and validation thresholds. |
//! | `2` | `EmergencyGuardian` | Emergency pause and circuit-breaker control for probe operations. |
//! | `3` | `Operator` | Operational execution: permissioned planting and testing runs. |
//!
//! Role assignments are stored in instance storage and validated via `require_role_for`.
//!
//! ## Streamlined Cross-Contract Invocation Overhead (#382)
//!
//! During high-frequency batch claims and multi-stream verification cycles, contracts
//! invoking probes per-item suffer from repetitive sub-invocation overhead.
//! This contract exposes batch operations `batch_plant`, `batch_read`, `batch_claim_probe`,
//! and `batch_get_claim_probes` that execute in a single invocation frame with bounded
//! limits, checked arithmetic, and zero TTL refresh.
//!
//! ## What it deliberately does not do
//!
//! No tokens, no value of any kind. It holds canary notes and probe records.
//! If it archives and is never restored, nothing is lost. Do not build on it, and do
//! not deploy it to mainnet.
//!
//! ## Release isolation (issue #1543)
//!
//! This probe is a workspace member but is **not** part of the deployable product
//! contract list. It stays a member so its smoke test remains wired into the
//! standard workspace checks (`cargo test --workspace`, `cargo fmt --all`,
//! `cargo clippy --all-targets`), but `script/release.sh` — the only command that
//! produces release artifacts — builds **only** the `fluxora-stream` package and
//! rejects any probe wasm among its outputs. To build this probe explicitly, use:
//!
//! ```text
//! cargo build -p fluxora-archival-probe --target wasm32v1-none --release
//! ```
//!
//! and for the live-network round trip, `script/archival-canary.sh`.

pub mod error;
pub mod invariants;
pub mod math;

#[cfg(test)]
mod test;

pub use error::{ErrorCategory, ProbeError};
pub use math::{calculate_bps_withdrawal, calculate_fractional_withdrawal, FractionalSplit};

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, Address, Env, String, Symbol, Vec,
};

/// Maximum batch probe size allowed to prevent host metering exhaustion.
pub const MAX_BATCH_PROBE_SIZE: u32 = 32;

/// Default maximum batch size setting.
pub const DEFAULT_MAX_BATCH_SIZE: u32 = 32;

/// Threshold in ledgers (~7 days at 5s/ledger) before TTL extension triggers.
pub const LIFETIME_THRESHOLD_LEDGERS: u32 = 120_960;

/// Target bump amount in ledgers (~30 days at 5s/ledger) for resilient entries.
pub const BUMP_AMOUNT_LEDGERS: u32 = 518_400;

/// Granular functional roles for archival probe configuration and operations.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Role {
    /// Contract governance, role delegation, and admin rotation.
    Admin = 0,
    /// Probe configuration parameters (e.g. batch size bounds, timeouts).
    ConfigAdmin = 1,
    /// Circuit breaker: emergency probe pause / unpause.
    EmergencyGuardian = 2,
    /// Operational probe execution and automated testing.
    Operator = 3,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeRecord {
    pub note: Symbol,
    pub planted_at: u32,
}

/// Batch claim probe record used to streamline cross-contract verification overhead.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimProbeRecord {
    pub stream_id: u64,
    pub amount: i128,
    pub claimant: Address,
    pub verified_at: u32,
}

/// Global probe configuration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeConfig {
    pub admin: Address,
    pub max_batch_size: u32,
    pub paused: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoleUpdated {
    pub role: Role,
    pub account: Address,
    pub authorized: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigUpdated {
    pub max_batch_size: u32,
    pub paused: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Key {
    /// The single persistent entry whose archival we are waiting for.
    Canary = 0,
    /// Multi-probe partitioned canaries identified by sequence id.
    Probe(u32) = 1,
    /// Batch claim probe entries for cross-contract claim verification.
    ClaimProbe(u64) = 2,
    /// Contract configuration record.
    Config = 3,
    /// Pending admin nominee for two-step rotation.
    PendingAdmin = 4,
    /// Granular role assignments: (Role, Address) -> bool.
    Role(Role, Address) = 5,
    /// Execution guard in instance storage to prevent re-entrancy.
    Executing = 6,
    /// Total count of planted probes for state desync tracking.
    ProbeCount = 7,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// The canary has never been planted.
    NotPlanted = 1,
    /// Batch is empty.
    EmptyBatch = 2,
    /// Batch size exceeds the configured maximum (bounded by [`MAX_BATCH_PROBE_SIZE`]).
    BatchTooLarge = 3,
    /// Entry already planted (write-once invariant).
    DuplicateEntry = 4,
    /// Arithmetic overflow in loop counter or calculation.
    ArithmeticOverflow = 5,
    /// Denominator cannot be zero.
    ZeroDenominator = 6,
    /// Invalid fraction parameters (e.g. numerator > denominator or negative).
    InvalidFraction = 7,
    /// Formal invariant assertion failed.
    InvariantViolation = 8,
    /// Caller does not possess required role authorization.
    UnauthorizedRole = 9,
    /// Invalid address for role or admin (zero address or contract self).
    InvalidRoleAddress = 10,
    /// Contract is already initialized.
    AlreadyInitialized = 11,
    /// Contract is not initialized.
    NotInitialized = 12,
    /// Contract operations are paused by EmergencyGuardian.
    ContractPaused = 13,
    /// Length mismatch in parallel vector arguments.
    LengthMismatch = 14,
}

/// Check whether an address is the forbidden zero address or contract self.
fn is_forbidden_address(env: &Env, addr: &Address) -> bool {
    let zero = Address::from_string(&String::from_str(
        env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    ));
    *addr == zero || *addr == env.current_contract_address()
}

#[contract]
pub struct ArchivalProbe;

#[contractimpl]
impl ArchivalProbe {
    /// Initialize the archival probe contract with an administrative owner.
    pub fn initialize(env: Env, admin: Address) -> Result<(), Error> {
        if env.storage().instance().has(&Key::Config) {
            return Err(Error::AlreadyInitialized);
        }
        if is_forbidden_address(&env, &admin) {
            return Err(Error::InvalidRoleAddress);
        }

        let config = ProbeConfig {
            admin: admin.clone(),
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            paused: false,
        };
        env.storage().instance().set(&Key::Config, &config);
        env.storage()
            .instance()
            .set(&Key::Role(Role::Admin, admin), &true);

        Ok(())
    }

    /// Load the contract configuration.
    pub fn get_config(env: Env) -> Result<ProbeConfig, Error> {
        env.storage()
            .instance()
            .get(&Key::Config)
            .ok_or(Error::NotInitialized)
    }

    /// Check if an account holds a specific role.
    /// Admin role inherently satisfies any role requirement.
    pub fn has_role(env: Env, role: Role, account: Address) -> bool {
        if let Some(config) = env.storage().instance().get::<_, ProbeConfig>(&Key::Config) {
            if account == config.admin {
                return true;
            }
        }
        if env
            .storage()
            .instance()
            .get::<_, bool>(&Key::Role(Role::Admin, account.clone()))
            .unwrap_or(false)
        {
            return true;
        }
        env.storage()
            .instance()
            .get::<_, bool>(&Key::Role(role, account))
            .unwrap_or(false)
    }

    /// Require caller authorization and role verification.
    fn require_role_for(env: &Env, role: Role, caller: &Address) -> Result<ProbeConfig, Error> {
        let config: ProbeConfig = env
            .storage()
            .instance()
            .get(&Key::Config)
            .ok_or(Error::NotInitialized)?;
        caller.require_auth();
        if Self::has_role(env.clone(), role, caller.clone()) {
            Ok(config)
        } else {
            Err(Error::UnauthorizedRole)
        }
    }

    /// Assign or revoke a granular role.
    /// Strictly gated by Role::Admin.
    pub fn set_role(
        env: Env,
        caller: Address,
        role: Role,
        account: Address,
        authorized: bool,
    ) -> Result<(), Error> {
        let config = Self::require_role_for(&env, Role::Admin, &caller)?;
        if is_forbidden_address(&env, &account) {
            return Err(Error::InvalidRoleAddress);
        }

        let key = Key::Role(role, account.clone());
        if authorized {
            env.storage().instance().set(&key, &true);
        } else {
            if role == Role::Admin && account == config.admin {
                return Err(Error::InvalidRoleAddress);
            }
            env.storage().instance().remove(&key);
        }

        env.events().publish(
            (Symbol::new(&env, "role_updated"), role),
            RoleUpdated {
                role,
                account,
                authorized,
            },
        );

        Ok(())
    }

    /// Update probe configuration parameters (gated by ConfigAdmin).
    pub fn set_max_batch_size(env: Env, caller: Address, max_batch_size: u32) -> Result<(), Error> {
        let mut config = Self::require_role_for(&env, Role::ConfigAdmin, &caller)?;
        if max_batch_size == 0 || max_batch_size > MAX_BATCH_PROBE_SIZE {
            return Err(Error::BatchTooLarge);
        }
        config.max_batch_size = max_batch_size;
        env.storage().instance().set(&Key::Config, &config);

        env.events().publish(
            (Symbol::new(&env, "config_updated"),),
            ConfigUpdated {
                max_batch_size: config.max_batch_size,
                paused: config.paused,
            },
        );
        Ok(())
    }

    /// Pause or unpause probe operations (gated by EmergencyGuardian).
    pub fn set_paused(env: Env, caller: Address, paused: bool) -> Result<(), Error> {
        let mut config = Self::require_role_for(&env, Role::EmergencyGuardian, &caller)?;
        config.paused = paused;
        env.storage().instance().set(&Key::Config, &config);

        env.events().publish(
            (Symbol::new(&env, "config_updated"),),
            ConfigUpdated {
                max_batch_size: config.max_batch_size,
                paused: config.paused,
            },
        );
        Ok(())
    }

    /// Rotate the admin address directly (gated by Admin).
    pub fn set_admin(env: Env, caller: Address, new_admin: Address) -> Result<(), Error> {
        let mut config = Self::require_role_for(&env, Role::Admin, &caller)?;
        if is_forbidden_address(&env, &new_admin) {
            return Err(Error::InvalidRoleAddress);
        }

        let old_admin = config.admin.clone();
        config.admin = new_admin.clone();
        env.storage().instance().set(&Key::Config, &config);
        env.storage()
            .instance()
            .set(&Key::Role(Role::Admin, new_admin.clone()), &true);
        env.storage()
            .instance()
            .remove(&Key::Role(Role::Admin, old_admin));
        env.storage().instance().remove(&Key::PendingAdmin);

        env.events().publish(
            (Symbol::new(&env, "admin_rotated"),),
            (config.admin, new_admin),
        );
        Ok(())
    }

    /// Propose a successor admin for two-step rotation (gated by Admin).
    pub fn propose_admin(env: Env, caller: Address, new_admin: Address) -> Result<(), Error> {
        let _config = Self::require_role_for(&env, Role::Admin, &caller)?;
        if is_forbidden_address(&env, &new_admin) {
            return Err(Error::InvalidRoleAddress);
        }
        env.storage().instance().set(&Key::PendingAdmin, &new_admin);
        Ok(())
    }

    /// Accept pending admin nomination (called by nominee).
    pub fn accept_admin(env: Env, nominee: Address) -> Result<(), Error> {
        nominee.require_auth();
        let pending: Address = env
            .storage()
            .instance()
            .get(&Key::PendingAdmin)
            .ok_or(Error::UnauthorizedRole)?;
        if nominee != pending {
            return Err(Error::UnauthorizedRole);
        }

        let mut config: ProbeConfig = env
            .storage()
            .instance()
            .get(&Key::Config)
            .ok_or(Error::NotInitialized)?;
        let old_admin = config.admin.clone();
        config.admin = nominee.clone();
        env.storage().instance().set(&Key::Config, &config);
        env.storage()
            .instance()
            .set(&Key::Role(Role::Admin, nominee.clone()), &true);
        env.storage()
            .instance()
            .remove(&Key::Role(Role::Admin, old_admin));
        env.storage().instance().remove(&Key::PendingAdmin);

        Ok(())
    }

    /// Check if contract is paused.
    fn check_not_paused(env: &Env) -> Result<(), Error> {
        if let Some(config) = env.storage().instance().get::<_, ProbeConfig>(&Key::Config) {
            if config.paused {
                return Err(Error::ContractPaused);
            }
        }
        Ok(())
    }

    /// Plant the canary once. Retries are first-write-wins, so an ambiguous
    /// transaction result cannot reset the archival countdown or replace the
    /// original note. The entry deliberately receives no TTL extension.
    ///
    /// Permissionless: there is nothing here worth protecting.
    pub fn plant(env: Env, note: Symbol) -> Result<(), ProbeError> {
        invariants::acquire_executing(&env)?;
        let result = Self::plant_inner(&env, note);
        invariants::release_executing(&env);
        invariants::extend_instance_lifetime(&env);
        result
    }

    fn plant_inner(env: &Env, note: Symbol) -> Result<(), ProbeError> {
        let count_before = Self::load_probe_count(env);
        let mut planted_delta: u32 = 0;

        if !env.storage().persistent().has(&Key::Canary) {
            env.storage().persistent().set(&Key::Canary, &note);
            // No extend_ttl call on Key::Canary. That omission is the entire point
            // of this contract; do not "fix" it.
            let count_after = count_before
                .checked_add(1)
                .ok_or(ProbeError::ArithmeticOverflow)?;
            env.storage().instance().set(&Key::ProbeCount, &count_after);
            planted_delta = 1;
        }

        let count_final = Self::load_probe_count(env);
        invariants::assert_state_sync(count_before, count_final, planted_delta)?;
        Ok(())
    }

    /// Read the canary.
    ///
    /// Once the entry archives, invoking this fails at the network level before
    /// the contract body runs — the caller must resubmit with a
    /// `RestoreFootprint` operation. That failure is the behaviour under test.
    pub fn read(env: Env) -> Result<Symbol, ProbeError> {
        env.storage()
            .persistent()
            .get(&Key::Canary)
            .ok_or(ProbeError::NotPlanted)
    }

    /// Whether the canary is present and live.
    ///
    /// Mirrors `FluxoraStream::stream_exists`: this is the signal an SDK keys
    /// on to tell "never existed" apart from "archived, needs restoring".
    pub fn planted(env: Env) -> bool {
        env.storage().persistent().has(&Key::Canary)
    }

    /// Plant a keyed probe entry once under a simulated partition id.
    /// Idempotent: first-write-wins to prevent reset during retry cascades.
    pub fn plant_probe(env: Env, id: u32, note: Symbol) {
        let key = Key::Probe(id);
        if env.storage().persistent().has(&key) {
            return;
        }
        let record = ProbeRecord {
            note,
            planted_at: env.ledger().sequence(),
        };
        env.storage().persistent().set(&key, &record);
    }

    /// Read a specific keyed probe entry.
    pub fn read_probe(env: Env, id: u32) -> Result<ProbeRecord, Error> {
        env.storage()
            .persistent()
            .get(&Key::Probe(id))
            .ok_or(Error::NotPlanted)
    }

    /// Calculate fractional stream withdrawal with zero precision loss.
    /// Returns the exact payout amount and remainder satisfying:
    /// `payout + remainder == total`.
    pub fn calculate_fractional_split(
        _env: Env,
        total: i128,
        numerator: i128,
        denominator: i128,
    ) -> Result<FractionalSplit, Error> {
        calculate_fractional_withdrawal(total, numerator, denominator)
    }

    /// Calculate proportional withdrawal split in basis points (10,000 = 100%).
    pub fn calculate_bps_split(_env: Env, total: i128, bps: u32) -> Result<FractionalSplit, Error> {
        calculate_bps_withdrawal(total, bps)
    }

    /// Batch plant multiple probe entries atomically with bounds checking.
    pub fn batch_plant(env: Env, ids: Vec<u32>, notes: Vec<Symbol>) -> Result<u32, Error> {
        Self::check_not_paused(&env)?;
        let len = ids.len();
        if len == 0 {
            return Err(Error::EmptyBatch);
        }
        let limit = env
            .storage()
            .instance()
            .get::<_, ProbeConfig>(&Key::Config)
            .map(|c| c.max_batch_size)
            .unwrap_or(MAX_BATCH_PROBE_SIZE);

        if len > MAX_BATCH_PROBE_SIZE || len > limit || len != notes.len() {
            return Err(Error::BatchTooLarge);
        }

        let mut planted_count: u32 = 0;
        let seq = env.ledger().sequence();

        for i in 0..len {
            let id = ids.get(i).ok_or(Error::ArithmeticOverflow)?;
            let note = notes.get(i).ok_or(Error::ArithmeticOverflow)?;
            let key = Key::Probe(id);

            if !env.storage().persistent().has(&key) {
                let record = ProbeRecord {
                    note,
                    planted_at: seq,
                };
                env.storage().persistent().set(&key, &record);
                planted_count = planted_count
                    .checked_add(1)
                    .ok_or(Error::ArithmeticOverflow)?;
            }
        }

        Ok(planted_count)
    }

    /// Batch read probe records in a single optimized pass without redundant lookups.
    pub fn batch_read(env: Env, ids: Vec<u32>) -> Result<Vec<ProbeRecord>, Error> {
        let len = ids.len();
        if len == 0 {
            return Err(Error::EmptyBatch);
        }
        let limit = env
            .storage()
            .instance()
            .get::<_, ProbeConfig>(&Key::Config)
            .map(|c| c.max_batch_size)
            .unwrap_or(MAX_BATCH_PROBE_SIZE);

        if len > MAX_BATCH_PROBE_SIZE || len > limit {
            return Err(Error::BatchTooLarge);
        }

        let mut records = Vec::new(&env);
        for i in 0..len {
            let id = ids.get(i).ok_or(Error::ArithmeticOverflow)?;
            let record: ProbeRecord = env
                .storage()
                .persistent()
                .get(&Key::Probe(id))
                .ok_or(Error::NotPlanted)?;
            records.push_back(record);
        }

        Ok(records)
    }

    /// Streamlined batch claim probe recording for high-frequency claims.
    ///
    /// Consolidates multi-stream claim probe assertions into a single cross-contract
    /// invocation frame, eliminating repetitive invocation overhead.
    /// Idempotent first-write-wins: existing records are never overwritten, and TTL is
    /// deliberately never extended.
    pub fn batch_claim_probe(
        env: Env,
        claimant: Address,
        stream_ids: Vec<u64>,
        amounts: Vec<i128>,
    ) -> Result<u32, Error> {
        Self::check_not_paused(&env)?;
        claimant.require_auth();

        let len = stream_ids.len();
        if len == 0 {
            return Err(Error::EmptyBatch);
        }
        let limit = env
            .storage()
            .instance()
            .get::<_, ProbeConfig>(&Key::Config)
            .map(|c| c.max_batch_size)
            .unwrap_or(MAX_BATCH_PROBE_SIZE);

        if len > MAX_BATCH_PROBE_SIZE || len > limit {
            return Err(Error::BatchTooLarge);
        }
        if len != amounts.len() {
            return Err(Error::LengthMismatch);
        }

        let seq = env.ledger().sequence();
        let mut recorded: u32 = 0;

        for i in 0..len {
            let stream_id = stream_ids.get(i).ok_or(Error::ArithmeticOverflow)?;
            let amount = amounts.get(i).ok_or(Error::ArithmeticOverflow)?;
            if amount < 0 {
                return Err(Error::ArithmeticOverflow);
            }

            let key = Key::ClaimProbe(stream_id);
            if !env.storage().persistent().has(&key) {
                let rec = ClaimProbeRecord {
                    stream_id,
                    amount,
                    claimant: claimant.clone(),
                    verified_at: seq,
                };
                env.storage().persistent().set(&key, &rec);
                recorded = recorded.checked_add(1).ok_or(Error::ArithmeticOverflow)?;
            }
        }

        Ok(recorded)
    }

    /// Batch read claim probe records in a single cross-contract invocation.
    ///
    /// Returns a vector of `Option<ClaimProbeRecord>` for each requested `stream_id`.
    pub fn batch_get_claim_probes(
        env: Env,
        stream_ids: Vec<u64>,
    ) -> Result<Vec<Option<ClaimProbeRecord>>, Error> {
        let len = stream_ids.len();
        if len == 0 {
            return Err(Error::EmptyBatch);
        }
        let limit = env
            .storage()
            .instance()
            .get::<_, ProbeConfig>(&Key::Config)
            .map(|c| c.max_batch_size)
            .unwrap_or(MAX_BATCH_PROBE_SIZE);

        if len > MAX_BATCH_PROBE_SIZE || len > limit {
            return Err(Error::BatchTooLarge);
        }

        let mut results = Vec::new(&env);
        for i in 0..len {
            let stream_id = stream_ids.get(i).ok_or(Error::ArithmeticOverflow)?;
            let key = Key::ClaimProbe(stream_id);
            let opt_record: Option<ClaimProbeRecord> = env.storage().persistent().get(&key);
            results.push_back(opt_record);
        }

        Ok(results)
    }
}
