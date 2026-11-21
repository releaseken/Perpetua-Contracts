//! Formal invariants and security properties for `contracts/factory`.
//!
//! # Core Protocol Invariants
//!
//! * **I1 (Active Streams Capacity Bound)**:
//!   `active_streams <= max_active_streams`.
//!   The factory must never admit streams beyond `max_active_streams`.
//!   Every state transition (`create_stream`, `set_max_active_streams`, `stream_terminated`)
//!   must preserve `active_streams <= max_active_streams` and prevent underflow/overflow.
//!
//! * **I2 (Re-entrancy Immunity and CEI)**:
//!   During cross-contract calls into the stream contract (e.g. `create_stream`),
//!   an execution lock (`DataKey::Executing`) prevents re-entrant calls into the
//!   factory contract before the call completes.
//!
//! * **I3 (State Conservation and Desync Resistance)**:
//!   Post-condition assertions verify that `active_streams` increments by exactly 1
//!   on successful stream creation and decrements by exactly 1 on `stream_terminated`.
//!   Any divergence aborts execution with `FactoryError::InvariantViolation`.
//!
//! * **I4 (Policy Configuration Consistency)**:
//!   `min_duration <= max_duration`, `max_deposit > 0`, and if rate bounds are set,
//!   `0 <= min_rate_per_second <= max_rate_per_second`.
//!   `max_creations_per_window > 0` and `rate_limit_window_ledgers > 0`.
//!
//! * **I5 (Deterministic Deployment and Admin Safety)**:
//!   Admin address must never be the zero address (`G...WHF`) or the factory's own contract address.
//!   Stream salts are deterministic, length-fixed, and collision-resistant across all `(sender, nonce, stream_id)` tuples.
