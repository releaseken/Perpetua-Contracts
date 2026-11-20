# Design: rent-manager RBAC, multisig-threshold resilience, oracle-router invariants

Status: **Draft** — design for net-new contracts. The three subsystems named in
the issues (`rent-manager`, `multisig-threshold`, `oracle-router`) do **not**
exist in this repository yet. This document defines how they are added as
standalone Soroban crates under `contracts/`, following the conventions already
established by `contracts/stream`, `contracts/factory`, and `contracts/governance`.

Issues covered:

| Issue | Subsystem | Objective |
|---|---|---|
| #439 | `rent-manager` | Granular role-based authorization to prevent unauthorized configuration updates |
| #437 | `multisig-threshold` | Fuzzing coverage for edge-case timestamps near `u64::MAX` |
| #435 | `multisig-threshold` | Automated entry lifetime extension for archival resilience |
| #424 | `oracle-router` | Strict invariant assertions against re-entrancy and state desync |

Each issue requires an in-depth code review, defensive error handling with
checked arithmetic, and unit/fuzz tests. Because the contracts are new, the
"review" step is a review against this repo's existing contracts: the design
below is the output of that cross-contract review, and the implementation must
satisfy it.

---

## 1. Cross-cutting principles (inherited from existing contracts)

These are not new decisions. They are the conventions every contract in this
repo already follows, restated here so the three new subsystems do not diverge.

### 1.1 Standalone crate layout

Each new contract is its own Cargo project with its own `Cargo.toml` and
`Cargo.lock`, exactly like `contracts/stream`, `contracts/factory`,
`contracts/governance`, `contracts/archival-probe`. There is no shared
workspace; crates build, test, and release independently.

```
contracts/
  rent-manager/         Cargo.toml, Cargo.lock, src/{lib.rs,error.rs,storage.rs,events.rs,types.rs}, tests/
  multisig-threshold/  Cargo.toml, Cargo.lock, src/{lib.rs,error.rs,storage.rs,events.rs,types.rs}, tests/
  oracle-router/       Cargo.toml, Cargo.lock, src/{lib.rs,error.rs,storage.rs,events.rs,types.rs}, tests/
```

Build/test invocation matches the existing pattern:

```bash
(cd contracts/rent-manager        && cargo test)
(cd contracts/multisig-threshold  && cargo test)
(cd contracts/oracle-router       && cargo test)
```

Toolchain: `soroban-sdk` 22.x contract surface against protocol 27, Rust
1.97.1, target `wasm32v1-none` — identical to the existing crates.

### 1.2 Typed errors are ABI

`#[contracterror]`, `#[repr(u32)]`, discriminants start at 1. Discriminants are
part of the public ABI: **never renumber an existing variant; only append.**
Every numeric failure is a typed error; nothing panics on a numeric edge case.
All arithmetic is checked (`checked_add` / `checked_sub` / `checked_mul` /
`checked_div`) and maps to an `Overflow`/`ArithmeticOverflow` variant. This is
the pattern in `contracts/stream/src/error.rs` and `contracts/governance/src/lib.rs`.

### 1.3 Storage keys are a versioned `DataKey` enum

`#[contracttype] #[repr(u32)] enum DataKey { ... }` with explicit discriminants.
Discriminants are chosen deliberately and documented inline, because sharing a
code between two unit variants makes them the same storage slot — the governance
contract documents a real bug of exactly this kind (`SignerGeneration` vs
`NextProposalId`). New keys append a fresh, unused discriminant with a comment
explaining why it is not the "next free" number.

### 1.4 Authorization

Mutating entrypoints call `Address::require_auth()` on the authorized caller.
The contract never trusts a caller's self-reported identity; auth is enforced by
the Soroban host. Views never mutate and never extend TTL (matching
`contracts/stream`'s view policy — views are called through simulation where a
footprint write is noise).

### 1.5 Events

Audit-log events use `#[contracttype]` structs. **Topics** carry
indexer-friendly filter keys (event name, entity id, acting address); **data**
carries the full schema. `entity_id` is always the second topic element so a
single topic subscription reconstructs a full lifecycle. State mutation
precedes the event (CEI).

### 1.6 TTL / rent policy (the hard part)

Persistent entries archive when their ledger TTL runs out. Archived does not
mean lost — funds/balances remain — but the accounting entry is unreadable
until someone pays to restore it. Every contract here engineers around this
with the same three mechanisms, and the two new persistent-state contracts
(`rent-manager`, `multisig-threshold`) must follow them:

1. **Extend on every touch.** Every mutating call bumps the touched entry's TTL.
2. **Extend generously at creation**, targeting remaining lifetime + buffer,
   floored at a minimum, clamped to the network's `max_entry_ttl` (queried
   dynamically via `env.storage().max_ttl()`, never baked in — it can change on
   protocol upgrade).
3. **Permissionless `extend_*_ttl` / `batch_extend_ttl`.** Anyone may pay to keep
   an entry alive. Unauthenticated on purpose: a claim must never depend on the
   original owner's continued goodwill, and TTL extension cannot move funds or
   change state, so there is nothing to grief.

Constants and helpers are ported from `contracts/stream/src/storage.rs`:
`SECONDS_PER_LEDGER = 5` (nominal, conservative), `seconds_to_ledgers` rounds
**up** (ceiling) so a TTL target never undershoots, a `MIN_*_TTL_LEDGERS` floor
with a drift headroom multiplier, and a per-state retention table.

### 1.7 Re-entrancy and state desync

Soroban cross-contract calls are synchronous and the host reverts the whole
transaction on error, so classic re-entrancy is largely structural. The
existing contracts still defend explicitly:

- **CEI ordering** (checks-effects-interactions): all state mutations happen
  before any external call, and before the success event.
- **Instance-flag reentrancy guard**: an `Executing` instance entry is set on
  entry to a dispatching function and cleared on exit; a re-entrant call finds
  it set and returns a `ReentrancyGuard` error. This is the pattern in
  `contracts/governance` (issue #47).
- **Invariant assertions** at trust boundaries: after loading state and before
  committing state, assert invariants the contract claims to maintain. A
  violated invariant is a typed error, not a silent corruption.

---

## 2. `rent-manager` — granular role-based authorization (#439)

### 2.1 Problem

A rent manager is responsible for collecting and distributing Soroban
persistent-storage rent across the protocol's contracts (or across streams). It
holds configuration that is operationally sensitive — which contracts are
covered, the rent budget per contract, the keeper reward, the fallback
recipient for unrecoverable rent. If any caller can change this configuration,
an attacker can redirect rent, drain the keeper reward, or unregister a
covered contract so its entries silently archive.

### 2.2 Design: roles, not a single admin

A single admin is too coarse — it forces all configuration changes through one
key, which is both an operational bottleneck and a single point of compromise.
Instead, authorization is split into **roles**, each scoped to the operations
it needs. This mirrors how `contracts/governance` separates the admin (membership
management) from emergency guardians (cancellation) from signers (approval).

#### Roles

| Role | Key | Permitted operations | Denied |
|---|---|---|---|
| `Admin` | instance, singleton | grant/revoke roles, set `ConfigAdmin` | everything else, by convention |
| `ConfigAdmin` | instance, set | `set_covered_contracts`, `set_rent_budget`, `set_keeper_reward`, `set_fallback_recipient` | role management |
| `Keeper` | instance, set | `collect_rent`, `distribute_rent`, `extend_covered_ttl` | configuration |
| `Auditor` | instance, set | all views (read-only) | any mutation |

Roles are stored as a `Map<Address, Role>` in instance storage plus a
`Map<Address, bool>` membership index per role for O(1) checks, matching the
governance `SignerIndex` pattern. `Admin` is a singleton assigned at
initialization; the initial admin is the initializer's address (the deployer),
rotatable via `set_admin` exactly as in `governance` and `factory`.

#### Authorization enforcement

Every mutating entrypoint begins with a `require_role(env, caller, Role::X)`
helper that calls `caller.require_auth()` **and** verifies the caller holds the
required role. The role check is what makes authorization *granular*: a caller
who is a `Keeper` cannot call `set_rent_budget` even though they are a known
principal, because `set_rent_budget` requires `ConfigAdmin`.

Configuration-update entrypoints (`set_*`) all require `ConfigAdmin`. This is
the literal resolution of #439: configuration updates are unauthorized for
everyone who is not `ConfigAdmin`, and `Keeper`/`Auditor`/unauthenticated
callers are rejected with `Unauthorized`.

#### Role management

- `grant_role(addr, role)` / `revoke_role(addr, role)`: require `Admin`.
- `set_admin(new_admin)`: require `Admin`, emits `AdminChanged { old, new }`.
- A role can be granted to an address that already holds a *different* role
  only by first revoking the old one (a principal holds exactly one role),
  enforced so that an address cannot silently accumulate privileges. This is
  stronger than governance's signer set and is the right choice for a contract
  that moves value.

#### Defensiveness

- `set_covered_contracts(Vec<Address>)` rejects an empty list and duplicates,
  and bounds the list (`MAX_COVERED_CONTRACTS`, derived from footprint budget as
  `MAX_BATCH_SIZE = 16` is derived from the event budget in `stream`).
- `set_rent_budget(i128)`, `set_keeper_reward(i128)` reject zero/negative and
  require `keeper_reward <= rent_budget` (a keeper cannot be promised more than
  the budget collects), checked with `checked_add` when summing across covered
  contracts.
- `set_fallback_recipient(Address)` rejects the zero address and the contract's
  own address.

### 2.3 Errors (append-only, `#[repr(u32)]`)

```
NotInitialized = 1
AlreadyInitialized = 2
Unauthorized = 3          // caller lacks the required role
RoleNotFound = 4
DuplicateRole = 5         // address already holds a different role
InvalidConfig = 6         // empty list, duplicate, zero/negative value, etc.
CoveredContractsTooMany = 7
ArithmeticOverflow = 8
CoveredContractNotFound = 9
InsufficientRent = 10
KeeperRewardExceedsBudget = 11
InvalidAddress = 12
ReentrancyGuard = 13
```

### 2.4 Storage (`DataKey`)

```
Admin = 0
ConfigAdmin = 1              // set of ConfigAdmin addresses
Keeper = 2                    // set of Keeper addresses
Auditor = 3
Roles = 4                     // Map<Address, Role> — authoritative role assignment
CoveredContracts = 5          // Vec<Address>
RentBudget = 6
KeeperReward = 7
FallbackRecipient = 8
RentLedger(contract_id) = 9   // per-contract rent accumulator (persistent)
Executing = 10                 // reentrancy guard (instance)
```

`CoveredContracts`, `RentBudget`, `KeeperReward`, `FallbackRecipient` live in
instance storage (small, frequently read, config). Per-contract rent
accumulators live in persistent storage keyed by contract address and follow
the §1.6 TTL policy.

### 2.5 Tests

Unit tests (`src/test/`):

- `grant_role`/`revoke_role` enforce `Admin`; non-admin rejected with `Unauthorized`.
- Each `set_*` rejects callers without `ConfigAdmin`.
- `Keeper` can `collect_rent`/`distribute_rent` but cannot `set_*`.
- `Auditor` can read all views but every mutating call is rejected.
- `DuplicateRole` when granting a role to an address holding another role.
- `KeeperRewardExceedsBudget` and `InvalidConfig` paths.
- `set_admin` rotates and emits `AdminChanged`; old admin loses access.
- Reentrancy guard: a `collect_rent` that re-enters `collect_rent` aborts with
  `ReentrancyGuard` (via a mock that calls back).

Fuzz tests (see §5): role-assignment sequences never grant a principal two
roles; every configuration reachable through valid `set_*` calls satisfies
`keeper_reward <= rent_budget` and non-empty `CoveredContracts`.

---

## 3. `multisig-threshold` — archival resilience + `u64` boundary fuzzing (#435, #437)

`multisig-threshold` is a threshold multisig: an operation is authorized once a
threshold of signers approve it, with no timelock (unlike `governance`, which is
the timelocked policy multisig). It is the closest analogue to
`contracts/governance`, and the design reuses governance's data model
approach — `Proposal`, `ProposalStatus`, signer set with weights, signer
generation, approval index — adapted to threshold-only semantics.

### 3.1 #435: automated entry lifetime extension (archival resilience)

The threat: a multisig entry whose TTL elapses archives, so the threshold of
approvals collected so far is unreadable until someone restores the entry — and
because there is no timelock, an archived entry can block an otherwise-complete
authorization indefinitely. The fix is the same retention model
`contracts/stream` and `contracts/governance` already use, ported to this
contract's entries.

#### State-aware retention

| Proposal state | Rent target on any touch | Why |
|---|---|---|
| `Proposed` / `Approved` (active, below threshold) | remaining executable lifetime + buffer, floored at `MIN_PROPOSAL_TTL_LEDGERS` | an active proposal must not archive before it reaches threshold, however long signers wait between approvals |
| `ThresholdMet` (executable, not yet executed) | `MIN_PROPOSAL_TTL_LEDGERS` floor | a met-threshold proposal is ready to execute; keep it readable long enough for an executor to act, then let it decay |
| `Executed` / `Cancelled` (terminal) | one-shot floor granted at transition, never topped up | terminal records are reconciled by indexers against their events, then left to decay; topping them up forever is the unbounded-rent growth this policy prevents |

A proposal owns three persistent entries (record, approval-index, signer-generation
snapshot). They are read and written together and share a lifetime, so they
share one policy — keeping the record alive while the index decays would
corrupt the duplicate-approval check or the threshold gate.

#### Mechanisms (ported from `contracts/stream/src/storage.rs`)

1. **Extend on every touch.** `approve`, `execute`, `cancel` each call
   `bump_proposal_retention` on the three entries they touched.
2. **Extend generously at creation.** `propose` grants the active-proposal bump
   (max proposal age + headroom, clamped to `max_entry_ttl`).
3. **Permissionless `extend_proposal_ttl(id)` and `batch_extend_ttl(ids)`.**
   Anyone may pay to keep a proposal alive; the calls only extend TTL, never
   touch state, so there is nothing to grief. The instance entry (proposal id
   counter) is always pinned to `max_entry_ttl`, so the counter can never archive
   before the proposals it issued (the ordering guarantee from `stream`).

Constants ported verbatim with renamed prefixes: `SECONDS_PER_LEDGER = 5`,
`seconds_to_ledgers` (ceiling), `MIN_PROPOSAL_TTL_LEDGERS` with the 10% drift
multiplier, `max_entry_ttl` queried at call time. `ledgers(seconds)` saturates
rather than wraps, so a misconfigured constant clamps instead of becoming a
negative-length window.

#### Why this resolves #435

An actively-used proposal cannot archive while it is still actionable (extend
on touch + generous creation bump). A proposal whose signers go quiet is kept
readable by any passer-by (permissionless extend). A terminal proposal stops
consuming rent (one-shot floor, no further top-ups). Archival resilience is the
combination of the three, and the tests below prove each leg.

### 3.2 #437: fuzzing edge-case timestamps near `u64::MAX`

The threshold multisig computes several timestamps:

- `created_at` = ledger timestamp at proposal submission.
- `max_executable_at = created_at.checked_add(MAX_PROPOSAL_AGE_SECONDS)?` — the
  window after which `approve`/`execute` reject with `ProposalExpired`.
- approval ordering: approvals are time-stamped; the *last* approval that
  crosses the threshold sets `threshold_met_at`, recorded for audit.
- `execute` may apply the operation at the current ledger timestamp.

Near `u64::MAX`, every one of these `checked_add`/`checked_sub` paths is a live
overflow surface. The existing `governance` contract uses `checked_add` and
returns `ArithmeticOverflow`; the design requirement here is *coverage* — a fuzz
harness that drives these paths with timestamps at and near `u64::MAX` and
asserts that the contract never panics, wraps, or accepts an impossible
deadline.

#### Fuzz targets

1. **`fuzz_proposal_age_near_u64_max`**: seed `created_at` in
   `[u64::MAX - 2*MAX_PROPOSAL_AGE_SECONDS, u64::MAX]`. Submit proposals, drive
   approvals across the boundary. Assert: no panic; every computed
   `max_executable_at` either equals `created_at + MAX_PROPOSAL_AGE_SECONDS` or
   the call returns `ArithmeticOverflow`; an overflowed deadline is never stored.
2. **`fuzz_threshold_met_timestamp`**: drive the final approval that crosses
   threshold at ledger timestamps near `u64::MAX`. Assert `threshold_met_at` is
   either the ledger timestamp or the call rejects with `ArithmeticOverflow`,
   never a wrapped value.
3. **`fuzz_ttl_target_near_u64_max`**: with `end_time`-equivalent fields near
   `u64::MAX`, assert `ttl_target_ledgers` saturates at `u32::MAX` and then
   clamps to `max_entry_ttl`, never wraps to a tiny TTL (which would let the
   entry archive immediately).
4. **`fuzz_checked_sub_remaining`**: `remaining = end.saturating_sub(now)` must
   never underflow; the saturating choice is intentional and documented
   (mirrors `contracts/stream/src/storage.rs`).

#### Harness choice

Two complementary layers, matching how `contracts/stream` already does property
testing (env vars `FLUXORA_FUZZ_SEEDS`, `FLUXORA_FUZZ_STEPS`, `PROPTEST_CASES`):

- **In-crate `proptest`** for deterministic, CI-friendly shrinking — the
  boundary inputs shrink cleanly, which is exactly what makes proptest valuable
  here.
- **`cargo +nightly fuzz`** targets for long unstructured runs (nightly CI),
  as the `stream` contract does randomized sweeps.

Both reuse a pure helper layer (timestamp arithmetic extracted into a
`#[inline] fn` with no `Env`, like `contracts/stream/src/accrual.rs`) so a fuzz
case costs microseconds instead of a host invocation.

### 3.3 Errors (append-only)

Reuse governance's discriminant plan, adapted:

```
NotInitialized = 1
AlreadyInitialized = 2
Unauthorized = 3
NotASigner = 4
ProposalNotFound = 5
AlreadyExecuted = 6
ThresholdNotReached = 7
AlreadyApproved = 8
ProposalExpired = 9
ProposalCancelled = 10
NotProposerOrAdmin = 11
InvalidThreshold = 12
ThresholdWouldBreak = 13       // removing a signer drops signers below threshold
DuplicateSigner = 14
ArithmeticOverflow = 15        // checked-add on timestamps/deadlines
InvalidSignerGeneration = 16
ReentrancyGuard = 17
BatchTooLarge = 18
```

### 3.4 Storage (`DataKey`)

```
Admin = 0
Signers = 1
Threshold = 2
NextProposalId = 3
Proposal(id) = 4              // persistent
SignerIndex = 5
ProposalApprovalIdx(id) = 6  // persistent, duplicate-approval detection
SignerGeneration = 7         // NOT 3 — collides with NextProposalId (see governance)
Executing = 8                 // reentrancy guard
SignerWeights = 9
```

The `SignerGeneration` discriminant is set deliberately **not** to the implicit
"next free" number, exactly as `contracts/governance` documents: `3` is
`NextProposalId`, and two unit variants sharing a code collapse to one slot.

### 3.5 Tests

Unit + property, in `src/test/` and `tests/`:

- Threshold semantics: exactly `threshold` approvals authorize; `threshold-1`
  does not.
- `InvalidThreshold` for zero or `> signer_count`; `ThresholdWouldBreak` when
  removing a signer would drop below threshold.
- `ProposalExpired` after `created_at + MAX_PROPOSAL_AGE_SECONDS`.
- Signer-generation invalidation: a proposal under an old generation is
  rejected with `InvalidSignerGeneration`.
- Retention (#435): a proposal kept only by keeper sweeps survives a
  multi-window lifetime; terminal proposals get a one-shot floor and decay;
  the instance counter never archives before its proposals (port
  `test::ttl` from `stream`).
- Fuzz (#437): the four targets above, run under both `proptest` and
  `cargo +nightly fuzz`, asserting no panic/wrap/accept-impossible-deadline.

The known caveat from `docs/KNOWN-LIMITATIONS.md §1` applies: the SDK test host
auto-restores expired entries in recording mode, so these tests prove the
*rent arithmetic and extend-on-touch behavior*, not the live-network
restore-fail-then-resubmit flow. That distinction is stated in the test module
docs, as it is in `stream`.

---

## 4. `oracle-router` — strict invariant assertions (#424)

### 4.1 Problem

An oracle router aggregates prices from a whitelist of upstream oracle
contracts and exposes a single `price(asset) -> i128` (and a decimal-scale
`decimals`) to consumers. Two classes of failure:

1. **State desync**: the router's cached aggregate disagrees with the oracle
   inputs it was derived from — e.g. a partial update left the aggregate stale
   while one oracle was marked fresh, or a decimal-rescaling rounded in a
   direction that broke the conservation `aggregate == f(inputs)`.
2. **Re-entrancy**: an upstream oracle callback that re-enters the router during
   an update could observe a half-updated whitelist or aggregate and corrupt
   state or double-count an update.

### 4.2 Design: invariant assertions at trust boundaries

The contract defines a small set of invariants it claims to maintain, and
asserts each at the point it could be violated: after loading oracle state,
before committing an aggregate, and before returning a price. A violated
invariant returns a typed error and reverts (Soroban reverts the whole
transaction, so no partial state is committed).

#### Invariants

- **I1 — Whitelist integrity**: every oracle used in an aggregate is in the
  whitelist and not disabled. Asserted before `update_aggregate` and before
  `price`.
- **I2 — Freshness**: the aggregate's `updated_at` is `>=` every contributing
  oracle's `updated_at`, and `<=` the current ledger timestamp. A stale
  aggregate (older than `MAX_STALENESS_SECONDS`) returns `StalePrice` and is
  never silently served.
- **I3 — Conservation**: `aggregate_price == aggregate_fn(oracle_prices)` for
  the configured aggregation (median by default). Recomputed from the stored
  inputs immediately before the aggregate is committed and before it is served.
  A mismatch returns `AggregateDesync` and reverts — this is the literal
  resolution of "state desync."
- **I4 — Decimal scaling**: `price` returns a value normalized to the router's
  declared `decimals`. Each oracle's `decimals` is read and the rescaling uses
  checked `checked_mul`/`checked_div`; a scaling that would overflow returns
  `ArithmeticOverflow`, never a truncated value.
- **I5 — Monotonic id**: every update increments an `update_id` with
  `checked_add`; the aggregate record carries the `update_id` it was computed
  from, and `price` asserts the served aggregate's `update_id` equals the latest
  — serving a stale `update_id` returns `AggregateDesync`.

Each invariant maps to a typed error so a consumer can distinguish stale price
from a corrupted aggregate from a missing oracle.

### 4.3 Re-entrancy defense

- **CEI**: `update_aggregate` loads all oracle prices (effects-input), computes
  the aggregate, writes the new aggregate and `update_id`, then emits
  `AggregateUpdated`. No external call follows the writes; the only external
  calls (to upstream oracles) precede the writes.
- **Instance-flag guard**: `Executing` instance entry set on entry to
  `update_aggregate`, cleared on exit. A re-entrant `update_aggregate` (an
  upstream oracle that calls back into the router) finds the flag set and
  returns `ReentrancyGuard`. This is the `governance` #47 pattern.
- **Whitelist mutation is not callable from an oracle callback path**: only
  `set_oracles` (an admin/config call) mutates the whitelist, and it cannot run
  while `Executing` is set, so a re-entrant update cannot enlarge the oracle set
  mid-aggregation.

### 4.4 Errors (append-only)

```
NotInitialized = 1
AlreadyInitialized = 2
Unauthorized = 3
OracleNotFound = 4
OracleNotWhitelisted = 5
OracleDisabled = 6
StalePrice = 7              // aggregate older than MAX_STALENESS_SECONDS
AggregateDesync = 8          // I3/I5: recomputed aggregate != stored, or update_id mismatch
NoOraclesConfigured = 9
InsufficientOracles = 10     // fewer than min_oracles for a valid aggregate
ArithmeticOverflow = 11      // I4 decimal scaling overflow
InvalidDecimals = 12
InvalidConfig = 13
ReentrancyGuard = 14
InvalidAddress = 15
```

### 4.5 Storage (`DataKey`)

```
Admin = 0
Oracles = 1                  // Vec<Address> whitelist
OracleStatus(addr) = 2       // persistent: enabled/disabled + last_updated_at
Aggregate = 3                // instance: {price, decimals, updated_at, update_id}
Decimals = 4                 // router's declared decimals
MinOracles = 5
MaxStalenessSeconds = 6
UpdateId = 7                 // monotonic, checked_add
Executing = 8                 // reentrancy guard (instance)
```

`Aggregate` and `UpdateId` are small and read on every `price`, so they live in
instance storage. Per-oracle status is persistent and follows the §1.6 TTL
policy (extend on touch + permissionless `extend_oracle_ttl`).

### 4.6 Tests

Unit + fuzz:

- **I1**: `price` with a disabled or un-whitelisted oracle in the contributing
  set returns `OracleNotWhitelisted`/`OracleDisabled`.
- **I2**: an aggregate older than `MAX_STALENESS_SECONDS` returns `StalePrice`;
  a fresh one serves. Timestamps near `u64::MAX` for `updated_at` do not wrap
  the staleness check (`checked_sub` → `ArithmeticOverflow`).
- **I3**: deliberately corrupt the stored aggregate (via a test-only setter)
  and assert `price` returns `AggregateDesync` and reverts; `update_aggregate`
  re-derives and the next `price` succeeds.
- **I4**: decimal rescaling with oracle decimals far from router decimals
  overflows → `ArithmeticOverflow`, never truncation.
- **I5**: serving an aggregate whose `update_id` != latest returns
  `AggregateDesync`.
- **Re-entrancy**: an upstream oracle mock that calls back into
  `update_aggregate` triggers `ReentrancyGuard` and the outer call reverts
  cleanly with no partial whitelist/aggregate change.
- **Fuzz**: randomized oracle price sequences (including extreme `i128` values
  and mismatched decimals) assert that every served price satisfies I2, I3, I4,
  I5 and that no sequence causes a panic or a non-typed abort.

---

## 5. Testing strategy (cross-cutting)

All three contracts use the repo's established testing approach:

- **Unit tests** in `src/test/` (or `tests/`), staged by build order as in
  `contracts/stream/src/test/`.
- **Property tests** via `proptest`, driven by the same env vars the suite
  already uses (`PROPTEST_CASES`, `FLUXORA_FUZZ_SEEDS`, `FLUXORA_FUZZ_STEPS`).
- **`cargo +nightly fuzz`** targets for long unstructured runs, run in nightly
  CI as `contracts/stream` already is.
- **Pure helper layer**: timestamp arithmetic, aggregate computation, and role
  resolution are extracted into `#[inline] fn`s with no `Env` (the
  `contracts/stream/src/accrual.rs` discipline), so a fuzz case is microseconds,
  not a host invocation, and the math is auditable in one place.

Every contract's test module documents the `KNOWN-LIMITATIONS.md §1` caveat:
the SDK test host auto-restores archived entries in recording mode, so TTL
tests prove the rent arithmetic and extend-on-touch behavior, not the
live-network restore-fail-then-resubmit flow. That half is closed against
testnet separately (the `archival-probe` pattern).

---

## 6. Out of scope / open questions

1. **Are these contracts actually wanted in this repo?** They are new
   subsystems, not hardening of existing ones. If the intent was to harden the
   closest existing analogues instead, the work maps to: `governance` for the
   two `multisig-threshold` issues, `factory` for the `rent-manager` RBAC issue.
   Confirm before implementation proceeds against new crates.
2. **`rent-manager` value flow**: this design covers authorization and
   configuration. Whether rent is *collected as tokens* (a SEP-41 transfer) or
   *accounted as ledger rent* (no token movement, only ledger-entry TTL) is a
   product decision that changes the `collect_rent`/`distribute_rent` surface.
   The RBAC design is independent of it, but the tests are not.
3. **`oracle-router` aggregation**: median is the default; a configurable
   aggregation function (mean with outlier rejection, etc.) is out of scope
   unless requested. The invariant-assertion design accommodates either, since
   I3 recomputes against whatever `aggregate_fn` is configured.
4. **Deployment naming**: following `fluxora_<name>.wasm`, the wasms would be
   `fluxora_rent_manager.wasm`, `fluxora_multisig_threshold.wasm`,
   `fluxora_oracle_router.wasm`. `script/release.sh` currently builds only the
   product (`fluxora_stream`); releasing these would extend it.

---

## 7. Acceptance criteria

- [ ] Each new contract is a standalone Cargo crate under `contracts/` and
      builds/tests independently: `cargo test` and
      `cargo build --target wasm32v1-none --release`.
- [ ] `rent-manager`: every `set_*` configuration entrypoint rejects non-
      `ConfigAdmin` callers with `Unauthorized`; unit tests cover all four roles
      and the cross-role rejection matrix.
- [ ] `multisig-threshold`: a proposal survives a multi-TTL-window lifetime on
      keeper sweeps alone (#435); terminal proposals decay after a one-shot
      floor; the instance counter never archives before its proposals.
- [ ] `multisig-threshold`: the four `u64::MAX` fuzz targets pass under both
      `proptest` and `cargo +nightly fuzz` with no panic, wrap, or accepted
      impossible deadline (#437).
- [ ] `oracle-router`: invariants I1–I5 are each asserted at their trust
      boundary and each maps to a typed error; a deliberately desynced
      aggregate returns `AggregateDesync` and reverts; the re-entrancy mock
      returns `ReentrancyGuard` with no partial state (#424).
- [ ] All arithmetic is checked; every numeric failure is a typed error; no
      `unwrap`/`expect`/`panic` on numeric paths.
- [ ] Error discriminants are append-only `#[repr(u32)]` starting at 1; `DataKey`
      discriminants are explicit, documented, and collision-free.

---

# Design — multisig-threshold hardening (#434, #432, #430) and oracle-router (#423)

Status: **Draft** · Date: 2026-11-19 · Author: Kiro
Related issues: #434 (refactor error types), #432 (Byzantine fault-tolerance tests),
#430 (granular RBAC), #423 (oracle-router BFT tests).

---

## 0. TL;DR

- The four issues name contracts that **do not exist at the paths they cite**.
  `contracts/multisig-threshold` and `contracts/oracle-router` are absent on both
  `main` and `origin/issues-98-102-107-101-security-docs-sim`, confirmed by
  directory listing, recursive filename search, and `git ls-tree`.
- `contracts/governance` **is** the threshold multisig the "multisig-threshold"
  issues describe (README: "timelocked multi-sig"; code: `Threshold`,
  `MAX_SIGNERS`, quorum, timelock, emergency guardians). Issues #434/#432/#430
  are mapped onto `governance`.
- `oracle-router` (#423) has **no analog** in this repository and is **out of
  scope** for this design. It would require a net-new contract, which is a
  separate decision (see §6).
- This document is design-only, per the user's direction. Implementation and PR
  submission are intentionally not included here.

## 1. Findings (read before reviewing the rest)

### 1.1 The named targets are absent

`contracts/` contains only:

```
archival-probe   factory   governance   stream   stream-nft
```

No `multisig*` or `oracle*` path exists anywhere in the tree (recursive `find`,
both branches). The README's repository-layout section enumerates the same five
contracts and its non-goals list nothing resembling a multisig-threshold module
or an oracle router. So the issue paths are not a rename hiding in another
module; the contracts simply are not part of Perpetua-Contracts.

### 1.2 `multisig-threshold` ≅ `governance`

`contracts/governance` (`fluxora-governance`, v1.0.0) is a timelocked threshold
multisig for protocol parameters. The mapping is exact on every axis the issues
imply:

| Issue concept | Governance implementation |
|---|---|
| threshold multisig | `DataKey::Threshold`, `1 <= threshold <= signers.len()`, `MAX_SIGNERS = 20` |
| co-signers | `DataKey::Signers`, `DataKey::SignerIndex` (O(1) membership), per-signer `SignerWeights` (#48) |
| proposal lifecycle | `ProposalStatus` {Proposed, Approved, Queued, Executed, Cancelled}; `propose`/`approve`/`execute`/`cancel_proposal` |
| timelock | `GOVERNANCE_TIMELOCK_SECONDS = 172_800` (48h); `eta = quorum_reached_at + timelock` |
| grace / expiry | `DEFAULT_GRACE_PERIOD_SECONDS = 2_419_200`; `MAX_PROPOSAL_AGE_SECONDS = 2_592_000` |
| emergency cancel | `DataKey::EmergencyGuardians` (bounded by `MAX_SIGNERS`) |
| batch proposals | `CallData::Batch(Vec<ExecutionCall>)`, `MAX_BATCH_CALLS = 5` (#51) |
| reentrancy guard | `DataKey::Executing` + CEI in `execute` |
| retention | #52 policy: active vs terminal proposal storage TTL |

**Decision:** treat #434, #432, #430 as applying to `contracts/governance`.
This is an assumption that should be confirmed before implementation, but it is
the only interpretation consistent with the issues' "refactor"/"code review"
wording (a non-existent contract cannot be refactored or reviewed).

### 1.3 `oracle-router` has no home

Nothing in the repo reads external prices, aggregates feeds, or routes
off-chain data. The factory is a policy gate (cap/duration/rate/allowlist/pause);
the stream is the product; governance is parameter tuning. #423 cannot be
mapped to existing code. See §6.

### 1.4 The issues are templated

All four share boilerplate ("contract security, gas efficiency, and robustness
in the [X] subsystem", "Soroban SDK best practices, ledger footprint
optimization, and formal verification standards", and an identical Requirements
block). Combined with §1.1, this is consistent with auto-generated issues. The
design below treats them as a *brief* and fills in the specifics the brief omits;
it does not assume the issues' wording is precise.

## 2. Scope

**In scope** (this design):

- #434 — modularize/clarify error types + custom error messages for client
  observability, applied to `governance`.
- #430 — granular role-based authorization to prevent unauthorized configuration
  updates, applied to `governance`.
- #432 — Byzantine fault-tolerance test scenarios under simulated network
  partitions, applied to `governance`.

**Out of scope:**

- #423 (oracle-router). No target contract exists. §6.
- Any change to `stream`, `factory`, `archival-probe`, or `stream-nft`.
- New deployable contracts, upgrade paths, or fee mechanisms (README non-goals).
- Changing existing `GovernanceError` discriminant values — they are ABI.
- Submitting a PR (paused per user direction; see §8 for the blocker anyway).

## 3. Constraints that bound every option below

1. **Error discriminants are ABI.** `contracts/stream/src/error.rs` states the
   idiom explicitly: *"Discriminants are part of the public ABI. Never renumber
   an existing variant; only append."* `GovernanceError` follows the same idiom
   (`#[contracterror] #[repr(u32)]` with explicit codes 1–27, including a
   *documented* renumbering of `InvalidProposalState` off `21` to `26` because
   `21` collided with `TimelockNotMet`). Any "modularize" change must be
   **append-only and non-renumbering**. Splitting `GovernanceError` into
   sub-enums is ABI-breaking and is rejected.
2. **The repo's error idiom is a single flat, section-commented enum**, not a
   hierarchy. "Modularize" in this codebase means *logical sectioning with clear
   doc comments* — which `GovernanceError` already largely has. The real
   observability gap is runtime messages, not enum shape (see §4.1).
3. **Soroban has no scheduler and the SDK test host has no network.** "Network
   partition" cannot be simulated at a transport layer; it must be modeled as
   *signer availability across ledgers* (§5).
4. **Immutability is a product guarantee** (README: no admin key, no upgrade
   path for the product). `governance` is the *config* layer, so it intentionally
   *does* have an admin — but adding RBAC must not introduce a backdoor that
   bypasses the timelock/threshold proposal flow for parameter changes.
5. **Gas/footprint discipline.** Per-call cost is a first-class concern
   (governance already documents `MAX_PAGE_SIZE = 100` with a DoS rationale).
   New storage keys or per-call scans must be bounded and motivated.

## 4. Design: #434 — error types and client observability

### 4.1 What "modularize" means here (and what it does not)

The issue asks to "modularize error types and custom error messages for enhanced
client observability." Two readings; only one is safe:

- **(rejected) Split `GovernanceError` into sub-enums** (`AuthError`,
  `ProposalError`, `ConfigError`, …). Breaks Constraint 1: it renumbers the
  discriminant space that indexers and the TypeScript SDK already decode against.
  The stream contract — the project's reference for error design — deliberately
  keeps a *single* flat enum and forbids renumbering. Governance should match.
- **(accepted) Keep one flat enum; sharpen the grouping and the messages.**
  `GovernanceError` is already ordered logically (init → auth → proposal →
  calldata → arithmetic → batch). The actionable work is **observability**: a
   client that calls a view and gets back discriminant `16` should be able to
   resolve it to a human-readable name *without* the contract ABI in hand.

Note: `#[contracterror]` already emits variant names into the contract spec JSON,
so SDK clients that ingest the ABI *can* already map code→name. The marginal
value of a runtime catalog is for dashboards, explorers, and ad-hoc
`soroban-sdk` simulations that do not load the ABI, plus a single source of
truth for the human-readable string.

### 4.2 Recommended change (additive, ABI-safe)

1. **Do not alter `GovernanceError` discriminants.** Optionally append new
   variants only if #430/#432 require them (e.g., a role error) — appended at
   the end (28+), never inserted.
2. **Add a read-only error catalog.** Two viable shapes; pick one:

   a. `pub fn error_name(error: GovernanceError) -> Symbol` — maps each variant
      to a short `Symbol` (e.g. `Unauthorized -> "Unauthorized"`). Cheapest in
      footprint (a `match`, no storage). The return is a `Symbol`, which is
      what the host already serializes cheaply and what the SDK decodes as a
      string.

   b. A `#[contracttype]` `ErrorEntry { code: u32, name: Symbol, category:
      Symbol }` plus a `pub fn error_catalog() -> Vec<ErrorEntry>` returning the
      full table in one call. Larger event/return budget but lets a dashboard
      enumerate every code in a single simulation. Bounded: 27 entries today.

   Recommendation: **(a)** as the primary, with the catalog exposed as a
   `Vec<ErrorEntry>` view only if a concrete client requests enumeration. (a)
   is the smaller blast radius and matches "client observability" for the
   common case (decode one error). Add `category` to the `Symbol`-returning form
   by returning e.g. `"Auth/Unauthorized"` so a client gets grouping for free
   without a second type.

3. **Group the existing variants with section comments** mirroring `stream`'s
   idiom (`// --- Auth ---`, `// --- Proposal lifecycle ---`, `// --- Calldata
   ---`, `// --- Arithmetic ---`, `// --- Batch ---`). This is the "modularize"
   half that is genuinely a no-op on the wire but improves reviewability and
   matches the repo's own pattern. It touches only comments.

4. **Keep `ArithmeticOverflow` as an alias** the way `stream` does
   (`pub const ArithmeticOverflow: Error = Error::Overflow;`). Governance
   already names its variant `ArithmeticOverflow` directly, so no alias is
   needed — called out only so the implementer does not "fix" this
   inconsistently.

### 4.3 Test plan (#434)

- A test that, for every variant in `GovernanceError`, `error_name(v)` returns a
  non-empty `Symbol` (exhaustive over the enum — a `match` with no `_` arm forces
  the compiler to keep this exhaustive when variants are appended later).
- A round-trip test: each code round-trips through `error_name` and a future
  `error_from_name` inverse (if (b) is chosen) without collision.
- A "no discriminant changed" guard test: assert the numeric value of a sample
  of variants (e.g. `Unauthorized = 3`, `BatchTooLarge = 27`) equals the
  documented constant, so a future renumbering fails CI. Mirrors the spirit of
  `stream`'s ABI-freeze comment.

## 5. Design: #432 — Byzantine fault-tolerance tests

### 5.1 What "network partition" can mean on Soroban

There is no network layer in the SDK test host, and a contract cannot observe
one on chain. A partition is modeled purely as **which signers can submit
authenticated transactions in a given ledger window** — i.e., which
`require_auth` calls succeed. The contract's safety properties under a partition
reduce to properties of quorum, snapshots, expiry, and the reentrancy guard that
already exist in code. So #432 is really: *targeted tests that exercise those
properties under signer-subset availability*, not a transport simulation.

### 5.2 Mapped BFT concerns → existing mechanisms

| Byzantine concern | Existing mechanism | What the test must show |
|---|---|---|
| Double-vote / equivocation | per-proposal `ProposalApprovalIdx` → `AlreadyApproved` | a signer approving twice is rejected; quorum is never reached by replaying one signer |
| Signer unavailability (partition) | `MAX_PROPOSAL_AGE_SECONDS` → `ProposalExpired` | a proposal that cannot reach quorum before max-age expires, not hangs |
| Quorum reached in a partition minority | `Threshold` is a count, not a ratio — minority < threshold cannot queue | a 2-of-3 set split 1|2 cannot queue from the 1 side |
| Admin censoring via threshold bump | `QuorumInfo::threshold` snapshot at quorum; `SignerGeneration` invalidates stale proposals | an admin `set_threshold` after quorum does not retro-actively block or enable an in-flight proposal |
| Emergency guardian over-reach | `EmergencyGuardians` is a fixed, bounded set; cancel is the only power | a non-guardian cannot cancel; a guardian cannot execute or alter config |
| Reentrant execution during partition heal | `DataKey::Executing` + CEI in `execute` | a target that calls back into `execute` is rejected with `ReentrancyGuard` |
| Timelock bypass on heal | `eta` + `TimelockNotMet` | executing immediately after quorum (before `eta`) is rejected even if the partition just healed |
| Grace-window expiry | `DEFAULT_GRACE_PERIOD_SECONDS` | a proposal past `eta + grace` returns `ProposalExpired`/`InvalidProposalState` on execute |

### 5.3 Recommended test module

Add `#[cfg(test)] mod test_byzantine` inside `contracts/governance/src/lib.rs`
(the crate keeps all tests inline — `contracts/governance/src/` has only
`lib.rs`). Scenarios, each named for the property:

1. `partition_minority_cannot_queue` — 3 signers, threshold 2; only the isolated
   signer approves; assert status stays `Proposed`/`Approved`, never `Queued`.
2. `partition_quorum_then_heal_executes` — quorum reached on the majority side,
   timelock elapses, the isolated signer rejoins and executes; assert success
   and exactly one `proposal_executed` event.
3. `no_quorum_proposal_expires_under_partition` — quorum never reached; ledger
   advanced past `MAX_PROPOSAL_AGE_SECONDS`; `approve` and `execute` both return
   `ProposalExpired`; no state transition to `Queued`.
4. `equivocation_rejected` — same signer approves twice; second returns
   `AlreadyApproved`; weight counted once.
5. `threshold_change_mid_flight_does_not_alter_quorum` — proposal reaches
   quorum at threshold 2; admin bumps to 3; `execute` still succeeds using the
   `QuorumInfo::threshold` snapshot.
6. `signer_generation_invalidates_stale_proposals` — proposal created under
   generation G; admin adds/removes a signer (bumps to G+1); behavior of the
   stale proposal follows the existing `InvalidSignerGeneration` semantics.
7. `emergency_guardian_cancel_during_partition` — majority unreachable, proposal
   stuck; a guardian cancels; assert `Cancelled`, terminal retention applied, no
   later approve/execute.
8. `reentrant_execute_rejected` — a malicious target that re-enters `execute`
   (same or other proposal) returns `ReentrancyGuard`; original dispatch unwinds.
9. `timelock_not_met_on_heal` — quorum reached, partition heals, `execute`
   called before `eta` → `TimelockNotMet`; called after → success.
10. `grace_window_expiry` — `execute` after `eta + grace` →
    `ProposalExpired`/`InvalidProposalState`.

The partition itself is modeled with the SDK auth testutils: a signer "in the
partition" is one whose `require_auth` is simply not invoked that ledger (we
advance `env.ledger().set_timestamp`) and/or whose mock auth is deliberately
withheld for the duration, then restored at heal. No new contract code is
required for #432 unless a scenario reveals a gap — which is the point.

### 5.4 Test plan summary

10 scenarios above; each asserts both the *safety* outcome (no unauthorized
state change, no double execution) and the *liveness* outcome (correct proposals
still execute once the partition heals within the grace window). All gated
behind `#[cfg(test)]` with `testutils`.

## 6. Design: #430 — granular role-based authorization

### 6.1 Current authorization model

- **Admin** (single `Address`, `DataKey::Admin`): all config — `set_admin`,
  `set_threshold`/`update_threshold`, `add_signer`, `remove_signer`,
  `set_vote_weight`, `set_emergency_guardians`, `set_grace_period`.
- **Signers** (`DataKey::Signers`, weighted): `propose`, `approve`.
- **Emergency guardians** (`DataKey::EmergencyGuardians`): `cancel_proposal`
  only.
- **Executor**: any address (`require_auth` but no role check) — execution is
  gated by quorum + timelock, not by who calls it.

So configuration updates are **already** admin-gated. The issue's "prevent
unauthorized configuration updates" is therefore satisfied today by a single
all-powerful admin. The meaningful improvement is **granularity**: letting the
admin delegate *specific* config powers without handing over full admin
(which today also rotates admin and the guardian set).

### 6.2 Options

- **(A) Status quo.** Argue #430 is already met; close with a test that
  documents the existing admin-gating. Lowest risk, least value.
- **(B) Role enum + per-address role map.** Introduce
  `#[contracttype] enum Role { Admin, SignerManager, ThresholdManager,
  GuardianManager, ... }` and `DataKey::Roles Map<Address, Role>`. Each config
  entry point checks a *capability* rather than "is admin". Admin retains all
  capabilities (backward compatible). Admin can grant a subset to another
  address. **ABI-additive** (new storage key, new type; existing functions
  unchanged in signature). This is the design that matches "granular
  role-based authorization."
- **(C) Capability bitset.** `Map<Address, u32>` bitmask of capabilities. More
  compact, less self-describing than (B); harder to extend without renumbering
  the bitmask (which is itself a small ABI surface).

### 6.3 Recommendation

**(B)**, with these rules to stay inside the constraints:

- **Centralize authorization** in one helper, e.g.
  `fn require_role(env, role: Role) -> Result<(), GovernanceError>`, replacing
  the scattered `get_admin()?.require_auth()` calls in config entry points. This
  is the single chokepoint a reviewer audits — which is itself the security
  value of RBAC.
- **Admin is the implicit superset.** Any address holding `Role::Admin`
  satisfies every capability check, so existing deployments that only ever set
  an admin keep identical behavior. `init` assigns the admin `Role::Admin`.
- **Role grants go through the proposal flow** for non-admin roles (the admin
  granting `SignerManager` to X is itself a config change → propose/approve/
  timelock), **except** the admin granting/revoking roles, which stays
  admin-direct — otherwise there is no way to bootstrap delegation. This is the
  one judgment call; document it. Alternative: *all* role changes go through
  governance proposals, admin can do nothing directly except propose — purer,
  but then "admin" is just another signer for config and #430 collapses into
  the existing multisig. State that alternative in the doc and let the reviewer
  choose.
- **New error variants (appended only):** `UnauthorizedRole = 28`,
  `UnknownRole = 29` if (B) needs them. Never renumber.
- **Footprint:** one new instance key (`DataKey::Roles`). Role lookup is O(1).
  No per-call scan added. Bounds documented as for `MAX_PAGE_SIZE`.

### 6.4 Test plan (#430)

- Admin can perform every config op (backward compatibility).
- A `SignerManager`-only address can `add_signer`/`remove_signer` but *not*
  `set_threshold` or `set_admin` (returns `UnauthorizedRole`).
- A non-role address cannot perform any config op.
- Admin retains the ability to grant/revoke roles; a delegated role cannot
  escalate itself or grant roles to others.
- Granting a role emits an event (`RoleGranted { addr, role }`) for indexer
  observability, consistent with the existing `SignerAdded`/`SignerRemoved`
  pattern.
- The single `require_role` helper is the only authorization path — asserted by
  a test that greps the binary for direct `require_auth` in config entry points
  (or by code review gate).

## 7. Design: #423 — oracle-router (excluded)

`oracle-router` does not exist and has no analog. Implementing #423 would mean
scaffolding a new oracle-aggregation contract (price feeds, quorum across
oracles, staleness/decay, deviation thresholds). That is a substantial new
subsystem, explicitly outside this repo's stated scope (README non-goals:
"no cross-chain anything"; the project is a single-token streaming primitive).
It also does not belong in a PR titled as a multisig-threshold refactor.

**Recommendation:** do not implement #423 here. If it is real, it needs its own
spec (contract purpose, oracle model, liveness/safety under byzantine oracles,
integration with `stream`/`factory`/`governance`) and confirmation that
Perpetua-Contracts is even the right repository. Flagged back to the issue
author / user.

## 8. Implementation & PR notes

- This document is design-only, per the user's direction ("just draft the
  design").
- For completeness on the PR step the user mentioned earlier: `gh` is not
  installed in this environment (`gh: command not found`), and the configured
  remote is an SSH alias (`git@github-devprom6:devprom6/Perpetua-Contracts.git`)
  rather than `github.com`, so `gh pr create` is not available as-is. The
  repository identity is also inconsistent across sources (issues avatar
  `releaseken`, git remote `devprom6`, `Cargo.toml` `hannyloveworld`) — a
  sandboxed clone. PR submission will need either `gh` installed and
  authenticated, the platform's own API/CLI, or manual creation; confirm the
  target before pushing.
- When implementation proceeds, the order should be #434 (smallest, ABI-safe,
  unblocks nothing else) → #430 (RBAC, the one real behavior change) → #432
  (tests, some of which assert #430's new auth paths). Each as its own commit on
  a feature branch.
- Before implementing #430/#432, read the remainder of `governance/src/lib.rs`
  not yet reviewed here (`execute`, `cancel_proposal`, the view functions, and
  the existing inline `#[cfg(test)]` modules) to match test idioms and confirm
  the exact set of admin-gated entry points. This design is based on the error
  enum, storage keys, `init`/`set_*`/`propose`/`approve` paths, the events, the
  retention policy, and the reentrancy guard; the execute/cancel paths were not
  read in full.

## 9. Open questions (require confirmation before implementation)

1. Is mapping `multisig-threshold` → `contracts/governance` correct, or are
   #434/#432/#430 meant for a different repository?
2. For #430, is the intent (B) granular delegation, or (A) "already met,
   document it"? The issues are too vague to tell.
3. Should #423 be dropped, or is there an oracle-router contract elsewhere that
   should be the real target?
4. How should the PR be submitted given no `gh` and a sandbox remote?

---

# Design: Oracle-Router & Multisig-Threshold Performance and Fuzz Hardening

**Status:** Draft
**Issues:** #422, #431, #427, #428
**Scope:** `contracts/oracle-router`, `contracts/multisig-threshold`

## Prerequisite and scope note

The four issues target `contracts/oracle-router` and `contracts/multisig-threshold`.
Neither contract is present in this checkout — the workspace currently contains only
`archival-probe`, `factory`, `governance`, `stream`, and `stream-nft` under `contracts/`,
and a repo-wide search finds no references to either contract name.

This design therefore specifies the **target behavior and performance characteristics**
for those contracts rather than a retrofit of existing code. Implementation requires
first importing or scaffolding `oracle-router` and `multisig-threshold` into the
workspace. The optimization and fuzz strategies below are defined against that target
so they apply directly once the contracts land. Open question Q1 tracks this.

## Goals

1. Minimize Soroban host CPU instructions in `oracle-router` and `multisig-threshold` by
   eliminating redundant deserialization in loops (#422, #431).
2. Streamline cross-contract invocation (CCI) overhead in `oracle-router` during
   high-frequency batch claims (#427).
3. Add fuzz coverage for edge-case timestamps near the `u64` maximum boundary in
   `oracle-router` (#428).
4. Defensive error handling and checked arithmetic throughout, with no panics on
   untrusted input.

## Non-goals

- Changing external contract APIs or on-wire storage formats beyond what the
  optimizations require.
- Treating before/after instruction counts as a deliverable in themselves; they are
  verification evidence, not a goal.
- Gas tuning unrelated to deserialization, CCIs, or timestamp arithmetic.

## Background: cost model

- **Deserialization.** Reading a collection from durable storage via
  `Env::storage().get()` returns a `Val` that is converted to the target type through
  `TryFromVal`. Re-reading the same key inside a loop, or re-deriving an element per
  iteration, repeats host work proportional to `N`. Binding the result to a local
  once and iterating the local avoids the repeat.
- **Cross-contract invocation.** Each `invoke_contract` / CCI carries fixed host
  overhead (args serialization, dispatch, result conversion). In a per-claim loop this
  cost is multiplied by the batch size.
- **Timestamps.** Soroban ledger timestamps are `u64`. Expiry, staleness-window, and
  next-update arithmetic can overflow near `u64::MAX`; `checked_add`/`checked_sub`
  return a typed `Error` instead of panicking or silently wrapping.

## Design

### 1. Loop deserialization (#422 oracle-router, #431 multisig-threshold)

**Problem.** Iterating a stored collection by calling `storage().get()` per element —
or holding a `Val` and re-converting each iteration — re-enters the host deserializer
`N` times.

**Approach.**

- **Deserialize once, iterate the local.** Pull the collection from storage into a
  strongly-typed local (`Vec<T>` / `Map<K, V>`) once per call, then iterate the bound
  local. Do not call `storage().get()` inside the loop.
- **Iterate, don't index-fetch.** Prefer `iter()` over the bound collection over
  repeated `Vec::get(i)` when the element is consumed once; materialize the whole
  collection up front rather than touching storage per index.
- **Nested collections.** Ensure the outer deserialize fully materializes nested
  structures (e.g. `Vec<Signer>` whose `Signer` holds a nested `Vec`); do not lazily
  re-read inner data per outer element.
- **multisig-threshold shape.** Store signers as a single `Map<Address, u32>`
  (address → weight) or a single `Vec<Signer>`, deserialized once per verification
  pass, instead of per-signer storage reads. Threshold checks then iterate the bound
  local.
- **oracle-router shape.** Cache the active price feed map deserialized once per
  routing decision; route lookups iterate the local rather than re-reading storage.

**Verification.**

- Unit tests at large `N` (256 signers; 256 price entries) assert correct aggregates
  with zero per-element storage reads (a test storage hook counts reads).
- Before/after host CPU instruction counts captured in test output to confirm the
  reduction.

### 2. Cross-contract invocation overhead in batch claims (#427 oracle-router)

**Problem.** In high-frequency batch claims, issuing one CCI per claim to fetch oracle
price(s) multiplies fixed CCI overhead by batch size.

**Approach.**

- **Single-read fan-out.** Fetch the relevant price feed(s) once at the start of the
  batch, bind to a local, and reuse for every claim that consumes the same feed. Only
  re-invoke when a claim requires a different feed/asset.
- **Group claims by feed.** Bucket claims by the price feed they consume; fetch each
  distinct feed at most once, then process each bucket. CCI count becomes
  `O(distinct feeds)`, not `O(claims)`.
- **Compact CCI args.** Pass minimal `Val`-typed arguments across the CCI boundary
  (a feed id / index) rather than large structs.
- **Cache-first reads.** Have the router pull prices into its own storage on update;
  claims read the cache, so a batch claim triggers zero CCIs in the common case and
  falls back to a single CCI per missing feed.

**Verification.**

- Unit/fuzz tests with batch sizes 1, 16, 256 assert CCI count scales with distinct
  feeds, not claims, via a test double that counts invocations.

### 3. Edge-case timestamp fuzzing (#428 oracle-router)

**Problem.** Timestamp arithmetic (expiry, staleness window, next-update) can overflow
near `u64` boundaries, causing panics or silent wraparound.

**Approach.**

- Add a `proptest` fuzz target generating timestamps across boundary regions: `0`,
  `1`, near ledger close time, near `u32::MAX`, near `u64::MAX`, and `u64::MAX` itself;
  deltas include `0`, `1`, large values, and values that force `t + delta` and
  `t - delta` to overflow.
- All timestamp arithmetic uses checked operations; overflow maps to a single typed
  `Error` variant rather than panicking or wrapping.
- **Property under test:** for any `(timestamp, delta)` the router never panics and
  returns a deterministic `Ok`/`Err`; every overflow maps to the same error variant.

**Verification.**

- `cargo test` fuzz target runs many shrinkable cases; CI gate prevents regressions.

### 4. Cross-cutting: defensive error handling and checked arithmetic

- Arithmetic on amounts, weights, timestamps, and counts uses checked operations; on
  overflow return a contract `Error` variant. No `unwrap`/`expect`/unvalidated
  indexing on untrusted input.
- Validate at trust boundaries: batch length, feed id, signer address, threshold
  range.
- Storage reads validated against expected types; missing keys return typed errors.

## Risks and trade-offs

- **Footprint.** Caching deserialized collections increases transient memory within a
  call, bounded by batch size — acceptable for the batch sizes in scope.
- **Failure atomicity.** Batching CCIs changes failure semantics: a single feed fetch
  failure now fails the whole batch. Mitigation: per-feed error isolation where the
  spec allows partial success; otherwise documented as intentional all-or-nothing
  (see Q2).

## Open questions

- **Q1.** Are `oracle-router` and `multisig-threshold` to be scaffolded in this repo, or
  imported from `releaseken/Perpetua-Contracts`? Neither exists in this checkout; this is
  blocking for implementation.
- **Q2.** Should batch claims support partial success, or remain all-or-nothing?

## Out of scope for this draft

Implementation code, the actual contract scaffolding, and measured before/after
numbers — all pending resolution of Q1 and the contracts landing in the workspace.

---

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


---

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


---

# `contracts/token-vault` — Architecture & Hardening Design (`Design.md`)

> **Status:** Draft / Pre-implementation  
> **Repository Note:** `contracts/token-vault` does not currently exist in `Perpetua-Contracts` (existing crates under `contracts/` are `archival-probe`, `factory`, `governance`, `stream`, and `stream-nft`). This document defines the design for `contracts/token-vault` and specifies the technical architecture addressing issues **#451**, **#447**, **#450**, and **#449**.

---

## 1. Overview & Crate Conventions

`contracts/token-vault` is a standalone Soroban smart contract crate following the repository's existing contract conventions (e.g., `contracts/stream/Cargo.toml`):

- **SDK & Toolchain:** `soroban-sdk = "27.0.6"` (Stellar Protocol 27), Rust `1.97.1`, `edition = "2021"`, target `wasm32v1-none`.
- **Testing:** `soroban-sdk` with `testutils` feature + `proptest = "1"` for property-based / fuzz testing.
- **Release Profile:** Size-optimized (`opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = "symbols"`, `debug = 0`).

### Responsibilities
The Token Vault manages custody, accounting, and fractional stream withdrawals for deposited Soroban SAC/SEP-41 tokens, enforcing strict solvency invariants, deterministic fixed-point arithmetic, minimal Soroban host CPU/memory footprint, and resilience under adversarial / partitioned execution.

---

## 2. Storage & Ledger Footprint Design

To minimize ledger entry reads/writes and avoid unbounded deserialization costs:

1. **Instance Storage (`DataKey::Config`, `DataKey::ReentrancyGuard`, `DataKey::TotalReserves(Address)`):**
   - Global vault configuration (admin, paused state, fee config) and compact re-entrancy lock flag.
   - Per-token aggregate accounting (`VaultTotals { total_deposited, total_reserved, total_withdrawn }`) stored in a single `#[contracttype]` struct so balance invariants are checked with one read/write.
2. **Persistent Storage (`DataKey::StreamAccount(u64)` / `DataKey::VaultPosition(Address, Address)`):**
   - Individual stream/vault positions keyed by ID or `(owner, token)` rather than stored inside a monolithic `Vec` or `Map` in a single ledger entry.
   - Avoids `O(N)` deserialization of unrelated positions during single-user operations.
3. **TTL Management:**
   - Explicit `extend_ttl` thresholds on persistent entries and instance storage to prevent archival desync.

---

## 3. Issue-Specific Designs

### 3.1 `#451` — Strict Invariant Assertions, Re-Entrancy Protection & State Desync Prevention

**Threat Model:**
- Cross-contract calls to external token contracts (`token::Client::transfer`) or hook receivers could attempt re-entrant entry into vault mutating methods (`deposit`, `withdraw`, `claim_stream`, `cancel_stream`).
- Partial state updates before external calls or unchecked balance deltas could cause `total_reserved` or `total_deposited` to diverge from actual vault token holdings.

**Design:**
1. **Checks-Effects-Interactions (CEI) Pattern:**
   - All authorization (`require_auth()`), input validation, and state mutations (updating position `withdrawn` amounts and `VaultTotals`) MUST complete **before** any external `token::Client` transfer.
2. **Explicit Re-Entrancy Guard:**
   - A scoped RAII-style or explicit storage lock (`DataKey::Locked(bool)`) checked at the entry of every state-mutating entrypoint.
   - If locked, immediately return `Err(VaultError::ReentrancyDetected)` (avoiding raw `panic!` where structured `#[contracterror]` is expected, while still aborting the transaction atomically).
3. **Strict Solvency & Conservation Invariants (`assert_vault_invariants`):**
   - After state updates (and optionally verified against `token_client.balance(&env.current_contract_address())` in debug/invariant-checked paths):
     - `total_deposited >= total_withdrawn`
     - `total_deposited - total_withdrawn >= total_reserved`
     - `position.withdrawn <= position.allocated`
     - `actual_vault_balance >= total_deposited - total_withdrawn` (prevents state desync from unexpected balance reductions; handles fee-on-transfer or direct-transfer edge cases explicitly).
   - All arithmetic uses `checked_add`, `checked_sub`, `checked_mul`, `checked_div` returning `VaultError::ArithmeticOverflow` / `VaultError::InsufficientLiquidity`.

---

### 3.2 `#447` — Resolving Precision Loss in Fractional Stream Withdrawal Calculations

**Problem:**
- Naive integer division `(elapsed * rate)` or `(total * elapsed) / duration` in incremental withdrawals truncates fractional remainders on every claim. Over many small withdrawals, repeated truncation either permanently locks dust in the vault or allows cumulative rounding drift.

**Design:**
1. **Cumulative-First Accounting (No Incremental Rounding Drift):**
   - Never compute `delta = ((now - last_claim) * total) / duration`.
   - Instead, always compute **cumulative vested amount** from `start_time`:
     $$\text{vested}(t) = \left\lfloor \frac{\text{total\_amount} \times (t - \text{start\_time})}{\text{duration}} \right\rfloor$$
   - Withdrawable amount is:
     $$\text{withdrawable}(t) = \text{vested}(t) - \text{already\_withdrawn}$$
   - At `t >= end_time`, `vested(t)` MUST equal `total_amount` exactly, guaranteeing zero dust remains locked when the stream completes.
2. **Overflow-Safe Wide Arithmetic (`u128` / `I256`):**
   - Cast `i128` token amounts and `u64` timestamps to `i256` / `u128` (or use `soroban_sdk::I256` / checked `u128` mul-div) before multiplying `total_amount * elapsed`, then divide by `duration`, and safely convert back to `i128`.
   - Explicitly handle edge cases:
     - `t <= cliff_time` → `0`
     - `duration == 0` → `VaultError::InvalidDuration` (rejected at creation)
     - `t >= end_time` → `total_amount`
     - `total_amount == 0` → `0`
     - `vested < already_withdrawn` → `VaultError::InvariantViolation`

---

### 3.3 `#449` — Minimizing Soroban Host CPU Instructions via Loop Deserialization Optimization

**Problem:**
- Iterating over `soroban_sdk::Vec<T>` or `Map<K, V>` stored as a single `Val` triggers repeated host-to-guest XDR/Val deserialization and host function calls inside loops, spiking Soroban CPU instruction budgets.

**Design:**
1. **Eliminate Unbounded Loops Over Storage Collections:**
   - Replace monolithic `Vec<StreamPosition>` storage with per-ID persistent ledger entries (`DataKey::Position(u64)`), making single-stream operations `O(1)`.
2. **Batch Processing Optimizations (Where Loops Are Required, e.g., `batch_withdraw`):**
   - **Hoist Invariants Out of Loops:** Read `env.ledger().timestamp()`, `VaultConfig`, and `VaultTotals` **once** before the loop; accumulate `total_payout` in a local `i128` variable and write `VaultTotals` **once** after the loop.
   - **Pre-indexed / Compact Keys:** Pass only `Vec<u64>` (stream IDs) as calldata rather than full structs, minimizing argument deserialization.
   - **Single Token Transfer Aggregation:** When batch-claiming multiple streams of the same token for the same recipient, sum the net payouts in memory and execute a single `token_client.transfer(...)` call.
   - **Budget Measurement:** Add `env.cost_estimate().budget()` assertions in unit tests to lock in CPU instruction and memory byte ceilings.

---

### 3.4 `#450` — Byzantine Fault Tolerance (BFT) & Network Partition Test Scenarios

**Scope:**
- Simulate adversarial, out-of-order, stale-ledger, and partitioned execution conditions in the Soroban test environment (`soroban_sdk::testutils`).

**Test Scenarios:**
1. **Stale / Out-of-Order Timestamp & Ledger Sequence Advancement:**
   - Simulate delayed transactions arriving after cliff/end boundaries, concurrent claims across ledger jumps, and verify cumulative-first math remains monotonic and deterministic.
2. **Conflicting / Byzantine Actor Sequences:**
   - Duplicate/replay withdrawal attempts within the same ledger and across ledgers.
   - Malicious token contract mock attempting re-entrancy into `withdraw` / `cancel` during `transfer`.
   - Unauthorized caller / spoofed admin or recipient attempting state mutation under `mock_auths` / explicit auth failure (`#[should_panic]` / `try_*` error assertions).
3. **Partial State / Partition Recovery:**
   - Simulate TTL expiration / archival boundary conditions and verify recovery or safe failure without corrupting `VaultTotals`.
   - Simulate `proptest` randomized operation sequences (`deposit`, `partial_withdraw`, `full_withdraw`, `cancel`, `reentrant_call`) interleaved with arbitrary time jumps, asserting that `VaultTotals` conservation invariants hold after every step.

---

## 4. Proposed Module Layout (`contracts/token-vault/`)

```text
contracts/token-vault/
├── Cargo.toml
└── src/
    ├── lib.rs          # Contract entrypoint, public methods, re-entrancy guard
    ├── storage.rs      # DataKey, VaultConfig, VaultTotals, Position, TTL helpers
    ├── math.rs         # Cumulative-first fixed-point stream math (overflow-safe)
    ├── errors.rs       # #[contracterror] VaultError enum
    ├── invariants.rs   # Solvency & conservation invariant assertions
    └── test.rs         # Unit, BFT/partition, budget-regression, and proptest fuzz suites
```

---

## 5. Verification Plan

Once `contracts/token-vault` is scaffolded and implemented:
1. **Unit & Invariant Tests:** `cargo test` inside `contracts/token-vault` covering all `VaultError` paths, CEI/re-entrancy mocks, and boundary timestamps (`0`, `cliff - 1`, `cliff`, `end - 1`, `end`, `end + N`).
2. **Property / Fuzz Tests (`proptest`):** Verify zero dust lockup on stream completion and strict conservation (`sum(withdrawn) + remaining == total_deposited`) across randomized withdrawal schedules.
3. **CPU / Memory Budget Tests:** Assert `env.cost_estimate().budget().cpu_instruction_cost()` stays within target thresholds for batch operations.
4. **WASM Build:** `cargo build --target wasm32v1-none --release` to verify Soroban compatibility and binary size.


---

# Token Vault (`contracts/token-vault`) — Architecture & Implementation Design

> **Status:** Draft  
> **Scope:** Issues **#448**, **#452**, **#453**, **#454**  
> **Target Crate:** `contracts/token-vault` (to be scaffolded; not yet present on `main`)

---

## 1. Context & Repository Alignment

`contracts/token-vault` does not currently exist in `Perpetua-Contracts` (which presently contains `archival-probe`, `factory`, `governance`, `stream`, and `stream-nft`). This document specifies the architecture for `contracts/token-vault` so that its initial implementation satisfies all four issues while matching the repository's existing Soroban conventions (see `contracts/stream/Cargo.toml` and `contracts/stream/src/`):

- **Standalone Cargo crate:** `[workspace]` with `members = ["."]`, `edition = "2021"`, `soroban-sdk = "27.0.6"` (Protocol 27).
- **Release profile:** `opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = "symbols"`, `overflow-checks = true`.
- **Testing stack:** `soroban-sdk` with `testutils` + `proptest = "1"` for property/fuzz testing.
- **Module layout:**
  ```text
  contracts/token-vault/
  ├── Cargo.toml
  └── src/
      ├── lib.rs        # Contract trait & public entrypoints
      ├── error.rs      # Modular VaultError enum & observability helpers (#452)
      ├── auth.rs       # Granular RBAC roles & guards (#448)
      ├── storage.rs    # Typed DataKeys & automated TTL extension (#453)
      ├── types.rs      # Vault config, ClaimItem, BatchClaimResult, Role
      ├── events.rs     # Structured Soroban events
      └── test/         # Unit & proptest suites
  ```

---

## 2. Issue Designs

### 2.1 Modular Error Types & Custom Error Messages (`#452`)

**Goal:** Provide deterministic, client-observable error codes and structured diagnostic context without inflating WASM size.

- **Grouped `#[contracterror]` enum (`src/error.rs`):**
  Assign stable, non-overlapping numeric ranges so SDKs and indexers can categorize failures immediately:
  - `100–199` **Lifecycle & Configuration:** `AlreadyInitialized = 100`, `NotInitialized = 101`, `Paused = 102`, `InvalidConfig = 103`, `ZeroAddress = 104`
  - `200–299` **Authorization & RBAC:** `Unauthorized = 200`, `MissingRole = 201`, `CannotRevokeLastAdmin = 202`, `PendingAdminNotSet = 203`
  - `300–399` **Accounting & Arithmetic:** `ZeroAmount = 300`, `InsufficientBalance = 301`, `ArithmeticOverflow = 302`, `ArithmeticUnderflow = 303`, `CapExceeded = 304`
  - `400–499` **Batch & Cross-Contract:** `EmptyBatch = 400`, `BatchTooLarge = 401`, `InvalidClaimItem = 402`, `TokenTransferFailed = 403`
  - `500–599` **Storage & Archival:** `EntryNotFound = 500`, `InvalidTtlConfig = 501`
- **Client observability:**
  - Every `VaultError` variant carries concise doc comments used by Soroban contract spec generation (`soroban contract bindings`).
  - For batch or complex validation failures where a single `u32` code is ambiguous (e.g., which item in a batch failed), emit a lightweight `VaultErrorEvent` topic/data tuple or return a structured error code before aborting, keeping `#[contracterror]` `Copy + Eq + Debug` and WASM-compact.
- **Defensive arithmetic:**
  - All balance, fee, and batch-total calculations use `checked_add`, `checked_sub`, `checked_mul`, and `checked_div`, converting `None` directly into `VaultError::ArithmeticOverflow` / `VaultError::ArithmeticUnderflow`.

---

### 2.2 Granular Role-Based Authorization (`#448`)

**Goal:** Eliminate single-key privilege concentration and prevent unauthorized configuration updates.

- **Role model (`src/types.rs`, `src/auth.rs`):**
  ```rust
  #[contracttype]
  #[derive(Clone, Copy, Debug, Eq, PartialEq)]
  pub enum Role {
      Admin,        // Role assignment, two-step admin rotation, emergency recovery
      ConfigAdmin,  // Update vault parameters (caps, fee bps, TTL policy)
      Pauser,       // Pause / unpause deposits and claims
      Operator,     // Execute operational sweeps / batch settlements
      Keeper,       // Permissionless/delegated TTL maintenance
  }
  ```
- **Storage & guards:**
  - Store role memberships as `DataKey::Role(Role, Address) -> bool` in `persistent` (or `instance` for singleton roles) storage, with `DataKey::AdminCount -> u32` to prevent accidentally revoking the last `Admin`.
  - Provide `require_role(env, &caller, Role::...) -> Result<(), VaultError>` which calls `caller.require_auth()` and verifies membership, returning `VaultError::MissingRole` on mismatch.
  - **Two-step admin rotation:** `propose_admin` (requires `Admin`) writes `DataKey::PendingAdmin(Address)`; `accept_admin` requires auth from the pending address before swapping roles.
  - **Least-privilege mapping:**
    - `grant_role` / `revoke_role`: `Admin` only.
    - `update_config` / `set_ttl_policy`: `ConfigAdmin` only.
    - `pause` / `unpause`: `Pauser` only.
    - `settle_batch` (if operator-gated): `Operator` only.
    - `extend_ttl`: Permissionless (or `Keeper`-assisted) since extending TTL is strictly monotonic and safe.

---

### 2.3 Automated Entry Lifetime Extension (`#453`)

**Goal:** Guarantee archival resilience for long-lived vault state and user balances following Soroban Protocol 27 best practices.

- **Instance vs. Persistent policy (`src/storage.rs`):**
  - **Instance storage** (contract metadata, `VaultConfig`, `TtlPolicy`, `Paused`, `PendingAdmin`): extended automatically on every state-mutating entrypoint via `bump_instance(env)`.
  - **Persistent storage** (per-user `Balance(Address)`, `Role(Role, Address)`, `ClaimNonce(Address)`): extended automatically on **every read and write** using `extend_ttl(threshold, extend_to)`.
- **Dynamic `max_ttl()` clamping:**
  - Never hardcode network max TTL. Query `env.storage().max_ttl()` at runtime and clamp `extend_to = target_ttl.min(max_ttl)` and `threshold = threshold_ttl.min(extend_to)`, matching the pattern in `contracts/stream/src/storage.rs`.
- **Configurable `TtlPolicy`:**
  - Store `TtlPolicy { instance_threshold, instance_extend_to, persistent_threshold, persistent_extend_to }` with safe defaults (e.g., 30-day floor in ledgers) and validation (`threshold <= extend_to`, `extend_to > 0`).
- **Proactive keeper entrypoint:**
  - Expose `bump_vault_ttl(env, keys: Vec<DataKey>)` so off-chain keepers can refresh dormant user balances before archival without mutating balances.

---

### 2.4 Streamlining Cross-Contract Invocation Overhead in Batch Claims (`#454`)

**Goal:** Minimize Soroban CPU instructions, memory, and ledger footprint during high-frequency batch claims.

- **Current bottleneck in naive batch claims:**
  - Invoking `token::Client::new(&env, &token).transfer(&vault, &recipient, &amount)` inside a `for item in claims` loop triggers `N` cross-contract calls, `N` sub-invocation frames, and repeated storage reads of `VaultConfig` / `Paused` / `TokenAddress`.
- **Optimization strategy:**
  1. **Single-pass validation & aggregation:**
     - Read `VaultConfig` and `Paused` **once** before the loop.
     - Enforce `1 <= claims.len() <= MAX_BATCH_SIZE` (`VaultError::EmptyBatch` / `VaultError::BatchTooLarge`).
     - Validate each `ClaimItem` (`amount > 0`, valid auth/proof/entitlement) and accumulate payouts in an in-memory `Map<Address, i128>` (coalescing multiple claims for the same recipient) using `checked_add`.
  2. **Coalesced cross-contract transfers:**
     - Instantiate `token::Client` **once** outside the transfer loop.
     - Issue **at most one `token_client.transfer(...)` per unique recipient** (or a single `transfer` when claiming multiple items for a single caller), cutting cross-contract invocations from `N` down to `U` (`U <= N`, often `U == 1`).
  3. **Batched storage updates & TTL bumps:**
     - Update each affected user's persistent balance once after aggregating their items, extending TTL once per touched key rather than per claim item.
  4. **Atomic failure:**
     - Validate all items and compute all checked deltas before committing storage writes and external token transfers, ensuring no partial state corruption on error.

---

## 3. Verification & Test Plan

All tests will live in `contracts/token-vault/src/test/` and run via `cargo test` inside `contracts/token-vault`:

1. **RBAC unit tests (`#448`):**
   - Verify every config mutator rejects callers lacking the exact required `Role` with `VaultError::MissingRole`.
   - Verify `revoke_role` rejects removing the final `Admin` (`VaultError::CannotRevokeLastAdmin`).
   - Verify two-step `propose_admin` -> `accept_admin` flow and rejection of unauthorized `accept_admin`.
2. **Error & arithmetic tests (`#452`):**
   - Trigger and assert every `VaultError` variant; verify `i128::MAX` overflow and underflow paths return `ArithmeticOverflow` / `ArithmeticUnderflow` instead of panicking.
3. **Archival / TTL tests (`#453`):**
   - Advance `env.ledger().sequence()` past `threshold`, invoke reads/writes and `bump_vault_ttl`, and assert `get_ttl` on both `instance` and `persistent` entries is extended and clamped to `env.storage().max_ttl()`.
4. **Batch claim performance & fuzz tests (`#454`):**
   - Unit test verifying `N` claim items for `U` unique recipients execute `U` token transfers (e.g., 20 claims for 1 recipient -> 1 transfer) and match balances exactly.
   - `proptest` fuzzing over arbitrary `Vec<ClaimItem>` (varying duplicates, amounts, and failure injections) asserting fund conservation (`vault_balance_before - vault_balance_after == sum(claimed)`) and zero panics.

---

## 4. Open Questions Before Implementation

1. **Asset model:** Should `token-vault` hold a single fixed SAC/SEP-41 token per vault instance (configured at `initialize`), or support multiple tokens per vault?
2. **Claim authorization model:** Are batch claims caller-initiated (one user claiming multiple epochs/tranches) or operator-initiated (distributing to multiple recipients), or both?
3. **Batch cap:** What `MAX_BATCH_SIZE` (e.g., `50` or `100`) best fits Protocol 27's per-transaction footprint and instruction budget?
