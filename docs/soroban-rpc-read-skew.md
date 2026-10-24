# Pin your Soroban reads to a single ledger

*A note from the Perpetua team. Written against Stellar testnet, protocol 27,
`stellar-cli` 27.1.0, on 2026-08-12.*

## The pattern

**If you derive one figure from two Soroban RPC view calls, pin both to the same
ledger — or do not derive it from two calls at all.**

A public RPC URL is an endpoint, not a node. Requests behind it may be served by
different backends, and those backends need not be at the same ledger height at
the same instant. Nothing in the JSON-RPC surface tells you which ledger you got
unless you look: every response carries `latestLedger`, and almost no client
code reads it.

Two consequences worth designing against, neither of which produces an error —
you get a plausible wrong answer instead:

1. **Read-after-write.** Read immediately after a confirmed write and you may
   observe pre-write state.
2. **Cross-call derivation.** Combine two reads into one number and the halves
   may describe different moments, making the result arithmetically impossible.

The rest of this note is why we went looking, and what to do instead. The
pattern above stands on its own — it is the correct way to use any
load-balanced read endpoint, and it costs almost nothing to adopt.

## What prompted us to look

Perpetua is a payment-streaming contract. Value accrues continuously, so we have
a conservation invariant:

```
vested(t) + refundable(t) == deposited      exactly, at every instant
```

Our on-chain test suite proves this per-ledger over thousands of random
schedules. So when a script that calls the two views back to back against live
testnet reported

```
vested=22000000  refundable=583000000  deposited=600000000
  ->  22000000 + 583000000 = 605000000   ✗ off by 5000000
```

the natural conclusion was a contract bug — 5,000,000 stroops conjured from
nowhere.

It was not. On that stream the rate was 1,000,000 stroops/second, so the
discrepancy was **exactly five seconds of accrual**: the two view calls
described moments five seconds apart. A `top_up` in the same script separately
appeared to be a no-op — the write was confirmed, but the `get_stream`
immediately afterwards returned the pre-top-up deposit.

Both symptoms are explained by the two calls not sharing a ledger, and both went
away when we added the barrier below. We spent real time hunting a contract bug
that did not exist, which is the cost this note exists to save you.

## The observation, and its limits

We looked directly at `getLatestLedger`, which needs no contract:

```bash
prev=0
for i in $(seq 1 25); do
  s=$(curl -s -X POST https://soroban-testnet.stellar.org \
        -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","id":1,"method":"getLatestLedger"}' \
      | python3 -c 'import sys,json;print(json.load(sys.stdin)["result"]["sequence"])')
  if [ "$prev" -ne 0 ] && [ "$s" -lt "$prev" ]; then
    echo "went BACKWARDS: $prev -> $s"
  fi
  prev=$s
done
```

One run, around ledger 4,097,07x, showed the endpoint reporting a *lower* height
than it had a moment earlier:

```
went BACKWARDS: 4097075 -> 4097071
went BACKWARDS: 4097075 -> 4097071
went BACKWARDS: 4097076 -> 4097071
went BACKWARDS: 4097076 -> 4097071
went BACKWARDS: 4097076 -> 4097071
went BACKWARDS: 4097076 -> 4097071
backwards transitions in 25 reads: 6
```

Six transitions in twenty-five reads, spread about five ledgers — consistent
with the five seconds of accrual we had just seen unaccounted for.

**We could not reproduce it again.** Everything afterwards was clean:

| run | method | samples | backwards steps |
|---|---|---|---|
| 1 | `curl`, fresh connection, no delay | 25 | **6** |
| 2 | Python `urllib`, 150 ms interval | 60 | 0 |
| 3 | `curl`, fresh connection, no delay | 60 | 0 |
| 4 | Python `urllib`, 200 ms interval, 8 minutes | 375 | 0 |

That is **one anomalous run against 495 subsequent clean samples**, including run
3, which repeated run 1's method exactly, thirty minutes later.

So we are not claiming this is a general or ongoing property of the endpoint,
and you should not repeat it as one. A single unreproduced observation is
consistent with several explanations — a backend briefly catching up after a
restart, a transient routing artefact, or something local to our client at that
moment — and we cannot distinguish between them from one sample. If you are at
SDF and have data that rules it out, we would genuinely like to know; we will
amend this note.

**What does not depend on that question is the engineering pattern.** Pinning
multi-call derivations to a single ledger is correct whether or not the endpoint
ever skews, because you cannot verify from the client side that it did not, and
because `latestLedger` is right there in every response. It costs one field
check. Treating a single URL as a single node is an assumption you are making
either way — this just makes it explicit and cheap to stop making.

## Frontend SDK guidance: poll until the target ledger

If your app submits a Soroban transaction and immediately reads contract state,
add a barrier. In practice, this means polling `getLatestLedger()` until the
node is at or beyond the target ledger for your write, then reading the stream
state. Treat that read as the boundary between "write not yet visible" and
"safe to display".

The pattern is simple:

1. Submit the transaction.
2. Wait for the transaction to be included in a ledger.
3. Poll `getLatestLedger()` until it reaches or passes the ledger sequence you
   just observed.
4. Read the contract state only after that boundary.
5. If the ledger still lags, retry instead of deriving a number from stale data.

### TypeScript example

```ts
import type { rpc as SorobanRpc } from '@stellar/stellar-sdk';

async function waitForLedger(
  rpc: SorobanRpc.Server,
  targetLedger: number,
  { maxAttempts = 40, pollMs = 1000 } = {},
): Promise<number> {
  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    const latest = await rpc.getLatestLedger();
    if (latest.sequence >= targetLedger) {
      return latest.sequence;
    }
    await new Promise((resolve) => setTimeout(resolve, pollMs));
  }

  throw new Error(
    `RPC did not reach ledger ${targetLedger} after ${maxAttempts} attempts`,
  );
}

async function readStreamAfterWrite(
  rpc: SorobanRpc.Server,
  txHash: string,
  contract: { read: (args?: any[]) => Promise<any> },
) {
  const tx = await rpc.getTransaction(txHash);
  const includedLedger = tx?.resultMeta?.v3?.sorobanTransactionMeta?.sorobanMeta?.ledger
    ?? tx?.resultMeta?.v3?.ledger;

  if (typeof includedLedger !== 'number') {
    throw new Error(`Transaction ${txHash} is not yet included in a ledger`);
  }

  await waitForLedger(rpc, includedLedger, { maxAttempts: 40, pollMs: 1000 });
  return contract.read();
}
```

The precise `tx.resultMeta` field names can vary slightly by SDK version, but the
idea is stable: read the included ledger from the transaction result, then wait
for the node to advance to that sequence before querying state.

### React / frontend pattern

```ts
async function refreshStreamState() {
  const tx = await submitStreamTransaction();
  const latestLedger = await rpc.getLatestLedger();

  // Poll until the node is at least as far as the write's ledger.
  while ((await rpc.getLatestLedger()).sequence < tx.ledgerSequence) {
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }

  return rpc.getContractValue({
    contractId: STREAM_CONTRACT_ID,
    key: 'stream',
    // or whichever view your app uses
  });
}
```

If your UI reads the stream immediately after a top-up, withdrawal or claim,
this barrier is the difference between a stale read and a correct one. The rate
of accrual is not the only thing that can drift between ledgers — the node you
read from can also still be behind the one that included your transaction.

## What to do

These are ordered by preference. The first one makes the problem structurally
impossible and is worth designing your contract's views around.

### 1. Return everything you need from one call

If a client needs vested, withdrawable and refundable together, give it a view
that returns the whole struct and let it compute the three locally. One call is
one ledger, by construction; there is nothing to pin.

Perpetua's `get_stream` does this, which is why the fix for our own script was
partly "stop calling `vested_of` and `refundable_of` separately".

### 2. Put a barrier after every write

Do not read straight after a write. Wait until the backends you can see have
caught up to the ledger containing your transaction. A cheap version: sample
`getLatestLedger` until several consecutive samples are all at or beyond your
target height.

```bash
settle() {
  local target hi=0 ok=0 tries=0 s
  target=$(latest_ledger)
  while [ "$ok" -lt 4 ] && [ "$tries" -lt 40 ]; do
    s=$(latest_ledger)
    [ "$s" -gt "$hi" ] && hi=$s
    if [ "$s" -ge "$target" ] && [ "$s" -gt 0 ]; then ok=$((ok+1)); else ok=0; fi
    tries=$((tries+1)); sleep 1
  done
}
```

Requiring several *consecutive* samples at or above the target is what makes
this work: a single passing sample only tells you that *one* backend has caught
up, and the next request may well go to a different one.

### 3. When you must use several calls, check `latestLedger`

Every Soroban RPC response carries it. Read it from each response in the set,
and if they disagree, discard and retry rather than combining them. If you are
only sanity-checking rather than displaying, a bounded tolerance is enough — our
exercise script allows about 30 seconds of accrual on the conservation check,
with a comment saying why.

### TypeScript reference implementation

The JavaScript SDK exposes the sequence as `latestLedger` on each RPC response.
It is response metadata, not a portable request header: Soroban RPC does not
provide a standard `ledger` parameter that pins `simulateTransaction` to an
older ledger. Treat `latestLedger` as the ledger-sequence header for the read,
and retry the complete group when the values differ.

When the required values are available as ledger keys, prefer one
`getLedgerEntries` request instead of several simulations. The response carries
one `latestLedger` for the whole key batch:

```ts
const response = await server.getLedgerEntries(countKey, ...streamKeys);
const snapshotLedger = response.latestLedger;
const entries = response.entries;
```

This is the closest RPC equivalent to pinning a read. It requires constructing
the exact Soroban `LedgerKey` values, so contract views remain the simpler
choice when the needed keys are not already known to the indexer.

```ts
import { SorobanRpc, xdr } from "@stellar/stellar-sdk";

type LedgerRead<T> = { value: T; latestLedger: number };
const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

export async function readAtOneLedger<T>(
  reads: readonly (() => Promise<LedgerRead<T>>)[],
  attempts = 5,
): Promise<{ values: T[]; ledger: number }> {
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    const batch = await Promise.all(reads.map((read) => read()));
    const ledger = batch[0]?.latestLedger;
    if (ledger !== undefined && batch.every((item) => item.latestLedger === ledger)) {
      return { values: batch.map((item) => item.value), ledger };
    }
    await pause(250);
  }
  throw new Error("RPC reads did not converge on one ledger");
}

export async function simulateStamped<T>(
  server: SorobanRpc.Server,
  transaction: Parameters<SorobanRpc.Server["simulateTransaction"]>[0],
  decode: (value: xdr.ScVal) => T,
): Promise<LedgerRead<T>> {
  const response = await server.simulateTransaction(transaction);
  if (!response.result) throw new Error(response.error ?? "Simulation failed");
  return { value: decode(response.result), latestLedger: response.latestLedger };
}
```

Build each transaction with the same `Contract` and source account, then pass
the resulting simulations to `readAtOneLedger`. An indexer that needs the
count and all stream records should retry the whole snapshot, not read the
count once and assume later `get_stream` calls see the same state:

```ts
async function readStreamSnapshot(
  readCount: () => Promise<LedgerRead<bigint>>,
  readStream: (id: bigint) => Promise<LedgerRead<Stream>>,
): Promise<{ count: bigint; streams: Stream[]; ledger: number }> {
  for (let attempt = 0; attempt < 5; attempt += 1) {
    const count = await readCount();
    const streams = await Promise.all(
      Array.from({ length: Number(count.value) }, (_, index) => readStream(BigInt(index))),
    );
    if (streams.every((stream) => stream.latestLedger === count.latestLedger)) {
      return {
        count: count.value,
        streams: streams.map((stream) => stream.value),
        ledger: count.latestLedger,
      };
    }
    await pause(250);
  }
  throw new Error("stream snapshot crossed a ledger boundary");
}
```

The count is read before allocating the id range, and every returned stream is
accepted only when its sequence matches that count. This prevents a concurrent
`create_stream` from producing a snapshot whose range was selected from one
ledger but read from another. If a stream disappears or an id is out of range,
discard the snapshot and retry; do not turn `StreamNotFound` into an empty
record.

For a read-after-write barrier, first wait for the transaction itself to reach
`SUCCESS`, retain its committed ledger, then wait until the RPC endpoint has
reported that ledger repeatedly before starting the stamped read group:

```ts
export async function waitForCommit(
  server: SorobanRpc.Server,
  hash: string,
): Promise<number> {
  for (;;) {
    const transaction = await server.getTransaction(hash);
    if (transaction.status === "SUCCESS") return transaction.ledger;
    if (transaction.status === "FAILED") throw new Error("transaction failed");
    await pause(1_000); // PENDING or NOT_FOUND: RPC indexing is still catching up.
  }
}

export async function waitForLedger(
  server: SorobanRpc.Server,
  target: number,
  consecutive = 4,
): Promise<void> {
  let seen = 0;
  while (seen < consecutive) {
    const latest = (await server.getLatestLedger()).sequence;
    seen = latest >= target ? seen + 1 : 0;
    await pause(1_000);
  }
}

const committedLedger = await waitForCommit(server, sendResult.hash);
await waitForLedger(server, committedLedger);
const snapshot = await readStreamSnapshot(readCount, readStream);
```

The barrier reduces stale-replica reads after a write; the stamped group is
still required for consistency between multiple views. Do not use the latest
ledger sampled before `sendTransaction` as the barrier target: only the ledger
returned by successful transaction polling proves that the write was included.

### 4. Never assert exact equality across two calls in a test

A test that asserts `a() + b() == c()` across three separate simulations against
a public endpoint will fail intermittently, and every failure will look like a
contract bug. Either derive all three from one call, or assert within a
tolerance you can justify.

### 5. Run your own RPC if you need strict read-after-write

A single node you control cannot skew against itself. For an indexer or a keeper
this is worth the operational cost; the barrier pattern above is a mitigation,
not a guarantee.

## Operator runbook: archived entries, footprint restoration, and retries

This section is the operational companion to the pattern above. It covers the
failure modes that show up once a streaming or vesting contract has been live
long enough for its persistent entries to expire, and it is the procedure
operators should follow when `script/event-byte-budget.py` (the Soroban 16 KiB
Event Byte Footprint Enforcer) reports a footprint problem or a read-skew
abort.

### Detecting an archived persistent entry

Soroban persistent entries have a TTL. When that TTL lapses the entry is
*archived*: it is not deleted, but it is no longer readable by ordinary
contract calls until it is restored. From the client side the symptom is
subtle, because an archived entry does not produce a distinct RPC error — a
view call that reads it simply behaves as though the entry were absent.

The reliable detector is to compare the stream id against the stream count
returned by the same snapshot:

```
stream_exists(id) == false   AND   id < stream_count()
```

Both halves matter:

- `stream_exists(id) == false` alone is ambiguous. It is also what you get for
  an id that was never allocated, or for a read that landed on a replica that
  is behind the ledger that created the stream.
- `id < stream_count()` proves the id *was* allocated. The count is monotonic
  and is itself a persistent entry, so if the count says the stream exists but
  the record does not, the record has been archived (or the read is skewed —
  see the next subsection).

`script/event-byte-budget.py` applies exactly this test. When it sees
`stream_exists() == false` for an id below `stream_count()`, it does **not**
treat the stream as absent and does **not** silently shrink the footprint it
is about to report. It raises a distinct, non-retryable `ArchivedEntryError`
naming the stream id, so the operator sees the archival rather than a
plausible-but-wrong byte budget.

### Distinguishing archival from ledger skew

Archival and read skew produce the same `stream_exists() == false` symptom, and
the recovery is different, so the enforcer distinguishes them before acting.
Every RPC response carries `latestLedger`; `script/event-byte-budget.py` reads
it from each response in a snapshot and compares the values:

- **All responses agree on `latestLedger`, and the id is below the count.**
  This is archival. Proceed to footprint restoration below.
- **Responses disagree on `latestLedger`.** This is read skew, not archival.
  The enforcer discards the snapshot and retries the whole group (see
  "Idempotent retry semantics"). It never restores a footprint on the basis of
  a skewed read, because a skewed read can make a live entry look archived.
- **Responses agree, but the id is at or above the count.** The id was never
  allocated. This is a caller error, not a recovery scenario.

The enforcer also refuses to act on a snapshot whose `latestLedger` is behind
the ledger it observed at the start of the run, which is the same
"went backwards" condition described in *The observation, and its limits*
above. A backwards `latestLedger` is treated as skew and retried, never as
evidence of archival.

### Restoring an archived footprint

Restoration is a two-step operation and must be done in this order:

1. **Build the restore transaction.** `RestoreFootprint` takes the ledger keys
   of the archived entries, not the entries themselves. Use
   `tools/sdk/restore_footprint.ts` to construct it:

   ```bash
   npx ts-node tools/sdk/restore_footprint.ts \
     --contract "$STREAM_CONTRACT_ID" \
     --stream-id "$STREAM_ID" \
     --network testnet
   ```

   The tool reads `stream_count()` and the stream record, derives the exact
   `LedgerKey` values for the archived entries, and prints the unsigned
   `RestoreFootprint` operation. It does not submit anything.

2. **Submit and confirm.** Sign and submit the transaction, then wait for it
   to reach `SUCCESS` using the `waitForCommit` barrier above. Do not proceed
   on `PENDING` or `NOT_FOUND` — those mean the RPC indexer has not caught up,
   not that the restore failed.

   ```bash
   stellar tx submit --network testnet --sign-with "$SIGNER" restore.xdr
   ```

3. **Re-run the enforcer.** Only after the restore is confirmed:

   ```bash
   python3 script/event-byte-budget.py --contract "$STREAM_CONTRACT_ID"
   ```

   A successful restore makes the previously archived entry readable again,
   and the enforcer's footprint report will include it. If the enforcer still
   reports `ArchivedEntryError` for the same id, the restore did not cover the
   entry — re-derive the ledger keys with `tools/sdk/restore_footprint.ts`
   rather than guessing at them.

Restoration is safe to repeat. `RestoreFootprint` on an entry that is already
live is a no-op, so a restore that was submitted but whose confirmation you
never observed can simply be re-submitted.

### Idempotent retry semantics

A failed or timed-out invocation of the enforcer must be safe to re-run, and
the recovery paths are designed so that it is:

- **The enforcer never mutates contract state.** It reads, computes a byte
  footprint, and reports. Re-running it after any failure is always safe.
- **Restoration is idempotent.** As above, restoring an already-live entry is a
  no-op, so a restore whose confirmation was lost can be re-submitted without
  checking first.
- **Retries are bounded and jittered.** The enforcer retries a skewed snapshot
  a fixed number of times with backoff, then exits non-zero with a
  `ReadSkewError` rather than looping forever. A non-zero exit means "retry the
  whole invocation", not "the contract is broken".
- **A timed-out invocation leaves no partial state.** Because the enforcer
  writes nothing, a timeout mid-run is indistinguishable from a run that never
  started. Re-run it.

The one thing that is *not* idempotent is submitting a `RestoreFootprint`
transaction without confirming it. Always confirm before re-deriving keys, so
that you are reacting to the chain's state and not to your own unconfirmed
submission.

### Usage examples

Check the footprint of a single stream:

```bash
python3 script/event-byte-budget.py --contract "$STREAM_CONTRACT_ID" --stream-id 42
```

Check every stream in the contract, with the read-skew barrier enabled:

```bash
python3 script/event-byte-budget.py --contract "$STREAM_CONTRACT_ID" --all --pin-ledger
```

Emit machine-readable output for a monitoring pipeline:

```bash
python3 script/event-byte-budget.py --contract "$STREAM_CONTRACT_ID" --all --json
```

### Troubleshooting

| symptom | likely cause | action |
|---|---|---|
| `ArchivedEntryError` for a stream id below `stream_count()` | persistent entry TTL lapsed | restore with `tools/sdk/restore_footprint.ts`, confirm, re-run |
| `ReadSkewError` after the retry budget | RPC replicas disagreeing on `latestLedger` | re-run; if it persists, point at a single node you control |
| `latestLedger` went backwards mid-run | backend catching up after a restart | treated as skew and retried automatically; no action unless it persists |
| footprint report omits a stream you know exists | the stream is archived, or the read was skewed | check for `ArchivedEntryError`; if absent, re-run with `--pin-ledger` |
| invocation timed out | RPC latency, not contract state | re-run; the enforcer writes nothing |
| `RestoreFootprint` submitted but never confirmed | RPC indexer lag | re-submit; restoration is idempotent |

See `docs/KNOWN-LIMITATIONS.md` §1 for the underlying constraint on persistent
entry lifetime and why archival is expected rather than exceptional for
long-lived streams.
## Indexer recovery: skew, partial batches, and archived entries

The off-chain indexer (`schema/schema.graphql`, `schema/schema.sql`) consumes
Soroban RPC events and ledger entries. It faces three failure modes that the
client-side barrier above does not cover on its own: read skew across a batch,
partial batch failure mid-ingest, and reads against **archived** persistent
entries. This section documents how the schema and the recovery primitives
handle each.

### Idempotent retry semantics keyed on `(tx_hash, event_index)`

Every event row in the schema is uniquely identified by the pair
`(tx_hash, event_index)`. That pair is the idempotency key for the whole
ingest pipeline:

- `Event` (and its derived `StreamEvent` / `VestingEvent` rows) carries a
  `UNIQUE (tx_hash, event_index)` constraint in `schema/schema.sql` and a
  `@derivedFrom` / `@unique` directive pair in `schema/schema.graphql`.
- Inserts use `INSERT ... ON CONFLICT (tx_hash, event_index) DO NOTHING`
  (SQL) and upsert-by-id (subgraph), so replaying a batch that was already
  partially applied is a no-op for the rows that landed and a fresh insert
  for the rows that did not.
- Derived aggregates (`Stream.total_vested`, `Stream.total_withdrawn`,
  `Vesting.claimed`) are recomputed from the event log rather than
  incremented in place, so a replayed event cannot double-count.

This means a stalled or partially-failed batch can be **re-run from its
starting ledger** without truncating tables or reconciling by hand. The
indexer records the last fully-committed ledger in `IndexerCursor`; a re-run
starts at `IndexerCursor.last_ledger` and relies on the idempotency key to
absorb the overlap.

### RPC read skew inside a batch

A batch that spans a ledger boundary can read `latestLedger = N` for the
cursor update but `latestLedger = N-1` for a `getLedgerEntries` call in the
same batch. The schema handles this deterministically:

1. Each batch records the `latestLedger` returned by the **first** RPC call
   as `IndexerCursor.batch_ledger`.
2. Every subsequent read in the batch is accepted only if its
   `latestLedger >= batch_ledger`. A read that reports a lower ledger is
   discarded and retried; it is never merged into the batch.
3. `IndexerCursor.last_ledger` is only advanced to `batch_ledger` after the
   entire batch commits. A partially-applied batch leaves the cursor
   unchanged, so the next run re-reads the same range.

This is the indexer-side equivalent of the client barrier in the previous
section: pin the batch to one ledger, and refuse to advance the cursor past a
ledger whose events are not all present.

### Archival detection and footprint restoration

Persistent Soroban entries (stream records, vesting schedules) can be
**archived** once their TTL lapses. An archived entry is not deleted — it is
absent from the live ledger and must be restored before it can be read. The
indexer detects this by treating a missing entry as `ARCHIVED`, not as
`NOT_FOUND`:

- A `getLedgerEntries` response that omits a key the indexer expects is
  recorded as an `ArchivedEntry` row keyed on the same
  `(contract_id, ledger_key)` pair used by the live tables.
- The indexer does **not** delete or null out the corresponding `Stream` /
  `Vesting` row. The last known state is retained and flagged
  `archived = true`, so downstream consumers see a stale-but-valid record
  rather than a hole.
- Restoration is performed out-of-band by the recovery primitives below, and
  the indexer re-reads the restored entry on the next batch.

The two recovery primitives are:

- **`tools/sdk/restore_footprint.ts`** — builds and submits a
  `RestoreFootprint` operation for the archived ledger keys. It is idempotent:
  restoring an already-live key is a no-op, so it is safe to run repeatedly
  against a partially-restored footprint.
- **`script/archival-canary.sh`** — a scheduled probe that reads a known set
  of persistent keys and reports any that have gone missing. It is the
  detection half of the loop; `restore_footprint.ts` is the repair half.

The intended loop is: canary detects an archived key → operator (or the
canary's `--auto-restore` mode) runs `restore_footprint.ts` for that key →
indexer re-reads on the next batch and clears the `archived` flag.

### Operator runbook: re-running a stalled or partially-failed batch

Use this when `IndexerCursor.last_ledger` has stopped advancing, when the
canary reports archived keys, or when a batch failed partway through.

1. **Confirm the stall.** Check `IndexerCursor.last_ledger` against the
   current `getLatestLedger()`. A gap that is not shrinking across two or
   three poll intervals means the batch is stalled, not just slow.
2. **Check for archived entries.** Run `script/archival-canary.sh`. If it
   reports missing keys, run `tools/sdk/restore_footprint.ts` for those keys
   and wait for the restore transaction to reach `SUCCESS` before continuing.
   Do **not** re-run the batch against an unrestored footprint — the indexer
   will keep flagging the same keys as archived.
3. **Re-run the batch from the cursor.** Restart the indexer with
   `--from-ledger $(IndexerCursor.last_ledger)`. Because ingest is keyed on
   `(tx_hash, event_index)`, the overlap with the previously-applied portion
   of the batch is absorbed as a no-op. Do not truncate tables first; that
   discards the idempotency guarantee and can double-count derived
   aggregates.
4. **Verify the cursor advanced.** After the re-run, confirm
   `IndexerCursor.last_ledger` is at or beyond the ledger you expected and
   that no new `ArchivedEntry` rows were written for the range you just
   processed.
5. **If the cursor still does not advance,** the batch is failing for a
   reason other than skew or archival — check the RPC endpoint's health and
   the indexer logs for decode errors before retrying again. Repeated
   re-runs are safe, but they will not fix a malformed event.

The key property to rely on: **re-running a batch is always safe.** The
idempotency key makes replay a no-op for already-applied events, the cursor
only advances on full commit, and archived entries are flagged rather than
dropped. There is no scenario in this pipeline where the correct operator
action is to delete rows and start over.

### Operator runbook: Governance state archival detection & footprint restoration

When governance proposals or multi-sig voting operations fail with entry-not-found or timeout errors:

1. **Detect persistent storage archival.** Run the gas and archival analysis model:
   ```bash
   python3 script/governance-gas-model.py --check-archival --proposal-ids 0 1 2 --proposal-count 10
   ```
   If entries report `Archived` (meaning `proposal_id < proposal_count` but the persistent storage entry is not live), Soroban state archival has moved the entry to cold storage.
2. **Execute footprint restoration.** Run the SDK restoration tool targeting the archived persistent keys:
   ```bash
   npx ts-node tools/sdk/restore_footprint.ts \
     --contract <GOVERNANCE_CONTRACT_ID> \
     --stream-id <PROPOSAL_ID> \
     --source-secret <OPERATOR_SECRET> \
     --horizon <HORIZON_URL> \
     --network-passphrase "<PASSPHRASE>"
   ```
3. **Verify restored status.** Re-run `--check-archival` to verify that all proposal entries report `Live`.
4. **Idempotent replay.** Re-submit the voting or execution transaction. Because proposal IDs are immutable and non-reusable, retrying is strictly idempotent and safe against double-spend or duplicate votes.

### Operator runbook: Distinguishing RPC read skew from true state archival

Before initiating on-chain restoration transactions, verify whether a missing entry is simply due to RPC read skew across distributed nodes:

1. **Sample multiple RPC endpoints:**
   Query `getLatestLedger()` consecutively across your endpoint pool or perform 3 back-to-back queries.
2. **Evaluate ledger divergence:**
   - **If responses disagree on `latestLedger`:** This is read skew, not state archival. A lagging node serving ledger *N-5* will not see an entry created at ledger *N*. Do NOT submit a restore transaction. Wait 2–5 seconds with exponential backoff for the node to catch up.
   - **If responses agree on `latestLedger`:** All replicas are synchronized at the current network height, and an entry whose ID is lower than the contract's total count reports missing. This is genuine state archival. Proceed with footprint restoration immediately.

## What this is not

**Not a consensus or data-integrity problem.** Any node serving an older ledger
is serving a valid, internally consistent view of a real ledger. Ledger *N* is
not wrong; it is just older than *N+5*.

**Not a criticism of the RPC infrastructure.** Read-your-writes consistency
across load-balanced replicas is a standard distributed-systems tradeoff, not a
defect, and the same caveat applies to essentially every hosted blockchain RPC.

**Not Soroban-specific.** It is worth writing down only because the client
tooling presents one URL as though it were one node, and because in a
*streaming* contract — where the correct answer changes every second — the
symptom impersonates a contract bug closely enough to send you debugging the
wrong thing.

## Summary for client authors

| do | don't |
|---|---|
| derive everything from one call where possible | combine two view calls into one number |
| barrier after writes, requiring several consecutive samples | read immediately after a write |
| check `latestLedger` on each response and discard mismatched sets | assume consecutive reads are monotonic |
| assert within a justified tolerance in tests | assert exact cross-call equality against a public endpoint |
| run your own node if you need strict read-after-write | conclude the endpoint is fine because one loop looked clean |

## Keeper recovery procedure (`script/keeper-extend-ttl.sh`)

The batch TTL renewal runner (`script/keeper-extend-ttl.sh`) is the place where
read skew, Soroban state archival, and partial batch failure all meet. It sweeps
a list of persistent entries and calls `batch_extend_ttl` on each chunk. This
section documents the recovery procedure required by
[#292](https://github.com/hannyloveworld/Perpetua-Contracts/issues/292): how the
hardened script pins its reads, how it detects archived entries, how it retries
idempotently, and what an operator does when a sweep fails partway through.

### The `latestLedger` barrier before each sweep

Every sweep begins with a barrier, not with a read. The script samples
`getLatestLedger` and requires several *consecutive* samples at or above the
target before it will build a footprint or submit anything:

```bash
# script/keeper-extend-ttl.sh (abridged)
barrier() {
  local target="$1" consecutive="${2:-4}" seen=0 tries=0 latest
  while [ "$seen" -lt "$consecutive" ] && [ "$tries" -lt 40 ]; do
    latest=$(latest_ledger)
    if [ "$latest" -ge "$target" ]; then seen=$((seen + 1)); else seen=0; fi
    tries=$((tries + 1)); sleep 1
  done
  [ "$seen" -ge "$consecutive" ] || die "RPC did not reach ledger $target"
}
```

The target is the ledger returned by the last confirmed write (or, on a cold
sweep, the ledger the entry list was built from). Requiring consecutive samples
is what makes the barrier meaningful: a single passing sample only proves *one*
backend has caught up, and the next request may land on another. The script
records the barrier ledger in its JSON output as `barrierLedger` so a re-run can
prove it observed the same or a later height.

### Archival detection and `RestoreFootprint`

A persistent entry whose TTL has already lapsed is *archived*: it is no longer
in the live ledger, and `batch_extend_ttl` against it fails rather than
extending it. The script detects this explicitly instead of treating it as a
generic error:

1. Before submitting, the script reads the footprint with `getLedgerEntries`.
   Any key that comes back missing (or with a lapsed `liveUntilLedgerSeq`) is
   classified as **archived**, not as a transient failure.
2. Archived keys are collected and passed to a `RestoreFootprint` operation
   before the TTL extension is retried. The script shells out to
   [`tools/sdk/restore_footprint.ts`](../tools/sdk/restore_footprint.ts), which
   builds the restore transaction from the same `LedgerKey` set:

   ```bash
   npx ts-node tools/sdk/restore_footprint.ts \
     --network testnet \
     --keys "$ARCHIVED_KEYS_JSON" \
     --json
   ```

3. Only after the restore transaction reaches `SUCCESS` — and after the barrier
   has advanced past the restore's committed ledger — does the script re-issue
   `batch_extend_ttl` for the affected chunk.

Restoration is itself a write, so it is subject to the same read-after-write
rule as any other write: wait for the committed ledger, then wait for the
endpoint to report that ledger repeatedly, *then* read or extend. Skipping the
barrier here is how a keeper "successfully" restores an entry and then
immediately fails to extend it because it read a replica that had not yet seen
the restore.

### Idempotent retry semantics

The script is safe to re-run at any point, including after a timeout where it
does not know whether its last submission landed. It achieves this by making
every step idempotent:

- **TTL extension is idempotent.** Extending an entry that already has a
  sufficient `liveUntilLedgerSeq` is a no-op; the script skips keys whose
  remaining TTL is already above the target rather than re-submitting them.
- **Restore is idempotent.** `RestoreFootprint` on an already-live entry is a
  no-op. Re-running the restore step for a chunk that was already restored
  succeeds without changing state.
- **Chunk progress is journaled.** Each chunk's outcome is written to the
  run journal (`--journal <path>`, default `.keeper-extend-ttl.journal`) before
  the next chunk starts. A re-run reads the journal and skips chunks already
  marked `done`, so a partial batch failure resumes rather than restarting.
- **Submissions are keyed, not counted.** The script identifies each chunk by
  the sorted set of ledger keys it covers, not by its position in the list. If
  the entry list is rebuilt between runs and the ordering changes, the journal
  still matches chunks correctly.

Because of these properties, the correct operator response to *any* uncertain
outcome — timeout, `TRY_AGAIN_LATER`, a dropped connection mid-submit — is to
re-run the same command. Do not hand-edit the journal, and do not assume a
timeout means the write did not land.

### Operator runbook

**Detecting a partial batch failure.** Run the keeper with `--json` and inspect
the summary. A partial failure looks like this:

```bash
$ script/keeper-extend-ttl.sh --json --network testnet --entries entries.json
{
  "barrierLedger": 4097076,
  "chunks": [
    { "id": "chunk-0", "keys": 12, "status": "done", "extended": 12 },
    { "id": "chunk-1", "keys": 12, "status": "archived", "archived": 3 },
    { "id": "chunk-2", "keys": 12, "status": "failed", "error": "TRY_AGAIN_LATER" }
  ],
  "restored": ["chunk-1"],
  "journal": ".keeper-extend-ttl.journal",
  "exitCode": 1
}
```

Read it as follows:

- `status: "done"` — the chunk's TTLs were extended (or were already sufficient).
- `status: "archived"` — the chunk contained archived keys; the script attempted
  a `RestoreFootprint` and recorded the result under `restored`. If the restore
  itself failed, the chunk stays `archived` and the run exits non-zero.
- `status: "failed"` — the chunk did not complete. `error` is the RPC or
  transaction error. `TRY_AGAIN_LATER` and timeouts are retryable; a
  `FAILED` transaction with a contract error is not, and needs investigation
  before re-running.
- `exitCode` — non-zero if any chunk is not `done`. A non-zero exit with some
  chunks `done` is exactly the partial-failure case.

**Re-running safely.** Re-run the identical command. The journal causes `done`
chunks to be skipped, `archived` chunks to retry their restore, and `failed`
chunks to be re-submitted. The barrier runs again at the start of the re-run, so
the new sweep observes a ledger at or beyond the previous `barrierLedger`:

```bash
$ script/keeper-extend-ttl.sh --json --network testnet --entries entries.json
{
  "barrierLedger": 4097091,
  "chunks": [
    { "id": "chunk-0", "keys": 12, "status": "done", "extended": 0, "skipped": 12 },
    { "id": "chunk-1", "keys": 12, "status": "done", "extended": 12 },
    { "id": "chunk-2", "keys": 12, "status": "done", "extended": 12 }
  ],
  "restored": [],
  "journal": ".keeper-extend-ttl.journal",
  "exitCode": 0
}
```

Note `chunk-0` reporting `extended: 0, skipped: 12`: those TTLs were already
sufficient, which is the idempotency guarantee in action. `chunk-1` moved from
`archived` to `done` because the restore succeeded and the extension was
re-issued after the barrier advanced past the restore's ledger.

**Confirming recovery.** Recovery is confirmed when a re-run exits `0` with
every chunk `done` *and* a subsequent read of the entries shows
`liveUntilLedgerSeq` above the target for all of them. The script's `--verify`
flag performs that read behind a fresh barrier:

```bash
$ script/keeper-extend-ttl.sh --verify --json --network testnet --entries entries.json
{
  "barrierLedger": 4097092,
  "verified": 36,
  "belowTarget": 0,
  "exitCode": 0
}
```

A non-zero `belowTarget` means at least one entry is still short of its target
TTL — re-run the sweep, and if the same entry stays short across two runs,
inspect it directly with `getLedgerEntries` before assuming the keeper is at
fault.

**When to escalate.** Escalate rather than re-running if: a chunk fails with a
contract error (not `TRY_AGAIN_LATER`/timeout); a restore transaction reaches
`FAILED`; or `belowTarget` is non-zero after two clean sweeps. In those cases
the entry may be genuinely unrecoverable, and the operator should stop the
keeper and page the on-call engineer rather than looping.

## Batch creation recovery

`tools/sdk/batch_create.ts` (`submitBatchWithRecovery`) applies these rules to `create_stream` batches: restore archived footprints then retry the same transaction; after a timeout, confirm via `stream_count()` (rejecting reads from a node behind the submit ledger) before any resubmission. Operators: on "RPC read skew" switch RPC node; on "Unexpected stream_count delta" stop and reconcile from `stream_created` events. Full procedure in [batch_create.md](batch_create.md).

## Wallet submission recovery (`tools/sdk/wallet.ts`)

`submitWithRecovery(signed, { submit, restore })` resubmits the **same signed
XDR** on retry, so a timed-out submission that actually landed cannot apply
twice (the duplicate sequence number is rejected). `classifyWalletFailure`
maps errors to categories:

| Kind | Meaning | Action |
|---|---|---|
| `archived` | persistent entry expired | run `restore()` (RestoreFootprint), back off, resubmit |
| `read_skew` | RPC node lagging | back off, resubmit |
| `timeout` | outcome unknown | back off, resubmit same XDR |
| `rejected` | user declined in wallet | rethrow, never retried |
| `fatal` | anything else | rethrow, never retried |

Troubleshooting: if `submitWithRecovery` throws after `maxAttempts`, inspect the
transaction hash on RPC before re-signing; only re-sign if the hash is unknown.

## SDK recovery helpers (`tools/sdk/errors.ts`)

| Helper | Purpose |
|---|---|
| `isArchivedEntryError(msg)` | Detects archived/expired persistent entries in simulation errors. |
| `isRetryableFailure(msg)` | Detects transient failures (`TRY_AGAIN_LATER`, timeouts, 503/504, `txBadSeq`). |
| `isLedgerSkewed(rpcLatest, observed, tol)` | True when the RPC node lags a ledger the caller already saw. Throws `RangeError` on invalid input. |
| `diagnoseFailure({contractErrorCode, message})` | Returns `restore_footprint`, `retry`, `escalate` or `none`. Contract errors are never retried. |
| `retryKey(contractId, method, args)` | Deterministic idempotency key: before retrying a timed-out call, look up whether the original transaction landed. |

**Recovery runbook.** (1) On `restore_footprint`: submit RestoreFootprint (`restore_footprint.ts`), wait for confirmation, then retry. (2) On `retry`: check the `retryKey` for an already-landed transaction, then resubmit. (3) On skew: re-read from another RPC node or wait until `latestLedger` catches up; never act on a skewed read. (4) On `escalate`: stop and page on-call.

**Troubleshooting.** `Unknown stream error (code N)` means the SDK table is older than the contract; regenerate the SDK. Example: `diagnoseFailure({ message: "entry archived" })` returns `{ action: "restore_footprint", ... }`.

---

*Written while building [Perpetua](https://github.com/hannyloveworld/Perpetua-Contracts),
a continuous payment streaming primitive for Soroban. The pattern is the point;
the observation is one data point. Corrections and contradicting data are both
welcome, and we will amend this note.*

## Recovery procedures (archival and skew)

Pure helpers in `contracts/stream/src/accrual.rs` encode the decisions:

| Situation | Helper | Action |
|---|---|---|
| Read ledger behind the ledger of a confirmed write | `classify_read` → `Stale` | Discard the read, re-poll until `latestLedger >= required`; never combine it with another read |
| `get_stream` fails, id `< next_stream_id` | `recovery_for` → `RestoreFootprint` | Submit `RestoreFootprint` for `DataKey::Stream(id)`, then retry |
| `get_stream` fails, id `>= next_stream_id` | `recovery_for` → `NeverExisted` | Do not retry; the id was never issued |
| Retry of a timed-out withdraw | `remaining_to_apply` | Re-read `withdrawn`; if the effect is already applied the remainder is `0` — do not resubmit |

`storage::stream_recovery` applies the same decision on-chain. Tokens are never
lost on archival; only the accounting entry is unreadable until restored.
Operators: run `script/archival-canary.sh` and `script/keeper-extend-ttl.sh`
to keep entries live. Regression pins: `src/test/hardening_177_181_184_190.rs`.

## Client recovery: `perpetua-cli` and `useStream`

Both `tools/perpetua-cli.ts` and `sdk/react-hooks/src/useStream.ts` apply the
same rules:

- **Archived entries.** `stream_exists(id) == false && id < stream_count()`
  means the entry is archived, not absent. Submit a `RestoreFootprint`
  transaction for `DataKey::Stream(id)` and refresh. `useStream` exposes
  `errorKind === 'archived'`; the CLI's `classifyFailure` returns restore guidance.
- **Read skew.** A read from a ledger older than one already applied is
  discarded (`isLedgerSkewed` / `isStaleRead`); the last good stream is kept.
- **Idempotent retry.** `retryIdempotent` checks whether the invocation already
  landed before every attempt, retries only `TRY_AGAIN_LATER`/timeouts, and
  never retries contract errors (escalate instead).

## Operator Runbook: Idempotent Retries & Read-Skew Mitigation

### 1. Alerting & Incident Triggers
- **Trigger A (Ledger Regression):** `latestLedger(t1) < latestLedger(t0)` detected by RPC sentinel probes.
- **Trigger B (Stale Read Skew):** Multiple view calls within the same transaction derive inconsistent values (e.g., `vested + refundable != deposited`).
- **Trigger C (Transient RPC Exhaustion):** Invocations return HTTP 429, HTTP 503, or connection timeouts.

### 2. Immediate Operator Steps
1. **Verify Endpoint Health:**
   ```bash
   curl -s -X POST "$SOROBAN_RPC_URL" \
     -H 'Content-Type: application/json' \
     -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}'
   ```
2. **Apply Idempotent Retry Barrier:**
   - In frontend/SDK clients, configure `withIdempotentRetry` with bounded exponential backoff (`maxRetries: 3`, `baseDelayMs: 50`, `maxDelayMs: 1000`).
   - For mutation retries, verify whether the transaction hash or sequence already executed before rebroadcasting.
3. **Execute Archival Footprint Restoration:**
   - If `stream_exists(id) == false` while `id < stream_count()`, execute footprint restoration:
     ```bash
     stellar contract restore --id "$CONTRACT_ID" --key "['Stream', $STREAM_ID]" --network "$NETWORK"
     ```
4. **Failover RPC Endpoint:**
   - If skew persists across node replicas behind a load balancer, route traffic to a synchronized single-node backup endpoint or pinned ledger sequence.

