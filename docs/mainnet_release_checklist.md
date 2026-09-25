# Perpetua Mainnet Release Checklist

This checklist is the final launch gate for Perpetua Mainnet. Every item is
blocking: do not deploy or announce the mainnet release while any checkbox is
unchecked or its evidence is missing.

## Release record

| Field | Value |
|---|---|
| Release version / tag | |
| Git commit | |
| Planned mainnet deployment window (UTC) | |
| Release owner | |
| Security/audit approver | |
| Operations approver | |
| Evidence bundle location | |

## Stage 4: deployed-contract and network verification

### 1. Live testnet archival canary

- [ ] The live archival canary has reached `ARCHIVED` state on Stellar Testnet.
- [ ] A read against the archived persistent entry failed as expected.
- [ ] Invocation against the archived entry failed as expected rather than
      returning stale data.
- [ ] `RestoreFootprint` restored the entry and the original value was read
      back successfully.
- [ ] The test output, network, probe contract ID, ledgers, and UTC timestamp
      are attached to the evidence bundle.

Run the complete round trip with:

```bash
NETWORK=testnet script/archival-canary.sh --restore
```

The canary is complete only when the command exits successfully after the
restore and data-integrity checks. A status-only run is not sufficient.

### 2. Security audit sign-off

- [ ] The final security audit report is attached to the release evidence.
- [ ] All critical and high-severity findings are closed, accepted by the
      security approver, or explicitly documented as a release blocker.
- [ ] The audit covers the deployed contract revision and generated ABI.
- [ ] The audit approver has recorded name, date, and approval in the release
      record below.

Reference implementation security material:

- [Security overview](SECURITY.md)
- [Audit entrypoint table](audit.md)
- [Threat model](threat_model.md)

### 3. Full test and property sweep

- [ ] All workspace contract tests pass, including the approximately 700
      unit, integration, and property tests.
- [ ] The release property sweep completes with `PROPTEST_CASES=5000`.
- [ ] The randomized sweep uses the release toolchain and the final commit.
- [ ] Test output and the exact command are attached to the evidence bundle.
- [ ] No unexplained warnings, flaky tests, or ignored failures remain.

Minimum release commands:

```bash
cargo test --workspace
FLUXORA_FUZZ_SEEDS=500 FLUXORA_FUZZ_STEPS=1000 PROPTEST_CASES=5000 \
  bash -lc 'cd contracts/stream && cargo test --release'
```

If the shell environment does not preserve the variables across the command,
run the equivalent commands from the repository root and record the output.

## Stage 5: release integrity and operations

### 4. Wasm provenance build and verification

- [ ] Release Wasm was produced by the repository release process from the
      recorded commit.
- [ ] A provenance manifest and SHA-256 checksums exist for every release Wasm.
- [ ] Provenance verification passes with no integrity, completeness, or
      environment-consistency errors.
- [ ] The verified Wasm digests match the artifacts selected for deployment.
- [ ] `provenance.json`, `SHASUMS`, command output, and toolchain details are
      attached to the evidence bundle.

Run the release integrity gate with:

```bash
script/provenance.sh build
script/provenance.sh verify
```

See [Wasm provenance](provenance.md) for the manifest contract and failure
conditions.

### 5. Mainnet keeper deployment

- [ ] The keeper service is deployed on approved mainnet infrastructure.
- [ ] Mainnet RPC, contract ID, keeper identity, and funding configuration are
      verified without exposing secrets in the evidence bundle.
- [ ] A one-shot keeper dry run or controlled run completed successfully.
- [ ] The keeper can extend stream TTLs, handles archived/nonexistent IDs as
      designed, and emits observable logs and alerts.
- [ ] Monitoring, restart policy, funding thresholds, and on-call ownership
      are documented.

Operational command reference:

```bash
CONTRACT_ID=<mainnet-contract-id> SOURCE=<keeper-account> \
  script/keeper-extend-ttl.sh --once
```

Record the deployment identifier, infrastructure environment, commit/image,
last successful run, and monitoring link in the evidence bundle.

### 6. Off-chain indexer synchronization

- [ ] The indexer is configured for the final mainnet contract ID and network.
- [ ] Synchronization has reached the agreed mainnet ledger checkpoint.
- [ ] The indexer projection agrees with on-chain stream state for the sampled
      create, withdraw, pause/resume, cancel, transfer, top-up, and TTL paths.
- [ ] Replay, deduplication, fork/reorg handling, and restart recovery have
      been verified.
- [ ] Indexer lag, ingestion errors, and projection mismatches are monitored,
      with an on-call owner assigned.
- [ ] The checkpoint, validation queries, sample transaction/event IDs, and
      verification output are attached to the evidence bundle.

The indexer must generate decoders from the deployed contract interface; do not
hand-roll topic parsing. See [ABI](ABI.md) and the indexer migration notes in
[MIGRATION.md](MIGRATION.md).

## Final launch sign-off

### 7. Release decision

- [ ] All six technical gates above are checked and their evidence is present.
- [ ] The release commit, Wasm digests, deployed contract ID, ABI, and network
      configuration are mutually consistent.
- [ ] Rollback, incident response, and post-deploy monitoring contacts are
      documented before the deployment window begins.
- [ ] Security/audit approver has signed off.
- [ ] Operations approver has signed off.
- [ ] Release owner has recorded the final GO decision.

| Role | Name | Decision | UTC date/time | Signature or approval reference |
|---|---|---|---|---|
| Release owner | | GO / NO-GO | | |
| Security/audit approver | | GO / NO-GO | | |
| Operations approver | | GO / NO-GO | | |

A mainnet deployment is authorized only when every decision is `GO` and every
blocking checkbox is checked. After deployment, append the mainnet contract ID,
transaction hashes, deployment ledger, and first monitoring checkpoint to the
release evidence bundle.
