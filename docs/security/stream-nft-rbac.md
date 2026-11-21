# Granular Role-Based Authorization in Stream-NFT Subsystem

## Overview
This document specifies the security architecture, granular role-based access control (RBAC), and administrative management mechanisms implemented in `contracts/stream-nft` under Issue #367.

## Role Separation & Authority Matrix

The `StreamNft` contract separates operational duties across three dedicated roles to adhere to the principle of least privilege and prevent unauthorized configuration updates:

| Role | Holder Authority & Responsibilities | Entry Points Protected |
|---|---|---|
| **`admin`** | Manages governance, role reassignments, and administrative hand-off. | `set_admin`, `propose_admin`, `set_metadata_admin`, `set_manager` |
| **`metadata_admin`** | Configures token collection descriptors (`name`, `symbol`). Cannot alter contract addresses or minting state. | `set_metadata` |
| **`manager`** | Operational controller: updates target stream instance address and controls emergency minting pauses. | `set_stream_contract`, `set_mint_paused` |
| **Pending Admin** | Successor nominated during two-step admin rotation. | `accept_admin` |

## Two-Step Admin Rotation Protocol

To prevent irreversible lockout or accidental transfer to unintended addresses:
1. **Nomination (`propose_admin`)**: The active `admin` nominates a proposed successor. The proposal is stored in instance storage under `DataKey::PendingAdmin` and emits `AdminProposedEvent`.
2. **Acceptance (`accept_admin`)**: The proposed successor must explicitly invoke `accept_admin` with cryptographic authorization (`pending.require_auth()`). Once verified:
   - `admin` is updated to the successor.
   - `PendingAdmin` is atomically cleared from instance storage.
   - `AdminTransferredEvent` is emitted.
3. **Direct Rotation (`set_admin`)**: Supports immediate rotation in single-sig or automated contexts, immediately wiping any lingering `PendingAdmin`.

## Defensive Security & Invariants

1. **Forbidden Address Sentinel**:
   - Zero-address (`GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF`) and contract self-address are strictly disallowed across all role configurations (`admin`, `metadata_admin`, `manager`, `stream`, and `propose_admin`). Attempts panic with `Error::InvalidRoleAddress`.
2. **Checked Arithmetic**:
   - Token supplies and balances use checked arithmetic operations (`checked_add`, `checked_sub`) preventing potential overflow or underflow conditions.
3. **Ledger Footprint Optimization & Storage TTL**:
   - Instance storage keys (`Config`, `Supply`, `PendingAdmin`) and persistent entries (`Owner`, `Balance`) employ `extend_ttl` to ensure entry retention up to network `max_ttl`.
4. **Emergency Mint Controls**:
   - The `manager` can set `mint_paused = true` in response to upstream vulnerabilities or market volatility, causing subsequent `mint` invocations to reject with `Error::MintPaused`.

## Audit & Verification Matrix

Unit and boundary test suites in `contracts/stream-nft/src/test.rs` verify:
- Independent role enforcement (least privilege)
- Negative-path rejection for unauthorized callers and forbidden addresses
- Two-step admin hand-off and proposal cancellation
- Fuzzing and boundary test cases on `stream_id` (`0`, `1`, `u64::MAX - 1`, `u64::MAX`)
- Safe mint pause and unpause lifecycles
