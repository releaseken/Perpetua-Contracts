# Design — Hardening `rent-manager` and `multisig-threshold`

Issues: **#443**, **#442** (`contracts/rent-manager`) and **#433**, **#436** (`contracts/multisig-threshold`).

| Issue | Subsystem | Theme |
|-------|-----------|-------|
| #443 | rent-manager | Modularize error types + custom error messages for client observability |
| #442 | rent-manager | Strict invariant assertions against re-entrancy and state desync |
| #436 | multisig-threshold | Streamline cross-contract invocation (CCI) overhead in high-frequency batch claims |
| #433 | multisig-threshold | Strict invariant assertions against re-entrancy and state desync |

---

## 0. Scope caveat and target mapping (read first)

The directories `contracts/rent-manager` and `contracts/multisig-threshold` **do not exist** in this workspace. What exists under `contracts/` is: `archival-probe`, `factory`, `governance`, `stream`, `stream-nft`.

This design is therefore written against the *requirements as stated in the issues*, and is grounded in the patterns this repository has already proven, so it is concrete and applicable either way:

- If `rent-manager` / `multisig-threshold` are **greenfield contracts**, the design specifies what they must contain and conform to. Each design decision cites the in-repo contract that already implements the pattern, so an implementer has a working reference rather than a prescription on paper.
- If the issues are intended to **map onto existing contracts**, the natural mapping is:
  - `multisig-threshold` ↔ `contracts/governance` — it already has co-signers, a quorum threshold, weighted approvals, an in-flight `ReentrancyGuard`, signer-generation snapshots, and batch dispatch. The governance code is the closest in-repo analog and the source for every pattern in §4.
  - `rent-manager` ↔ `contracts/stream` — a continuous-payment/escrow primitive with deposits, accrual, withdrawal, cancellation/refund, and a token sub-invocation. The stream code is the source for every pattern in §3.

**This design does not assume a specific mapping.** It is written so that applying it to a greenfield contract or to the existing analog produces the same shape. The one open decision — which is real and must be confirmed before implementation — is recorded in §6.

Throughout, in-repo precedent is cited as `contracts/<crate>/src/<file>.rs :: <symbol>`.

---

## 1. Goals and non-goals

### Goals
- A single, append-only, `#[contracterror]` ABI surface per contract whose discriminants never renumber and whose variants are grouped by domain, each carrying a custom message that tells a client *what went wrong and what to do* (issue #443).
- Deterministic, matchable errors on every entry point (no `panic!` on the hot path; `panic_with_error!` only on unauthenticated view preconditions, matching the factory's convention).
- Cross-contract failures bucketed into stable, contract-local categories rather than forwarding foreign discriminants (the stream's `TokenTransferFailed` / `TokenMissing` discipline).
- Machine-checkable conservation and state invariants that run in the test profile and compile out of the release WASM (issue #442 / #433).
- A re-entrancy control set appropriate to Soroban's actual execution model (§5), not the EVM's.
- A batch-claim path that amortizes authorization, storage round-trips, and TTL bumps, with a *bounded, declarable* footprint (issue #436).

### Non-goals
- Changing the public ABI discriminants of any existing, deployed contract. The stream contract's `Error` doc is explicit: *"Discriminants are part of the public ABI. Never renumber an existing variant; only append."* This design honors that for any contract that already ships.
- Introducing an upgrade/admin path into the (deliberately immutable) stream contract. Rent-manager, if greenfield, inherits the same immutability stance unless its requirements say otherwise.
- Off-chain indexer or SDK changes. Observability here is on-chain contract behavior; indexer parity is a separate workstream (the stream already couples that via `script/abi-drift-check.py`).

---

## 2. Shared foundation — patterns already proven in this repo

Every decision below reuses a pattern that is already shipping and tested here. Implementers should read the cited code, not just this summary.

| Pattern | In-repo precedent | Used by this design in |
|---|---|---|
| `#[contracterror] #[repr(u32)]` enum, append-only discriminants, section comments, rich per-variant docs | `contracts/stream/src/error.rs :: Error` (34 variants, ABI-frozen) | §3.4, §4.3 |
| Bucketing foreign (token) errors into stable local categories; never forward raw discriminants | `contracts/stream/src/lib.rs :: token_transfer` (`TokenTransferFailed` / `TokenMissing`) | §3.4, §3.6 |
| `panic_with_error!` only on unauthenticated view preconditions; `Result<T, E>` on all mutating paths | `contracts/factory/src/lib.rs :: get_factory_config`, `is_allowlisted` | §3.4, §4.3 |
| Re-entrancy guard via an instance `Executing` flag + a dedicated error variant | `contracts/governance/src/lib.rs :: DataKey::Executing`, `GovernanceError::ReentrancyGuard` (=23) | §3.6, §4.6 |
| CEI: persist effects **before** the external dispatch, rely on host rollback only after | `contracts/governance/src/lib.rs :: execute` ("after the proposal has been marked executed (CEI)"); `contracts/stream/src/lib.rs :: create_stream` | §3.6, §4.4, §4.6 |
| Conservation invariants as `debug_assert!` (compile out of release) | `contracts/stream/src/lib.rs :: cancel` (`refund + vested == deposited`) | §3.5, §4.5 |
| Pre-emission invariant helpers returning typed errors | `contracts/stream/src/events.rs :: check_withdrawn_invariants`, `check_cancelled_invariants` | §3.5, §4.5 |
| Monotonic ids via `checked_add`, never wrapping | `contracts/stream/src/storage.rs :: allocate_stream_id` | §3.3, §4.3 |
| TTL: instance pinned to network `max_ttl()`; persistent entries targeted to remaining lifetime + buffer, floored | `contracts/stream/src/storage.rs :: extend_instance`, `ttl_target_ledgers`; `contracts/governance/src/lib.rs :: bump_active_entry` | §3.3, §4.3 |
| Snapshot decision-relevant config at decision time; reject if generation changed | `contracts/governance/src/lib.rs :: Proposal::signer_generation`, `QuorumInfo::threshold`, `InvalidSignerGeneration` | §4.5, §4.6 |
| Batch: validate **all** before mutating **any**; one `require_auth_for_args` covering the whole batch; bounded size | `contracts/stream/src/lib.rs :: batch_withdraw` (`MAX_BATCH_SIZE = 16`) | §4.4 |
| Event/storage parity by reading off the settled state, not by re-passing parameters | `contracts/stream/src/events.rs :: cancelled` ("`vested` is deliberately not a parameter") | §3.5, §4.5 |
| `proptest` + lifecycle + race + atomicity + hardening test files | `contracts/stream/src/test/{invariants,lifecycle_proptest,race_cancel_withdraw,withdrawal_atomicity,hardening_177_181_184_190}.rs` | §3.7, §4.7 |

### Toolchain facts that constrain the design
From `contracts/stream/Cargo.toml`: `soroban-sdk = "27.0.6"` (Stellar Protocol 27), `rust-version = "1.97.1"`, release profile has `debug-assertions = false` and `overflow-checks = true`. Consequences:
- `debug_assert!` invariants cost zero in the deployed WASM but run under the test profile and under `release-with-logs`. Use them freely for conservation and state invariants.
- `overflow-checks = true` in release means integer overflow in the generated code traps — but the contracts nonetheless use explicit `checked_*` so the failure is a *typed* `Error::Overflow` the client can match, not a host abort. That distinction is the whole point of #443's "client observability": a trap is unobservable; a typed error is matchable.

---

## 3. Subsystem: `rent-manager`

### 3.1 Problem
#443 wants modularized error types with custom, client-actionable messages. #442 wants strict invariants guarding against re-entrancy and state desync. Rent-manager is a funds-custody primitive: a payer deposits rent, it accrues to a lessor over the lease term, with cancellation/refund and withdrawal. Structurally it is the stream contract's sibling, so the same failure modes apply: token sub-invocations, conservation of custody, schedule collapse on cancellation, and bounded batches.

### 3.2 Approach
Adopt the stream contract's architecture wholesale, then *modularize the implementation without touching the ABI surface*:

- Keep one public `#[contracterror] Error` enum with `#[repr(u32)]` discriminants — the ABI contract — and an `ABI_VERSION` constant mirroring `contracts/stream/src/lib.rs :: ABI_VERSION`.
- Split the *validation logic* into per-domain modules (`validation`, `schedule`, `custody`), each exposing strongly-typed sub-enums that convert into the public `Error` via `From<SubError> for Error`. The public enum stays flat and append-only; the implementation is organized by concern. This is "modularize error types" done correctly: the client-facing surface is unchanged and stable, the internal call sites are typed and self-documenting.
- For *custom error messages / observability*: emit a **diagnostic log** with the offending inputs at the failure site before returning the typed error. In Soroban, `env.logs()` writes to the transaction's diagnostic events, which are emitted only when diagnostic mode is on and **cost nothing in production**. This is the channel for "custom error messages for client observability" that does not bloat the public event stream or the ABI. The typed `Error` remains the machine-matchable return; the log is the human/forensic detail.

### 3.3 Data model & storage
Mirror `contracts/stream/src/types` (`DataKey`, `Stream`, `StreamStatus`, `DelegateGrant`) and the storage layer:

```
DataKey::NextRentId        // instance, pinned to max_ttl, monotonic via checked_add
DataKey::Rent(u64)         // persistent, lifetime-targeted TTL (ttl_target_ledgers)
DataKey::Delegate(...)     // persistent, max_ttl while active
```

- `allocate_rent_id` = `allocate_stream_id`'s twin: `checked_add(1)` with `Error::RentIdExhausted` / `Error::RentIdOverflow` on the two failure modes (the stream contract deliberately separates "counter at u64::MAX" from "increment would overflow"; see `Error::StreamIdExhausted = 24` vs `Error::StreamIdOverflow = 33`). Rent-manager inherits that separation.
- `load_rent` (extends TTL) vs `peek_rent` (view-only) — exactly `load_stream` / `peek_stream`.
- Instance entry always at `max_ttl()`; persistent entries at `ttl_target_ledgers`. The ordering invariant (instance cannot expire before the entries it issued) carries over verbatim from `contracts/stream/src/storage.rs`.

### 3.4 API surface & error taxonomy
One public `Error` enum, grouped by domain with section comments, each variant documented with *cause* and *remediation*. Sketch (discriminants illustrative only; an existing contract must *append*, never renumber):

```rust
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    // --- Lookup ---
    RentNotFound = 1,

    // --- Creation validation ---
    InvalidTimeRange, InvalidLeaseTerm, InvalidDeposit, DepositRateTooLow, SelfRent,

    // --- Authorization / capability ---
    Unauthorized, NotCancellable, NotPausable, NotTransferable,

    // --- State machine ---
    RentNotActive, RentNotPaused, RentAlreadyPaused, RentTerminated, RentMatured,

    // --- Withdrawal ---
    InsufficientWithdrawable, NothingToWithdraw, InvalidAmount,

    // --- Resource limits ---
    BatchTooLarge, EmptyBatch, DuplicateRentId, MalformedRentId,

    // --- Arithmetic ---
    Overflow, TopUpTooSmall,

    // --- Identifier exhaustion ---
    RentIdExhausted, RentIdOverflow,

    // --- Token sub-invocation (bucketed, not forwarded) ---
    TokenTransferFailed, TokenMissing,

    // --- Delegation / transfer ---
    DelegateNotPermitted, DelegateExpired, RepeatedTransfer,
}
```

Rules, each lifted from the stream contract's `error.rs` and `lib.rs`:

1. **Bucket foreign errors.** A rent-manager that pulls/pushes tokens must call `token::TokenClient::new(env, token).try_transfer(...)` and map to exactly two categories — `TokenTransferFailed` (token returned a typed contract error) and `TokenMissing` (host `Abort`, callee has no code) — discarding the token's raw discriminant. This is the #443 "observability" decision: forwarding the token's `#N` would let a client decode it against rent-manager's table and silently misinterpret it (token's `#7` reading as `Unauthorized`). The raw cause survives in the transaction's `diagnosticEvents`; the ABI stays clean. Cite: `contracts/stream/src/lib.rs :: token_transfer`.
2. **`Result<T, Error>` on every mutating entry point; `panic_with_error!` only for unauthenticated view preconditions** (uninitialized contract). Cite: `contracts/factory/src/lib.rs :: get_factory_config`.
3. **Append-only.** A doc comment on the enum states the invariant and points at `ABI_VERSION`; adding a variant bumps no existing number. Cite: `contracts/stream/src/error.rs` ("only append").
4. **Custom messages via `env.logs()`.** At each failure site, before `return Err(...)`, emit a diagnostic log naming the failing check and the offending values (e.g. `env.logs().add("rent: deposit 0 rejected for term N");`). These are invisible in production (diagnostics off) and free of public-ABI cost; they are the "custom error messages" channel. Do **not** put human strings into the `Error` enum — the enum is the machine surface, the log is the human surface.

### 3.5 Invariants (issue #442)
Two layers, both present in the stream contract:

**Layer 1 — conservation, as `debug_assert!` at the state-transition site.** For rent custody the identity is:

```
deposited == withdrawn_to_lessor + refunded_to_payer + outstanding
```

or, in the collapse-on-cancel form the stream uses,

```
refunded + vested_now == pre_cancel_deposited      // conservation
vested_now >= withdrawn                            // no clawback of earned funds
```

`debug_assert!` these at the cancel/refund site exactly as `cancel` does (`contracts/stream/src/lib.rs`), so they run in test and `release-with-logs` and compile out of release. Name the invariant in the failure message ("rent: conservation broken — …") for forensic clarity.

**Layer 2 — pre-emission invariant helpers returning typed errors.** Mirror `contracts/stream/src/events.rs :: check_withdrawn_invariants` / `check_cancelled_invariants`: pure functions that validate the state a transition is about to publish and return `Error::InvalidAmount` / `Error::Overflow` instead of emitting a corrupt event. Call them before every `.publish(env)`.

**Monotonicity and non-reuse.** Rent ids are monotonic and never reused (`allocate_rent_id` with `checked_add`), so a restored instance resumes without id collision. This is a state-desync guard: it is what makes "needs restoring" distinguishable from "never existed" (see `stream_exists` + `next_stream_id` in `storage.rs`).

**Event/storage parity.** The `Cancelled`-style event reads `vested` off the *settled* stream rather than taking it as a parameter (`contracts/stream/src/events.rs :: cancelled`), which makes event/storage divergence *unrepresentable*. Rent-manager's cancel/refund event follows the same rule: read the post-collapse figures off the saved record, pass only the token-movement amount.

### 3.6 Re-entrancy & state-desync controls (issue #442)
Soroban does **not** block a contract from being re-entered via a cross-contract call (A→token→A is allowed); transaction atomicity only helps *after* a failure, it does not stop a malicious callee from observing half-updated state mid-call. The protection is therefore CEI + an in-flight guard, not host-level re-entrancy blocking (see §5).

1. **CEI on every state-changing entry point.** Apply all checks and write all storage **before** the token sub-invocation, then rely on host rollback if the transfer fails. The stream's `create_stream` allocates the id before the transfer and saves the stream after; the doc explicitly notes the failed transfer rolls back the id allocation. Rent-manager does the same and documents the reliance. The one subtlety: clone the token address and the acting party out of the record *before* the external call (the stream does `let token = stream.token.clone(); let sender = stream.sender.clone();`), so the call never reads through a borrow that a hypothetical callback could have mutated.
2. **In-flight guard on the cancel/refund path** (the only rent-manager path that both writes state and calls out to a token, where a malicious token could call back). Adopt the governance `Executing`-flag pattern: set an instance flag at entry, clear on exit, reject re-entry with `Error::ReentrancyGuard` (a new variant; the governance contract already ships `GovernanceError::ReentrancyGuard = 23` as the model). Because the guard is an instance entry it must itself be TTL-pinned to `max_ttl()`.
3. **Read-after-write discipline.** Never cache a `Rent` record across an external call; reload from storage after any sub-invocation. Storage reads are cheap; a stale cache is the classic desync vector.
4. **No `unwrap`/`expect`/`panic` on the hot path.** Every `Option`/`Result` from storage or arithmetic becomes a typed `Error`. `overflow-checks = true` already traps in release, but the contracts use `checked_*` so the trap is a matchable error, not an abort.

### 3.7 Testing (issue #442 / #443)
Reuse the stream test taxonomy and add the issue-specific ones. New/required tests:

- `error_discriminants.rs` — every variant's `as u32` is stable and unique; appending is the only legal change. (Stream already ships this.)
- `token_errors.rs` — a token that returns a typed error maps to `TokenTransferFailed`; a missing-token `Abort` maps to `TokenMissing`; the raw discriminant is never observable in the contract error.
- `invariants.rs` — the conservation identity holds across create / top-up / withdraw / cancel, including partially-withdrawn-then-cancelled and pre-cliff-cancel.
- `withdrawal_atomicity.rs` / `race_cancel_withdraw.rs` — a withdrawal and a cancel racing on the same rent resolve to a consistent state; neither creates nor destroys funds.
- `reentrancy.rs` — a token whose transfer callback re-enters rent-manager's cancel is rejected with `ReentrancyGuard` and leaves state unchanged.
- `lifecycle_proptest.rs` — `proptest` driving random sequences of top-up/withdraw/pause/resume/cancel, asserting the conservation identity after every step. (Stream ships a `lifecycle_proptest.rs`; rent-manager needs the equivalent.)
- `resource_limits.rs` — the batch cap rejects oversize batches with `BatchTooLarge`; the Nth rent costs what the 2nd did (stream's "153rd stream costs what the 2nd did" property).

---

## 4. Subsystem: `multisig-threshold`

### 4.1 Problem
#433 wants strict invariants against re-entrancy and state desync. #436 wants the cross-contract invocation overhead streamlined during *high-frequency batch claims*. The governance contract is the in-repo analog and already implements most of #433's surface (`ReentrancyGuard`, `signer_generation` snapshot, `QuorumInfo::threshold` snapshot, CEI around dispatch). #436's batch-claim optimization is the net-new performance work.

### 4.2 Approach
Inherit the governance contract's security structure (it already closes most of #433), and focus the new design on #436: a **batch-claim** entry point that lets an authorized claimant discharge many claims in one transaction while amortizing the per-claim overhead. The governing principle, lifted from `contracts/stream/src/lib.rs :: batch_withdraw`, is *validate-all-then-mutate-any* with one authorization for the whole batch.

### 4.3 Data model & storage
Mirror `contracts/governance/src/lib.rs`:

```
DataKey::Admin              // instance, max_ttl
DataKey::Signers            // instance
DataKey::SignerIndex        // instance, Map<Address,bool> for O(1) membership
DataKey::SignerWeights      // instance
DataKey::Threshold          // instance
DataKey::SignerGeneration   // instance, monotonic — invalidates stale proposals
DataKey::Executing          // instance, the reentrancy guard
DataKey::NextProposalId     // instance, monotonic via checked_add
DataKey::Proposal(u32)      // persistent, retention policy per lifecycle state
DataKey::QuorumReachedAt(u32)
DataKey::ProposalApprovalIdx(u32)  // Map<Address,bool>, O(1) duplicate-approval
```

Three things from the governance code are load-bearing for #433 and must be preserved verbatim:

1. **`SignerGeneration` ≠ `NextProposalId` slot.** Governance's `DataKey` doc records that `SignerGeneration` is explicitly `8`, *not* the implicit `3` that would follow `Threshold`, because `3` is `NextProposalId` and sharing the slot would make `increment_proposal_id` bump the generation and invalidate every queued proposal. This is a state-desync footgun that was already fixed once; the design must not reintroduce it.
2. **Weighted approvals accumulated with `checked_add`.** `approval_weight` is overflow-safe and *snapshots the weights in effect when each approval was cast* — a later `set_vote_weight` does not retroactively change a queued proposal's quorum.
3. **Threshold snapshot at quorum.** `QuorumInfo::threshold` is captured when quorum is first reached, so a mid-flight threshold change cannot rush or block execution.

### 4.4 Batch-claim optimization (issue #436)
"High-frequency batch claims" = many claim operations dispatched in one transaction. The overhead to remove is per-iteration authorization, per-iteration storage round-trips, per-iteration TTL bumps, and per-iteration client instantiation. The design, in order of impact:

1. **One authorization for the whole batch.** Use `claimant.require_auth_for_args((batch_ids.clone(),))` — a single signature covering the entire batch tuple — exactly as `batch_withdraw` does with `recipient.require_auth_for_args((stream_ids,))`. This is the dominant gas win for high-frequency claims and the one most likely to matter at scale.
2. **Validate all, then mutate any.** Two passes:
   - *Resolve pass:* for every id, `peek` the proposal, verify the claimant is authorized, verify quorum/timelock/generation, compute the dispatch inputs, and accumulate any totals with `checked_add`. No storage writes, no external calls. Any failure aborts the whole batch with no partial effects (host atomicity).
   - *Apply pass:* for each id, perform the CEI-ordered mutation (mark executed → set `Executing` → dispatch → clear `Executing`) using the already-resolved inputs. This avoids a second storage round-trip per item.
3. **Bound the batch; declare the footprint.** A dynamic batch cannot have its storage footprint fully pre-declared, which is the real Soroban constraint (not instructions, not bytes necessarily). Cap the batch at a constant `MAX_BATCH_CLAIMS` chosen like the stream's `MAX_BATCH_SIZE = 16`: measured against the network's footprint/writes/event budgets with 2× headroom, and documented with the binding constraint. Oversize batches return `Error::BatchTooLarge`; the SDK chunks. Cite the stream's measured-cap rationale (`contracts/stream/src/lib.rs :: MAX_BATCH_SIZE`).
4. **Amortize TTL and client work.** Defer the instance TTL bump to a single call after the batch (the stream's `batch_withdraw` does `Self::bump_instance_ttl(&env)` once, gated on `total > 0`). Where multiple claims hit the same target contract, reuse a single client handle across iterations rather than constructing one per item (the stream's `token_transfer` constructs a `TokenClient::new` per call; a batch path that knows its targets up front can hoist this). Do **not** defer persistent TTL for *active* (non-terminal) proposals — the governance retention policy (#52) must still hold, because letting an active proposal archive mid-batch is a state-desync bug. Apply `bump_active_entry` per touched active proposal as today; only the *instance* bump is amortized.
5. **Checked arithmetic for the running totals.** `total = total.checked_add(amount).ok_or(Error::ArithmeticOverflow)?` — governance already does this for `approval_weight`; the batch totals do the same.

### 4.5 Invariants (issue #433)
Inherit governance's invariants and make them explicit as `debug_assert!`:

- **Quorum snapshot:** `executed proposal.approval_weight >= quorum_threshold_snapshot`. The threshold used is the one stored in `QuorumInfo` at quorum time, not the live `Threshold` — defends against admin threshold changes mid-flight.
- **Generation consistency:** `proposal.signer_generation == current SignerGeneration`, else `InvalidSignerGeneration`. Defends against a signer-set change desyncing a queued proposal.
- **No double-approval / no double-execute:** `ProposalApprovalIdx` (per-proposal `Map<Address,bool>`) prevents a signer approving twice; the `executed`/`cancelled` flags + `status` prevent double execution. Both are state-desync guards.
- **Weight conservation:** `approval_weight == Σ signer weight of distinct approvers`, accumulated with `checked_add`. No approval can change the weight of an already-cast approval.
- **Pre-emission helpers** (mirror `check_withdrawn_invariants`): validate the `ProposalExecuted` payload against the persisted proposal before `.publish(env)`, so the event and storage cannot disagree.

### 4.6 Re-entrancy & state-desync controls (issue #433)
1. **The `Executing` guard already exists** (`GovernanceError::ReentrancyGuard = 23`, `DataKey::Executing`). The design extends it to **every entry point that dispatches to an external contract** — `execute`, batch execute, and the new batch-claim path — not only the single execute path. Set on entry, clear on exit, reject re-entry with `ReentrancyGuard`.
2. **CEI for execute:** mark the proposal `Executed` **before** the target dispatch (governance already does: *"Called inside `execute` after the proposal has been marked executed (CEI)"*). A re-entrant `execute` then sees `status == Executed` and is rejected with `AlreadyExecuted` — the guard is belt-and-suspenders to the status check.
3. **Snapshot-then-dispatch:** read and snapshot all decision inputs (threshold, signer generation, approval set) before any external call; never read a governance parameter *across* an external call. This is the rule that makes the threshold-snapshot and generation-snapshot defenses actually hold.
4. **No external call while holding a borrow** of a storage-backed value; clone the needed fields out first, as the stream does with `token`/`sender`.
5. **Terminal retention is one-shot:** when a proposal becomes terminal, grant the 30-day floor once (`seal_terminal_proposal` in governance) and never top it up again, so terminal-proposal rent decays instead of growing unbounded. This is a cost invariant, but it is also a state-desync guard: it prevents a terminal proposal from being kept alive long enough to be "resurrected" by a stale path.

### 4.7 Testing (issue #433 / #436)
- `reentrancy.rs` — a target contract whose dispatch re-enters `execute` / batch-claim is rejected with `ReentrancyGuard`; state is unchanged; no double execution.
- `threshold_snapshot.rs` — quorum reached at threshold T; admin changes threshold to T' before execution; execution uses T (the snapshot), not T', in both directions (lower-to-rush and raise-to-block).
- `signer_generation.rs` — a signer added/removed after a proposal is queued invalidates it (`InvalidSignerGeneration`); an already-executed proposal is not retroactively affected.
- `batch_claim.rs` — a full batch at the cap executes all-or-nothing; one bad id reverts the whole batch including already-applied claims; oversize returns `BatchTooLarge`.
- `batch_claim_overhead.rs` (the #436 measurement) — assert the batch path's instruction/footprint/writes/event cost is sub-linear in batch size (single auth, single instance bump, hoisted client), and that it stays within 2× headroom of the network limits at the cap. This is the analog of the stream's `resource_limits.rs` + `release_profile.rs`.
- `lifecycle_proptest.rs` — random propose/approve/cancel/execute sequences, asserting the quorum-snapshot and generation invariants after every step.

---

## 5. Cross-cutting: Soroban's re-entrancy model, and why these controls are the right ones

The EVM re-entrancy intuition ("the contract calls an attacker, the attacker calls back into the contract while its state is half-updated, funds are drained") does not map 1:1 onto Soroban. What is and isn't true:

- **Single-threaded, one invocation tree per transaction.** There is no concurrent re-entry.
- **A contract *can* be re-entered.** `A → B → A` is a legal cross-contract call graph. If `B` is a malicious/custom token that calls back into `A` during `A`'s token transfer, `A` sees its own storage *as `A` left it before the call to `B`* — which is exactly the half-updated state the EVM attack exploits.
- **Host atomicity protects *after* a failure, not *during*.** If the callback traps, the whole transaction reverts; but if the callback *succeeds* while state is half-updated, the damage is done within the same transaction and is not rolled back.

Therefore the correct controls are the ones this repo already uses and that this design prescribes:

1. **CEI** so that, by the time any external call is made, all effects that a callback could exploit are already persisted. A callback then observes a fully-updated state, not a half-updated one.
2. **An in-flight guard** so that a callback re-entering the *same* entry point is rejected before it can act on the half-updated state at all (governance's `Executing` + `ReentrancyGuard`).
3. **Snapshot-then-dispatch** so that decision inputs read before the call cannot be changed by the call's effects mid-flight (threshold/generation snapshots).
4. **Read-after-write discipline** so a post-call read reflects persisted state, not a stale cache.

Controls that are *not* needed here and that this design deliberately does **not** add: a mutex/lock beyond the single boolean guard (single-threaded), and a global non-reentrancy flag that blocks benign recursion (governance legitimately recurses through `dispatch_call`/`dispatch_operation`; only the external-dispatch surface is guarded).

---

## 6. Open questions / decisions to confirm before implementation

1. **Target mapping.** Are `rent-manager` and `multisig-threshold` greenfield crates, or do they map onto `contracts/stream` and `contracts/governance`? This design is written to apply to both, but the implementer must confirm. If they map to existing contracts, the "append-only, never renumber" ABI constraint is binding and the error-taxonomy work in §3.4/§4.3 becomes *additive only*.
2. **`env.logs()` availability in the target SDK profile.** The observability design in §3.4 leans on `env.logs()` emitting only under diagnostic mode. Confirm the deployed network and the SDK version (27.0.6 here) expose it as expected; if not, fall back to emitting the offending inputs as a dedicated `#[contractevent]` that clients opt into (higher cost; not the default path).
3. **`MAX_BATCH_CLAIMS` value.** Must be *measured*, not chosen, against Protocol 27 mainnet limits — the same way the stream's `MAX_BATCH_SIZE = 16` was. The binding constraint is the storage footprint (which must be declarable), not instructions or event bytes, though those are checked too. Pick the value after running the `batch_claim_overhead.rs` measurement test.
4. **Whether rent-manager is immutable like the stream.** The stream contract's whole security argument is its immutability ("no admin key, no upgrade path, no fee switch"). If rent-manager must have an admin (e.g. for allowlist), inherit the factory's two-step `propose_admin`/`accept_admin` rotation and `is_forbidden_admin` zero/self guard rather than single-step `set_admin`.

---

## 7. Out of scope

- Off-chain indexer/SDK changes and `script/abi-drift-check.py` parity wiring (separate workstream; the stream contract already couples these).
- Changing any deployed contract's existing discriminants.
- Adding an upgrade path to the stream contract.
- Formal-verification harness (the issues mention "formal verification standards"; this design provides the *invariants* a harness would check, not the harness itself — that is a follow-up once the contracts and their exact line numbers exist).
- Gas benchmarking of the existing contracts (the stream already ships `release_profile.rs` / `gas_benchmark.rs`; the new measurement work is only the #436 batch-claim path in §4.7).
