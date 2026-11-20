# Design — Hardening the multisig/rent/archival tranche

Status: **Draft — pending scope confirmation** (see §0).
Scope: issues #429, #440, #441, #426.

---

## 0. Scope caveat — read this first

The four issues name three contracts that **do not exist in this workspace**:

| Issue | Names this contract | Present in this repo? |
|-------|--------------------|------------------------|
| #429 | `contracts/multisig-threshold` | ❌ no such crate |
| #440 | `contracts/rent-manager` | ❌ no such crate |
| #441 | `contracts/rent-manager` | ❌ no such crate |
| #426 | `contracts/oracle-router` | ❌ no such crate |

The actual contracts in this workspace are `stream`, `stream-nft`, `factory`,
`governance`, and `archival-probe`. The issues are also filed against
`releaseken/Perpetua-Contracts`, while this checkout is `aristotle224/Perpetua-Contracts`
— a different fork.

This design therefore **translates each issue's substantive theme onto the real
contracts that already carry that responsibility here**, rather than inventing
three new contract crates from a one-line issue title. The mapping is explicit
below and is the single most important assumption to confirm before any code is
written. If the intent was instead to scaffold `multisig-threshold`,
`rent-manager`, and `oracle-router` as brand-new contracts, §8 lists what that
path requires and this document does not design it.

### Mapping

| Issue | Theme | Real contract(s) this design targets | Why this is the right home |
|-------|-------|---------------------------------------|----------------------------|
| #429 | Precision loss in fractional stream withdrawal under a multisig threshold | `governance` (the threshold layer) + `stream` (the withdrawal math) | `governance` **is** a multisig-threshold contract (co-signers, `Threshold`, weighted `approval_weight`, timelock, quorum) and already dispatches operations into `stream` via `CallData` — but has no withdrawal operation today. |
| #440 | Minimize host CPU instructions by optimizing loop deserialization | `stream` `batch_withdraw` + `storage` | The rent/TTL management in this repo lives inside `stream::storage`; `batch_withdraw` is the loop that deserializes many stream entries and is the realistic optimization target. |
| #441 | Byzantine fault tolerance test scenarios under simulated network partitions | `stream` `accrual` read-freshness/replay primitives + `archival-probe` + `governance` | `accrual` already ships `classify_read` / `ReadFreshness`, `recovery_for` / `Recovery`, and `remaining_to_apply` — the exact primitives a BFT-under-partition test suite would exercise. |
| #426 | Automated entry lifetime extension for archival resilience | `stream::storage` TTL engine + `archival-probe` | `storage` already implements schedule-aware TTL targets and a permissionless keeper path; `archival-probe` is the live-network canary for the archival failure mode. |

Everything below is written against the contracts in the left-hand real-contract
column. Where an issue's literal wording implies a new contract, the design
notes that explicitly rather than silently inventing one.

---

## 1. Goals

1. **#429** — Make a threshold-gated, *fractional* stream withdrawal
   numerically exact: no stroop is created or destroyed by the fraction→amount
   conversion, residue is accounted for deterministically, and every arithmetic
   step is checked.
2. **#440** — Reduce host CPU instructions in the multi-entry deserialization loop
   (`batch_withdraw`) by removing redundant deserialization and consolidating the
   storage footprint, without changing observable behaviour or the public ABI.
3. **#441** — Add a deterministic BFT test matrix for the rent/TTL subsystem that
   simulates ledger skew, stale reads, replayed/duplicate operations, and
   archive→restore transitions, asserting the invariants the production code
   already claims.
4. **#426** — Specify an automated, idempotent entry-lifetime-extension routine
   that guarantees no live stream entry archives, and degrades gracefully
   (detectable restore) if it ever does.

## 2. Non-goals

- Designing new standalone `multisig-threshold`, `rent-manager`, or
  `oracle-router` crates. (See §8 if that is actually wanted.)
- Changing the stream's core accrual rounding policy (round-down, residue to
  sender). That policy is load-bearing for conservation (I1, I4, I6) and stays.
- Renumbering any `Error` / `GovernanceError` discriminant — both are frozen
  public ABI, append-only.
- Adding an on-chain scheduler/cron. Stellar has none; "automated" in #426 means
  a deterministic keeper *routine* plus an off-chain trigger, not a contract that
  wakes itself.

## 3. Background (grounding in the current code)

### 3.1 Stream accrual (`contracts/stream/src/accrual.rs`)

The withdrawal pipeline is:

```
vested(t)        = 0                                  if before cliff
                 = deposited                         if duration==0 or elapsed>=duration
                 = deposited * elapsed / duration     otherwise  (integer div, rounds DOWN)
withdrawable(t)  = max(0, vested(t) - withdrawn)
refundable(t)    = deposited - vested(t)              (exact; I4: vested+refundable==deposited)
```

Rounding is always **down**, by design: truncation residue stays in the pool and
returns to the sender at settlement, so the pool can never owe more than it
holds (`accrual.rs`, `vested` doc + final clamp `if raw > deposited { deposited
} else { raw }`). The only arithmetic error is `Error::Overflow` (alias
`ArithmeticOverflow`), used at the `checked_mul` in `vested` and the
`checked_sub` in `withdrawable`/`refundable`/`liability`.

Three primitives already exist that a threshold-gated withdrawal and the BFT
tests will lean on:

- `remaining_to_apply(requested_total, withdrawn) = (requested_total - withdrawn).max(0)`
  — idempotent replay guard: replaying a withdrawal whose effect is already in
  `withdrawn` yields zero, not a double payout.
- `classify_read(observed_ledger, required_ledger) -> ReadFreshness::{Fresh, Stale{behind_by}}`
  — deterministic freshness verdict for a read that may lag the ledger the
  caller requires; a stale read is discarded and re-read, never combined.
- `recovery_for(present, stream_id, next_stream_id) -> Recovery::{None, RestoreFootprint, NeverExisted}`
  — distinguishes an archived-but-issued entry from one that was never created,
  the signal a keeper/SDK keys on.

### 3.2 Stream storage / TTL engine (`contracts/stream/src/storage.rs`)

The rent model in this repo:

- **Instance** entry `DataKey::NextStreamId` is pinned to `max_ttl()` on every
  mutating call (`extend_instance`), so the id counter can never archive before
  the streams it issued.
- **Persistent** entry `DataKey::Stream(id)` is extended to
  `ttl_target_ledgers` = `seconds_to_ledgers(remaining_life + TTL_BUFFER_SECONDS)`,
  floored at `MIN_STREAM_TTL_LEDGERS` (~30 days × 1.1 drift headroom), clamped to
  the network `max_entry_ttl` (queried dynamically, never baked in).
- `seconds_to_ledgers` rounds **up** (ceiling) so the ledger count never
  undershoots the requested lifetime.
- `load_stream` extends TTL on read; `peek_stream` is view-only (no write) — used
  by batch preflight and views.
- A permissionless keeper path (`extend_stream_ttl`) keeps a stream readable
  without the sender's cooperation; it bumps both the persistent stream entry
  and the instance counter together (the ordering guarantee).

### 3.3 Governance = the multisig-threshold layer (`contracts/governance/src/lib.rs`)

`governance` is a co-signer multisig with a configurable threshold, per-signer
voting weight, a timelock, quorum snapshots, a grace window, and a reentrancy
guard. It executes by decoding a `CallData` variant and dispatching a
cross-contract call to a `target`. The stream-related `CallData` variants today
are `StreamSetAdmin`, `StreamSetMaxRate`, `StreamGlobalResume`, and
`StreamBulkResumeAsAdmin`. **There is no variant that performs a withdrawal.**
That gap is exactly where #429 lands.

### 3.4 Archival probe (`contracts/archival-probe/src/lib.rs`)

A non-product canary that writes one persistent entry and deliberately never
extends its TTL, so it archives on the network's minimum schedule. It proves the
live-network sequence `read archived → tx FAILS → resubmit with
RestoreFootprint → read succeeds`. The restore mechanism it exercises is a
ledger property, so proving it for the probe proves it for `DataKey::Stream(id)`.

---

## 4. Design — #429: exact fractional withdrawal under a multisig threshold

### 4.1 Problem

A threshold-gated withdrawal lets governance move a *fraction* of a stream's
withdrawable balance — e.g. "pay the recipient one-third of what they have
earned" — without the proposer precomputing the stroop amount off-chain (which
would race the accrual clock between propose and execute). Converting a fraction
to stroops is where precision loss enters: `withdrawable * num / denom` is
integer division, and if `denom` does not divide the product cleanly the residue
has to go somewhere deterministic, or stroops are silently created or destroyed.

### 4.2 Shape

Add one stream entry point and one `CallData` variant, both additive (no ABI
renumber, no existing behaviour change).

**Stream side — new function** (additive, behind admin auth, mirroring the
existing `bulk_resume_streams_as_admin` admin path):

```
FluxoraStream::withdraw_fraction_as_admin(
    env, stream_id, recipient, numerator, denominator
) -> Result<i128, Error>
```

- `numerator`/`denominator` are positive `i128`, `denominator > 0`, `numerator <= denominator`
  (a fraction in `[0,1]`; `numerator == denominator` is "withdraw everything").
- Auth: the stream **admin** (`require_auth_for_args`). Governance becomes the
  stream admin (it already supports `StreamSetAdmin`), so a passed proposal is
  what authorizes this call. This reuses the existing admin-auth pattern rather
  than introducing a second authorization surface.
- Computes `payout = withdrawable(stream, now) * numerator / denominator` with the
  same rounding discipline as `vested`: **round down**, residue stays in the pool.

**Governance side — new `CallData` variant** (append-only, wire form is the
variant *name* not its position, so existing encoded proposals stay decodable):

```
CallData::StreamWithdrawFraction { stream_id: u64, recipient: Address, numerator: i128, denominator: i128 }
```

Dispatched via `dispatch_operation` to `withdraw_fraction_as_admin` on the
proposal's `target`. Reaches threshold → waits the timelock (`eta`) → executes
inside the existing reentrancy guard.

### 4.3 Exact arithmetic (the core of #429)

The conversion must be overflow-safe and conserving. The design:

```
let available = accrual::withdrawable(&stream, now)?;            // i128, >= 0
// overflow-safe product: available * numerator
let scaled = available.checked_mul(numerator).ok_or(Error::Overflow)?;
let payout = scaled.checked_div(denominator).ok_or(Error::DivisionByZero)?;
```

Conservation and residue handling, matching the stream's existing invariant set
(I1/I4/I6 in `accrual.rs`):

1. **Rounding direction = down.** Identical to `vested`. The residue
   `available - payout - (already withdrawn delta)` stays in the pool and is
   refundable to the sender at settlement. The pool can never owe more than it
   holds.
2. **Conservation is exact across the call.** `withdrawable` is already defined
   as `vested - withdrawn`, and `apply_withdrawal` (the existing helper used by
   `withdraw` and `batch_withdraw`) bumps `stream.withdrawn` by exactly `payout`
   and transfers exactly `payout`. So `vested + refundable == deposited` (I4) is
   preserved by construction — the fractional path reuses `apply_withdrawal`
   rather than carrying its own transfer.
3. **Idempotency against replay.** A proposal that executes, then is replayed
   (e.g. an ambiguous tx result under a partition — see #441) must not pay twice.
   `withdraw_fraction_as_admin` applies `remaining_to_apply` semantics: the
   *intended* total is bounded by the current `withdrawable`, which already
   subtracts `withdrawn`. A replay computes `payout` against a `withdrawn` that
   has already absorbed the first execution, so a replayed identical fraction
   pays strictly less (the remaining fraction of the *newly accrued* balance
   only) — never a duplicate of the original amount. This is the property
   `remaining_to_apply` was written to guarantee; the fractional path inherits it
   by reading `withdrawable` fresh rather than caching an amount.
4. **Fraction normalization is the caller's job, not the contract's.** `1/3` and
   `2/6` are different inputs that happen to produce the same `payout` only when
   `denominator` divides `available*numerator` identically; the contract does
   not reduce fractions on chain (reduction is a pure function the proposer can
   do off-chain). This keeps the on-chain path one division, not a GCD + division.
5. **Overflow is a typed error, never a trap.** `available.checked_mul(numerator)`
   maps to `Error::Overflow`, consistent with `vested`'s guard. Because
   `available <= deposited <= i128::MAX` and `numerator <= denominator`, the
   product is bounded by `deposited * denominator`; `create_stream` already
   guarantees `deposited * duration` fits `i128`, but `denominator` is
   caller-chosen and unbounded, so the checked_mul is **not** defence-in-depth
   here — it is the primary guard. The design rejects `denominator` values that
   would make `available * numerator` overflow by returning `Error::Overflow`
   rather than truncating.

### 4.4 Edge cases explicitly handled

| Edge case | Handling |
|-----------|----------|
| `denominator == 0` | New `Error::DivisionByZero` (append to `Error`, e.g. `= 35`). Distinct from `Overflow` so a client can tell "bad fraction" from "too big". |
| `numerator == 0` | `payout == 0`; the call is a no-op that still pays gas. Reject with `Error::InvalidAmount` (existing variant) to match the zero-amount policy in `error.rs`. |
| `numerator > denominator` | Reject — a fraction > 1 would let governance over-draw. New `Error::InvalidFraction` (append) or reuse `InsufficientWithdrawable` semantics. Design prefers a dedicated variant for clarity. |
| `available == 0` (pre-cliff / pre-start / fully drawn) | Existing `NothingToWithdraw` / `StreamTerminated` paths apply unchanged; the fractional call routes through the same `available == 0` branch as `withdraw`. |
| Stream terminal | `StreamTerminated` before computing the fraction, matching `withdraw`. |
| Replay under partition | `withdrawable` is recomputed from fresh `withdrawn`; see §4.3(3). |
| Maturity mid-proposal | If the stream matures between propose and execute, `withdrawable` grows to the full deposit and the fraction pays out of the larger pool — correct, and exactly why the amount is computed at execute time, not propose time. |

### 4.5 Tests / fuzz (#429)

- **Unit:** for each `(available, num, denom)` in a boundary set
  (`available = 0`, `1`, `i128::MAX`, `denom = 1`, `2`, `available`, prime
  denominators), assert `payout == available*num/denom` (floor), `payout <=
  available`, and that after applying, `vested + refundable == deposited` still
  holds (reuse the existing `Harness::assert_invariants`).
- **Conservation fuzz:** random `deposited`, `elapsed`/`duration` pair, random
  fraction; assert no stroop created/destroyed across a sequence of fractional
  withdrawals ending in a `cancel`.
- **Replay fuzz:** apply a fractional withdrawal, then replay the identical
  signed payload; assert the second pays at most the first's accrual delta, never
  a duplicate.
- **Governance path:** propose → reach threshold → wait timelock → execute
  dispatches `StreamWithdrawFraction` and moves the expected stroops; a
  sub-threshold or pre-timelock execution reverts (reuses governance's existing
  `QuorumNotReached` / `TimelockNotElapsed` guards).

---

## 5. Design — #426: automated entry lifetime extension for archival resilience

### 5.1 Problem

A stream that lives longer than the network's `max_entry_ttl` *must* be
periodically re-extended or its accounting entry archives and becomes unreadable
until someone pays to restore it. The contract already has a permissionless
`extend_stream_ttl` keeper path and schedule-aware targets; #426 asks for the
**automated, resilience-guaranteeing** layer on top: a routine that provably
keeps every live entry alive and degrades detectably if it ever fails.

### 5.2 Two components

**A. On-chain: a single deterministic `sweep_ttl` routine** (additive view/keeper
entry point on `stream`), not a new contract:

```
FluxoraStream::sweep_ttl(env, stream_ids: Vec<u64>) -> Result<u32, Error>
```

- Permissionless (any caller — keeper, recipient, passer-by), mirroring
  `extend_stream_ttl`.
- For each id: load via `peek_stream` (view-only, no TTL bump on the *read*),
  then `extend_stream` to `ttl_target_ledgers`. Returns the count extended.
- **Idempotent:** extending an already-fresh entry to the same target is a
  no-op at the storage level (Soroban `extend_ttl` to a TTL ≥ current is a
  no-op), so a keeper may re-sweep safely and at low cost.
- **Bounds:** reuses `MAX_BATCH_SIZE` (16) and the same `BatchTooLarge` /
  `DuplicateStreamId` / `MalformedStreamId` validation as `batch_withdraw`, so
  the footprint and event budget math already measured for batches applies
  unchanged. No new resource limit.

**B. Off-chain: the automation trigger** (specification, not contract code —
Stellar has no on-chain scheduler):

- A keeper watches TTLs (readable via `get_ttl` on the persistent entry) and
  calls `sweep_ttl` for any stream whose TTL falls inside `TTL_BUFFER_SECONDS`.
- The schedule-aware target already gives the keeper the full 30-day buffer, so
  "automated" means "well before expiry", not "at the last ledger".
- `archival-probe` is the regression canary: a CI job runs
  `script/archival-canary.sh` against quicktest/testnet to prove the
  archive→RestoreFootprint→read sequence still works, so the resilience
  guarantee is tested against a real network, not just the in-memory host (which
  auto-restores and would hide a regression).

### 5.3 The resilience guarantee

The guarantee has two tiers, both enforceable from existing primitives:

1. **Liveness (normal operation):** for any stream that is not terminal,
   `sweep_ttl` before the TTL drops below `MIN_STREAM_TTL_LEDGERS` keeps the
   entry readable. The instance counter is always at `max_ttl`, so it never
   archives before the streams it issued (the existing ordering guarantee).
2. **Detectable degradation (if the keeper misses):** if an entry does archive,
   `stream_recovery(env, id)` (already in `storage.rs`, wrapping
   `recovery_for`) returns `Recovery::RestoreFootprint` for an issued id whose
   entry is absent — distinct from `NeverExisted`. The caller resubmits with a
   `RestoreFootprint` and the accounting is intact (the probe proves this is a
   ledger property, not a contract one). So a missed sweep is a recoverable
   outage, not a fund loss: tokens sit in the pooled balance; only the
   accounting entry is temporarily unreadable.

### 5.4 Tests / fuzz (#426)

- **Unit:** seed a ledger state with a near-expiry stream (`testutils::Ledger`
  to advance the sequence), call `sweep_ttl`, assert the TTL is restored above
  `MIN_STREAM_TTL_LEDGERS` and below `max_entry_ttl`.
- **Idempotency:** sweep the same ids twice; assert the second is a no-op
  (TTL unchanged, no spurious events).
- **Archival round-trip (network-level, via the probe):** the probe's existing
  `the_probe_does_not_extend_its_own_ttl` test plus a canary run that archives,
  fails to read, restores, and reads — proving the degradation path.
- **Property:** over random schedules with random sweep cadence, no
  non-terminal stream's TTL ever reaches zero (monotonic-in-time TTL floor).

---

## 6. Design — #440: minimize host CPU instructions in the deserialization loop

### 6.1 Problem

`batch_withdraw` today runs **two passes** over the same stream entries:

1. A preflight loop: `peek_stream` (deserialize) for each id, validate
   recipient, compute `withdrawable`, accumulate `total`, and push the
   deserialized `Stream` into a `Vec<Stream>` (`streams`) and amounts into
   `payouts`.
2. An apply loop: pop each stored `Stream` back out of the `Vec`, and for
   non-zero payouts call `apply_withdrawal` — which itself loads/extends storage
   again.

Each `peek_stream`/`load_stream` is a host storage read that **deserializes** the
`Stream` from its ledger entry. Holding an entire `Vec<Stream>` in memory also
costs CPU to deserialize-and-copy every entry up front, even streams that will
pay zero and only need a TTL bump. The instruction cost scales with batch size
and with how many bytes each `Stream` serializes to. #440 asks to minimize that.

### 6.2 Optimizations (behaviour-preserving, ABI-stable)

1. **Load once, reuse.** Replace the two-pass `peek`-then-`load` with a single
   pass: `load_stream` (which extends TTL) on each id in the preflight loop, hold
   the **already-deserialized** `Stream` in the `Vec`, and reuse that same value
   in the apply loop — do not call `load_stream`/`peek_stream` a second time.
   `apply_withdrawal` already takes a `&mut Stream`; it should not re-load. This
   halves the deserialization count from `2*N` to `N`.
2. **Hoist the clock.** `now = env.ledger().timestamp()` is currently read once
   before the loop — keep that, and ensure no per-iteration `env.ledger()` call
   sneaks in via helpers. `accrual::withdrawable` takes `now` by value, so this
   is already satisfied; the change is to keep it that way and avoid any new
   per-iteration host call.
3. **Footprint preloading (the real CPU win).** Soroban charges per storage
   *deserialization*. If the batch's stream keys are declared in the transaction
   footprint up front, the host can carry the deserialized entries across the
   transaction rather than re-deserializing on each access. The contract cannot
   itself declare a footprint, but `sweep_ttl`/`batch_withdraw` can be paired
   with guidance (and a SDK helper) to preload `DataKey::Stream(id)` for the
   batch ids. Where the host supports it, this turns N repeated deserializations
   into N footprint-declared reads. The design documents this as the expected
   caller pattern; the on-chain code change is the single-pass load (#1) which
   is correct regardless of footprint.
4. **Skip the `Vec<Stream>` for zero-payout entries.** A stream whose
   `withdrawable == 0` only needs a TTL extension, not a payout. Its entry can
   be extended in the preflight pass (it is already deserialized) and not held
   in the payout `Vec`, cutting the memory/CPU of carrying it through the apply
   pass. Behaviour is unchanged: zero-payout entries are already "skipped" today,
   this just skips them earlier.
5. **No algorithmic change to validation.** Duplicate detection
   (`reject_duplicate_ids`), `MAX_BATCH_SIZE`, and recipient checks stay exactly
   as-is; they are not in the hot deserialization path.

### 6.3 What does not change

- **Atomicity.** The batch stays all-or-nothing: any error unwinds every staged
  write, including payouts already applied to earlier streams. Single-pass load
  does not affect this — the host still owns the transaction.
- **Event order and shape.** Per-stream `withdrawn` events still emit in batch
  order; the total returned is unchanged.
- **ABI.** No signature change. This is purely an internal loop restructure.

### 6.4 Measurement

- Add a `gas_benchmark`-style test (the factory crate already has
  `tests/gas_benchmark.rs` as a template) that records instruction count and
  `mem_bytes` for `batch_withdraw` at `MAX_BATCH_SIZE` before and after, asserting
  a strict reduction in instructions with no increase in byte reads. The
  existing `test::resource_limits` suite (which reports the 16-stream cap
  against protocol-27 limits) is the place to assert the optimization keeps 2x
  headroom on footprint/writes/reads/instructions.

---

## 7. Design — #441: BFT test scenarios under simulated network partitions

### 7.1 Problem

Under a network partition, a client may read stale ledger state, a transaction
may be replayed after an ambiguous result, and an entry may archive and need
restoration. The production code already has primitives for each of these
(`classify_read`, `recovery_for`, `remaining_to_apply`), but they are currently
`#[allow(dead_code)]` — present for defence in depth but not exercised by a
named scenario suite. #441 asks for that suite.

### 7.2 What "simulated partition" means on Soroban

Soroban has no network simulation primitive, so "partition" is modelled at the
deterministic inputs the contract already consumes:

- **Ledger skew** — an observer's `observed_ledger` is behind the
  `required_ledger` of a confirmed write. `classify_read` returns
  `Stale { behind_by }`; the suite asserts a stale read is discarded and
  re-read, never combined with a fresh one.
- **Replay** — the same signed operation resubmitted after an ambiguous result.
  `remaining_to_apply` asserts the replay pays only the un-applied delta, never
  a duplicate.
- **Archive/restore** — an entry's TTL elapses and the host refuses the read
  until a `RestoreFootprint` is supplied. `recovery_for` asserts the contract
  reports `RestoreFootprint` for an issued-but-absent id (vs `NeverExisted` for
  an unissued one); the `archival-probe` round-trip asserts the data is intact
  after restore.

### 7.3 Scenario matrix

Each scenario is a deterministic test (no real network except the archival
canary), grouped under `contracts/stream/src/test/` alongside the existing
`accrual_*` suites:

1. **stale-read-discard** — `classify_read(observed=99, required=100)` ⇒ `Stale
   { behind_by: 1 }`; caller re-reads at ledger 100 ⇒ `Fresh`. Assert no
   accounting is derived from the stale read.
2. **replay-no-double-pay** — withdraw `X`, then replay the identical withdraw
   request; assert the second pays `remaining_to_apply(X, withdrawn_after_first)`
   = the newly accrued delta, ≤ the first only if accrual advanced, never `X`
   again.
3. **duplicate-id-rejection-under-replay** — a replayed `batch_withdraw` whose
   ids include a duplicate is rejected with `DuplicateStreamId` on every
   attempt (deterministic, position-independent) — already an invariant, now
   asserted under the replay model.
4. **archive-then-restore-preserves-accounting** — advance the ledger past an
   entry's TTL, assert `stream_exists` is `false` and `stream_recovery` returns
   `RestoreFootprint`; restore the footprint, re-read, assert `vested`/`withdrawn`
   are unchanged. (Unit-level via `testutils`; network-level via
   `archival-probe` + `archival-canary.sh`.)
5. **threshold-under-partition (governance × stream)** — a proposal that
   dispatches `StreamWithdrawFraction` (§4) executes after a simulated
   partition gap (ledger advanced, stale reads discarded, timelock still
   enforced); assert the payout reflects the *execute-time* balance, not the
   propose-time balance, and that a replayed execute is rejected
   (`AlreadyExecuted`).
6. **keeper-miss-then-recover** — let a stream's TTL lapse (simulating a keeper
   that missed its window under partition), assert `sweep_ttl` (§5) restores
   readability without loss; this ties #441 to #426.

### 7.4 Fuzz

- **Replay fuzz:** random withdraw schedules, random replay points; invariant:
  total transferred across all (original + replayed) calls == total transferred
  by a single non-replayed run with the same ledger advances. Stronger than
  "no double pay" — it pins conservation under replay exactly.
- **Skew fuzz:** random `observed`/`required` ledger pairs; invariant:
  `classify_read` never returns `Fresh` when `observed < required`, and
  `Stale{behind_by}` equals the exact difference.
- **Restore fuzz:** random stream states, random archive points; invariant:
  post-restore `vested(t)` equals pre-archive `vested(t)` for all `t`.

### 7.5 What this does not claim

These are **tests**, not new on-chain partition-tolerance logic. Soroban's
ordering and atomicity are the host's; the contract cannot "survive" a
partition at the consensus level. What the suite proves is that the contract's
*accounting* is partition-safe: stale reads don't corrupt it, replays don't
enrich it, and archival doesn't lose it. That is the meaningful BFT property at
the contract layer.

---

## 8. Open questions / risks

1. **[Blocking] Contract-name mismatch (§0).** This design targets the real
   contracts (`stream`/`governance`/`archival-probe`). If the intent was to
   create new `multisig-threshold`, `rent-manager`, and `oracle-router` crates,
   this document is the wrong artifact and the work is substantially larger
   (three new crates, their ABIs, their tests, and integration with the existing
   factory). **Confirm the mapping before implementation.**
2. **`withdraw_fraction_as_admin` authorization surface.** Design reuses the
   existing stream-admin path (governance is set as admin). If the stream has no
   admin concept today beyond what `bulk_resume_streams_as_admin` uses, the
   exact auth wiring needs a read of the untruncated tail of
   `stream/src/lib.rs` before implementation.
3. **New `Error` variants.** §4.4 proposes appending `DivisionByZero` (= 35) and
   possibly `InvalidFraction`. Append-only is ABI-safe, but the exact
   discriminants and whether `InvalidFraction` is warranted vs reusing
   `InsufficientWithdrawable` is a review call.
4. **Footprint preloading (#440, #6.2-3).** The contract cannot declare its own
   transaction footprint; the preload is a caller/SDK pattern. If a purely
   on-chain instruction reduction is required, the single-pass load (#1) is the
   ceiling of what the contract alone can do, and the expected gain is ~halving
   deserialization, not eliminating it.
5. **`sweep_ttl` as a new public entry point** is an ABI addition (bumps
   `ABI_VERSION` only if it is *breaking*; a new method is additive per the
   `ABI_VERSION` comment in `lib.rs`, so no bump is needed). Confirm against
   `contracts/stream/abi/fluxora_stream.json`.
6. **No on-chain scheduler.** #426's "automated" is a keeper + spec, not a
   self-waking contract; this is a Stellar constraint, not a design choice.

---

## 9. Summary of concrete changes

| Issue | Contract | Change | Additive? |
|-------|----------|--------|-----------|
| #429 | `stream` | new `withdraw_fraction_as_admin`; reuse `apply_withdrawal` + `withdrawable` | new method (additive) |
| #429 | `stream` | append `Error::DivisionByZero` (+ optional `InvalidFraction`) | append-only |
| #429 | `governance` | new `CallData::StreamWithdrawFraction` + `dispatch_operation` arm | additive (name-keyed) |
| #440 | `stream` | single-pass load in `batch_withdraw`; skip zero-payout hold | internal, ABI-stable |
| #441 | `stream` (tests) | new `test/bft_partition.rs` scenario suite + fuzz | test-only |
| #426 | `stream` | new `sweep_ttl` keeper entry point | new method (additive) |
| #426 | off-chain | keeper spec + `archival-canary.sh` in CI | non-contract |
