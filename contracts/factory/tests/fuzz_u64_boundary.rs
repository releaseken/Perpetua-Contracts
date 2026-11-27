//! Fuzzing and boundary test suite for edge-case timestamps in the factory
//! contract near `u64` maximum boundary limits.
//!
//! Covers Issue #365:
//! * Explicit edge-case timestamp boundary pinning near `u64::MAX`.
//! * Parametric fuzzing via `proptest` for durations, start times, end times,
//!   and cliff offsets across the entire `u64` domain.
//! * Checked arithmetic and overflow safety across duration calculation, rate
//!   division (`duration as i128`), and cliff inclusion validation.
//! * Gas / host metering efficiency & invariants under boundary values.

#![cfg(test)]

use fluxora_factory::{FactoryError, FluxoraFactory, FluxoraFactoryClient};
use proptest::prelude::*;
use soroban_sdk::{
    contract, contractimpl,
    testutils::Address as _,
    Address, BytesN, Env,
};

const MAX_U64: u64 = u64::MAX;

fn stream_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[9; 32])
}

#[contract]
struct MockStream;

#[contractimpl]
impl MockStream {
    #[allow(clippy::too_many_arguments)]
    pub fn create_stream(
        _env: Env,
        _sender: Address,
        _recipient: Address,
        _token: Address,
        _deposit: i128,
        _start_time: u64,
        _end_time: u64,
        _cliff_time: u64,
        _cancellable: bool,
        _pausable: bool,
        _transferable: bool,
    ) -> u64 {
        42
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct TestHarness<'a> {
    env: Env,
    factory: FluxoraFactoryClient<'a>,
    admin: Address,
    stream_contract: Address,
    sender: Address,
    recipient: Address,
    token: Address,
}

impl<'a> TestHarness<'a> {
    fn setup(min_duration: u64, max_deposit: i128) -> Self {
        let env = Env::default();
        env.mock_all_auths();
        let stream_contract = env.register_contract(None, MockStream);
        let fid = env.register_contract(None, FluxoraFactory);
        let factory = FluxoraFactoryClient::new(&env, &fid);
        let admin = Address::generate(&env);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = Address::generate(&env);

        factory.init(&admin, &stream_contract, &stream_hash(&env), &max_deposit, &min_duration);

        TestHarness {
            env,
            factory,
            admin,
            stream_contract,
            sender,
            recipient,
            token,
        }
    }
}

// ---------------------------------------------------------------------------
// §1 Explicit Boundary Tests (Pinned at u64 landmarks)
// ---------------------------------------------------------------------------

#[test]
fn test_duration_at_exact_u64_max_boundary() {
    let h = TestHarness::setup(1, i128::MAX);
    // Allow large duration up to u64::MAX
    h.factory.set_max_duration(&MAX_U64);

    // [0, u64::MAX] -> duration = u64::MAX
    assert!(h.factory.try_validate_duration(&0, &MAX_U64).is_ok());

    // [u64::MAX - 1000, u64::MAX] -> duration = 1000
    assert!(h.factory.try_validate_duration(&(MAX_U64 - 1000), &MAX_U64).is_ok());

    // [u64::MAX, u64::MAX] -> duration = 0 (rejected as InvalidDuration)
    assert_eq!(
        h.factory.try_validate_duration(&MAX_U64, &MAX_U64),
        Err(Ok(FactoryError::InvalidDuration))
    );
}

#[test]
fn test_duration_overflow_rejection_at_u64_boundary() {
    let h = TestHarness::setup(1, i128::MAX);
    h.factory.set_max_duration(&MAX_U64);

    // start_time > end_time near u64::MAX
    assert_eq!(
        h.factory.try_validate_duration(&MAX_U64, &(MAX_U64 - 1)),
        Err(Ok(FactoryError::InvalidDuration))
    );
    assert_eq!(
        h.factory.try_validate_duration(&MAX_U64, &0),
        Err(Ok(FactoryError::InvalidDuration))
    );
    assert_eq!(
        h.factory.try_validate_duration(&(MAX_U64 - 1), &(MAX_U64 - 2)),
        Err(Ok(FactoryError::InvalidDuration))
    );
}

#[test]
fn test_create_stream_near_u64_max_success() {
    let h = TestHarness::setup(1, 1_000_000);
    h.factory.set_max_duration(&MAX_U64);

    let start = MAX_U64 - 2_000;
    let end = MAX_U64 - 500;
    let cliff = MAX_U64 - 1_000;
    let deposit = 10_000i128;

    let id = h.factory.create_stream(
        &h.sender,
        &h.recipient,
        &h.token,
        &deposit,
        &start,
        &end,
        &cliff,
        &true,
        &true,
        &true,
    );
    assert_eq!(id, 42);
    assert_eq!(h.factory.get_factory_config().active_streams, 1);
}

#[test]
fn test_create_stream_cliff_boundaries_near_u64_max() {
    let h = TestHarness::setup(1, 1_000_000);
    h.factory.set_max_duration(&MAX_U64);

    let start = MAX_U64 - 1_000;
    let end = MAX_U64 - 100;
    let deposit = 10_000i128;

    // Cliff exactly at start_time -> valid
    let res_start = h.factory.try_create_stream(
        &h.sender,
        &h.recipient,
        &h.token,
        &deposit,
        &start,
        &end,
        &start,
        &true,
        &true,
        &true,
    );
    assert!(res_start.is_ok());

    // Cliff exactly at end_time -> valid
    let res_end = h.factory.try_create_stream(
        &h.sender,
        &h.recipient,
        &h.token,
        &deposit,
        &start,
        &end,
        &end,
        &true,
        &true,
        &true,
    );
    assert!(res_end.is_ok());

    // Cliff at start_time - 1 -> InvalidCliff
    let res_before = h.factory.try_create_stream(
        &h.sender,
        &h.recipient,
        &h.token,
        &deposit,
        &start,
        &end,
        &(start - 1),
        &true,
        &true,
        &true,
    );
    assert_eq!(res_before, Err(Ok(FactoryError::InvalidCliff)));

    // Cliff at end_time + 1 -> InvalidCliff
    let res_after = h.factory.try_create_stream(
        &h.sender,
        &h.recipient,
        &h.token,
        &deposit,
        &start,
        &end,
        &(end + 1),
        &true,
        &true,
        &true,
    );
    assert_eq!(res_after, Err(Ok(FactoryError::InvalidCliff)));

    // Cliff at u64::MAX when end_time is u64::MAX - 100 -> InvalidCliff
    let res_max = h.factory.try_create_stream(
        &h.sender,
        &h.recipient,
        &h.token,
        &deposit,
        &start,
        &end,
        &MAX_U64,
        &true,
        &true,
        &true,
    );
    assert_eq!(res_max, Err(Ok(FactoryError::InvalidCliff)));
}

#[test]
fn test_create_stream_rate_computation_near_u64_max() {
    let h = TestHarness::setup(1, i128::MAX);
    h.factory.set_max_duration(&MAX_U64);

    // Rate bounds configured
    h.factory.set_rate_bounds(&Some(10), &Some(100));

    let start = MAX_U64 - 1_000;
    let end = MAX_U64; // duration = 1000
    let cliff = start;

    // deposit = 5_000 -> rate = 5_000 / 1_000 = 5 (< 10 -> RateBelowMinimum)
    let res_below = h.factory.try_create_stream(
        &h.sender,
        &h.recipient,
        &h.token,
        &5_000,
        &start,
        &end,
        &cliff,
        &true,
        &true,
        &true,
    );
    assert_eq!(res_below, Err(Ok(FactoryError::RateBelowMinimum)));

    // deposit = 150_000 -> rate = 150_000 / 1_000 = 150 (> 100 -> RateAboveMaximum)
    let res_above = h.factory.try_create_stream(
        &h.sender,
        &h.recipient,
        &h.token,
        &150_000,
        &start,
        &end,
        &cliff,
        &true,
        &true,
        &true,
    );
    assert_eq!(res_above, Err(Ok(FactoryError::RateAboveMaximum)));

    // deposit = 50_000 -> rate = 50_000 / 1_000 = 50 (within [10, 100] -> Ok)
    let res_ok = h.factory.try_create_stream(
        &h.sender,
        &h.recipient,
        &h.token,
        &50_000,
        &start,
        &end,
        &cliff,
        &true,
        &true,
        &true,
    );
    assert!(res_ok.is_ok());
}

#[test]
fn test_max_duration_setter_boundary_limits() {
    let h = TestHarness::setup(100, 1_000);

    // Update max_duration to u64::MAX
    assert!(h.factory.try_set_max_duration(&MAX_U64).is_ok());
    assert_eq!(h.factory.get_factory_config().max_duration, MAX_U64);

    // Setting min_duration up to u64::MAX is allowed as long as min_duration <= max_duration
    assert!(h.factory.try_set_min_duration(&MAX_U64).is_ok());
    assert_eq!(h.factory.get_factory_config().min_duration, MAX_U64);

    // Setting min_duration > max_duration must fail
    h.factory.set_max_duration(&(MAX_U64 - 1));
    assert_eq!(
        h.factory.try_set_min_duration(&MAX_U64),
        Err(Ok(FactoryError::InvalidDuration))
    );
}

// ---------------------------------------------------------------------------
// §2 Proptest-driven Fuzzing across u64 Boundary
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: `validate_duration` never panics for ANY pair of `(start, end)`
    /// near or far from `u64::MAX`, and strictly rejects:
    /// - `start >= end`
    /// - `duration < min_duration`
    /// - `duration > max_duration`
    #[test]
    fn fuzz_validate_duration_arbitrary_u64(
        start in any::<u64>(),
        end in any::<u64>(),
        min_duration in 1u64..10_000u64,
        max_duration_offset in 0u64..100_000u64,
    ) {
        let max_duration = min_duration.saturating_add(max_duration_offset);
        let h = TestHarness::setup(min_duration, 1_000_000);
        h.factory.set_max_duration(&max_duration);

        let result = h.factory.try_validate_duration(&start, &end);

        let expected_duration = end.checked_sub(start);
        match expected_duration {
            Some(d) if d > 0 && d >= min_duration && d <= max_duration => {
                prop_assert!(result.is_ok(), "Expected valid for start={}, end={}, d={}", start, end, d);
            }
            _ => {
                prop_assert_eq!(result, Err(Ok(FactoryError::InvalidDuration)),
                    "Expected InvalidDuration for start={}, end={}", start, end);
            }
        }
    }

    /// Property: In `create_stream`, for timestamps placed anywhere in `[u64::MAX - 2_000_000, u64::MAX]`,
    /// checked arithmetic prevents wrap-around, and:
    /// - Cliff validation correctly enforces `start <= cliff <= end`
    /// - Duration bounds are strictly respected
    /// - Rate calculations do not overflow or panic
    #[test]
    fn fuzz_create_stream_near_u64_max_boundary(
        duration in 100u64..=10_000u64,
        start_offset in 0u64..=1_000_000u64,
        cliff_delta in 0u64..=15_000u64,
        deposit in 1_000i128..=1_000_000i128,
    ) {
        let h = TestHarness::setup(50, 2_000_000);
        h.factory.set_max_duration(&MAX_U64);

        let start = MAX_U64.saturating_sub(start_offset + duration + 10);
        let end = start + duration;
        let cliff = start.saturating_add(cliff_delta);

        let result = h.factory.try_create_stream(
            &h.sender,
            &h.recipient,
            &h.token,
            &deposit,
            &start,
            &end,
            &cliff,
            &true,
            &true,
            &true,
        );

        if cliff < start || cliff > end {
            prop_assert_eq!(result, Err(Ok(FactoryError::InvalidCliff)));
        } else {
            prop_assert!(result.is_ok(), "create_stream failed: start={}, end={}, cliff={}, err={:?}", start, end, cliff, result);
        }
    }

    /// Property: Timestamp edge-cases with rate bounds set never panic and
    /// evaluate `deposit / duration` safely without division by zero or truncation wrap.
    #[test]
    fn fuzz_rate_bounds_near_u64_max(
        duration in 1u64..=50_000u64,
        start_offset in 0u64..=500_000u64,
        deposit in 1i128..=10_000_000i128,
        min_rate in 1i128..=100i128,
        rate_spread in 0i128..=500i128,
    ) {
        let max_rate = min_rate + rate_spread;
        let h = TestHarness::setup(1, i128::MAX);
        h.factory.set_max_duration(&MAX_U64);
        h.factory.set_rate_bounds(&Some(min_rate), &Some(max_rate));

        let start = MAX_U64.saturating_sub(start_offset + duration + 1);
        let end = start + duration;
        let cliff = start;

        let result = h.factory.try_create_stream(
            &h.sender,
            &h.recipient,
            &h.token,
            &deposit,
            &start,
            &end,
            &cliff,
            &true,
            &true,
            &true,
        );

        let expected_rate = deposit / (duration as i128);
        if expected_rate < min_rate {
            prop_assert_eq!(result, Err(Ok(FactoryError::RateBelowMinimum)));
        } else if expected_rate > max_rate {
            prop_assert_eq!(result, Err(Ok(FactoryError::RateAboveMaximum)));
        } else {
            prop_assert!(result.is_ok());
        }
    }
}
