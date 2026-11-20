//! Test suite, staged to match the build order.
//!
//! * **Stage 1** — data model, create, withdraw, views, plus the two tests that
//!   gate everything else: the accrual property suite and the pool invariant.
//! * **Stage 2** — cliff, cancel, pause/resume, top-up, recipient transfer, and
//!   every adversarial boundary case.
//! * **Stage 3** — TTL survival and archival recovery, resource consumption at
//!   the batch cap.
//! * **Stage 4** — the stream id invariant: unique, strictly monotonic, and
//!   never consumed or reused by a failed create, independent of fixture
//!   order.

mod common;
mod events;
mod missing;
mod hardening_177_181_184_190;

// ABI inventory — generated from the contract spec, independent of stage.
mod abi;

// Issue #1535 — discriminant fixture and public error-path regression tests.
mod error_discriminants;
mod types_schema;

// Stage 1
mod create;
mod props;
mod withdraw;
// Issue #1583: withdrawal return value matches emitted amounts.
mod withdraw_events;
// Reads get, stream_count, exists + TTL side-effect freedom.
mod read_methods_no_side_effects;

// Stage 2
mod auth;
// Issue #13: authorization matrix for custom `__check_auth` contract accounts.
mod auth_matrix;
mod cancel;
// Issue #1584: the cancellation event's accounting contract.
mod amount_domain;
mod cancel_events;
mod cliff;
mod delegation;
mod pause;
mod race_cancel_withdraw;
mod storage_keys;
mod terminal_operations;
// Issue #14: SEP-41 non-standard return values and transfer failure rollbacks.
mod token_errors;
// Issue #1571: withdrawal and batch-withdraw accounting rollback on token failure.
mod withdrawal_atomicity;
mod top_up;
mod transfer;

// Issue #104: capability flags are immutable — static source + runtime proof.
mod immutability;

// Stage 3
mod accrual_overflow;
mod batch;
// Issue #10: conservation invariant engine — vested(t) + refundable(t) == deposited.
mod invariants;
mod lifecycle_proptest;
mod monotonicity;
mod release_profile;
mod resource_limits;
// Issue #16: extend_stream_ttl clamps to the dynamically queried max_entry_ttl.
mod ttl;

// Stage 4
mod stream_ids;

// Issue 1: Explicit invariant enforcement for edge-case inputs
// (zero-duration cliffs, i128::MAX deposits, same-ledger transitions).
// Regression pins for adversarial accrual inputs that the pure property suite
// and the overflow suite do not jointly cover.
mod accrual_edge_cases;

// Issue 2: Deterministic parity and snapshot tests for event topic layouts and
// accrual payload schemas. Guards downstream indexer and SDK ABI stability.
mod accrual_indexer_parity;

// Issue 3: Performance regression tests — accrual stays within Soroban
// CPU/memory budgets under batch payroll workloads (MAX_BATCH_SIZE streams,
// pause/resume history, no super-linear growth).
mod accrual_budget;

// Issue #407: cliff-vesting error taxonomy and human-readable messages.
mod cliff_error_taxonomy;

// Issue #408: cliff-phase TTL automation — creation coverage, keeper path,
// paused-stream wall-clock drift.
mod cliff_ttl_tests;

// Issue #409: batch cross-contract invocation optimisation — token-group
// coalescing, homogeneous-batch fast path, conservation across grouping.
mod batch_opt_tests;

// Issue #411: precision-safe fractional withdrawal arithmetic — split
// conservation, proportional_withdraw, withdraw_with_reserve, batch split.
mod withdrawal_math_tests;
