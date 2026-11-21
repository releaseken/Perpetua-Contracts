# Stream-NFT Edge-Case Timestamp Fuzzing & Boundary Hardening

## Overview
This document specifies the verification, defensive arithmetic model, and fuzzing coverage implemented in the `stream-nft` subsystem for issue **#374** ("test(contracts/stream-nft): add fuzzing coverage for edge-case timestamps in stream-nft near u64 maximum boundary limits").

## Problem Statement & Risks
In streaming contracts and continuous token representations:
- Time fields (`start_time`, `end_time`, `cliff_time`, `paused_at`, `paused_total`) as well as NFT identifiers (`stream_id`) are represented as `u64`.
- Ledger timestamps at boundary points near `u64::MAX` present distinct risks:
  1. Arithmetic overflow in interval computations `end_time - start_time` or elapsed calculations.
  2. Underflow in saturating subtractions `reference.saturating_sub(paused_total)`.
  3. Pre-cliff and maturity clamp invariants: `vested` must be 0 prior to cliff gate and clamp cleanly to `deposited` at or after maturity without integer wraps.
  4. Token supply / balance accounting: minting and transferring NFTs keyed by `stream_id` values near `u64::MAX` or when `balance` is incremented/decremented must use checked operations (`checked_add`, `checked_sub`).

## Architecture & Implementation

### 1. Contract Hardening (`contracts/stream-nft/src/lib.rs`)
- **Explicit Error Discriminants**: Added `Error::Overflow` and `Error::ArithmeticOverflow`.
- **Defensive Mint & Transfer Accounting**:
  - `mint`: Replaced unchecked additions on `supply` and `caller_balance` with `checked_add(1)`, panicking with `Error::Overflow` if exhausted.
  - `transfer`: Replaced unchecked subtraction and addition on recipient/sender balances with `checked_sub(1)` and `checked_add(1)`.

### 2. Defensive Math Library (`contracts/stream-nft/src/math.rs`)
- Added pure functions for timestamp and accrual operations:
  - `checked_duration(start, end) -> Result<u64, Error>`: Safe duration calculation rejecting `end < start`.
  - `stream_time(now, paused_at, paused_total) -> u64`: Effective stream time freezing during pause and saturating defensively.
  - `elapsed(start, end, now, paused_at, paused_total) -> u64`: Clamped elapsed duration bounded strictly within `[0, duration]`.
  - `cliff_reached(cliff_time, now, paused_at, paused_total) -> bool`: Step-function cliff gate evaluator.
  - `compute_vested(...) -> Result<i128, Error>`: Fixed-point vesting computation using checked arithmetic.
  - `compute_refundable(...) -> Result<i128, Error>`: Conservation complement calculation `deposited - vested`.

### 3. Fuzzing & Unit Test Suites (`contracts/stream-nft/src/test/`)
- **Unit Tests (`unit.rs`)**:
  - Initialization and metadata assertions.
  - Tracking token balances and total supply on single and multiple mints.
  - Querying unminted tokens.
  - Minting at boundary `stream_id == u64::MAX`.
- **Edge-Case & Fuzz Tests (`fuzz_u64_boundary.rs`)**:
  - **Explicit Handcrafted Boundaries**: Probes at `u64::MAX`, `u64::MAX - 1`, `u64::MAX - 1000`, `start == 0`, and `end == u64::MAX`.
  - **Host-Level Ledger Timestamps**: Verifies minting and balance lookups with host ledger timestamps advanced to `u64::MAX - 1000` and `u64::MAX`.
  - **Proptest-Driven Fuzzing**:
    - `fuzz_timestamps_near_u64_max`: Parameterized fuzzing across the upper `u64` boundary asserting invariants **I1** (bounds), **I2** (monotonicity), **I3** (conservation `vested + refundable == deposited`), **I4** (cliff gate), and **I5** (maturity clamp).
    - `fuzz_paused_stream_at_u64_max_boundary`: Parameterized fuzzing verifying pause freezes stream clock and prevents premature cliff gate opening under extreme timestamps.

## Verification
- Code formatted with `rustfmt` cleanly.
- Static checks verify checked arithmetic across all code paths.
