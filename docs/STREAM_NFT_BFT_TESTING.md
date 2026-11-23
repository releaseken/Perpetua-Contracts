# Stream-NFT Byzantine Fault Tolerance & Partition Resilience Testing

## Overview
This document outlines the Byzantine fault tolerance (BFT) and simulated network partition test scenarios implemented for `contracts/stream-nft` in accordance with Soroban SDK best practices, ledger footprint optimization, and formal verification standards (#369).

## Threat Model & Partition Scenarios

In Soroban's single-threaded, host-metered transactional model, distributed consensus partitions and Byzantine RPC faults manifest as:
1. **Network Partition Timeouts & Replayed Invocations**:
   - Clients send `transfer` or `mint` transactions across an RPC partition that fails to acknowledge receipt before timeout.
   - Retried invocations must be deterministic and reject stale requests without corrupting state or balance counters.
2. **Atomic Rollback on Cross-Contract Failures**:
   - `transfer` calls the stream contract's `transfer_recipient` entrypoint.
   - If the downstream contract rejects the call (e.g. unfulfilled authorization, paused stream, depleted claim, or partition failure), the entire invocation rolls back atomically, leaving owner mappings and user token balances unaltered.
3. **Competing Partition Minting Race**:
   - Multiple partitioned actors attempting to mint an NFT for the same `stream_id`.
   - The first write commits; subsequent attempts immediately fail with `AlreadyMinted`, preventing duplicate tokens or supply drift.
4. **Checked Arithmetic & Defensive Error Handling**:
   - All balance and supply updates utilize checked arithmetic (`checked_add`, `checked_sub`) mapping overflows to `Error::Overflow`.
   - Idempotent self-transfers bypass balance decrements/increments to minimize storage footprint and avoid ledger churn.
5. **Ledger Footprint & TTL Optimization**:
   - All persistent entries (`Owner`, `Balance`) and instance entries (`Config`, `Supply`) are extended (`bump`) upon access.
   - Zero-balance entries are pruned from persistent storage to minimize host footprint.

## Test Matrix (`contracts/stream-nft/src/test/byzantine_fault_tolerance.rs`)

| Test Scenario | Fault / Partition Condition | Expected Behavior |
|---|---|---|
| `test_partition_lost_response_transfer_retry_is_safely_rejected` | Partition disconnects after transfer; client retries previous owner's transfer | Replay rejected; new owner, balances, and supply unchanged |
| `test_stream_contract_rejection_reverts_nft_state_atomically` | Downstream stream contract reverts during `transfer_recipient` | Transaction reverts atomically; NFT owner and balances remain pristine |
| `test_byzantine_competing_partition_mint_race` | Multiple nodes race to mint the same `stream_id` | Only first succeeds; second receives `AlreadyMinted`; supply invariant holds |
| `test_byzantine_replayed_transfer_after_subsequent_transfer` | Adversary attempts to replay stale transfer after subsequent handoff | Replay rejected; latest owner and balance distribution preserved |
| `test_self_transfer_partition_recovery` | Self-transfer attempted during network partition recovery | Idempotent no-op; balances unchanged |
| `test_uninitialized_calls_rejected` | Invocation on uninitialized wrapper instance | Deterministic failure or None read; no corrupt state |
