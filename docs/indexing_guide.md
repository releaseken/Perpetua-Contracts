# Indexing Perpetua: Soroban RPC and Horizon

Perpetua has two useful Stellar data surfaces, but they answer different
questions:

| Need | Recommended source | Reason |
|---|---|---|
| Discover or replay Perpetua contract events | Soroban RPC `getEvents` | Contract events are returned with their contract ID, topics, value, ledger, transaction hash, and event indexes. |
| Read current stream state | Soroban RPC `getLedgerEntries` or contract simulation | The contract storage is the source of truth for the current state. Use the generated contract client to decode values. |
| Find payments, account activity, trustlines, offers, and asset history | Horizon REST | Horizon's resource model is designed for account, operation, payment, and asset queries. |
| Link a contract event to a transaction or inspect transaction status | Soroban RPC, optionally Horizon | RPC supplies the event's transaction hash and ledger context; use Horizon only when its transaction/resource view is useful for the same network. |

Horizon is not a substitute for `getEvents` when the indexer needs every
Perpetua event. A payment operation may show token movement, but it does not
replace the contract event's stream ID, event-specific payload, or event order.

## Perpetua event model

The stream contract emits ten event types. `topic[0]` is always the event name
in snake case, and `topic[1]` is always `stream_id`; the remaining slots hold
the parties the event routes on.

| event | topic[1] | topic[2] | topic[3] |
|---|---|---|---|
| `stream_created` | `stream_id` | `sender` | `recipient` |
| `withdrawn` | `stream_id` | `recipient` | — |
| `cancelled` | `stream_id` | `sender` | `recipient` |
| `paused` | `stream_id` | `sender` | — |
| `resumed` | `stream_id` | `sender` | — |
| `topped_up` | `stream_id` | `sender` | — |
| `recipient_transferred` | `stream_id` | `old_recipient` | `new_recipient` |
| `delegate_granted` | `stream_id` | `grantor` | `delegate` |
| `delegate_revoked` | `stream_id` | `grantor` | `delegate` |
| `ttl_extended` | `stream_id` | — | — |

**Topic arity is not constant.** It ranges from 2 (`ttl_extended`) to 4
(`stream_created`, `cancelled`, `recipient_transferred`, and both delegate
events). A consumer must read the topic count per event rather than assuming
four entries.

`stream_created` carries the complete initial stream state in its payload and
is the bootstrap event for building a sender/recipient mapping. `withdrawn`
carries the amount and cumulative accounting fields. The full payload layout
and the ordering rationale are in [ABI.md](ABI.md#topic-positions), which is
the interface of record.

A `stream_created` event's topic vector is therefore:

```text
["stream_created", stream_id, sender, recipient]
```

The values sent to JSON-RPC are base64-encoded XDR `SCVal` values, not plain
text. Do not put the literal string `stream_created` or a raw integer in a
filter. Use the Stellar SDK's `ScVal`/XDR encoder for topic filters and decoder
for returned topics and values. The contract interface is the decoding source
of truth; see [`contracts/stream/abi/fluxora_stream.json`](../contracts/stream/abi/fluxora_stream.json).

## Event reconstruction rules

This section is normative. An indexer that follows these rules will produce a
byte-for-byte identical projection to any other conforming indexer, and to the
on-chain state, for the same set of events. The rules are machine-checkable:
[`script/event-byte-budget.py`](../script/event-byte-budget.py) encodes the
canonical topic layout and payload field order in its `EVENT_SCHEMA` table and
exposes `schema_fingerprint()` as a stable digest of that schema. Treat
`EVENT_SCHEMA` and `schema_fingerprint()` as the source of truth; this document
is a human-readable restatement of them.

### Topic layout

Every Perpetua event uses the same positional topic layout:

1. `topic[0]` — the event name as a snake_case `symbol` (for example
   `stream_created`). This is the discriminator an indexer switches on.
2. `topic[1]` — `stream_id`, encoded as `u64`. Every event carries it, so a
   stream-scoped projection can always be keyed by `(contract_id, stream_id)`.
3. `topic[2..]` — the routing parties for the event, in the order listed in
   the table above. These are `address` values. Events with no routing parties
   (`ttl_extended`) stop at `topic[1]`; events with one or two parties append
   them in the documented order.

The number of topics is therefore `2 + len(routing_parties)` and is **not**
constant across event types. Read the topic count from the event; never assume
a fixed arity. The routing-party order is part of the schema: `cancelled`
routes `(sender, recipient)` and `recipient_transferred` routes
`(old_recipient, new_recipient)`, and swapping them is a schema violation even
though the arity is identical.

### Payload field order

The event `data` payload is a struct whose fields are ordered exactly as
declared in `EVENT_SCHEMA`. Field order is significant because the payload is
serialized positionally; an indexer must decode fields by position, not by
name, and must not reorder them. Per-event payload layouts:

- `stream_created` — the complete initial stream state (sender, recipient,
  token, amounts, schedule, and status). This is the bootstrap event; a
  projection is built from it and then mutated by later events.
- `withdrawn` — the withdrawn amount plus the cumulative accounting fields.
- `cancelled` — the terminal accounting fields for the stream.
- `paused`, `resumed`, `topped_up` — the fields describing the transition.
- `recipient_transferred` — the old and new recipient.
- `delegate_granted`, `delegate_revoked` — the grantor and delegate.
- `ttl_extended` — the extension metadata.

Decode amounts as `i128` token base units and keep them as integers or decimal
strings; never pass them through an IEEE-754 `Number`.

### Forward compatibility: tolerate unknown trailing fields

Indexers **must tolerate unknown trailing fields** in the payload. A future
contract version may append fields to an existing event's payload without
changing its topic layout or its leading field order. A conforming decoder:

- reads the fields it knows by position from the front of the payload,
- ignores any additional trailing fields it does not recognize, and
- does not fail, truncate, or reorder the event when extra fields are present.

This keeps the topic layout and the leading payload prefix stable across
contract upgrades, so an indexer built against an older `EVENT_SCHEMA` keeps
working against a newer contract. Removing or reordering existing fields is a
breaking change and requires a new event name; appending trailing fields is
not.

### Parity corpus and CI checks

The golden JSON fixtures under
[`tests/fixtures/event_snapshots/`](../tests/fixtures/event_snapshots/) are the
parity corpus for these rules. Each fixture is a frozen, canonical
serialization of an event (topics plus payload) that every implementation —
Rust contract, TypeScript SDK, and GraphQL/SQL schema — must reproduce exactly.

CI enforces parity with:

- [`script/event-byte-budget.py`](../script/event-byte-budget.py) — the
  `EVENT_SCHEMA` table and `schema_fingerprint()` that define the canonical
  schema, plus the 16 KiB byte-footprint budget.
- [`script/verify_event_bytes.py`](../script/verify_event_bytes.py) — verifies
  each event's encoded footprint against the budget and the schema.
- [`script/check_snapshot_diff.py`](../script/check_snapshot_diff.py) — diffs
  freshly generated snapshots against the golden fixtures to detect schema
  drift.

A change to the topic layout or payload field order must update `EVENT_SCHEMA`,
regenerate the golden fixtures, and pass all three scripts; otherwise CI fails
with a schema-drift error.

## `getEvents` filter

A request for Perpetua events uses the Soroban RPC `getEvents` method. The
contract ID belongs in `filters[].contractIds`:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "getEvents",
  "params": {
    "startLedger": 123456,
    "endLedger": 124000,
    "filters": [
      {
        "type": "contract",
        "contractIds": ["C...PERPETUA_CONTRACT_ID..."],
        "topics": [["AAA...base64-SCVal(stream_created)..."]]
      }
    ],
    "pagination": {
      "limit": 1000
    }
  }
}
```

Important details:

- `startLedger` is the checkpoint to resume from. Set `endLedger` to bound a
  backfill; omit it when following new closed ledgers, subject to the RPC
  provider's limits.
- `contractIds` is the strongest filter and should always contain the exact
  Perpetua contract ID. Do not rely on event names alone when sharing an RPC
  endpoint with other contracts.
- `topics` is optional. A topic filter is positional: the first item matches
  `topic[0]`, the second matches `topic[1]`, and so on. Use the RPC wildcard
  for positions that should not be constrained. For example, a stream-specific
  filter can match `stream_created` at position zero and the encoded stream ID
  at position one.
- A filter with only `contractIds` is usually the safest way to backfill every
  Perpetua event. Narrow topic filters are useful for live projections, but a
  new event type can otherwise be missed by an indexer that has hard-coded an
  incomplete filter set.
- Consume every page before advancing the checkpoint. Persist the last fully
  processed ledger and event identity, and use a small overlap when restarting
  so a provider boundary or process failure cannot create a gap. Deduplicate by
  `(txHash, eventIndex)`; retain `ledger`, `txIndex`, and the raw event for
  auditing.
- Decode amounts as integers or decimal strings. Perpetua amounts are `i128`
  token base units and must never pass through an IEEE-754 JavaScript `Number`.
- `getEvents` is for closed-ledger history. It is not a mempool subscription and
  it does not replace confirmation handling for a submitted transaction.

For a stream-specific query, the conceptual filter is:

```json
{
  "type": "contract",
  "contractIds": ["C...PERPETUA_CONTRACT_ID..."],
  "topics": [
    ["AAA...SCVal(symbol('stream_created'))...", "AAA...SCVal(u64(42))..."]
  ]
}
```

The exact base64 strings should be generated by the SDK, not copied from this
example. Topic indexes are defined by the deployed contract ABI and must be
updated if a new contract deployment changes the event schema.

## Minimal JavaScript indexer

This example uses native `fetch` and treats the event topic/value fields as
opaque until they are decoded with the Stellar SDK generated from the contract
spec. It demonstrates a contract-wide backfill with a persisted ledger
checkpoint and event deduplication.

```js
const RPC_URL = process.env.SOROBAN_RPC_URL;
const CONTRACT_ID = process.env.PERPETUA_STREAM_CONTRACT_ID;
let nextLedger = Number(process.env.START_LEDGER);
const seen = new Set();

async function getEvents(startLedger, endLedger) {
  const response = await fetch(RPC_URL, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "getEvents",
      params: {
        startLedger,
        ...(endLedger === undefined ? {} : { endLedger }),
        filters: [{ type: "contract", contractIds: [CONTRACT_ID] }],
        pagination: { limit: 1000 },
      },
    }),
  });

  const body = await response.json();
  if (body.error) throw new Error(JSON.stringify(body.error));
  return body.result;
}

async function indexClosedLedgers(endLedger) {
  const page = await getEvents(nextLedger, endLedger);
  for (const event of page.events) {
    const identity = `${event.txHash}:${event.eventIndex}`;
    if (seen.has(identity)) continue;
    seen.add(identity);

    // Decode event.topic and event.value with the Stellar SDK and the
    // fluxora_stream ABI before writing the typed projection.
    await saveRawEvent({ identity, event });
  }

  // Persist this only after saveRawEvent has committed successfully. In a
  // production indexer, use the provider cursor as well when more pages exist.
  if (page.events.length > 0) {
    nextLedger = Math.max(...page.events.map((event) => event.ledger)) + 1;
    await saveCheckpoint(nextLedger);
  }
}
```

For a production indexer, replace the in-memory `seen` set with a database
unique constraint on `(contract_id, tx_hash, event_index)`, persist the RPC
pagination cursor, and retry failed pages before moving the ledger checkpoint.
Use a generated decoder rather than relying on the event names alone.

## Minimal Python request

The same contract-wide filter can be issued with Python's standard library:

```python
import json
import os
import urllib.request

payload = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "getEvents",
    "params": {
        "startLedger": int(os.environ["START_LEDGER"]),
        "endLedger": int(os.environ["END_LEDGER"]),
        "filters": [{
            "type": "contract",
            "contractIds": [os.environ["PERPETUA_STREAM_CONTRACT_ID"]],
        }],
        "pagination": {"limit": 1000},
    },
}

request = urllib.request.Request(
    os.environ["SOROBAN_RPC_URL"],
    data=json.dumps(payload).encode("utf-8"),
    headers={"content-type": "application/json"},
)
with urllib.request.urlopen(request) as response:
    result = json.load(response)

if "error" in result:
    raise RuntimeError(result["error"])
for event in result["result"]["events"]:
    print(event["ledger"], event["txHash"], event["eventIndex"])
```

This Python snippet intentionally prints raw event metadata only. Decode the
base64 XDR topics and value with a Stellar Python SDK or XDR implementation
before interpreting stream IDs, addresses, statuses, or amounts.

## Event reconstruction rules for the off-chain schema

The GraphQL schema in [`schema/schema.graphql`](../schema/schema.graphql) is a
thin, deterministic projection over the raw `events` table. Every GraphQL event
type maps 1:1 to a row in `events`, and every `streams` field is derived by
folding those rows forward. The rules below are the contract between the
indexer and the schema; they must not drift from
[ABI.md](ABI.md#topic-positions).

### Raw `events` table row

Each decoded Soroban event is persisted as exactly one row:

| column | source | notes |
|---|---|---|
| `id` | indexer sequence | Monotonic surrogate key; ordering only. |
| `contract_id` | `getEvents` filter | Always the Perpetua stream contract ID. |
| `ledger` | `event.ledger` | Closed ledger sequence. |
| `tx_hash` | `event.txHash` | Hex transaction hash. |
| `tx_index` | `event.txIndex` | Transaction position within the ledger. |
| `event_index` | `event.eventIndex` | Event position within the transaction. |
| `topic0` | `event.topic[0]` | Decoded event name, e.g. `stream_created`. |
| `stream_id` | `event.topic[1]` | Decoded `u64` stream ID; `NULL` only if the ABI ever drops it. |
| `topics` | `event.topic[1..]` | Remaining decoded topics as JSONB, preserving order. |
| `payload` | `event.value` | Decoded event body as JSONB, per the ABI payload layout. |
| `raw_topics` | `event.topic` | Base64 XDR topics, retained for re-decoding. |
| `raw_value` | `event.value` | Base64 XDR value, retained for re-decoding. |
| `event_version` | decoder | ABI/decoder version used to produce `payload`. |

`topic0` is the snake_case event name and is the discriminator the GraphQL
resolvers switch on. `stream_id` is lifted out of `topic[1]` so that stream
queries never have to scan JSONB topics. The remaining topics stay in `topics`
in positional order because their arity varies from 2 to 4 (see the event model
table above); consumers must read the topic count per event rather than assume
a fixed width.

### Folding events into the `streams` projection

The `streams` projection is rebuilt by replaying `events` in
`(ledger, tx_index, event_index)` order and applying each event to the stream
identified by `stream_id`:

1. `stream_created` bootstraps the row: it sets `sender`, `recipient`,
   `token`, `deposit_amount`, `start_time`, `end_time`, `cliff_time`,
   `status`, and the initial accounting fields from `payload`.
2. `withdrawn` adds to `withdrawn_amount` and updates the cumulative
   accounting fields from `payload`.
3. `cancelled`, `paused`, `resumed`, and `topped_up` transition `status` and
   adjust `deposit_amount`/`remaining_amount` as defined by the ABI payload.
4. `recipient_transferred` updates `recipient`; `delegate_granted` and
   `delegate_revoked` update the delegate set; `ttl_extended` updates
   `last_extended_ledger` only.

Folding is deterministic and order-sensitive: never apply events out of
`(ledger, tx_index, event_index)` order, and never apply an event whose
`stream_created` predecessor has not been seen. If a fold encounters an event
for an unknown `stream_id`, stop and reconcile with `getLedgerEntries` rather
than inventing a partial row.

### Idempotent dedup key

The dedup key is `(tx_hash, event_index)`. It is the only identity that is
stable across re-org-free Soroban history and across RPC pagination retries.
Enforce it with a database unique constraint:

```sql
CREATE UNIQUE INDEX events_dedup ON events (tx_hash, event_index);
```

Insert with `ON CONFLICT (tx_hash, event_index) DO NOTHING` so that a retried
page, a restarted backfill, or an overlapping checkpoint window cannot create a
duplicate event or double-apply a fold. `(contract_id, tx_hash, event_index)`
is also unique and is the preferred key when a single database indexes more
than one contract.

### Detecting and recovering archived streams

Soroban persistent entries can expire. A stream whose entry has been archived
still has historical events in `events`, but its live storage is gone. Detect
this condition before serving a stream:

```text
stream_exists == false && stream_id < stream_count()  ->  archived
```

When the condition holds, the stream is archived, not missing. Recover it by
submitting a `RestoreFootprint` operation for the stream's persistent entry,
then re-read the entry with `getLedgerEntries` and re-fold any events that
landed while the entry was archived. Do not delete the projection row and do
not treat the stream as cancelled; archival is a storage condition, not a
contract state transition.

Recovery is idempotent: `RestoreFootprint` is safe to retry, and re-folding
events is safe because the dedup key prevents double application. If a restore
times out, retry the same footprint before advancing the checkpoint.

## Read-skew handling

RPC reads can be served from different ledgers within a single logical
operation, which produces torn views (for example, a `streams` row from ledger
N and a `withdrawn_amount` from ledger N+1). Pin every multi-view read to one
ledger:

1. Resolve a single `latestLedger` for the operation.
2. Pass that ledger to every `getLedgerEntries` call in the operation.
3. Reject or retry any response whose `latestLedger` differs from the pinned
   value.
4. Only advance the indexer checkpoint after all reads for the operation agree
   on the pinned ledger.

The full failure taxonomy, retry policy, and provider-specific caveats are in
[`docs/soroban-rpc-read-skew.md`](soroban-rpc-read-skew.md). Treat that document
as the interface of record for skew handling.

## Usage examples

Query a stream projection and its recent events through GraphQL:

```graphql
query StreamWithEvents($id: ID!) {
  stream(id: $id) {
    id
    sender
    recipient
    status
    depositAmount
    withdrawnAmount
    events(orderBy: { ledger: ASC, txIndex: ASC, eventIndex: ASC }) {
      topic0
      streamId
      payload
      ledger
      txHash
      eventIndex
    }
  }
}
```

Reconstruct the same projection directly from SQL:

```sql
SELECT
  e.topic0,
  e.stream_id,
  e.payload,
  e.ledger,
  e.tx_hash,
  e.event_index
FROM events e
WHERE e.stream_id = $1
ORDER BY e.ledger, e.tx_index, e.event_index;
```

Insert a decoded event idempotently:

```sql
INSERT INTO events (
  contract_id, ledger, tx_hash, tx_index, event_index,
  topic0, stream_id, topics, payload, raw_topics, raw_value, event_version
) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
ON CONFLICT (tx_hash, event_index) DO NOTHING;
```

## Troubleshooting

| Symptom | Likely cause | Resolution |
|---|---|---|
| A stream is missing from `streams` but its events exist | `stream_created` was never folded, or the fold stopped on an unknown `stream_id`. | Replay `events` for that `stream_id` in `(ledger, tx_index, event_index)` order. |
| Duplicate events after a restart | The dedup key was not enforced, or the checkpoint overlapped without `ON CONFLICT`. | Add the `(tx_hash, event_index)` unique index and re-run the backfill. |
| `stream_exists == false` for a stream below `stream_count()` | The persistent entry was archived. | Submit `RestoreFootprint`, re-read with `getLedgerEntries`, and re-fold. |
| `withdrawn_amount` jumps backwards | A multi-view read was served from different ledgers. | Pin all reads in the operation to one `latestLedger` and retry on mismatch. |
| `payload` fields decode to the wrong types | The decoder version does not match the ABI used at emit time. | Re-decode from `raw_topics`/`raw_value` with the correct `event_version`. |
| Topic arity mismatch | The consumer assumed a fixed four-topic layout. | Read the topic count per event; arity ranges from 2 to 4. |
## Reconstructing stream state from `ttl_extended` events

The Automated Keeper Batch TTL Renewal Runner
([`script/keeper-extend-ttl.sh`](../script/keeper-extend-ttl.sh)) emits one
`ttl_extended` event per stream it renews. These events are the only on-chain
record of a keeper sweep, so an indexer must be able to rebuild the renewed TTL
state from them alone.

### Topic layout

`ttl_extended` is the narrowest Perpetua event. It has exactly two topics and
no routing parties:

| position | value | notes |
|---|---|---|
| `topic[0]` | `symbol("ttl_extended")` | base64-encoded XDR `SCVal` |
| `topic[1]` | `u64(stream_id)` | base64-encoded XDR `SCVal` |

There is no `topic[2]` or `topic[3]`. A decoder that assumes four topics will
read past the end of the vector; always branch on `topic.length` (see
[ABI.md](ABI.md#topic-positions) for the frozen layout of every event).

### Payload schema

The event value is a single `i128`-free integer map with one field:

```text
{ extended_to_ledgers: u32 }
```

`extended_to_ledgers` is the number of ledgers the keeper added to the stream's
live-until ledger in this sweep, not the absolute expiry. The absolute expiry
is `previous_live_until_ledger + extended_to_ledgers`, so a projection that
only stores the delta must be replayed in ledger order to stay correct.

### Mapping to `ttlExtendedAt`

The GraphQL schema exposes the renewal as a timestamp, not a ledger delta.
`schema/schema.graphql` defines `ttlExtendedAt` on the stream type; populate it
from the event's ledger close time:

```text
ttlExtendedAt = close_time(event.ledger)
```

Do not derive `ttlExtendedAt` from `extended_to_ledgers`; the delta is a ledger
count and the field is a wall-clock timestamp. If a stream is renewed more than
once, `ttlExtendedAt` holds the most recent renewal, so apply events in
`(ledger, txIndex, eventIndex)` order and let the last write win.

### Reconciling a keeper sweep summary

`script/keeper-extend-ttl.sh` prints a sweep summary (streams attempted,
streams renewed, ledgers added). Reconcile it against the event stream before
trusting either source:

1. Count `ttl_extended` events in the sweep's ledger range and compare with the
   summary's renewed count. A mismatch means a page was dropped or the summary
   was truncated.
2. Sum `extended_to_ledgers` across those events and compare with the summary's
   total ledgers added. A mismatch means a payload was decoded with the wrong
   integer width.
3. For each event, confirm `stream_exists(topic[1])` is still true. A renewed
   stream that has since been cancelled or fully withdrawn is expected to be
   absent; a stream that vanished without a terminal event is a gap.

### Detecting archived entries

A stream whose TTL lapsed is archived rather than deleted, so it disappears
from `getLedgerEntries` while its ID remains below the current counter. Detect
it with the pair of read-only calls:

```text
stream_exists(id) == false  &&  id < stream_count()
```

When both hold, the stream is archived and the projection should surface a
restore action instead of dropping the row. Mark the record `archived`, keep
the last known state, and emit a `restore` task so an operator can submit the
restore transaction. Do not treat the missing entry as a deletion: the ID is
still allocated and will be reused by no other stream.

Reads that race a ledger close can observe a stale `stream_count()` or a
missing entry that is about to appear. Apply the skew rules in
[soroban-rpc-read-skew.md](soroban-rpc-read-skew.md) before concluding that a
stream is archived.

## Operational recommendations

1. Backfill from a known ledger before the contract deployment ledger, then
   advance in bounded ranges until the latest closed ledger.
2. Store the contract ID, ledger, transaction hash, event index, raw topics,
   raw value, and decoded event version. Raw data makes re-decoding possible
   after an ABI decoder fix.
3. Build the stream projection from `stream_created` and apply subsequent
   events in ledger/transaction/event order. Periodically reconcile important
   records with `getLedgerEntries` because events describe transitions while
   storage describes current state.
4. Keep Horizon ingestion separate. Join its payment/account records to RPC
   events by transaction hash when a user-facing view needs both contract
   semantics and token/account activity.
5. Treat an RPC provider's retention and rate limits as deployment concerns.
   For a durable historical index, archive raw pages or use a provider that
   guarantees access to the ledger range you need.
   guarantees access to the ledger range you need.

## Deterministic event reconstruction rules

This section is normative for any indexer that must produce byte-identical
projections from the same ledger history. The frozen interface of record is
[ABI.md](ABI.md); the golden fixtures under
[`tests/fixtures/event_snapshots/`](../tests/fixtures/event_snapshots/) are the
executable form of these rules. When this guide, the ABI, and the fixtures
disagree, the fixtures and ABI win and this document must be corrected.

### Topic layout

Every Perpetua event has a fixed topic layout. `topic[0]` is always the event
name encoded as a Soroban `symbol` in snake case. `topic[1]` is always the
`stream_id` encoded as `u64`. Any remaining slots are the routing parties for
that event, in the order listed below. Slots that are not listed are absent —
they are not present-but-empty.

| event | topic[0] | topic[1] | topic[2] | topic[3] | topic count |
|---|---|---|---|---|---|
| `stream_created` | `symbol("stream_created")` | `u64 stream_id` | `address sender` | `address recipient` | 4 |
| `withdrawn` | `symbol("withdrawn")` | `u64 stream_id` | `address recipient` | — | 3 |
| `cancelled` | `symbol("cancelled")` | `u64 stream_id` | `address sender` | `address recipient` | 4 |
| `paused` | `symbol("paused")` | `u64 stream_id` | `address sender` | — | 3 |
| `resumed` | `symbol("resumed")` | `u64 stream_id` | `address sender` | — | 3 |
| `topped_up` | `symbol("topped_up")` | `u64 stream_id` | `address sender` | — | 3 |
| `recipient_transferred` | `symbol("recipient_transferred")` | `u64 stream_id` | `address old_recipient` | `address new_recipient` | 4 |
| `delegate_granted` | `symbol("delegate_granted")` | `u64 stream_id` | `address grantor` | `address delegate` | 4 |
| `delegate_revoked` | `symbol("delegate_revoked")` | `u64 stream_id` | `address grantor` | `address delegate` | 4 |
| `ttl_extended` | `symbol("ttl_extended")` | `u64 stream_id` | — | — | 2 |

Rules that follow from the table:

- Never assume a constant topic arity. Read `topics.length` per event and
  dispatch on `topic[0]` first, then validate the arity for that event name.
- An event whose `topic[0]` is not in the table is unknown. Persist it raw and
  skip projection; do not guess a layout.
- An event whose arity does not match the table is malformed. Fail the page,
  do not advance the checkpoint, and surface the raw event for investigation.
- Addresses are `ScVal` addresses, not strings. Decode them with the generated
  SDK; never compare base64 topic bytes against hand-written literals.

### Payload schema

The data payload (`event.value`) is a single `ScVal` map. Field names are
snake case and match the ABI. All amounts are `i128` token base units.

| event | payload fields |
|---|---|
| `stream_created` | `stream_id`, `sender`, `recipient`, `deposit_amount`, `rate_per_ledger`, `start_ledger`, `cliff_ledger`, `end_ledger`, `withdrawable`, `status` |
| `withdrawn` | `stream_id`, `recipient`, `amount`, `total_withdrawn`, `withdrawable`, `status` |
| `cancelled` | `stream_id`, `sender`, `recipient`, `refund_amount`, `total_withdrawn`, `status` |
| `paused` | `stream_id`, `sender`, `paused_at_ledger`, `accrued_at_pause`, `status` |
| `resumed` | `stream_id`, `sender`, `resumed_at_ledger`, `accrued_at_resume`, `status` |
| `topped_up` | `stream_id`, `sender`, `amount`, `deposit_amount`, `end_ledger`, `status` |
| `recipient_transferred` | `stream_id`, `old_recipient`, `new_recipient`, `status` |
| `delegate_granted` | `stream_id`, `grantor`, `delegate`, `permissions`, `status` |
| `delegate_revoked` | `stream_id`, `grantor`, `delegate`, `status` |
| `ttl_extended` | `stream_id`, `extended_to_ledger` |

Payload rules:

- Decode the map by field name, not by position. A future additive field must
  not shift the meaning of an existing one.
- `status` is the stream status after the state change, encoded as the ABI
  enum. It is authoritative for the transition described by the event.
- `withdrawable` is the amount claimable at the event's ledger, computed by the
  contract. Do not recompute it from `rate_per_ledger` when the contract
  supplies it; recomputation is only for reconciliation checks.
- Amounts must be handled as integers or decimal strings. Never route an
  `i128` through an IEEE-754 `Number`.

### Ordering guarantees

The contract emits exactly one event per state change, and events are ordered
by the ledger that closed them:

1. Ledgers are totally ordered by `ledger` (ascending).
2. Within a ledger, transactions are ordered by `txIndex` (ascending).
3. Within a transaction, events are ordered by `eventIndex` (ascending).

The tuple `(ledger, txIndex, eventIndex)` is the canonical total order. Apply
events in exactly that order. `(txHash, eventIndex)` is the deduplication key;
`ledger` and `txIndex` are retained for audit and ordering. Because there is
one event per state change, folding the ordered event stream reproduces the
contract's state transitions without replaying storage writes.

### Folding events into the `Stream` mirror

Start from the `stream_created` event, which carries the complete initial
state, then apply each subsequent event for that `stream_id` in canonical
order. The mirror is a pure function of the ordered event stream.

- `stream_created`: insert a new `Stream` row from the payload. Set
  `deposit_amount`, `rate_per_ledger`, `start_ledger`, `cliff_ledger`,
  `end_ledger`, `withdrawable`, and `status`. Record `sender` and `recipient`
  from the payload and cross-check them against `topic[2]`/`topic[3]`.
- `withdrawn`: add `amount` to `total_withdrawn`, set `withdrawable` to the
  payload value, and set `status` from the payload. Do not recompute
  `total_withdrawn` by summing `amount` across events unless reconciling; the
  payload's cumulative field is authoritative.
- `topped_up`: add `amount` to `deposit_amount`, adopt the payload's
  `end_ledger`, and set `status`. A top-up can extend the stream, so
  `end_ledger` must be overwritten, not max-merged blindly.
- `paused`: set `status` to paused, store `paused_at_ledger`, and store
  `accrued_at_pause`. Accrual stops at `paused_at_ledger`; the mirror must not
  accrue withdrawable between `paused_at_ledger` and the resume ledger.
- `resumed`: set `status` to active, store `resumed_at_ledger` and
  `accrued_at_resume`, and shift the accrual window so that accrual restarts at
  `resumed_at_ledger`. The elapsed paused interval is excluded from accrual;
  `accrued_at_resume` is the contract's authoritative accrued amount at resume.
- `cancelled`: set `status` to cancelled, record `refund_amount` and
  `total_withdrawn` from the payload. A cancelled stream is terminal: no
  further `withdrawn`, `paused`, `resumed`, or `topped_up` events may be
  applied to it. If one arrives, treat it as a schema violation and halt.
- `recipient_transferred`: replace `recipient` with `new_recipient`. The
  `old_recipient` field and `topic[2]` must match the current mirror value;
  a mismatch is a reconstruction error.
- `delegate_granted` / `delegate_revoked`: upsert or delete the delegate
  permission row keyed by `(stream_id, grantor, delegate)`. These events do not
  change the `Stream` row itself.
- `ttl_extended`: record `extended_to_ledger` for operational visibility. It
  does not change stream accounting or status.

Status transitions are therefore: created → active; active → paused → active;
active → cancelled; active → completed when `withdrawable` reaches zero at or
after `end_ledger`. Terminal states (`cancelled`, `completed`) never transition
again.

### Decoding with the generated SDK

Decode topics and payloads with the SDK generated from
[`contracts/stream/abi/fluxora_stream.json`](../contracts/stream/abi/fluxora_stream.json),
not with hand-rolled XDR parsing:

```js
import { scValToNative, xdr } from "@stellar/stellar-sdk";

function decodeEvent(event) {
  const topics = event.topic.map((t) =>
    scValToNative(xdr.ScVal.fromXDR(t, "base64")),
  );
  const value = scValToNative(xdr.ScVal.fromXDR(event.value, "base64"));
  return { name: topics[0], streamId: topics[1], parties: topics.slice(2), value };
}
```

The generated client exposes the same decoders used by the contract tests, so
the SDK, the Rust contracts, and the GraphQL/SQL projections stay in lockstep.
Any change to a topic position or payload field is a breaking ABI change and
must be accompanied by updated fixtures.

### Validating parity with the snapshot fixtures

The fixtures under
[`tests/fixtures/event_snapshots/`](../tests/fixtures/event_snapshots/) are
golden JSON snapshots of decoded events and the resulting `Stream` mirror.
Indexers should run the same checks in CI:

- `script/verify_event_bytes.py` asserts that each fixture's encoded event
  stays within the 16 KiB Soroban event byte budget and that the topic/payload
  layout matches the ABI.
- `script/check_snapshot_diff.py` re-folds the fixture event stream and diffs
  the resulting `Stream` mirror against the golden snapshot, failing on any
  drift.

A projection is in parity only when both scripts pass. Treat a snapshot diff
as a schema change: update the ABI, the fixtures, and this guide together, and
never regenerate a fixture to hide an unexplained diff.

## Stream types and bitmask reconstruction

Golden fixture: `tests/fixtures/event_snapshots/stream_types.json`, asserted by `test::types_schema` against `contracts/stream/src/types.rs`.

- `Stream.flags` (`u32`) is the immutable packed bitmask `CANCELLABLE=1`, `PAUSABLE=2`, `TRANSFERABLE=4`. Indexers derive `cancellable = flags & 1 != 0`, `pausable = flags & 2 != 0`, `transferable = flags & 4 != 0`. The `stream_created` event still carries the three booleans; both views must agree.
- `DelegateGrant.ops` is an OR of `WITHDRAW=1, CANCEL=2, PAUSE=4, RESUME=8, TOP_UP=16, TRANSFER_RECIPIENT=32`. Unknown higher bits must be preserved, not rejected.
- `StreamStatus`: `Active=0, Paused=1, Cancelled=2, Depleted=3`; `Cancelled` and `Depleted` are terminal and `Cancelled` is sticky.
- Storage key discriminants: `NextStreamId=0, StreamCount=1, Stream(u64)=2, Delegate(u64, Address)=3`.

## Event reconstruction rules (storage parity)

* `DataKey::Stream(id)` state is reconstructed by folding events in ledger order:
  `stream_created` seeds the record; `withdrawn` sets `withdrawn`; `topped_up`
  raises `deposited`; `cancelled` sets `deposited = vested`; `paused`/`resumed`
  set `paused_at`/`paused_total`.
* A missing entry whose id is below the issued-id count is *archived*, not
  deleted: restore the footprint before reconciling.
* Topic layout and field order are frozen by `docs/ABI.md`; fixtures live in
  `tests/fixtures/event_snapshots/` and are gated by `script/check_snapshot_diff.py`.

## Error code parity (`tools/sdk/errors.ts`)

Indexers decode `Error(Contract, #N)` with `decodeContractError`. The table is
frozen against `contracts/stream/src/error.rs` and the golden fixture
`tests/fixtures/errors/stream_errors.json`; adding a discriminant requires
updating the Rust enum, `errors.ts`, the fixture, and `docs/ABI.md` together.

### CLI and `useStream` schema parity

`tools/perpetua-cli.ts` (`CLI_EVENT_TOPIC_LAYOUTS`, `CLI_EVENT_PAYLOAD_SCHEMAS`)
and `sdk/react-hooks/src/useStream.ts` consume the `Stream` mirror and the
`withdrawn` event exactly as frozen in `docs/ABI.md` and
`tests/fixtures/event_snapshots/withdrawn.json`. To reconstruct state, fold
events in ledger order by `stream_id` (topic 1); a client must never apply a
read from a ledger older than one already applied.

### Restore footprint and wallet SDK parity (`restore_footprint.ts`, `wallet.ts`)

* The restore key is `ScVec[Symbol("Stream"), U64(stream_id)]`, persistent
  durability (`DataKey::Stream`, see `docs/ABI.md`). `stream_id` outside
  `[0, 2^64-1]` is rejected. Archived means
  `stream_exists == false && stream_id < stream_count`.
* `wallet.ts` encodes `create_stream` as `deposit:i128`, times as `u64`
  (never `parseInt`/float). Rejections are deterministic `WalletParamError`
  codes: `InvalidDeposit`, `InvalidStartTime`, `InvalidEndTime`,
  `InvalidCliffTime`, `InvalidTimeRange`, `InvalidCliff`, `InvalidStreamId`,
  `InvalidAmount`.
* Reconstruct by folding `stream_created`/`withdrawn`/... events by
  `stream_id` in ledger order. Golden fixtures:
  `restore_footprint_key.json`, `wallet_create_stream.json`.
