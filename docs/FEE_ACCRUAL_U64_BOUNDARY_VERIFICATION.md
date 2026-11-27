# Fee Accrual `u64` Timestamp & Boundary Verification

## Overview

This audit document details formal verification, checked arithmetic invariants, and fuzzing coverage for the `fee-accrual` subsystem under extreme edge-case timestamps and stream identifiers near `u64` maximum boundary limits (`u64::MAX`).

Reference issue: **#392** — `test(contracts/fee-accrual): add fuzzing coverage for edge-case timestamps in fee-accrual near u64 maximum boundary limits`.

---

## Technical Scope & Threat Model

In Soroban SDK contracts, timestamps and streaming identifier types are represented as unsigned 64-bit integers (`u64`). If defensive arithmetic, saturating progressions, or footprint TTL bumps are not strictly checked, extreme timestamp conditions introduce critical failure modes:

1. **Arithmetic Overflow / Underflow**: Calculations involving timestamps or basis point fee distributions could wrap or trap if unchecked arithmetic is performed.
2. **Persistent Storage / Footprint Desynchronization**: Persistent entries (`DataKey::StreamFee(stream_id)`) keyed by identifiers approaching `u64::MAX` could collide or corrupt persistent state if key serialization or hashing behaves irregularly at limits.
3. **Ledger Time Warping & Skew**: Network reorgs, out-of-order calls, or large timestamp advancements up to `u64::MAX` must not invalidate role-based access control or break state machines (such as settled streams or emergency pause switches).

---

## Defensive Invariants & Formal Guarantees

The `fee-accrual` subsystem enforces the following algebraic and state invariants:

- **Conservation of Fee Splits (I1)**:
  $$\text{protocol\_fee} + \text{keeper\_fee} = \text{total\_fee}$$
  $$\text{net\_amount} + \text{total\_fee} = \text{gross\_amount}$$
- **Strict Bounded Ranges (I2)**:
  $$0 \le \text{total\_fee} \le \text{gross\_amount}$$
  $$0 \le \text{protocol\_fee} \le \text{total\_fee}$$
  $$0 \le \text{keeper\_fee} \le \text{total\_fee}$$
- **Cumulative Global Monotonicity (I3)**:
  $$\Delta \text{TotalProtocolFees} = \text{protocol\_fee}$$
  $$\Delta \text{TotalKeeperFees} = \text{keeper\_fee}$$
- **Zero Identification Trap Prevention (I4)**:
  Stream ID `0` is strictly rejected with `FeeError::InvalidStreamId`.
- **Settlement Terminal State (I5)**:
  Once a stream's fee accounting is marked settled (`is_settled = true`), subsequent accrual attempts revert with `FeeError::FeeAlreadySettled`.

---

## Test Suite Architecture

Located in `contracts/fee-accrual/src/test/`:

1. **`common.rs`**:
   - `Harness`: Instantiates `FeeAccrual`, registers test addresses, mocks authorizations, and provides utilities for deterministic time advancement (`advance`) and boundary warping (`warp_to`).
2. **`fuzz_u64_boundary.rs`**:
   - **Boundary Tests**:
     - `pure_math_invariants_hold_at_extremes`: Tests rate extremes ($0\%$, $100\%$, $>100\%$, negative inputs).
     - `fee_accrual_at_u64_max_timestamp_and_stream_id`: Exercises accrual at `u64::MAX` timestamp and `stream_id = u64::MAX`.
     - `adjacent_boundary_stream_ids_near_u64_max_do_not_interfere`: Validates storage isolation for `u64::MAX`, `u64::MAX - 1`, and `u64::MAX - 2`.
     - `full_lifecycle_at_u64_max_boundary`: Validates complete lifecycle (`accrue` $\to$ `claim` $\to$ `settle`) crossing the `u64::MAX` boundary.
     - `batch_accrual_at_u64_boundary`: Tests batch processing containing mixed boundary stream IDs.
     - `emergency_pause_at_u64_max_boundary`: Validates emergency pause gating when invoked at `u64::MAX`.
   - **Proptest Property Fuzzing**:
     - `prop_fuzz_fee_breakdown_conservation`: Proptest generator testing conservation over millions of gross amount and basis point combinations.
     - `prop_fuzz_stream_id_and_timestamp_boundary`: Samples timestamp offsets and stream ID offsets near `u64::MAX` verifying non-panicking execution and exact state persistence.
     - `prop_fuzz_monotonic_fee_aggregation`: Verifies monotonicity of protocol and keeper fee aggregates across multi-step randomized time hops.
