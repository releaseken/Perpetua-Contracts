use crate::error::FeeError;
use crate::types::AccruedFeeBreakdown;

pub const BPS_DENOMINATOR: i128 = 10_000;

/// Compute the fee breakdown for a given gross amount and basis point rates.
/// Uses defensive checked arithmetic throughout.
pub fn compute_fee_breakdown(
    gross_amount: i128,
    protocol_fee_bps: u32,
    keeper_split_bps: u32,
) -> Result<AccruedFeeBreakdown, FeeError> {
    if gross_amount < 0 {
        return Err(FeeError::ArithmeticOverflow);
    }
    if protocol_fee_bps > 10_000 || keeper_split_bps > 10_000 {
        return Err(FeeError::InvalidBpsRate);
    }
    if gross_amount == 0 || protocol_fee_bps == 0 {
        return Ok(AccruedFeeBreakdown {
            gross_amount,
            total_fee: 0,
            protocol_fee: 0,
            keeper_fee: 0,
            net_amount: gross_amount,
        });
    }

    let proto_bps_i128 = protocol_fee_bps as i128;
    let keeper_bps_i128 = keeper_split_bps as i128;

    // total_fee = (gross_amount * protocol_fee_bps) / 10_000
    let total_fee_prod = gross_amount
        .checked_mul(proto_bps_i128)
        .ok_or(FeeError::ArithmeticOverflow)?;
    let total_fee = total_fee_prod
        .checked_div(BPS_DENOMINATOR)
        .ok_or(FeeError::ArithmeticOverflow)?;

    // keeper_fee = (total_fee * keeper_split_bps) / 10_000
    let keeper_fee_prod = total_fee
        .checked_mul(keeper_bps_i128)
        .ok_or(FeeError::ArithmeticOverflow)?;
    let keeper_fee = keeper_fee_prod
        .checked_div(BPS_DENOMINATOR)
        .ok_or(FeeError::ArithmeticOverflow)?;

    // protocol_fee = total_fee - keeper_fee
    let protocol_fee = total_fee
        .checked_sub(keeper_fee)
        .ok_or(FeeError::ArithmeticOverflow)?;

    // net_amount = gross_amount - total_fee
    let net_amount = gross_amount
        .checked_sub(total_fee)
        .ok_or(FeeError::ArithmeticOverflow)?;

    // Invariant check: protocol_fee + keeper_fee == total_fee && net_amount + total_fee == gross_amount
    if protocol_fee
        .checked_add(keeper_fee)
        .ok_or(FeeError::ArithmeticOverflow)?
        != total_fee
    {
        return Err(FeeError::InvariantViolation);
    }
    if net_amount
        .checked_add(total_fee)
        .ok_or(FeeError::ArithmeticOverflow)?
        != gross_amount
    {
        return Err(FeeError::InvariantViolation);
    }

    Ok(AccruedFeeBreakdown {
        gross_amount,
        total_fee,
        protocol_fee,
        keeper_fee,
        net_amount,
    })
}
