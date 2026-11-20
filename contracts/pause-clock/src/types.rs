use soroban_sdk::{contracttype, Address};

/// Storage keys for the pause-clock contract.
///
/// `Config` and `PendingAdmin` live in instance storage (shared with the
/// contract's TTL, tiny, always bumped together).  Per-stream `PausedTotal`
/// records live in persistent storage with their own independently managed
/// TTLs.
#[contracttype]
#[repr(u32)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Instance storage. The main configuration record.
    Config = 0,
    /// Instance storage. Proposed next admin for two-step rotation.
    PendingAdmin = 1,
    /// Persistent storage. Per-stream cumulative paused seconds.
    PausedTotal(u64) = 2,
}

/// The on-chain configuration record for the pause-clock.
///
/// Written atomically by `init` and re-written atomically by every setter.
/// A reader of `get_config` always observes a coherent record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClockConfig {
    /// Address authorised to call admin-gated operations.
    pub admin: Address,
    /// The stream contract whose calls this clock trusts.
    pub stream_contract: Address,
    /// Whether the global clock is currently paused.
    pub paused: bool,
    /// Ledger timestamp at which the global clock was last paused.
    /// `None` when the clock is not paused.
    pub paused_at: Option<u64>,
    /// Cumulative seconds the global clock has been paused (not including
    /// any in-progress pause interval).
    pub paused_total: u64,
    /// Total number of pause/resume cycles ever recorded.
    pub cycle_count: u64,
}
