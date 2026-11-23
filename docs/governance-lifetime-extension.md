# Automated Entry Lifetime Extension & Archival Resilience for Governance

## Executive Summary
This document addresses **Issue #354**: *feat(contracts/governance): implement automated entry lifetime extension for governance to guarantee archival resilience*.

The `contracts/governance` subsystem manages critical multi-sig timelocked administrative operations across Perpetua Contracts, including signer rotation, threshold updates, protocol parameter changes, and batch operations. Archival resilience guarantees that contract instance storage and active proposal persistent storage remain preserved against Soroban ledger expiration and cold archival evictions under all standard operating conditions.

---

## Technical Scope & Architecture

### 1. Lifetime Extension Strategy & Storage Tiers

| Storage Tier | Target Keys | Lifetime Strategy | Mechanism |
|--------------|-------------|-------------------|-----------|
| **Instance Storage** | `Admin`, `Signers`, `Threshold`, `SignerWeights`, `SignerGeneration`, `NextProposalId`, `GracePeriod`, `EmergencyGuardians`, `SignerIndex` | Extended to `INSTANCE_BUMP_AMOUNT` (120,960 ledgers ≈ 7 days) whenever TTL drops below `INSTANCE_LIFETIME_THRESHOLD` (17,280 ledgers ≈ 1 day) | Bumper function `bump_instance` triggered on: initialization (`init`), administrative updates (`set_admin`, `add_signer`, `remove_signer`, `set_threshold`, `update_threshold`, `set_vote_weight`, `set_emergency_guardians`, `set_grace_period`), proposal lifecycle transitions (`propose`, `approve`, `execute`, `cancel_proposal`, `prune_expired_proposals`), and public read views (`get_admin`, `get_signers`, `get_emergency_guardians`, `get_threshold`, `quorum`, `proposal_count`, `grace_period`, `vote_weight`, `get_proposals_by_id_range`). |
| **Persistent Storage (Active Proposals)** | `DataKey::Proposal(id)`, `DataKey::QuorumReachedAt(id)`, `DataKey::ProposalApprovalIdx(id)` | Extended to `ACTIVE_PROPOSAL_BUMP_AMOUNT` (30 days + 7 days buffer = 640,800 ledgers) | Bumped via `bump_proposal_retention` and `bump_active_entry` on every read and mutation (`load_proposal`, `save_proposal`, `approve`, `execute`, `get_quorum_info`, `is_executable`, `get_proposals_by_id_range`). Active proposals cannot archive while accumulating votes or queued behind timelocks. |
| **Persistent Storage (Terminal Proposals)** | `DataKey::Proposal(id)`, `DataKey::QuorumReachedAt(id)`, `DataKey::ProposalApprovalIdx(id)` | One-shot terminal floor (`TERMINAL_PROPOSAL_RETENTION` = 30 days) | Sealed via `seal_terminal_proposal` at the instant of completion (`Executed` or `Cancelled`). Subsequent reads deliberately skip bumping to prevent unbounded rent growth and optimize ledger footprint. |

### 2. Defensive Arithmetic & Bound Defenses
- **Checked Arithmetic**: Proposal counter increments (`increment_proposal_id`), signer generation bumps (`bump_signer_generation`), voting weight aggregations, and execution deadlines (`checked_deadline`) strictly utilize checked arithmetic (`checked_add`), preventing host execution faults or arithmetic overflow.
- **Checked Key Probing**: The Soroban host rejects `extend_ttl` on non-existent keys. `bump_active_entry` and `seal_terminal_proposal` explicitly check `env.storage().persistent().has(key)` prior to invoking `extend_ttl`.
- **DoS Protections**: Range queries (`get_proposals_by_id_range`) clamp limits to `MAX_PAGE_SIZE = 100`, bounding CPU instructions and storage reads within Soroban host transaction metering budgets.

### 3. Formal Invariant Guarantees
- **I1 (Instance Preservation)**: Instance configuration entries never archive while the governance contract is queried or actively governing targets.
- **I2 (Active Proposal Liveness)**: Storage entries for in-flight proposals (`Proposed`, `Approved`, `Queued`) outlive `MAX_PROPOSAL_AGE_SECONDS` and are kept live across arbitrary voting intervals.
- **I3 (Finite Retention for Terminal Proposals)**: Historical executed/cancelled proposals stop consuming protocol rent after their 30-day indexer retrieval window.
- **I4 (Safe Host Footprint)**: Bumps are conditioned on `PERSISTENT_LIFETIME_THRESHOLD` / `INSTANCE_LIFETIME_THRESHOLD` to prevent redundant host writes and metering overhead.

### 4. Verification and Test Suite
The contract test suite in `contracts/governance/src/lib.rs` verifies the resolution:
- `test_instance_lifetime_extended_on_init`: Validates initial instance TTL bump.
- `test_instance_lifetime_extended_on_get_signers`: Validates automated instance lifetime extension on signer retrieval.
- `test_instance_lifetime_extended_on_get_threshold_and_quorum`: Validates automated instance lifetime extension on threshold and quorum reads.
- `test_instance_lifetime_extended_on_get_emergency_guardians`: Validates automated instance lifetime extension on emergency guardian reads.
- `test_persistent_proposal_lifetime_extended_on_active_load`: Validates proposal persistent storage extension past max age.
