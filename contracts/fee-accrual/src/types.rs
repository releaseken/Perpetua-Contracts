use soroban_sdk::{contracttype, Address};

/// Storage keys for the fee-accrual contract.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Instance storage: contract configuration record.
    Config,
    /// Instance storage: proposed next admin for two-step rotation.
    PendingAdmin,
    /// Persistent storage: per-stream fee accrual record.
    StreamFee(u64),
    /// Instance storage: total cumulative protocol fees accrued.
    TotalProtocolFees,
    /// Instance storage: total cumulative keeper fees accrued.
    TotalKeeperFees,
    /// Instance storage: re-entrancy guard flag (issue #388).
    /// Set to `true` while a state-modifying fee operation is executing, cleared on exit.
    /// A re-entrant invocation that finds this flag set returns [`FeeError::ReentrancyGuard`]
    /// immediately before reading or modifying state.
    Executing,
}

/// Global fee configuration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeConfig {
    /// Administrative authority.
    pub admin: Address,
    /// Recipient address for protocol fee withdrawals.
    pub fee_recipient: Address,
    /// Protocol fee basis points (e.g. 100 = 1.00%, max 10_000).
    pub protocol_fee_bps: u32,
    /// Keeper fee split basis points out of protocol fee (e.g. 2_000 = 20%).
    pub keeper_split_bps: u32,
}

/// Fee accrual details for a specific stream.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamFeeRecord {
    /// Associated stream identifier.
    pub stream_id: u64,
    /// Cumulative gross amount processed.
    pub total_processed: i128,
    /// Accrued protocol fee amount.
    pub protocol_fee_accrued: i128,
    /// Accrued keeper fee amount.
    pub keeper_fee_accrued: i128,
    /// Claimed protocol fee amount.
    pub protocol_fee_claimed: i128,
    /// Claimed keeper fee amount.
    pub keeper_fee_claimed: i128,
    /// Whether this stream's fee accounting is finalized.
    pub is_settled: bool,
}

/// Summary of an accrued fee computation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccruedFeeBreakdown {
    /// Gross amount evaluated.
    pub gross_amount: i128,
    /// Total fee cut taken.
    pub total_fee: i128,
    /// Portion allocated to protocol treasury.
    pub protocol_fee: i128,
    /// Portion allocated to keeper reward.
    pub keeper_fee: i128,
    /// Net amount remaining after fee deduction.
    pub net_amount: i128,
}

/// Input parameter for batch fee accrual.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchAccrueItem {
    pub stream_id: u64,
    pub amount: i128,
}

/// Coalesced summary returned by batch fee operations (Issue #391).
///
/// Allows callers (such as the stream contract or keeper routers) to receive
/// the entire batch fee settlement or accrual breakdown in a single
/// cross-contract return payload, eliminating O(N) sub-invocations during
/// high-frequency batch claims.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchClaimSummary {
    /// Number of streams processed in the batch.
    pub streams_processed: u32,
    /// Aggregate gross amount across all processed streams.
    pub total_gross_amount: i128,
    /// Aggregate protocol fee accrued or claimed across the batch.
    pub total_protocol_fee: i128,
    /// Aggregate keeper fee accrued or claimed across the batch.
    pub total_keeper_fee: i128,
    /// Aggregate net amount remaining after fee deduction.
    pub total_net_amount: i128,
}
