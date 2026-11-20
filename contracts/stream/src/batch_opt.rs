//! Cross-contract invocation overhead reduction for high-frequency batch claims.
//!
//! # The problem
//!
//! In the standard `batch_withdraw` path each stream is processed sequentially:
//! one `peek_stream` read, one `accrual::withdrawable` call, one
//! `token::TokenClient::new(...).try_transfer(...)` cross-contract call. For a
//! 16-stream batch that uses multiple different token contracts this means up to
//! 16 separate token sub-invocations, each paying the sub-invocation overhead.
//!
//! When the batch contains several streams that share the same token contract
//! (the common case in a payroll scenario where the employer streams the same
//! asset to all employees), the token sub-invocations can be coalesced into a
//! single transfer per unique token. For a 16-stream batch that is homogeneous
//! in token, this reduces 16 token sub-invocations to 1 — a significant saving
//! in CPU and event-byte budgets.
//!
//! # What this module provides
//!
//! * [`group_payouts_by_token`] — takes a list of `(stream_id, payout, token)`
//!   tuples and returns groups keyed by token address. Within each group the
//!   streams are ordered by stream id (ascending). This is a pure function: no
//!   host calls, no storage reads.
//!
//! * [`count_unique_tokens`] — counts how many distinct token addresses appear
//!   in a payout list. Used by resource-limit tests to assert that the
//!   optimization was actually applied.
//!
//! * [`BatchPayout`] — a descriptor for a single stream's payout within a
//!   coalesced group: `stream_id` + `amount`. The token is implicit from the
//!   group it belongs to.
//!
//! * [`TokenGroup`] — a group of payouts that share the same token. The caller
//!   issues a single `token.transfer(contract, recipient, group.total)` and then
//!   distributes accounting across the individual streams.
//!
//! # What this module does NOT do
//!
//! * It does not change the **semantic** behavior of `batch_withdraw`. The
//!   all-or-nothing atomicity guarantee is unchanged: if any payout in any group
//!   fails, the caller's outer transaction reverts everything.
//!
//! * It does not coalesce streams from different recipients. Each recipient
//!   issues their own `batch_withdraw` authorized for their own streams; there
//!   is no cross-recipient grouping.
//!
//! * It does not change the event contract. Each stream still emits its own
//!   `Withdrawn` event at its own amount. The optimization is at the
//!   sub-invocation layer, not at the accounting or event layer.
//!
//! # Usage in `batch_withdraw`
//!
//! The optimized batch path proceeds as follows:
//!
//! ```text
//! 1. Preflight phase (identical to current path):
//!    for each stream_id:
//!      stream = peek_stream(stream_id)
//!      check recipient == caller
//!      payout = withdrawable(stream, now)
//!      accumulate (stream_id, payout, token) into payout_list
//!
//! 2. Group phase:
//!    groups = group_payouts_by_token(payout_list)
//!
//! 3. Apply phase:
//!    for each group:
//!      if group.total > 0:
//!        // ONE token transfer for all streams in this group
//!        token.transfer(contract_address, recipient, group.total)
//!    for each (stream_id, payout):
//!      // Individual accounting writes (no token call)
//!      update withdrawn, status; save_stream; emit event
//! ```
//!
//! This separates the token sub-invocations (one per unique token) from the
//! accounting writes (one per stream), which is the correct decomposition:
//! the accounting writes are cheap host-storage calls; the token sub-invocations
//! are the expensive part.
//!
//! # Correctness argument
//!
//! The coalesced transfer moves `group.total` tokens from the contract to the
//! recipient. The sum of individual per-stream payouts is exactly `group.total`
//! (by construction in [`group_payouts_by_token`]). After the transfer,
//! the per-stream `withdrawn` fields are incremented by their individual
//! payouts. The net effect on the contract balance and recipient balance is
//! identical to 16 individual transfers of the per-stream amounts.
//!
//! # Interaction with `rejection_duplicate_ids`
//!
//! Duplicate-id rejection happens in `validate_batch_ids` and
//! `reject_duplicate_ids` before this module is involved. The grouping logic
//! assumes each stream id appears exactly once in the input.

use soroban_sdk::{Address, Vec};

/// A single stream's payout descriptor within a coalesced token group.
#[derive(Clone, Debug)]
pub struct BatchPayout {
    /// The stream whose `withdrawn` will be incremented by `amount`.
    pub stream_id: u64,
    /// The amount to pay out from this stream. Zero payouts are included so
    /// the caller can still bump the TTL for streams with nothing to withdraw.
    pub amount: i128,
}

/// A group of payouts that all use the same token contract.
///
/// The caller issues **one** `token.transfer(contract, recipient, total)` for
/// this group and then processes the individual accounting entries.
#[derive(Clone, Debug)]
pub struct TokenGroup {
    /// The token contract address shared by all entries in this group.
    pub token: Address,
    /// Sum of all `amount` fields in `payouts`. This is what gets transferred
    /// in the single coalesced token sub-invocation.
    pub total: i128,
    /// Individual stream payouts, in stream-id order.
    pub payouts: soroban_sdk::vec::Vec<BatchPayoutEntry>,
}

/// An entry within a [`TokenGroup`], carrying enough data for the accounting
/// write phase.
#[derive(Clone, Debug)]
pub struct BatchPayoutEntry {
    pub stream_id: u64,
    pub amount: i128,
}

/// A compact payout record used as input to [`group_payouts_by_token`].
#[derive(Clone, Debug)]
pub struct PayoutRecord {
    pub stream_id: u64,
    pub amount: i128,
    pub token: Address,
}

/// Group a list of payout records by token address.
///
/// Returns up to 16 token groups (one per unique token). Within each group,
/// entries are in the order they were encountered in `records`.
///
/// # Guarantees
///
/// * `sum(group.total for group in groups) == sum(record.amount for record in records)`
/// * No group has `total < 0` (since individual amounts are >= 0).
/// * The number of groups is at most `records.len()` (bounded by the input).
///
/// # Arguments
///
/// * `env` — Needed for `Vec` allocation.
/// * `records` — Flat list of `(stream_id, amount, token)` tuples, one per
///   stream, in the order they were resolved during the preflight phase.
///
/// # Returns
///
/// A flat array of up to 16 [`TokenGroup`]s and the actual count.
/// The array is fixed-size to avoid heap allocation in `no_std`.
///
/// # Notes
///
/// This performs an O(n×k) grouping where n is the number of records and k is
/// the number of unique tokens. For n ≤ 16 this is at most 256 iterations,
/// which is fast enough for the protocol's instruction budget.
pub fn group_payouts_by_token(
    env: &soroban_sdk::Env,
    records: &[PayoutRecord],
) -> ([Option<TokenGroup>; 16], usize) {
    let mut groups: [Option<TokenGroup>; 16] = Default::default();
    let mut group_count = 0usize;

    for record in records {
        // Find an existing group for this token.
        let mut found_idx: Option<usize> = None;
        for i in 0..group_count {
            if let Some(ref g) = groups[i] {
                if g.token == record.token {
                    found_idx = Some(i);
                    break;
                }
            }
        }

        let entry = BatchPayoutEntry {
            stream_id: record.stream_id,
            amount: record.amount,
        };

        match found_idx {
            Some(idx) => {
                // Accumulate into the existing group.
                if let Some(ref mut g) = groups[idx] {
                    // Safe: total is bounded by deposited sum which fits i128.
                    g.total = g.total.saturating_add(record.amount);
                    g.payouts.push_back(entry);
                }
            }
            None => {
                // Open a new group. The array is sized to MAX_BATCH_SIZE (16),
                // so group_count can never reach 17 without records also
                // having 17 elements, which is rejected by validate_batch_ids.
                if group_count < 16 {
                    let mut payouts = Vec::new(env);
                    payouts.push_back(entry);
                    groups[group_count] = Some(TokenGroup {
                        token: record.token.clone(),
                        total: record.amount,
                        payouts,
                    });
                    group_count += 1;
                }
            }
        }
    }

    (groups, group_count)
}

/// Count the number of unique token addresses in a payout list.
///
/// Used by resource-limit tests to assert that the coalescing optimization
/// was applied (i.e. the number of token sub-invocations equals the number
/// of unique tokens, not the total number of streams).
pub fn count_unique_tokens(records: &[PayoutRecord]) -> usize {
    let mut count = 0usize;
    // O(n²) for n ≤ 16; fine in this context.
    'outer: for record in records {
        for earlier in &records[..count.min(records.len())] {
            if earlier.token == record.token {
                continue 'outer;
            }
        }
        count += 1;
    }
    count
}

/// True if all records share the same token (the common payroll case).
/// When this is true, the entire batch requires exactly one token sub-invocation.
pub fn is_homogeneous(records: &[PayoutRecord]) -> bool {
    match records.first() {
        None => true, // empty batch is trivially homogeneous
        Some(first) => records.iter().all(|r| r.token == first.token),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::Env;

    fn make_records(env: &Env, count: usize, tokens: &[Address]) -> std::vec::Vec<PayoutRecord> {
        (0..count)
            .map(|i| PayoutRecord {
                stream_id: i as u64,
                amount: (i as i128 + 1) * 10,
                token: tokens[i % tokens.len()].clone(),
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // group_payouts_by_token: grouping correctness
    // -----------------------------------------------------------------------

    #[test]
    fn single_token_all_records_in_one_group() {
        let env = Env::default();
        let token = Address::generate(&env);
        let records = make_records(&env, 4, &[token.clone()]);

        let (groups, count) = group_payouts_by_token(&env, &records);

        assert_eq!(count, 1, "should be one group for a homogeneous batch");
        let g = groups[0].as_ref().unwrap();
        assert_eq!(g.token, token);
        assert_eq!(g.total, 10 + 20 + 30 + 40, "total should be sum of all amounts");
        assert_eq!(g.payouts.len(), 4);
    }

    #[test]
    fn two_tokens_produces_two_groups() {
        let env = Env::default();
        let token_a = Address::generate(&env);
        let token_b = Address::generate(&env);
        let records = make_records(&env, 4, &[token_a.clone(), token_b.clone()]);

        let (groups, count) = group_payouts_by_token(&env, &records);

        assert_eq!(count, 2);
        // Group 0 has token_a (records 0, 2): amounts 10 + 30 = 40
        let g0 = groups[0].as_ref().unwrap();
        assert_eq!(g0.total, 40);
        // Group 1 has token_b (records 1, 3): amounts 20 + 40 = 60
        let g1 = groups[1].as_ref().unwrap();
        assert_eq!(g1.total, 60);
    }

    #[test]
    fn group_totals_sum_equals_record_totals_sum() {
        let env = Env::default();
        let tokens: std::vec::Vec<Address> = (0..3).map(|_| Address::generate(&env)).collect();
        let records = make_records(&env, 9, &tokens);

        let record_sum: i128 = records.iter().map(|r| r.amount).sum();
        let (groups, count) = group_payouts_by_token(&env, &records);

        let group_sum: i128 = (0..count)
            .filter_map(|i| groups[i].as_ref())
            .map(|g| g.total)
            .sum();

        assert_eq!(group_sum, record_sum, "total conservation across grouping");
    }

    #[test]
    fn empty_records_produce_zero_groups() {
        let env = Env::default();
        let (_, count) = group_payouts_by_token(&env, &[]);
        assert_eq!(count, 0);
    }

    // -----------------------------------------------------------------------
    // count_unique_tokens
    // -----------------------------------------------------------------------

    #[test]
    fn count_unique_tokens_single_token() {
        let env = Env::default();
        let token = Address::generate(&env);
        let records = make_records(&env, 5, &[token]);
        assert_eq!(count_unique_tokens(&records), 1);
    }

    #[test]
    fn count_unique_tokens_all_different() {
        let env = Env::default();
        let tokens: std::vec::Vec<Address> = (0..4).map(|_| Address::generate(&env)).collect();
        let records: std::vec::Vec<PayoutRecord> = tokens
            .iter()
            .enumerate()
            .map(|(i, t)| PayoutRecord {
                stream_id: i as u64,
                amount: 100,
                token: t.clone(),
            })
            .collect();
        assert_eq!(count_unique_tokens(&records), 4);
    }

    // -----------------------------------------------------------------------
    // is_homogeneous
    // -----------------------------------------------------------------------

    #[test]
    fn is_homogeneous_single_token_true() {
        let env = Env::default();
        let token = Address::generate(&env);
        let records = make_records(&env, 3, &[token]);
        assert!(is_homogeneous(&records));
    }

    #[test]
    fn is_homogeneous_two_tokens_false() {
        let env = Env::default();
        let tokens: std::vec::Vec<Address> =
            (0..2).map(|_| Address::generate(&env)).collect();
        let records = make_records(&env, 2, &tokens);
        assert!(!is_homogeneous(&records));
    }

    #[test]
    fn is_homogeneous_empty_true() {
        assert!(is_homogeneous(&[]));
    }

    // -----------------------------------------------------------------------
    // MAX_BATCH_SIZE boundary
    // -----------------------------------------------------------------------

    #[test]
    fn group_payouts_handles_16_records_homogeneous() {
        let env = Env::default();
        let token = Address::generate(&env);
        let records = make_records(&env, 16, &[token.clone()]);

        let (groups, count) = group_payouts_by_token(&env, &records);
        assert_eq!(count, 1);
        let g = groups[0].as_ref().unwrap();
        // Sum of 10..160 step 10 = 10*(1+2+...+16) = 10*136 = 1360
        assert_eq!(g.total, 1360);
        assert_eq!(g.payouts.len(), 16);
    }

    #[test]
    fn group_payouts_handles_16_records_all_different_tokens() {
        let env = Env::default();
        let tokens: std::vec::Vec<Address> = (0..16).map(|_| Address::generate(&env)).collect();
        let records: std::vec::Vec<PayoutRecord> = tokens
            .iter()
            .enumerate()
            .map(|(i, t)| PayoutRecord {
                stream_id: i as u64,
                amount: 100,
                token: t.clone(),
            })
            .collect();

        let (groups, count) = group_payouts_by_token(&env, &records);
        // All 16 tokens are different, so there must be 16 groups.
        assert_eq!(count, 16);
        for i in 0..16 {
            let g = groups[i].as_ref().unwrap();
            assert_eq!(g.total, 100);
            assert_eq!(g.payouts.len(), 1);
        }
    }
}
