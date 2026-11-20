# Oracle Router — Design

`FluxoraOracleRouter` is a price-aggregation and conversion layer for Perpetua.
It routes price queries across a registry of on-chain oracle sources, validates
them for staleness and cross-source deviation, and returns a canonical rational
price for an asset pair. The stream contract's opt-in cross-asset withdrawal
path calls the router to convert a fractional withdrawal amount from the
stream's deposit token into the requested payout token at a trust-minimized
rate.

This document is a **design only**. No contract code is written here; it is the
specification the implementation in `contracts/oracle-router/` will follow, and
it resolves three open issues:

- **#425** — modularize error types and custom error messages for client
  observability.
- **#421** — granular role-based authorization to prevent unauthorized
  configuration updates.
- **#420** — edge-case precision loss during fractional stream withdrawal
  calculations.

It follows the conventions already established by `contracts/stream`,
`contracts/factory`, and `contracts/governance`: a standalone Cargo project,
`#![no_std]`, `soroban-sdk` 27.x, frozen-discriminant `#[contracterror]`,
checked arithmetic everywhere, size-optimized release profile, and a rich
`src/test/` suite (unit + fuzz/property tests).

> Relationship to the stream contract: Perpetua v1 is single-token by design
> (see the README "Non-goals"). The router does **not** change that. It is an
> independent, opt-in module: a stream continues to settle in its deposit
> token by default. Cross-asset withdrawal is a forward capability the router
> enables, and the router is fully usable by any caller without the stream
> contract. The integration point with `FluxoraStream` is specified in
> [§ Integration](#integration-with-stream) but is out of scope for this design
> beyond the conversion function signature.

---

## Architecture

The router is a **pull-model** aggregator. A caller asks for the price of a
pair `(base, quote)`; the router reads each registered oracle source for that
pair, discards stale and outlier reports, and returns the canonical price plus
metadata. Prices are not stored long-term — each call re-reads sources — which
keeps the footprint bounded and avoids stale-data bugs. An optional
operator-pushed cache (`submit_price`) is supported for pairs that opt into a
push model; cached values carry their own expiry.

Components:

1. **Pair registry** — `Map<AssetPair, Vec<OracleSource>>`. An `AssetPair` is a
   `(base: BytesN<32>, quote: BytesN<32>)` key (see [Data model](#data-model)).
   Each `OracleSource` wraps a source contract address and a decoder descriptor.
2. **Aggregation policy** — per-pair: `staleness_threshold` (seconds),
   `deviation_threshold` (basis points), `min_sources`. The aggregator drops a
   report older than the staleness window, drops reports that deviate from the
   median by more than the deviation threshold, and requires at least
   `min_sources` survivors; otherwise it returns `InsufficientSources`.
3. **Conversion math** — the rational fixed-point conversion used by the
   cross-asset withdrawal path ([§ Precision](#precision-and-conservation-420)).
4. **Role registry** — instance-storage `Map<Address, Role>` plus the admin
   address ([§ Authorization](#role-based-authorization-421)).

---

## Data model

```text
AssetPair      { base: BytesN<32>, quote: BytesN<32> }   // contract/asset ids
OracleSource   { addr: Address, decoder: Symbol, scale: i128 }
PairConfig     { staleness_threshold: u64,
                 deviation_threshold_bps: u32,
                 min_sources: u32,
                 paused: bool }
PriceReport    { numerator: i128, denominator: i128, last_updated: u64, source: Address }
CanonicalPrice { numerator: i128, denominator: i128,
                 updated_at: u64, sources_used: u32 }
Role           enum { Admin, OracleManager, ParameterManager, EmergencyGuardian }
```

`CanonicalPrice` is a **rational** price: `1 smallest_unit(base) =
numerator / denominator smallest_units(quote)`. Storing the ratio as a reduced
fraction (rather than a decimal + exponent) is what makes the conversion in
[§ Precision](#precision-and-conservation-420) exact and overflow-safe; it also
sidesteps the decimal-juggling that causes the precision loss #420 describes.

### Storage keys (instance vs persistent)

| Key | Scope | Holds |
|---|---|---|
| `Admin` | instance | current admin address |
| `Roles` | instance | `Map<Address, Role>` |
| `PendingAdmin` | instance | two-step admin transfer nominee |
| `Pairs` | instance | `Map<AssetPair, Vec<OracleSource>>` |
| `PairConfig(AssetPair)` | persistent | per-pair aggregation policy |
| `Cache(AssetPair)` | persistent | operator-pushed price + expiry (optional) |

Instance entries are extended on every load (17,280-ledger threshold,
120,960-ledger bump, matching factory/governance). Persistent pair-config and
cache entries are extended on read/write with the same threshold/bump as the
governance active-proposal policy. A missing/archived `PairConfig` is treated as
absent (the pair has no sources → `PairNotFound`), not as a host error — the same
"archived ⇒ absent, not trap" stance the factory takes for allowlist entries.

---

## Role-based authorization (#421)

Today the factory uses a single `admin` plus Soroban `require_auth`. The router
needs finer granularity because several parties legitimately mutate different
parts of the configuration, and conflating them into one admin is exactly the
"unauthorized configuration update" surface #421 wants closed.

### Roles

| Role | Powers |
|---|---|
| `Admin` | assign/revoke other roles; two-step transfer of the admin seat; the only role that can grant `Admin`. |
| `OracleManager` | `add_source`, `remove_source` for any pair. Cannot change policy. |
| `ParameterManager` | `set_pair_config` (staleness, deviation, min_sources). Cannot touch the source list. |
| `EmergencyGuardian` | `set_pair_paused(pair, bool)` and `global_pause`. Cannot move funds, change sources, or change policy. |

Each mutating entry point checks the caller's role against the required one via
a single `require_role(env, caller, role)` helper that:
1. Calls `caller.require_auth()` (Soroban native auth — non-repudiable).
2. Reads `Roles` and verifies the caller holds `role` (or `Admin`, which
   satisfies every check).
3. On failure emits a [diagnostic event](#modular-error-model-425) carrying
   `{op, caller, required_role, held_role}` and returns `UnauthorizedRole`.

### Guards against unauthorized configuration updates

- **No implicit privilege escalation.** `OracleManager` cannot grant roles; only
  `Admin` mutates `Roles`. There is no "super-editor" that can do both.
- **Two-step admin transfer** (matches `factory`): `propose_admin(new)` →
  `accept_admin()`. A nominee is recorded in `PendingAdmin`; the incumbent keeps
  power until the nominee accepts, so a typo'd address cannot orphan the
  contract. `Admin` may not be the zero address or the contract itself.
- **Pause is fail-closed.** `EmergencyGuardian` can pause a pair or the whole
  router; while paused, `get_price` returns `PairPaused` / `RouterPaused` rather
  than a stale value. Unpausing is a separate, logged action.
- **Role changes are auditable.** Every `grant_role` / `revoke_role` /
  `propose_admin` / `accept_admin` emits a contract event (topic + structured
  data), so indexers reconstruct the live role set from events alone.
- **No self-grant.** `Admin` cannot grant `Admin` to itself (no-op) or remove its
  own admin role (rejected with `CannotRevokeSelf`) — preventing accidental
  lockout. To rotate admin, use the two-step transfer.

### Why not reuse `governance`?

Governance already provides timelocked multi-sig for protocol parameters, and a
`FactorySetCap`-style `CallData` variant could route router config through it.
That is the **recommended** path for production parameter changes: governance
proposes, the timelock elapses, and `execute` calls a `ParameterManager`-keyed
setter. The on-chain role checks here are the **fail-closed floor**: even a
direct (non-governance) call is still gated to the correct role, so the router
cannot be misconfigured by a caller who only holds, say, the guardian key. The
two layers compose; neither replaces the other.

---

## Precision and conservation (#420)

This is the heart of the design. The issue: the cross-asset withdrawal path
converts a (fractional) withdrawal amount using the oracle price, and the naive
`amount_in * price / scale` both **overflows `i128`** for large
amount-or-price products and **truncates away dust** on every fractional
withdrawal. Per the stream contract's own rules, rounding must be **down** (in
the recipient's disfavour) and the residue must be conserved in the pool — never
destroyed and never owed.

### The rational conversion

The router reports a **reduced rational** price
`CanonicalPrice { numerator, denominator }`, so:

```
amount_out = (amount_in * numerator) / denominator        // naive
```

with `gcd(numerator, denominator) == 1` (reduced at report time). The danger is
the intermediate product `amount_in * numerator`, which can exceed `i128::MAX`
(~1.7e38) even for ordinary inputs — e.g. `amount_in ≈ 1e18` stroops and
`numerator ≈ 1e21` (a price with 3 decimals on a 7-decimal asset). A direct
`checked_mul` would return `Overflow` and the withdrawal would fail for large
but legitimate amounts.

### Overflow-safe reduction

Before multiplying, cancel common factors so the product stays in range:

```text
// Step 1: cancel gcd(amount_in, denominator)
g1 = gcd(|amount_in|, denominator)
a  = amount_in / g1
d  = denominator / g1

// Step 2: cancel gcd(numerator, d)
g2 = gcd(|numerator|, d)
n  = numerator / g2
d2 = d / g2

// Now amount_out = a * n / d2, with a*n as small as possible.
prod = a.checked_mul(n)?            // Overflow ⇒ fall through to split path
amount_out = prod / d2
residue_units = prod % d2           // in reduced (a*n) units
```

Because `g1 * g2` divides `denominator`, the reductions are exact — no precision
is lost by cancelling. The product `a * n` is the smallest representation of
`amount_in * numerator / denominator`'s numerator, so it overflows only when the
true result itself does not fit in `i128` (i.e. the payout genuinely exceeds
`i128::MAX` smallest units), which is a real `ConversionOverflow` rather than a
spurious one.

### Split path (last resort)

If `a.checked_mul(n)` still overflows, the amount is split into halves, each
converted recursively, and the results and residues summed with checked
arithmetic. This terminates because each halving strictly shrinks `a`, and the
combined result is identical to the single-shot computation (the reduction is
associative over addition). Depth is bounded by `ceil(log2(amount_in))` ≤ 128;
in practice the double-reduction above resolves virtually all inputs without
recursion.

### Rounding and the residue

- **Round down** (toward zero), matching `FluxoraStream`'s "rounding is always
  down" rule. The recipient never receives more than exact; the residue stays
  in the pool and returns to the sender at settlement.
- **Residue is conserved, not discarded.** The conversion returns
  `(amount_out, residue_units, d2)`. The caller (stream withdrawal path) books
  `residue_units / d2` back into the stream's refundable balance, so that:

  ```
  amount_out * d2 + residue_units == prod == a * n
  ```

  holds exactly. This is the conservation invariant; the fuzz suite asserts it
  on every generated case (see [Test plan](#test-plan)).

- **Fractional withdrawals compose.** Withdrawing in two parts must yield the
  same total as one withdrawal of the sum, modulo at most one unit of residue
  per call. The reduction is deterministic, so `convert(x)` + `convert(y)`
  differs from `convert(x + y)` by at most `d2 - 1` residue units total; the
  stream's settlement returns the full residue to the sender, so no dust is ever
  stranded (in contrast to the per-interval accumulation the stream contract was
  specifically rewritten to avoid).

### Why this fixes #420

1. **No overflow panic** on large amounts: the reduction cancels common factors
   before the multiply, and the split path handles the pathological tail.
2. **No silent dust loss**: the residue is returned to the caller and booked
   back, so repeated fractional withdrawals cannot erode the pool — the exact
   class of bug the stream contract's cumulative-formula accrual was written to
   prevent.
3. **Deterministic & overflow-checked end-to-end**: every `+`, `*`, `/` uses
   `checked_*`; the only `Overflow` returns are genuine result-too-large, mapped
   to `ConversionOverflow`.

---

## Modular error model (#425)

The stream contract uses one flat `Error` enum. The router **modularizes** by
reserving discriminant ranges per subsystem, so a client can tell from the
number alone *which* layer failed — and pairs every error return with a
structured diagnostic event carrying the human-readable context the issue title
calls "custom error messages".

### Discriminant ranges

```text
 1–19   Authorization & roles        (UnauthorizedRole, NotAdmin, ...)
20–39   Pair registry & config      (PairNotFound, DuplicateSource, ...)
40–59   Price aggregation           (InsufficientSources, StaleReport, DeviationExceeded, ...)
60–79   Conversion / arithmetic    (ConversionOverflow, NegativeAmount, ZeroDenominator, ...)
80–99   Lifecycle / pause           (RouterPaused, PairPaused, ...)
```

Discriminants are **frozen ABI**: append-only, never renumber (same rule as the
stream and factory error tables). Each variant carries a precise doc comment
and is validated against this table by `script/validate-doc-alignment.py` (the
existing stream contract already enforces this for `docs/error.md`).

### Custom error messages via diagnostic events

A `#[contracterror]` discriminant alone is just a number; it cannot say *why*.
For client observability the router emits a **diagnostic event** on every error
path through one helper:

```text
fn fail(env, err: Error, op: Symbol, ctx: Map<Symbol, Val>) -> Error
```

`fail` publishes an event whose topic is `("oracle_error", op, err-as-symbol)`
and whose data is `ctx` — the offending pair, the value that breached a
threshold, the role held vs. required, the source that went stale, etc. Clients
that read a failed transaction's `diagnosticEvents` (the same channel the
stream contract already documents for `TokenTransferFailed`) get the custom
message; clients that only read the contract error still get the typed
discriminant. Both audiences are served without coupling the error ABI to
prose.

Rules:
- The diagnostic event is **always** emitted before the `Err` return, and never
  after a state mutation (CEI: effects before events), so a reverted transaction
  never leaves a misleading "success" event.
- `ctx` carries only values already in the call (caller, pair, amounts,
  thresholds) — never secrets.
- The error table documents, per variant, which `ctx` fields accompany it, so the
  message contract is part of the ABI.

This is the "modularize error types and custom error messages for enhanced
client observability" deliverable: subsystem-segmented discriminants **plus**
structured per-error context on the observability channel.

---

## Integration with stream

The router is self-contained, but the design must define the seam the stream
contract will call. The conversion is exposed as a pure view:

```text
convert(amount_in: i128, pair: AssetPair) -> (amount_out: i128, residue: i128, price: CanonicalPrice)
```

The stream withdrawal path, when a payout token differs from the deposit token,
calls `convert` on the fractional amount being withdrawn, pays `amount_out` to
the recipient, and credits `residue` back to the stream's refundable balance so
the sender recovers it at settlement. The router itself never holds tokens and
never moves balances; it only computes. This keeps the trust boundary clean: a
router bug can mis-price, but cannot steal funds, and the stream's conservation
invariant (`vested + refundable == deposited`) is preserved because the residue
is booked as refundable.

---

## Ledger footprint and TTL

- Instance storage (`Admin`, `Roles`, `Pairs`) is bumped on every load to the
  120,960-ledger amount at the 17,280-ledger threshold (factory/governance
  parity).
- Persistent `PairConfig` and `Cache` entries use the active-proposal-style
  threshold/bump and are treated as absent when archived (never as a trap).
- `get_price` is a **view**: it extends the entries it reads but performs no
  writes, so simulations do not produce footprint noise. Mutating setters bump
  once, after the new config is known (CEI).
- Source-list reads are O(sources) per pair; `min_sources` and the deviation
  filter bound the worst case. Pair source counts are capped (see
  [Resource limits](#resource-limits)).

### Resource limits

- `MAX_SOURCES_PER_PAIR = 8` — bounds the read budget of `get_price` and the
  deviation-filter loop. Eight sources covers the realistic oracles (Stellar
  Asset Contract price feeds, Tellor, Pyth-equivalent adapters) with a 2x
  margin; more would blow the per-call instruction budget before the event
  budget.
- `MAX_PAIRS` is not capped on-chain (pairs live in a `Map`), but the
  aggregation cost is per-call-per-pair, so the router's footprint is
  independent of how many pairs are registered — the same
  "cost-independent-of-count" property the stream contract asserts for stream
  ids.

---

## Checked arithmetic and formal invariants

All arithmetic is checked (`checked_add`, `checked_mul`, `checked_sub`,
`checked_div`). `Overflow` / `ConversionOverflow` are the only numeric failures;
nothing panics. The design asserts these invariants, each mapped to a test:

1. **Conversion conservation**: for all valid `(amount_in, numerator,
   denominator)`, `amount_out * d2 + residue_units == a * n` exactly.
2. **Conversion monotonicity**: `amount_in1 ≤ amount_in2 ⇒ amount_out1 ≤
   amount_out2` (rounding down preserves order).
3. **No overflow panic**: for any `amount_in`, `numerator`, `denominator` in
   `i128`, `convert` returns a value or `ConversionOverflow` — never traps.
4. **Aggregation safety**: `get_price` returns a price whose `updated_at` is
   within `staleness_threshold` of the ledger timestamp and that was backed by
   ≥ `min_sources` non-deviating reports, or it returns an aggregation error.
5. **Authorization completeness**: every mutating entry point either
   `require_auth`s a caller holding the required role or returns
   `UnauthorizedRole`; there is no unguarded mutator.
6. **Role-set soundness**: `Admin` can never be empty after `init`; `Admin`
   cannot revoke its own role; the two-step transfer cannot orphan the
   contract.

---

## Test plan

Unit tests (`src/test/`), property/fuzz tests (proptest, matching the stream
contract's `lifecycle_proptest` / `props` modules):

- **#421 (auth)**: each setter rejects a caller missing the role; each role can
  perform exactly its powers and no more; two-step admin transfer rejects a
  nominee that never accepts and cannot be hijacked; `CannotRevokeSelf` fires;
  pause is fail-closed. Property: a randomly generated caller/role pair is
  authorized iff the role table says so.
- **#420 (precision)**: fuzz `(amount_in, numerator, denominator)` across the
  `i128` range with edge-weighted distributions near zero, near `i128::MAX`, and
  with `denominator` sharing large common factors with `amount_in`. Assert
  conservation, monotonicity, no-trap, and that the split path agrees with the
  single-shot path when both apply. Property: `convert(x) ⊕ convert(y)` and
  `convert(x + y)` differ by at most `d2 - 1` residue units.
- **#425 (errors)**: each error variant maps to its documented discriminant and
  range; `validate-doc-alignment.py` keeps this table in sync; every error path
  emits exactly one diagnostic event with the documented `ctx` fields and
  performs no preceding state mutation (CEI) — asserted with a test that
  triggers each failure mode and inspects emitted events.
- **Aggregation**: staleness drops old reports; deviation drops outliers;
  `InsufficientSources` when survivors < `min_sources`; median is correct for
  odd/even source counts; a paused pair returns `PairPaused`.
- **Footprint**: `get_price` issues no writes; cost is independent of registered
  pair count (mirrors `resource_limits::cost_is_independent_of_how_many_streams_exist`).

---

## Public surface

| Entry point | Auth | Behavior |
|---|---|---|
| `init(admin)` | one-time | set admin; empty role map (admin implied). |
| `grant_role(addr, role)` / `revoke_role(addr, role)` | `Admin` | mutate role map; emit event. |
| `propose_admin(new)` / `accept_admin()` | `Admin` / nominee | two-step transfer. |
| `add_source(pair, source)` / `remove_source(pair, addr)` | `OracleManager` | mutate pair registry. |
| `set_pair_config(pair, cfg)` | `ParameterManager` | set staleness/deviation/min_sources. |
| `set_pair_paused(pair, bool)` / `global_pause(bool)` | `EmergencyGuardian` | fail-closed pause. |
| `submit_price(pair, report)` | `OracleManager` or configured submitter | optional push-cache write. |
| `get_price(pair)` | view | aggregate sources → `CanonicalPrice` or aggregation error. |
| `convert(amount_in, pair)` | view | overflow-safe rational conversion → `(out, residue, price)`. |
| `get_role(addr)` / `is_paused(pair)` | view | role/pause introspection. |

## Error table (modularized)

| Range | Variant examples | Meaning |
|---|---|---|
| 1 | `UnauthorizedRole` | caller lacks the required role. |
| 2 | `NotAdmin` | operation requires the admin seat. |
| 3 | `CannotRevokeSelf` | admin may not revoke its own role. |
| 4 | `InvalidNewAdmin` | nominee is zero address or the contract itself. |
| 5 | `NoPendingAdmin` | `accept_admin` with no nominee staged. |
| 20 | `PairNotFound` | pair has no registered sources. |
| 21 | `DuplicateSource` | source already registered for pair. |
| 22 | `TooManySources` | exceeds `MAX_SOURCES_PER_PAIR`. |
| 23 | `InvalidConfig` | staleness/deviation/min_sources out of bounds. |
| 40 | `InsufficientSources` | survivors below `min_sources`. |
| 41 | `StaleReport` | a source report exceeds the staleness window. |
| 42 | `DeviationExceeded` | a report deviates from the median past the threshold. |
| 43 | `NoSources` | pair registry empty. |
| 60 | `ConversionOverflow` | result genuinely exceeds `i128::MAX`. |
| 61 | `NegativeAmount` | `amount_in < 0`. |
| 62 | `ZeroDenominator` | price denominator is zero (unreachable for reduced prices). |
| 80 | `RouterPaused` | global pause active. |
| 81 | `PairPaused` | per-pair pause active. |

> Final discriminants are assigned at implementation time, kept within the
> documented ranges, and frozen thereafter. This table is the ABI reference the
> alignment script will enforce, mirroring `docs/error.md` for the stream
> contract.
