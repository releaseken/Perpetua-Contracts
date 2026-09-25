//! Stage 1 — property tests over the accrual math.
//!
//! These drive [`crate::accrual`] directly rather than going through the
//! contract. The functions there are pure, so a case is a few microseconds
//! instead of a host invocation, which buys enough cases to actually explore
//! the space of schedules.
//!
//! # The conservation property
//!
//! The headline invariant is stronger than "dust is bounded":
//!
//! ```text
//! vested(t) + refundable(t) == deposited      for all t
//! ```
//!
//! *Exactly*, with no dust term at all. That falls out of computing `vested`
//! from the cumulative formula `deposited * elapsed / duration` rather than by
//! summing per-interval deltas. Truncation error therefore never accumulates:
//! it is re-derived from scratch on every call and bounded by one stroop at any
//! instant, and it vanishes entirely once the stream settles, because
//! `refundable` is *defined* as the complement of `vested`.
//!
//! A per-interval implementation — the obvious one, and the one the existing
//! MVPs use — loses a stroop per withdrawal and strands it in the pool forever.

use proptest::prelude::*;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Env};

use crate::accrual;
use crate::test::invariants::assert_conservation;
use crate::types::{Stream, StreamStatus};

/// Build a stream directly, bypassing the contract, so a property case costs
/// no host invocations.
fn stream_of(deposited: i128, start: u64, duration: u64, cliff_offset: u64) -> Stream {
    let env = Env::default();
    Stream {
        sender: Address::generate(&env),
        recipient: Address::generate(&env),
        token: Address::generate(&env),
        deposited,
        withdrawn: 0,
        start_time: start,
        end_time: start + duration,
        cliff_time: start + cliff_offset,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: None,
        paused_total: 0,
        status: StreamStatus::Active,
    }
}

/// Longest schedule generated. Bounded so that `deposited * duration` cannot
/// overflow given the deposit ceiling used by the strategies below.
const MAX_DURATION: u64 = 20 * 365 * 86_400;

/// Coerce a raw generated deposit into one `create_stream` would accept.
///
/// Deliberately *derives* a valid value rather than filtering with
/// `prop_assume!`. Filter-based generation starves: proptest aborts a test
/// after 1024 global rejects, so a filter that rejects even a modest fraction
/// of cases turns into a spurious failure once the case count is raised — which
/// is exactly what CI does nightly. Every strategy here is rejection-free.
fn deposit_for(raw: i128, duration: u64) -> i128 {
    // At least one stroop per second, mirroring the contract's rate floor.
    raw.max(duration as i128)
}

/// Map a raw value into `[0, duration)`.
fn within(raw: u64, duration: u64) -> u64 {
    raw % duration
}

proptest! {
    // `ProptestConfig::default()` reads PROPTEST_CASES from the environment
    // (defaulting to 256). Do NOT use `with_cases(n)` here — it overrides the
    // env var, which would silently pin CI's nightly deep sweep back to the
    // local default.
    #![proptest_config(ProptestConfig::default())]

    /// Vesting is bounded below by zero and above by the deposit, at every
    /// instant, including far past the end and far before the start.
    #[test]
    fn vested_stays_within_zero_and_deposited(
        deposited in 1i128..i128::MAX / (1 << 40),
        duration in 1u64..(20 * 365 * 86_400),
        cliff_frac in 0u64..=100,
        offset in -1_000_000i64..(40 * 365 * 86_400),
    ) {
        let cliff_offset = duration * cliff_frac / 100;
        let deposited = deposit_for(deposited, duration);

        let start = 1_700_000_000u64;
        let s = stream_of(deposited, start, duration, cliff_offset);
        let now = (start as i64 + offset).max(0) as u64;

        let v = accrual::vested(&s, now).unwrap();
        prop_assert!(v >= 0, "vested went negative: {}", v);
        prop_assert!(v <= deposited, "vested {} exceeded deposit {}", v, deposited);
    }

    /// **Conservation.** What the recipient has earned plus what the sender
    /// would get back on cancellation is always exactly the deposit. No dust,
    /// no leak, at any instant.
    #[test]
    fn vested_plus_refundable_equals_deposited(
        deposited in 1i128..i128::MAX / (1 << 40),
        duration in 1u64..(20 * 365 * 86_400),
        cliff_frac in 0u64..=100,
        elapsed in 0u64..(40 * 365 * 86_400),
    ) {
        let cliff_offset = duration * cliff_frac / 100;
        let deposited = deposit_for(deposited, duration);

        let start = 1_700_000_000u64;
        let s = stream_of(deposited, start, duration, cliff_offset);
        let now = start + elapsed;

        // Shared invariant checker: panics with a clear message on breach.
        assert_conservation(&s, now);

        let v = accrual::vested(&s, now).unwrap();
        let r = accrual::refundable(&s, now).unwrap();
        prop_assert_eq!(v + r, deposited);
    }

    /// Vesting never goes backwards. If it could, `withdrawn` would be able to
    /// exceed `vested` and the withdrawable calculation would underflow.
    #[test]
    fn vested_is_monotonic_in_time(
        deposited in 1i128..i128::MAX / (1 << 40),
        duration in 1u64..(20 * 365 * 86_400),
        cliff_frac in 0u64..=100,
        t in 0u64..(20 * 365 * 86_400),
        step in 1u64..(365 * 86_400),
    ) {
        let cliff_offset = duration * cliff_frac / 100;
        let deposited = deposit_for(deposited, duration);

        let start = 1_700_000_000u64;
        let s = stream_of(deposited, start, duration, cliff_offset);

        let earlier = accrual::vested(&s, start + t).unwrap();
        let later = accrual::vested(&s, start + t + step).unwrap();
        prop_assert!(later >= earlier, "vesting went backwards: {} -> {}", earlier, later);
    }

    /// Rounding is **down**, and tight to within one stroop.
    ///
    /// Truncating in the recipient's disfavour is the correct direction: the
    /// residue stays in the pool and returns to the sender at settlement, so
    /// the contract can never owe more than it holds.
    #[test]
    fn vesting_rounds_down_and_is_tight(
        deposited in 1i128..i128::MAX / (1 << 40),
        duration in 2u64..MAX_DURATION,
        elapsed_raw in 1u64..MAX_DURATION,
    ) {
        let deposited = deposit_for(deposited, duration);
        // Derived, not filtered: always strictly inside the schedule.
        let elapsed = within(elapsed_raw, duration).max(1);

        let start = 1_700_000_000u64;
        let s = stream_of(deposited, start, duration, 0);
        let v = accrual::vested(&s, start + elapsed).unwrap();

        let d = duration as i128;
        let e = elapsed as i128;
        // v == floor(deposited * elapsed / duration)
        prop_assert!(v * d <= deposited * e, "rounded up");
        prop_assert!((v + 1) * d > deposited * e, "rounded down too far");
    }

    /// Before the cliff the entitlement is exactly zero; at the cliff instant
    /// the recipient is owed everything accrued since `start_time`, not merely
    /// what accrues after the cliff.
    #[test]
    fn cliff_gates_but_does_not_delay(
        deposited in 1i128..i128::MAX / (1 << 40),
        duration in 100u64..(20 * 365 * 86_400),
        cliff_frac in 1u64..100,
    ) {
        let cliff_offset = (duration * cliff_frac / 100).max(1);
        let deposited = deposit_for(deposited, duration);

        let start = 1_700_000_000u64;
        let s = stream_of(deposited, start, duration, cliff_offset);

        prop_assert_eq!(accrual::vested(&s, start + cliff_offset - 1).unwrap(), 0);

        let at_cliff = accrual::vested(&s, start + cliff_offset).unwrap();
        let expected = deposited * cliff_offset as i128 / duration as i128;
        prop_assert_eq!(at_cliff, expected, "cliff must release all prior accrual");
    }

    /// A full withdrawal schedule: draw at arbitrary times, then settle. The
    /// total paid out plus the final refund is exactly the deposit.
    #[test]
    fn withdrawal_schedule_conserves_deposit(
        deposited in 1i128..i128::MAX / (1 << 40),
        duration in 1u64..(20 * 365 * 86_400),
        cliff_frac in 0u64..=100,
        draw_frac in 0u64..=100,
    ) {
        let cliff_offset = duration * cliff_frac / 100;
        let deposited = deposit_for(deposited, duration);

        let start = 1_700_000_000u64;
        let s = stream_of(deposited, start, duration, cliff_offset);

        // Draw at an arbitrary instant, then settle at the end.
        let draw_at = start + duration * draw_frac / 100;
        assert_conservation(&s, draw_at);

        let vested_at_draw = accrual::vested(&s, draw_at).unwrap();
        let refundable_at_draw = accrual::refundable(&s, draw_at).unwrap();
        prop_assert_eq!(vested_at_draw + refundable_at_draw, deposited);

        // After settlement everything has vested and nothing is refundable.
        let end = start + duration;
        assert_conservation(&s, end);
        prop_assert_eq!(accrual::vested(&s, end).unwrap(), deposited);
        prop_assert_eq!(accrual::refundable(&s, end).unwrap(), 0);
    }
}
