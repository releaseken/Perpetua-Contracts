//! Precision-safe fractional withdrawal arithmetic.
//!
//! # The problem: fractional withdrawal precision loss
//!
//! The root accrual formula (`deposited * elapsed / duration`) uses integer
//! division, which truncates toward zero. That is intentional and correct for
//! the **vested** quantity: truncation keeps the contract solvent (the residue
//! is refundable, never stranded), and conservation is exact by definition.
//!
//! The problem arises in a subtler place: **partial withdrawals** that are
//! expressed as a fraction of the currently withdrawable balance. Consider:
//!
//! ```text
//! withdrawable = 7 stroops
//! user requests: "50% of withdrawable"
//! naive: 7 * 50 / 100 = 3  (loses 0.5 stroops)
//! ```
//!
//! With the naive formula, repeated 50% withdrawals leave a 1-stroop residue
//! in the contract indefinitely — not a solvency bug (the residue is still
//! accessible by a full withdraw), but a precision artifact that breaks
//! referential-transparency expectations for clients that model the contract in
//! floating-point arithmetic and expect their split to yield the whole amount
//! when summed.
//!
//! A second case is **multi-party pro-rata splits**: a stream shared by N
//! beneficiaries, each claiming `deposited / N` per epoch. If `deposited` is
//! not divisible by `N`, naïve division silently leaves a residue.
//!
//! # What this module provides
//!
//! * [`split_proportional`] — divide a withdrawable balance into two parts
//!   with the guarantee that `part_a + part_b == total`. The first part is
//!   computed by `total * numerator / denominator` (truncated), and the second
//!   part is the exact complement `total - part_a`. This eliminates the residue
//!   by assignment rather than by raising precision.
//!
//! * [`batch_split_equal`] — divide a withdrawable balance into `n` equal
//!   shares, distributing the truncation residue across the first `r` shares
//!   (where `r = total % n`). No residue is ever discarded.
//!
//! * [`WithdrawalSplit`] — the return type of [`split_proportional`], carrying
//!   both parts and an invariant-checked constructor.
//!
//! * [`ProportionalWithdraw`] — a single-step helper used by the dispute-window
//!   path: computes how much of the accrued balance to release now vs. hold in
//!   reserve, ensuring the two halves always sum to the full withdrawable amount.
//!
//! # Formal precision guarantee
//!
//! **Claim.** For any `total >= 0`, `numerator >= 0`, `denominator > 0`:
//!
//! ```text
//! split_proportional(total, numerator, denominator).part_a
//!     + split_proportional(total, numerator, denominator).part_b
//!     == total
//! ```
//!
//! **Proof.** `part_a = total * numerator / denominator` (integer division).
//! `part_b = total - part_a`. Therefore `part_a + part_b = total - part_a + part_a = total`. ∎
//!
//! **Corollary.** Neither part is negative. `part_a >= 0` because integer
//! division of non-negative values is non-negative. `part_b = total - part_a >= 0`
//! because `total * numerator / denominator <= total * denominator / denominator = total`.
//!
//! # Interaction with `accrual.rs`
//!
//! The functions in this module operate on amounts that have already been
//! computed by [`crate::accrual::withdrawable`]. They are a *post-processing*
//! layer on top of the accrual result, not a replacement for it. The accrual
//! invariants (I1–I6 in `accrual.rs`) continue to be the ultimate source of
//! truth; this module cannot and does not violate them.

use crate::error::Error;

/// The result of a proportional split of a withdrawable balance.
///
/// `part_a + part_b == total` is guaranteed by construction.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct WithdrawalSplit {
    /// The portion allocated to the first party (numerator / denominator of
    /// the total). Always `<= total`.
    pub part_a: i128,
    /// The exact complement: `total - part_a`. Never negative, never zero when
    /// `numerator < denominator` and `total > 0`.
    pub part_b: i128,
    /// The original total. Stored here so callers can assert conservation
    /// without re-summing.
    pub total: i128,
}

impl WithdrawalSplit {
    /// Assert the conservation identity `part_a + part_b == total`.
    ///
    /// A `debug_assert`-equivalent: compiled out in release builds, so it costs
    /// nothing on chain while remaining available to the test suite.
    #[cfg(debug_assertions)]
    pub fn assert_invariant(&self) {
        debug_assert_eq!(
            self.part_a + self.part_b,
            self.total,
            "WithdrawalSplit invariant broken: {} + {} != {}",
            self.part_a,
            self.part_b,
            self.total,
        );
    }

    #[cfg(not(debug_assertions))]
    #[inline(always)]
    pub fn assert_invariant(&self) {}
}

/// Split `total` proportionally: `part_a = floor(total * numerator / denominator)`,
/// `part_b = total - part_a`.
///
/// # Precision guarantee
///
/// `part_a + part_b == total` exactly. The truncation residue from integer
/// division of `total * numerator / denominator` is absorbed into `part_b`
/// rather than discarded, so no stroop is ever lost.
///
/// # Arguments
///
/// * `total` — The total withdrawable amount to split. Must be `>= 0`.
/// * `numerator` — The numerator of the fraction allocated to `part_a`.
///   Must be in `[0, denominator]`.
/// * `denominator` — The denominator. Must be `> 0`.
///
/// # Errors
///
/// * [`Error::Overflow`] — `total * numerator` overflows `i128`.
/// * [`Error::InvalidAmount`] — `total < 0`, `numerator < 0`, or
///   `numerator > denominator`.
/// * [`Error::InvalidTimeRange`] — `denominator == 0` (reuses the
///   "zero denominator" semantic from creation validation; a dedicated
///   `ZeroDenominator` variant would require a new ABI discriminant).
///
/// # Examples
///
/// ```rust
/// use crate::withdrawal_math::split_proportional;
///
/// // 7 split 50/100 → (3, 4), not (3, 3)
/// let s = split_proportional(7, 50, 100).unwrap();
/// assert_eq!(s.part_a, 3);
/// assert_eq!(s.part_b, 4);
/// assert_eq!(s.part_a + s.part_b, 7);
/// ```
pub fn split_proportional(
    total: i128,
    numerator: i128,
    denominator: i128,
) -> Result<WithdrawalSplit, Error> {
    if total < 0 {
        return Err(Error::InvalidAmount);
    }
    if denominator == 0 {
        // Reuse InvalidTimeRange as the "zero denominator" sentinel. See the
        // doc comment above for why a new variant was not added.
        return Err(Error::InvalidTimeRange);
    }
    if numerator < 0 || numerator > denominator {
        return Err(Error::InvalidAmount);
    }

    // Compute part_a as floor(total * numerator / denominator).
    let product = total.checked_mul(numerator).ok_or(Error::Overflow)?;
    let part_a = product
        .checked_div(denominator)
        .ok_or(Error::Overflow)?;

    // part_b is the exact complement. Because part_a <= total (see the proof
    // in the module doc), this subtraction never underflows.
    let part_b = total - part_a;

    let result = WithdrawalSplit { part_a, part_b, total };
    result.assert_invariant();
    Ok(result)
}

/// Divide `total` into `n` equal shares, distributing the truncation residue.
///
/// Returns a `Vec`-like structure as an array up to 16 elements (matching
/// `MAX_BATCH_SIZE`). The first `r = total % n` shares receive one extra
/// stroop so that the sum across all shares equals `total` exactly.
///
/// # Precision guarantee
///
/// `shares.iter().sum() == total` exactly, for any `total >= 0` and `n > 0`.
///
/// # Arguments
///
/// * `total` — Total amount to split. Must be `>= 0`.
/// * `n` — Number of shares. Must be in `1..=MAX_N`.
///
/// # Errors
///
/// * [`Error::InvalidAmount`] — `total < 0` or `n == 0`.
/// * [`Error::BatchTooLarge`] — `n > MAX_N` (16, matching `MAX_BATCH_SIZE`).
///
/// # Notes
///
/// Returns a fixed-size `[i128; 16]` array together with the actual count `n`
/// to avoid heap allocation in `no_std`. The first `n` elements are populated;
/// the rest are zero.
pub fn batch_split_equal(total: i128, n: u32) -> Result<([i128; 16], u32), Error> {
    const MAX_N: u32 = 16;
    if total < 0 {
        return Err(Error::InvalidAmount);
    }
    if n == 0 {
        return Err(Error::InvalidAmount);
    }
    if n > MAX_N {
        return Err(Error::BatchTooLarge);
    }

    let base = total / n as i128;
    let remainder = (total % n as i128) as u32;

    let mut shares = [0i128; 16];
    for i in 0..n as usize {
        shares[i] = if (i as u32) < remainder {
            base + 1
        } else {
            base
        };
    }

    Ok((shares, n))
}

/// A proportional release descriptor for the dispute-window path.
///
/// When a stream is used in a dispute-window context, the amount that can be
/// released during a challenge period is `release_now`, and the amount held in
/// reserve until the dispute window closes is `hold_in_reserve`. The two
/// always sum to `withdrawable`.
///
/// # Formal guarantee
///
/// `release_now + hold_in_reserve == withdrawable` exactly.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ProportionalWithdraw {
    /// Amount released immediately: `withdrawable * release_bps / 10_000`.
    pub release_now: i128,
    /// Amount held in reserve: `withdrawable - release_now`.
    pub hold_in_reserve: i128,
    /// The source amount this split was computed from.
    pub withdrawable: i128,
}

/// Compute a proportional release for the dispute-window path.
///
/// `release_bps` is a basis-point fraction in `[0, 10_000]`. Setting
/// `release_bps = 10_000` releases everything; `release_bps = 0` holds
/// everything in reserve.
///
/// # Precision guarantee
///
/// `result.release_now + result.hold_in_reserve == withdrawable` exactly.
///
/// # Arguments
///
/// * `withdrawable` — The full currently-withdrawable amount (`>= 0`).
/// * `release_bps` — Basis points to release now. Must be in `[0, 10_000]`.
///
/// # Errors
///
/// * [`Error::InvalidAmount`] — `withdrawable < 0` or `release_bps > 10_000`.
/// * [`Error::Overflow`] — `withdrawable * release_bps` overflows `i128`.
///
/// # Examples
///
/// ```rust
/// use crate::withdrawal_math::proportional_withdraw;
///
/// // 7 withdrawable, 5000 bps (50%) → release 3, hold 4
/// let p = proportional_withdraw(7, 5_000).unwrap();
/// assert_eq!(p.release_now, 3);
/// assert_eq!(p.hold_in_reserve, 4);
/// assert_eq!(p.release_now + p.hold_in_reserve, 7);
/// ```
pub fn proportional_withdraw(withdrawable: i128, release_bps: u32) -> Result<ProportionalWithdraw, Error> {
    if release_bps > 10_000 {
        return Err(Error::InvalidAmount);
    }

    let split = split_proportional(withdrawable, release_bps as i128, 10_000)?;
    Ok(ProportionalWithdraw {
        release_now: split.part_a,
        hold_in_reserve: split.part_b,
        withdrawable,
    })
}

/// Compute the amount that can be withdrawn while respecting a minimum reserve
/// fraction expressed in basis points.
///
/// This is the complement of [`proportional_withdraw`]: instead of specifying
/// what fraction to release, the caller specifies what fraction must be kept in
/// reserve (`reserve_bps`). The releasable amount is the complement.
///
/// # Arguments
///
/// * `withdrawable` — The full currently-withdrawable amount (`>= 0`).
/// * `reserve_bps` — Basis points to hold in reserve. Must be in `[0, 10_000]`.
///
/// # Errors
///
/// Same as [`proportional_withdraw`].
pub fn withdraw_with_reserve(withdrawable: i128, reserve_bps: u32) -> Result<ProportionalWithdraw, Error> {
    if reserve_bps > 10_000 {
        return Err(Error::InvalidAmount);
    }
    let release_bps = 10_000 - reserve_bps;
    proportional_withdraw(withdrawable, release_bps)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // split_proportional: conservation
    // -----------------------------------------------------------------------

    #[test]
    fn split_sums_to_total_exact_divisor() {
        let s = split_proportional(100, 1, 2).unwrap();
        assert_eq!(s.part_a, 50);
        assert_eq!(s.part_b, 50);
        assert_eq!(s.part_a + s.part_b, 100);
    }

    #[test]
    fn split_distributes_residue_to_part_b() {
        // 7 split 50/100: 3.5 → part_a=3, part_b=4
        let s = split_proportional(7, 50, 100).unwrap();
        assert_eq!(s.part_a, 3);
        assert_eq!(s.part_b, 4);
        assert_eq!(s.part_a + s.part_b, 7);
    }

    #[test]
    fn split_at_zero_numerator_gives_all_to_part_b() {
        let s = split_proportional(1_000, 0, 100).unwrap();
        assert_eq!(s.part_a, 0);
        assert_eq!(s.part_b, 1_000);
    }

    #[test]
    fn split_at_full_numerator_gives_all_to_part_a() {
        let s = split_proportional(1_000, 100, 100).unwrap();
        assert_eq!(s.part_a, 1_000);
        assert_eq!(s.part_b, 0);
    }

    #[test]
    fn split_zero_total_yields_zero_parts() {
        let s = split_proportional(0, 3, 10).unwrap();
        assert_eq!(s.part_a, 0);
        assert_eq!(s.part_b, 0);
    }

    /// Conservation holds across a dense sweep of (total, numerator,
    /// denominator) triples. This is a deterministic pseudo-random sweep — no
    /// external RNG, reproducible in CI.
    #[test]
    fn split_conservation_sweep() {
        let totals: [i128; 10] = [0, 1, 2, 3, 7, 10, 100, 999, 10_000, i128::MAX / 10_000];
        let fracs: [(i128, i128); 8] = [
            (0, 1),
            (1, 1),
            (1, 2),
            (1, 3),
            (2, 3),
            (3, 7),
            (99, 100),
            (9_999, 10_000),
        ];

        for total in totals {
            for (num, den) in fracs {
                let s = split_proportional(total, num, den)
                    .unwrap_or_else(|e| panic!("split_proportional({total},{num},{den}) failed: {e:?}"));
                assert_eq!(
                    s.part_a + s.part_b,
                    total,
                    "conservation broken for total={total} num={num} den={den}: {} + {} != {total}",
                    s.part_a,
                    s.part_b,
                );
                assert!(s.part_a >= 0, "part_a is negative: {}", s.part_a);
                assert!(s.part_b >= 0, "part_b is negative: {}", s.part_b);
            }
        }
    }

    // -----------------------------------------------------------------------
    // split_proportional: error cases
    // -----------------------------------------------------------------------

    #[test]
    fn split_zero_denominator_errors() {
        let err = split_proportional(100, 1, 0).unwrap_err();
        assert_eq!(err, Error::InvalidTimeRange);
    }

    #[test]
    fn split_negative_total_errors() {
        let err = split_proportional(-1, 1, 2).unwrap_err();
        assert_eq!(err, Error::InvalidAmount);
    }

    #[test]
    fn split_numerator_exceeds_denominator_errors() {
        let err = split_proportional(100, 11, 10).unwrap_err();
        assert_eq!(err, Error::InvalidAmount);
    }

    // -----------------------------------------------------------------------
    // batch_split_equal: conservation
    // -----------------------------------------------------------------------

    #[test]
    fn batch_split_exact_divisor() {
        let (shares, n) = batch_split_equal(12, 3).unwrap();
        let used: &[i128] = &shares[..n as usize];
        assert_eq!(used, [4, 4, 4]);
        assert_eq!(used.iter().sum::<i128>(), 12);
    }

    #[test]
    fn batch_split_distributes_residue_across_first_r_shares() {
        // 10 / 3 = 3 remainder 1 → shares: [4, 3, 3]
        let (shares, n) = batch_split_equal(10, 3).unwrap();
        let used: &[i128] = &shares[..n as usize];
        assert_eq!(used, [4, 3, 3]);
        assert_eq!(used.iter().sum::<i128>(), 10);
    }

    #[test]
    fn batch_split_single_share_is_total() {
        let (shares, n) = batch_split_equal(99, 1).unwrap();
        assert_eq!(shares[0], 99);
        assert_eq!(n, 1);
    }

    #[test]
    fn batch_split_zero_total_all_shares_zero() {
        let (shares, n) = batch_split_equal(0, 5).unwrap();
        let used: &[i128] = &shares[..n as usize];
        assert!(used.iter().all(|&s| s == 0));
        assert_eq!(used.iter().sum::<i128>(), 0);
    }

    #[test]
    fn batch_split_max_n_accepted() {
        let (shares, n) = batch_split_equal(16, 16).unwrap();
        let used: &[i128] = &shares[..n as usize];
        assert_eq!(used.iter().sum::<i128>(), 16);
    }

    #[test]
    fn batch_split_exceeds_max_n_errors() {
        let err = batch_split_equal(100, 17).unwrap_err();
        assert_eq!(err, Error::BatchTooLarge);
    }

    #[test]
    fn batch_split_zero_n_errors() {
        let err = batch_split_equal(100, 0).unwrap_err();
        assert_eq!(err, Error::InvalidAmount);
    }

    /// Conservation sweep: for every (total, n) pair, the shares sum to total.
    #[test]
    fn batch_split_conservation_sweep() {
        let totals: [i128; 8] = [0, 1, 3, 7, 10, 15, 100, 10_007];
        for total in totals {
            for n in 1u32..=16 {
                let (shares, count) = batch_split_equal(total, n).unwrap();
                let sum: i128 = shares[..count as usize].iter().sum();
                assert_eq!(
                    sum, total,
                    "batch conservation broken for total={total} n={n}: sum={sum}",
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // proportional_withdraw
    // -----------------------------------------------------------------------

    #[test]
    fn proportional_withdraw_full_release_bps() {
        let p = proportional_withdraw(1_000, 10_000).unwrap();
        assert_eq!(p.release_now, 1_000);
        assert_eq!(p.hold_in_reserve, 0);
        assert_eq!(p.release_now + p.hold_in_reserve, 1_000);
    }

    #[test]
    fn proportional_withdraw_zero_release_bps() {
        let p = proportional_withdraw(1_000, 0).unwrap();
        assert_eq!(p.release_now, 0);
        assert_eq!(p.hold_in_reserve, 1_000);
        assert_eq!(p.release_now + p.hold_in_reserve, 1_000);
    }

    #[test]
    fn proportional_withdraw_fractional_residue_absorbed_by_reserve() {
        // 7 at 5000 bps (50%): release 3, hold 4
        let p = proportional_withdraw(7, 5_000).unwrap();
        assert_eq!(p.release_now, 3);
        assert_eq!(p.hold_in_reserve, 4);
        assert_eq!(p.release_now + p.hold_in_reserve, 7);
    }

    #[test]
    fn proportional_withdraw_exceeds_10_000_bps_errors() {
        let err = proportional_withdraw(1_000, 10_001).unwrap_err();
        assert_eq!(err, Error::InvalidAmount);
    }

    #[test]
    fn proportional_withdraw_conservation_sweep() {
        let amounts: [i128; 8] = [0, 1, 3, 7, 11, 100, 999, 10_007];
        let bps_values: [u32; 7] = [0, 1, 1_000, 3_333, 5_000, 9_999, 10_000];

        for amount in amounts {
            for bps in bps_values {
                let p = proportional_withdraw(amount, bps).unwrap();
                assert_eq!(
                    p.release_now + p.hold_in_reserve,
                    amount,
                    "proportional_withdraw conservation broken: amount={amount} bps={bps}",
                );
                assert!(p.release_now >= 0);
                assert!(p.hold_in_reserve >= 0);
            }
        }
    }

    // -----------------------------------------------------------------------
    // withdraw_with_reserve
    // -----------------------------------------------------------------------

    #[test]
    fn withdraw_with_reserve_full_reserve_releases_nothing() {
        let p = withdraw_with_reserve(500, 10_000).unwrap();
        assert_eq!(p.release_now, 0);
        assert_eq!(p.hold_in_reserve, 500);
    }

    #[test]
    fn withdraw_with_reserve_zero_reserve_releases_all() {
        let p = withdraw_with_reserve(500, 0).unwrap();
        assert_eq!(p.release_now, 500);
        assert_eq!(p.hold_in_reserve, 0);
    }

    #[test]
    fn withdraw_with_reserve_conservation() {
        let p = withdraw_with_reserve(13, 3_000).unwrap();
        // release_bps = 7_000 → 13 * 7000 / 10000 = 9 (truncated), hold = 4
        assert_eq!(p.release_now + p.hold_in_reserve, 13);
    }
}
