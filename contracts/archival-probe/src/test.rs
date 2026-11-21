//! Shared test harness, unit/fuzz tests, and fractional withdrawal verification for archival probe.

use super::*;
use soroban_sdk::symbol_short;
use soroban_sdk::testutils::storage::Persistent as _;
use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::Address;

pub struct TestHarness<'a> {
    pub env: Env,
    pub contract_id: Address,
    pub client: ArchivalProbeClient<'a>,
    pub admin: Address,
    pub config_admin: Address,
    pub emergency_guardian: Address,
    pub operator: Address,
    pub stranger: Address,
}

impl<'a> TestHarness<'a> {
    pub fn new() -> Self {
        let env = Env::default();
        let contract_id = env.register(ArchivalProbe, ());
        let client = ArchivalProbeClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let config_admin = Address::generate(&env);
        let emergency_guardian = Address::generate(&env);
        let operator = Address::generate(&env);
        let stranger = Address::generate(&env);

        client.initialize(&admin);

        TestHarness {
            env,
            contract_id,
            client,
            admin,
            config_admin,
            emergency_guardian,
            operator,
            stranger,
        }
    }

    pub fn new_uninitialized() -> Self {
        let env = Env::default();
        let contract_id = env.register(ArchivalProbe, ());
        let client = ArchivalProbeClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let config_admin = Address::generate(&env);
        let emergency_guardian = Address::generate(&env);
        let operator = Address::generate(&env);
        let stranger = Address::generate(&env);

        TestHarness {
            env,
            contract_id,
            client,
            admin,
            config_admin,
            emergency_guardian,
            operator,
            stranger,
        }
    }

    pub fn advance_ledgers(&self, ledgers: u32) {
        let current = self.env.ledger().sequence();
        self.env.ledger().set_sequence_number(current + ledgers);
    }
}

// ---------------------------------------------------------------------------
// Granular Role-Based Authorization Tests (#376)
// ---------------------------------------------------------------------------

#[test]
fn test_role_initialization_and_admin_supremacy() {
    let h = TestHarness::new();

    // Admin has Admin role inherently and has_role reflects true
    assert!(h.client.has_role(&Role::Admin, &h.admin));
    assert!(h.client.has_role(&Role::ConfigAdmin, &h.admin));
    assert!(h.client.has_role(&Role::EmergencyGuardian, &h.admin));
    assert!(h.client.has_role(&Role::Operator, &h.admin));

    // Stranger has no roles
    assert!(!h.client.has_role(&Role::Admin, &h.stranger));
    assert!(!h.client.has_role(&Role::ConfigAdmin, &h.stranger));
    assert!(!h.client.has_role(&Role::EmergencyGuardian, &h.stranger));
    assert!(!h.client.has_role(&Role::Operator, &h.stranger));

    let config = h.client.get_config();
    assert_eq!(config.admin, h.admin);
    assert_eq!(config.max_batch_size, DEFAULT_MAX_BATCH_SIZE);
    assert!(!config.paused);
}

#[test]
fn test_role_grant_and_revocation() {
    let h = TestHarness::new();

    // Admin assigns ConfigAdmin to config_admin
    h.client
        .set_role(&h.admin, &Role::ConfigAdmin, &h.config_admin, &true);
    assert!(h.client.has_role(&Role::ConfigAdmin, &h.config_admin));
    // config_admin does NOT have EmergencyGuardian
    assert!(!h.client.has_role(&Role::EmergencyGuardian, &h.config_admin));

    // Stranger attempts to grant role -> UnauthorizedRole
    let res = h
        .client
        .try_set_role(&h.stranger, &Role::Operator, &h.operator, &true);
    assert_eq!(res.unwrap_err().unwrap(), Error::UnauthorizedRole);

    // Revoke ConfigAdmin
    h.client
        .set_role(&h.admin, &Role::ConfigAdmin, &h.config_admin, &false);
    assert!(!h.client.has_role(&Role::ConfigAdmin, &h.config_admin));
}

#[test]
fn test_invalid_role_address_rejection() {
    let h = TestHarness::new();
    let zero = Address::from_string(&soroban_sdk::String::from_str(
        &h.env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    ));

    // Zero address rejected for initialize
    let h_uninit = TestHarness::new_uninitialized();
    let res_init = h_uninit.client.try_initialize(&zero);
    assert_eq!(res_init.unwrap_err().unwrap(), Error::InvalidRoleAddress);

    // Contract self address rejected for initialize
    let res_self = h_uninit.client.try_initialize(&h_uninit.contract_id);
    assert_eq!(res_self.unwrap_err().unwrap(), Error::InvalidRoleAddress);

    // Zero address rejected for set_role
    let res_role = h
        .client
        .try_set_role(&h.admin, &Role::Operator, &zero, &true);
    assert_eq!(res_role.unwrap_err().unwrap(), Error::InvalidRoleAddress);

    // Cannot revoke Admin role from active admin
    let res_revoke = h
        .client
        .try_set_role(&h.admin, &Role::Admin, &h.admin, &false);
    assert_eq!(res_revoke.unwrap_err().unwrap(), Error::InvalidRoleAddress);
}

#[test]
fn test_config_admin_scoped_operations() {
    let h = TestHarness::new();
    h.client
        .set_role(&h.admin, &Role::ConfigAdmin, &h.config_admin, &true);

    // ConfigAdmin can update max_batch_size
    h.client.set_max_batch_size(&h.config_admin, &16);
    assert_eq!(h.client.get_config().max_batch_size, 16);

    // ConfigAdmin cannot update paused state (EmergencyGuardian only)
    let res_pause = h.client.try_set_paused(&h.config_admin, &true);
    assert_eq!(res_pause.unwrap_err().unwrap(), Error::UnauthorizedRole);

    // Stranger cannot update max_batch_size
    let res_stranger = h.client.try_set_max_batch_size(&h.stranger, &20);
    assert_eq!(res_stranger.unwrap_err().unwrap(), Error::UnauthorizedRole);

    // Batch size exceeding MAX_BATCH_PROBE_SIZE rejected
    let res_too_large = h
        .client
        .try_set_max_batch_size(&h.config_admin, &(MAX_BATCH_PROBE_SIZE + 1));
    assert_eq!(res_too_large.unwrap_err().unwrap(), Error::BatchTooLarge);
}

#[test]
fn test_emergency_guardian_pause_unpause() {
    let h = TestHarness::new();
    h.client.set_role(
        &h.admin,
        &Role::EmergencyGuardian,
        &h.emergency_guardian,
        &true,
    );

    // EmergencyGuardian can pause
    h.client.set_paused(&h.emergency_guardian, &true);
    assert!(h.client.get_config().paused);

    // When paused, batch_plant fails with ContractPaused
    let mut ids = Vec::new(&h.env);
    let mut notes = Vec::new(&h.env);
    ids.push_back(1);
    notes.push_back(symbol_short!("p1"));
    let res_plant = h.client.try_batch_plant(&ids, &notes);
    assert_eq!(res_plant.unwrap_err().unwrap(), Error::ContractPaused);

    // EmergencyGuardian can unpause
    h.client.set_paused(&h.emergency_guardian, &false);
    assert!(!h.client.get_config().paused);

    // After unpause, batch_plant succeeds
    let count = h.client.batch_plant(&ids, &notes);
    assert_eq!(count, 1);
}

#[test]
fn test_two_step_admin_rotation() {
    let h = TestHarness::new();
    let new_admin = Address::generate(&h.env);

    // Propose admin
    h.client.propose_admin(&h.admin, &new_admin);

    // Stranger cannot accept
    let res_stranger = h.client.try_accept_admin(&h.stranger);
    assert_eq!(res_stranger.unwrap_err().unwrap(), Error::UnauthorizedRole);

    // Nominee accepts
    h.client.accept_admin(&new_admin);

    // Verify new admin is active
    let config = h.client.get_config();
    assert_eq!(config.admin, new_admin);
    assert!(h.client.has_role(&Role::Admin, &new_admin));
    assert!(!h.client.has_role(&Role::Admin, &h.admin));
}

// ---------------------------------------------------------------------------
// Streamlined Cross-Contract Invocation Overhead Tests (#382)
// ---------------------------------------------------------------------------

#[test]
fn test_streamlined_batch_claim_probe_overhead_reduction() {
    let h = TestHarness::new();
    let claimant = Address::generate(&h.env);

    let mut stream_ids = Vec::new(&h.env);
    let mut amounts = Vec::new(&h.env);

    for i in 1..=10u64 {
        stream_ids.push_back(i);
        amounts.push_back(i as i128 * 1_000);
    }

    // Consolidated single cross-contract invocation
    let recorded = h.client.batch_claim_probe(&claimant, &stream_ids, &amounts);
    assert_eq!(recorded, 10);

    // Verify single-pass batch read of claim probes
    let probes = h.client.batch_get_claim_probes(&stream_ids);
    assert_eq!(probes.len(), 10);

    for i in 0..10usize {
        let probe_opt = probes.get(i as u32).unwrap();
        assert!(probe_opt.is_some());
        let probe = probe_opt.unwrap();
        assert_eq!(probe.stream_id, (i + 1) as u64);
        assert_eq!(probe.amount, (i + 1) as i128 * 1_000);
        assert_eq!(probe.claimant, claimant);
        assert_eq!(probe.verified_at, h.env.ledger().sequence());
    }

    // Replay / retry idempotency (first-write-wins without error or TTL reset)
    h.advance_ledgers(50);
    let mut retry_amounts = Vec::new(&h.env);
    for i in 1..=10u64 {
        retry_amounts.push_back(i as i128 * 9_999);
    }
    let retried = h
        .client
        .batch_claim_probe(&claimant, &stream_ids, &retry_amounts);
    assert_eq!(retried, 0); // All 10 already recorded

    // Check first-write-wins preserved original amounts and sequence
    let probes_after = h.client.batch_get_claim_probes(&stream_ids);
    for i in 0..10usize {
        let probe = probes_after.get(i as u32).unwrap().unwrap();
        assert_eq!(probe.amount, (i + 1) as i128 * 1_000);
        assert_eq!(probe.verified_at, 0);
    }
}

#[test]
fn test_batch_claim_probe_defensive_bounds() {
    let h = TestHarness::new();
    let claimant = Address::generate(&h.env);

    // 1. Empty batch
    let empty_ids: Vec<u64> = Vec::new(&h.env);
    let empty_amounts: Vec<i128> = Vec::new(&h.env);
    assert_eq!(
        h.client
            .try_batch_claim_probe(&claimant, &empty_ids, &empty_amounts)
            .unwrap_err()
            .unwrap(),
        Error::EmptyBatch
    );
    assert_eq!(
        h.client
            .try_batch_get_claim_probes(&empty_ids)
            .unwrap_err()
            .unwrap(),
        Error::EmptyBatch
    );

    // 2. Length mismatch
    let mut ids = Vec::new(&h.env);
    ids.push_back(1u64);
    ids.push_back(2u64);
    let mut amounts = Vec::new(&h.env);
    amounts.push_back(100i128);
    assert_eq!(
        h.client
            .try_batch_claim_probe(&claimant, &ids, &amounts)
            .unwrap_err()
            .unwrap(),
        Error::LengthMismatch
    );

    // 3. Oversized batch beyond dynamic limit
    h.client
        .set_role(&h.admin, &Role::ConfigAdmin, &h.config_admin, &true);
    h.client.set_max_batch_size(&h.config_admin, &5);

    let mut over_ids = Vec::new(&h.env);
    let mut over_amounts = Vec::new(&h.env);
    for i in 0..6u64 {
        over_ids.push_back(i);
        over_amounts.push_back(100);
    }
    assert_eq!(
        h.client
            .try_batch_claim_probe(&claimant, &over_ids, &over_amounts)
            .unwrap_err()
            .unwrap(),
        Error::BatchTooLarge
    );
    assert_eq!(
        h.client
            .try_batch_get_claim_probes(&over_ids)
            .unwrap_err()
            .unwrap(),
        Error::BatchTooLarge
    );
}

// ---------------------------------------------------------------------------
// Existing Canary & Probe Invariants
// ---------------------------------------------------------------------------

#[test]
fn plant_and_read_round_trip() {
    let h = TestHarness::new();

    assert!(!h.client.planted());
    assert_eq!(h.client.try_read().unwrap_err().unwrap(), Error::NotPlanted);

    h.client.plant(&symbol_short!("canary"));
    assert!(h.client.planted());
    assert_eq!(h.client.read(), symbol_short!("canary"));
}

#[test]
fn retry_does_not_replace_note_or_refresh_ttl() {
    let h = TestHarness::new();

    h.client.plant(&symbol_short!("first"));
    let planted_at = h.env.ledger().sequence();
    h.env.ledger().set_sequence_number(planted_at + 100);
    let ttl_before_retry = h.env.as_contract(&h.contract_id, || {
        h.env.storage().persistent().get_ttl(&Key::Canary)
    });

    h.client.plant(&symbol_short!("retry"));

    let ttl_after_retry = h.env.as_contract(&h.contract_id, || {
        h.env.storage().persistent().get_ttl(&Key::Canary)
    });
    assert_eq!(ttl_after_retry, ttl_before_retry);
    assert_eq!(h.client.read(), symbol_short!("first"));
}

/// The probe must never extend its own TTL — that omission is what makes it
/// archive on the network's schedule instead of the contract's.
#[test]
fn the_probe_does_not_extend_its_own_ttl() {
    let h = TestHarness::new();
    h.client.plant(&symbol_short!("canary"));

    let ttl = h.env.as_contract(&h.contract_id, || {
        h.env.storage().persistent().get_ttl(&Key::Canary)
    });
    let min = h.env.ledger().get().min_persistent_entry_ttl;

    assert!(
        ttl < min,
        "probe TTL {ttl} exceeds the network minimum {min}; something is \
         extending it and the probe will not archive on schedule",
    );
}

/// Batch plant and batch read round trip
#[test]
fn batch_plant_and_read_round_trip_eliminates_invocation_overhead() {
    let h = TestHarness::new();

    let mut ids = Vec::new(&h.env);
    let mut notes = Vec::new(&h.env);

    for i in 0..16u32 {
        ids.push_back(i);
        notes.push_back(symbol_short!("p_item"));
    }

    let planted = h.client.batch_plant(&ids, &notes);
    assert_eq!(planted, 16);

    let records = h.client.batch_read(&ids);
    assert_eq!(records.len(), 16);

    for i in 0..16u32 {
        let rec = records.get(i).unwrap();
        assert_eq!(rec.note, symbol_short!("p_item"));
        assert_eq!(rec.planted_at, h.env.ledger().sequence());
    }
}

/// High-frequency concurrent batch claim replay preservation (first-write-wins)
#[test]
fn high_frequency_batch_claim_idempotency_and_no_ttl_reset() {
    let h = TestHarness::new();

    let mut ids = Vec::new(&h.env);
    let mut notes = Vec::new(&h.env);
    ids.push_back(101);
    notes.push_back(symbol_short!("original"));

    let planted = h.client.batch_plant(&ids, &notes);
    assert_eq!(planted, 1);

    // Advance ledger sequence to simulate later retry during high-frequency claim cycle
    h.advance_ledgers(40);

    let mut retry_notes = Vec::new(&h.env);
    retry_notes.push_back(symbol_short!("overwrte"));
    let retried = h.client.batch_plant(&ids, &retry_notes);
    assert_eq!(retried, 0); // Already planted, count = 0

    let rec = h.client.read_probe(&101);
    assert_eq!(rec.note, symbol_short!("original"));
    assert_eq!(rec.planted_at, 0); // Preserved original planted sequence
}

/// Bounded batch size enforcement (MAX_BATCH_PROBE_SIZE = 32)
#[test]
fn batch_size_defensive_bounds_and_error_handling() {
    let h = TestHarness::new();

    // 1. Empty batch
    let empty_ids = Vec::new(&h.env);
    let empty_notes = Vec::new(&h.env);
    assert_eq!(
        h.client
            .try_batch_plant(&empty_ids, &empty_notes)
            .unwrap_err()
            .unwrap(),
        Error::EmptyBatch
    );
    assert_eq!(
        h.client.try_batch_read(&empty_ids).unwrap_err().unwrap(),
        Error::EmptyBatch
    );

    // 2. Length mismatch
    let mut ids = Vec::new(&h.env);
    ids.push_back(1);
    ids.push_back(2);
    let mut notes = Vec::new(&h.env);
    notes.push_back(symbol_short!("note1"));
    assert_eq!(
        h.client.try_batch_plant(&ids, &notes).unwrap_err().unwrap(),
        Error::BatchTooLarge
    );

    // 3. Exactly MAX_BATCH_PROBE_SIZE (32) succeeds
    let mut max_ids = Vec::new(&h.env);
    let mut max_notes = Vec::new(&h.env);
    for i in 0..MAX_BATCH_PROBE_SIZE {
        max_ids.push_back(i);
        max_notes.push_back(symbol_short!("probe"));
    }
    let count = h.client.batch_plant(&max_ids, &max_notes);
    assert_eq!(count, MAX_BATCH_PROBE_SIZE);

    let read_records = h.client.batch_read(&max_ids);
    assert_eq!(read_records.len(), MAX_BATCH_PROBE_SIZE);

    // 4. Over MAX_BATCH_PROBE_SIZE (33) fails with BatchTooLarge
    let mut over_ids = Vec::new(&h.env);
    let mut over_notes = Vec::new(&h.env);
    for i in 0..=MAX_BATCH_PROBE_SIZE {
        over_ids.push_back(i);
        over_notes.push_back(symbol_short!("probe"));
    }
    assert_eq!(
        h.client
            .try_batch_plant(&over_ids, &over_notes)
            .unwrap_err()
            .unwrap(),
        Error::BatchTooLarge
    );
    assert_eq!(
        h.client.try_batch_read(&over_ids).unwrap_err().unwrap(),
        Error::BatchTooLarge
    );
}

/// Verification of fractional withdrawal calculations and precision preservation
#[test]
fn fractional_withdrawal_precision_preservation_and_exact_conservation() {
    let h = TestHarness::new();

    // Case 1: Simple 50% split of 7 stroops (the classic odd-number truncation edge-case)
    let split = h.client.calculate_fractional_split(&7, &50, &100);
    assert_eq!(split.payout, 3);
    assert_eq!(split.remainder, 4);
    assert_eq!(split.total, 7);
    assert_eq!(split.payout + split.remainder, 7);
    assert!(invariants::assert_fractional_conservation(&split));

    // Case 2: One-third of 100 stroops (33 payout, 67 remainder -> sum == 100)
    let split_third = h.client.calculate_fractional_split(&100, &1, &3);
    assert_eq!(split_third.payout, 33);
    assert_eq!(split_third.remainder, 67);
    assert_eq!(split_third.total, 100);
    assert_eq!(split_third.payout + split_third.remainder, 100);

    // Case 3: Zero total or zero numerator
    let zero_split = h.client.calculate_fractional_split(&0, &50, &100);
    assert_eq!(zero_split.payout, 0);
    assert_eq!(zero_split.remainder, 0);

    let zero_num = h.client.calculate_fractional_split(&1000, &0, &100);
    assert_eq!(zero_num.payout, 0);
    assert_eq!(zero_num.remainder, 1000);

    // Case 4: 100% split (numerator == denominator)
    let full = h.client.calculate_fractional_split(&54321, &100, &100);
    assert_eq!(full.payout, 54321);
    assert_eq!(full.remainder, 0);

    // Case 5: Basis points (bps) split: 250 bps = 2.5% of 10,000
    let bps_split = h.client.calculate_bps_split(&10_000, &250);
    assert_eq!(bps_split.payout, 250);
    assert_eq!(bps_split.remainder, 9_750);
}

/// Boundary tests for fractional calculations near i128::MAX without intermediate overflow
#[test]
fn fractional_withdrawal_large_balance_no_overflow() {
    let h = TestHarness::new();

    let large_total = i128::MAX / 2;
    // 50% of (i128::MAX / 2)
    let split = h.client.calculate_fractional_split(&large_total, &1, &2);
    assert_eq!(split.payout, large_total / 2);
    assert_eq!(split.remainder, large_total - (large_total / 2));
    assert_eq!(split.payout + split.remainder, large_total);

    // Total near i128::MAX with high numerator (naive total * num would overflow i128)
    let high_total = i128::MAX - 100;
    let split_high = h
        .client
        .calculate_fractional_split(&high_total, &9_999, &10_000);
    assert!(split_high.payout > 0);
    assert_eq!(split_high.payout + split_high.remainder, high_total);
    assert!(invariants::assert_fractional_conservation(&split_high));
}

/// Negative validation tests for invalid fraction parameters
#[test]
fn fractional_withdrawal_defensive_validation() {
    let h = TestHarness::new();

    // 1. Zero denominator
    assert_eq!(
        h.client
            .try_calculate_fractional_split(&100, &1, &0)
            .unwrap_err()
            .unwrap(),
        Error::ZeroDenominator
    );

    // 2. Negative denominator
    assert_eq!(
        h.client
            .try_calculate_fractional_split(&100, &1, &-5)
            .unwrap_err()
            .unwrap(),
        Error::ZeroDenominator
    );

    // 3. Numerator > denominator
    assert_eq!(
        h.client
            .try_calculate_fractional_split(&100, &101, &100)
            .unwrap_err()
            .unwrap(),
        Error::InvalidFraction
    );

    // 4. Negative numerator
    assert_eq!(
        h.client
            .try_calculate_fractional_split(&100, &-1, &100)
            .unwrap_err()
            .unwrap(),
        Error::InvalidFraction
    );

    // 5. Negative total
    assert_eq!(
        h.client
            .try_calculate_fractional_split(&-100, &50, &100)
            .unwrap_err()
            .unwrap(),
        Error::ArithmeticOverflow
    );

    // 6. Bps > 10_000
    assert_eq!(
        h.client
            .try_calculate_bps_split(&1000, &10_001)
            .unwrap_err()
            .unwrap(),
        Error::InvalidFraction
    );
}

/// Equal shares batch split verification
#[test]
fn test_batch_split_equal_shares_conservation() {
    let total = 100i128;
    let (shares, count) = math::batch_split_equal_shares(total, 3).unwrap();
    assert_eq!(count, 3);
    // 100 / 3 = 33 r 1 -> shares should be [34, 33, 33]
    assert_eq!(shares[0], 34);
    assert_eq!(shares[1], 33);
    assert_eq!(shares[2], 33);
    assert_eq!(shares[0] + shares[1] + shares[2], total);

    // Test with prime total and 7 shares
    let prime_total = 73i128;
    let (shares7, count7) = math::batch_split_equal_shares(prime_total, 7).unwrap();
    assert_eq!(count7, 7);
    let mut sum = 0i128;
    for &s in &shares7[..7] {
        sum += s;
    }
    assert_eq!(sum, prime_total);
}

/// Formal invariant assertions and probe integrity verification
#[test]
fn formal_invariants_and_integrity_assertions() {
    let h = TestHarness::new();

    assert!(invariants::verify_batch_bounds(1));
    assert!(invariants::verify_batch_bounds(MAX_BATCH_PROBE_SIZE));
    assert!(!invariants::verify_batch_bounds(0));
    assert!(!invariants::verify_batch_bounds(MAX_BATCH_PROBE_SIZE + 1));

    assert!(invariants::is_valid_role(Role::Admin));
    assert!(invariants::is_valid_role(Role::ConfigAdmin));
    assert!(invariants::is_valid_role(Role::EmergencyGuardian));
    assert!(invariants::is_valid_role(Role::Operator));

    h.client.plant_probe(&999, &symbol_short!("probe999"));
    let rec = h.client.read_probe(&999);
    assert!(invariants::assert_record_integrity(
        &rec,
        &symbol_short!("probe999")
    ));
    assert!(!invariants::assert_record_integrity(
        &rec,
        &symbol_short!("mismatch")
    ));

    let claimant = Address::generate(&h.env);
    let mut ids = Vec::new(&h.env);
    let mut amounts = Vec::new(&h.env);
    ids.push_back(42);
    amounts.push_back(12345);
    h.client.batch_claim_probe(&claimant, &ids, &amounts);

    let claim_probes = h.client.batch_get_claim_probes(&ids);
    let claim_probe = claim_probes.get(0).unwrap().unwrap();
    assert!(invariants::assert_claim_probe_integrity(
        &claim_probe,
        42,
        12345,
        &claimant
    ));
}

#[cfg(test)]
mod fuzz {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn fuzz_fractional_split_conservation(
            total in 0i128..=1_000_000_000_000_000_000i128,
            num in 0i128..=10_000i128,
            den in 1i128..=10_000i128,
        ) {
            let n = num.min(den);
            let split = math::calculate_fractional_withdrawal(total, n, den).unwrap();
            prop_assert_eq!(split.payout + split.remainder, total);
            prop_assert!(split.payout >= 0);
            prop_assert!(split.remainder >= 0);
            prop_assert!(split.payout <= total);
            prop_assert!(split.remainder <= total);
            prop_assert!(split.assert_conservation());
        }

        #[test]
        fn fuzz_batch_split_equal_shares(
            total in 0i128..=10_000_000_000i128,
            n in 1u32..=16u32,
        ) {
            let (shares, count) = math::batch_split_equal_shares(total, n).unwrap();
            prop_assert_eq!(count, n);
            let mut sum: i128 = 0;
            for &share in &shares[..n as usize] {
                prop_assert!(share >= 0);
                sum += share;
            }
            prop_assert_eq!(sum, total);
        }
    }
}
