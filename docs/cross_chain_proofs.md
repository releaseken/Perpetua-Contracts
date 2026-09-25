# Cross-Chain Stream State Proofs

## Status and objective

This document specifies how an Ethereum contract, another Soroban contract, or
an off-chain relayer can verify the state of
`DataKey::Stream(stream_id)` without bridging the stream's token. The proof
attests to a stream record committed by a Stellar ledger; it does not transfer
funds or make the destination chain a custodian of those funds.

The design target is a proof of this statement:

> At Stellar ledger `L`, on network `N`, contract `C` had persistent contract
> data key `DataKey::Stream(id)` with canonical XDR value `V`, and `V` decodes
> to the claimed Perpetua stream record.

A verifier must additionally decide whether that record is fresh enough and
whether the claimed stream state is sufficient for the destination-chain
application. A proof of an old valid stream is still an old proof.

This specification is deliberately independent of a particular bridge token.
A destination application may use the attested balance as collateral data,
credit limit input, or an authorization condition while leaving the underlying
stream and token on Stellar.

## Existing Perpetua storage contract

The stream contract stores one persistent entry per stream:

```text
DataKey::Stream(id)
  -> Stream {
       sender: Address,
       recipient: Address,
       token: Address,
       deposited: i128,
       withdrawn: i128,
       start_time: u64,
       end_time: u64,
       cliff_time: u64,
       flags: u8,              // cancellable, pausable, transferable
       paused_at: Option<u64>,
       paused_total: u64,
       status: StreamStatus,
     }
```

The deployed key encoding is part of the data ABI. The repository's storage
key tests pin the following Soroban `ScVal` encoding:

```text
ScVal::Vec([
  ScVal::Symbol("Stream"),
  ScVal::U64(id),
])
```

For example, `Stream(1)` is the 40-byte XDR value:

```text
0000001000000001000000020000000f0000000653747265616d0000000000050000000000000001
```

Do not construct this key by concatenating text or by hashing the stream ID.
Use the generated Soroban SDK/XDR encoder, and pin the deployed WASM hash and
interface version. A redeployment that changes the `DataKey` encoding is a new
proof domain and requires a migration specification.

The ledger key used by Stellar core is not just this `ScVal`. It is a
`LedgerKey::ContractData` containing:

```text
LedgerKey::ContractData {
  contract: ContractId(C),
  key: ScVal::Vec([Symbol("Stream"), U64(id)]),
  durability: Persistent,
}
```

The contract ID, key, and durability all bind the proof. A proof for the same
ScVal under another contract, or for a temporary entry, is not a proof of this
stream.

## What Stellar ledger hashes prove

A Stellar ledger header contains a `bucketListHash`. That hash commits to the
state represented by the BucketList at that ledger. The state commitment is
part of a consensus-authenticated header chain; it is not derived from the
Soroban RPC response.

For a state proof, the verifier needs two logically separate proofs:

1. **Header finality:** the supplied header and its ancestry are accepted by a
   trusted Stellar light client, bridge committee, or another explicitly chosen
   finality mechanism. The verifier checks the network passphrase/network ID,
   ledger sequence, previous-hash linkage, and the Stellar consensus signature
   or quorum certificate appropriate to the deployment.
2. **State inclusion:** a BucketList proof shows that the canonical
   `LedgerKey::ContractData` is included in the state committed by that
   header's `bucketListHash`. The proof contains the relevant bucket records,
   bucket levels, sibling hashes, and any protocol-version metadata needed to
   reconstruct the root.

The `bucketListHash` by itself is only a commitment. A response from
`getLedgerEntries` or another Soroban RPC read usually supplies an entry and a
ledger number, but it is not by itself a cryptographic proof that the entry was
included in the header's state root. RPC `latestLedger` is also a routing and
freshness hint, not finality. A production relayer must obtain a proof from a
Stellar archive/Core proof service or run the proof-producing state machinery;
adding a JSON field containing a ledger hash does not make an RPC response
verifiable.

The exact BucketList path format is protocol-version-sensitive. A verifier
must bind the proof parser to the Stellar protocol version and reject unknown
or unsupported proof versions. It must not substitute a generic binary Merkle
proof unless that proof is demonstrably the proof of the BucketList structure
used by the target protocol.

## Proof envelope

A relayer submits an envelope equivalent to the following. The wire encoding
should be canonical CBOR, SCALE, ABI words, or another format selected by the
destination chain, but the signed/hashed fields must have one unambiguous
encoding.

```text
StreamStateProof {
  network_id: bytes32,             // hash of the Stellar network identity
  protocol_version: uint32,
  ledger_sequence: uint32,
  ledger_header_xdr: bytes,
  finality_certificate: bytes,
  contract_id: bytes32,
  stream_id: uint64,
  durability: Persistent,
  storage_key_xdr: bytes,           // ScVal([Symbol("Stream"), U64(id)])
  storage_value_xdr: bytes,         // Stream value, canonical XDR
  bucket_list_proof: bytes,
  generated_at: uint64,
  expires_at: uint64,
}
```

The verifier should recompute, rather than trust, `storage_key_xdr` from
`contract_id`, `stream_id`, and `Persistent`. It should also recompute the
stream value digest from the decoded canonical XDR. `generated_at` and
`expires_at` are policy metadata and do not replace checking the ledger
sequence or close time.

For a destination chain with limited calldata, the relayer may submit the
header and state proof once and expose only a digest to later calls. The digest
must commit to the network, contract, stream ID, ledger sequence, key, value,
and proof domain, not merely to the decoded balance.

## Verification algorithm

The following is pseudocode for a destination verifier. `verify_stellar_header`
is a deployment-specific light-client or quorum-certificate check, and
`verify_bucket_list_proof` is a protocol-version-specific BucketList verifier.

```text
verifyStreamState(proof, expectedNetwork, expectedContract, now):
    assert proof.network_id == expectedNetwork
    assert proof.contract_id == expectedContract
    assert proof.durability == Persistent
    assert proof.protocol_version in supportedProtocolVersions
    assert proof.expires_at >= now

    header = decodeLedgerHeader(proof.ledger_header_xdr)
    assert header.sequence == proof.ledger_sequence
    assert verify_stellar_header(
        header,
        proof.finality_certificate,
        expectedNetwork,
    )

    key = ContractDataKey(
        contract = proof.contract_id,
        key = ScValVec(Symbol("Stream"), U64(proof.stream_id)),
        durability = Persistent,
    )
    assert proof.storage_key_xdr == canonicalXdr(key.key)

    assert verify_bucket_list_proof(
        proof.bucket_list_proof,
        key,
        proof.storage_value_xdr,
        header.bucketListHash,
        proof.protocol_version,
    )

    stream = decodePerpetuaStream(canonicalXdr(proof.storage_value_xdr))
    assert stream.token is a valid contract address
    assert stream.deposited > 0
    assert 0 <= stream.withdrawn <= stream.deposited
    assert stream.start_time < stream.end_time
    assert stream.start_time <= stream.cliff_time <= stream.end_time
    assert stream.status in {Active, Paused, Cancelled, Depleted}

    return VerifiedStream(
        network = expectedNetwork,
        contract = expectedContract,
        id = proof.stream_id,
        ledger = proof.ledger_sequence,
        value = stream,
    )
```

The final invariant checks are useful corruption guards, but they do not prove
that the contract itself produced the value. The state inclusion proof and the
pinned contract code/interface establish that binding. If the destination
needs a balance at time `t`, it must compute the Perpetua accrual formula from
the same ledger timestamp policy and stream state, or require a value produced
by a trusted Perpetua view implementation. It must not combine fields fetched
from different ledgers.

## Demonstration pattern

### 1. Produce a canonical state read

A relayer first chooses a finalized ledger `L`, not simply the RPC's current
height. It obtains the `ContractData` ledger entry for:

```text
contract = <deployed Perpetua contract C>
key      = ScVal::Vec([Symbol("Stream"), U64(42)])
type     = Persistent
```

It records the returned value XDR and `lastModifiedLedgerSeq`, but treats those
RPC fields as candidate data only. The relayer then obtains a BucketList
inclusion proof for the same key and the header for ledger `L`.

The relayer checks locally:

```text
proof.key == LedgerKey::ContractData(C, Stream(42), Persistent)
proof.value == RPC entry value XDR
proof.root == header.bucketListHash
proof.header is finalized for the expected Stellar network
```

Only after those checks does it submit the envelope to the destination.

### 2. Verify a recipient's claimable amount

For a stream with value `S`, a destination can verify a claim at a chosen
ledger timestamp `t` as follows:

```text
stream_time = (S.paused_at or t) - S.paused_total
elapsed     = clamp(stream_time, S.start_time, S.end_time) - S.start_time
vested      = 0                                  if stream_time < S.cliff_time
              S.deposited * elapsed / duration   otherwise
claimable   = max(vested - S.withdrawn, 0)
```

The destination must use checked arithmetic, the same truncation direction,
and the same paused/cliff/status semantics as the deployed contract. The
proof says what the stream record was at `L`; it does not prove that the
recipient has not withdrawn again after `L`. A consuming application should
therefore require a freshness window, a nonce/sequence policy, or a new proof
for every state-changing action.

Example policy:

```text
assert proof.ledger_sequence >= trustedStellarSequence - MAX_LEDGER_AGE
assert S.token == expectedToken
assert S.recipient == expectedRecipient
assert claimable >= requestedCredit
assert hash(proof) has not been consumed before
```

A crediting application must choose whether the credit is based on the
conservative value at `L`, the claimable amount at `L`, or a fixed field such as
`deposited - withdrawn`. Those are different risk policies. It must never call
`deposited` a currently withdrawable balance.

### 3. Ethereum integration shape

A Solidity verifier can follow this boundary:

```text
submitStreamProof(proof, recipient, requestedAmount):
    verified = stellarLightClient.verifyHeaderAndFinality(
        proof.ledgerHeader,
        proof.finalityCertificate,
        proof.networkId,
    )
    require(verified)

    stream = bucketListVerifier.verifyContractData(
        proof.bucketListProof,
        proof.ledgerHeader.bucketListHash,
        proof.contractId,
        proof.streamId,
        Persistent,
    )
    require(hash(stream.xdr) == hash(proof.storageValueXdr))

    decoded = decodePerpetuaStream(stream.xdr)
    require(decoded.recipient == recipient)
    require(decoded.token == configuredToken)
    require(decoded.ledgerTimestamp + maxAge >= block.timestamp)
    require(!usedProof[proofDigest])
    usedProof[proofDigest] = true

    credit(recipient, amountAllowedByPolicy(decoded, requestedAmount))
```

`stellarLightClient` and `bucketListVerifier` are intentionally separate
components. A bridge that verifies only a multisig attestation is using a
committee trust model, not a native Stellar Merkle proof. That can be a valid
engineering choice, but the trust assumption must be disclosed and the
attestation must bind exactly the same fields as the native proof envelope.

## Inclusion, absence, and historical-state cases

### Inclusion proof

An inclusion proof is enough when the stream entry exists at ledger `L`. The
value XDR must be decoded under the deployed contract's exact ABI version.
Unknown fields, changed enum discriminants, or a different contract ID must
cause rejection.

### Non-inclusion proof

A missing stream is not proven by an empty RPC response. To prove absence, the
BucketList proof format must support an authenticated absence/range proof for
the canonical ledger key. If the available proof service cannot produce one,
the verifier must treat "not returned" as unknown, not as `StreamNotFound`.

### Archived entries

Persistent entries can be archived and become unreadable until restored. A
proof of the state before archival is still valid for that historical ledger,
but it says nothing about the current ledger. A proof service must document
whether it can prove archived historical entries and how it identifies their
last live ledger. A destination requiring current state should reject a proof
whose ledger is outside its freshness policy.

### Contract redeployment

The contract is immutable in the current deployment. A new address, WASM hash,
ABI version, or storage-key encoding is a new trust domain. The destination
must maintain an allowlist with at least:

```text
network_id -> contract_id -> wasm_hash -> ABI/storage-key version
```

Never accept a proof solely because it contains a familiar stream ID.

## Security and operational requirements

- Pin the Stellar network identity and contract ID; do not accept a network or
  contract supplied by an untrusted relayer.
- Verify finality before state inclusion. A merely observed latest ledger can
  be replaced or can be inconsistent with another RPC response.
- Bind the proof to `Persistent`, the exact `ScVal` key, protocol version, and
  the header's `bucketListHash`.
- Use canonical XDR and reject alternate encodings of the same logical value.
- Use a freshness bound and replay protection on the destination chain.
- Keep the proof verifier deterministic and bounded. Reject oversized paths,
  unsupported bucket versions, excessive sibling counts, and malformed XDR.
- Treat relayers as untrusted transport. A relayer may delay or omit a proof,
  but must not be able to change a verified value.
- Monitor header lag, proof-generation lag, invalid-proof rate, and contract
  code/storage-version changes.
- Do not infer token ownership or transfer finality from stream state alone.
  The proof attests to a contract record; token balances and any destination
  credit policy are separate claims.

## Test and rollout plan

1. Freeze the storage-key XDR and contract/ABI allowlist for the deployment.
2. Build a fixture from a real or local Stellar ledger containing a known
   `DataKey::Stream(id)` entry, its header, and its BucketList proof.
3. Verify the fixture in a reference implementation against the exact
   `bucketListHash` and decoded `Stream` value.
4. Mutate one field, one sibling hash, the contract ID, durability, ledger
   sequence, and network ID; each mutation must fail.
5. Test a stale-but-valid proof, an archived entry, an absent key, duplicate
   proof submission, and a protocol-version mismatch.
6. Cross-check values from one ledger only; do not combine separate RPC view
   calls. This is especially important for accrued balances because the
   repository documents load-balanced RPC read skew.
7. Audit the light client/finality component and the BucketList verifier
   independently from the relayer.
8. Start with an off-chain verifier and signed attestations for integration
   testing. Move to native on-chain verification only after proof size, gas,
   protocol upgrades, and failure behavior are measured.

## Open implementation decisions

The following choices must be settled before committing a production proof
format:

- Which Stellar finality model the destination trusts: native light client,
  bridge committee, or a hybrid.
- Where BucketList proofs are generated and how historical/archived entries are
  served.
- The supported Stellar protocol versions and upgrade procedure.
- The canonical proof serialization and destination-chain size limit.
- Whether a proof attests raw stream state only or also a derived claimable
  amount, and which timestamp is authoritative for that derivation.
- Freshness, replay, and reorg policy for each consuming application.

Until those decisions are implemented and audited, `getLedgerEntries` output
may be used for monitoring and relayer input, but it must not be described as a
cryptographic cross-chain state proof.
