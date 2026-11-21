# Archival Probe Granular Role Authorization & Cross-Contract Batch Claim Optimization Specification

## Executive Summary
This document addresses:
- **Issue #376**: *feat(contracts/archival-probe): add granular role-based authorization to archival-probe to prevent unauthorized configuration updates*
- **Issue #382**: *perf(contracts/archival-probe): streamline cross-contract invocation overhead in archival-probe during high-frequency batch claims*

The `contracts/archival-probe` subsystem serves as a proof harness against live Soroban networks, validating archival expiration dynamics, persistent storage restore footprints, and stream arithmetic integrity. To support multi-party validation and high-frequency testing without compromising security or exhausting host metering budgets, this implementation introduces granular role-based authorization and streamlined batch claim probe mechanisms.

---

## 1. Granular Role-Based Authorization (#376)

### Functional Role Partitioning
Administrative and operational authority is partitioned into four orthogonal, non-overlapping functional roles:

| Role ID | Role Enum | Functional Scope | Gated Operations |
|---|---|---|---|
| `0` | `Role::Admin` | Ultimate governance, role delegation, admin rotation. | `initialize`, `set_role`, `set_admin`, `propose_admin` |
| `1` | `Role::ConfigAdmin` | Probe parameter updates and batch size boundary enforcement. | `set_max_batch_size` |
| `2` | `Role::EmergencyGuardian` | Fast-acting circuit breaker: global probe operation pause / unpause. | `set_paused` |
| `3` | `Role::Operator` | Operational test execution and probe planting runs. | Operational verification |

### Security Guarantees & Constraints
1. **Admin Supremacy & Recovery**:
   - Holding `Role::Admin` inherently satisfies lower-tier role checks (`ConfigAdmin`, `EmergencyGuardian`, `Operator`), preventing governance deadlocks while preserving fine-grained operational delegation.
2. **Address Sanitization & Lockout Protection**:
   - Zero-account sentinel (`GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF`) is rejected for any role assignment (`Error::InvalidRoleAddress`).
   - The contract's own address (`env.current_contract_address()`) is rejected for any role assignment (`Error::InvalidRoleAddress`).
   - The contract admin cannot revoke `Role::Admin` from their own active account, preventing accidental contract lockout.
3. **Two-Step Admin Hand-off**:
   - Rotating contract ownership follows a safe two-step workflow (`propose_admin` -> `accept_admin`), revoking the incumbent and transferring `Role::Admin` only upon valid signed acceptance by the nominee.
4. **Auditable Role Lifecycle**:
   - Role updates emit structured events `(role_updated, role)` containing `{role, account, authorized}`.

---

## 2. Streamlining Cross-Contract Invocation Overhead (#382)

### The Problem
During high-frequency batch claims or multi-stream settlement sweeps, external contracts (such as `FluxoraStream`, dispute arbiters, or pause-clock coordinators) validating archival and stream claims per-item incur significant Soroban host cross-contract sub-invocation overhead:
- O(N) host context switches, stack frames, and parameter deserializations.
- Cumulative gas and metering exhaustion.

### Streamlined Batch Processing
The contract provides consolidated batch endpoints:
- `batch_claim_probe(claimant, stream_ids, amounts) -> Result<u32, Error>`: Records claim probe assertions across multiple streams in a single cross-contract invocation frame.
- `batch_get_claim_probes(stream_ids) -> Result<Vec<Option<ClaimProbeRecord>>, Error>`: Single-pass batch retrieval of claim probe records.

### Archival & Metering Guarantees
1. **Zero TTL Extension (Strict Invariant I2)**:
   - Neither `batch_claim_probe` nor `batch_get_claim_probes` extends the TTL of `Key::ClaimProbe(stream_id)` entries. This ensures entries archive naturally at `min_persistent_ttl` without unintended renewal.
2. **Idempotent First-Write-Wins (Invariant I3)**:
   - Replay attempts during retry cascades return success without mutating existing records or refreshing timestamps.
3. **Checked Arithmetic & Bounded Batches (Invariant I4)**:
   - Dynamic batch limits (`max_batch_size <= MAX_BATCH_PROBE_SIZE = 32`) and vector length matching prevent out-of-bounds execution and host metering overflow.

---

## 3. Formal Invariants Matrix

| Invariant | Description | Enforcement Point |
|---|---|---|
| **I1: Single Deserialization** | In-memory representations held in contiguous guest memory | `batch_read`, `batch_get_claim_probes` |
| **I2: Persistence Without Extension** | Canary and probe entries never refresh TTL | `plant`, `batch_plant`, `batch_claim_probe` |
| **I3: Write-Once / Idempotence** | First write wins; re-executions do not alter records | `plant_probe`, `batch_plant`, `batch_claim_probe` |
| **I4: Safe Bounds & Checked Math** | Batches capped by `MAX_BATCH_PROBE_SIZE`; checked arithmetic | All batch and math methods |
| **I5: Invocation Overhead Reduction** | Consolidates multi-item operations into 1 invocation frame | `batch_claim_probe`, `batch_get_claim_probes` |
| **I6: Fractional Conservation** | Exact conservation identity: `payout + remainder == total` | `calculate_fractional_withdrawal` |
| **I7: Granular Role-Based Authorization** | Gated operations require specific roles; zero/self rejected | `require_role_for`, `set_role`, `set_admin` |

---

## 4. Verification Suite Summary
The test suite in `contracts/archival-probe/src/test.rs` covers:
1. Role initialization and admin supremacy.
2. Role delegation, stranger rejection (`UnauthorizedRole`), and revocation.
3. Invalid address guards (zero address and contract self rejected with `InvalidRoleAddress`).
4. Two-step admin rotation (`propose_admin` / `accept_admin`).
5. ConfigAdmin parameter isolation (`set_max_batch_size`).
6. EmergencyGuardian circuit breaking (`set_paused` halts mutating batches with `ContractPaused`).
7. Streamlined batch claim probe recording, single-pass batch retrieval, and replay idempotency.
8. Defensive bounds validation (empty batches, length mismatches, oversized batches).
9. Proptest property-based fuzzing for fractional conservation and equal-share splits.
