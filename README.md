# Perpetua

**Continuous payment streaming for Soroban.**

Deposit once, stream continuously. A sender locks a token balance with a
schedule, value accrues to the recipient second by second, and the recipient
pulls their earnings whenever they want. No cron, no keepers, no background
processes — every drip is settled at the moment it is claimed.

Perpetua is a building block for anything that wants to pay slowly in the open:
payroll rails, grant disbursement, subscription billing, vesting and revived
savings schedules. The on-chain contract is the product; everything else in this
project exists to build, verify and release it with integrity.

| | |
|---|---|
| Protocol | 27 (live on testnet and mainnet) |
| SDK | `soroban-sdk` 27.0.5 |
| Rust | 1.97.1, target `wasm32v1-none` |
| Token interface | SEP-41 (USDC on Stellar has **7 decimals**) |
| Product contract size | sub-40 KiB target; enforced by `contracts/stream/wasm-size-budget.env` |
| Tests | ~700 across four contract crates (unit, property and integration) |

> **Read [docs/KNOWN-LIMITATIONS.md](docs/KNOWN-LIMITATIONS.md) before relying on this.**
> A green suite here does not mean TTL is solved — the archival *recovery* flow
> is not yet proven against a live network. See §1 there, and the summary below.
>
> **Read [docs/threat_model.md](docs/threat_model.md) for a structured analysis of
> trust boundaries, actor capabilities, and mitigations.**

## The project is divided in two

This repository is a single repo with two independent halves:

1. **Contracts** — `contracts/`, the deployable Soroban smart contracts. Each
   is its own standalone Cargo project with its own lockfile (no shared
   workspace), so it can be built, tested and released independently:
   - `contracts/stream` — the product: the streaming primitive.
   - `contracts/factory` — policy gate: capacity cap, minimum duration, rate
     bounds, allowlist, creation pause.
   - `contracts/governance` — timelocked multi-sig tuning of factory policy.
   - `contracts/archival-probe` — a deliberate throwaway that proves the
     live-network archival/restore round trip. Never deploy it.
2. **Tooling and automation** — `script/` (release, provenance, sandbox,
   validation), `tools/provenance/` (the release-integrity gate), `tests/`
   (validator test suite), `docs/` and the build spec.

The halves depend on each other only through paths and the released wasm, never
through a shared build artifact.

## Quick start

```bash
# One line per contract crate (they are independent projects):
(cd contracts/stream    && cargo test --all-features)
(cd contracts/factory   && cargo test)
(cd contracts/governance && cargo test)
(cd contracts/archival-probe && cargo test)

# Print measured resource costs for the product contract:
(cd contracts/stream && cargo test resource_limits -- --nocapture)

# Deeper randomized sweep. CI runs this nightly; worth running before a release
# or after touching accrual.rs. Both suites have found real bugs.
FLUXORA_FUZZ_SEEDS=500 FLUXORA_FUZZ_STEPS=1000 PROPTEST_CASES=5000 \
  (cd contracts/stream && cargo test --release)

# Local end-to-end testing with standalone Soroban network:
script/local-sandbox-proof.sh
```

## Building and releasing

**Release artifacts come from `script/release.sh` only.** It builds exactly the
product contract and refuses to ship anything else:

```bash
script/release.sh        # -> contracts/stream/target/wasm32v1-none/release/fluxora_stream.wasm
```

Each contract crate builds and tests on its own:

```bash
cd contracts/stream      # or factory, governance, archival-probe
cargo build              # host target check
cargo build --target wasm32v1-none --release   # -> fluxora_<name>.wasm
```

> The archival probe is a **standalone** throwaway project with its own manifest.
> Because it does not share an output directory with the product, it cannot leak
> into a release; `script/release.sh` additionally verifies that only the
> product wasm is present in its output. Never deploy the probe.

## Release integrity

Every contract wasm ships with a provenance manifest tying its bytes to the git
revision, Rust toolchain, soroban-sdk version, target triple and release
profile, plus a SHA-256 digest per artifact (see
[docs/provenance.md](docs/provenance.md)). Verification is mandatory — a
mismatch fails the release:

```bash
script/provenance.sh build   # wasm build + generate + verify (the release gate)
script/provenance.sh verify  # re-check the current build
```

### WASM checksum verification

To verify that a locally built WASM matches the official released binary bytes:

```bash
script/verify-wasm-checksum.sh        # builds and verifies
script/verify-wasm-checksum.sh --no-build  # verifies existing build
```

Output shows green status for success, red for failure.

## Design

### Pull-based, because Stellar has no scheduler

There is no cron, no keeper network, no way for a contract to wake itself up.
Every state change must be triggered by an external transaction. Nothing here
runs in the background: the recipient calls `withdraw` and the contract computes
what they have earned at that instant.

### No on-chain stream discovery

There is deliberately **no per-user index** in storage. A `Vec<u64>` of a
treasury's streams grows without bound, costs rent forever, and blows Soroban's
transaction footprint limit once that treasury has a few hundred recipients. On
chain, a stream is only ever addressed by its `u64` id.

Discovery is an off-chain concern. `create_stream` returns the new id and emits
an event carrying sender, recipient and every schedule field, so an indexer can
answer "show me my streams" without the contract paying rent to remember.

`test::resource_limits::cost_is_independent_of_how_many_streams_exist` states
this as a test: the 153rd stream costs exactly what the 2nd did.

### Immutable guarantees

`cancellable`, `pausable` and `transferable` are fixed at creation and can never
change. Before accepting a stream a recipient can verify that the sender cannot
claw it back, freeze it, or reassign it. A stream that could *become* cancellable
later would be worthless as a guarantee.

This is enforced statically, not just by convention: `script/check-flag-immutability.py`
and `test::immutability` (which compiles `lib.rs` into the test binary) fail the
build if any function assigns to a flag post-creation (issue #104).

For the same reason there is no admin key, no upgrade path, no fee switch and no
global pause. Immutability is what lets another protocol depend on this one.

## The accrual model

Everything is expressed against a **stream clock** that stops while the stream is
paused:

```
stream_time(now) = paused_at.unwrap_or(now) - paused_total

elapsed  = clamp(stream_time, start_time, end_time) - start_time
vested   = 0                                    if stream_time < cliff_time
         = deposited * elapsed / duration       otherwise   (rounds down)
```

All arithmetic is checked and every failure is a typed error. Nothing panics on
a numeric edge case.

**Rounding is always down.** Truncating in the recipient's disfavour is correct:
the residue stays in the pool and returns to the sender at settlement, so the
contract can never owe more than it holds.

**The cliff gates, it does not delay.** At `cliff_time` the recipient becomes
entitled to everything accrued *since `start_time`* — not merely what accrues
after the cliff. This is standard vesting semantics and it surprises people.

**Conservation is exact.** For all `t`:

```
vested(t) + refundable(t) == deposited
```

with no dust term. That falls out of computing `vested` from the cumulative
formula rather than by summing per-interval deltas — truncation error is
re-derived from scratch on every call instead of accumulating. The obvious
per-interval implementation, which the existing MVPs use, loses a stroop per
withdrawal and strands it in the pool forever.

### Pause

Pausing freezes the clock and pushes the effective end forward by the paused
duration. Total value delivered stays constant; the schedule stretches. The
recipient can still withdraw while paused — pausing stops *accrual*, not access.
Freezing earned funds would make pausable streams unacceptable to any serious
recipient.

### Cancel

Rather than a second state machine, cancellation rewrites the schedule so the
stream looks like one that has fully matured: `deposited` drops to the amount
vested right now, and `end_time` is pulled back to the current point on the
stream clock. Every later `vested` call clamps to the reduced deposit, so
`withdraw` needs no special-casing at all.

Cancelling before the cliff refunds everything — pre-cliff the recipient's
entitlement is zero by definition.

## Decisions

The spec left four questions open. All four are settled, and the reasoning is in
the code where the behaviour lives.

### 1. `top_up` extends the duration; it never raises the rate

```
before:  10_000 over 100 days  ->  100/day, ends day 100
top_up(1_000)
after:   11_000 over 110 days  ->  100/day, ends day 110
```

The per-second rate the recipient agreed to never changes. The alternative —
hold `end_time` and raise the rate — retroactively re-vests elapsed time: a
top-up at the halfway point would instantly increase what is already
withdrawable. Keeping the rate fixed means a top-up can never accelerate or
dilute an existing schedule, which is what makes it safe to accept a stream from
an untrusted sender.

The extension rounds **down**, and that direction is load-bearing rather than
cosmetic. Rounding *up* makes the new duration slightly longer than exact, which
lowers the rate and therefore retroactively *reduces* the amount already vested —
letting `withdrawn` exceed `vested`, and letting a subsequent `cancel` (which
sets `deposited = vested`) drive liability negative and refund the sender money
the recipient already holds. This was a real bug, caught by the randomized
sequence suite; see [docs/KNOWN-LIMITATIONS.md](docs/KNOWN-LIMITATIONS.md) for the class of
issue and `test::top_up::a_top_up_never_reduces_what_is_already_vested` for the
regression. Rounding down guarantees `vested` never decreases; the residual is at
most one second of schedule, in the recipient's favour.

Topping up a *matured* stream is rejected (`StreamMatured`) rather than silently
making the new funds instantly withdrawable. A top-up too small to buy one second
of schedule is rejected (`TopUpTooSmall`), because absorbing it would mean raising
the rate — exactly the retroactive re-vesting this design avoids.

### 2. `MAX_BATCH_SIZE = 16`, derived from measurement

The spec's "roughly 200 ledger entry reads" predates protocol 23. Since then
live Soroban state is held in memory, and `disk_read_entries` is usually zero for
a contract touching live state. Measured against protocol 27's real limits:

| limit | 16-stream batch | ceiling |
|---|---|---|
| total footprint (entries) | 43 | 400 |
| write entries | 20 | 200 |
| instructions | ~4.6M | 400M |
| **contract event bytes** | **8,192** | **16,384** |

Entry counts would allow well over a hundred streams per call. The **event
budget** is what binds: each stream emits a `withdrawn` event plus the token
contract's own `transfer` event, roughly 512 bytes between them, so the hard
ceiling is about 32.

Sixteen is that ceiling with a 2x safety factor. The margin matters because the
per-stream event cost depends on the *token's* event payload — a token heavier
than the Stellar Asset Contract used in tests would inflate it, and a cap that
merely fits today would fail on somebody else's token.

### 3. Minimum deposit: `deposited >= duration`

At least one stroop per second. Below that the rate truncates to zero and the
recipient accrues nothing until very late — a real footgun for a treasury
streaming a small grant over a year. The floor excludes nothing realistic: a
year-long USDC stream needs only ~3.16 USDC to clear it.

### 4. `transfer_recipient` is disableable at creation

A `transferable: bool` flag alongside `cancellable` and `pausable`. A
compliance-bound sender — payroll, a KYC'd grant program — can pin the payee at
creation. Without it those senders simply could not use Perpetua.

Transfer moves the stream's entire remaining claim. Funds already withdrawn
stay with the old recipient; accrued but unwithdrawn funds and all future
accrual belong to the new recipient. The transfer itself changes no schedule or
accounting value, and only the new recipient may withdraw afterward.

## TTL, rent and archival

The hardest problem in the project, and the one existing implementations skip.

Persistent entries have a time-to-live in ledgers. When it runs out the entry is
archived and becomes unreadable until restored. A stream running twelve months
outlives its initial TTL. If a stream entry archives, the **tokens are not lost**
— they sit in the contract's pooled balance — but the accounting entry saying who
they belong to is inaccessible until someone pays to restore it.

Three mechanisms:

1. **Extend on every touch.** Every mutating call bumps that entry's TTL, so an
   actively-used stream never expires.
2. **Extend generously at creation**, targeting the stream's remaining lifetime
   plus a 30-day buffer, clamped to the network's `max_entry_ttl`.
3. **Permissionless `extend_stream_ttl` and `batch_extend_ttl`.** Anyone can pay
   to keep any stream alive. Unauthenticated on purpose: a recipient's claim must
   never depend on the sender's continued goodwill. There is nothing to grief —
   the caller only ever *pays* rent, and TTL extension cannot move funds or
   change stream state.

Views deliberately do **not** extend TTL. They are called through simulation,
where a footprint write is at best noise. Keeping a stream alive is the explicit
job of `extend_stream_ttl`.

### Retention policy by state

Every touch tops the entry back up to one target — the stream's remaining
effective life plus the 30-day buffer, floored at `MIN_STREAM_TTL_LEDGERS`
(~30 days) and clamped to the network's `max_entry_ttl`. The threshold equals
the extend-to, so an entry below its target is topped back up to it in full —
and one already funded past the target keeps its higher balance: rent is never
clawed back, so a stream entering a terminal state decays toward its floor
rather than dropping to it. State only changes what "remaining life" means:

| State | Rent target on any touch | Why |
|---|---|---|
| `Active` | remaining effective life (schedule plus any accumulated pauses) + buffer | funded to its end plus the keeper's working window |
| `Paused` | stretched end (schedule + accumulated and in-progress pauses) + buffer | a paused stream is not settled; its end slides forward in wall-clock terms |
| `Cancelled` | the floor | cancel collapses the schedule onto "now", so remaining life is zero; the floor keeps the vested tail withdrawable and the final state indexable |
| `Depleted` | the floor | fully paid out is not the same as forgotten; the terminal record stays readable |

The instance entry (the id counter) is always extended to the network maximum,
whatever the streams are doing.

### What the tests prove, and what they do not

**This is the most important caveat in the project. Do not skip it.**

The SDK's test host runs storage in recording mode, where reading an expired
persistent entry is **silently auto-restored** rather than failing. So `test::ttl`
proves the rent arithmetic, the extend-on-touch behaviour, that a year-long
stream survives on keeper sweeps alone, and that crossing the archive/restore
boundary preserves every field of the accounting with the pool still backing it.

It does **not** prove the recovery flow. On a real network the transaction
*fails first* and the caller must resubmit with a `RestoreFootprint` operation —
a step the test host skips entirely. Nothing here establishes that the failure is
diagnosable, that the footprint we would build is correct, or what a restore
costs.

**TTL is therefore half-proven.** Closing the other half against live testnet is
the acceptance criterion for stage 4, not a nice-to-have. Full detail and
integrator guidance in [docs/KNOWN-LIMITATIONS.md §1](docs/KNOWN-LIMITATIONS.md).

## Function surface

```rust
// Lifecycle
create_stream(sender, recipient, token, deposit,
              start, end, cliff,
              cancellable, pausable, transferable) -> u64   // sender auth
top_up(stream_id, amount)                                   // sender auth
withdraw(stream_id, amount: Option<i128>) -> i128           // recipient auth; None = max
batch_withdraw(recipient, stream_ids) -> i128               // recipient auth
cancel(stream_id)                                           // sender auth
pause(stream_id) / resume(stream_id)                        // sender auth
transfer_recipient(stream_id, new_recipient)                // recipient auth

// Views (read-only, no TTL side effects)
get_stream(stream_id) -> Stream
withdrawable_of(stream_id) -> i128
vested_of(stream_id) -> i128
refundable_of(stream_id) -> i128
stream_count() -> u64
stream_exists(stream_id) -> bool

// Maintenance (permissionless)
extend_stream_ttl(stream_id) -> u32
batch_extend_ttl(stream_ids) -> u32
```

Both classic keypairs and custom `__check_auth` smart accounts work everywhere.

### Events

`stream_created`, `withdrawn`, `cancelled`, `paused`, `resumed`, `topped_up`,
`recipient_transferred`, `ttl_extended`.

Declared with `#[contractevent]`, so their schemas are embedded in the deployed
contract's interface spec — the indexer and TypeScript SDK generate typed
decoders from the contract itself rather than hand-rolling topic parsers.

Every event carries `stream_id` as a topic, plus the addresses an indexer routes
on, plus enough state to reconstruct the stream without replaying from genesis.
Field order and topic placement are ABI: adding a field is compatible,
reordering one is not.

## Repository layout

```
contracts/                        the deployable contracts (standalone Cargo projects)
  stream/                         the product: streaming primitive
    src/lib.rs                    contract entry points
    src/accrual.rs                pure vesting math, no Env
    src/storage.rs                storage access and TTL policy
    src/events.rs                 event definitions
    src/types.rs                  Stream, StreamStatus, DataKey
    src/error.rs                  typed errors (discriminants are ABI)
    src/test/                     37 modules, ~600 tests, staged by build order
  factory/                        policy gate (cap, duration, rate bounds, allowlist, pause)
  governance/                     timelocked multi-sig for factory policy
  archival-probe/                 throwaway archival/restore probe — never deploy
apps/demo/                        reference Next.js + Tailwind payroll dashboard (stage 6)

sdk/react-hooks/                  reference React hooks (useStream, useAccruedBalance)

script/                           release, provenance, sandbox, validation automation
tools/provenance/                 release-integrity gate: SLSA-style wasm manifests
tests/                            validator test suite (pytest)
sdk/                              off-chain stream math mirror (JS/BigInt)
docs/                             ABI, limitations, migration and design documents
```

`accrual.rs` takes a `Stream` and a timestamp and returns a number — no `Env`, no
storage, no host calls. That keeps the interesting arithmetic in one auditable
place and makes the vesting model property-testable without a Soroban host, so a
case costs microseconds instead of a host invocation.

### The archival probe: a deliberate exception

`contracts/archival-probe/` is a **throwaway** contract, not part of the product.
Its entire purpose is to prove the live-network archival/restore round trip that
the unit suite structurally cannot (see [KNOWN-LIMITATIONS.md §1](KNOWN-LIMITATIONS.md)
and [`script/archival-canary.sh`](script/archival-canary.sh)). It writes a
persistent entry and deliberately never extends its TTL, so it archives on the
network's minimum schedule.

It is a **standalone Cargo project** — it builds and tests on its own, and
because it never shares an output directory with the product its wasm cannot be
swept into a release. `script/release.sh` builds only the `fluxora-stream`
package and rejects any unexpected wasm in the product's output. To work with
the probe explicitly:

```bash
cd contracts/archival-probe && cargo test
cd contracts/archival-probe && cargo build --target wasm32v1-none --release
```

This is the documented design decision for issue #1543.

## Non-goals for v1

No admin key, no upgradeability, no global pause. No fee mechanism. No on-chain
stream discovery. No multi-token streams. No unlock curves other than cliff plus
linear. No cross-chain anything.

## Status

Stages 1–3 complete: contract core, full lifecycle, TTL and resource limits.
The repository is now a two-part project: the contract crates are standalone
and independently buildable/testable, and the tooling (release, provenance,
validation) operates on them through committed paths only.

**Stage 4 (in progress).** Deployed to testnet as
[`CBCGTSCJ…THXW`](https://stellar.expert/explorer/testnet/contract/CBCGTSCJXBMPPPE4BPDIPYZXPE2J5TQEKD2KCS7VQF533NKKEYGUTHXW);
`script/testnet-exercise.sh` calls every entrypoint against the live deployment.

Its acceptance criterion — the live archival restore round trip — is **not yet
met**. A canary entry was planted on 2026-08-12; see
[docs/KNOWN-LIMITATIONS.md §1](docs/KNOWN-LIMITATIONS.md) and
`script/archival-canary.sh`.

Then the indexer, keeper and TypeScript SDK (stage 5), reference UI last (stage 6).
The reference React hooks for those UIs live in
[`sdk/react-hooks/`](sdk/react-hooks/README.md) (issues #106).

Migrating from the pre-rewrite contract? See [docs/MIGRATION.md](docs/MIGRATION.md).

## Documents

| | |
|---|---|
| [docs/ABI.md](docs/ABI.md) | **Interface of record.** Frozen 2026-08-12. Read this before integrating. |
| [docs/griefing-analysis-extend-ttl.md](docs/griefing-analysis-extend-ttl.md) | Issue #97: formal audit of the permissionless TTL keeper surface. |
| [docs/KNOWN-LIMITATIONS.md](docs/KNOWN-LIMITATIONS.md) | What a green suite does not prove. |
| [docs/MIGRATION.md](docs/MIGRATION.md) | Deletion audit vs the pre-rewrite contract, and downstream impact. |
| [docs/URI-SCHEME.md](docs/URI-SCHEME.md) | `stellar:stream` URI / QR-code standard for sharing a stream. |
| [docs/soroban-rpc-read-skew.md](docs/soroban-rpc-read-skew.md) | Pin multi-call reads to one ledger, and the read-after-write barrier. |
| [docs/provenance.md](docs/provenance.md) | Wasm provenance schema, design decisions, and the release gate. |
| [docs/dust-theft-proof.md](docs/dust-theft-proof.md) | Why micro-top-ups cannot erode the pool or steal residue (§102). |
| [docs/SECURITY.md](docs/SECURITY.md) | Audit scope, reporting process and bug bounty rules (§101). |
| [fluxora-build-spec.md](fluxora-build-spec.md) | The build spec, with amendments where measurement contradicted it. |

> **Note for deployment:** the `stellar` CLI must be at least version 27 to match
> the protocol. A protocol-23 CLI will scaffold and may misreport against a
> protocol-27 network.

## Handsoff notes

<!-- handsoff-issue-7 -->
- #7: [Pause & Resume] Sliding end-time drift during cumulative multi-pause cycles

<!-- handsoff-issue-15 -->
- #15: [Recipient Transfer] Immutable transferable flag check in transfer_recipient
