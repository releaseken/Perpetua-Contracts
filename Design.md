# `contracts/rent-manager` — Architecture & Technical Design

> **Status:** Draft Design (target crate `contracts/rent-manager` does not yet exist in `Perpetua-Contracts`)
> **Target SDK:** `soroban-sdk = "27.0.6"` (Stellar Protocol 27), Rust `1.97.1` (`edition = "2021"`), `proptest = "1"`
> **Scope:** Addresses issues **#438**, **#444**, **#445**, and **#446**

---

## 1. Overview & Motivation

`contracts/rent-manager` is a Soroban smart contract subsystem in `Perpetua-Contracts` responsible for managing rent/stream-backed allocations, batch claims, and Soroban ledger entry archival resilience.

This document specifies the architecture, storage layout, arithmetic invariants, cross-contract batching model, and verification strategy required to satisfy:

| Issue | Category | Summary |
|-------|----------|---------|
| **#445** | Performance | Streamline cross-contract invocation overhead during high-frequency batch claims |
| **#444** | Resilience | Automated entry lifetime (TTL) extension for archival resilience |
| **#438** | Correctness | Eliminate edge-case precision loss during fractional stream withdrawal calculations |
| **#446** | Verification | Fuzzing (`proptest`) coverage for edge-case timestamps near `u64::MAX` |

---

## 2. Crate Layout & Conventions

Following the standalone crate pattern used by [`contracts/stream/Cargo.toml`](contracts/stream/Cargo.toml):

```text
contracts/rent-manager/
├── Cargo.toml          # Standalone workspace, soroban-sdk = "27.0.6", proptest = "1"
├── src/
│   ├── lib.rs          # Contract entrypoints, auth checks, batch claim orchestration
│   ├── math.rs         # Pure checked arithmetic & fractional withdrawal core (no Env)
│   ├── storage.rs      # Typed DataKey, TTL policy, automated lifetime extension
│   ├── types.rs        # RentSchedule, ClaimBatch, Config, TokenAggregate
│   ├── events.rs       # Structured Soroban contract events
│   ├── error.rs        # #[contracterror] enum with explicit overflow/bounds variants
│   └── test/
│       ├── mod.rs
│       ├── unit.rs     # Deterministic boundary & regression tests
│       └── fuzz.rs     # proptest suites (u64::MAX timestamps, fractional dust, batches)
```

### Core Design Principles
1. **Pure arithmetic core (`math.rs`):** All vesting/accrual/withdrawal math is pure Rust without `Env` or host calls, enabling fast `proptest` execution over millions of cases.
2. **Checked arithmetic everywhere:** No unchecked `+`, `-`, `*`, `/` in contract logic; overflow/underflow maps to explicit `RentError` variants (`Overflow`, `DivisionByZero`, `InvalidTimestampRange`).
3. **Minimal ledger footprint:** Group per-claimant and per-token state to minimize read/write footprint entries and cross-contract `token::Client` calls.

---

## 3. Issue #445 — Streamlining Cross-Contract Invocation Overhead in Batch Claims

### Problem
In high-frequency batch claims, naively iterating over `N` stream/rent entries and invoking `token::Client::transfer` (or external `stream` contract calls) once per entry incurs:
- `O(N)` cross-contract invocation frames (WASM dispatch, host metering, auth/sub-invocation overhead).
- Redundant storage reads/writes for shared metadata (token addresses, config, claimant records).
- Excessive ledger read/write footprint size, risking Soroban transaction resource limits.

### Design Solution

#### 3.1 Per-Token Transfer Aggregation (Netting)
During `batch_claim(env, claimant, schedule_ids: Vec<u64>)`:
1. **Single-pass validation & accrual:** Load each `RentSchedule` once, compute claimable delta via `math::claimable_amount`, update `withdrawn` in memory, and accumulate payouts into a bounded map keyed by `Address` (token contract):
   ```rust
   // Pseudo-structure
   // Map<Address, i128> (or sorted Vec<(Address, i128)> for deterministic small-K tokens)
   ```
   Using a small sorted `Vec<(Address, i128)>` when distinct tokens per batch are few avoids heavy `Map` host-object allocations, while keeping deterministic execution order.
2. **Coalesced `token::Client::transfer`:** Emit **at most one** cross-contract `transfer` call per unique token address in the batch (summing all non-zero claimable amounts for that token via `checked_add`).
3. **Skip zero-amount transfers:** If a schedule has `claimable == 0`, do not write back unchanged storage entries and do not emit a cross-contract transfer.

#### 3.2 Cached External Contract Clients & Bounded Batch Size
- Instantiate `token::Client::new(&env, &token_addr)` once per unique token rather than per schedule.
- Enforce `MAX_BATCH_SIZE` (configurable or constant, e.g., `50`) to guarantee execution stays within Soroban CPU/memory/footprint budgets; reject oversized batches early with `RentError::BatchTooLarge`.
- Deduplicate `schedule_ids` (or reject duplicates with `RentError::DuplicateScheduleId`) before mutating state to prevent double-crediting within a single batch.

#### 3.3 Single Instance Extension per Batch
- Extend instance storage TTL **once** at the end of `batch_claim` instead of inside the per-schedule loop.

---

## 4. Issue #444 — Automated Entry Lifetime Extension (Archival Resilience)

### Problem
Soroban `Persistent` and `Instance` ledger entries expire and become archived if their TTL drops to zero. While underlying token balances remain in the contract's pooled balance, archived accounting records become unreadable until explicitly restored via a separate restore transaction.

### Design Solution
Adopt and extend the dual-tier TTL policy established in [`contracts/stream/src/storage.rs`](contracts/stream/src/storage.rs):

| Storage Tier | Entries | TTL Policy |
|--------------|---------|------------|
| **Instance** | `Config`, `NextScheduleId`, `Admin` | Always extended to `max_ttl()` (or `(threshold, extend_to)` pinned near `env.storage().max_ttl()`) on every mutating entrypoint. |
| **Persistent** | `DataKey::Schedule(id)`, `DataKey::ClaimantIndex(addr)` | Extended on **creation** and **every read/write touch** using `extend_ttl(threshold, extend_to)`. |

#### 4.1 Automated Extension Mechanics
- **Constants / Config:**
  - `LEDGER_SECONDS_ESTIMATE: u64 = 5`
  - `MIN_TTL_LEDGERS`: floor so settled or short-lived entries remain readable for keeper/audit windows.
  - `TTL_BUFFER_LEDGERS`: safety buffer added beyond a schedule's `end_time`.
  - `TTL_RENEWAL_THRESHOLD`: fraction of target TTL (e.g., `50%` or `max_ttl / 2`) below which `extend_ttl` triggers, avoiding unnecessary rent charges when TTL is already fresh.
- **On Schedule Creation:**
  Compute remaining duration `end_time.saturating_sub(now)` converted to ledgers (using `checked_div` / `saturating_add`, clamped to `env.storage().max_ttl()`), and call `env.storage().persistent().extend_ttl(&key, threshold, extend_to)`.
- **On Every Read/Write Touch (`load_schedule` / `save_schedule`):**
  Automatically bump persistent TTL so actively claimed schedules never archive.
- **Permissionless Keeper Entrypoint (`extend_schedule_ttl`):**
  Allow any caller (claimant, sender, or automated keeper) to pass a `schedule_id` (or batch of `schedule_ids`) to bump both persistent schedule TTL and instance TTL without modifying balances.
- **Ordering Invariant:**
  Instance storage (`NextScheduleId`) must never expire before any persistent `Schedule(id)` entry.

---

## 5. Issue #438 — Resolving Edge-Case Precision Loss in Fractional Stream Withdrawals

### Problem
Fractional stream withdrawal calculations using integer division `(deposited * elapsed) / duration` suffer from two classic precision pitfalls:
1. **Intermediate multiplication overflow** when `deposited` (`i128`) and `elapsed` (`u64` / `u128`) are large.
2. **Cumulative truncation dust / rounding drift** across frequent small withdrawals:
   - If `withdrawn` is subtracted before scaling or if rate-per-second is pre-rounded (`rate = deposited / duration`), every withdrawal loses fractional remainder, causing `sum(withdrawals) < deposited` at `t >= end_time` or allowing subtle rounding mismatches.

### Design Solution

#### 5.1 Canonical Cumulative Vested Formulation
Never compute incremental `delta = rate * dt` with a pre-rounded `rate`. Instead, always compute **cumulative vested** from `start_time` and subtract `withdrawn`:

```rust
// All inputs validated: start_time <= end_time, deposited >= 0, 0 <= withdrawn <= deposited
// t_eff = effective_time(schedule, now).clamp(start_time, end_time)
// elapsed = (t_eff - start_time) as u128
// duration = (end_time - start_time) as u128
```

1. **Boundary short-circuits (exactness):**
   - If `t_eff <= start_time` (or before cliff): `vested = 0`.
   - If `t_eff >= end_time` (or `duration == 0`): `vested = deposited` (**zero dust at settlement**).
2. **Wide / Overflow-Safe Pro-Rata Scaling (`mul_div_floor`):**
   Compute `floor((deposited as u128 * elapsed) / duration)` without overflow:
   - Use 256-bit intermediate product (e.g., `U256` from `soroban_sdk::U256` or pure-Rust `u128` widening helper `(hi, lo)` long division) so `deposited <= i128::MAX` multiplied by any `u64` elapsed time **never** overflows `u128` intermediate state.
   - `claimable = vested.checked_sub(withdrawn)?.max(0)`.
3. **Invariants Enforced:**
   - **Conservation:** `vested(t) + refundable(t) == deposited` for all `t`.
   - **Monotonicity:** `t1 <= t2 => vested(t1) <= vested(t2)`.
   - **Zero Dust at Maturity:** `t >= end_time => vested(t) == deposited`.
   - **Bounds:** `0 <= withdrawn <= vested(t) <= deposited`.

---

## 6. Issue #446 — Fuzzing Coverage for Timestamps Near `u64::MAX`

### Problem
Timestamps in Soroban (`env.ledger().timestamp()`) are `u64`. Near `u64::MAX`:
- `start_time + duration` overflows `u64`.
- `now + buffer` or `paused_total + pause_duration` overflows `u64`.
- `(now - start_time)` underflows if `now < start_time`.
- `end_time - start_time` or `u64::MAX - 1` edge cases can trigger panics or silent wrapping if unchecked arithmetic is used.

### Design Solution

#### 6.1 Defensive Timestamp Arithmetic
- Validate at creation: `start_time <= end_time`, and if `cliff_time` exists, `start_time <= cliff_time <= end_time`.
- Never compute `end_time = start_time + duration` with unchecked `+`; use `checked_add` and return `RentError::TimestampOverflow`.
- In elapsed/remaining time calculations:
  - Use `t.saturating_sub(start_time)` only after explicit range checks, or `checked_sub` returning `0` when `t <= start_time`.
  - Convert differences to `u128` before any multiplication or ledger-to-second scaling.
  - When computing TTL extension targets from `end_time.saturating_sub(now)`, convert seconds to ledgers via `u128` and clamp to `u32::MAX` / `max_ttl()` so `u64::MAX` timestamps cannot overflow `u32` ledger counts.

#### 6.2 `proptest` Fuzzing Suite (`src/test/fuzz.rs`)
Add targeted `proptest!` strategies in `contracts/rent-manager/src/test/fuzz.rs`:
1. **`u64::MAX` Boundary Strategy:**
   Generate timestamps weighted heavily toward `[u64::MAX - 10_000 ..= u64::MAX]`, `0`, `1`, and `u64::MAX / 2`:
   - Verify `vested(schedule, now)` never panics for any `(start_time, end_time, now)` in `0..=u64::MAX`.
   - Verify `create_schedule` rejects `start_time > end_time` cleanly with `RentError`.
   - Verify TTL calculation `ttl_ledgers_for_schedule(end_time, now)` never panics or wraps when `end_time == u64::MAX` and `now == 0` (clamps cleanly to `max_ttl()`).
2. **Fractional Precision & Zero-Dust Strategy:**
   Fuzz `deposited` across prime/odd `i128` values (`1..=i128::MAX / 2`), odd `duration`s (`1..=u64::MAX`), and arbitrary sequences of `N` withdrawal timestamps `t_1 <= t_2 <= ... <= t_n >= end_time`:
   - Assert `sum(withdrawn_i) == deposited` at `t_n >= end_time`.
   - Assert `0 <= vested(t_i) <= deposited` and monotonicity `vested(t_i) <= vested(t_{i+1})`.
3. **Batch Claim & Coalescing Equivalence Strategy:**
   Fuzz batches of `1..=50` schedules across `1..=5` distinct tokens and assert that `batch_claim` produces identical final token balances and schedule states to sequential individual claims, with `transfer_count <= unique_tokens`.

---

## 7. Verification Plan

Once `contracts/rent-manager` is scaffolded:
1. Run unit and `proptest` suites:
   ```bash
   cd contracts/rent-manager && cargo test
   ```
2. Build WASM target to verify Soroban `no_std`/SDK compatibility:
   ```bash
   cd contracts/rent-manager && cargo build --target wasm32v1-none --release
   ```
3. Verify ledger footprint and cross-contract transfer counts in Soroban `Env` integration tests (`env.auths()` / event assertions).
