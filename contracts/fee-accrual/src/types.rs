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
