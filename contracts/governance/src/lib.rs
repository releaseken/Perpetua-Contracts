#![no_std]
#![allow(clippy::too_many_arguments)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, xdr::FromXdr, Address,
    Bytes, BytesN, Env, IntoVal, Map, Symbol, Vec,
};

// ---------------------------------------------------------------------------
// Governance constants
// ---------------------------------------------------------------------------

/// Seconds a proposal must remain unexecuted after reaching quorum before it can
/// be executed. Default: 48 hours.
const GOVERNANCE_TIMELOCK_SECONDS: u64 = 172_800;

/// Maximum number of co-signers the governance contract supports.
const MAX_SIGNERS: u32 = 20;

/// Maximum byte length for proposal calldata payload.
const MAX_CALLDATA_BYTES: u32 = 4_096;

/// Maximum number of operations in a single batch proposal (#51).
///
/// A batch is executed as a sequence of cross-contract invocations inside one
/// transaction, so its cost is linear in this number. Five covers the policy
/// updates that motivated batching (for example duration bounds *and*
/// allowlist) with room to spare, while keeping the worst case well inside a
/// transaction's instruction and rent budget. See
/// `docs/governance-batch-proposals.md` for the bounds this was checked
/// against, and for what is and is not measured.
const MAX_BATCH_CALLS: u32 = 5;

/// Maximum age in seconds for a proposal before it expires and becomes
/// non-executable. Default: 30 days.
const MAX_PROPOSAL_AGE_SECONDS: u64 = 2_592_000;

/// Default grace window in seconds after a proposal's `eta` (executable-after
/// timestamp) during which it may still be executed. Default: 28 days.
///
/// `eta = quorum_reached_at + GOVERNANCE_TIMELOCK_SECONDS`, so this default
/// keeps the execution window aligned with the 30-day `MAX_PROPOSAL_AGE_SECONDS`
/// hard cap for proposals whose quorum was reached at proposal time.
const DEFAULT_GRACE_PERIOD_SECONDS: u64 = 2_419_200;

/// Maximum number of proposals that `get_proposals_by_id_range` will return in
/// a single call.
///
/// # DoS protection
///
/// Each proposal record contains a `Vec<Address>` of approvals (up to
/// `MAX_SIGNERS = 20` entries), a `Bytes` calldata payload (up to
/// `MAX_CALLDATA_BYTES = 4 096`), and several scalar fields.  At 100 proposals
/// per page the total read budget stays well within Soroban's metered limits.
/// Callers that pass a larger `limit` have it silently clamped to this value;
/// they cannot exceed it by any means.
pub const MAX_PAGE_SIZE: u32 = 100;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Lifecycle state of a governance proposal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProposalStatus {
    /// Created, but no signer has approved it yet.
    Proposed,
    /// At least one signer has approved it, but quorum has not been reached.
    Approved,
    /// Quorum has been reached and the proposal is waiting for its timelock.
    Queued,
    /// The proposal was executed successfully.
    Executed,
    /// The proposal was cancelled and is permanently terminal.
    Cancelled,
}

/// Persistent record of a governance proposal.
///
/// `calldata` is stored as an opaque `Bytes` payload whose interpretation is
/// left to the off-chain executor or to a typed adapter layer.  Storing the
/// payload on-chain provides a tamper-evident audit trail and enables indexers
/// to reconstruct the full proposal intent without any additional side-channel.
#[contracttype]
#[derive(Clone, Debug)]
pub struct Proposal {
    /// Address that submitted the proposal.
    pub proposer: Address,
    /// Target contract whose parameters should be changed upon execution.
    pub target: Address,
    /// Opaque calldata encoding the intended function call and arguments.
    /// Must be non-empty (at least 1 byte) and must not exceed `MAX_CALLDATA_BYTES`.
    pub calldata: Bytes,
    /// List of co-signer addresses that have approved this proposal.
    pub approvals: Vec<Address>,
    /// Total accumulated voting weight of all approvals so far (#48).
    ///
    /// Each approval adds its signer's *assigned* voting weight (default 1). The
    /// sum is accumulated with `checked_add` at approval time, so it is
    /// overflow-safe and snapshots the weights in effect when each approval was
    /// cast — a later `set_vote_weight` does not retroactively change quorum.
    pub approval_weight: u64,
    /// Ledger timestamp at which the proposal was submitted.
    pub created_at: u64,
    /// Earliest ledger timestamp at which this proposal may execute; zero until quorum.
    pub eta: u64,
    /// Signer configuration generation captured when the proposal was created.
    pub signer_generation: u64,
    /// Authoritative lifecycle state.
    pub status: ProposalStatus,
    /// True once `execute` has been called successfully.
    pub executed: bool,
    /// True once `cancel_proposal` has been called. Terminal — no further
    /// approvals or execution are allowed.
    pub cancelled: bool,
}

/// Error codes for the governance contract.
#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum GovernanceError {
    /// Contract has not been initialised.
    NotInitialized = 1,
    /// Contract is already initialised.
    AlreadyInitialized = 2,
    /// Caller is not the admin.
    Unauthorized = 3,
    /// Caller is not a registered co-signer.
    NotASigner = 4,
    /// Proposal with this ID does not exist.
    ProposalNotFound = 5,
    /// Proposal has already been executed.
    AlreadyExecuted = 6,
    /// Proposal has not yet accumulated the required number of approvals.
    QuorumNotReached = 7,
    /// Timelock period has not elapsed since quorum was first reached.
    TimelockNotElapsed = 8,
    /// Proposal execution was attempted before its stored eta timestamp.
    TimelockNotMet = 21,
    /// Signer has already approved this proposal.
    AlreadyApproved = 9,
    /// Calldata exceeds MAX_CALLDATA_BYTES.
    CalldataTooLarge = 10,
    /// Signer list exceeds MAX_SIGNERS.
    TooManySigners = 11,
    /// Proposal has passed the max age window and can no longer be approved or executed.
    ProposalExpired = 12,
    /// Proposal has been cancelled and can no longer be approved or executed.
    ProposalCancelled = 13,
    /// Caller is not the proposer nor the admin of the contract.
    NotProposerOrAdmin = 14,
    /// Provided threshold is zero or exceeds signer count.
    InvalidThreshold = 15,
    /// Removing this signer would leave fewer signers than the required threshold.
    QuorumWouldBreak = 16,
    /// Signer is already registered in the co-signer set.
    DuplicateSigner = 17,
    /// Governance arithmetic would overflow instead of producing a valid deadline or ID.
    ArithmeticOverflow = 18,
    /// Proposal calldata is empty; at least one byte is required.
    CalldataEmpty = 19,
    /// Proposal calldata failed to decode into a known `CallData` variant.
    InvalidCalldata = 20,
    /// A proposal predates the current signer-configuration generation and must
    /// be re-proposed under the signer set currently in force.
    InvalidSignerGeneration = 22,
    /// A dispatch is already in flight; re-entering `execute` is rejected.
    ReentrancyGuard = 23,
    /// The emergency-guardian list exceeds `MAX_SIGNERS`.
    TooManyEmergencyGuardians = 24,
    /// An address is already registered as an emergency guardian.
    DuplicateEmergencyGuardian = 25,
    /// The requested operation is not legal for the proposal's current state.
    ///
    /// Renumbered from `21`, where it collided with [`Self::TimelockNotMet`].
    /// The old code was unreachable: decoding `21` always resolved to
    /// `TimelockNotMet`.
    InvalidProposalState = 26,
    /// A batch payload holds more than [`MAX_BATCH_CALLS`] operations.
    ///
    /// Bounds the worst-case instruction and rent cost of a single execution
    /// (#51). The limit is structural, not a policy knob: no valid batch
    /// exceeds it, so a proposal that trips it can never be executed.
    BatchTooLarge = 27,
}

/// Storage keys for the governance contract.
#[contracttype]
#[repr(u32)]
pub enum DataKey {
    /// Admin address (instance storage).
    Admin = 0,
    /// Registered co-signers list (instance storage).
    Signers = 1,
    /// Minimum approval threshold (instance storage).
    Threshold = 2,
    /// Seconds a proposal may remain executable after its `eta` (instance storage).
    GracePeriod = 9,
    /// Emergency guardian addresses authorized to cancel proposals.
    EmergencyGuardians = 10,
    /// Assigned voting weight per signer (instance storage). Absent signers
    /// default to weight 1 (#48).
    SignerWeights = 12,
    /// Monotonic signer configuration generation (instance storage).
    ///
    /// Explicitly `8`, **not** the implicit `3` that follows `Threshold`:
    /// `3` is the long-standing code of [`Self::NextProposalId`], and both are
    /// unit variants, so sharing it made the proposal-ID counter and the
    /// signer generation the *same* storage slot. `increment_proposal_id`
    /// would then bump the generation, invalidating every queued proposal.
    SignerGeneration = 8,
    /// Monotonic proposal ID counter (instance storage).
    NextProposalId = 3,
    /// Persistent record for a proposal (persistent storage, keyed by ID).
    Proposal(u32) = 4,
    /// Ledger timestamp at which a proposal first reached quorum (persistent).
    QuorumReachedAt(u32) = 5,
    /// Map<Address, bool> membership index for O(1) signer lookups (instance storage).
    SignerIndex = 6,
    /// Per-proposal Map<Address, bool> for O(1) duplicate-approval detection (persistent).
    ProposalApprovalIdx(u32) = 7,
    /// In-flight flag set while a proposal's target call is being dispatched
    /// (instance storage). The non-reentrancy guard tested against in #47.
    Executing = 11,
}

// ---------------------------------------------------------------------------
// Typed calldata adapter
// ---------------------------------------------------------------------------

/// One entry of a batch proposal: a target contract plus the XDR-encoded
/// [`CallData`] describing what to do to it.
///
/// A batch is a list of these, so a single proposal can update several
/// parameters — on one target or across several — inside one timelock cycle.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ExecutionCall {
    /// Contract to invoke.
    pub target: Address,
    /// XDR-encoded single-operation [`CallData`]. Batches may not nest.
    pub calldata: Bytes,
}

/// Typed encoding of every parameter change that governance is authorised to
/// perform on-chain.  Proposers serialise one of these variants to XDR bytes
/// via `.to_xdr(&env)` and pass the result as the `calldata` field of
/// `propose`.  `execute` decodes the bytes with `CallData::from_xdr` and
/// dispatches to the target contract.
///
/// The wire form of a variant is its *name* (a `Symbol`), not its position, so
/// adding a variant — [`Batch`](CallData::Batch) included — leaves every
/// already-encoded proposal decodable.
///
/// Adding a new governed operation = adding a new variant here and a matching
/// arm in `dispatch_operation`.
#[contracttype]
#[derive(Clone, Debug)]
pub enum CallData {
    // ---- no-op (for testing governance mechanics without a live target) ----
    /// No operation — dispatch performs no cross-contract call.
    Noop,

    // ---- batch of operations (issue #51) ----
    /// `Vec<ExecutionCall>` applied in order, all-or-nothing.
    ///
    /// Each entry's `calldata` must itself decode to a single-operation
    /// variant; a nested `Batch` is rejected. Bounded by `MAX_BATCH_CALLS`.
    Batch(Vec<ExecutionCall>),

    // ---- stream contract operations ----
    /// `set_admin(new_admin)`
    StreamSetAdmin(Address),
    /// `set_max_rate_per_second(max_rate)`
    StreamSetMaxRate(i128),
    /// `global_resume()` — clear the stream contract's global emergency pause.
    /// Requires the governance contract to be the stream admin.
    StreamGlobalResume,
    /// `bulk_resume_streams_as_admin(stream_ids)` — atomically resume a batch of
    /// paused streams. Requires the governance contract to be the stream admin.
    /// Mixed batches that include a non-resumable stream (e.g. Cancelled) revert
    /// the entire dispatch with no partial state changes.
    StreamBulkResumeAsAdmin(soroban_sdk::Vec<u64>),

    // ---- factory contract operations ----
    /// `set_admin(new_admin)`
    FactorySetAdmin(Address),
    /// `set_cap(max_deposit)`
    FactorySetCap(i128),
    /// `set_min_duration(min_duration)`
    FactorySetMinDuration(u64),
    /// `set_allowlist(recipient, allowed)`
    FactorySetAllowlist(Address, bool),
    /// `set_stream_contract(new_stream_contract)`
    FactorySetStreamContract(Address),
    /// `set_stream_wasm_hash(reviewed_wasm_hash)`
    FactorySetStreamWasmHash(BytesN<32>),
    /// `deploy_stream_contract(approved_wasm_hash, salt)`
    FactoryDeployStreamContract(BytesN<32>, BytesN<32>),
}

/// Decode `calldata` bytes into a `CallData` variant.
fn decode_calldata(env: &Env, calldata: &Bytes) -> Result<CallData, GovernanceError> {
    CallData::from_xdr(env, calldata).map_err(|_| GovernanceError::InvalidCalldata)
}

/// Decode `calldata` bytes into a `CallData` variant and invoke the target.
/// Called inside `execute` *after* the proposal has been marked executed (CEI).
///
/// A [`CallData::Batch`] payload is applied in order, each entry carrying its
/// own target. Atomicity is the host's: every write performed here — including
/// the writes made by the earlier entries of a batch — is part of the
/// transaction, so an error from any entry, or a trap in any target, unwinds
/// all of them. A partially applied batch is not reachable.
fn dispatch_call(env: &Env, target: &Address, calldata: &Bytes) -> Result<(), GovernanceError> {
    match decode_calldata(env, calldata)? {
        CallData::Batch(calls) => {
            if calls.is_empty() {
                return Err(GovernanceError::InvalidCalldata);
            }
            if calls.len() > MAX_BATCH_CALLS {
                return Err(GovernanceError::BatchTooLarge);
            }
            for call in calls.iter() {
                let inner = decode_calldata(env, &call.calldata)?;
                if matches!(inner, CallData::Batch(_)) {
                    // Batches do not nest: nesting would let a proposal
                    // exceed MAX_BATCH_CALLS by hiding calls one level down.
                    return Err(GovernanceError::InvalidCalldata);
                }
                dispatch_operation(env, &call.target, inner)?;
            }
            Ok(())
        }
        // A single-operation proposal: dispatch to the proposal's own target.
        op => dispatch_operation(env, target, op),
    }
}

/// Invoke `target` for a single decoded operation. `op` is never
/// [`CallData::Batch`]; `dispatch_call` handles that variant itself.
///
/// Takes `op` by value so each arm binds the same owned values the
/// non-batch dispatch path has always passed to `invoke_contract`.
fn dispatch_operation(env: &Env, target: &Address, op: CallData) -> Result<(), GovernanceError> {
    match op {
        CallData::Noop => {}
        CallData::Batch(_) => {
            // Unreachable: `dispatch_call` peels the batch off before calling
            // here, and inner entries are rejected if they are batches.
            return Err(GovernanceError::InvalidCalldata);
        }
        CallData::StreamSetAdmin(new_admin) => {
            env.invoke_contract::<()>(
                target,
                &Symbol::new(env, "set_admin"),
                (new_admin,).into_val(env),
            );
        }
        CallData::StreamSetMaxRate(max_rate) => {
            env.invoke_contract::<()>(
                target,
                &Symbol::new(env, "set_max_rate_per_second"),
                (max_rate,).into_val(env),
            );
        }
        CallData::StreamGlobalResume => {
            env.invoke_contract::<()>(target, &Symbol::new(env, "global_resume"), Vec::new(env));
        }
        CallData::StreamBulkResumeAsAdmin(stream_ids) => {
            env.invoke_contract::<()>(
                target,
                &Symbol::new(env, "bulk_resume_streams_as_admin"),
                (stream_ids,).into_val(env),
            );
        }
        CallData::FactorySetAdmin(new_admin) => {
            env.invoke_contract::<()>(
                target,
                &Symbol::new(env, "set_admin"),
                (new_admin,).into_val(env),
            );
        }
        CallData::FactorySetCap(max_deposit) => {
            env.invoke_contract::<()>(
                target,
                &Symbol::new(env, "set_cap"),
                (max_deposit,).into_val(env),
            );
        }
        CallData::FactorySetMinDuration(min_duration) => {
            env.invoke_contract::<()>(
                target,
                &Symbol::new(env, "set_min_duration"),
                (min_duration,).into_val(env),
            );
        }
        CallData::FactorySetAllowlist(recipient, allowed) => {
            env.invoke_contract::<()>(
                target,
                &Symbol::new(env, "set_allowlist"),
                (recipient, allowed).into_val(env),
            );
        }
        CallData::FactorySetStreamContract(new_contract) => {
            env.invoke_contract::<()>(
                target,
                &Symbol::new(env, "set_stream_contract"),
                (new_contract,).into_val(env),
            );
        }
        CallData::FactorySetStreamWasmHash(new_hash) => {
            env.invoke_contract::<()>(
                target,
                &Symbol::new(env, "set_stream_wasm_hash"),
                (new_hash,).into_val(env),
            );
        }
        CallData::FactoryDeployStreamContract(wasm_hash, salt) => {
            env.invoke_contract::<Address>(
                target,
                &Symbol::new(env, "deploy_stream_contract"),
                (wasm_hash, salt).into_val(env),
            );
        }
    }
    Ok(())
}

const INSTANCE_LIFETIME_THRESHOLD: u32 = 17_280;
const INSTANCE_BUMP_AMOUNT: u32 = 120_960;
/// Threshold below which a proposal-scoped entry is topped up. The bump amount
/// that goes with it is chosen per lifecycle state by the #52 retention policy
/// ([`ACTIVE_PROPOSAL_BUMP_AMOUNT`] / [`TERMINAL_PROPOSAL_RETENTION`]),
/// so there is deliberately no single persistent bump constant any more.
const PERSISTENT_LIFETIME_THRESHOLD: u32 = 17_280;

// ---------------------------------------------------------------------------
// Proposal retention policy (#52)
// ---------------------------------------------------------------------------

/// Seconds per ledger on Stellar. TTL arguments are denominated in **ledgers**,
/// not seconds, so every retention window below is converted from its
/// wall-clock intent through this constant.
const SECONDS_PER_LEDGER: u64 = 5;

/// Convert a wall-clock window in seconds into the ledger count the host expects.
///
/// Saturating rather than wrapping: a misconfigured constant should clamp to the
/// network maximum and trip the bounds test below, not silently wrap into a
/// negative-length window.
const fn ledgers(seconds: u64) -> u32 {
    let count = seconds / SECONDS_PER_LEDGER;
    if count > u32::MAX as u64 {
        u32::MAX
    } else {
        count as u32
    }
}

/// TTL bump applied to a proposal-scoped entry while the proposal is still
/// **active** (`Proposed` / `Approved` / `Queued`).
///
/// A proposal is never useful past `created_at + MAX_PROPOSAL_AGE_SECONDS`
/// (30 days): `approve` and `execute` both reject it as `ProposalExpired` after
/// that, and the quorum and grace windows are sized to land inside the same
/// bound. Bumping past that whole span means one bump carries the entry beyond
/// the last moment it could legally be read or written, so an active proposal
/// cannot archive while it is still executable however long signers wait.
///
/// The extra week over `MAX_PROPOSAL_AGE_SECONDS` is deliberate headroom: the
/// worst case is a bump landing exactly at `created_at`, after which the entry
/// must survive one full max-age window. Remaining under the network's
/// `max_entry_ttl` (6_312_000 ledgers, about a year) is asserted by
/// `test_retention_windows_stay_within_network_limits`.
const ACTIVE_PROPOSAL_BUMP_AMOUNT: u32 = ledgers(MAX_PROPOSAL_AGE_SECONDS + 604_800);

/// Retention floor granted to a proposal that has just reached a **terminal**
/// state (`Executed` / `Cancelled`).
///
/// Applied exactly once, at the moment the proposal becomes terminal, and never
/// again. 30 days is the standard archival floor: long enough for indexers and
/// auditors to reconcile the proposal against the events it emitted, after
/// which the entry is left to decay back to the ledger's minimum rent instead of
/// being held alive indefinitely by later reads.
///
/// Because the floor is never topped up, a terminal proposal's storage cost
/// falls to zero once the window elapses — see `docs/governance-storage.md`.
const TERMINAL_PROPOSAL_RETENTION: u32 = ledgers(2_592_000);

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Standardised audit-log schema for the governance lifecycle (#49).
///
/// Every lifecycle action emits a structured event whose **topics** carry the
/// indexer-friendly filter keys — event name, `proposal_id` and the acting
/// participant — and whose **data** payload carries the full schema including
/// targets and timestamps:
///
/// | Action          | Topic                                        | Data                                |
/// |-----------------|----------------------------------------------|-------------------------------------|
/// | propose         | `("proposal_created",  id, proposer)`        | `ProposalCreated`   (with target)   |
/// | approve         | `("vote_cast",       id, voter)`             | `VoteCast`          (count + weight + ts) |
/// | quorum reached  | `("proposal_queued", id)`                    | `ProposalQueued`    (timestamps)    |
/// | execute         | `("proposal_executed", id, executor)`        | `ProposalExecuted`  (with target)   |
/// | cancel          | `("proposal_cancelled", id, canceller)`      | `ProposalCancelled` (timestamp)     |
///
/// `proposal_id` is always the second topic element so indexers can filter the
/// full lifecycle of a single proposal with a single topic subscription.

/// Event: a new proposal was submitted (topic `proposal_created`).
#[contracttype]
#[derive(Clone, Debug)]
pub struct ProposalCreated {
    pub proposal_id: u32,
    pub proposer: Address,
    pub target: Address,
    /// Ledger timestamp at which the proposal was submitted.
    pub created_at: u64,
}

/// Records the timestamp and effective threshold when quorum was first reached.
/// Used to judge in-flight proposals against the threshold that was active at
/// quorum time, protecting against mid-flight threshold changes by the admin.
#[contracttype]
#[derive(Clone, Debug)]
pub struct QuorumInfo {
    pub reached_at: u64,
    pub threshold: u32,
}

/// Event: a registered signer cast a vote (topic `vote_cast`).
#[contracttype]
#[derive(Clone, Debug)]
pub struct VoteCast {
    pub proposal_id: u32,
    pub voter: Address,
    /// Total headcount of approvals so far (including this vote).
    pub approval_count: u32,
    /// Ledger timestamp at which the vote was cast.
    pub timestamp: u64,
    /// Accumulated voting weight after this approval.
    pub approval_weight: u64,
}

/// Event: a proposal first reached quorum and was queued, starting the timelock
/// (topic `proposal_queued`).
#[contracttype]
#[derive(Clone, Debug)]
pub struct ProposalQueued {
    pub proposal_id: u32,
    pub quorum_reached_at: u64,
    /// `quorum_reached_at + GOVERNANCE_TIMELOCK_SECONDS` — earliest execution time.
    pub executable_after: u64,
    /// The threshold snapshot used for this quorum decision.
    pub threshold: u32,
}

/// Event: a proposal was cancelled by the proposer or admin (topic `proposal_cancelled`).
#[contracttype]
#[derive(Clone, Debug)]
pub struct ProposalCancelled {
    pub proposal_id: u32,
    pub canceller: Address,
    /// Ledger timestamp at which the proposal was cancelled.
    pub cancelled_at: u64,
}

/// Event: a proposal was executed after quorum and timelock (topic `proposal_executed`).
#[contracttype]
#[derive(Clone, Debug)]
pub struct ProposalExecuted {
    pub proposal_id: u32,
    pub executor: Address,
    pub target: Address,
    pub calldata: Bytes,
    /// Ledger timestamp at which the proposal was executed.
    pub executed_at: u64,
}

/// Emitted when the admin adds a new co-signer to the governance set.
///
/// Published by [`add_signer`](FluxoraGovernance::add_signer) after the signer
/// list has been persisted (CEI: state mutation precedes the event). Indexers
/// use this to reconstruct the live co-signer set from chain events alone.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SignerAdded {
    /// The address that was added to the co-signer set.
    pub signer: Address,
}

/// Emitted when the admin removes an existing co-signer from the governance set.
///
/// Published by [`remove_signer`](FluxoraGovernance::remove_signer) only when a
/// matching address was actually removed and the updated signer list persisted.
/// Removing an address that is not registered is a no-op and emits **no** event.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SignerRemoved {
    /// The address that was removed from the co-signer set.
    pub signer: Address,
}

/// Emitted when the co-signer set size changes, allowing indexers to track
/// whether the current threshold remains satisfiable and how close the
/// membership is to dropping below the threshold.
///
/// Published by [`add_signer`](FluxoraGovernance::add_signer) and
/// [`remove_signer`](FluxoraGovernance::remove_signer) after the membership
/// change is persisted.
#[contracttype]
#[derive(Clone, Debug)]
pub struct QuorumConfig {
    pub threshold: u32,
    pub signer_count: u32,
}

/// Emitted when the admin changes the approval threshold.
///
/// Published by [`set_threshold`](FluxoraGovernance::set_threshold) after the
/// new threshold has been persisted. Existing proposals that already reached
/// quorum keep using their stored [`QuorumInfo::threshold`] snapshot; this event
/// only describes the threshold used for future quorum decisions.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ThresholdUpdated {
    pub old_threshold: u32,
    pub new_threshold: u32,
}

/// Emitted when the admin address is rotated.
///
/// Published by [`set_admin`](FluxoraGovernance::set_admin) after the new admin
/// has been persisted (CEI: state mutation precedes the event). Carries both the
/// previous and new admin so indexers can reconstruct the full admin history.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminChanged {
    /// The admin address that was in effect before the rotation.
    pub old: Address,
    /// The admin address that is in effect after the rotation.
    pub new: Address,
}

// ---------------------------------------------------------------------------
// Storage helpers
// ---------------------------------------------------------------------------

fn bump_instance(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);
}

/// `true` once a proposal can never change state again.
///
/// Terminal proposals are the ones the retention policy stops paying for: they
/// get a one-shot archival floor when they terminate and are never topped up
/// again, so their rent decays instead of growing without bound (#52).
fn is_terminal(status: &ProposalStatus) -> bool {
    matches!(
        status,
        ProposalStatus::Executed | ProposalStatus::Cancelled
    )
}

/// Apply the #52 retention policy to a single proposal-scoped persistent entry.
///
/// **Active** proposals are extended past their own maximum possible lifetime
/// ([`ACTIVE_PROPOSAL_BUMP_AMOUNT`]), so a proposal that is still collecting
/// votes — or queued behind its timelock — cannot archive before it is acted
/// on, however long signers wait between votes.
///
/// **Terminal** proposals are skipped entirely. Their one-shot 30-day floor was
/// already granted by [`seal_terminal_proposal`] at the transition, and topping
/// it up again on every subsequent read is exactly the unbounded rent growth
/// this policy exists to prevent.
fn bump_active_entry(env: &Env, key: &DataKey, status: &ProposalStatus) {
    if is_terminal(status) {
        return;
    }
    // The host treats extending a non-existent or already-archived entry as an
    // error, not a no-op, so every key is probed first. A proposal that has not
    // reached quorum has no `QuorumReachedAt` entry, and one that has never
    // been approved has no `ProposalApprovalIdx`.
    if env.storage().persistent().has(key) {
        env.storage().persistent().extend_ttl(
            key,
            PERSISTENT_LIFETIME_THRESHOLD,
            ACTIVE_PROPOSAL_BUMP_AMOUNT,
        );
    }
}

/// Apply [`bump_active_entry`] to every persistent key a proposal owns.
///
/// A proposal's storage is three entries — the record itself, the quorum
/// snapshot, and the duplicate-approval index. They are read and written
/// together and share a lifetime, so they share one policy: keeping the record
/// alive while the index or the quorum snapshot decays would corrupt the
/// duplicate-approval check or the execution gates.
fn bump_proposal_retention(env: &Env, id: u32, status: &ProposalStatus) {
    bump_active_entry(env, &DataKey::Proposal(id), status);
    bump_active_entry(env, &DataKey::QuorumReachedAt(id), status);
    bump_active_entry(env, &DataKey::ProposalApprovalIdx(id), status);
}

/// Grant a proposal's storage the one-shot terminal retention floor (#52).
///
/// Called at the single moment a proposal becomes `Executed` or `Cancelled`.
/// Every key it touches is topped up to [`TERMINAL_PROPOSAL_RETENTION`]
/// (30 days) and, per [`bump_active_entry`], is never extended again — so after
/// the window the entries decay to the ledger minimum and stop consuming rent.
///
/// The threshold is deliberately `PERSISTENT_LIFETIME_THRESHOLD`, not the
/// retention window itself. The host only applies an extension when the entry's
/// current TTL is at or below the threshold, so a threshold equal to the target
/// would make this a no-op for any entry that still had a longer life left from
/// its active bumps. A low threshold means "top up whenever this entry is
/// anywhere near expiring", which is exactly the intent at a transition.
fn seal_terminal_proposal(env: &Env, id: u32) {
    for key in [
        DataKey::Proposal(id),
        DataKey::QuorumReachedAt(id),
        DataKey::ProposalApprovalIdx(id),
    ] {
        if env.storage().persistent().has(&key) {
            env.storage().persistent().extend_ttl(
                &key,
                PERSISTENT_LIFETIME_THRESHOLD,
                TERMINAL_PROPOSAL_RETENTION,
            );
        }
    }
}

/// `true` if the proposal exists and has reached a terminal state (#52).
///
/// Used by the read-only views, which hold a specific storage key rather than a
/// loaded `Proposal` and so cannot pass a status to [`bump_active_entry`]
/// directly. A proposal that cannot be found is reported as *not* terminal:
/// there is no storage left to keep alive, so the distinction does not matter,
/// and treating it as active keeps the caller's branch conservative.
fn proposal_is_terminal(env: &Env, id: u32) -> bool {
    env.storage()
        .persistent()
        .get::<DataKey, Proposal>(&DataKey::Proposal(id))
        .is_some_and(|p| is_terminal(&p.status))
}

fn get_signer_index(env: &Env) -> Result<Map<Address, bool>, GovernanceError> {
    env.storage()
        .instance()
        .get(&DataKey::SignerIndex)
        .ok_or(GovernanceError::NotInitialized)
}

fn save_signer_index(env: &Env, index: &Map<Address, bool>) {
    env.storage().instance().set(&DataKey::SignerIndex, index);
}

/// Read the per-proposal approval index, applying the #52 retention policy.
///
/// The index must outlive the proposal record it mirrors: if it decayed first,
/// the duplicate-approval check would fall back to an empty map and a signer
/// could approve the same proposal twice, so it is bumped together with the
/// record rather than independently.
fn get_approval_index(
    env: &Env,
    proposal_id: u32,
    status: &ProposalStatus,
) -> Map<Address, bool> {
    bump_active_entry(env, &DataKey::ProposalApprovalIdx(proposal_id), status);
    env.storage()
        .persistent()
        .get(&DataKey::ProposalApprovalIdx(proposal_id))
        .unwrap_or_else(|| Map::new(env))
}

fn save_approval_index(
    env: &Env,
    proposal_id: u32,
    index: &Map<Address, bool>,
    status: &ProposalStatus,
) {
    env.storage()
        .persistent()
        .set(&DataKey::ProposalApprovalIdx(proposal_id), index);
    bump_active_entry(env, &DataKey::ProposalApprovalIdx(proposal_id), status);
}

fn get_admin(env: &Env) -> Result<Address, GovernanceError> {
    bump_instance(env);
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(GovernanceError::NotInitialized)
}

fn get_signers(env: &Env) -> Result<Vec<Address>, GovernanceError> {
    bump_instance(env);
    env.storage()
        .instance()
        .get(&DataKey::Signers)
        .ok_or(GovernanceError::NotInitialized)
}

fn get_emergency_guardians(env: &Env) -> Vec<Address> {
    bump_instance(env);
    env.storage()
        .instance()
        .get(&DataKey::EmergencyGuardians)
        .unwrap_or_else(|| Vec::new(env))
}

fn get_threshold(env: &Env) -> Result<u32, GovernanceError> {
    bump_instance(env);
    env.storage()
        .instance()
        .get(&DataKey::Threshold)
        .ok_or(GovernanceError::NotInitialized)
}

fn get_signer_generation(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&DataKey::SignerGeneration)
        .unwrap_or(0u64)
}

fn bump_signer_generation(env: &Env) -> Result<u64, GovernanceError> {
    let next = get_signer_generation(env)
        .checked_add(1)
        .ok_or(GovernanceError::ArithmeticOverflow)?;
    env.storage().instance().set(&DataKey::SignerGeneration, &next);
    Ok(next)
}

fn read_next_proposal_id(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::NextProposalId)
        .unwrap_or(0u32)
}

/// The configured post-`eta` execution window (#46), falling back to
/// [`DEFAULT_GRACE_PERIOD_SECONDS`] so views never fail on a contract where
/// `set_grace_period` has not been called (including before `init`).
fn get_grace_period(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&DataKey::GracePeriod)
        .unwrap_or(DEFAULT_GRACE_PERIOD_SECONDS)
}

fn checked_deadline(start: u64, seconds: u64) -> Result<u64, GovernanceError> {
    start
        .checked_add(seconds)
        .ok_or(GovernanceError::ArithmeticOverflow)
}

/// Return the voting weight assigned to a signer (#48). Unset signers (and
/// non-signers) default to weight 1 so the pre-`#48` equal-vote behaviour is
/// preserved unless the admin explicitly assigns different weights.
fn signer_weight(env: &Env, signer: &Address) -> u64 {
    env.storage()
        .instance()
        .get::<DataKey, Map<Address, u64>>(&DataKey::SignerWeights)
        .and_then(|m| m.get(signer.clone()))
        .unwrap_or(1u64)
}

fn increment_proposal_id(env: &Env) -> Result<u32, GovernanceError> {
    let id = read_next_proposal_id(env);
    let next = id
        .checked_add(1)
        .ok_or(GovernanceError::ArithmeticOverflow)?;
    env.storage()
        .instance()
        .set(&DataKey::NextProposalId, &next);
    Ok(id)
}

fn load_proposal(env: &Env, id: u32) -> Result<Proposal, GovernanceError> {
    let proposal: Proposal = env
        .storage()
        .persistent()
        .get(&DataKey::Proposal(id))
        .ok_or(GovernanceError::ProposalNotFound)?;
    bump_proposal_retention(env, id, &proposal.status);
    Ok(proposal)
}

/// Persist a proposal and apply the #52 retention policy for its new state.
///
/// When the write moves a proposal into a terminal state, the one-shot archival
/// floor is granted here as well as the skip in [`bump_proposal_retention`] —
/// this is the moment the proposal stops needing to be kept alive, and the last
/// moment the contract is willing to pay for its storage.
fn save_proposal(env: &Env, id: u32, proposal: &Proposal) {
    env.storage()
        .persistent()
        .set(&DataKey::Proposal(id), proposal);
    if is_terminal(&proposal.status) {
        seal_terminal_proposal(env, id);
    }
    bump_proposal_retention(env, id, &proposal.status);
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct FluxoraGovernance;

#[contractimpl]
impl FluxoraGovernance {
    /// Initialise the governance contract with an admin, a list of co-signers,
    /// and an approval threshold.
    ///
    /// # Parameters
    /// - `admin`: Address that can add/remove signers and reset governance state.
    /// - `signers`: Initial list of co-signers eligible to approve proposals.
    ///   Must not exceed `MAX_SIGNERS` and must not contain duplicates.
    /// - `threshold`: Minimum number of approvals required for a proposal to
    ///   execute.  Must satisfy `1 <= threshold <= signers.len()`.
    ///
    /// # Errors
    /// - `AlreadyInitialized`: Contract has already been initialised.
    /// - `TooManySigners`: Provided signer list exceeds `MAX_SIGNERS`.
    /// - `DuplicateSigner`: Provided signer list contains the same address twice.
    /// - `InvalidThreshold`: `threshold` is zero or exceeds the number of signers.
    pub fn init(
        env: Env,
        admin: Address,
        signers: Vec<Address>,
        threshold: u32,
    ) -> Result<(), GovernanceError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(GovernanceError::AlreadyInitialized);
        }
        if signers.len() > MAX_SIGNERS {
            return Err(GovernanceError::TooManySigners);
        }
        if threshold == 0 || threshold > signers.len() {
            return Err(GovernanceError::InvalidThreshold);
        }

        // Build Map index in a single O(n) pass; duplicates are detected via the map.
        let mut signer_index: Map<Address, bool> = Map::new(&env);
        for i in 0..signers.len() {
            let s = signers.get(i).unwrap();
            if signer_index.contains_key(s.clone()) {
                return Err(GovernanceError::DuplicateSigner);
            }
            signer_index.set(s, true);
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Signers, &signers);
        env.storage()
            .instance()
            .set(&DataKey::SignerIndex, &signer_index);
        env.storage()
            .instance()
            .set(&DataKey::Threshold, &threshold);
        env.storage()
            .instance()
            .set(&DataKey::SignerGeneration, &0u64);
        env.storage()
            .instance()
            .set(&DataKey::NextProposalId, &0u32);

        bump_instance(&env);
        Ok(())
    }

    /// Update the admin address.
    ///
    /// # Authorization
    /// - Requires admin signature.
    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), GovernanceError> {
        let old_admin = get_admin(&env)?;
        old_admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        bump_instance(&env);

        // CEI: the new admin is persisted before the event is emitted.
        env.events().publish(
            (symbol_short!("adm_chg"),),
            AdminChanged {
                old: old_admin,
                new: new_admin,
            },
        );

        Ok(())
    }

    /// Set the emergency guardian addresses.
    pub fn set_emergency_guardians(
        env: Env,
        guardians: Vec<Address>,
    ) -> Result<(), GovernanceError> {
        get_admin(&env)?.require_auth();
        if guardians.len() > MAX_SIGNERS {
            return Err(GovernanceError::TooManyEmergencyGuardians);
        }
        for i in 0..guardians.len() {
            let guardian = guardians.get(i).unwrap();
            for j in (i + 1)..guardians.len() {
                if guardians.get(j) == Some(guardian.clone()) {
                    return Err(GovernanceError::DuplicateEmergencyGuardian);
                }
            }
        }
        env.storage()
            .instance()
            .set(&DataKey::EmergencyGuardians, &guardians);
        bump_instance(&env);
        Ok(())
    }

    /// Update the approval threshold for future governance proposals.
    ///
    /// # Authorization
    /// - Requires the current admin signature.
    ///
    /// # Validation
    /// `new_threshold` must satisfy `1 <= new_threshold <= signers.len()`.
    /// Invalid values return [`GovernanceError::InvalidThreshold`] before any
    /// storage write or event emission.
    ///
    /// # Security
    /// Proposals that already reached quorum are not retroactively affected:
    /// `approve` stores a [`QuorumInfo::threshold`] snapshot when quorum is
    /// first reached, and `execute` verifies against that snapshot.
    pub fn set_threshold(env: Env, new_threshold: u32) -> Result<(), GovernanceError> {
        get_admin(&env)?.require_auth();

        let signers = get_signers(&env)?;
        if new_threshold == 0 || new_threshold > signers.len() {
            return Err(GovernanceError::InvalidThreshold);
        }

        let old_threshold = get_threshold(&env)?;
        env.storage()
            .instance()
            .set(&DataKey::Threshold, &new_threshold);
        bump_signer_generation(&env)?;
        bump_instance(&env);

        env.events().publish(
            (symbol_short!("thr_upd"),),
            ThresholdUpdated {
                old_threshold,
                new_threshold,
            },
        );

        Ok(())
    }

    /// Assign a voting weight to a co-signer (#48).
    ///
    /// A signer's voting weight is added to the proposal's running approval
    /// weight every time they approve. The default weight is 1, so this is an
    /// opt-in escalation model: an admin can give a trusted signer more than
    /// one vote's worth of power, or zero to make them purely advisory.
    ///
    /// The accumulated weight of an in-flight proposal is snapshotted at each
    /// approval, so changing a weight here does *not* retroactively change the
    /// quorum status of proposals that have already collected approvals.
    ///
    /// # Authorization
    /// - Requires the current admin signature.
    ///
    /// # Errors
    /// - `NotASigner`: `signer` is not registered in the signer set.
    pub fn set_vote_weight(env: Env, signer: Address, weight: u64) -> Result<(), GovernanceError> {
        get_admin(&env)?.require_auth();
        if !Self::is_registered_signer(&env, &signer)? {
            return Err(GovernanceError::NotASigner);
        }
        let mut weights = env
            .storage()
            .instance()
            .get::<DataKey, Map<Address, u64>>(&DataKey::SignerWeights)
            .unwrap_or_else(|| Map::new(&env));
        weights.set(signer.clone(), weight);
        env.storage()
            .instance()
            .set(&DataKey::SignerWeights, &weights);
        bump_instance(&env);
        Ok(())
    }

    /// Return the voting weight assigned to `signer` (#48).
    ///
    /// Returns 1 (the default) for signers that have no explicit weight.
    pub fn vote_weight(env: Env, signer: Address) -> u64 {
        bump_instance(&env);
        signer_weight(&env, &signer)
    }

    /// Add a co-signer to the governance set.
    ///
    /// The signer set is unique: an address may occupy at most one co-signer slot.
    ///
    /// # Authorization
    /// - Requires admin signature.
    ///
    /// # Errors
    /// - `TooManySigners`: Adding this signer would exceed `MAX_SIGNERS`.
    /// - `DuplicateSigner`: `signer` is already registered.
    pub fn add_signer(env: Env, signer: Address) -> Result<(), GovernanceError> {
        get_admin(&env)?.require_auth();
        let mut signers = get_signers(&env)?;
        let mut signer_index = get_signer_index(&env)?;

        // O(1) duplicate check via Map index.
        if signer_index.contains_key(signer.clone()) {
            return Err(GovernanceError::DuplicateSigner);
        }
        if signers.len() >= MAX_SIGNERS {
            return Err(GovernanceError::TooManySigners);
        }
        signers.push_back(signer.clone());
        signer_index.set(signer.clone(), true);
        env.storage().instance().set(&DataKey::Signers, &signers);
        save_signer_index(&env, &signer_index);
        bump_signer_generation(&env)?;
        bump_instance(&env);

        // CEI: the updated signer set is persisted before the event is emitted.
        env.events()
            .publish((symbol_short!("sgnr_add"),), SignerAdded { signer });

        let threshold = get_threshold(&env)?;
        env.events().publish(
            (symbol_short!("quor_cfg"),),
            QuorumConfig {
                threshold,
                signer_count: signers.len(),
            },
        );

        Ok(())
    }

    /// Remove a co-signer from the governance set.
    ///
    /// # Authorization
    /// - Requires admin signature.
    ///
    /// # Errors
    /// - `QuorumWouldBreak`: Removal would leave fewer signers than the required
    ///   threshold, making future proposals permanently unexecutable.
    pub fn remove_signer(env: Env, signer: Address) -> Result<(), GovernanceError> {
        get_admin(&env)?.require_auth();
        let mut signer_index = get_signer_index(&env)?;

        // O(1) membership check — skip Vec scan entirely when address is absent.
        if !signer_index.contains_key(signer.clone()) {
            return Ok(());
        }

        let mut signers = get_signers(&env)?;
        let threshold = get_threshold(&env)?;
        if signers.len() - 1 < threshold {
            return Err(GovernanceError::QuorumWouldBreak);
        }

        // Scan Vec only to find the removal position (unavoidable for ordered Vec removal).
        for i in 0..signers.len() {
            if signers.get(i).is_some_and(|candidate| candidate == signer) {
                signers.remove(i);
                break;
            }
        }

        signer_index.remove(signer.clone());
        env.storage().instance().set(&DataKey::Signers, &signers);
        save_signer_index(&env, &signer_index);
        bump_signer_generation(&env)?;
        bump_instance(&env);

        // CEI: the updated signer set is persisted before the event is
        // emitted. Only reached when a matching signer was actually removed;
        // removing a non-existent address returned early above (silent no-op,
        // no event).
        env.events()
            .publish((symbol_short!("sgnr_rm"),), SignerRemoved { signer });

        env.events().publish(
            (symbol_short!("quor_cfg"),),
            QuorumConfig {
                threshold,
                signer_count: signers.len(),
            },
        );

        Ok(())
    }

    /// Set the approval threshold for governance proposals.
    ///
    /// # Parameters
    /// - `threshold`: Minimum number of approvals required for a proposal to
    ///   execute. Must satisfy `1 <= threshold <= current_signer_count`.
    ///
    /// # Authorization
    /// - Requires admin signature.
    ///
    /// # Errors
    /// - `InvalidThreshold`: `threshold` is zero or exceeds the current number of signers.
    pub fn update_threshold(env: Env, threshold: u32) -> Result<(), GovernanceError> {
        get_admin(&env)?.require_auth();
        let signers = get_signers(&env)?;

        if threshold == 0 || threshold > signers.len() {
            return Err(GovernanceError::InvalidThreshold);
        }

        env.storage()
            .instance()
            .set(&DataKey::Threshold, &threshold);
        bump_signer_generation(&env)?;
        bump_instance(&env);

        // CEI: the new threshold is persisted before the event is emitted.
        env.events().publish(
            (symbol_short!("quor_cfg"),),
            QuorumConfig {
                threshold,
                signer_count: signers.len(),
            },
        );

        Ok(())
    }

    /// Submit a new governance proposal.
    ///
    /// Any registered co-signer may propose. The proposer does not automatically
    /// approve the proposal — they must call `approve` separately.
    ///
    /// # Parameters
    /// - `proposer`: The co-signer submitting the proposal.
    /// - `target`: The contract address to call when the proposal is executed.
    ///   Ignored when `calldata` is a [`CallData::Batch`], where every entry
    ///   carries its own target; it is still stored for the audit trail.
    /// - `calldata`: Opaque bytes encoding the intended operation (stored for
    ///   audit). Either a single-operation [`CallData`] variant or a
    ///   [`CallData::Batch`] of up to [`MAX_BATCH_CALLS`] operations applied in
    ///   order, all-or-nothing (#51).
    ///
    /// # Returns
    /// - The proposal ID assigned to the new proposal (monotonically increasing u32).
    ///
    /// # Authorization
    /// - Requires `proposer.require_auth()`.
    ///
    /// # Errors
    /// - `NotASigner`: `proposer` is not in the registered signers list.
    /// - `CalldataEmpty`: `calldata` is empty (zero bytes).
    /// - `CalldataTooLarge`: `calldata.len() > MAX_CALLDATA_BYTES`.
    /// - `ArithmeticOverflow`: proposal ID counter has reached `u32::MAX`.
    pub fn propose(
        env: Env,
        proposer: Address,
        target: Address,
        calldata: Bytes,
    ) -> Result<u32, GovernanceError> {
        proposer.require_auth();

        // O(1) signer membership check via Map index.
        if !Self::is_registered_signer(&env, &proposer)? {
            return Err(GovernanceError::NotASigner);
        }

        if calldata.is_empty() {
            return Err(GovernanceError::CalldataEmpty);
        }

        if calldata.len() > MAX_CALLDATA_BYTES {
            return Err(GovernanceError::CalldataTooLarge);
        }

        let id = increment_proposal_id(&env)?;
        let now = env.ledger().timestamp();

        let proposal = Proposal {
            proposer: proposer.clone(),
            target: target.clone(),
            calldata: calldata.clone(),
            approvals: Vec::new(&env),
            approval_weight: 0,
            created_at: now,
            eta: 0,
            signer_generation: get_signer_generation(&env),
            status: ProposalStatus::Proposed,
            executed: false,
            cancelled: false,
        };

        save_proposal(&env, id, &proposal);
        bump_instance(&env);

        env.events().publish(
            (symbol_short!("proposal_created"), id, proposer.clone()),
            ProposalCreated {
                proposal_id: id,
                proposer,
                target,
                created_at: now,
            },
        );

        Ok(id)
    }

    /// Approve a proposal as a registered co-signer.
    ///
    /// Each signer may approve at most once per proposal.  When the accumulated
    /// approval *weight* first reaches the configured threshold, quorum is
    /// recorded (starting the timelock). Because every signer defaults to a
    /// weight of 1, the pre-`#48` headcount behaviour is unchanged unless the
    /// admin assigns weighted powers via [`set_vote_weight`](Self::set_vote_weight).
    ///
    /// # Authorization
    /// - Requires `approver.require_auth()`.
    ///
    /// # Errors
    /// - `NotASigner`: `approver` is not in the registered signers list.
    /// - `ProposalNotFound`: No proposal with this ID.
    /// - `AlreadyExecuted`: Proposal has already been executed.
    /// - `AlreadyApproved`: This signer already approved this proposal.
    /// - `ArithmeticOverflow`: proposal age or quorum timelock deadline cannot
    ///   be represented, or accumulating this signer's vote weight would
    ///   overflow `u64` (the approval is rejected without any state change).
    pub fn approve(env: Env, approver: Address, proposal_id: u32) -> Result<(), GovernanceError> {
        approver.require_auth();

        // O(1) signer membership check via Map index.
        if !Self::is_registered_signer(&env, &approver)? {
            return Err(GovernanceError::NotASigner);
        }

        let mut proposal = load_proposal(&env, proposal_id)?;

        match proposal.status {
            ProposalStatus::Cancelled => return Err(GovernanceError::ProposalCancelled),
            ProposalStatus::Executed => return Err(GovernanceError::AlreadyExecuted),
            ProposalStatus::Queued => return Err(GovernanceError::InvalidProposalState),
            ProposalStatus::Proposed | ProposalStatus::Approved => {}
        }
        if env.ledger().timestamp()
            > checked_deadline(proposal.created_at, MAX_PROPOSAL_AGE_SECONDS)?
        {
            return Err(GovernanceError::ProposalExpired);
        }

        // O(1) duplicate-approval check via per-proposal Map index.
        // The proposal was already loaded, and the guards above reject every
        // terminal state, so it is active here and the #52 policy keeps its
        // storage alive for the rest of the voting window.
        let mut approval_idx = get_approval_index(&env, proposal_id, &proposal.status);
        if approval_idx.contains_key(approver.clone()) {
            return Err(GovernanceError::AlreadyApproved);
        }

        // Accumulate this approver's voting weight with `checked_add` so the
        // running total can never wrap (#48). On overflow the approval is
        // rejected *before* any state is written (all checks precede effects).
        let weight = signer_weight(&env, &approver);
        let accumulated = proposal
            .approval_weight
            .checked_add(weight)
            .ok_or(GovernanceError::ArithmeticOverflow)?;

        proposal.approvals.push_back(approver.clone());
        proposal.approval_weight = accumulated;
        approval_idx.set(approver.clone(), true);
        let approval_count = proposal.approvals.len();

        let threshold = get_threshold(&env)?;
        let now = env.ledger().timestamp();
        let quorum_reached = if accumulated >= threshold as u64 {
            let executable_after = checked_deadline(now, GOVERNANCE_TIMELOCK_SECONDS)?;
            proposal.eta = executable_after;
            proposal.status = ProposalStatus::Queued;
            Some((now, executable_after))
        } else {
            proposal.status = ProposalStatus::Approved;
            None
        };

        save_proposal(&env, proposal_id, &proposal);
        save_approval_index(&env, proposal_id, &approval_idx, &proposal.status);
        bump_instance(&env);

        env.events().publish(
            (symbol_short!("vote_cast"), proposal_id, approver.clone()),
            VoteCast {
                proposal_id,
                voter: approver,
                approval_count,
                timestamp: now,
                approval_weight: proposal.approval_weight,
            },
        );

        // Record the timestamp and effective threshold at which quorum was first
        // reached.  Using the stored snapshot at execution time protects in-flight
        // proposals against mid-flight threshold changes by the admin.
        if let Some((now, executable_after)) = quorum_reached {
            let info = QuorumInfo {
                reached_at: now,
                threshold,
            };
            env.storage()
                .persistent()
                .set(&DataKey::QuorumReachedAt(proposal_id), &info);
            // The proposal was just queued, so it is active: the #52 policy
            // carries the snapshot past the timelock *and* the grace window,
            // which is what `execute` needs from it.
            bump_active_entry(
                &env,
                &DataKey::QuorumReachedAt(proposal_id),
                &proposal.status,
            );

            env.events().publish(
                (symbol_short!("proposal_queued"), proposal_id),
                ProposalQueued {
                    proposal_id,
                    quorum_reached_at: now,
                    executable_after,
                    threshold,
                },
            );
        }

        Ok(())
    }

    /// Execute a proposal that has reached quorum and passed the timelock.
    ///
    /// Marks the proposal as executed and emits `ProposalExecuted`.  The
    /// `target` address and `calldata` are included in the event so that
    /// off-chain executors or indexers can reconstruct and verify the call.
    ///
    /// # Parameters
    /// - `executor`: The address triggering execution (need not be a signer).
    /// - `proposal_id`: The proposal to execute.
    ///
    /// # Authorization
    /// - Requires `executor.require_auth()`.
    ///
    /// # Errors
    /// - `ProposalNotFound`: No proposal with this ID.
    /// - `AlreadyExecuted`: Proposal already executed.
    /// - `QuorumNotReached`: Approval count < threshold.
    /// - `TimelockNotMet`: The current ledger timestamp is before the proposal eta.
    /// - `ArithmeticOverflow`: proposal age or quorum timelock deadline cannot be represented.
    ///
    /// # Security
    /// The execution engine is protected against reentrancy:
    /// 1. Checks-Effects-Interactions: the proposal is marked `executed` (the
    ///    "effect") *before* the untrusted target call (the "interaction"),
    ///    so a reentrant call can never trigger a second execution of the same
    ///    proposal.
    /// 2. Non-reentrancy guard: the `Executing` in-flight flag rejects *any*
    ///    reentrant `execute` — including for a different proposal — while a
    ///    dispatch is in progress.
    ///
    /// # Failure recovery (#53)
    ///
    /// If the dispatched call fails, the whole transaction fails and the host
    /// unwinds it. The proposal is therefore left `Queued` with
    /// `executed == false`, the target is untouched, no `proposal_executed` is
    /// emitted, and the `Executing` flag is cleared — all of which happen
    /// because those writes are part of the same transaction as the target call.
    ///
    /// The practical effect is that a transient downstream failure is retryable:
    /// the proposal stays executable for the remainder of its grace window. A
    /// failure that never clears expires with that window, returning
    /// `ProposalExpired`. See `docs/governance-failure-recovery.md`.
    pub fn execute(env: Env, executor: Address, proposal_id: u32) -> Result<(), GovernanceError> {
        executor.require_auth();

        // SECURITY AUDIT (reentrancy):
        // The execution engine is *not* reentrant. `dispatch_call` invokes
        // arbitrary cross-contract code that could attempt to call back into
        // `execute` to push a second proposal through before the current
        // dispatch returns (a classic governance reentrancy attack). The
        // in-flight `Executing` flag, combined with Checks-Effects-Interactions
        // below, makes double execution impossible:
        //   - CEI: the proposal is marked `executed` *before* any external call.
        //   - Guard: any reentrant `execute` while a dispatch is in progress is
        //     rejected with `ReentrancyGuard` — including for *other* proposals.
        if env
            .storage()
            .instance()
            .get::<DataKey, bool>(&DataKey::Executing)
            .unwrap_or(false)
        {
            return Err(GovernanceError::ReentrancyGuard);
        }

        let mut proposal = load_proposal(&env, proposal_id)?;

        match proposal.status {
            ProposalStatus::Cancelled => return Err(GovernanceError::ProposalCancelled),
            ProposalStatus::Executed => return Err(GovernanceError::AlreadyExecuted),
            ProposalStatus::Proposed | ProposalStatus::Approved => {
                return Err(GovernanceError::QuorumNotReached)
            }
            ProposalStatus::Queued => {}
        }
        if env.ledger().timestamp()
            > checked_deadline(proposal.created_at, MAX_PROPOSAL_AGE_SECONDS)?
        {
            return Err(GovernanceError::ProposalExpired);
        }
        if proposal.signer_generation != get_signer_generation(&env) {
            return Err(GovernanceError::InvalidSignerGeneration);
        }

        // Verify quorum was reached by weight and use the recorded threshold (snapshot
        // at quorum time) so that in-flight proposals are immune to mid-flight
        // threshold changes (#48).
        let quorum_info: QuorumInfo = env
            .storage()
            .persistent()
            .get(&DataKey::QuorumReachedAt(proposal_id))
            .ok_or(GovernanceError::QuorumNotReached)?;
        // Still `Queued` at this point, so the #52 policy keeps the snapshot
        // alive across the dispatch below.
        bump_active_entry(
            &env,
            &DataKey::QuorumReachedAt(proposal_id),
            &proposal.status,
        );

        if proposal.approval_weight < quorum_info.threshold as u64 {
            return Err(GovernanceError::QuorumNotReached);
        }

        // Verify timelock has elapsed from the moment quorum was reached.
        let now = env.ledger().timestamp();
        let exec_after = Self::executable_after(&quorum_info)?;
        if now < exec_after {
            return Err(GovernanceError::TimelockNotMet);
        }

        // Verify the proposal-level eta before dispatching the target call.
        if env.ledger().timestamp() < proposal.eta {
            return Err(GovernanceError::TimelockNotMet);
        }

        // Grace window: a proposal that passed its `eta` more than
        // `grace_period` seconds ago is stale and can no longer be executed
        // (#46). Execution is still allowed exactly at the `eta + grace_period`
        // boundary; the check is strictly `now > deadline`.
        let deadline = checked_deadline(exec_after, get_grace_period(&env))?;
        if now > deadline {
            return Err(GovernanceError::ProposalExpired);
        }

        // CEI: mark as executed before emitting the event.
        proposal.status = ProposalStatus::Executed;
        proposal.executed = true;
        save_proposal(&env, proposal_id, &proposal);
        bump_instance(&env);

        // SECURITY AUDIT (reentrancy):
        // Raise the in-flight flag immediately before the (untrusted) target
        // call and lower it immediately afterwards. If an error/normal return
        // leaves the flag set it is harmless because any later *new* call to
        // `execute` simply observes `Executing == true` and fails the guard —
        // and a panic inside `dispatch_call` reverts the whole transaction,
        // rolling the flag (and every other write) back.
        env.storage()
            .instance()
            .set(&DataKey::Executing, &true);
        let dispatched = dispatch_call(&env, &proposal.target, &proposal.calldata);
        env.storage()
            .instance()
            .set(&DataKey::Executing, &false);
        dispatched?;

        env.events().publish(
            (symbol_short!("proposal_executed"), proposal_id, executor.clone()),
            ProposalExecuted {
                proposal_id,
                executor,
                target: proposal.target.clone(),
                calldata: proposal.calldata.clone(),
                executed_at: now,
            },
        );

        Ok(())
    }

    /// Cancel a proposal, marking it as terminal so no further approvals or
    /// execution are possible.
    ///
    /// # Authorization
    /// - Requires `caller.require_auth()`.
    /// - `caller` must be the original `proposer` or the contract `admin`.
    ///
    /// # Parameters
    /// - `caller`: The address requesting cancellation.
    /// - `proposal_id`: The proposal to cancel.
    ///
    /// # Errors
    /// - `ProposalNotFound`: No proposal with this ID.
    /// - `AlreadyExecuted`: Proposal has already been executed.
    /// - `ProposalCancelled`: Proposal is already cancelled.
    /// - `NotProposerOrAdmin`: `caller` is neither the proposer nor the admin.
    pub fn cancel_proposal(
        env: Env,
        caller: Address,
        proposal_id: u32,
    ) -> Result<(), GovernanceError> {
        caller.require_auth();

        let mut proposal = load_proposal(&env, proposal_id)?;

        match proposal.status {
            ProposalStatus::Executed => return Err(GovernanceError::AlreadyExecuted),
            ProposalStatus::Cancelled => return Err(GovernanceError::ProposalCancelled),
            ProposalStatus::Proposed | ProposalStatus::Approved | ProposalStatus::Queued => {}
        }

        // The original proposer, admin, or a configured emergency guardian may cancel.
        let admin = get_admin(&env)?;
        let guardians = get_emergency_guardians(&env);
        let mut is_guardian = false;
        for i in 0..guardians.len() {
            if guardians.get(i) == Some(caller.clone()) {
                is_guardian = true;
                break;
            }
        }
        if caller != proposal.proposer && caller != admin && !is_guardian {
            return Err(GovernanceError::NotProposerOrAdmin);
        }

        proposal.status = ProposalStatus::Cancelled;
        proposal.cancelled = true;
        save_proposal(&env, proposal_id, &proposal);
        bump_instance(&env);
        let now = env.ledger().timestamp();

        env.events().publish(
            (symbol_short!("proposal_cancelled"), proposal_id, caller.clone()),
            ProposalCancelled {
                proposal_id,
                canceller: caller,
                cancelled_at: now,
            },
        );

        Ok(())
    }

    /// Prune expired, unexecuted proposals from persistent storage to reclaim
    /// ledger rent (#46).
    ///
    /// A proposal is prunable when it is neither executed nor cancelled and its
    /// `timestamp > created_at + MAX_PROPOSAL_AGE_SECONDS` (hard max-age cap) or
    /// its `timestamp > eta + grace_period` (post-eta grace window). The helper
    /// only ever deletes data that is logically dead: such a proposal can never
    /// be approved or executed again.
    ///
    /// Removes the proposal record, its `QuorumInfo` snapshot, and its
    /// per-proposal approval index from persistent storage so indexers and
    /// dashboards no longer pay rent for stale state.
    ///
    /// # Parameters
    /// - `start_id`: First proposal ID to consider (inclusive).
    /// - `limit`: Maximum number of IDs to scan. Hard-capped at
    ///   [`MAX_PAGE_SIZE`]. IDs that do not exist, are executed, or are
    ///   cancelled are skipped silently.
    ///
    /// # Returns
    /// The number of proposals actually removed from storage.
    ///
    /// # Authorization
    /// None — pruning only removes entries that are already non-executable, so
    /// it can be invoked by anyone (e.g. a keeper) without admin approval.
    pub fn prune_expired_proposals(
        env: Env,
        start_id: u32,
        limit: u32,
    ) -> Result<u32, GovernanceError> {
        bump_instance(&env);

        let page_size = limit.min(MAX_PAGE_SIZE);
        let total = read_next_proposal_id(&env);
        if start_id >= total || page_size == 0 {
            return Ok(0u32);
        }

        let end_exclusive = start_id.saturating_add(page_size).min(total);
        let mut pruned = 0u32;
        let mut current = start_id;
        while current < end_exclusive {
            let proposal = match env
                .storage()
                .persistent()
                .get::<DataKey, Proposal>(&DataKey::Proposal(current))
            {
                Some(p) => p,
                None => {
                    current += 1;
                    continue;
                }
            };

            if proposal.executed || proposal.cancelled {
                current += 1;
                continue;
            }

            if !Self::proposal_is_expired(&env, current, &proposal)? {
                current += 1;
                continue;
            }

            env.storage()
                .persistent()
                .remove(&DataKey::Proposal(current));
            env.storage()
                .persistent()
                .remove(&DataKey::QuorumReachedAt(current));
            env.storage()
                .persistent()
                .remove(&DataKey::ProposalApprovalIdx(current));
            pruned += 1;
            current += 1;
        }

        Ok(pruned)
    }

    // -----------------------------------------------------------------------
    // Query entrypoints
    // -----------------------------------------------------------------------

    /// Read a proposal by ID.
    pub fn get_proposal(env: Env, proposal_id: u32) -> Result<Proposal, GovernanceError> {
        load_proposal(&env, proposal_id)
    }

    /// Return the proposal-level execution timestamp (`eta`).
    pub fn get_proposal_eta(env: Env, proposal_id: u32) -> Result<u64, GovernanceError> {
        Ok(load_proposal(&env, proposal_id)?.eta)
    }

    /// Return the configured grace period in seconds.
    ///
    /// Returns `DEFAULT_GRACE_PERIOD_SECONDS` before `init` has been called.
    pub fn grace_period(env: Env) -> u64 {
        bump_instance(&env);
        get_grace_period(&env)
    }

    /// Set the post-`eta` execution window, in seconds.
    ///
    /// # Authorization
    /// - Requires the current admin signature.
    pub fn set_grace_period(env: Env, grace_period: u64) -> Result<(), GovernanceError> {
        get_admin(&env)?.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::GracePeriod, &grace_period);
        bump_instance(&env);
        Ok(())
    }

    /// Return the current lifecycle state of a proposal.
    pub fn get_proposal_status(
        env: Env,
        proposal_id: u32,
    ) -> Result<ProposalStatus, GovernanceError> {
        Ok(load_proposal(&env, proposal_id)?.status)
    }

    /// Return whether a proposal is currently in the requested lifecycle state.
    pub fn is_proposal_in_status(
        env: Env,
        proposal_id: u32,
        expected: ProposalStatus,
    ) -> Result<bool, GovernanceError> {
        Ok(load_proposal(&env, proposal_id)?.status == expected)
    }

    /// Return the number of proposals created so far.
    ///
    /// Proposal IDs are assigned densely starting at 0, so this is also the
    /// exclusive upper bound for enumerating proposals by ID.
    pub fn proposal_count(env: Env) -> u32 {
        bump_instance(&env);
        read_next_proposal_id(&env)
    }

    /// Return the list of registered co-signers.
    pub fn get_signers(env: Env) -> Result<Vec<Address>, GovernanceError> {
        get_signers(&env)
    }

    /// Return the admin address.
    ///
    /// Returns `GovernanceError::NotInitialized` if `init` has not been called.
    pub fn get_admin(env: Env) -> Result<Address, GovernanceError> {
        get_admin(&env)
    }

    /// Return the configured emergency guardian addresses.
    pub fn get_emergency_guardians(env: Env) -> Vec<Address> {
        get_emergency_guardians(&env)
    }

    /// Return the configured approval threshold.
    ///
    /// Returns `GovernanceError::NotInitialized` if `init` has not been called.
    /// For a non-erroring convenience wrapper that returns `0` when
    /// uninitialized, see [`quorum`](Self::quorum).
    pub fn get_threshold(env: Env) -> Result<u32, GovernanceError> {
        get_threshold(&env)
    }

    /// Return the effective approval threshold.
    ///
    /// Convenience alias for [`get_threshold`](Self::get_threshold) that
    /// returns `0` instead of an error when the contract is not initialized.
    pub fn quorum(env: Env) -> u32 {
        get_threshold(&env).unwrap_or(0)
    }

    /// Return the timelock duration in seconds.
    pub fn timelock_seconds(_env: Env) -> u64 {
        GOVERNANCE_TIMELOCK_SECONDS
    }

    /// Return the maximum proposal age in seconds before it expires.
    pub fn max_proposal_age_seconds(_env: Env) -> u64 {
        MAX_PROPOSAL_AGE_SECONDS
    }

    /// Return the stored `QuorumInfo` snapshot for a proposal, or `None` if
    /// quorum has not yet been reached.
    ///
    /// # Parameters
    /// - `proposal_id`: The proposal to query.
    ///
    /// # Returns
    /// - `Some(QuorumInfo { reached_at, threshold })` if quorum was reached.
    /// - `None` if quorum has not been reached (no approvals, below threshold,
    ///   or proposal does not exist).
    ///
    /// This is a pure read — no authorization required, no state mutation
    /// other than the TTL bump applied by the #52 retention policy to the
    /// stored `QuorumInfo` entry (skipped once the proposal is terminal).
    pub fn get_quorum_info(env: Env, proposal_id: u32) -> Option<QuorumInfo> {
        let info: Option<QuorumInfo> = env
            .storage()
            .persistent()
            .get(&DataKey::QuorumReachedAt(proposal_id));
        // A view must not keep a finished proposal's storage alive, so the bump
        // is conditional on the proposal still being active. The status lookup
        // is a single O(1) read of a key we would not otherwise touch here.
        if info.is_some() && !proposal_is_terminal(&env, proposal_id) {
            bump_active_entry(
                &env,
                &DataKey::QuorumReachedAt(proposal_id),
                &ProposalStatus::Queued,
            );
        }
        info
    }

    /// Return `true` if the proposal is in an executable state **right now**.
    ///
    /// Mirrors the exact gating order used by [`execute`](Self::execute):
    ///
    /// 1. Proposal exists (`ProposalNotFound` otherwise).
    /// 2. Not cancelled.
    /// 3. Not already executed.
    /// 4. Not expired.
    /// 5. Quorum has been reached (accumulated approval weight >= threshold snapshot).
    /// 6. Timelock has elapsed (`now >= executable_after`).
    ///
    /// # Parameters
    /// - `proposal_id`: The proposal to check.
    ///
    /// # Returns
    /// - `Ok(true)` iff all gates pass — the proposal can be executed now.
    /// - `Ok(false)` if any gate blocks execution (cancelled, executed,
    ///   expired, quorum not reached, timelock not elapsed).
    /// - `Err(GovernanceError::ProposalNotFound)` if the ID is unknown.
    /// - `Err(GovernanceError::ArithmeticOverflow)` if timelock arithmetic
    ///   overflows (should not happen under normal ledger conditions).
    ///
    /// This is a pure read — no authorization required, no state mutation
    /// beyond the TTL bumps already performed by [`load_proposal`] and
    /// [`get_quorum_info`].
    pub fn is_executable(env: Env, proposal_id: u32) -> Result<bool, GovernanceError> {
        let proposal = load_proposal(&env, proposal_id)?;

        match proposal.status {
            ProposalStatus::Cancelled | ProposalStatus::Executed => return Ok(false),
            ProposalStatus::Proposed | ProposalStatus::Approved => return Ok(false),
            ProposalStatus::Queued => {}
        }
        if env.ledger().timestamp()
            > checked_deadline(proposal.created_at, MAX_PROPOSAL_AGE_SECONDS)?
        {
            return Ok(false);
        }
        if proposal.signer_generation != get_signer_generation(&env) {
            return Ok(false);
        }

        let quorum_info: QuorumInfo = match env
            .storage()
            .persistent()
            .get(&DataKey::QuorumReachedAt(proposal_id))
        {
            // `Queued` is the only status that reaches this point, so the #52
            // policy keeps the snapshot alive for this read.
            Some(info) => {
                bump_active_entry(
                    &env,
                    &DataKey::QuorumReachedAt(proposal_id),
                    &proposal.status,
                );
                info
            }
            None => return Ok(false),
        };

        if proposal.approval_weight < quorum_info.threshold as u64 {
            return Ok(false);
        }

        let now = env.ledger().timestamp();
        let exec_after = Self::executable_after(&quorum_info)?;
        if now < exec_after || now < proposal.eta {
            return Ok(false);
        }

        // Mirror of the grace-window gate in [execute](Self::execute).
        let deadline = checked_deadline(exec_after, get_grace_period(&env))?;
        if now > deadline {
            return Ok(false);
        }

        Ok(true)
    }

    /// Return `true` if `signer` is a registered co-signer of the governance
    /// contract.
    ///
    /// Cheap O(1) membership probe over `DataKey::SignerIndex` (the same
    /// `Map<Address, bool>` index consulted internally by `propose`,
    /// `approve`, `add_signer`, and `remove_signer`). Lets off-chain tooling
    /// and cross-contract callers verify signer membership without
    /// downloading the entire signer list via `get_signers` (which is O(n)
    /// on the wire and O(n) on the receiving side).
    ///
    /// # Parameters
    /// - `signer`: The address to test for membership.
    ///
    /// # Returns
    /// - `true` if `signer` is in the current co-signer set.
    /// - `false` if `signer` is not a co-signer, has been removed by
    ///   `remove_signer`, or the contract has not been initialised.
    ///
    /// # Pre-init behaviour
    /// Returns `false` (no panic, no error) when `init` has not been called.
    /// This is a deliberate design choice: callers should be able to use
    /// `is_signer` as a safe "is this address a potential signer?" probe
    /// before reading other governance state, without first having to call
    /// `get_admin` to check initialisation.
    ///
    /// # Security
    /// - Pure read — no `require_auth`, no state mutation.
    /// - Does **not** extend any TTL. Instance storage is kept alive by
    ///   every state-mutating entrypoint (`init`, `add_signer`,
    ///   `remove_signer`, `set_admin`, `propose`, `approve`, `execute`,
    ///   `cancel_proposal`) via `bump_instance`. Letting `is_signer`
    ///   extend TTL would let a third party keep a stale `SignerIndex`
    ///   alive by repeatedly polling, which is unnecessary and would
    ///   cost the network rent on every call.
    /// - Reuses [`get_signer_index`], the same helper that backs the
    ///   duplicate-prevention paths in `add_signer` and the membership
    ///   check in `remove_signer`. There is no second source of truth;
    ///   any address reported by `is_signer` is also reported as a
    ///   duplicate by `add_signer` and as removable by `remove_signer`.
    pub fn is_signer(env: Env, signer: Address) -> bool {
        get_signer_index(&env)
            .map(|index| index.contains_key(signer))
            .unwrap_or(false)
    }

    /// Return a bounded page of proposals whose IDs fall in `[start_id, start_id + limit)`.
    ///
    /// This mirrors `FluxoraStream::get_streams_by_id_range` and is the primary
    /// entrypoint for dashboard or migration tooling that needs to enumerate
    /// governance history without issuing one RPC per proposal.
    ///
    /// # Parameters
    /// - `start_id`: First proposal ID to include (inclusive).
    /// - `limit`: Maximum number of proposals to return.  Hard-capped at
    ///   [`MAX_PAGE_SIZE`] regardless of the value supplied by the caller.
    ///   Passing a value above the cap is **not** an error; the cap is silently
    ///   applied.  Passing `0` returns an empty `Vec`.
    ///
    /// # Returns
    /// A `Vec<Proposal>` containing at most `min(limit, MAX_PAGE_SIZE)` entries.
    /// IDs for proposals that were cancelled, executed, or never created (i.e.
    /// storage entries that do not exist) are silently skipped — the caller
    /// receives only the proposals that are present in storage.  The returned
    /// slice preserves ascending ID order.
    ///
    /// # DoS protection
    ///
    /// `limit` is hard-capped at `MAX_PAGE_SIZE` (100).  The cap is enforced
    /// before any storage reads; it is impossible to exceed via any call path.
    /// Callers should page by advancing `start_id` to `start_id + limit` on
    /// each successive call.
    ///
    /// # Example
    ///
    /// ```text
    /// // Page 1: proposals 0-99
    /// get_proposals_by_id_range(env, 0, 100)
    /// // Page 2: proposals 100-199
    /// get_proposals_by_id_range(env, 100, 100)
    /// ```
    pub fn get_proposals_by_id_range(env: Env, start_id: u32, limit: u32) -> Vec<Proposal> {
        bump_instance(&env);

        // Hard-cap: silently clamp to MAX_PAGE_SIZE. This is the sole read-DoS
        // control — it must be applied before any storage iteration.
        let page_size = limit.min(MAX_PAGE_SIZE);

        let mut result = Vec::new(&env);

        // Zero limit or uninitialized contract (no proposals yet) → empty.
        if page_size == 0 {
            return result;
        }

        // The monotonic counter is the exclusive upper bound for valid IDs.
        let total = read_next_proposal_id(&env);
        if start_id >= total {
            return result;
        }

        // Iterate [start_id, start_id + page_size) ∩ [0, total).
        // Use saturating_add so an extreme start_id + page_size cannot wrap.
        let end_exclusive = start_id.saturating_add(page_size).min(total);
        let mut current = start_id;
        while current < end_exclusive {
            // Missing IDs (cancelled proposals whose storage was never pruned
            // still exist, but genuinely absent keys — e.g. IDs that were
            // reserved and then rolled back — are skipped silently).
            if let Some(proposal) = env
                .storage()
                .persistent()
                .get::<DataKey, Proposal>(&DataKey::Proposal(current))
            {
                // Paging is a bulk read, so it is exactly the kind of call that
                // would otherwise hold every historical proposal alive forever.
                // The #52 policy bumps only the active ones.
                bump_active_entry(&env, &DataKey::Proposal(current), &proposal.status);
                result.push_back(proposal);
            }
            current += 1;
        }

        result
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Compute the ledger timestamp at which a proposal becomes executable,
    /// given its `QuorumInfo` snapshot.
    ///
    /// Returns `reached_at + GOVERNANCE_TIMELOCK_SECONDS`, or
    /// `ArithmeticOverflow` if the sum would overflow `u64`.
    fn executable_after(info: &QuorumInfo) -> Result<u64, GovernanceError> {
        checked_deadline(info.reached_at, GOVERNANCE_TIMELOCK_SECONDS)
    }

    /// O(1) signer membership check via the Map index stored in instance storage.
    ///
    /// Returns `Err(NotInitialized)` if the contract has not been initialised.
    /// Used by `propose` and `approve` where the caller must be a registered
    /// co-signer (and where calling on an uninitialised contract is itself an
    /// error condition surfaced to the caller).
    fn is_registered_signer(env: &Env, addr: &Address) -> Result<bool, GovernanceError> {
        let index = get_signer_index(env)?;
        Ok(index.contains_key(addr.clone()))
    }

    /// Returns `true` when a proposal has passed its execution window and is
    /// logically dead (non-executable, eligible for pruning).
    ///
    /// Mirrors the two expiry gates enforced by [`execute`](Self::execute):
    /// 1. `now > created_at + MAX_PROPOSAL_AGE_SECONDS` (hard max-age cap).
    /// 2. `now > eta + grace_period` once quorum has been reached.
    ///
    /// Any `ArithmeticOverflow` bubbles up to the caller; callers that already
    /// loaded the proposal pass it in so we avoid a redundant storage read.
    fn proposal_is_expired(
        env: &Env,
        id: u32,
        proposal: &Proposal,
    ) -> Result<bool, GovernanceError> {
        let now = env.ledger().timestamp();
        if now > checked_deadline(proposal.created_at, MAX_PROPOSAL_AGE_SECONDS)? {
            return Ok(true);
        }
        if let Some(info) = env
            .storage()
            .persistent()
            .get::<DataKey, QuorumInfo>(&DataKey::QuorumReachedAt(id))
        {
            let eta = checked_deadline(info.reached_at, GOVERNANCE_TIMELOCK_SECONDS)?;
            let deadline = checked_deadline(eta, get_grace_period(env))?;
            if now > deadline {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events, Ledger};
    use soroban_sdk::{vec, Env, TryFromVal, Val, Vec as SVec};

    const TIMELOCK: u64 = 172_800;
    const MAX_AGE: u64 = 2_592_000;

    struct Ctx {
        env: Env,
        #[allow(dead_code)]
        contract_id: Address,
        admin: Address,
        signer_a: Address,
        signer_b: Address,
        #[allow(dead_code)]
        signer_c: Address,
        client: FluxoraGovernanceClient<'static>,
    }

    impl Ctx {
        fn setup() -> Self {
            let env = Env::default();
            env.mock_all_auths();
            env.ledger().set_timestamp(1_000_000);

            let contract_id = env.register_contract(None, FluxoraGovernance);
            let admin = Address::generate(&env);
            let signer_a = Address::generate(&env);
            let signer_b = Address::generate(&env);
            let signer_c = Address::generate(&env);

            let client = FluxoraGovernanceClient::new(&env, &contract_id);
            client.init(
                &admin,
                &vec![&env, signer_a.clone(), signer_b.clone(), signer_c.clone()],
                &2u32,
            );

            Ctx {
                env,
                contract_id,
                admin,
                signer_a,
                signer_b,
                signer_c,
                client,
            }
        }

        fn dummy_target(&self) -> Address {
            Address::generate(&self.env)
        }

        /// Returns XDR-encoded `CallData::Noop`. The `_tag` parameter is
        /// accepted only to keep call-sites readable; it has no effect on the
        /// returned bytes.
        fn calldata(&self, _tag: &str) -> Bytes {
            use soroban_sdk::xdr::ToXdr;
            CallData::Noop.to_xdr(&self.env)
        }
    }

    fn last_contract_event(env: &Env, contract_id: &Address) -> (Symbol, Val) {
        use soroban_sdk::xdr::ContractEventBody;
        let contract_events = env.events().all().filter_by_contract(contract_id);
        let events = contract_events.events();
        for event in events.iter().rev() {
            let ContractEventBody::V0(body) = &event.body;
            let topic: Val = body
                .topics
                .first()
                .map(|t| Val::try_from_val(env, t).expect("topic converts"))
                .expect("event has a topic");
            let symbol = Symbol::try_from_val(env, &topic).expect("topic is a symbol");
            let data: Val = Val::try_from_val(env, &body.data).expect("data converts");
            return (symbol, data);
        }

        panic!("no event emitted by the contract");
    }

    // -----------------------------------------------------------------------
    // CallData dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn test_invalid_calldata_errors_on_execute() {
        let ctx = Ctx::setup();
        // XDR bytes that deserialize but are not a CallData variant.  Encode a
        // plain u32 — it deserialises fine but `CallData::try_from_val` will
        // reject the type, surfacing as `InvalidCalldata`.
        use soroban_sdk::xdr::ToXdr;
        let bad = 42_u32.to_xdr(&ctx.env);
        let id = ctx.client.propose(&ctx.signer_a, &ctx.dummy_target(), &bad);
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&ctx.env);
        let result = ctx.client.try_execute(&executor, &id);
        assert_eq!(result, Err(Ok(GovernanceError::InvalidCalldata)));
        // Proposal must NOT be marked executed after a failed calldata decode.
        let p = ctx.client.get_proposal(&id);
        assert!(!p.executed);
    }

    #[test]
    fn test_noop_calldata_executes_cleanly() {
        use soroban_sdk::xdr::ToXdr;
        let ctx = Ctx::setup();
        let noop = CallData::Noop.to_xdr(&ctx.env);
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &noop);
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&ctx.env);
        ctx.client.execute(&executor, &id);
        assert!(ctx.client.get_proposal(&id).executed);
    }

    #[test]
    fn test_stream_global_resume_and_bulk_resume_calldata_xdr_round_trip() {
        use soroban_sdk::xdr::{FromXdr, ToXdr};
        let ctx = Ctx::setup();

        let resume = CallData::StreamGlobalResume.to_xdr(&ctx.env);
        let decoded = CallData::from_xdr(&ctx.env, &resume).expect("StreamGlobalResume XDR");
        assert!(matches!(decoded, CallData::StreamGlobalResume));

        let ids = vec![&ctx.env, 1u64, 2u64, 9u64];
        let bulk = CallData::StreamBulkResumeAsAdmin(ids.clone()).to_xdr(&ctx.env);
        let decoded_bulk =
            CallData::from_xdr(&ctx.env, &bulk).expect("StreamBulkResumeAsAdmin XDR");
        match decoded_bulk {
            CallData::StreamBulkResumeAsAdmin(got) => {
                assert_eq!(got.len(), 3);
                assert_eq!(got.get(0).unwrap(), 1);
                assert_eq!(got.get(1).unwrap(), 2);
                assert_eq!(got.get(2).unwrap(), 9);
            }
            other => panic!("unexpected variant: {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Cross-contract dispatch — governance driving factory policy (#38)
    // -----------------------------------------------------------------------

    #[test]
    fn test_governance_executes_factory_policy_update_cross_contract() {
        use soroban_sdk::xdr::ToXdr;
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);

        // Governance contract: 3 signers, threshold 2.
        let gov_id = env.register(FluxoraGovernance, ());
        let gov = FluxoraGovernanceClient::new(&env, &gov_id);
        let admin = Address::generate(&env);
        let signer_a = Address::generate(&env);
        let signer_b = Address::generate(&env);
        let signer_c = Address::generate(&env);
        gov.init(
            &admin,
            &vec![&env, signer_a.clone(), signer_b.clone(), signer_c.clone()],
            &2u32,
        );

        // Factory contract whose admin is the governance contract.
        let factory_id = env.register(fluxora_factory::FluxoraFactory, ());
        let factory = fluxora_factory::FluxoraFactoryClient::new(&env, &factory_id);
        let stream_contract = Address::generate(&env);
        factory.init(&gov_id, &stream_contract, &10_000, &100);
        assert_eq!(factory.get_factory_config().max_deposit, 10_000);

        // Queue a governed `FactorySetCap(42)` proposal targeted at the factory
        // and drive it through the full multi-sig lifecycle in the test host.
        let calldata = CallData::FactorySetCap(42).to_xdr(&env);
        let id = gov.propose(&signer_a, &factory_id, &calldata);
        gov.approve(&signer_a, &id);
        gov.approve(&signer_b, &id);

        env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&env);
        gov.execute(&executor, &id);

        // The cross-contract dispatch reached the factory and applied the
        // policy update; execution is recorded on the proposal.
        assert_eq!(factory.get_factory_config().max_deposit, 42);
        assert!(gov.get_proposal(&id).executed);
    }

    #[test]
    fn test_governance_rotates_factory_admin_cross_contract() {
        use soroban_sdk::xdr::ToXdr;
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);

        let gov_id = env.register(FluxoraGovernance, ());
        let gov = FluxoraGovernanceClient::new(&env, &gov_id);
        let admin = Address::generate(&env);
        let signer_a = Address::generate(&env);
        let signer_b = Address::generate(&env);
        let signer_c = Address::generate(&env);
        gov.init(
            &admin,
            &vec![&env, signer_a.clone(), signer_b.clone(), signer_c.clone()],
            &2u32,
        );

        let factory_id = env.register(fluxora_factory::FluxoraFactory, ());
        let factory = fluxora_factory::FluxoraFactoryClient::new(&env, &factory_id);
        let stream_contract = Address::generate(&env);
        factory.init(&gov_id, &stream_contract, &10_000, &100);

        // Governance rotates the factory admin via the generic dispatch path.
        let new_admin = Address::generate(&env);
        let calldata = CallData::FactorySetAdmin(new_admin.clone()).to_xdr(&env);
        let id = gov.propose(&signer_a, &factory_id, &calldata);
        gov.approve(&signer_a, &id);
        gov.approve(&signer_b, &id);
        env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&env);
        gov.execute(&executor, &id);

        assert_eq!(factory.get_factory_config().admin, new_admin);
    }

    #[test]
    fn test_quorum_and_timelock_constants() {
        let ctx = Ctx::setup();
        assert_eq!(ctx.client.quorum(), 2);
        assert_eq!(ctx.client.timelock_seconds(), TIMELOCK);
    }

    #[test]
    fn test_max_proposal_age_constant() {
        let ctx = Ctx::setup();
        assert_eq!(ctx.client.max_proposal_age_seconds(), MAX_AGE);
    }

    // -----------------------------------------------------------------------
    // get_admin / get_threshold views
    // -----------------------------------------------------------------------

    #[test]
    fn test_get_admin_after_init() {
        let ctx = Ctx::setup();
        let admin = ctx.client.get_admin();
        assert_eq!(admin, ctx.admin);
    }

    #[test]
    fn test_get_admin_pre_init() {
        let env = Env::default();
        let contract_id = env.register_contract(None, FluxoraGovernance);
        let client = FluxoraGovernanceClient::new(&env, &contract_id);
        let result = client.try_get_admin();
        assert_eq!(result, Err(Ok(GovernanceError::NotInitialized)));
    }

    #[test]
    fn test_get_threshold_after_init() {
        let ctx = Ctx::setup();
        let threshold = ctx.client.get_threshold();
        assert_eq!(threshold, 2);
    }

    #[test]
    fn test_get_threshold_pre_init() {
        let env = Env::default();
        let contract_id = env.register_contract(None, FluxoraGovernance);
        let client = FluxoraGovernanceClient::new(&env, &contract_id);
        let result = client.try_get_threshold();
        assert_eq!(result, Err(Ok(GovernanceError::NotInitialized)));
    }

    // -----------------------------------------------------------------------
    // Threshold validation
    // -----------------------------------------------------------------------

    #[test]
    fn test_init_rejects_zero_threshold() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);
        let contract_id = env.register_contract(None, FluxoraGovernance);
        let admin = Address::generate(&env);
        let signer = Address::generate(&env);
        let client = FluxoraGovernanceClient::new(&env, &contract_id);
        let result = client.try_init(&admin, &vec![&env, signer], &0u32);
        assert_eq!(result, Err(Ok(GovernanceError::InvalidThreshold)));
    }

    #[test]
    fn test_init_rejects_threshold_above_signer_count() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);
        let contract_id = env.register_contract(None, FluxoraGovernance);
        let admin = Address::generate(&env);
        let signer_a = Address::generate(&env);
        let signer_b = Address::generate(&env);
        let client = FluxoraGovernanceClient::new(&env, &contract_id);
        // 2 signers but threshold = 3
        let result = client.try_init(&admin, &vec![&env, signer_a, signer_b], &3u32);
        assert_eq!(result, Err(Ok(GovernanceError::InvalidThreshold)));
    }

    #[test]
    fn test_init_accepts_threshold_equal_to_signer_count() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);
        let contract_id = env.register_contract(None, FluxoraGovernance);
        let admin = Address::generate(&env);
        let signer_a = Address::generate(&env);
        let signer_b = Address::generate(&env);
        let client = FluxoraGovernanceClient::new(&env, &contract_id);
        let result = client.try_init(&admin, &vec![&env, signer_a, signer_b], &2u32);
        assert!(result.is_ok());
        assert_eq!(client.quorum(), 2);
    }

    #[test]
    fn test_init_accepts_threshold_of_one() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);
        let contract_id = env.register_contract(None, FluxoraGovernance);
        let admin = Address::generate(&env);
        let signer = Address::generate(&env);
        let client = FluxoraGovernanceClient::new(&env, &contract_id);
        let result = client.try_init(&admin, &vec![&env, signer], &1u32);
        assert!(result.is_ok());
        assert_eq!(client.quorum(), 1);
    }

    #[test]
    fn test_set_threshold_updates_value_and_emits_event() {
        let ctx = Ctx::setup(); // 3 signers, threshold=2

        ctx.client.set_threshold(&3u32);

        let (topic, data) = last_contract_event(&ctx.env, &ctx.contract_id);
        assert_eq!(ctx.client.get_threshold(), 3);
        assert_eq!(topic, symbol_short!("thr_upd"));
        let payload =
            ThresholdUpdated::try_from_val(&ctx.env, &data).expect("decodes to ThresholdUpdated");
        assert_eq!(payload.old_threshold, 2);
        assert_eq!(payload.new_threshold, 3);
    }

    #[test]
    fn test_set_threshold_rejects_zero() {
        let ctx = Ctx::setup();
        let events_before = ctx.env.events().all().events().len();

        let result = ctx.client.try_set_threshold(&0u32);

        assert_eq!(result, Err(Ok(GovernanceError::InvalidThreshold)));
        assert_eq!(ctx.client.get_threshold(), 2);
        assert_eq!(ctx.env.events().all().events().len(), events_before);
    }

    #[test]
    fn test_set_threshold_rejects_above_signer_count() {
        let ctx = Ctx::setup(); // 3 signers
        let events_before = ctx.env.events().all().events().len();

        let result = ctx.client.try_set_threshold(&4u32);

        assert_eq!(result, Err(Ok(GovernanceError::InvalidThreshold)));
        assert_eq!(ctx.client.get_threshold(), 2);
        assert_eq!(ctx.env.events().all().events().len(), events_before);
    }

    #[test]
    fn test_set_threshold_accepts_one() {
        let ctx = Ctx::setup();

        ctx.client.set_threshold(&1u32);

        assert_eq!(ctx.client.get_threshold(), 1);
    }

    #[test]
    fn test_set_threshold_accepts_valid_range() {
        let ctx = Ctx::setup(); // 3 signers, threshold=2
                                // Set to 1 (valid: 1 <= 1 <= 3)
        ctx.client.set_threshold(&1u32);
        assert_eq!(ctx.client.get_threshold(), 1);
        // Set to 3 (valid: 1 <= 3 <= 3)
        ctx.client.set_threshold(&3u32);
        assert_eq!(ctx.client.get_threshold(), 3);
        // Set back to 2 (valid: 1 <= 2 <= 3)
        ctx.client.set_threshold(&2u32);
        assert_eq!(ctx.client.get_threshold(), 2);
    }

    #[test]
    fn test_set_threshold_requires_admin_auth() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let contract_id = env.register_contract(None, FluxoraGovernance);
        let admin = Address::generate(&env);
        let signer_a = Address::generate(&env);
        let signer_b = Address::generate(&env);
        let signer_c = Address::generate(&env);
        let client = FluxoraGovernanceClient::new(&env, &contract_id);
        client.init(&admin, &vec![&env, signer_a, signer_b, signer_c], &2u32);
        // Without mock_all_auths, require_auth() on the admin address fails at
        // the host level (HostError / Abort) because the caller has not
        // authorized as the admin.  This verifies that set_threshold does
        // enforce admin authorization.
        let result = client.try_set_threshold(&1u32);
        assert!(
            result.is_err(),
            "set_threshold should abort without admin auth"
        );
        // Verify threshold is unchanged.
        assert_eq!(client.get_threshold(), 2);
    }

    #[test]
    fn test_set_threshold_after_signer_removal_respects_current_count() {
        let ctx = Ctx::setup(); // 3 signers, threshold=2
        ctx.client.remove_signer(&ctx.signer_c); // Now 2 signers
                                                 // Setting threshold to 2 should succeed (2 <= 2)
        ctx.client.set_threshold(&2u32);
        assert_eq!(ctx.client.get_threshold(), 2);
        // Setting threshold to 3 should fail (3 > 2)
        let result = ctx.client.try_set_threshold(&3u32);
        assert_eq!(result, Err(Ok(GovernanceError::InvalidThreshold)));
    }

    // -----------------------------------------------------------------------
    // update_threshold — dynamic threshold updates (issue #42)
    // -----------------------------------------------------------------------

    #[test]
    fn test_update_threshold_updates_value_and_emits_event() {
        let ctx = Ctx::setup(); // 3 signers, threshold=2

        ctx.client.update_threshold(&3u32);

        let (topic, data) = last_contract_event(&ctx.env, &ctx.contract_id);
        assert_eq!(ctx.client.quorum(), 3);
        assert_eq!(topic, symbol_short!("quor_cfg"));
        let payload = QuorumConfig::try_from_val(&ctx.env, &data).expect("decodes to QuorumConfig");
        assert_eq!(payload.threshold, 3);
        assert_eq!(payload.signer_count, 3);
    }

    #[test]
    fn test_update_threshold_rejects_zero() {
        let ctx = Ctx::setup();
        let events_before = ctx.env.events().all().events().len();

        let result = ctx.client.try_update_threshold(&0u32);

        assert_eq!(result, Err(Ok(GovernanceError::InvalidThreshold)));
        assert_eq!(ctx.client.quorum(), 2);
        assert_eq!(ctx.env.events().all().events().len(), events_before);
    }

    #[test]
    fn test_update_threshold_rejects_above_signer_count() {
        let ctx = Ctx::setup(); // 3 signers, threshold=2
        let events_before = ctx.env.events().all().events().len();

        let result = ctx.client.try_update_threshold(&4u32);

        assert_eq!(result, Err(Ok(GovernanceError::InvalidThreshold)));
        assert_eq!(ctx.client.quorum(), 2);
        assert_eq!(ctx.env.events().all().events().len(), events_before);
    }

    #[test]
    fn test_update_threshold_accepts_one() {
        let ctx = Ctx::setup();

        ctx.client.update_threshold(&1u32);

        assert_eq!(ctx.client.quorum(), 1);
    }

    #[test]
    fn test_update_threshold_accepts_valid_range() {
        let ctx = Ctx::setup(); // 3 signers, threshold=2
        ctx.client.update_threshold(&1u32);
        assert_eq!(ctx.client.quorum(), 1);
        ctx.client.update_threshold(&3u32);
        assert_eq!(ctx.client.quorum(), 3);
        ctx.client.update_threshold(&2u32);
        assert_eq!(ctx.client.quorum(), 2);
    }

    #[test]
    fn test_update_threshold_requires_admin_auth() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let contract_id = env.register_contract(None, FluxoraGovernance);
        let admin = Address::generate(&env);
        let signer_a = Address::generate(&env);
        let signer_b = Address::generate(&env);
        let signer_c = Address::generate(&env);
        let client = FluxoraGovernanceClient::new(&env, &contract_id);
        client.init(&admin, &vec![&env, signer_a, signer_b, signer_c], &2u32);

        // No mock_all_auths: require_auth() on the admin address fails at the
        // host level, so the threshold must remain untouched.
        let result = client.try_update_threshold(&3u32);
        assert!(
            result.is_err(),
            "update_threshold should abort without admin auth"
        );
        assert_eq!(client.quorum(), 2);
    }

    #[test]
    fn test_update_threshold_after_signer_removal_respects_current_count() {
        let ctx = Ctx::setup(); // 3 signers, threshold=2
        ctx.client.remove_signer(&ctx.signer_c); // Now 2 signers
        ctx.client.update_threshold(&2u32);
        assert_eq!(ctx.client.quorum(), 2);
        // 3 > 2 remaining signers — must be rejected.
        let result = ctx.client.try_update_threshold(&3u32);
        assert_eq!(result, Err(Ok(GovernanceError::InvalidThreshold)));
        assert_eq!(ctx.client.quorum(), 2);
    }

    // -----------------------------------------------------------------------
    // Quorum invariant on remove_signer
    // -----------------------------------------------------------------------

    #[test]
    fn test_remove_signer_down_to_threshold_succeeds() {
        let ctx = Ctx::setup(); // 3 signers, threshold=2
                                // After removing signer_c, we have 2 signers == threshold — should succeed.
        ctx.client.remove_signer(&ctx.signer_c);
        let signers = ctx.client.get_signers();
        assert_eq!(signers.len(), 2);
        // quorum still 2, which is <= signers.len() — invariant holds.
        assert_eq!(ctx.client.quorum(), 2);
    }

    #[test]
    fn test_remove_signer_below_threshold_errors() {
        let ctx = Ctx::setup(); // 3 signers, threshold=2
        ctx.client.remove_signer(&ctx.signer_c); // 2 signers left
                                                 // Trying to remove another signer would leave 1 < threshold=2
        let result = ctx.client.try_remove_signer(&ctx.signer_b);
        assert_eq!(result, Err(Ok(GovernanceError::QuorumWouldBreak)));
        // Verify signer set is unchanged.
        let signers = ctx.client.get_signers();
        assert_eq!(signers.len(), 2);
    }

    #[test]
    fn test_remove_signer_nonexistent_does_not_break_quorum() {
        let ctx = Ctx::setup(); // 3 signers, threshold=2
        let stranger = Address::generate(&ctx.env);
        // Removing a non-existent signer should be a no-op, not an error.
        let result = ctx.client.try_remove_signer(&stranger);
        assert!(result.is_ok());
        let signers = ctx.client.get_signers();
        assert_eq!(signers.len(), 3);
    }

    // -----------------------------------------------------------------------
    // Proposal creation
    // -----------------------------------------------------------------------

    #[test]
    fn test_propose_returns_incremental_ids() {
        let ctx = Ctx::setup();
        let id0 = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("a"));
        let id1 = ctx
            .client
            .propose(&ctx.signer_b, &ctx.dummy_target(), &ctx.calldata("b"));
        assert_eq!(id0, 0);
        assert_eq!(id1, 1);
    }

    #[test]
    fn test_propose_stores_proposal() {
        let ctx = Ctx::setup();
        let target = ctx.dummy_target();
        let data = ctx.calldata("set_cap:5000");
        let id = ctx.client.propose(&ctx.signer_a, &target, &data);
        let p = ctx.client.get_proposal(&id);
        assert_eq!(p.proposer, ctx.signer_a);
        assert_eq!(p.target, target);
        assert_eq!(p.status, ProposalStatus::Proposed);
        assert!(!p.executed);
        assert!(!p.cancelled);
        assert_eq!(p.approvals.len(), 0);
    }

    #[test]
    fn test_proposal_status_lifecycle_and_illegal_transitions() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("lifecycle"));
        assert_eq!(ctx.client.get_proposal_status(&id), ProposalStatus::Proposed);
        assert!(ctx.client.is_proposal_in_status(&id, &ProposalStatus::Proposed));

        ctx.client.approve(&ctx.signer_a, &id);
        assert_eq!(ctx.client.get_proposal_status(&id), ProposalStatus::Approved);

        ctx.client.approve(&ctx.signer_b, &id);
        assert_eq!(ctx.client.get_proposal_status(&id), ProposalStatus::Queued);
        assert_eq!(
            ctx.client.try_approve(&ctx.signer_c, &id),
            Err(Ok(GovernanceError::InvalidProposalState))
        );

        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK);
        ctx.client.execute(&Address::generate(&ctx.env), &id);
        assert_eq!(ctx.client.get_proposal_status(&id), ProposalStatus::Executed);
        assert_eq!(
            ctx.client.try_execute(&Address::generate(&ctx.env), &id),
            Err(Ok(GovernanceError::AlreadyExecuted))
        );

        let cancelled_id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("cancel"));
        ctx.client.cancel_proposal(&ctx.signer_a, &cancelled_id);
        assert_eq!(
            ctx.client.get_proposal_status(&cancelled_id),
            ProposalStatus::Cancelled
        );
        assert_eq!(
            ctx.client.try_approve(&ctx.signer_b, &cancelled_id),
            Err(Ok(GovernanceError::ProposalCancelled))
        );
        assert_eq!(
            ctx.client.try_execute(&Address::generate(&ctx.env), &cancelled_id),
            Err(Ok(GovernanceError::ProposalCancelled))
        );
    }

    #[test]
    fn test_status_helpers_reject_unknown_proposal() {
        let ctx = Ctx::setup();
        assert_eq!(
            ctx.client.try_get_proposal_status(&99),
            Err(Ok(GovernanceError::ProposalNotFound))
        );
        assert_eq!(
            ctx.client.try_is_proposal_in_status(&99, &ProposalStatus::Proposed),
            Err(Ok(GovernanceError::ProposalNotFound))
        );
    }

    #[test]
    fn test_propose_returns_structured_error_when_proposal_id_counter_overflows() {
        let ctx = Ctx::setup();
        ctx.env.as_contract(&ctx.contract_id, || {
            ctx.env
                .storage()
                .instance()
                .set(&DataKey::NextProposalId, &u32::MAX);
        });

        let result = ctx.client.try_propose(
            &ctx.signer_a,
            &ctx.dummy_target(),
            &ctx.calldata("overflow"),
        );

        assert_eq!(result, Err(Ok(GovernanceError::ArithmeticOverflow)));
        ctx.env.as_contract(&ctx.contract_id, || {
            assert_eq!(read_next_proposal_id(&ctx.env), u32::MAX);
        });
    }

    #[test]
    fn test_approve_returns_structured_error_when_quorum_timelock_overflows() {
        let ctx = Ctx::setup();
        ctx.env.ledger().set_timestamp(u64::MAX - MAX_AGE);
        let id = ctx.client.propose(
            &ctx.signer_a,
            &ctx.dummy_target(),
            &ctx.calldata("timelock"),
        );

        ctx.client.approve(&ctx.signer_a, &id);
        ctx.env.ledger().set_timestamp(u64::MAX - TIMELOCK + 1);

        let result = ctx.client.try_approve(&ctx.signer_b, &id);

        assert_eq!(result, Err(Ok(GovernanceError::ArithmeticOverflow)));
    }

    #[test]
    fn test_execute_returns_structured_error_when_quorum_timelock_overflows() {
        let ctx = Ctx::setup();
        ctx.env.ledger().set_timestamp(u64::MAX - MAX_AGE);
        let id = ctx.client.propose(
            &ctx.signer_a,
            &ctx.dummy_target(),
            &ctx.calldata("timelock"),
        );
        let mut proposal = ctx.client.get_proposal(&id);
        proposal.approvals.push_back(ctx.signer_a.clone());
        proposal.approvals.push_back(ctx.signer_b.clone());
        ctx.env.as_contract(&ctx.contract_id, || {
            save_proposal(&ctx.env, id, &proposal);
            ctx.env.storage().persistent().set(
                &DataKey::QuorumReachedAt(id),
                &QuorumInfo {
                    reached_at: u64::MAX - TIMELOCK + 1,
                    threshold: 2,
                },
            );
        });
        ctx.env.ledger().set_timestamp(u64::MAX - 100);
        let executor = Address::generate(&ctx.env);

        let result = ctx.client.try_execute(&executor, &id);

        assert_eq!(result, Err(Ok(GovernanceError::ArithmeticOverflow)));
    }

    // -----------------------------------------------------------------------
    // Cancellation
    // -----------------------------------------------------------------------

    #[test]
    fn test_cancel_by_proposer_succeeds() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.cancel_proposal(&ctx.signer_a, &id);
        let p = ctx.client.get_proposal(&id);
        assert!(p.cancelled);
    }

    #[test]
    fn test_cancel_by_admin_succeeds() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.cancel_proposal(&ctx.admin, &id);
        let p = ctx.client.get_proposal(&id);
        assert!(p.cancelled);
    }

    #[test]
    fn test_cancel_unauthorized_errors() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        let result = ctx.client.try_cancel_proposal(&ctx.signer_b, &id);
        assert_eq!(result, Err(Ok(GovernanceError::NotProposerOrAdmin)));
    }

    #[test]
    fn test_cancel_twice_errors() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.cancel_proposal(&ctx.signer_a, &id);
        let result = ctx.client.try_cancel_proposal(&ctx.signer_a, &id);
        assert_eq!(result, Err(Ok(GovernanceError::ProposalCancelled)));
    }
    #[test]
    fn test_emergency_guardian_cancels_queued_proposal() {
        let ctx = Ctx::setup();
        let guardian = Address::generate(&ctx.env);
        ctx.client
            .set_emergency_guardians(&vec![&ctx.env, guardian.clone()]);
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);

        ctx.client.cancel_proposal(&guardian, &id);
        assert!(ctx.client.get_proposal(&id).cancelled);
        assert_eq!(
            ctx.client.try_execute(&Address::generate(&ctx.env), &id),
            Err(Ok(GovernanceError::ProposalCancelled))
        );
        assert_eq!(
            ctx.client.try_approve(&ctx.signer_c, &id),
            Err(Ok(GovernanceError::ProposalCancelled))
        );
    }

    #[test]
    fn test_non_guardian_cannot_cancel_queued_proposal() {
        let ctx = Ctx::setup();
        let stranger = Address::generate(&ctx.env);
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        assert_eq!(
            ctx.client.try_cancel_proposal(&stranger, &id),
            Err(Ok(GovernanceError::NotProposerOrAdmin))
        );
    }



    #[test]
    fn test_cancel_executed_proposal_errors() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&ctx.env);
        ctx.client.execute(&executor, &id);
        let result = ctx.client.try_cancel_proposal(&ctx.signer_a, &id);
        assert_eq!(result, Err(Ok(GovernanceError::AlreadyExecuted)));
    }

    #[test]
    fn test_cancel_before_quorum() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.cancel_proposal(&ctx.signer_a, &id);
        let result = ctx.client.try_approve(&ctx.signer_b, &id);
        assert_eq!(result, Err(Ok(GovernanceError::ProposalCancelled)));
    }

    #[test]
    fn test_cancel_after_quorum_before_timelock() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.client.cancel_proposal(&ctx.signer_a, &id);
        let executor = Address::generate(&ctx.env);
        let result = ctx.client.try_execute(&executor, &id);
        assert_eq!(result, Err(Ok(GovernanceError::ProposalCancelled)));
    }

    #[test]
    fn test_approve_after_cancel_errors() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.cancel_proposal(&ctx.signer_a, &id);
        let result = ctx.client.try_approve(&ctx.signer_b, &id);
        assert_eq!(result, Err(Ok(GovernanceError::ProposalCancelled)));
    }

    #[test]
    fn test_execute_after_cancel_errors() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.client.cancel_proposal(&ctx.signer_a, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&ctx.env);
        let result = ctx.client.try_execute(&executor, &id);
        assert_eq!(result, Err(Ok(GovernanceError::ProposalCancelled)));
    }

    // -----------------------------------------------------------------------
    // Expiry
    // -----------------------------------------------------------------------

    #[test]
    fn test_approve_after_expiry_errors() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.env.ledger().set_timestamp(1_000_000 + MAX_AGE + 1);
        let result = ctx.client.try_approve(&ctx.signer_b, &id);
        assert_eq!(result, Err(Ok(GovernanceError::ProposalExpired)));
    }

    #[test]
    fn test_execute_after_expiry_errors() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        ctx.env.ledger().set_timestamp(1_000_000 + MAX_AGE + 1);
        let executor = Address::generate(&ctx.env);
        let result = ctx.client.try_execute(&executor, &id);
        assert_eq!(result, Err(Ok(GovernanceError::ProposalExpired)));
    }

    #[test]
    fn test_execute_at_expiry_boundary_succeeds() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        // Set timestamp to exactly the expiry boundary (created_at + MAX_AGE).
        // This is *not* past the boundary, so the proposal is not expired.
        // Since MAX_AGE >> TIMELOCK, the timelock has also elapsed.
        ctx.env.ledger().set_timestamp(1_000_000 + MAX_AGE);
        let executor = Address::generate(&ctx.env);
        let result = ctx.client.try_execute(&executor, &id);
        assert!(result.is_ok());
    }

    #[test]
    fn test_expired_not_executable_even_with_quorum_and_timelock_met() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.env
            .ledger()
            .set_timestamp(1_000_000 + MAX_AGE + TIMELOCK + 100);
        let executor = Address::generate(&ctx.env);
        let result = ctx.client.try_execute(&executor, &id);
        assert_eq!(result, Err(Ok(GovernanceError::ProposalExpired)));
    }

    #[test]
    fn test_execute_respects_proposal_eta_boundary() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("eta"));
        assert_eq!(ctx.client.get_proposal_eta(&id), 0);
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        let eta = 1_000_000 + TIMELOCK;
        assert_eq!(ctx.client.get_proposal_eta(&id), eta);

        ctx.env.ledger().set_timestamp(eta - 1);
        assert_eq!(
            ctx.client.try_execute(&Address::generate(&ctx.env), &id),
            Err(Ok(GovernanceError::TimelockNotMet))
        );
        assert!(!ctx.client.get_proposal(&id).executed);

        ctx.env.ledger().set_timestamp(eta);
        ctx.client.execute(&Address::generate(&ctx.env), &id);
        assert!(ctx.client.get_proposal(&id).executed);
    }



    // -----------------------------------------------------------------------
    // Grace period / stale-proposal expiry (#46)
    // -----------------------------------------------------------------------

    #[test]
    fn test_grace_period_defaults_after_init() {
        let ctx = Ctx::setup();
        assert_eq!(
            ctx.client.grace_period(),
            DEFAULT_GRACE_PERIOD_SECONDS,
            "init must persist the default grace period"
        );
    }

    #[test]
    fn test_grace_period_pre_init_returns_default() {
        let env = Env::default();
        let contract_id = env.register_contract(None, FluxoraGovernance);
        let client = FluxoraGovernanceClient::new(&env, &contract_id);
        assert_eq!(client.grace_period(), DEFAULT_GRACE_PERIOD_SECONDS);
    }

    #[test]
    fn test_set_grace_period_updates_value() {
        let ctx = Ctx::setup();
        ctx.client.set_grace_period(&30_000u64);
        assert_eq!(ctx.client.grace_period(), 30_000);
    }

    #[test]
    fn test_set_grace_period_requires_admin_auth() {
        // Without mock_all_auths, `require_auth()` fails at the host layer, so
        // the call must fail and leave the grace period unchanged.
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let contract_id = env.register_contract(None, FluxoraGovernance);
        let admin = Address::generate(&env);
        let signer_a = Address::generate(&env);
        let signer_b = Address::generate(&env);
        let client = FluxoraGovernanceClient::new(&env, &contract_id);
        client.init(&admin, &vec![&env, signer_a, signer_b], &1u32);
        assert!(client.try_set_grace_period(&42u64).is_err());
        assert_eq!(client.grace_period(), DEFAULT_GRACE_PERIOD_SECONDS);
    }

    #[test]
    fn test_execute_rejects_proposal_past_eta_plus_grace_period() {
        let ctx = Ctx::setup();
        ctx.client.set_grace_period(&1000u64);

        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);

        let eta = 1_000_000 + TIMELOCK; // quorum reached at 1_000_000
        let executor = Address::generate(&ctx.env);

        // Exactly at eta + grace_period — still executable (boundary is `>`).
        ctx.env.ledger().set_timestamp(eta + 1000);
        assert!(
            ctx.client.try_execute(&executor, &id).is_ok(),
            "execution must be allowed exactly at eta + grace_period"
        );

        // One second past eta + grace_period — rejected as expired.
        let id2 = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("y"));
        ctx.client.approve(&ctx.signer_a, &id2);
        ctx.client.approve(&ctx.signer_b, &id2);
        ctx.env.ledger().set_timestamp(eta + 1001);
        let result = ctx.client.try_execute(&executor, &id2);
        assert_eq!(result, Err(Ok(GovernanceError::ProposalExpired)));
        // The rejected proposal must NOT have been marked executed.
        assert!(!ctx.client.get_proposal(&id2).executed);
    }

    #[test]
    fn test_execute_grace_period_expiry_updates_executable_view() {
        let ctx = Ctx::setup();
        ctx.client.set_grace_period(&1000u64);

        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);

        let eta = 1_000_000 + TIMELOCK;
        ctx.env.ledger().set_timestamp(eta + 999);
        assert!(ctx.client.is_executable(&id));

        ctx.env.ledger().set_timestamp(eta + 1001);
        assert!(!ctx.client.is_executable(&id));
    }

    #[test]
    fn test_prune_expired_proposals_removes_only_stale_entries() {
        let ctx = Ctx::setup();
        ctx.client.set_grace_period(&1000u64);

        // p0: reaches quorum, then waits past eta + grace_period -> stale.
        let p0 = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("a"));
        ctx.client.approve(&ctx.signer_a, &p0);
        ctx.client.approve(&ctx.signer_b, &p0);

        // p1: reaches quorum, but is executed.
        let p1 = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("b"));
        ctx.client.approve(&ctx.signer_a, &p1);
        ctx.client.approve(&ctx.signer_b, &p1);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        ctx.client.execute(&Address::generate(&ctx.env), &p1);

        // p2: cancelled -> never pruned.
        let p2 = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("c"));
        ctx.client.cancel_proposal(&ctx.signer_a, &p2);

        // p3: fresh proposal, still well within its grace window.
        let p3 = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("d"));

        // Advance time so p0 is stale but p3 is still fresh.
        ctx.env
            .ledger()
            .set_timestamp(1_000_000 + TIMELOCK + 1001);

        let pruned = ctx.client.prune_expired_proposals(&0, &100);
        assert_eq!(pruned, 1, "exactly p0 must be pruned");

        // p0 removed from storage.
        assert_eq!(
            ctx.client.try_get_proposal(&p0),
            Err(Ok(GovernanceError::ProposalNotFound))
        );
        // Its approval index and quorum snapshot are gone too (verify via range).
        assert!(!ctx.client.get_quorum_info(&p0).is_some());

        // p1, p2, p3 unaffected.
        assert!(ctx.client.get_proposal(&p1).is_ok());
        assert!(ctx.client.get_proposal(&p2).is_ok());
        assert!(ctx.client.get_proposal(&p3).is_ok());

        // A second pass finds nothing left to prune.
        assert_eq!(ctx.client.prune_expired_proposals(&0, &100), 0);
    }

    #[test]
    fn test_prune_expired_proposals_empty_range_and_pre_init() {
        // start_id beyond all proposals -> 0.
        let ctx = Ctx::setup();
        assert_eq!(ctx.client.prune_expired_proposals(&999, &10), 0);
        // Pre-init contract: no proposals, empty range -> 0.
        let env = Env::default();
        let contract_id = env.register_contract(None, FluxoraGovernance);
        let client = FluxoraGovernanceClient::new(&env, &contract_id);
        assert_eq!(client.prune_expired_proposals(&0, &10), 0);
    }

    // -----------------------------------------------------------------------
    // Full happy path (regression)
    // -----------------------------------------------------------------------

    #[test]
    fn test_signer_configuration_change_invalidates_queued_proposal() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("stale"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        assert_eq!(ctx.client.get_proposal(&id).signer_generation, 0);

        ctx.client.remove_signer(&ctx.signer_c);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK);
        assert_eq!(
            ctx.client.try_execute(&Address::generate(&ctx.env), &id),
            Err(Ok(GovernanceError::InvalidSignerGeneration))
        );
        assert!(!ctx.client.get_proposal(&id).executed);
    }

    #[test]
    fn test_proposal_after_signer_configuration_change_executes() {
        let ctx = Ctx::setup();
        ctx.client.remove_signer(&ctx.signer_c);
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("fresh"));
        assert_eq!(ctx.client.get_proposal(&id).signer_generation, 1);
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK);
        assert!(ctx.client.try_execute(&Address::generate(&ctx.env), &id).is_ok());
    }


    #[test]
    fn test_full_governance_flow() {
        let ctx = Ctx::setup();
        let target = ctx.dummy_target();
        let calldata = ctx.calldata("set_cap:100000");
        let id = ctx.client.propose(&ctx.signer_a, &target, &calldata);
        assert_eq!(id, 0);

        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);

        let p = ctx.client.get_proposal(&id);
        assert_eq!(p.approvals.len(), 2);
        assert!(!p.executed);
        assert!(!p.cancelled);

        let executor = Address::generate(&ctx.env);
        let early = ctx.client.try_execute(&executor, &id);
        assert_eq!(ctx.client.get_proposal_eta(&id), 1_000_000 + TIMELOCK);
        assert_eq!(early, Err(Ok(GovernanceError::TimelockNotMet)));

        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        ctx.client.execute(&executor, &id);
        let p = ctx.client.get_proposal(&id);
        assert!(p.executed);
        assert_eq!(p.target, target);
    }

    /// End-to-end multi-sig threshold update flow (issue #42): the admin moves
    /// the approval threshold from 2-of-3 to 3-of-3, and a proposal submitted
    /// *after* the change requires the new threshold before it can execute.
    #[test]
    fn test_threshold_update_enforced_end_to_end() {
        let ctx = Ctx::setup(); // 3 signers, threshold=2

        // Admin raises the threshold before the new proposal is queued.
        ctx.client.update_threshold(&3u32);
        assert_eq!(ctx.client.quorum(), 3);

        let target = ctx.dummy_target();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &target, &ctx.calldata("post-update"));

        // Two approvals no longer reach the new 3-of-3 threshold.
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        let p = ctx.client.get_proposal(&id);
        assert_eq!(p.approvals.len(), 2);

        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&ctx.env);
        let result = ctx.client.try_execute(&executor, &id);
        assert_eq!(result, Err(Ok(GovernanceError::QuorumNotReached)));

        // The third signer brings the proposal to quorum under the new threshold.
        ctx.client.approve(&ctx.signer_c, &id);
        // Quorum was only reached once the third vote landed, so the timelock
        // clock starts from that moment, not from the earlier warp.
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK * 2 + 1);
        ctx.client.execute(&executor, &id);
        let p = ctx.client.get_proposal(&id);
        assert!(p.executed);
    }

    #[test]
    fn test_weighted_approval_reaches_quorum_by_weight() {
        let ctx = Ctx::setup();
        ctx.client.set_vote_weight(&ctx.signer_a, &2u64);
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("weighted"));

        ctx.client.approve(&ctx.signer_a, &id);

        let proposal = ctx.client.get_proposal(&id);
        assert_eq!(proposal.approval_weight, 2);
        assert_eq!(proposal.status, ProposalStatus::Queued);
        let quorum = ctx.client.get_quorum_info(&id).unwrap();
        assert_eq!(quorum.threshold, 2);
        assert_eq!(quorum.reached_at, 1_000_000);
    }

    #[test]
    fn test_vote_event_includes_accumulated_weight() {
        let ctx = Ctx::setup();
        ctx.client.set_threshold(&3u32);
        ctx.client.set_vote_weight(&ctx.signer_a, &2u64);
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("weighted-event"));

        ctx.client.approve(&ctx.signer_a, &id);

        let (topic, data) = last_contract_event(&ctx.env, &ctx.contract_id);
        assert_eq!(topic, symbol_short!("vote_cast"));
        let payload = VoteCast::try_from_val(&ctx.env, &data).expect("decodes to VoteCast");
        assert_eq!(payload.proposal_id, id);
        assert_eq!(payload.approval_count, 1);
        assert_eq!(payload.approval_weight, 2);
    }

    /// In-flight proposals snapshot the threshold at quorum time: a proposal
    /// that reached 2-of-3 quorum before the admin raised the threshold to 3
    /// remains executable, because the change cannot rewrite history.
    #[test]
    fn test_threshold_change_does_not_retroactively_invalidate_quorum() {
        let ctx = Ctx::setup(); // 3 signers, threshold=2

        let id = ctx.client.propose(
            &ctx.signer_a,
            &ctx.dummy_target(),
            &ctx.calldata("pre-update"),
        );
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id); // quorum reached at threshold 2

        // Admin raises the threshold afterwards — precisely the race the
        // QuorumInfo snapshot guard exists for.
        ctx.client.update_threshold(&3u32);
        assert_eq!(ctx.client.quorum(), 3);

        let quorum_info = ctx.client.get_quorum_info(&id).unwrap();
        assert_eq!(quorum_info.threshold, 2); // snapshot, not the live value

        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&ctx.env);
        ctx.client.execute(&executor, &id);
        let p = ctx.client.get_proposal(&id);
        assert!(p.executed);
    }

    #[test]
    fn test_weighted_vote_reaches_quorum_before_approval_count_threshold() {
        let ctx = Ctx::setup();
        ctx.client.set_vote_weight(&ctx.signer_a, &2);

        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("weighted"));
        ctx.client.approve(&ctx.signer_a, &id);

        let proposal = ctx.client.get_proposal(&id);
        assert_eq!(proposal.approvals.len(), 1);
        assert_eq!(proposal.approval_weight, 2);
        assert_eq!(proposal.status, ProposalStatus::Queued);
        assert_eq!(ctx.client.get_quorum_info(&id).unwrap().threshold, 2);

        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&ctx.env);
        ctx.client.execute(&executor, &id);
        assert!(ctx.client.get_proposal(&id).executed);
    }

    #[test]
    fn test_execute_without_quorum_errors() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&ctx.env);
        let result = ctx.client.try_execute(&executor, &id);
        assert_eq!(result, Err(Ok(GovernanceError::QuorumNotReached)));
    }

    #[test]
    fn test_execute_twice_errors() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&ctx.env);
        ctx.client.execute(&executor, &id);
        let result = ctx.client.try_execute(&executor, &id);
        assert_eq!(result, Err(Ok(GovernanceError::AlreadyExecuted)));
    }

    // -----------------------------------------------------------------------
    // get_quorum_info
    // -----------------------------------------------------------------------

    #[test]
    fn test_get_quorum_info_before_quorum() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        // No approvals yet — quorum not reached.
        assert!(ctx.client.get_quorum_info(&id).is_none());
    }

    #[test]
    fn test_get_quorum_info_below_threshold() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        // Only 1 approval — threshold is 2, quorum not reached.
        assert!(ctx.client.get_quorum_info(&id).is_none());
    }

    #[test]
    fn test_get_quorum_info_after_quorum() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        // First approval — below threshold.
        ctx.client.approve(&ctx.signer_a, &id);
        // Second approval — hits threshold, quorum reached at timestamp 1_000_000.
        ctx.client.approve(&ctx.signer_b, &id);

        let info = ctx
            .client
            .get_quorum_info(&id)
            .expect("should have quorum info");
        assert_eq!(info.reached_at, 1_000_000);
        assert_eq!(info.threshold, 2);
    }

    #[test]
    fn test_get_quorum_info_preserves_snapshot_threshold() {
        // Verify the snapshot threshold is independent of later threshold changes.
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);

        let info = ctx
            .client
            .get_quorum_info(&id)
            .expect("should have quorum info");
        assert_eq!(info.threshold, 2);

        // Remove signer_c — threshold stays 2, snapshot should still be 2.
        ctx.client.remove_signer(&ctx.signer_c);
        let info = ctx
            .client
            .get_quorum_info(&id)
            .expect("should still have quorum info");
        assert_eq!(info.threshold, 2);
    }

    #[test]
    fn test_get_quorum_info_none_for_nonexistent_proposal() {
        let ctx = Ctx::setup();
        // A valid ID that was never proposed; no QuorumInfo exists.
        assert!(ctx.client.get_quorum_info(&999).is_none());
    }

    #[test]
    fn test_get_quorum_info_none_after_execute() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&ctx.env);
        ctx.client.execute(&executor, &id);
        // QuorumInfo should still exist (execution does not delete it).
        let info = ctx
            .client
            .get_quorum_info(&id)
            .expect("should still have quorum info after execute");
        assert_eq!(info.reached_at, 1_000_000);
        assert_eq!(info.threshold, 2);
    }

    // -----------------------------------------------------------------------
    // is_executable
    // -----------------------------------------------------------------------
    #[test]
    fn test_is_executable_nonexistent_proposal() {
        let ctx = Ctx::setup();
        let result = ctx.client.try_is_executable(&999);
        assert_eq!(result, Err(Ok(GovernanceError::ProposalNotFound)));
    }

    #[test]
    fn test_is_executable_pre_quorum() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        // No approvals yet.
        assert!(!ctx.client.is_executable(&id));
    }

    #[test]
    fn test_is_executable_below_threshold() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        // Only 1 approval — threshold 2 not met.
        assert!(!ctx.client.is_executable(&id));
    }

    #[test]
    fn test_is_executable_post_quorum_pre_timelock() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        // Timelock not yet elapsed (current time is still 1_000_000).
        assert!(!ctx.client.is_executable(&id));
    }

    #[test]
    fn test_is_executable_post_timelock() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        assert!(ctx.client.is_executable(&id));
    }

    #[test]
    fn test_is_executable_cancelled() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.client.cancel_proposal(&ctx.signer_a, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        assert!(!ctx.client.is_executable(&id));
    }

    #[test]
    fn test_is_executable_executed() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&ctx.env);
        ctx.client.execute(&executor, &id);
        assert!(!ctx.client.is_executable(&id));
    }

    #[test]
    fn test_is_executable_expired() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + MAX_AGE + 1);
        assert!(!ctx.client.is_executable(&id));
    }

    #[test]
    fn test_is_executable_at_timelock_boundary() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);

        // Exactly at reached_at + TIMELOCK — timelock has elapsed (now >= exec_after).
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK);
        assert!(ctx.client.is_executable(&id));
    }

    #[test]
    fn test_is_executable_one_second_before_timelock() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);

        // One second before the timelock elapses — should NOT be executable.
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK - 1);
        assert!(!ctx.client.is_executable(&id));
    }

    #[test]
    fn test_is_executable_at_expiry_boundary() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);

        // Exactly at created_at + MAX_AGE — not past it, so not expired.
        // Since MAX_AGE >> TIMELOCK, the timelock has also elapsed.
        ctx.env.ledger().set_timestamp(1_000_000 + MAX_AGE);
        assert!(ctx.client.is_executable(&id));
    }

    #[test]
    fn test_is_executable_one_second_before_expiry() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);

        // One second before expiry — still executable if timelock has elapsed.
        ctx.env.ledger().set_timestamp(1_000_000 + MAX_AGE - 1);
        assert!(ctx.client.is_executable(&id));
    }

    #[test]
    fn test_is_executable_one_second_after_expiry() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);

        // One second past expiry — not executable.
        ctx.env.ledger().set_timestamp(1_000_000 + MAX_AGE + 1);
        assert!(!ctx.client.is_executable(&id));
    }

    // -----------------------------------------------------------------------
    // get_proposals_by_id_range
    // -----------------------------------------------------------------------

    /// Empty contract — no proposals created yet — returns an empty Vec for any
    /// start_id and any limit.
    #[test]
    fn test_get_proposals_by_id_range_empty_contract() {
        let ctx = Ctx::setup();
        let result = ctx.client.get_proposals_by_id_range(&0, &10);
        assert_eq!(result.len(), 0);
    }

    /// Zero limit always returns an empty Vec, even when proposals exist.
    #[test]
    fn test_get_proposals_by_id_range_zero_limit() {
        let ctx = Ctx::setup();
        ctx.client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("a"));
        let result = ctx.client.get_proposals_by_id_range(&0, &0);
        assert_eq!(result.len(), 0);
    }

    /// start_id exactly equal to proposal_count (exclusive upper bound) → empty.
    #[test]
    fn test_get_proposals_by_id_range_start_at_count() {
        let ctx = Ctx::setup();
        ctx.client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("a"));
        // proposal_count() == 1 after one propose; IDs are 0-based, so ID 0 exists,
        // and start_id = 1 is already out of range.
        let count = ctx.client.proposal_count();
        let result = ctx.client.get_proposals_by_id_range(&count, &10);
        assert_eq!(result.len(), 0);
    }

    /// start_id well beyond proposal_count → empty.
    #[test]
    fn test_get_proposals_by_id_range_start_beyond_count() {
        let ctx = Ctx::setup();
        ctx.client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("a"));
        let result = ctx.client.get_proposals_by_id_range(&9999, &10);
        assert_eq!(result.len(), 0);
    }

    /// Full page: 5 proposals, request all 5, receive all 5 in order.
    #[test]
    fn test_get_proposals_by_id_range_full_page() {
        let ctx = Ctx::setup();
        for tag in ["a", "b", "c", "d", "e"] {
            ctx.client
                .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata(tag));
        }
        let result = ctx.client.get_proposals_by_id_range(&0, &5);
        assert_eq!(result.len(), 5);
        for i in 0..5u32 {
            assert!(!result.get(i).unwrap().executed);
        }
    }

    /// Partial page: 5 proposals, request from start_id=2, limit=10 → returns IDs 2,3,4.
    #[test]
    fn test_get_proposals_by_id_range_partial_page() {
        let ctx = Ctx::setup();
        for tag in ["a", "b", "c", "d", "e"] {
            ctx.client
                .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata(tag));
        }
        // IDs are 0,1,2,3,4. Requesting from 2 with limit 10 should yield IDs 2,3,4.
        let result = ctx.client.get_proposals_by_id_range(&2, &10);
        assert_eq!(result.len(), 3, "Expected IDs 2,3,4 only");
    }

    /// Limit clamping: a limit above MAX_PAGE_SIZE is silently clamped.
    ///
    /// We create MAX_PAGE_SIZE + 5 proposals and request MAX_PAGE_SIZE + 50.
    /// The result must be exactly MAX_PAGE_SIZE entries.
    #[test]
    fn test_get_proposals_by_id_range_limit_clamping() {
        let ctx = Ctx::setup();
        let total = MAX_PAGE_SIZE + 5;
        for _ in 0..total {
            ctx.client
                .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        }
        let over_cap = MAX_PAGE_SIZE + 50;
        let result = ctx.client.get_proposals_by_id_range(&0, &over_cap);
        assert_eq!(
            result.len(),
            MAX_PAGE_SIZE,
            "Result must be clamped to MAX_PAGE_SIZE regardless of caller-supplied limit"
        );
    }

    /// Exactly MAX_PAGE_SIZE limit is not clamped (it is the cap itself).
    #[test]
    fn test_get_proposals_by_id_range_limit_exactly_cap() {
        let ctx = Ctx::setup();
        for _ in 0..MAX_PAGE_SIZE {
            ctx.client
                .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        }
        let result = ctx.client.get_proposals_by_id_range(&0, &MAX_PAGE_SIZE);
        assert_eq!(result.len(), MAX_PAGE_SIZE);
    }

    /// Cancelled proposals are NOT removed from storage — they remain present
    /// with `cancelled = true` and must be returned by the range query.
    #[test]
    fn test_get_proposals_by_id_range_includes_cancelled() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("a"));
        ctx.client.cancel_proposal(&ctx.signer_a, &id);

        let result = ctx.client.get_proposals_by_id_range(&0, &10);
        assert_eq!(
            result.len(),
            1,
            "Cancelled proposal must still appear in range"
        );
        assert!(result.get(0).unwrap().cancelled);
    }

    /// Executed proposals remain in storage with `executed = true` and must be
    /// returned by the range query.
    #[test]
    fn test_get_proposals_by_id_range_includes_executed() {
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("a"));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&ctx.env);
        ctx.client.execute(&executor, &id);

        let result = ctx.client.get_proposals_by_id_range(&0, &10);
        assert_eq!(
            result.len(),
            1,
            "Executed proposal must still appear in range"
        );
        assert!(result.get(0).unwrap().executed);
    }

    /// Pagination: two consecutive pages with limit=3 over 5 proposals cover all IDs.
    #[test]
    fn test_get_proposals_by_id_range_pagination_two_pages() {
        let ctx = Ctx::setup();
        for tag in ["a", "b", "c", "d", "e"] {
            ctx.client
                .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata(tag));
        }
        // Page 1: IDs 0,1,2
        let page1 = ctx.client.get_proposals_by_id_range(&0, &3);
        assert_eq!(page1.len(), 3);
        // Page 2: IDs 3,4
        let page2 = ctx.client.get_proposals_by_id_range(&3, &3);
        assert_eq!(page2.len(), 2);
    }

    /// u32::MAX limit is handled safely via saturating_add; only existing proposals
    /// within [start_id, total) are returned.
    #[test]
    fn test_get_proposals_by_id_range_u32_max_limit() {
        let ctx = Ctx::setup();
        for _ in 0..3 {
            ctx.client
                .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        }
        // MAX_PAGE_SIZE clamp applies before the saturating_add path is reached,
        // but the saturating arithmetic must not panic.
        let result = ctx.client.get_proposals_by_id_range(&0, &u32::MAX);
        // Clamped to MAX_PAGE_SIZE; only 3 proposals exist so we get 3.
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_is_signer_returns_true_for_registered() {
        let ctx = Ctx::setup();
        // All three signers from `Ctx::setup` must be reported as members.
        assert!(ctx.client.is_signer(&ctx.signer_a));
        assert!(ctx.client.is_signer(&ctx.signer_b));
        assert!(ctx.client.is_signer(&ctx.signer_c));
    }

    #[test]
    fn test_is_signer_returns_false_for_unregistered() {
        let ctx = Ctx::setup();
        // An address that was never added to the signer set.
        let stranger = Address::generate(&ctx.env);
        assert!(!ctx.client.is_signer(&stranger));
    }

    #[test]
    fn test_is_signer_returns_false_for_admin_if_not_a_signer() {
        // The admin address is distinct from the signer set; the admin is not
        // automatically counted as a co-signer.
        let ctx = Ctx::setup();
        assert!(!ctx.client.is_signer(&ctx.admin));
    }

    #[test]
    fn test_is_signer_returns_false_after_removal() {
        let ctx = Ctx::setup();
        // Sanity: signer_a starts as a member.
        assert!(ctx.client.is_signer(&ctx.signer_a));
        // Admin removes signer_a.
        ctx.client.remove_signer(&ctx.signer_a);
        // After removal the index entry is gone; the view must reflect that.
        assert!(!ctx.client.is_signer(&ctx.signer_a));
        // Other signers are unaffected.
        assert!(ctx.client.is_signer(&ctx.signer_b));
        assert!(ctx.client.is_signer(&ctx.signer_c));
    }

    #[test]
    fn test_is_signer_returns_true_after_add() {
        let ctx = Ctx::setup();
        let newcomer = Address::generate(&ctx.env);
        // Pre-condition: not a member.
        assert!(!ctx.client.is_signer(&newcomer));
        // Admin adds newcomer.
        ctx.client.add_signer(&newcomer);
        // Post-condition: now a member.
        assert!(ctx.client.is_signer(&newcomer));
    }

    #[test]
    fn test_is_signer_returns_false_pre_init_no_panic() {
        // Contract deployed but `init` never called. `is_signer` MUST return
        // `false` rather than panicking or returning an error.
        let env = Env::default();
        let contract_id = env.register_contract(None, FluxoraGovernance);
        let client = FluxoraGovernanceClient::new(&env, &contract_id);

        // Try several addresses, including a freshly generated one and the
        // zero address-equivalent pattern. None must panic, all must report
        // `false`.
        let a1 = Address::generate(&env);
        let a2 = Address::generate(&env);
        assert!(!client.is_signer(&a1));
        assert!(!client.is_signer(&a2));
    }

    #[test]
    fn test_is_signer_pre_init_matches_no_signers() {
        // Pre-init behaviour must match the "no signers" semantics of a freshly
        // initialised contract whose signer list happens to be empty (although
        // `init` rejects an empty signer list, the contract state is logically
        // indistinguishable from pre-init for the purposes of `is_signer`).
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);
        let contract_id = env.register_contract(None, FluxoraGovernance);
        let admin = Address::generate(&env);
        let client = FluxoraGovernanceClient::new(&env, &contract_id);
        // Note: `init` requires at least 1 signer, so we cannot exercise the
        // "post-init empty set" path directly. The pre-init path returns
        // `false` for every address — the strictest possible membership view.
        let stranger = Address::generate(&env);
        assert!(!client.is_signer(&stranger));
        // `admin` is also not a signer pre-init.
        assert!(!client.is_signer(&admin));
    }

    #[test]
    fn test_is_signer_agrees_with_get_signers_membership() {
        // For every signer reported by `get_signers`, `is_signer` must return
        // `true`; for every address NOT in `get_signers`, it must return
        // `false`. This is the cross-check that `is_signer` and `get_signers`
        // share a single source of truth (`DataKey::SignerIndex`).
        let ctx = Ctx::setup();
        let on_chain = ctx.client.get_signers();
        for i in 0..on_chain.len() {
            let addr = on_chain.get(i).unwrap();
            assert!(
                ctx.client.is_signer(&addr),
                "is_signer disagreed with get_signers for a listed signer"
            );
        }
        // A stranger that is definitely not in the set.
        let stranger = Address::generate(&ctx.env);
        assert!(!ctx.client.is_signer(&stranger));
    }

    #[test]
    fn test_is_signer_can_be_called_repeatedly() {
        // The new view must remain side-effect-free under repeated calls.
        // We can't directly observe TTL bytes in the host, but we can at
        // least confirm the call returns and does not error or panic when
        // called many times in a row. If a future change accidentally adds
        // `bump_instance` to `is_signer`, this test will still pass — the
        // actual TTL guarantee is documented in the function's doc comment
        // and the security note in `docs/governance.md`.
        let ctx = Ctx::setup();
        for _ in 0..32 {
            assert!(ctx.client.is_signer(&ctx.signer_a));
            assert!(!ctx.client.is_signer(&Address::generate(&ctx.env)));
        }
    }

    #[test]
    fn test_is_signer_after_full_lifecycle() {
        // Walk the full signer lifecycle: init, propose, approve, remove,
        // re-add. `is_signer` must track `SignerIndex` correctly at every
        // step.
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        // All three signers are still present.
        assert!(ctx.client.is_signer(&ctx.signer_a));
        assert!(ctx.client.is_signer(&ctx.signer_b));
        assert!(ctx.client.is_signer(&ctx.signer_c));

        // Approvals proceed; signer_b approves too.
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);

        // Remove signer_c (set goes from 3 to 2, still >= threshold=2).
        ctx.client.remove_signer(&ctx.signer_c);
        assert!(!ctx.client.is_signer(&ctx.signer_c));
        assert!(ctx.client.is_signer(&ctx.signer_a));
        assert!(ctx.client.is_signer(&ctx.signer_b));

        // Re-add signer_c (back to 3).
        ctx.client.add_signer(&ctx.signer_c);
        assert!(ctx.client.is_signer(&ctx.signer_c));
    }

    #[test]
    fn test_is_signer_agrees_with_propose_membership_check() {
        // The internal membership check used by `propose` (via
        // `is_registered_signer`) and the public `is_signer` view must agree.
        // If a non-signer reports `is_signer == true` but `propose` rejects
        // with `NotASigner`, the two sources of truth have diverged.
        let ctx = Ctx::setup();
        let stranger = Address::generate(&ctx.env);
        // Stranger: is_signer false, propose must fail with NotASigner.
        assert!(!ctx.client.is_signer(&stranger));
        let result = ctx
            .client
            .try_propose(&stranger, &ctx.dummy_target(), &ctx.calldata("x"));
        assert_eq!(result, Err(Ok(GovernanceError::NotASigner)));
        // Registered signer: is_signer true, propose must succeed.
        assert!(ctx.client.is_signer(&ctx.signer_a));
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        // Sanity: the proposal got a real ID.
        assert_eq!(id, 0);
    }

    #[test]
    fn test_is_signer_agrees_with_approve_membership_check() {
        // Symmetric check for `approve` (also goes through
        // `is_registered_signer`).
        let ctx = Ctx::setup();
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        // Stranger cannot approve.
        let stranger = Address::generate(&ctx.env);
        assert!(!ctx.client.is_signer(&stranger));
        let result = ctx.client.try_approve(&stranger, &id);
        assert_eq!(result, Err(Ok(GovernanceError::NotASigner)));
        // Registered signer can approve.
        assert!(ctx.client.is_signer(&ctx.signer_b));
        ctx.client.approve(&ctx.signer_b, &id);
    }

    // -----------------------------------------------------------------------
    // Calldata validation (#733)
    // -----------------------------------------------------------------------

    /// Empty calldata is rejected before the proposal ID is consumed.
    #[test]
    fn test_propose_rejects_empty_calldata() {
        let ctx = Ctx::setup();
        let empty = Bytes::new(&ctx.env);
        let before = ctx.client.proposal_count();
        let result = ctx
            .client
            .try_propose(&ctx.signer_a, &ctx.dummy_target(), &empty);
        assert_eq!(result, Err(Ok(GovernanceError::CalldataEmpty)));
        // Proposal ID counter must not have advanced.
        assert_eq!(ctx.client.proposal_count(), before);
    }

    /// One-byte calldata satisfies the minimum and is accepted.
    #[test]
    fn test_propose_accepts_one_byte_calldata() {
        let ctx = Ctx::setup();
        let one_byte = Bytes::from_slice(&ctx.env, &[0x01]);
        let result = ctx
            .client
            .try_propose(&ctx.signer_a, &ctx.dummy_target(), &one_byte);
        assert!(result.is_ok());
    }

    /// MAX_CALLDATA_BYTES-length calldata is still accepted (existing upper bound unchanged).
    #[test]
    fn test_propose_accepts_max_calldata() {
        let ctx = Ctx::setup();
        // Build exactly MAX_CALLDATA_BYTES bytes via repeated concatenation.
        let mut calldata = Bytes::new(&ctx.env);
        let one = Bytes::from_slice(&ctx.env, &[0xABu8]);
        for _ in 0..MAX_CALLDATA_BYTES {
            calldata.append(&one);
        }
        let result = ctx
            .client
            .try_propose(&ctx.signer_a, &ctx.dummy_target(), &calldata);
        assert!(result.is_ok());
    }

    /// Calldata one byte over the maximum is still rejected with CalldataTooLarge.
    #[test]
    fn test_propose_rejects_oversized_calldata() {
        let ctx = Ctx::setup();
        let mut calldata = Bytes::new(&ctx.env);
        let one = Bytes::from_slice(&ctx.env, &[0xFFu8]);
        for _ in 0..MAX_CALLDATA_BYTES + 1 {
            calldata.append(&one);
        }
        let result = ctx
            .client
            .try_propose(&ctx.signer_a, &ctx.dummy_target(), &calldata);
        assert_eq!(result, Err(Ok(GovernanceError::CalldataTooLarge)));
    }

    #[test]
    fn test_is_executable_agrees_with_execute_across_states() {
        let ctx = Ctx::setup();

        // --- Pre-quorum ---
        let id = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        assert!(!ctx.client.is_executable(&id));
        let executor = Address::generate(&ctx.env);
        assert_eq!(
            ctx.client.try_execute(&executor, &id),
            Err(Ok(GovernanceError::QuorumNotReached))
        );

        // --- Post-quorum, pre-timelock ---
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        assert!(!ctx.client.is_executable(&id));
        let result = ctx.client.try_execute(&executor, &id);
        assert_eq!(result, Err(Ok(GovernanceError::TimelockNotMet)));

        // --- Post-timelock, executable ---
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        assert!(ctx.client.is_executable(&id));
        assert!(ctx.client.try_execute(&executor, &id).is_ok());

        // --- Post-execution ---
        assert!(!ctx.client.is_executable(&id));
        assert_eq!(
            ctx.client.try_execute(&executor, &id),
            Err(Ok(GovernanceError::AlreadyExecuted))
        );

        // --- Cancelled proposal (fresh) ---
        let id2 = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("y"));
        ctx.client.approve(&ctx.signer_a, &id2);
        ctx.client.approve(&ctx.signer_b, &id2);
        ctx.client.cancel_proposal(&ctx.signer_a, &id2);
        assert!(!ctx.client.is_executable(&id2));
        assert_eq!(
            ctx.client.try_execute(&executor, &id2),
            Err(Ok(GovernanceError::ProposalCancelled))
        );

        // --- Expired proposal (fresh) ---
        let id3 = ctx
            .client
            .propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("z"));
        ctx.client.approve(&ctx.signer_a, &id3);
        ctx.client.approve(&ctx.signer_b, &id3);
        ctx.env
            .ledger()
            .set_timestamp(1_000_000 + MAX_AGE + TIMELOCK + 100);
        assert!(!ctx.client.is_executable(&id3));
        assert_eq!(
            ctx.client.try_execute(&executor, &id3),
            Err(Ok(GovernanceError::ProposalExpired))
        );
    }

    // -----------------------------------------------------------------------
    // Reentrancy protection (#47)
    // -----------------------------------------------------------------------

    /// Storage for the hostile reentrant mock target.
    #[contracttype]
    pub enum ReentrantDataKey {
        /// Governance contract id to re-enter (set by `arm`).
        Governance,
        /// Executor to use for the reentrant `execute` attempt (arm).
        Executor,
        /// Proposal id the mock attempts to re-execute (arm).
        Proposal,
        /// True iff the reentrant `execute` unexpectedly returned `Ok`.
        Reentered,
        /// True iff the reentrant `execute` was rejected by `ReentrancyGuard`.
        GuardBlocked,
    }

    /// Mock evil target: when governance dispatches `set_admin` to it, it tries
    /// to re-enter governance's `execute` for the armed proposal. It records
    /// whether its reentrant call (a) succeeded or (b) was blocked by the
    /// non-reentrancy guard, then returns `Ok` so the outer dispatch completes.
    #[contract]
    pub struct ReentrantMock;

    #[contractimpl]
    impl ReentrantMock {
        /// Configure the reentrancy attempt before execution.
        pub fn arm(
            env: Env,
            governance_id: Address,
            executor: Address,
            proposal_id: u32,
        ) {
            env.storage().instance().set(&ReentrantDataKey::Governance, &governance_id);
            env.storage().instance().set(&ReentrantDataKey::Executor, &executor);
            env.storage().instance().set(&ReentrantDataKey::Proposal, &proposal_id);
        }

        /// Invoked by governance's `StreamSetAdmin` dispatch. Attempts a
        /// reentrant `execute` and records the outcome.
        pub fn set_admin(env: Env, _new_admin: Address) {
            let governance_id: Address = env
                .storage()
                .instance()
                .get(&ReentrantDataKey::Governance)
                .expect("mock must be armed before use");
            let executor: Address = env
                .storage()
                .instance()
                .get(&ReentrantDataKey::Executor)
                .expect("mock must be armed before use");
            let proposal_id: u32 = env
                .storage()
                .instance()
                .get(&ReentrantDataKey::Proposal)
                .expect("mock must be armed before use");

            let client = FluxoraGovernanceClient::new(&env, &governance_id);
            let outcome = client.try_execute(&executor, &proposal_id);
            let reentered = outcome.is_ok();
            let guard_blocked = matches!(
                outcome,
                Err(Ok(GovernanceError::ReentrancyGuard))
            );

            env.storage()
                .instance()
                .set(&ReentrantDataKey::Reentered, &reentered);
            env.storage()
                .instance()
                .set(&ReentrantDataKey::GuardBlocked, &guard_blocked);
        }

        pub fn reentered(env: Env) -> bool {
            env.storage()
                .instance()
                .get(&ReentrantDataKey::Reentered)
                .unwrap_or(false)
        }

        pub fn guard_blocked(env: Env) -> bool {
            env.storage()
                .instance()
                .get(&ReentrantDataKey::GuardBlocked)
                .unwrap_or(false)
        }
    }

    fn executed_event_count(env: &Env, contract_id: &Address) -> u32 {
        use soroban_sdk::xdr::ContractEventBody;
        let mut count = 0u32;
        for event in env
            .events()
            .all()
            .filter_by_contract(contract_id)
            .events()
            .iter()
        {
            let ContractEventBody::V0(body) = &event.body;
            if let Some(topic) = body.topics.first() {
                let val = Val::try_from_val(env, topic).expect("topic converts");
                let sym = Symbol::try_from_val(env, &val).expect("topic is a symbol");
                if sym == symbol_short!("executed") {
                    count += 1;
                }
            }
        }
        count
    }

    #[test]
    fn test_reentrant_execute_of_same_proposal_blocked() {
        use soroban_sdk::xdr::ToXdr;
        let ctx = Ctx::setup();
        let mock_id = ctx.env.register_contract(None, ReentrantMock);
        let mock_client = ReentrantMockClient::new(&ctx.env, &mock_id);
        let executor = Address::generate(&ctx.env);
        let new_admin = Address::generate(&ctx.env);

        let id = ctx.client.propose(
            &ctx.signer_a,
            &mock_id,
            &CallData::StreamSetAdmin(new_admin).to_xdr(&ctx.env),
        );
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);

        mock_client.arm(&ctx.contract_id, &executor, &id);

        // The legit execution must succeed…
        let result = ctx.client.try_execute(&executor, &id);
        assert!(result.is_ok(), "legit execution must succeed");
        // …the reentrant attempt inside the target must fail safely…
        assert!(
            mock_client.guard_blocked(),
            "reentrant execute must be rejected by the non-reentrancy guard"
        );
        assert!(!mock_client.reentered(), "reentrant execute must not succeed");
        // …and the proposal is executed exactly once.
        assert!(ctx.client.get_proposal(&id).executed);
        assert_eq!(executed_event_count(&ctx.env, &ctx.contract_id), 1);
    }

    #[test]
    fn test_reentrant_execute_of_other_proposal_blocked() {
        use soroban_sdk::xdr::ToXdr;
        let ctx = Ctx::setup();
        let mock_id = ctx.env.register_contract(None, ReentrantMock);
        let mock_client = ReentrantMockClient::new(&ctx.env, &mock_id);
        let executor = Address::generate(&ctx.env);
        let new_admin = Address::generate(&ctx.env);

        // Attack surface: dispatching proposal 0 makes the mock try to force
        // proposal 1 through *before* the outer dispatch returns.
        let id0 = ctx.client.propose(
            &ctx.signer_a,
            &mock_id,
            &CallData::StreamSetAdmin(new_admin).to_xdr(&ctx.env),
        );
        let id1 = ctx.client.propose(&ctx.signer_a, &ctx.dummy_target(), &ctx.calldata("x"));
        ctx.client.approve(&ctx.signer_a, &id0);
        ctx.client.approve(&ctx.signer_b, &id0);
        ctx.client.approve(&ctx.signer_a, &id1);
        ctx.client.approve(&ctx.signer_b, &id1);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);

        // The mock re-enters with the *other* (not yet executed) proposal.
        mock_client.arm(&ctx.contract_id, &executor, &id1);

        let result = ctx.client.try_execute(&executor, &id0);
        assert!(result.is_ok(), "legit execution must succeed");
        assert!(
            mock_client.guard_blocked(),
            "reentrant execute of another proposal must be rejected by the guard"
        );
        assert!(!mock_client.reentered());

        // Proposal 0 executed exactly once; proposal 1 left untouched.
        assert!(ctx.client.get_proposal(&id0).executed);
        assert!(
            !ctx.client.get_proposal(&id1).executed,
            "the guard must prevent a second proposal from being pushed through reentrancy"
        );
        assert_eq!(executed_event_count(&ctx.env, &ctx.contract_id), 1);
    }

    // -----------------------------------------------------------------------
    // Batch proposals (issue #51)
    //
    // A `CallData::Batch` payload carries up to MAX_BATCH_CALLS
    // `ExecutionCall`s, each with its own target, and `execute` applies them in
    // order. The property that matters is all-or-nothing: if any entry fails,
    // the host unwinds the whole transaction, so the entries that already ran
    // leave no trace and the proposal stays executable.
    //
    // The mocks below make that observable. `BatchTarget` records the order in
    // which governance applied its setters, `FlakyTarget` can be told to start
    // failing to model a transient downstream lock, and both require the
    // governance contract's authorization, so no state changes except through
    // `execute`.
    //
    // The write-up is docs/governance-batch-proposals.md.
    // -----------------------------------------------------------------------

    /// Storage keys for [`BatchTarget`].
    #[contracttype]
    #[derive(Clone, Debug)]
    pub enum BatchTargetKey {
        /// The only address permitted to mutate this mock.
        Controller,
        /// Deposit cap, written by `set_cap`.
        Cap,
        /// Minimum duration, written by `set_min_duration`.
        MinDuration,
        /// Allowlist flag, written by `set_allowlist`.
        Allowed,
        /// Order in which the governed setters were applied.
        Applied,
    }

    /// Op codes recorded in [`BatchTargetKey::Applied`].
    const OP_SET_CAP: u32 = 1;
    const OP_SET_MIN_DURATION: u32 = 2;
    const OP_SET_ALLOWLIST: u32 = 3;

    /// Governed stand-in whose three setters are reachable through
    /// `FactorySetCap`, `FactorySetMinDuration` and `FactorySetAllowlist`.
    #[contract]
    pub struct BatchTarget;

    #[contractimpl]
    impl BatchTarget {
        pub fn init(env: Env, controller: Address) {
            env.storage()
                .instance()
                .set(&BatchTargetKey::Controller, &controller);
            env.storage().instance().set(&BatchTargetKey::Cap, &0i128);
            env.storage()
                .instance()
                .set(&BatchTargetKey::MinDuration, &0u64);
            env.storage()
                .instance()
                .set(&BatchTargetKey::Allowed, &false);
            env.storage()
                .instance()
                .set(&BatchTargetKey::Applied, &Vec::new(&env));
        }

        pub fn set_cap(env: Env, cap: i128) {
            controller_of(&env, &BatchTargetKey::Controller).require_auth();
            env.storage().instance().set(&BatchTargetKey::Cap, &cap);
            record_applied(&env, OP_SET_CAP);
        }

        pub fn set_min_duration(env: Env, min_duration: u64) {
            controller_of(&env, &BatchTargetKey::Controller).require_auth();
            env.storage()
                .instance()
                .set(&BatchTargetKey::MinDuration, &min_duration);
            record_applied(&env, OP_SET_MIN_DURATION);
        }

        pub fn set_allowlist(env: Env, _recipient: Address, allowed: bool) {
            controller_of(&env, &BatchTargetKey::Controller).require_auth();
            env.storage()
                .instance()
                .set(&BatchTargetKey::Allowed, &allowed);
            record_applied(&env, OP_SET_ALLOWLIST);
        }

        /// The ordered log of setters governance has applied.
        pub fn applied(env: Env) -> Vec<u32> {
            env.storage()
                .instance()
                .get(&BatchTargetKey::Applied)
                .unwrap_or_else(|| Vec::new(&env))
        }

        pub fn cap(env: Env) -> i128 {
            env.storage()
                .instance()
                .get(&BatchTargetKey::Cap)
                .unwrap_or(0)
        }

        pub fn min_duration(env: Env) -> u64 {
            env.storage()
                .instance()
                .get(&BatchTargetKey::MinDuration)
                .unwrap_or(0)
        }

        pub fn allowed(env: Env) -> bool {
            env.storage()
                .instance()
                .get(&BatchTargetKey::Allowed)
                .unwrap_or(false)
        }
    }

    /// Read the controller recorded under `key` for one of the batch mocks.
    fn controller_of(env: &Env, key: &BatchTargetKey) -> Address {
        env.storage()
            .instance()
            .get(key)
            .expect("controller set by init")
    }

    /// Append an op code to the mock's ordered log.
    fn record_applied(env: &Env, op: u32) {
        let mut log: Vec<u32> = env
            .storage()
            .instance()
            .get(&BatchTargetKey::Applied)
            .expect("log initialised by init");
        log.push_back(op);
        env.storage().instance().set(&BatchTargetKey::Applied, &log);
    }

    /// Storage keys for [`FlakyTarget`].
    #[contracttype]
    #[derive(Clone, Debug)]
    pub enum FlakyTargetKey {
        /// The only address permitted to mutate this mock.
        Controller,
        /// When true, `set_cap` traps instead of applying.
        Failing,
        /// Value written by `set_cap` when the target is healthy.
        Cap,
    }

    /// Governed stand-in that can be switched into a failing state, to model a
    /// transient downstream lock during `execute` (#51 atomicity, #53 recovery).
    #[contract]
    pub struct FlakyTarget;

    #[contractimpl]
    impl FlakyTarget {
        pub fn init(env: Env, controller: Address) {
            env.storage()
                .instance()
                .set(&FlakyTargetKey::Controller, &controller);
            env.storage()
                .instance()
                .set(&FlakyTargetKey::Failing, &false);
            env.storage().instance().set(&FlakyTargetKey::Cap, &0i128);
        }

        /// Toggle the simulated downstream failure. Callable by anyone: it is
        /// the test fixture's own switch, not governed state.
        pub fn set_failing(env: Env, failing: bool) {
            env.storage()
                .instance()
                .set(&FlakyTargetKey::Failing, &failing);
        }

        pub fn is_failing(env: Env) -> bool {
            env.storage()
                .instance()
                .get(&FlakyTargetKey::Failing)
                .unwrap_or(false)
        }

        /// Traps while the target is failing, so governance's dispatch panics
        /// and the host unwinds the entire transaction.
        pub fn set_cap(env: Env, cap: i128) {
            let controller: Address = env
                .storage()
                .instance()
                .get(&FlakyTargetKey::Controller)
                .expect("controller set by init");
            controller.require_auth();
            if env
                .storage()
                .instance()
                .get(&FlakyTargetKey::Failing)
                .unwrap_or(false)
            {
                panic!("downstream state locked");
            }
            env.storage().instance().set(&FlakyTargetKey::Cap, &cap);
        }

        pub fn cap(env: Env) -> i128 {
            env.storage()
                .instance()
                .get(&FlakyTargetKey::Cap)
                .unwrap_or(0)
        }
    }

    // Timelock bypass audit (issue #50)
    //
    // Two layers:
    //   * `test_static_audit_*` parse this file's own source at compile time
    //     (`include_str!("lib.rs")`) and pin the shape of the dispatch graph.
    //   * the `test_negative_*` / `test_positive_*` cases drive real proposals
    //     against `MockTarget`, a governed contract whose only setter requires
    //     the controller's authorization. The mock's own state is therefore
    //     ground truth for "did governance dispatch, or not?".
    //
    // The write-up is docs/governance-timelock-audit.md.
    // -----------------------------------------------------------------------

    /// The contract's own source, embedded so the static audit below inspects
    /// the code that is actually compiled rather than a copy of it.
    const SOURCE: &str = include_str!("lib.rs");

    /// `SOURCE` with line endings normalised to `\n`, so the needles below
    /// behave the same in a CRLF working tree (Windows checkouts) and in the
    /// LF tree CI builds.
    fn source() -> &'static str {
        static NORMALIZED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        NORMALIZED.get_or_init(|| SOURCE.replace("\r\n", "\n"))
    }

    /// The full `impl FluxoraGovernance` block: public entrypoints *and* the
    /// private methods they call.
    fn impl_block() -> &'static str {
        let source = source();
        let start = source
            .find("impl FluxoraGovernance {")
            .expect("the contract impl block must exist");
        let rest = &source[start..];
        let end = rest
            .find("\n}\n")
            .expect("the contract impl block must be closed at column 0");
        &rest[..end]
    }

    /// The source of a single entrypoint: its signature through to the next
    /// entrypoint, so a body cannot borrow a sibling's text.
    fn entrypoint_source(name: &str) -> &'static str {
        let block = impl_block();
        let needle = format!("\n    pub fn {name}(");
        let start = block
            .find(&needle)
            .unwrap_or_else(|| panic!("entrypoint `{name}` is not declared"));
        let end = block[start + 1..]
            .find("\n    pub fn ")
            .map_or(block.len() - start, |i| i + 1);
        &block[start..start + end]
    }

    fn count_occurrences(haystack: &str, needle: &str) -> u32 {
        haystack.matches(needle).count() as u32
    }

    /// Every public entrypoint of `FluxoraGovernance`, in declaration order.
    ///
    /// `test_static_audit_entrypoint_inventory_is_complete` fails if this list
    /// and the contract ever drift apart, so a new entrypoint cannot escape
    /// the audit below.
    const ENTRYPOINTS: &[&str] = &[
        "init",
        "set_admin",
        "set_emergency_guardians",
        "set_threshold",
        "set_vote_weight",
        "vote_weight",
        "add_signer",
        "remove_signer",
        "update_threshold",
        "propose",
        "approve",
        "execute",
        "cancel_proposal",
        "prune_expired_proposals",
        "get_proposal",
        "get_proposal_eta",
        "grace_period",
        "set_grace_period",
        "get_proposal_status",
        "is_proposal_in_status",
        "proposal_count",
        "get_signers",
        "get_admin",
        "get_emergency_guardians",
        "get_threshold",
        "quorum",
        "timelock_seconds",
        "max_proposal_age_seconds",
        "get_quorum_info",
        "is_executable",
        "is_signer",
        "get_proposals_by_id_range",
    ];

    /// Entrypoints that mutate contract state and therefore must authenticate
    /// their caller.
    ///
    /// `init` is deliberately excluded: it is the one-time bootstrap and has no
    /// admin yet (see residual risks in docs/governance-timelock-audit.md).
    /// `prune_expired_proposals` is excluded because it is permissionless
    /// maintenance that can only delete entries which are already past
    /// `created_at + MAX_PROPOSAL_AGE_SECONDS`, i.e. entries `execute` already
    /// refuses; it can never reach a governed target.
    const STATE_MUTATING_ENTRYPOINTS: &[&str] = &[
        "set_admin",
        "set_emergency_guardians",
        "set_threshold",
        "set_vote_weight",
        "add_signer",
        "remove_signer",
        "update_threshold",
        "propose",
        "approve",
        "execute",
        "cancel_proposal",
        "set_grace_period",
    ];

    // -- static audit -------------------------------------------------------

    /// The whole `impl` block — entrypoints and private methods alike — must
    /// contain no cross-contract invocation. The only path to a target is the
    /// free function `dispatch_call`, which is not an entrypoint and therefore
    /// cannot be called by a transaction.
    #[test]
    fn test_static_audit_contract_impl_never_invokes_a_contract_directly() {
        assert!(
            !impl_block().contains("invoke_contract"),
            "no entrypoint (public or private) may invoke a contract; \
             every target call must go through dispatch_call"
        );
        assert!(
            !impl_block().contains("fn dispatch_call("),
            "dispatch_call must stay a free function outside the impl block"
        );
    }

    /// `execute` is the only entrypoint that dispatches a target call.
    #[test]
    fn test_static_audit_execute_is_the_only_dispatch_site() {
        assert_eq!(
            count_occurrences(impl_block(), "dispatch_call("),
            1,
            "exactly one entrypoint may call dispatch_call"
        );
        for name in ENTRYPOINTS {
            assert_eq!(
                entrypoint_source(name).contains("dispatch_call("),
                *name == "execute",
                "{name} dispatch check failed"
            );
        }
    }

    /// In `execute`'s own source, every gate precedes the dispatch.
    ///
    /// This is the test that fails if someone reorders the body to dispatch
    /// first and validate afterwards, or drops one of the gates.
    #[test]
    fn test_static_audit_execute_dispatches_after_every_gate() {
        let body = entrypoint_source("execute");
        let dispatch = body
            .find("dispatch_call(")
            .expect("execute dispatches through dispatch_call");
        for gate in [
            "executor.require_auth",
            "ReentrancyGuard",
            "ProposalCancelled",
            "AlreadyExecuted",
            "ProposalExpired",
            "InvalidSignerGeneration",
            "QuorumNotReached",
            "TimelockNotMet",
        ] {
            let at = body
                .find(gate)
                .unwrap_or_else(|| panic!("execute no longer enforces {gate}"));
            assert!(at < dispatch, "{gate} must be checked before dispatching");
        }
    }

    /// Checks-effects-interactions: the proposal is marked executed and the
    /// reentrancy flag is raised *before* the untrusted call. If either write
    /// moved after the dispatch, `execute` would be reentrant (#47).
    #[test]
    fn test_static_audit_execute_applies_cei_before_dispatch() {
        let body = entrypoint_source("execute");
        let dispatch = body.find("dispatch_call(").expect("execute dispatches");
        for effect in [
            "proposal.status = ProposalStatus::Executed",
            "Executing, &true",
        ] {
            let at = body
                .find(effect)
                .unwrap_or_else(|| panic!("execute no longer performs `{effect}`"));
            assert!(at < dispatch, "`{effect}` must precede the dispatch (CEI)");
        }
    }

    /// Every state-mutating entrypoint still authenticates its caller.
    #[test]
    fn test_static_audit_state_mutating_entrypoints_require_auth() {
        for name in STATE_MUTATING_ENTRYPOINTS {
            assert!(
                entrypoint_source(name).contains("require_auth"),
                "{name} must require auth before mutating state"
            );
        }
    }

    /// The audited inventory matches the contract exactly: a renamed, added or
    /// removed entrypoint fails here rather than slipping past the audit.
    #[test]
    fn test_static_audit_entrypoint_inventory_is_complete() {
        for name in ENTRYPOINTS {
            // `entrypoint_source` panics if the entrypoint is not declared.
            assert!(entrypoint_source(name).contains("pub fn "));
        }
        assert_eq!(
            count_occurrences(impl_block(), "\n    pub fn "),
            ENTRYPOINTS.len() as u32,
            "every public entrypoint must appear in the audit inventory"
        );
    }

    // -- dynamic negative tests ---------------------------------------------

    use soroban_sdk::InvokeError;

    /// Storage keys for [`MockTarget`].
    #[contracttype]
    #[derive(Clone, Debug)]
    pub enum MockTargetKey {
        /// The only address permitted to mutate this mock — the governance
        /// contract in every test below.
        Controller,
        /// Number of accepted state-mutating calls received so far.
        CallCount,
        /// Value written by the guarded setter.
        Cap,
    }

    /// Stand-in for a governed protocol contract.
    ///
    /// `set_cap` is the entrypoint that `CallData::FactorySetCap` dispatches to.
    /// It requires the controller's authorization, so the only way to change
    /// the mock's state is *through* the governance contract — which is what
    /// makes the negative tests meaningful: a timelock bypass would show up
    /// here as `call_count() > 0` or `cap() != 0` before the timelock elapsed.
    #[contract]
    pub struct MockTarget;

    #[contractimpl]
    impl MockTarget {
        /// Record the address allowed to mutate this mock. Callable by anyone:
        /// it only stores the expectation, it mutates no governed state.
        pub fn init(env: Env, controller: Address) {
            env.storage()
                .instance()
                .set(&MockTargetKey::Controller, &controller);
            env.storage()
                .instance()
                .set(&MockTargetKey::CallCount, &0u32);
        }

        /// The governed setter, reached only via `dispatch_call`.
        pub fn set_cap(env: Env, cap: i128) {
            let controller: Address = env
                .storage()
                .instance()
                .get(&MockTargetKey::Controller)
                .expect("controller set");
            controller.require_auth();
            let calls: u32 = env
                .storage()
                .instance()
                .get(&MockTargetKey::CallCount)
                .expect("call count set");
            env.storage()
                .instance()
                .set(&MockTargetKey::CallCount, &calls + 1);
            env.storage().instance().set(&MockTargetKey::Cap, &cap);
        }

        pub fn call_count(env: Env) -> u32 {
            env.storage()
                .instance()
                .get(&MockTargetKey::CallCount)
                .unwrap_or(0)
        }

        pub fn cap(env: Env) -> i128 {
            env.storage()
                .instance()
                .get(&MockTargetKey::Cap)
                .unwrap_or(0)
        }
    }

    /// XDR-encode any `CallData` payload.
    fn op_calldata(env: &Env, op: CallData) -> Bytes {
        use soroban_sdk::xdr::ToXdr;
        op.to_xdr(env)
    }

    /// XDR-encode a batch payload of `calls`, in order.
    ///
    /// Takes the `Vec` by value: it becomes the `CallData::Batch` payload
    /// verbatim, so the test module hands over exactly the entries it built
    /// with the `vec!` macro.
    fn batch_calldata(env: &Env, calls: Vec<ExecutionCall>) -> Bytes {
        op_calldata(env, CallData::Batch(calls))
    }

    /// `count` entries that each apply `FactorySetCap(3)` to `target`.
    fn cap_entries(env: &Env, target: &Address, count: u32) -> Vec<ExecutionCall> {
        let mut entries = Vec::new(env);
        for _ in 0..count {
            entries.push_back(ExecutionCall {
                target: target.clone(),
                calldata: set_cap(env, 3),
            });
        }
        entries
    }

    /// One entry of a batch: `op` applied to `target`.
    fn entry(env: &Env, target: &Address, op: CallData) -> ExecutionCall {
        ExecutionCall {
            target: target.clone(),
            calldata: op_calldata(env, op),
        }
    }

    /// One entry of a batch with an arbitrary payload — used to build entries
    /// that must be refused (nested batch, undecodable bytes).
    fn entry_raw(target: &Address, calldata: Bytes) -> ExecutionCall {
        ExecutionCall {
            target: target.clone(),
            calldata,
        }
    }

    /// XDR-encode `FactorySetCap(cap)`.
    fn set_cap(env: &Env, cap: i128) -> Bytes {
        op_calldata(env, CallData::FactorySetCap(cap))
    }

    fn deploy_batch_target(ctx: &Ctx) -> Address {
        let target = ctx.env.register(BatchTarget, ());
        BatchTargetClient::new(&ctx.env, &target).init(&ctx.contract_id);
        target
    }

    fn deploy_flaky_target(ctx: &Ctx) -> Address {
        let target = ctx.env.register(FlakyTarget, ());
        FlakyTargetClient::new(&ctx.env, &target).init(&ctx.contract_id);
        target
    }

    /// Queue a proposal for `calldata` and advance past the timelock.
    fn queued(ctx: &Ctx, target: &Address, calldata: &Bytes) -> u32 {
        let id = ctx.client.propose(&ctx.signer_a, target, calldata);
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        id
    }

    /// A contract whose only purpose is the classic bypass shape: mutate a
    /// governed target directly, from outside governance.
    #[contract]
    pub struct RogueCaller;

    #[contractimpl]
    impl RogueCaller {
        pub fn try_set_cap(env: Env, target: Address) {
            env.invoke_contract::<()>(
                &target,
                &Symbol::new(&env, "set_cap"),
                (1i128,).into_val(&env),
            );
        }
    }

    /// XDR-encoded `CallData::FactorySetCap` payload.
    fn set_cap_calldata(env: &Env, cap: i128) -> Bytes {
        use soroban_sdk::xdr::ToXdr;
        CallData::FactorySetCap(cap).to_xdr(env)
    }

    /// Register a `MockTarget` governed by `ctx` and return its address.
    fn deploy_target(ctx: &Ctx) -> Address {
        let target = ctx.env.register(MockTarget, ());
        MockTargetClient::new(&ctx.env, &target).init(&ctx.contract_id);
        target
    }

    /// Propose a `set_cap` payload, bring it to quorum, and return its ID.
    fn queued_set_cap_proposal(ctx: &Ctx, target: &Address, cap: i128) -> u32 {
        let id = ctx
            .client
            .propose(&ctx.signer_a, target, &set_cap_calldata(&ctx.env, cap));
        ctx.client.approve(&ctx.signer_a, &id);
        ctx.client.approve(&ctx.signer_b, &id);
        id
    }

    /// The control case: a single operation still dispatches unchanged.
    #[test]
    fn test_single_call_proposal_still_dispatches() {
        let ctx = Ctx::setup();
        let target = deploy_batch_target(&ctx);
        let client = BatchTargetClient::new(&ctx.env, &target);
        let id = queued(&ctx, &target, &set_cap(&ctx.env, 42));

        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK);
        ctx.client.execute(&Address::generate(&ctx.env), &id);

        assert_eq!(client.cap(), 42);
        assert_eq!(client.applied(), vec![&ctx.env, OP_SET_CAP]);
    }

    /// Every entry of a batch is applied, in the order it was written.
    #[test]
    fn test_batch_applies_every_call_in_order() {
        let ctx = Ctx::setup();
        let target = deploy_batch_target(&ctx);
        let client = BatchTargetClient::new(&ctx.env, &target);
        let recipient = Address::generate(&ctx.env);
        let calls = vec![
            entry(&ctx.env, &target, CallData::FactorySetCap(500)),
            entry(
                &ctx.env,
                &target,
                CallData::FactorySetMinDuration(1_209_600),
            ),
            entry(
                &ctx.env,
                &target,
                CallData::FactorySetAllowlist(recipient, true),
            ),
            // A `Noop` entry is a legal no-op and must not disturb the others.
            entry(&ctx.env, &target, CallData::Noop),
        ];
        let calldata = batch_calldata(&ctx.env, calls);
        let id = queued(&ctx, &target, &calldata);

        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK);
        ctx.client.execute(&Address::generate(&ctx.env), &id);

        assert_eq!(client.cap(), 500);
        assert_eq!(client.min_duration(), 1_209_600);
        assert!(client.allowed());
        assert_eq!(
            client.applied(),
            vec![&ctx.env, OP_SET_CAP, OP_SET_MIN_DURATION, OP_SET_ALLOWLIST]
        );
        assert!(ctx.client.get_proposal(&id).executed);
    }

    /// One proposal, two targets: each entry carries its own address, so a
    /// policy update that spans contracts needs a single timelock cycle.
    #[test]
    fn test_batch_spans_multiple_targets() {
        let ctx = Ctx::setup();
        let target = deploy_batch_target(&ctx);
        let flaky = deploy_flaky_target(&ctx);
        let target_client = BatchTargetClient::new(&ctx.env, &target);
        let flaky_client = FlakyTargetClient::new(&ctx.env, &flaky);

        let calls = vec![
            entry(&ctx.env, &target, CallData::FactorySetCap(7)),
            entry(&ctx.env, &flaky, CallData::FactorySetCap(9)),
        ];
        let calldata = batch_calldata(&ctx.env, calls);
        let id = queued(&ctx, &target, &calldata);

        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK);
        ctx.client.execute(&Address::generate(&ctx.env), &id);

        assert_eq!(target_client.cap(), 7);
        assert_eq!(flaky_client.cap(), 9);
        assert!(ctx.client.get_proposal(&id).executed);
    }

    /// The atomicity property: a failing entry unwinds the entries before it.
    ///
    /// `set_cap` on the healthy target runs first and succeeds; the second
    /// entry traps. After the failed execution the healthy target must still
    /// read zero, the proposal must not be marked executed, no
    /// `proposal_executed` event may exist, and the proposal must still be
    /// executable.
    #[test]
    fn test_batch_reverts_every_call_when_one_fails() {
        let ctx = Ctx::setup();
        let target = deploy_batch_target(&ctx);
        let flaky = deploy_flaky_target(&ctx);
        let target_client = BatchTargetClient::new(&ctx.env, &target);
        let flaky_client = FlakyTargetClient::new(&ctx.env, &flaky);
        flaky_client.set_failing(&true);

        let calls = vec![
            entry(&ctx.env, &target, CallData::FactorySetCap(7)),
            entry(&ctx.env, &flaky, CallData::FactorySetCap(9)),
        ];
        let calldata = batch_calldata(&ctx.env, calls);
        let id = queued(&ctx, &target, &calldata);

        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK);
        let executor = Address::generate(&ctx.env);
        assert!(
            ctx.client.try_execute(&executor, &id).is_err(),
            "a trapping sub-call must fail the whole execution"
        );

        // The first entry's write is gone, not merely the second's.
        assert_eq!(target_client.cap(), 0);
        assert!(target_client.applied().is_empty());
        assert_eq!(flaky_client.cap(), 0);
        assert!(!ctx.client.get_proposal(&id).executed);
        assert_eq!(executed_event_count(&ctx.env, &ctx.contract_id), 0);
        // Still executable: a transient failure is retryable (#53).
        assert!(ctx.client.is_executable(&id));
    }

    /// A transient failure is retryable: once the downstream lock clears, the
    /// same proposal executes and every entry lands exactly once.
    #[test]
    fn test_batch_is_retryable_after_a_transient_failure() {
        let ctx = Ctx::setup();
        let target = deploy_batch_target(&ctx);
        let flaky = deploy_flaky_target(&ctx);
        let target_client = BatchTargetClient::new(&ctx.env, &target);
        let flaky_client = FlakyTargetClient::new(&ctx.env, &flaky);

        let calls = vec![
            entry(&ctx.env, &target, CallData::FactorySetCap(7)),
            entry(&ctx.env, &flaky, CallData::FactorySetCap(9)),
        ];
        let calldata = batch_calldata(&ctx.env, calls);
        let id = queued(&ctx, &target, &calldata);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK);
        let executor = Address::generate(&ctx.env);

        flaky_client.set_failing(&true);
        assert!(ctx.client.try_execute(&executor, &id).is_err());
        assert_eq!(target_client.cap(), 0);

        flaky_client.set_failing(&false);
        ctx.client.execute(&executor, &id);

        assert_eq!(target_client.cap(), 7);
        assert_eq!(flaky_client.cap(), 9);
        assert_eq!(target_client.applied(), vec![&ctx.env, OP_SET_CAP]);
        assert!(ctx.client.get_proposal(&id).executed);
        assert_eq!(executed_event_count(&ctx.env, &ctx.contract_id), 1);

        // A second execution cannot replay the batch.
        assert_eq!(
            ctx.client.try_execute(&executor, &id),
            Err(Ok(GovernanceError::AlreadyExecuted))
        );
        assert_eq!(target_client.applied(), vec![&ctx.env, OP_SET_CAP]);
    }

    /// Batching does not weaken the timelock: the whole batch waits for the eta.
    #[test]
    fn test_batch_still_respects_the_timelock() {
        let ctx = Ctx::setup();
        let target = deploy_batch_target(&ctx);
        let client = BatchTargetClient::new(&ctx.env, &target);
        let calls = vec![
            entry(&ctx.env, &target, CallData::FactorySetCap(7)),
            entry(
                &ctx.env,
                &target,
                CallData::FactorySetMinDuration(1_209_600),
            ),
        ];
        let calldata = batch_calldata(&ctx.env, calls);
        let id = queued(&ctx, &target, &calldata);

        assert_eq!(
            ctx.client.try_execute(&Address::generate(&ctx.env), &id),
            Err(Ok(GovernanceError::TimelockNotMet))
        );
        assert_eq!(client.cap(), 0);
        assert!(client.applied().is_empty());
    }

    #[test]
    fn test_empty_batch_is_rejected() {
        let ctx = Ctx::setup();
        let target = deploy_batch_target(&ctx);
        let calldata = batch_calldata(&ctx.env, Vec::new(&ctx.env));
        let id = queued(&ctx, &target, &calldata);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK);

        assert_eq!(
            ctx.client.try_execute(&Address::generate(&ctx.env), &id),
            Err(Ok(GovernanceError::InvalidCalldata))
        );
        assert!(BatchTargetClient::new(&ctx.env, &target)
            .applied()
            .is_empty());
    }

    /// The bound is inclusive: exactly `MAX_BATCH_CALLS` entries execute, and
    /// one more is refused before any target is reached.
    #[test]
    fn test_batch_is_bounded_by_max_batch_calls() {
        let ctx = Ctx::setup();
        let target = deploy_batch_target(&ctx);
        let client = BatchTargetClient::new(&ctx.env, &target);
        let executor = Address::generate(&ctx.env);

        let too_big = batch_calldata(
            &ctx.env,
            cap_entries(&ctx.env, &target, MAX_BATCH_CALLS + 1),
        );
        let big_id = queued(&ctx, &target, &too_big);
        let at_limit = batch_calldata(&ctx.env, cap_entries(&ctx.env, &target, MAX_BATCH_CALLS));
        let ok_id = queued(&ctx, &target, &at_limit);

        // Both proposals are queued at the same instant, so one clock advance
        // past the timelock makes them executable.
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK);

        assert_eq!(
            ctx.client.try_execute(&executor, &big_id),
            Err(Ok(GovernanceError::BatchTooLarge))
        );
        assert!(
            client.applied().is_empty(),
            "an over-limit batch must be refused before the first target call"
        );

        ctx.client.execute(&executor, &ok_id);

        assert_eq!(client.cap(), 3);
        assert_eq!(client.applied().len(), MAX_BATCH_CALLS);
        assert!(ctx.client.get_proposal(&ok_id).executed);
    }

    /// The cost envelope, measured where it can be measured without a network:
    /// a worst-case batch payload stays inside `MAX_CALLDATA_BYTES`, so the
    /// entry bound can never be widened by relaxing the calldata limit.
    #[test]
    fn test_max_batch_payload_fits_within_max_calldata_bytes() {
        let ctx = Ctx::setup();
        let target = deploy_batch_target(&ctx);
        // Largest inner payload governance can dispatch: an approved hash and salt.
        let worst = entry(
            &ctx.env,
            &target,
            CallData::FactoryDeployStreamContract(
                BytesN::from_array(&ctx.env, &[0x11; 32]),
                BytesN::from_array(&ctx.env, &[0x22; 32]),
            ),
        );
        let entries = vec![
            &ctx.env,
            worst.clone(),
            worst.clone(),
            worst.clone(),
            worst.clone(),
            worst,
        ];
        let payload = batch_calldata(&ctx.env, entries);

        assert!(
            payload.len() <= MAX_CALLDATA_BYTES,
            "a {}-call batch of the largest operation encodes to {} bytes, over the {}-byte limit",
            MAX_BATCH_CALLS,
            payload.len(),
            MAX_CALLDATA_BYTES
        );
    }

    /// Batches do not nest, so `MAX_BATCH_CALLS` cannot be side-stepped by
    /// hiding calls one level down.
    #[test]
    fn test_nested_batch_is_rejected() {
        let ctx = Ctx::setup();
        let target = deploy_batch_target(&ctx);
        let inner = batch_calldata(&ctx.env, cap_entries(&ctx.env, &target, 1));
        let calldata = batch_calldata(&ctx.env, vec![&ctx.env, entry_raw(&target, inner)]);
        let id = queued(&ctx, &target, &calldata);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK);

        assert_eq!(
            ctx.client.try_execute(&Address::generate(&ctx.env), &id),
            Err(Ok(GovernanceError::InvalidCalldata))
        );
        assert_eq!(BatchTargetClient::new(&ctx.env, &target).cap(), 0);
    }

    /// An entry whose payload is not a decodable `CallData` fails the batch
    /// before any target is reached.
    #[test]
    fn test_batch_entry_with_undecodable_calldata_is_rejected() {
        let ctx = Ctx::setup();
        let target = deploy_batch_target(&ctx);
        let junk = Bytes::from_slice(&ctx.env, &[0xff, 0x00, 0x01]);
        let calldata = batch_calldata(
            &ctx.env,
            vec![
                &ctx.env,
                entry(&ctx.env, &target, CallData::FactorySetCap(7)),
                entry_raw(&target, junk),
            ],
        );
        let id = queued(&ctx, &target, &calldata);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK);

        assert_eq!(
            ctx.client.try_execute(&Address::generate(&ctx.env), &id),
            Err(Ok(GovernanceError::InvalidCalldata))
        );
        assert_eq!(BatchTargetClient::new(&ctx.env, &target).cap(), 0);
        assert!(BatchTargetClient::new(&ctx.env, &target)
            .applied()
            .is_empty());
    }

    /// Plausible backdoor entrypoint names — none of them may exist.
    #[test]
    fn test_no_backdoor_entrypoint_is_reachable() {
        let ctx = Ctx::setup();
        for name in ["bypass", "dispatch", "forcecall", "raw_call", "force_exec"] {
            let sym = Symbol::new(&ctx.env, name);
            let args = (0u32,).into_val(&ctx.env);
            assert!(
                ctx.env
                    .try_invoke_contract::<(), InvokeError>(&ctx.contract_id, &sym, args)
                    .is_err(),
                "unexpected entrypoint {name} on the governance contract"
            );
        }
    }

    #[test]
    fn test_negative_execute_before_timelock_leaves_target_untouched() {
        let ctx = Ctx::setup();
        let target = deploy_target(&ctx);
        let target_client = MockTargetClient::new(&ctx.env, &target);
        let id = queued_set_cap_proposal(&ctx, &target, 42);

        let executor = Address::generate(&ctx.env);
        assert!(!ctx.client.is_executable(&id));
        assert_eq!(
            ctx.client.try_execute(&executor, &id),
            Err(Ok(GovernanceError::TimelockNotMet))
        );
        assert_eq!(target_client.call_count(), 0);
        assert_eq!(target_client.cap(), 0);
        assert!(!ctx.client.get_proposal(&id).executed);
    }

    /// No off-by-one window: the call is refused up to the last second.
    #[test]
    fn test_negative_execute_one_second_before_timelock_leaves_target_untouched() {
        let ctx = Ctx::setup();
        let target = deploy_target(&ctx);
        let target_client = MockTargetClient::new(&ctx.env, &target);
        let id = queued_set_cap_proposal(&ctx, &target, 42);

        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK - 1);
        let executor = Address::generate(&ctx.env);
        assert_eq!(
            ctx.client.try_execute(&executor, &id),
            Err(Ok(GovernanceError::TimelockNotMet))
        );
        assert_eq!(target_client.call_count(), 0);
    }

    #[test]
    fn test_negative_execute_without_quorum_leaves_target_untouched() {
        let ctx = Ctx::setup();
        let target = deploy_target(&ctx);
        let target_client = MockTargetClient::new(&ctx.env, &target);
        // A single approval against a threshold of two.
        let id = ctx
            .client
            .propose(&ctx.signer_a, &target, &set_cap_calldata(&ctx.env, 42));
        ctx.client.approve(&ctx.signer_a, &id);

        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&ctx.env);
        assert_eq!(
            ctx.client.try_execute(&executor, &id),
            Err(Ok(GovernanceError::QuorumNotReached))
        );
        assert_eq!(target_client.call_count(), 0);
    }

    #[test]
    fn test_negative_cancelled_and_executed_proposals_cannot_reach_target() {
        let ctx = Ctx::setup();
        let target = deploy_target(&ctx);
        let target_client = MockTargetClient::new(&ctx.env, &target);
        let executor = Address::generate(&ctx.env);

        // Two proposals, both queued at the same ledger timestamp.
        let cancelled = queued_set_cap_proposal(&ctx, &target, 7);
        let executed = queued_set_cap_proposal(&ctx, &target, 9);
        ctx.client.cancel_proposal(&ctx.signer_a, &cancelled);
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);

        // The cancelled one never reaches the target, even past the timelock.
        assert_eq!(
            ctx.client.try_execute(&executor, &cancelled),
            Err(Ok(GovernanceError::ProposalCancelled))
        );
        assert_eq!(target_client.call_count(), 0);

        // The surviving one runs exactly once.
        ctx.client.execute(&executor, &executed);
        assert_eq!(target_client.call_count(), 1);
        assert_eq!(target_client.cap(), 9);
        assert_eq!(
            ctx.client.try_execute(&executor, &executed),
            Err(Ok(GovernanceError::AlreadyExecuted))
        );
        assert_eq!(target_client.call_count(), 1);
    }

    /// A proposal that aged out cannot be revived by waiting longer.
    #[test]
    fn test_negative_expired_proposal_cannot_reach_target() {
        let ctx = Ctx::setup();
        let target = deploy_target(&ctx);
        let target_client = MockTargetClient::new(&ctx.env, &target);
        let id = queued_set_cap_proposal(&ctx, &target, 42);

        ctx.env.ledger().set_timestamp(1_000_000 + MAX_AGE + 1);
        let executor = Address::generate(&ctx.env);
        assert_eq!(
            ctx.client.try_execute(&executor, &id),
            Err(Ok(GovernanceError::ProposalExpired))
        );
        assert_eq!(target_client.call_count(), 0);
        assert_eq!(target_client.cap(), 0);
    }

    #[test]
    fn test_negative_non_signer_cannot_propose_or_approve() {
        let ctx = Ctx::setup();
        let target = deploy_target(&ctx);
        let target_client = MockTargetClient::new(&ctx.env, &target);
        let stranger = Address::generate(&ctx.env);

        assert_eq!(
            ctx.client
                .try_propose(&stranger, &target, &set_cap_calldata(&ctx.env, 42)),
            Err(Ok(GovernanceError::NotASigner))
        );

        let id = ctx
            .client
            .propose(&ctx.signer_a, &target, &set_cap_calldata(&ctx.env, 42));
        assert_eq!(
            ctx.client.try_approve(&stranger, &id),
            Err(Ok(GovernanceError::NotASigner))
        );

        // The stranger's failed approval did not manufacture quorum.
        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK + 1);
        let executor = Address::generate(&ctx.env);
        assert_eq!(
            ctx.client.try_execute(&executor, &id),
            Err(Ok(GovernanceError::QuorumNotReached))
        );
        assert_eq!(target_client.call_count(), 0);
    }

    #[test]
    fn test_negative_admin_cannot_bypass_the_timelock() {
        let ctx = Ctx::setup();
        let target = deploy_target(&ctx);
        let target_client = MockTargetClient::new(&ctx.env, &target);

        // The admin is not a signer by default, so it cannot even propose.
        assert!(!ctx.client.is_signer(&ctx.admin));
        assert_eq!(
            ctx.client
                .try_propose(&ctx.admin, &target, &set_cap_calldata(&ctx.env, 42)),
            Err(Ok(GovernanceError::NotASigner))
        );

        // Nor can it widen its own powers into a shortcut: being an emergency
        // guardian buys cancellation, never early execution.
        ctx.client
            .set_emergency_guardians(&vec![&ctx.env, ctx.admin.clone()]);

        // With every auth mocked, the admin still cannot skip the timelock.
        let id = queued_set_cap_proposal(&ctx, &target, 42);
        assert_eq!(
            ctx.client.try_execute(&ctx.admin, &id),
            Err(Ok(GovernanceError::TimelockNotMet))
        );
        assert_eq!(target_client.call_count(), 0);

        // Nor can it cancel its way to an immediate dispatch.
        ctx.client.cancel_proposal(&ctx.admin, &id);
        assert_eq!(
            ctx.client.try_execute(&ctx.admin, &id),
            Err(Ok(GovernanceError::ProposalCancelled))
        );
        assert_eq!(target_client.call_count(), 0);
    }

    /// A third-party contract cannot move governed state directly: the target's
    /// own controller check rejects the call because the rogue caller cannot
    /// produce the governance contract's authorization.
    ///
    /// This test deliberately uses an env **without** `mock_all_auths`: with
    /// every authorization mocked, `require_auth` is a no-op and the negative
    /// case could not be observed at all.
    #[test]
    fn test_negative_rogue_contract_cannot_mutate_target_directly() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let contract_id = env.register_contract(None, FluxoraGovernance);
        let admin = Address::generate(&env);
        let signer = Address::generate(&env);
        FluxoraGovernanceClient::new(&env, &contract_id).init(&admin, &vec![&env, signer], &1u32);

        // The mock is governed by the governance contract. `init` needs no
        // authorization; only the setter does.
        let target = env.register(MockTarget, ());
        MockTargetClient::new(&env, &target).init(&contract_id);
        let target_client = MockTargetClient::new(&env, &target);

        // An unrelated contract cannot move the target's state.
        let rogue = env.register(RogueCaller, ());
        let args = (target.clone(),).into_val(&env);
        let res = env.try_invoke_contract::<(), InvokeError>(
            &rogue,
            &Symbol::new(&env, "try_set_cap"),
            args,
        );
        assert!(res.is_err(), "rogue call must be rejected by the target");
        assert_eq!(target_client.cap(), 0);
        assert_eq!(target_client.call_count(), 0);
    }

    /// Positive counterpart: after the timelock, `execute` is the path that
    /// actually mutates the governed target, and it does so exactly once.
    #[test]
    fn test_positive_execute_is_the_only_path_that_mutates_the_target() {
        let ctx = Ctx::setup();
        let target = deploy_target(&ctx);
        let target_client = MockTargetClient::new(&ctx.env, &target);
        let id = queued_set_cap_proposal(&ctx, &target, 42);

        ctx.env.ledger().set_timestamp(1_000_000 + TIMELOCK);
        assert!(ctx.client.is_executable(&id));
        let executor = Address::generate(&ctx.env);
        ctx.client.execute(&executor, &id);

        assert_eq!(target_client.call_count(), 1);
        assert_eq!(target_client.cap(), 42);
        assert!(ctx.client.get_proposal(&id).executed);
    }

    #[test]
    fn test_governance_error_discriminants_and_observability() {
        let variants = [
            GovernanceError::NotInitialized,
            GovernanceError::AlreadyInitialized,
            GovernanceError::Unauthorized,
            GovernanceError::NotASigner,
            GovernanceError::ProposalNotFound,
            GovernanceError::AlreadyExecuted,
            GovernanceError::QuorumNotReached,
            GovernanceError::TimelockNotElapsed,
            GovernanceError::AlreadyApproved,
            GovernanceError::CalldataTooLarge,
            GovernanceError::TooManySigners,
            GovernanceError::ProposalExpired,
            GovernanceError::ProposalCancelled,
            GovernanceError::NotProposerOrAdmin,
            GovernanceError::InvalidThreshold,
            GovernanceError::QuorumWouldBreak,
            GovernanceError::DuplicateSigner,
            GovernanceError::ArithmeticOverflow,
            GovernanceError::CalldataEmpty,
            GovernanceError::InvalidCalldata,
            GovernanceError::TimelockNotMet,
            GovernanceError::InvalidSignerGeneration,
            GovernanceError::ReentrancyGuard,
            GovernanceError::TooManyEmergencyGuardians,
            GovernanceError::DuplicateEmergencyGuardian,
            GovernanceError::InvalidProposalState,
            GovernanceError::BatchTooLarge,
        ];

        for (i, a) in variants.iter().enumerate() {
            let code_a = *a as u32;
            assert!(code_a > 0, "governance error discriminants must be non-zero");
            for b in variants.iter().skip(i + 1) {
                assert_ne!(code_a, *b as u32, "duplicate GovernanceError discriminant");
            }
        }
    }
}

