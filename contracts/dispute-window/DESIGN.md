# Dispute Window — Design

`FluxoraDisputeWindow` is a reusable, time-bounded challenge primitive. An
attested action enters an **open** dispute window; authorized challengers may
file a dispute while the window is open; if the window elapses undisputed the
action becomes **final** (resolvable); if it is disputed it is **frozen**
pending arbiter resolution. The contract is a standalone building block — usable
by the oracle router (to challenge a reported price), by the stream contract
(to challenge a recipient transfer), or by governance (as an escape hatch on an
executed proposal) — and carries no dependency on any of them.

This document is a **design only**. No contract code is written here; it is the
specification the implementation in `contracts/dispute-window/` will follow, and
it resolves one open issue:

- **#419** — add fuzzing coverage for edge-case timestamps in the dispute window
  near `u64::MAX` boundary limits.

It follows the conventions already established by `contracts/stream`,
`contracts/factory`, and `contracts/governance`: a standalone Cargo project,
`#![no_std]`, `soroban-sdk` 27.x, frozen-discriminant `#[contracterror]`,
checked arithmetic everywhere, size-optimized release profile, and a rich
`src/test/` suite (unit + fuzz/property tests).

---

## Architecture

The contract owns a monotonic `item_id` counter and a per-item persistent
record. Each item is created by an attester (the `proposer`) who stakes a claim
— represented abstractly by a `subject` hash (e.g. a price-report hash, a
transfer commitment, or a proposal id) — and a `window_duration`. The window
opens at the ledger timestamp of creation and closes at `window_open +
window_duration`. Three roles act on items:

- **Proposer** — opens the window and, if undisputed, may resolve it final after
  the close.
- **Challenger** — files a dispute while the window is open, freezing
  resolution.
- **Arbiter** — the contract admin; the only party that may force-resolve a
  disputed item (uphold or slash), and the only party that may assign the
  challenger/arbiter allowlists when allowlisting is enabled.

The contract performs **no token movement**. It records outcomes; any slashing
or payout is the caller's responsibility keyed on the resolved status. This
keeps the trust boundary narrow: a dispute-window bug can stall or misclear a
challenge, but cannot move funds.

---

## Data model

```text
DisputeStatus enum { Open, Disputed, Resolved, Expired }

DisputeItem {
    proposer:    Address,
    subject:     BytesN<32>,     // hash of the attested action being challenged
    window_open: u64,            // ledger timestamp at creation
    duration:    u64,            // window length in seconds
    status:      DisputeStatus,
    challenger:  Option<Address>,
    evidence:    Option<BytesN<32>>,
}

Role enum { Arbiter, Challenger }   // Arbiter == admin; Challenger is allowlisted
```

### Storage keys

| Key | Scope | Holds |
|---|---|---|
| `Admin` | instance | arbiter / admin address |
| `ChallengerAllowlist` | instance | `Map<Address, bool>` (only when allowlisting is on) |
| `NextItemId` | instance | monotonic `u64` counter |
| `AllowlistingEnabled` | instance | bool |
| `Item(u64)` | persistent | the `DisputeItem` record |

`window_close` is **not stored**; it is derived as `window_open + duration`
under checked arithmetic (see [§ Timestamp boundary handling](#timestamp-boundary-handling-419)).
Storing a derived value would create a second source of truth and a place for
the boundary bug to hide.

---

## Timestamp boundary handling (#419)

The whole point of #419 is that timestamp arithmetic near `u64::MAX` is a trap:
`window_open + duration` wraps silently under wrapping addition, turning a
window that should *never close* into one that already closed in the past. The
design makes the arithmetic total and the failure explicit.

### Open-time overflow guard

`open_window` computes `window_close = window_open.checked_add(duration)`. If
that overflows `u64`, the call returns `WindowOverflow` and writes nothing — the
item is never created, so no `item_id` is consumed (matching the stream
contract's transactional-creation rule: a failed open consumes no id). This is
**defensive, checked arithmetic**: reject rather than silently saturate, the
same stance the stream contract takes for `next_stream_id` overflow
(`StreamIdOverflow` / `StreamIdExhausted`).

Consequences:
- A successfully opened item is guaranteed `window_close ≤ u64::MAX`, so every
  later comparison (`is_open`, `is_final`, `dispute`, `resolve`) is well-defined
  for **all** `now`, including `now == u64::MAX`.
- A proposer opening a window when the ledger timestamp is already within
  `duration` of `u64::MAX` cannot open it; it must wait for a lower timestamp or
  request a shorter window. In practice `u64` seconds span ~5.8×10¹¹ years, so a
  live ledger never approaches the bound — but the fuzz suite proves the
  arithmetic is sound regardless, which is exactly what #419 asks.

### Derived comparisons (all checked, all in range)

```text
window_close = window_open + duration                  // proven ≤ u64::MAX at open

is_open(now)     = (now < window_close) && status == Open
is_final(now)    = (now >= window_close) && status == Open
can_dispute(now) = is_open(now)
can_resolve(now) = (now >= window_close) && status == Open
                 || (status == Disputed && caller == Arbiter)   // forced
```

No subtraction is ever performed on timestamps. The only arithmetic is the one
`checked_add` at open time; everything else is comparison. This is deliberate:
subtraction (`now - window_open`) is where most timestamp-boundary bugs
originate, and the contract never needs it.

### Why reject rather than saturate

A saturated `window_close = u64::MAX` would make the window *effectively never
close*, which sounds benign but is unsafe: it lets a proposer open an
un-disputable-yet-also-un-resolvable item (since `now >= window_close` is
unreachable), parking a record forever at a non-terminal status — exactly the
unbounded-rent growth the governance retention policy exists to prevent.
Rejecting at open time keeps the lifecycle total: every created item has a
reachable close.

---

## Lifecycle

```
                         open_window
        ───────────────────────────────────────►  Open
        │                                          │
        │ dispute (now < window_close)             │ resolve (now >= window_close)
        ▼                                          ▼
     Disputed ──── arbiter force-resolve ─────►  Resolved (final)
        │
        └── (no auto-expiry; Open past close that is never resolved
            stays Open until resolve() is called → Resolved)
```

- **`open_window(proposer, subject, duration) -> u64`** — proposer auth; checked
  add guards the close; emits `dispute_opened(id, proposer, subject, close)`.
- **`dispute(item_id, challenger, evidence)`** — challenger auth (and must be
  allowlisted if allowlisting is on); requires `is_open(now)` and `status ==
  Open`; sets `Disputed`, records challenger/evidence; emits
  `dispute_challenged(id, challenger)`. Filing after close returns
  `WindowExpired`.
- **`resolve(item_id)`** — proposer auth; requires `now >= window_close` and
  `status == Open`; sets `Resolved`; emits `dispute_resolved(id, proposer)`. A
  `Disputed` item can only leave that state via the arbiter.
- **`arbiter_resolve(item_id, upheld)`** — arbiter auth; only on `Disputed`;
  sets `Resolved` (the `upheld` flag is recorded in the event for the caller to
  key payout on); emits `dispute_arbitered(id, upheld)`.
- Views: `window_close_of(item_id) -> u64`, `is_open(item_id, now) -> bool`,
  `is_final(item_id, now) -> bool`, `get_item(item_id) -> DisputeItem`.

---

## Modular error model

Discriminants are reserved by subsystem and frozen (append-only), mirroring the
oracle-router and stream conventions.

```text
 1–19  Authorization          (Unauthorized, NotArbiter, NotAllowlisted, ...)
20–39  Item / window lifecycle (ItemNotFound, WindowExpired, AlreadyDisputed,
                               NotDisputed, AlreadyResolved, InvalidDuration)
40–49  Arithmetic             (WindowOverflow, ItemIdOverflow)
```

| # | Variant | Meaning |
|---|---|---|
| 1 | `Unauthorized` | caller failed `require_auth` for the role. |
| 2 | `NotArbiter` | operation requires the arbiter/admin seat. |
| 3 | `NotAllowlisted` | challenger not in the allowlist while enforcement is on. |
| 20 | `ItemNotFound` | no item with the given id. |
| 21 | `WindowExpired` | `dispute` called at or after `window_close`. |
| 22 | `AlreadyDisputed` | `dispute` on an item already in `Disputed`. |
| 23 | `NotDisputed` | `arbiter_resolve` on a non-`Disputed` item. |
| 24 | `AlreadyResolved` | action on an item already `Resolved`. |
| 25 | `InvalidDuration` | `duration == 0`. |
| 40 | `WindowOverflow` | `window_open + duration` overflows `u64` (#419). |
| 41 | `ItemIdOverflow` | the `next_item_id` counter would overflow `u64` on increment. |

As with the oracle router, each error path may emit a structured diagnostic event
(topic `dispute_error`, data `{op, item_id, now, window_close}`) so clients
observe *why* via `diagnosticEvents`, not just *that* via the discriminant.

---

## Ledger footprint and TTL

- Instance entries (`Admin`, allowlist, counter, flag) bump on every load to
  120,960 ledgers at the 17,280 threshold (factory/governance parity).
- Per-item `Item(id)` persistent entries follow the governance retention policy:
  **active** items (`Open`, `Disputed`) are bumped past their own remaining
  window plus headroom on every touch so they cannot archive while still
  actionable; **terminal** items (`Resolved`) get a one-shot archival floor (30
  days, for indexer/auditor reconciliation) and are never topped up again, so
  rent decays rather than growing unbounded. (There is no `Expired` status; an
  undisputed item past its close simply awaits `resolve`.)
- `window_close_of` and the other views are read-only and do **not** extend TTL
  beyond the standard read bump, matching the stream contract's rule that views
  carry no TTL side effects.

### Resource limits

- No on-chain cap on item count — the contract holds one persistent entry per
  item and the cost of any single operation is independent of how many items
  exist (the same `cost_is_independent_of_count` property the stream contract
  asserts). There is no per-user index, by the same reasoning: a `Vec<u64>` would
  grow without bound and blow the footprint limit.

---

## Checked arithmetic and formal invariants

All arithmetic is checked. The design asserts:

1. **Open is total**: for any `(window_open, duration)`, `open_window` returns
   an id or `WindowOverflow` — never traps, never wraps, consumes no id on
   failure.
2. **Close is in range**: for any opened item, `window_close ==
   window_open + duration` exactly and `window_close ≤ u64::MAX`.
3. **No subtraction on timestamps**: the contract never computes `now -
   window_open`; all ordering is by comparison against the in-range
   `window_close`.
4. **Open/final partition**: for all `now` (including `u64::MAX`) and an
   `Open` item, exactly one of `is_open(now)` and `is_final(now)` holds.
5. **Finality is monotonic in `now`**: for an undisputed item, `is_final(t)`
   implies `is_final(t')` for all `t' ≥ t`.
6. **Dispute window is strict**: `dispute` succeeds iff `now < window_close` and
   `status == Open`; it fails with `WindowExpired` iff `now >= window_close`.
7. **Counter soundness**: `next_item_id` increments only on a successful open;
   it never wraps (`ItemIdOverflow` on overflow).

---

## Test plan

Unit tests (`src/test/`); the headline addition for #419 is a fuzz harness.

### Fuzz harness — edge-case timestamps near `u64::MAX` (#419)

A proptest module (`src/test/boundary_fuzz.rs`, mirroring the stream contract's
`lifecycle_proptest` / `props` modules) generating `(window_open, duration, now)`
with **edge-weighted** distributions concentrated at the boundaries, then
asserting every invariant above.

`proptest` strategies:

- `window_open ∈ {0, 1, u64::MAX, u64::MAX - 1, u64::MAX - 1000, …, random}`
- `duration ∈ {0, 1, u64::MAX, u64::MAX - window_open, u64::MAX - window_open + 1
  (to force overflow), …, random}`
- `now ∈ {0, 1, u64::MAX, window_open, window_close, window_close - 1,
  window_close + 1, …, random}`

The strategy explicitly includes the **overflow-forcing** combination
(`window_open + duration > u64::MAX`) so the open-time guard is exercised, not
just the happy path.

Assertions per generated case:

1. `open_window(window_open, duration)` either returns an id **or** returns
   `WindowOverflow` iff `window_open + duration > u64::MAX`; it never panics.
2. For a successfully opened item: `window_close_of(id) == window_open +
   duration` (no wrap), and `window_close_of(id) >= window_open`.
3. `is_open(id, now) == (now < window_close && status == Open)` for every
   generated `now`, including `now == u64::MAX`.
4. `is_final(id, now) == (now >= window_close && status == Open)`; and once
   true for some `t`, true for all `t' ≥ t` (monotonicity — checked by sampling
   two `now` values `t ≤ t'`).
5. `dispute(id, now)` succeeds iff `now < window_close && status == Open`;
   returns `WindowExpired` iff `now >= window_close`; never panics for any
   `now` up to `u64::MAX`.
6. Re-running the conversion of all timestamp ops under `u64::MAX` inputs
   produces no `panic`/abort — the suite treats any abort as a failure.

The harness runs under the same `FLUXORA_FUZZ_SEEDS` / `PROPTEST_CASES`
knobs the stream contract uses, so CI's nightly fuzz sweep covers it.

### Unit tests

- Open → undisputed → resolve-after-close succeeds; resolve-before-close fails
  with `WindowExpired`.
- Open → dispute-before-close succeeds and freezes resolution; resolve on a
  `Disputed` item by the proposer fails (`NotDisputed` via `resolve`);
  `arbiter_resolve` succeeds and records `upheld`.
- `WindowOverflow` when `window_open + duration > u64::MAX` (direct boundary
  cases: `(u64::MAX, 1)`, `(u64::MAX, u64::MAX)`, `(u64::MAX - 5, 10)`).
- `InvalidDuration` on `duration == 0`.
- `ItemIdOverflow` after forcing the counter to `u64::MAX`.
- Allowlist enforcement: a non-allowlisted challenger is rejected while
  allowlisting is on and accepted (or un-gated) while off.
- CEI: no diagnostic/error event is emitted after a state mutation; the open,
  dispute, and resolve events carry the documented fields.
- Footprint: `window_close_of` / `is_open` / `is_final` issue no writes; cost is
  independent of item count.

---

## Public surface

| Entry point | Auth | Behavior |
|---|---|---|
| `init(admin)` | one-time | set arbiter/admin. |
| `set_allowlist(addr, bool)` / `set_allowlisting(bool)` | `Arbiter` | challenger allowlist management. |
| `propose_admin(new)` / `accept_admin()` | `Arbiter` / nominee | two-step arbiter transfer (matches factory). |
| `open_window(proposer, subject, duration)` | `proposer` | open a window → `item_id` or `WindowOverflow`. |
| `dispute(item_id, evidence)` | `challenger` (allowlisted if on) | freeze an open item. |
| `resolve(item_id)` | `proposer` | finalize an undisputed item past close. |
| `arbiter_resolve(item_id, upheld)` | `Arbiter` | force-resolve a disputed item. |
| `window_close_of(item_id)` | view | `window_open + duration` (checked, in range). |
| `is_open(item_id, now)` / `is_final(item_id, now)` | view | boundary-correct status. |
| `get_item(item_id)` | view | full `DisputeItem` record. |

> Final discriminants are assigned at implementation time, kept within the
> documented ranges, and frozen thereafter, mirroring `docs/error.md` for the
> stream contract.
