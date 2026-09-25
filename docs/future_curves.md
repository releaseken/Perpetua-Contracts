# RFC: Future Non-Linear Vesting Curves

## Status

Research and architecture proposal for a future Perpetua deployment. This is
not a change to v1. The current v1 contract remains linear, uses its frozen
`Stream` ABI, and preserves the existing cliff and pause semantics.

## Summary

Perpetua v1 computes cumulative vested value from elapsed stream time:

```text
vested(t) = floor(deposited * elapsed(t) / duration)
refundable(t) = deposited - vested(t)
```

A future curve implementation should replace the linear fraction with a
bounded cumulative progress function, not with per-tick balance mutations:

```text
p(t)       = cumulative progress in [0, 1]
vested(t)  = floor(deposited * p(t))
refundable(t) = deposited - vested(t)
```

The conservation identity then holds by construction:

```text
vested(t) + refundable(t) == deposited
```

The curve must be monotone in the stream clock, equal to zero before the
schedule begins or its payout gate, and reach exactly one at settlement. All
rounding residue stays in `refundable`; it is never accumulated as an
independent, lossy per-interval balance.

## Goals and non-goals

### Goals

- Support step-wise monthly or milestone unlock schedules.
- Support bounded piecewise-linear and smooth decay-style schedules.
- Preserve exact conservation for every timestamp and operation ordering.
- Preserve monotonic vesting across time and state-changing calls.
- Keep execution cost, serialized parameters, and verification behavior
  bounded and auditable.
- Keep v1 streams unchanged and make curve type visible to indexers and users.

### Non-goals

- Arbitrary user-provided bytecode or callbacks for calculating vesting.
- Floating-point arithmetic in contract code.
- Retroactively changing a stream's curve after creation.
- Yield distribution or interest-bearing stream balances.
- Making a curve an excuse to change authorization, token custody, or pause
  behavior.

## Invariants that every curve must satisfy

Let `c = stream_time(stream, now)` be the paused-aware clock already used by
v1. Let `a(c)` be the curve's elapsed time and `P(a)` its normalized cumulative
progress.

### Bounds and conservation

For every representable stream and timestamp:

```text
0 <= P(a) <= 1
0 <= vested(t) <= deposited
0 <= refundable(t) <= deposited
vested(t) + refundable(t) == deposited
```

Use integer arithmetic with a common scale `Q`, for example `Q = 10^18`:

```text
progress_q(a) in [0, Q]
vested = floor(deposited * progress_q(a) / Q)
refundable = deposited - vested
```

The multiplication must be checked before division. If it cannot fit the
chosen integer domain, reject the stream or return the contract's typed
overflow error. Never saturate a multiplication silently, because saturation
could create an apparent claim that is not backed by the original deposit.

### Monotonicity over time

For `c1 <= c2`:

```text
P(a(c1)) <= P(a(c2))
```

A curve may pause progress, as a step curve does between unlocks, but it may
not decrease. A decreasing curve is not a vesting curve; it would make a
recipient's already-withdrawn amount exceed the new earned amount.

### Monotonicity across state changes

At a fixed clock value, any operation that changes curve parameters, deposited
amount, schedule bounds, or pause accounting must not decrease cumulative
vested value:

```text
vested(S_after, t) >= vested(S_before, t)
```

This is the same dangerous invariant identified for v1 top-ups. A new curve
must test it with the ledger clock frozen around `top_up`, `pause`, `resume`,
and cancellation transitions.

### Settlement

At or after the effective end of the curve:

```text
P(a) = Q
vested = deposited
refundable = 0
```

The implementation must explicitly clamp the terminal point to `Q`. An
approximation that ends at `Q - 1` strands a unit of value and violates the
intended settlement behavior even if all intermediate values look reasonable.

### Withdrawal safety

Given cumulative `withdrawn`:

```text
0 <= withdrawn <= vested
withdrawable = max(vested - withdrawn, 0)
```

The curve evaluator does not own or mutate `withdrawn`; lifecycle code keeps
that accounting and transfers only the computed `withdrawable` amount.

## Clock, cliff, and pause semantics

### One paused-aware clock

Every curve uses the existing stream clock:

```text
stream_time(now) = (paused_at.unwrap_or(now)) - paused_total
```

A pause freezes the curve at its current progress. Resuming advances from the
same curve position. Curves must never use wall-clock time directly for accrual
or they will vest through a pause.

### Cliff policy

V1's cliff is a payout gate, not a delayed accrual origin: at the cliff, the
recipient can claim all value accrued since `start_time`. Future curves should
choose one explicit policy and encode it in the curve version:

1. **Gate policy, v1-compatible:** evaluate progress from `start_time`, return
   zero while `stream_time < cliff_time`, and expose all prior curve progress at
   the cliff.
2. **Delayed-start policy:** define the curve origin as `cliff_time`, so no
   progress exists before the cliff.

A stream cannot leave this ambiguous. The selected policy must appear in the
ABI, event metadata, SDK, and user-facing stream summary. The recommended
compatibility rule is to retain gate policy for `curve_id = linear_v1` and use
an explicit curve flag for delayed-start curves.

### Endpoints

Let `start` and `end` be the curve's effective origin and end, with
`duration = end - start > 0`:

```text
x(t) = clamp(stream_time(t) - start, 0, duration)
```

The curve evaluator receives `x` and `duration`, not an unchecked timestamp.
This keeps all curve families subject to the same endpoint and pause rules.

## Candidate curve families

### 1. Linear, retained for compatibility

```text
P(x) = x / duration
```

With `progress_q`:

```text
progress_q = floor(Q * x / duration)
```

At `x == duration`, return `Q` before applying any generic approximation. This
is the v1 behavior generalized behind a curve interface.

### 2. Step-wise unlock schedule

Represent a schedule as sorted unlock points and weights:

```text
[(u_0, w_0), (u_1, w_1), ... (u_n, w_n)]
```

where `0 <= u_i <= duration`, each `w_i > 0`, and:

```text
sum(w_i) == Q
```

The cumulative progress is:

```text
progress_q(x) = sum(w_i for u_i <= x)
```

Example monthly schedule for four equal unlocks:

```text
[(30 days, 0.25Q),
 (60 days, 0.25Q),
 (90 days, 0.25Q),
 (120 days, 0.25Q)]
```

The contract should store weights as integer units whose checked sum is exactly
`Q`. It should reject unsorted points, duplicate points unless the format
explicitly combines them, zero total weight, and a final unlock after the
curve end. A bounded maximum number of points is mandatory because a loop over
untrusted schedule data creates a denial-of-service risk.

Step curves are attractive for payroll and grants because the unlock amount is
predictable and exact. They also create sharp liquidity demand at unlock
boundaries; reserve sizing must stress all claims at the same timestamp.

### 3. Piecewise-linear curve

Represent knots as cumulative progress pairs:

```text
[(x_0, p_0), (x_1, p_1), ... (x_n, p_n)]
```

with:

```text
x_0 = 0, x_n = duration
p_0 = 0, p_n = Q
x_i < x_(i+1)
p_i <= p_(i+1)
```

For `x_i <= x <= x_(i+1)`:

```text
progress_q(x) = p_i
  + floor((p_(i+1) - p_i) * (x - x_i) / (x_(i+1) - x_i))
```

Clamp the result to `[0, Q]` and return `Q` at the terminal knot. This family
can express front-loaded, back-loaded, and plateau schedules without requiring
transcendental functions. It is the recommended general-purpose family if a
step schedule is insufficient.

Its representation must define whether a knot exactly at `x_i` uses the value
from the left or right. The canonical rule should be right-continuous at a
knot, with duplicate x-values forbidden. Tests must cover every knot and the
seconds immediately before and after it.

### 4. Polynomial ease-in/ease-out curves

A small, fixed set of normalized polynomials can provide smooth schedules:

```text
Ease-in:  P(z) = z^2
Ease-out: P(z) = 1 - (1 - z)^2
Smoothstep: P(z) = 3z^2 - 2z^3
```

where `z = x / duration` and `0 <= z <= 1`. These are monotone on the domain
and end exactly at zero and one. They require checked multiplications and a
fixed-point scale, but no lookup table.

Use only a fixed allowlist of formulas. Do not accept arbitrary polynomial
coefficients until the contract can prove monotonicity and bound execution
cost at creation.

### 5. Normalized exponential decay schedule

A decay-style release can be modeled as a monotone cumulative function, not as
an amount that decreases:

```text
P(z) = (1 - exp(-k z)) / (1 - exp(-k))
```

for `k > 0` and `z = x / duration`. This starts quickly and approaches the
terminal value smoothly. Direct exponentials are unsuitable for deterministic
Soroban integer execution unless implemented with a bounded, audited
approximation.

A practical representation is a fixed lookup table of monotone `progress_q`
values at fixed normalized points, with piecewise-linear interpolation. The
constructor must verify all table entries are non-decreasing, first is zero,
last is `Q`, and table length is within the resource limit. The name "decay"
must not imply that a recipient's vested amount can fall; only the release rate
may decay.

## Fixed-point and rounding rules

A curve evaluator must use one canonical integer scale and one rounding rule.
The recommended rule is:

```text
progress_q = floor(P(x) * Q)
vested = floor(deposited * progress_q / Q)
refundable = deposited - vested
```

There are two floors, but conservation remains exact because `refundable` is
derived as the complement. The recipient receives no more than the curve
entitles them to, and the sender receives every residue on cancellation or
settlement according to the existing lifecycle rules.

Do not calculate `refundable` as `floor(deposited * (Q - progress_q) / Q)`;
that can create a rounding gap where the two quantities sum to less than the
deposit. Do not calculate it from a separately approximated reverse curve.

For interpolation, multiply before dividing using checked arithmetic. Where
an intermediate product cannot fit in `i128`, either reject the curve
parameters at creation or use a wider, deterministic intermediate supported by
the contract toolchain. Never use floating point, platform-dependent math, or
unbounded loops.

## Top-ups and curve identity

Top-ups are the hardest compatibility question. V1 top-up extends the end time
while preserving the agreed linear rate, and the implementation must protect
monotonicity at a frozen clock. A future curve version should select one of
these explicit policies:

### Policy A: immutable schedule, top-up creates a new tranche

Keep the original curve unchanged. Store a separate top-up tranche with its
own deposit and schedule, and sum cumulative vested amounts:

```text
vested_total = sum(vested_tranche_j)
refundable_total = deposited_total - vested_total
```

Each tranche must have its own bounded curve and the combined total must be
checked. This is the most semantically precise policy but increases storage,
iteration, and proof costs.

### Policy B: top-up extends the same normalized curve

Increase deposit and extend the schedule according to a documented rule, then
check at the current frozen clock that vested value does not decrease. This is
compact but can surprise users on front-loaded or step curves. A top-up that
moves an already passed unlock or reduces an existing slope must be rejected.

### Policy C: disable top-ups for curve streams

This is the safest initial implementation. Users create another stream for
additional funds. It preserves curve immutability and keeps the evaluator
simple, at the cost of more stream entries and a less convenient UX.

The recommended first release is Policy C, followed by Policy A if demand
justifies the additional storage and gas. Never silently apply v1's duration
extension formula to a non-linear curve.

## Proposed architecture

### Versioned curve interface

Add a pure, bounded evaluator in a new deployment or versioned module:

```text
CurveSpec {
  curve_id: u32
  origin_policy: Gate | DelayedStart
  params: bounded_bytes
}

progress_q(curve_spec, elapsed, duration) -> Result<u128, Error>
```

The evaluator must not access storage, token contracts, authorization, or wall
clock. It receives the already computed paused-aware elapsed time. That makes
it independently property-testable and prevents a curve from changing token
movement semantics.

A curve registry may map `curve_id` to immutable built-in implementations, but
registry entries must not be mutable for existing streams. If governance adds
a new curve, it gets a new ID and a documented code/version hash. A registry
must not allow an admin to reinterpret an existing ID.

### Stream storage and ABI

Do not add optional curve fields to the deployed v1 record in place. The v1
contract ABI and storage encoding are frozen; adding fields or changing field
meaning requires a new deployment and migration plan.

A future record should include either:

```text
curve_id: u32
curve_params: bounded bytes
```

or a compact immutable reference to a validated curve object. The record must
make the curve identity available to `get_stream`, events, SDKs, indexers, and
cross-chain proof consumers. The curve parameters must be authenticated as
part of stream creation and immutable thereafter.

For Policy A tranches, prefer a bounded per-stream tranche vector only if the
maximum count and storage footprint are explicit. Otherwise use separate
stream IDs and let the indexer group them off-chain; on-chain grouping must
never become an unbounded user list.

### Events and off-chain consumers

Creation and top-up events must include curve ID, curve version, origin policy,
and a canonical parameter digest. Consumers must generate decoders from the
new deployed interface. Indexers should store both raw parameters and the
validated normalized representation, then recompute progress using the same
versioned evaluator.

A UI must display the curve shape, unlock points or rate parameters, terminal
amount, and cliff semantics before the recipient accepts a stream. "Non-linear"
is not sufficient user disclosure.

## Resource and safety limits

Initial safety limits should be conservative and enforced at creation:

| Parameter | Proposed initial bound | Reason |
|---|---:|---|
| Built-in curve IDs | small allowlist | Prevents unreviewed formula behavior |
| Step/linear knots | 32 maximum | Bounds loop and storage cost |
| Lookup-table points | 64 maximum | Bounds interpolation work |
| Curve parameter bytes | 256 maximum | Bounds footprint and decoding |
| Fixed-point scale `Q` | one protocol constant | Prevents cross-curve rounding ambiguity |
| Duration | existing timestamp bounds | Avoids overflow and nonsensical schedules |
| Top-ups | disabled initially | Avoids curve mutation hazards |
| Final progress | exactly `Q` | Guarantees full settlement |
| Intermediate arithmetic | checked | Prevents wraparound claims |

These are engineering starting points, not final economics. Resource-limit
benchmarks must be run against the Soroban protocol version targeted by the
release. Reject a curve before writing storage if validation would exceed the
transaction's footprint or instruction budget.

## Verification and testing plan

### Unit and property tests

For every built-in curve and generated valid parameter set, test:

- `progress_q(0) == 0` and `progress_q(duration) == Q`;
- progress is in `[0, Q]` for every input;
- progress is non-decreasing over timestamps;
- `vested + refundable == deposited` exactly for boundary and random values;
- `withdrawn <= vested` after every valid withdrawal sequence;
- no overflow or panic at maximum deposit, duration, parameter, and timestamp;
- pause freezes progress and resume continues at the same curve point;
- cliff behavior matches the declared origin policy;
- every step/knot boundary is deterministic;
- a failed curve validation leaves storage and token balances unchanged.

For state transitions, freeze the ledger timestamp and test that `vested` does
not decrease across create-related migration logic, top-ups, pause/resume,
recipient transfer, cancellation, and any future parameter operation.

### Differential and reference testing

Implement a small reference evaluator outside the contract using rational
arithmetic or a high-precision decimal library. Compare the contract's fixed
point result against the reference, allowing only the documented downward
rounding. Include vectors for every curve version in the repository and in the
release artifact.

### Cross-component tests

- The SDK and indexer decode the same curve ID and parameters as the contract.
- Event replay reconstructs the same curve state as direct storage reads.
- A proof consumer can verify the curve metadata and independently recompute
  the claimed vested amount.
- Migration tests prove v1 linear streams retain their exact old result.
- Resource tests prove the maximum accepted curve stays within budget and the
  first rejected size fails cleanly.

## Migration and rollout

1. Keep v1 linear streams unchanged and document them as `linear_v1`.
2. Publish the curve RFC, fixed-point constant, curve IDs, and test vectors.
3. Implement pure evaluators and exhaustive property tests before contract
   integration.
4. Deploy a new contract address or explicitly versioned curve module; never
   reinterpret existing v1 storage.
5. Start with one curve family, preferably bounded piecewise-linear or steps,
   and disable top-ups for curve streams.
6. Exercise cliff, pause, cancellation, settlement, indexer replay, and
   failed-transfer behavior on testnet.
7. Obtain an independent audit focused on fixed-point arithmetic, monotonicity,
   storage bounds, and ABI/event compatibility.
8. Add curve streams to release and cross-chain proof checklists only after
   their curve ID and parameter digest are part of the attested state.

## Open questions

- Should a delayed-start cliff be a separate curve origin policy or a separate
  curve ID for simpler downstream decoding?
- Is the product willing to accept separate stream IDs instead of top-ups for
  the first curve release?
- Should step schedules use absolute unlock timestamps, normalized offsets, or
  explicit monthly periods with calendar semantics? Normalized offsets are less
  ambiguous and cheaper to verify.
- What fixed-point scale provides adequate precision without making maximum
  multiplication unsafe under the target Soroban SDK?
- Does the indexer need a canonical preview endpoint for future claimable value,
  or should every consumer run the versioned evaluator locally?

## RFC decision request

Approve the following direction for implementation planning:

1. Retain v1 unchanged as `linear_v1`.
2. Define all future curves as bounded, immutable, cumulative progress
   functions over the paused-aware stream clock.
3. Derive `refundable` as `deposited - vested`, never as an independently
   rounded curve.
4. Start with a fixed allowlist and bounded piecewise-linear or step curves.
5. Disable top-ups for curve streams in the first release.
6. Require new ABI/storage versioning, generated events, property tests, and an
   independent audit before deployment.
