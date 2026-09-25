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

// ABI inventory — generated from the contract spec, independent of stage.
mod abi;

// Issue #1535 — discriminant fixture and public error-path regression tests.
mod error_discriminants;

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
