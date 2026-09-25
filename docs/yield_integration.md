# Yield Integration Research

## Scope and recommendation

Perpetua currently holds each stream's deposited token in the stream contract's
pooled token balance. A stream records fixed principal, schedule, withdrawals,
and authorization flags; it does not record a yield rate or an external asset
position. The contract accepts a token address per stream, so liquidity and
risk must be accounted for **per token**, not as one fungible platform pool.

The safest first architecture is a **separate, permissioned allocator with a
short allowlist of audited vaults**, backed by a conservative on-contract
liquidity reserve. It should initially deploy only demonstrable surplus, keep
all stream accounting denominated in the original token, and treat positive
yield as protocol surplus. Stream recipients and senders must receive exactly
the amounts defined by the existing stream equations; a vault must never be
allowed to change vesting, withdrawal, cancellation, or top-up semantics.

Do not make Blend or another money market a required dependency of the stream
contract until the adapter, loss handling, and live withdrawal behavior have
been audited independently. A vault integration increases the trusted code,
external-call surface, operational dependencies, and consequences of an asset
loss.

## Current accounting boundary

For a token `T`, define the balance and liabilities at ledger `l` as:

```text
B_T(l) = spendable token-T balance held by Perpetua
         + value redeemable from approved token-T vault positions

U_T(l) = sum over streams using T of (deposited - withdrawn)

W_T(l) = immediately authorized outflows under the product rules
```

`U_T` is the contractual principal still owed. It is larger than the current
withdrawable amount because it includes future recipient vesting and any
sender refund that a cancellable stream may request. `W_T` is a stress measure
for reserve sizing, not a replacement for `U_T`.

The integration must preserve these rules:

- Fixed stream math remains unchanged: `vested + refundable == deposited`.
- A withdrawal, cancellation refund, or top-up never depends on a favorable
  vault price, a keeper being online, or a future interest payment.
- Vault shares and pending withdrawals are not counted as spendable liquidity.
- A vault loss cannot reduce a recipient's or sender's contractual claim. If
  the system cannot cover all claims, new yield deployment must stop and an
  incident mode must preserve withdrawals and refunds as far as reserves allow.
- The invariant is evaluated separately for every token address. XLM, a
  stablecoin, and an arbitrary SEP-41 token cannot cross-subsidize one another.

The current contract's token transfer path moves deposits into the contract
address and transfers directly to recipients or senders. There is no generic
admin withdrawal, vault registry, or yield ledger today. Any implementation
therefore needs a new explicit authority and storage model rather than
silently reusing a stream entry field.

## Architectural patterns

### Pattern A: External allocator, explicit vault adapter

A separate allocator watches indexed liabilities and moves only approved
surplus from the token pool into an allowlisted vault. The allocator can be a
contract, or an operational service calling a narrowly scoped allocator
contract. The stream contract remains the source of truth for stream state.

**Advantages**

- Keeps vault protocol code out of the stream accrual path.
- Allows an emergency pause or vault replacement without changing stream math.
- Makes per-token exposure, adapter limits, and deployment policy observable.
- Supports a staged rollout with one vault and one asset first.

**Risks and requirements**

- The stream contract must expose no unrestricted token escape hatch.
- The allocator must not be able to withdraw user principal for itself.
- Every vault call needs bounded amounts, bounded slippage, and an allowlisted
  destination.
- A service-based allocator introduces key management and liveness risk, so
  redemption and emergency recovery cannot depend only on that service.

This is the recommended production boundary, provided the allocator contract
has an independent audit and a tested emergency path.

### Pattern B: Vault shares held directly by the stream contract

The stream contract calls a fixed vault interface and stores per-token share
balances. Deposits are invested and withdrawals redeem shares when reserve
liquidity is insufficient.

**Advantages**

- Solvency calculations are on-chain and less dependent on an off-chain
  reconciler.
- Authorization and exposure limits can be enforced at the source.

**Risks**

- External calls occur in the highest-value contract and enlarge its audit
  surface.
- Vault share pricing, rounding, pausing, and callback behavior become stream
  contract concerns.
- A broken vault can block unrelated stream operations if calls are coupled.
- Migration and upgrade policy become difficult because the current stream
  contract intentionally has no admin or upgradeability.

This is acceptable only for a fixed, minimal adapter interface and a vault
whose behavior is proven on testnet and in failure tests. It is not the first
choice for the existing immutable stream primitive.

### Pattern C: Dedicated per-asset yield pool

A new pool contract owns the token and vault position. Stream contracts hold
claims against that pool, and the pool exposes reserve, deposit, redeem, and
accounting operations.

**Advantages**

- Strong separation between stream lifecycle and capital management.
- One risk domain per asset and vault.
- Easier to migrate a vault or add a second strategy without modifying stream
  math.

**Risks**

- Introduces a second accounting layer and cross-contract claim semantics.
- Requires careful authorization so a pool cannot rewrite stream liabilities.
- Existing streams cannot be moved without a migration plan and explicit user
  protection.

This is the cleanest long-term architecture if Perpetua expects multiple
strategies or assets, but it is a larger product change than a first yield
experiment.

### Pattern D: Off-chain accounting only

An operator lends or invests funds outside the contracts and reports yield to
an indexer or dashboard.

This is not a safe custody architecture. It provides no on-chain solvency
guarantee, gives users no atomic redemption path, and makes the indexer a
financial source of truth. It may be useful for simulation and reporting, but
must not move production stream liquidity.

## Liquidity buffer design

### Reserve tiers

For each token `T`, divide assets into three tiers:

1. **Immediate reserve:** spendable token balance kept in the Perpetua contract
   for normal withdrawals and refunds.
2. **Recovery reserve:** assets that can be redeemed from the approved vault
   within the documented maximum redemption latency. These are not counted as
   immediate liquidity.
3. **Yield exposure:** assets subject to vault withdrawal delay, loss, pause,
   oracle risk, market risk, or strategy loss. This tier is capped.

At all times, the immediate reserve should satisfy:

```text
R_immediate,T >= W_normal,T(H) + F_operational,T
```

where `H` is the redemption and incident response horizon, `W_normal` is the
maximum expected authorized outflow during that horizon, and `F_operational`
covers transaction fees, rounding, reserve measurement error, and a stress
margin.

A conservative initial definition is:

```text
W_normal,T(H) = current withdrawable_T
              + current refundable_T for cancellable streams
              + scheduled vesting due within H
              + expected top-up reversals/refunds within H
```

Do not count future deposits, future yield, or successful vault redemption in
this calculation. For a stronger guarantee, size against a bounded stress
scenario in which all currently authorized cancellable refunds and all
currently withdrawable balances are requested together.

The total asset requirement is stricter:

```text
B_T >= U_T + loss_buffer_T
```

`loss_buffer_T` is first-loss capital or unallocated surplus that absorbs
valuation changes and exit costs. If the vault cannot be marked reliably, its
position should be valued at a conservative redeemable amount, not at a
reported share price.

### Deployment rule

Only invest surplus after reserving both contractual principal and the buffer:

```text
max_deploy_T = max(0, B_spendable,T - R_target,T)
```

A production implementation should enforce the cap on-chain or in the
allocator contract. An off-chain bot may propose a rebalance, but it must not
be able to exceed the cap by configuration error or stale indexer data.

The first release should use a low exposure cap, for example 10% of the
per-token balance or the amount above a 2x stressed-outflow reserve, whichever
is smaller. Raise the cap only after observing real redemption latency and
reconciliation behavior. The percentage is a starting policy, not a solvency
proof.

### Liquidity scenarios that must be simulated

- All currently withdrawable recipient balances are claimed in one ledger.
- Every cancellable sender cancels at once and claims the resulting refund.
- A vault pauses deposits and redemptions for the maximum documented window.
- The vault suffers the maximum configured loss or share-price haircut.
- The indexer is stale, duplicated, or unavailable during a rebalance.
- A token transfer, vault call, or redemption partially fails and the
  transaction rolls back.
- A stream is archived and restored while the token position remains invested.
- Several tokens have simultaneous stress events; no asset is used to cover
  another asset's liability.

## Vault and adapter risk controls

The allowlist should require all of the following before a vault is eligible:

- Audited code, published source, deterministic interface, and a documented
  upgrade/admin model.
- A defined underlying asset and share-price calculation with known rounding.
- Permissionless or reliably permissioned redemption that does not require a
  discretionary operator to return principal.
- Documented maximum withdrawal delay, queue behavior, caps, fees, and pause
  semantics.
- No callback into the stream contract, or a design that is safe under the
  platform's reentrancy rules and checks-effects-interactions ordering.
- Explicit handling for deauthorized trustlines, frozen assets, oracle failure,
  and vault insolvency.
- A testnet deployment exercised through deposit, partial redeem, full redeem,
  loss simulation, pause, recovery, and upgrade/admin transitions.

The adapter should enforce:

- immutable or governance-controlled vault allowlist;
- one underlying token per vault position;
- maximum position size per token and per vault;
- minimum reserve ratio and maximum allocation ratio;
- minimum and maximum rebalance amounts;
- slippage and share-price deviation limits;
- cooldown between rebalances;
- emergency pause for new deposits and vault deployment;
- redemption destination fixed to the Perpetua-controlled account;
- event emission for every deposit, redemption, loss, pause, and parameter
  change.

Vault tokens must not be accepted as stream payment tokens unless the product
explicitly supports them. Streams should continue to promise the original
underlying token, not a vault share whose value can fluctuate.

## Interest and loss distribution

### Recommended policy: yield is surplus

Keep existing stream balances and equations principal-only. On redemption,
calculate realized proceeds in the underlying token:

```text
realized_yield = redeemed_underlying - deposited_underlying
```

After accounting for fees, rounding, and any reserve top-up, realized positive
yield belongs to the protocol reserve or a separately governed treasury. It
must not be silently added to `deposited`, `vested`, `withdrawable`, or
`refundable`.

This policy has important properties:

- Recipients receive the promised schedule, neither less because of a vault
  loss nor more because of an unannounced rate change.
- Senders receive the exact refundable amount defined by cancellation.
- Existing indexers do not need to reinterpret stream events as interest
  events.
- Yield can first replenish a loss buffer or recovery reserve before any
  treasury distribution.

A distribution waterfall should be:

1. Keep enough underlying liquid for the immediate reserve.
2. Replenish any loss buffer and cover accrued vault fees.
3. Reconcile vault shares and realized underlying proceeds.
4. Only then move excess realized yield to the governed surplus account.

### Alternative: pro-rata yield allocation

A future product could allocate yield to streams by time-weighted principal or
by each stream's share of invested balances. This is materially harder:

- paused, cancelled, depleted, and newly created streams need precise cutoff
  rules;
- yield must be distributed without making `vested` non-monotonic;
- partial withdrawals and top-ups need checkpointed indices;
- vault losses need a defined pro-rata or first-loss treatment;
- the ABI, event model, indexer, and user disclosures all change.

Do not implement this by changing `deposited`. Use a separate cumulative index
and explicit `yield_earned` / `yield_withdrawn` fields only after a new product
specification and audit. Until then, protocol-surplus yield is safer and easier
to reconcile.

### Loss waterfall

Losses must never be hidden by optimistic accounting. A proposed waterfall is:

1. absorb ordinary fees and rounding in the yield account;
2. absorb losses with realized yield and the dedicated loss buffer;
3. pause new vault deployment and new risk exposure;
4. use governed first-loss capital if one exists;
5. enter an incident mode with transparent shortfall accounting if assets are
   still insufficient.

No contract should mint a stream claim, rewrite `deposited`, or use another
asset's balance to make a vault loss appear covered.

## Safety parameters

These are proposed starting parameters and must be approved, configured, and
published before implementation. Values should be expressed per token.

| Parameter | Initial policy | Purpose |
|---|---:|---|
| `max_vault_exposure_bps` | 1,000 (10%) | Caps invested balance during initial rollout |
| `min_reserve_ratio_bps` | 2,000 (20%) | Hard floor for liquid balance; stress sizing may require more |
| `reserve_horizon_seconds` | 86,400 | Covers at least one day of modeled outflows and operations |
| `max_redemption_latency_seconds` | 3,600 | Vault must meet this or exposure is treated as unavailable |
| `stress_multiplier_bps` | 20,000 (2.0x) | Multiplies modeled immediate outflows during initial rollout |
| `max_share_price_deviation_bps` | 100 (1%) | Blocks rebalances on unexpected pricing movement |
| `max_rebalance_bps` | 500 (5%) | Limits one rebalance relative to the token pool |
| `rebalance_cooldown_seconds` | 3,600 | Limits churn and stale-data races |
| `loss_buffer_bps` | 500 (5%) of exposure | First-loss capacity before incident mode |
| `vault_allowlist_size` | 1 per token initially | Keeps audit and monitoring scope narrow |
| `oracle_age_seconds` | 300 | Maximum age for any external valuation input |
| `emergency_pause_threshold_bps` | 500 (5%) | Pause new deployment after a measured loss or mismatch |

These values are not universal defaults. Governance must replace them with
asset-specific limits after measuring the vault's real redemption queue,
transaction costs, volatility, and stream outflow distribution. Parameters
must be monotonic in safety: lowering the exposure cap or raising the reserve
requirement must be possible without migrating stream state.

## Reconciliation and monitoring

The indexer and allocator should independently calculate, per token:

- total `deposited`, total `withdrawn`, and `U_T` from stream events;
- current withdrawable and refundable amounts under the deployed ABI;
- Perpetua's spendable token balance;
- vault share balance, conservative redeemable value, pending redemptions,
  fees, and last successful redemption;
- reserve ratio, exposure ratio, valuation age, and reconciliation delta.

A rebalance must fail closed when the indexer checkpoint is stale, a stream
projection is inconsistent, a vault event is missing, or the reconciliation
delta exceeds a configured tolerance. The system should alert on:

- immediate reserve below target;
- exposure above cap;
- redemption latency above the maximum;
- share-price or oracle deviation;
- any negative unexplained reconciliation delta;
- vault pause, upgrade, admin change, or emergency event;
- keeper/allocator key activity outside the expected schedule.

Reconciliation is evidence, not authority: the on-chain reserve and adapter
limits must remain the final safety controls.

## Implementation and rollout gates

1. Specify the per-token liability and reserve formulas in a versioned design
   before writing contract code.
2. Build a simulator that replays stream events and stress scenarios, including
   simultaneous withdrawal and cancellation demand.
3. Implement the adapter or dedicated pool with no changes to existing stream
   accrual semantics.
4. Add invariant and property tests for reserve solvency, rollback on failed
   vault calls, share-price rounding, stale data, and per-token isolation.
5. Deploy one allowlisted vault on testnet and measure actual deposit,
   redemption, pause, and recovery behavior.
6. Complete an independent security audit covering external calls,
   authorization, accounting, loss handling, and emergency controls.
7. Run a bounded mainnet pilot with the initial exposure cap and a published
   monitoring dashboard.
8. Increase exposure only through a recorded governance decision backed by
   reconciliation history and incident drills.

The launch decision for yield must remain separate from the launch decision
for basic streaming. If any vault or allocator gate is incomplete, Perpetua
can operate without yield deployment while preserving the existing stream
liquidity model.
