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
//! ## Automated Lifetime Extension & Archival Resilience (Issue #381)
//!
//! While the primary canary entry `Key::Canary` deliberately receives no TTL
//! extension so as to archive on the network schedule, the contract guarantees
//! instance state resilience and operational continuity through automated entry
//! lifetime extension:
//! 1. Instance storage carrying execution state (`Key::Executing`) and probe accounting
//!    (`Key::ProbeCount`) is automatically refreshed via [`invariants::extend_instance_lifetime`].
//! 2. Resilient probe entries and keeper sweeps can invoke [`ArchivalProbe::extend_probe_lifetime`]
//!    or [`ArchivalProbe::batch_extend_probe_lifetime`] to refresh designated probe entries up to
//!    `max_ttl()`, ensuring long-term ledger resilience without footprint bloat.
//!
//! ## What it deliberately does not do
//!
//! No auth, no tokens, no value of any kind. It holds canary and batch
//! probe entries. If it archives and is never restored, nothing is lost. Do not
//! build on it, and do not deploy it to mainnet.
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

#[cfg(test)]
mod test;

pub use error::{ErrorCategory, ProbeError};
/// Backward-compatibility alias for Error.
pub type Error = ProbeError;

use soroban_sdk::{contract, contractimpl, contracttype, Env, String, Symbol, Vec};

/// Maximum batch probe size allowed to prevent host metering exhaustion.
pub const MAX_BATCH_PROBE_SIZE: u32 = 32;

/// Threshold in ledgers (~7 days at 5s/ledger) before TTL extension triggers.
pub const LIFETIME_THRESHOLD_LEDGERS: u32 = 120_960;

/// Target bump amount in ledgers (~30 days at 5s/ledger) for resilient entries.
pub const BUMP_AMOUNT_LEDGERS: u32 = 518_400;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeRecord {
    pub note: Symbol,
    pub planted_at: u32,
}

#[contracttype]
#[repr(u32)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Key {
    /// The single persistent entry whose archival we are waiting for.
    Canary = 0,
    /// Multi-probe partitioned canaries identified by sequence id.
    Probe(u32) = 1,
    /// Execution guard in instance storage to prevent re-entrancy.
    Executing = 2,
    /// Total count of planted probes for state desync tracking.
    ProbeCount = 3,
}

#[contract]
pub struct ArchivalProbe;

#[contractimpl]
impl ArchivalProbe {
    /// Plant the canary once with re-entrancy protection, state synchronization, and instance lifetime extension.
    /// Retries are first-write-wins, so an ambiguous transaction result cannot
    /// reset the archival countdown or replace the original note.
    /// The entry deliberately receives no persistent TTL extension.
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

    /// Plant a keyed probe entry once under a simulated partition id with re-entrancy
    /// lock, state-desync guard, and automated instance lifetime refresh.
    /// Idempotent: first-write-wins to prevent reset during retry cascades.
    pub fn plant_probe(env: Env, id: u32, note: Symbol) -> Result<(), ProbeError> {
        invariants::acquire_executing(&env)?;
        let result = Self::plant_probe_inner(&env, id, note);
        invariants::release_executing(&env);
        invariants::extend_instance_lifetime(&env);
        result
    }

    fn plant_probe_inner(env: &Env, id: u32, note: Symbol) -> Result<(), ProbeError> {
        let count_before = Self::load_probe_count(env);
        let mut planted_delta: u32 = 0;
        let key = Key::Probe(id);

        if !env.storage().persistent().has(&key) {
            let record = ProbeRecord {
                note,
                planted_at: env.ledger().sequence(),
            };
            env.storage().persistent().set(&key, &record);
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

    /// Read a specific keyed probe entry.
    pub fn read_probe(env: Env, id: u32) -> Result<ProbeRecord, ProbeError> {
        env.storage()
            .persistent()
            .get(&Key::Probe(id))
            .ok_or(ProbeError::NotPlanted)
    }

    /// Batch plant multiple probe entries atomically with strict bounds checking,
    /// re-entrancy protection, state-desync verification, and automated instance refresh.
    ///
    /// Consolidates multi-key probes into a single execution frame while guaranteeing
    /// that total probe count advances by exactly the number of newly planted entries.
    pub fn batch_plant(env: Env, ids: Vec<u32>, notes: Vec<Symbol>) -> Result<u32, ProbeError> {
        invariants::acquire_executing(&env)?;
        let result = Self::batch_plant_inner(&env, ids, notes);
        invariants::release_executing(&env);
        invariants::extend_instance_lifetime(&env);
        result
    }

    fn batch_plant_inner(env: &Env, ids: Vec<u32>, notes: Vec<Symbol>) -> Result<u32, ProbeError> {
        let len = ids.len();
        if len == 0 {
            return Err(ProbeError::EmptyBatch);
        }
        if len > MAX_BATCH_PROBE_SIZE || len != notes.len() {
            return Err(ProbeError::BatchTooLarge);
        }

        let count_before = Self::load_probe_count(env);
        let mut planted_count: u32 = 0;
        let seq = env.ledger().sequence();

        for i in 0..len {
            let id = ids.get(i).ok_or(ProbeError::ArithmeticOverflow)?;
            let note = notes.get(i).ok_or(ProbeError::ArithmeticOverflow)?;
            let key = Key::Probe(id);

            if !env.storage().persistent().has(&key) {
                let record = ProbeRecord {
                    note,
                    planted_at: seq,
                };
                env.storage().persistent().set(&key, &record);
                planted_count = planted_count
                    .checked_add(1)
                    .ok_or(ProbeError::ArithmeticOverflow)?;
            }
        }

        let new_count = count_before
            .checked_add(planted_count)
            .ok_or(ProbeError::ArithmeticOverflow)?;
        env.storage().instance().set(&Key::ProbeCount, &new_count);

        let count_final = Self::load_probe_count(env);
        invariants::assert_state_sync(count_before, count_final, planted_count)?;

        Ok(planted_count)
    }

    /// Batch read probe records in a single optimized pass without redundant lookups.
    pub fn batch_read(env: Env, ids: Vec<u32>) -> Result<Vec<ProbeRecord>, ProbeError> {
        let len = ids.len();
        if len == 0 {
            return Err(ProbeError::EmptyBatch);
        }
        if len > MAX_BATCH_PROBE_SIZE {
            return Err(ProbeError::BatchTooLarge);
        }

        let mut records = Vec::new(&env);
        for i in 0..len {
            let id = ids.get(i).ok_or(ProbeError::ArithmeticOverflow)?;
            let record: ProbeRecord = env
                .storage()
                .persistent()
                .get(&Key::Probe(id))
                .ok_or(ProbeError::NotPlanted)?;
            records.push_back(record);
        }

        Ok(records)
    }

    /// Extend the persistent lifetime of an individual keyed probe entry.
    ///
    /// Provides automated archival resilience for designated probe keys requiring
    /// prolonged survival across observation epochs. Unknown or archived IDs
    /// return [`ProbeError::NotPlanted`].
    pub fn extend_probe_lifetime(env: Env, id: u32) -> Result<(), ProbeError> {
        invariants::acquire_executing(&env)?;
        let key = Key::Probe(id);
        if !env.storage().persistent().has(&key) {
            invariants::release_executing(&env);
            return Err(ProbeError::NotPlanted);
        }

        invariants::extend_persistent_entry(&env, &key);
        invariants::release_executing(&env);
        invariants::extend_instance_lifetime(&env);
        Ok(())
    }

    /// Batch extend the persistent entry lifetime of multiple probe entries in one sweep.
    ///
    /// Unknown or unplanted probe IDs are skipped gracefully. Returns the count of
    /// successfully refreshed entries.
    ///
    /// # Errors
    /// * [`ProbeError::EmptyBatch`] — empty vector provided.
    /// * [`ProbeError::BatchTooLarge`] — vector size exceeds [`MAX_BATCH_PROBE_SIZE`].
    pub fn batch_extend_probe_lifetime(env: Env, ids: Vec<u32>) -> Result<u32, ProbeError> {
        let len = ids.len();
        if len == 0 {
            return Err(ProbeError::EmptyBatch);
        }
        if len > MAX_BATCH_PROBE_SIZE {
            return Err(ProbeError::BatchTooLarge);
        }

        invariants::acquire_executing(&env)?;
        let mut extended_count: u32 = 0;

        for i in 0..len {
            let id = match ids.get(i) {
                Some(val) => val,
                None => {
                    invariants::release_executing(&env);
                    return Err(ProbeError::ArithmeticOverflow);
                }
            };
            let key = Key::Probe(id);
            if env.storage().persistent().has(&key) {
                invariants::extend_persistent_entry(&env, &key);
                extended_count = match extended_count.checked_add(1) {
                    Some(val) => val,
                    None => {
                        invariants::release_executing(&env);
                        return Err(ProbeError::ArithmeticOverflow);
                    }
                };
            }
        }

        invariants::release_executing(&env);
        invariants::extend_instance_lifetime(&env);
        Ok(extended_count)
    }

    /// Returns the total number of distinct probe items planted across lifetime.
    pub fn probe_count(env: Env) -> u32 {
        Self::load_probe_count(&env)
    }

    /// Returns whether the contract is currently executing a mutating operation.
    pub fn is_executing(env: Env) -> bool {
        invariants::is_executing(&env)
    }

    /// Return human-readable error description for a numeric error discriminant.
    pub fn error_message(env: Env, discriminant: u32) -> String {
        let msg = ProbeError::from_discriminant(discriminant)
            .map(|e| e.message())
            .unwrap_or("");
        String::from_str(&env, msg)
    }

    /// Return the single-character group letter ('A' through 'E') for an error discriminant.
    ///
    /// Returns 0 (NUL byte) if the discriminant is unrecognized.
    pub fn error_group(_env: Env, discriminant: u32) -> u32 {
        ProbeError::from_discriminant(discriminant)
            .map(|e| e.group() as u32)
            .unwrap_or(0)
    }

    fn load_probe_count(env: &Env) -> u32 {
        env.storage().instance().get(&Key::ProbeCount).unwrap_or(0)
    }
}
