#!/usr/bin/env python3
"""Static cost model for the governance contract's vote and execute paths.

Issue #54 asks for governance gas profiling: instruction counts and event
bytes for 10-signer multi-sig operations, with multi-sig voting under 1M CPU
instructions per vote.

**This script does not measure instructions.** It cannot. Instruction counts
come from `soroban-env-host`'s metering, which requires executing the contract
on a host. Instead it derives the *operation counts* that metering charges for
- storage reads, storage writes, TTL bumps, ledger reads and event emissions -
by reading `contracts/governance/src/lib.rs` and resolving the helper call
graph of each entrypoint.

Operation counts are the honest, checkable part of the question. They are
deterministic, derived from the source, and they are what actually determines
whether a path stays inside a ledger's limits. A `cargo test` benchmark that
reports raw instruction counts is still required before quoting a number; this
script exists so that benchmark has a second, independent check to agree with,
and so the *shape* of the cost is documented rather than guessed.

What it does:

  1. Parse `contracts/governance/src/lib.rs`.
  2. Build a call graph over the contract's free helper functions.
  3. For `propose`, `approve` and `execute`, walk the graph and count the leaf
     storage operations, separating the steady-state cost of a vote from the
     quorum-reaching branch in `approve`.
  4. Check the structural claims that matter for scaling: that per-vote cost
     does not grow with the number of signers, and that the approval index is
     a Map lookup (O(1)) rather than a scan of the approvals Vec (O(n)).

Exit codes (the stable contract tests assert on):

  0 = all checks pass
  1 = a structural check failed (the report is still emitted)
  2 = could not run (missing `GOV_LIB`, unparseable source, malformed input)

`--json` emits a stable schema with four keys:

  ``ok``        bool  - true when no structural problems were found
  ``report``    dict  - per-entrypoint operation counts (empty on exit 2)
  ``problems``  list  - human-readable structural problems (empty on pass)
  ``exit_code`` int   - the process exit code, mirroring the contract above

Error paths are explicit and testable: `ParseError` carries a stable message
for a missing or unterminated function, `check_constant` reports malformed
constants rather than raising, and `main` returns 2 (never raises) when
`GOV_LIB` is absent.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

# Exit-code contract. Tests import these rather than hard-coding integers.
EXIT_OK = 0
EXIT_CHECK_FAILED = 1
EXIT_COULD_NOT_RUN = 2

REPO_ROOT = Path(__file__).resolve().parent.parent
GOV_LIB = REPO_ROOT / "contracts" / "governance" / "src" / "lib.rs"
DOCS_ABI = REPO_ROOT / "docs" / "ABI.md"

# Soroban host limits (Protocol 27). These bound a whole transaction, not one
# call, so they are used here as ceilings a single path must stay under.
MAX_INSTRUCTIONS_PER_TX = 100_000_000
MAX_LEDGER_READ_BYTES = 2 * 1024 * 1024
MAX_LEDGER_WRITE_BYTES = 2 * 1024 * 1024
MAX_EVENT_BYTES_PER_TX = 16_384

# The per-vote budget issue #54 states.
VOTE_INSTRUCTION_BUDGET = 1_000_000

# Constants read out of lib.rs, so the model cannot drift from the contract.
MAX_SIGNERS = 20
MAX_CALLDATA_BYTES = 4_096
MAX_PAGE_SIZE = 100
PROPOSAL_SIGS = 10  # the scenario the issue asks about


class ParseError(RuntimeError):
    """Raised when lib.rs cannot be parsed well enough to analyse."""


def strip_comments(source: str) -> str:
    """Remove `//` and `/* */` comments, honouring string literals.

    Block comments must go too: a `(` or `{` inside a doc comment would
    otherwise desynchronise the brace matcher in `extract_function` and make
    it report a real function as unterminated.

    Implemented as a single pass over the whole text (rather than
    line-by-line) so that a block comment opening and closing on one line is
    handled without duplicating output.
    """
    out: list[str] = []
    i = 0
    n = len(source)
    in_str = False
    quote = ""

    while i < n:
        ch = source[i]

        if in_str:
            out.append(ch)
            if ch == "\\" and i + 1 < n:
                out.append(source[i + 1])
                i += 2
                continue
            if ch == quote:
                in_str = False
            i += 1
            continue

        # Line comment: drop to end of line, keep the newline itself so line
        # numbering in later diagnostics stays aligned with the input.
        if ch == "/" and i + 1 < n and source[i + 1] == "/":
            while i < n and source[i] != "\n":
                i += 1
            continue

        # Block comment: drop everything through the closing delimiter.
        if ch == "/" and i + 1 < n and source[i + 1] == "*":
            end = source.find("*/", i + 2)
            if end == -1:
                return "".join(out)  # unterminated: drop the remainder
            # Preserve the newlines the comment spanned, so that line numbers
            # still line up with the original file.
            out.append("\n" * source.count("\n", i, end))
            i = end + 2
            continue

        if ch in ('"', "'"):
            in_str = True
            quote = ch

        out.append(ch)
        i += 1

    return "".join(out)


def extract_function(source: str, name: str) -> str:
    """Return the brace-matched body of `fn name(...)`.

    Works for free functions and `impl` methods alike, which is what lets the
    call graph span `approve` -> `load_proposal` -> `bump_proposal`.
    """
    match = re.search(rf"\bfn\s+{re.escape(name)}\s*(?:<[^>]*>)?\s*\(", source)
    if not match:
        raise ParseError(f"function `{name}` not found in lib.rs")

    # Walk forward past the parameter list and any return type to the body's
    # opening brace. Depth starts at 1 because the opening paren of the
    # signature is already behind us. A `;` before the brace means this is a
    # declaration (a trait method or extern block) with no body here.
    idx = match.end()
    depth = 1
    started = False
    while idx < len(source):
        ch = source[idx]
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
            if depth == 0:
                j = idx + 1
                while j < len(source):
                    if source[j] == "{":
                        started = True
                        idx = j
                        break
                    if source[j] == ";":
                        break
                    j += 1
                break
        idx += 1

    if not started:
        raise ParseError(f"could not find body of `{name}`")

    depth = 0
    start = idx
    while idx < len(source):
        ch = source[idx]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return source[start : idx + 1]
        idx += 1
    raise ParseError(f"unterminated body for `{name}`")


# Leaf operation patterns. Each is one thing Soroban meters independently of
# raw instruction count.
COSTS = [
    ("instance_read", r"\.instance\(\)\s*\.get::<"),
    ("instance_write", r"\.instance\(\)\s*\.set\("),
    ("instance_bump", r"\.instance\(\)\s*\.extend_ttl\("),
    ("persistent_read", r"\.persistent\(\)\s*\.get::<"),
    ("persistent_write", r"\.persistent\(\)\s*\.set\("),
    ("persistent_bump", r"\.persistent\(\)\s*\.extend_ttl\("),
    ("persistent_exists", r"\.persistent\(\)\s*\.has\("),
    ("ledger_read", r"\.ledger\(\)\s*\.timestamp\(\)"),
    ("event_emit", r"\.events\(\)\s*\.publish\("),
]

HELPER_RE = re.compile(r"\bfn\s+(\w+)\s*(?:<[^>]*>)?\s*\(")
CALL_RE = re.compile(r"\b([a-z_][a-z0-9_]*)\s*\(")
KEYWORDS = {
    "if", "match", "while", "for", "return", "let", "fn", "impl", "mod",
    "pub", "const", "struct", "enum", "use", "Some", "None", "Ok", "Err",
}


def helper_names(source: str) -> set[str]:
    return set(HELPER_RE.findall(source))


def count_ops(body: str) -> dict[str, int]:
    """Count leaf storage/ledger/event operations in one function body."""
    counts: dict[str, int] = {}
    for label, pattern in COSTS:
        n = len(re.findall(pattern, body))
        if n:
            counts[label] = n
    return counts


def called_helpers(body: str, known: set[str]) -> set[str]:
    """Free-function calls in `body` that resolve to a known helper.

    Method calls (`.foo(`) are excluded, so only real intra-crate calls are
    followed.
    """
    return {
        name
        for name in CALL_RE.findall(body)
        if name in known and name not in KEYWORDS
    }


def walk(
    source: str, entry: str, known: set[str], seen: set[str] | None = None
) -> dict[str, int]:
    """Transitively count operations reachable from `entry`.

    A helper already visited on this path is not re-counted: each helper is a
    distinct storage key, so its cost is counted once per call site chain.
    """
    if seen is None:
        seen = set()
    if entry in seen:
        return {}
    seen.add(entry)

    body = extract_function(source, entry)
    total = count_ops(body)
    for helper in called_helpers(body, known):
        for key, val in walk(source, helper, known, seen).items():
            total[key] = total.get(key, 0) + val
    return total


def enclosing_if(body: str, needle: str) -> bool:
    """True when `needle` occurs inside a block opened by an `if`.

    Scans backwards from the occurrence, tracking brace depth to find the
    innermost enclosing block, then checks whether the statement that opened it
    begins with `if`. This is robust to `if let` and to nested blocks, which a
    regex over the preceding text gets wrong.
    """
    for match in re.finditer(re.escape(needle), body):
        depth = 0
        i = match.start() - 1
        while i >= 0:
            ch = body[i]
            if ch == "}":
                depth += 1
            elif ch == "{":
                if depth == 0:
                    # This `{` opens the block containing the occurrence.
                    # Walk back to the start of the statement that opened it.
                    j = i
                    while j > 0 and body[j - 1] not in ";}{":
                        j -= 1
                    stmt = body[j:i].strip()
                    if re.match(r"^(if|else\s+if)\b", stmt):
                        return True
                    # Not an `if` block; keep looking further out.
                    depth = 0
                    i -= 1
                    continue
                depth -= 1
            i -= 1
    return False


def reachable_text(source: str, entry: str, known: set[str], depth: int = 3) -> str:
    """Concatenate `entry`'s body with the bodies of the helpers it calls.

    Used by the structural checks so that a fact proven in a helper (for
    example that `get_signer_index` reads `DataKey::SignerIndex`) counts as a
    fact about the caller, rather than being reported as a missing check.
    """
    seen: set[str] = set()
    parts: list[str] = []

    def visit(name: str, level: int) -> None:
        if name in seen or level < 0:
            return
        seen.add(name)
        try:
            body = extract_function(source, name)
        except ParseError:
            return
        parts.append(body)
        if level == 0:
            return
        for helper in called_helpers(body, known):
            visit(helper, level - 1)

    visit(entry, depth)
    return "\n".join(parts)


# ---------------------------------------------------------------------------
# Event byte sizing
# ---------------------------------------------------------------------------
#
# The event budget is a separate limit from instructions: Protocol 27 caps the
# SUM of all contract events in a transaction at 16,384 bytes. Governance
# emits one `vote_cast` per signer, so a 10-signer round emits 10 of them.
#
# Sizes are computed from the SCVal definitions in Stellar-contract.x, using
# the same encoding model as script/event-byte-budget.py:
#   ContractEvent = ext(4) + contractID ScVal::Address(40)
#                 + topics SCVec (disc 4 + ptr 4 + count 4)
#                 + data   ScVal (an ScVal::Map: disc 4 + ptr 4 + count 4)
# Every body is XDR-padded to a multiple of 4.

EVENT_FRAME = 4 + 40
SC_ADDRESS = 4 + 4 + 32


def pad4(n: int) -> int:
    return n + ((4 - n % 4) % 4)


def sc_symbol(name: str) -> int:
    # SCSymbol is `string<SCSYMBOL_LIMIT=32>`.
    assert len(name) <= 32, f"symbol too long: {name}"
    return 4 + 4 + pad4(len(name))


def sc_u32(_v: int) -> int:
    return 4 + 4


def sc_u64(_v: int) -> int:
    return 4 + 8


def sc_address(_v=None) -> int:
    return SC_ADDRESS


def sc_vec(elements: list[int]) -> int:
    return 4 + 4 + 4 + sum(elements)


def sc_map(pairs: list[tuple[int, int]]) -> int:
    return 4 + 4 + 4 + sum(pad4(k) + pad4(v) for k, v in pairs)


def event_bytes(topic_elements: list[int], data_pairs: list[tuple[str, int]]) -> int:
    """Total encoded size of one ContractEvent.

    `topic_elements` must be the *encoded sizes* of the topic values, in
    order. Topics are not all symbols: `vote_cast` publishes
    `(symbol_short!("vote_cast"), proposal_id, approver)`, so the second topic
    is a u32 and the third an Address. Deriving them from names would
    mis-type every non-symbol topic.
    """
    topics = sc_vec(topic_elements)
    data = sc_map([(sc_symbol(k), v) for k, v in data_pairs])
    return EVENT_FRAME + topics + data


# The two events a multi-signer voting round emits.
#
# vote_cast topics: (vote_cast symbol, proposal_id u32, approver Address)
VOTE_CAST_BYTES = event_bytes(
    [sc_symbol("vote_cast"), sc_u32(0), sc_address()],
    [
        ("proposal_id", sc_u32(0)),
        ("voter", sc_address()),
        ("approval_count", sc_u32(0)),
        ("timestamp", sc_u64(0)),
    ],
)

# proposal_queued topics: (proposal_queued symbol, proposal_id u32)
PROPOSAL_QUEUED_BYTES = event_bytes(
    [sc_symbol("proposal_queued"), sc_u32(0)],
    [
        ("proposal_id", sc_u32(0)),
        ("quorum_reached_at", sc_u64(0)),
        ("executable_after", sc_u64(0)),
        ("threshold", sc_u32(0)),
    ],
)


def voting_round_event_bytes(signers: int = PROPOSAL_SIGS) -> dict[str, int]:
    """Event bytes for a full round: N votes, one queue, one proposal."""
    votes = VOTE_CAST_BYTES * signers
    return {
        "vote_cast_each": VOTE_CAST_BYTES,
        "votes_total": votes,
        "proposal_queued": PROPOSAL_QUEUED_BYTES,
        "total": votes + PROPOSAL_QUEUED_BYTES,
    }


# ---------------------------------------------------------------------------
# Structural checks
# ---------------------------------------------------------------------------


def check_constant(source: str, name: str, expected: int) -> str | None:
    """Confirm a contract constant still has the value the model assumes.

    Returns a problem string (never raises) for a missing constant or one whose
    literal is malformed - for example an empty or non-numeric value - so the
    caller can report it as a check failure rather than crashing.
    """
    m = re.search(
        rf"\bconst\s+{re.escape(name)}\s*:\s*\w+\s*=\s*([^;]*);", source
    )
    if not m:
        return f"constant {name} not found in lib.rs"
    raw = m.group(1).strip()
    digits = raw.replace("_", "")
    if not digits or not digits.isdigit():
        return f"constant {name} has malformed value `{raw}`"
    actual = int(digits)
    if actual != expected:
        return f"constant {name} is {actual}, model assumes {expected}"
    return None


def check_vote_cost_is_signer_independent(source: str) -> list[str]:
    """A vote must not scan the signer set.

    `approve` checks membership through `is_registered_signer`, which reads the
    `SignerIndex` Map via `get_signer_index`. If that regressed to a linear scan
    of `Signers`, cost would grow with the signer count and the per-vote budget
    would become a function of deployment configuration rather than a fixed
    property of the contract.
    """
    problems = []
    known = helper_names(source)
    approve = extract_function(source, "approve")

    if "is_registered_signer" not in approve:
        problems.append(
            "approve no longer checks membership via is_registered_signer; if "
            "this became a linear scan over Signers, per-vote cost would grow "
            "with the signer count"
        )
        return problems

    # Follow the call: the SignerIndex read lives in get_signer_index, so the
    # check has to consider what is_registered_signer reaches, not just itself.
    reachable = reachable_text(source, "is_registered_signer", known)
    if "SignerIndex" not in reachable:
        problems.append(
            "is_registered_signer no longer reaches the SignerIndex Map"
        )
    if "contains_key" not in reachable:
        problems.append(
            "is_registered_signer no longer uses an O(1) Map::contains_key"
        )
    return problems


def check_duplicate_approval_is_o1(source: str) -> list[str]:
    """Duplicate detection must use the per-proposal Map, not the Vec."""
    problems = []
    approve = extract_function(source, "approve")
    if "get_approval_index" not in approve:
        problems.append(
            "approve no longer consults the per-proposal approval index; a "
            "linear scan of the approvals Vec would make the n-th vote O(n)"
        )
    if "contains_key" not in approve:
        problems.append("approve no longer uses O(1) duplicate detection")
    # A regression to a positional scan would show up as iterating approvals.
    for bad in ("approvals.iter()", "approvals.get(", "for _a in"):
        if bad in approve:
            problems.append(
                f"approve scans the approvals Vec (`{bad}`); per-vote cost "
                "would become O(n) in the number of approvals"
            )
    return problems


def check_quorum_branch_is_conditional(source: str) -> list[str]:
    """The extra quorum work in `approve` must stay off the steady-state path.

    Every vote beyond the first pays the same cost; only the vote that reaches
    threshold additionally writes QuorumReachedAt and emits `proposal_queued`.
    If that work were unconditional, all 10 votes in a 10-signer round would
    pay for it.
    """
    problems = []
    approve = extract_function(source, "approve")
    if "QuorumReachedAt" not in approve:
        return [
            "approve no longer records QuorumReachedAt; the timelock anchor "
            "would be lost"
        ]
    if "proposal_queued" not in approve:
        problems.append("approve no longer emits the proposal_queued event")
    if not enclosing_if(approve, "QuorumReachedAt"):
        problems.append(
            "the QuorumReachedAt write is no longer inside a conditional; if "
            "it became unconditional every vote would pay the quorum cost"
        )
    if not enclosing_if(approve, "proposal_queued"):
        problems.append(
            "the proposal_queued emit is no longer inside a conditional; if "
            "it became unconditional every vote would pay the quorum cost"
        )
    return problems


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------

READ_OPS = {"instance_read", "persistent_read", "persistent_exists", "ledger_read"}
WRITE_OPS = {"instance_write", "persistent_write", "instance_bump",
             "persistent_bump", "event_emit"}


def summarise(counts: dict[str, int]) -> dict[str, int]:
    reads = sum(v for k, v in counts.items() if k in READ_OPS)
    writes = sum(v for k, v in counts.items() if k in WRITE_OPS)
    return {"reads": reads, "writes": writes, "total_ops": reads + writes}


def analyse(source: str) -> dict:
    """Produce the full cost report for the three entrypoints."""
    known = helper_names(source)
    report = {}
    for entry in ("propose", "approve", "execute"):
        counts = walk(source, entry, known)
        report[entry] = {"counts": counts, **summarise(counts)}
    return report


def check_event_budget() -> list[str]:
    """A full voting round must fit the per-transaction event byte limit.

    Governance emits one `vote_cast` per signer. A 10-signer round is the
    scenario issue #54 names; the contract permits up to `MAX_SIGNERS = 20`,
    so the 20-signer case is checked too.
    """
    problems = []
    for signers in (PROPOSAL_SIGS, MAX_SIGNERS):
        total = voting_round_event_bytes(signers)["total"]
        if total > MAX_EVENT_BYTES_PER_TX:
            problems.append(
                f"a {signers}-signer voting round emits {total} event bytes, "
                f"over the {MAX_EVENT_BYTES_PER_TX}-byte limit"
            )
    return problems


# ---------------------------------------------------------------------------
# Archival detection & footprint restoration guidance
# Idempotent retry & invocation recovery
# ---------------------------------------------------------------------------

STATE_LIVE = "Live"
STATE_ARCHIVED = "Archived"
STATE_MISSING = "Missing"

RESTORE_FOOTPRINT_TOOL = "tools/sdk/restore_footprint.ts"

ERR_OK = "OK"
ERR_ARCHIVED_ENTRY = "ERR_ARCHIVED_ENTRY"
ERR_INVALID_INPUT = "ERR_INVALID_INPUT"
ERR_RETRY_EXHAUSTED = "ERR_RETRY_EXHAUSTED"


def detect_archival_state(
    proposal_ids: list[int] | tuple[int, ...],
    liveness: dict[int, bool] | None = None,
    proposal_count: int | None = None,
) -> dict:
    """Classify each proposal id as ``Live``, ``Archived``, or ``Missing``.

    ``liveness`` maps a proposal id to a boolean indicating whether the
    proposal's persistent entry (e.g. ``Proposal(id)``) is currently live.
    ``proposal_count`` is the contract's total proposal count. When supplied,
    an id below ``proposal_count`` whose entry is not live is classified as
    ``Archived`` (Soroban state archival condition). An id at or above
    ``proposal_count`` is classified as ``Missing``.

    Returns ``{"status": <code>, "states": {id: state}, "archived": [...]}``.
    Never raises on malformed input; returns ``ERR_INVALID_INPUT``.
    """
    if not isinstance(proposal_ids, (list, tuple)):
        return {"status": ERR_INVALID_INPUT, "states": {}, "archived": []}
    if liveness is not None and not isinstance(liveness, dict):
        return {"status": ERR_INVALID_INPUT, "states": {}, "archived": []}

    live_map = liveness or {}
    states: dict[int, str] = {}
    archived: list[int] = []

    for pid in proposal_ids:
        if not isinstance(pid, int) or isinstance(pid, bool) or pid < 0:
            return {"status": ERR_INVALID_INPUT, "states": {}, "archived": []}
        is_live = live_map.get(pid)
        if is_live is True:
            states[pid] = STATE_LIVE
        elif proposal_count is not None and pid < proposal_count:
            states[pid] = STATE_ARCHIVED
            archived.append(pid)
        else:
            states[pid] = STATE_MISSING

    status = ERR_ARCHIVED_ENTRY if archived else ERR_OK
    return {"status": status, "states": states, "archived": archived}


def restoration_guidance(archived_ids: list[int] | tuple[int, ...]) -> dict:
    """Return footprint restoration guidance for archived proposal ids.

    Guides operators on restoring archived persistent storage entries using
    the SDK restoration tool (``tools/sdk/restore_footprint.ts``) before
    re-attempting governance operations.
    """
    if not isinstance(archived_ids, (list, tuple)):
        return {"status": ERR_INVALID_INPUT, "steps": []}
    ids = sorted({pid for pid in archived_ids if isinstance(pid, int) and pid >= 0})
    if not ids:
        return {"status": ERR_OK, "steps": []}

    return {
        "status": ERR_ARCHIVED_ENTRY,
        "tool": RESTORE_FOOTPRINT_TOOL,
        "proposal_ids": ids,
        "footprint_keys": [f"Proposal({pid})" for pid in ids] + [f"QuorumReachedAt({pid})" for pid in ids],
        "commands": [
            f"stellar contract restore --id <GOVERNANCE_CONTRACT_ID> --key '[\"Proposal\", {pid}]'"
            for pid in ids
        ],
        "steps": [
            f"1. Generate footprint restoration transaction for proposal entries: {ids} using {RESTORE_FOOTPRINT_TOOL}.",
            "2. Submit RestoreFootprintOp on-chain to restore the archived entries from state archival into live storage.",
            "3. Re-run --check-archival to verify that all proposal entries report 'Live'.",
            "4. Re-submit the voting or execution transaction. Proposal IDs are immutable, ensuring idempotent retry.",
        ],
    }



def idempotent_retry(
    proposal_ids: list[int] | tuple[int, ...],
    pre_state: dict,
    operation: callable,
    max_attempts: int = 3,
) -> dict:
    """Run ``operation`` retry-safely, keyed on stable proposal ids.

    ``pre_state`` is the mapping returned by archival detection.
    The operation is only invoked when every proposal id is in ``Live`` state;
    otherwise the pre-state is returned without invoking mutation.
    ``operation`` is called as ``operation(proposal_ids)`` and must itself be
    idempotent.

    Returns ``{"status", "attempts", "result", "pre_state"}``. Never raises.
    """
    if not isinstance(proposal_ids, (list, tuple)) or not callable(operation):
        return {
            "status": ERR_INVALID_INPUT,
            "attempts": 0,
            "result": None,
            "pre_state": pre_state,
        }
    if not isinstance(pre_state, dict) or "states" not in pre_state:
        return {
            "status": ERR_INVALID_INPUT,
            "attempts": 0,
            "result": None,
            "pre_state": pre_state,
        }
    if not isinstance(max_attempts, int) or max_attempts < 1:
        return {
            "status": ERR_INVALID_INPUT,
            "attempts": 0,
            "result": None,
            "pre_state": pre_state,
        }

    # Verify that all target proposal IDs are live
    non_live = [
        pid
        for pid in proposal_ids
        if pre_state.get("states", {}).get(pid) != STATE_LIVE
    ]
    if non_live:
        status = (
            ERR_ARCHIVED_ENTRY
            if any(
                pre_state.get("states", {}).get(p) == STATE_ARCHIVED
                for p in non_live
            )
            else ERR_INVALID_INPUT
        )
        return {
            "status": status,
            "attempts": 0,
            "result": None,
            "pre_state": pre_state,
        }

    attempts = 0
    last_exc = None
    for attempt in range(1, max_attempts + 1):
        attempts = attempt
        try:
            res = operation(proposal_ids)
            return {
                "status": ERR_OK,
                "attempts": attempts,
                "result": res,
                "pre_state": pre_state,
            }
        except Exception as exc:
            last_exc = str(exc)

    return {
        "status": ERR_RETRY_EXHAUSTED,
        "attempts": attempts,
        "result": None,
        "error": last_exc,
        "pre_state": pre_state,
    }

# ---------------------------------------------------------------------------
# ABI Event Schema Verification
# ---------------------------------------------------------------------------

GOVERNANCE_ABI_SCHEMAS = {
    "vote_cast": {
        "topics": ["proposal_id", "approver"],
        "payload": ["proposal_id", "voter", "approval_count", "timestamp"],
    },
    "proposal_queued": {
        "topics": ["proposal_id"],
        "payload": [
            "proposal_id",
            "quorum_reached_at",
            "executable_after",
            "threshold",
        ],
    },
    "proposal_created": {
        "topics": ["proposal_id", "proposer"],
        "payload": ["proposal_id", "proposer", "target", "created_at"],
    },
    "proposal_executed": {
        "topics": ["proposal_id", "executor"],
        "payload": ["proposal_id", "executor", "target", "calldata"],
    },
    "proposal_cancelled": {
        "topics": ["proposal_id", "caller"],
        "payload": ["proposal_id", "canceller", "cancelled_at"],
    },
}


def verify_governance_abi_schemas(abi_path: Path = DOCS_ABI) -> list[str]:
    """Verify topic and data payload schemas against docs/ABI.md."""
    if not abi_path.exists():
        return [f"docs/ABI.md not found at {abi_path}"]

    text = abi_path.read_text(encoding="utf-8")
    problems: list[str] = []

    if "## Governance Contract Events ABI" not in text:
        problems.append("docs/ABI.md is missing the Governance Contract Events ABI section")
        return problems

    for event_name, schema in GOVERNANCE_ABI_SCHEMAS.items():
        if f"`{event_name}`" not in text:
            problems.append(f"event `{event_name}` missing from docs/ABI.md governance events table")
            continue

        for topic in schema["topics"]:
            if topic not in text:
                problems.append(f"topic `{topic}` for `{event_name}` missing from docs/ABI.md")

        for field in schema["payload"]:
            if field not in text:
                problems.append(f"payload field `{field}` for `{event_name}` missing from docs/ABI.md")

    return problems


def run_checks() -> tuple[dict, list[str]]:
    source = strip_comments(GOV_LIB.read_text(encoding="utf-8"))
    problems: list[str] = []

    for name, expected in (
        ("MAX_SIGNERS", MAX_SIGNERS),
        ("MAX_CALLDATA_BYTES", MAX_CALLDATA_BYTES),
        ("MAX_PAGE_SIZE", MAX_PAGE_SIZE),
    ):
        problem = check_constant(source, name, expected)
        if problem:
            problems.append(problem)

    problems.extend(check_vote_cost_is_signer_independent(source))
    problems.extend(check_duplicate_approval_is_o1(source))
    problems.extend(check_quorum_branch_is_conditional(source))
    problems.extend(check_event_budget())
    problems.extend(verify_governance_abi_schemas())

    return analyse(source), problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true", help="emit JSON")
    parser.add_argument(
        "--check-archival",
        action="store_true",
        help="check proposal archival state and print restoration guidance",
    )
    parser.add_argument(
        "--proposal-ids",
        type=int,
        nargs="*",
        default=[0, 1],
        help="proposal IDs to check for archival (default: [0, 1])",
    )
    parser.add_argument(
        "--proposal-count",
        type=int,
        default=None,
        help="contract total proposal count for archival boundary comparison",
    )
    args = parser.parse_args(argv)

    if args.check_archival:
        archival_info = detect_archival_state(
            args.proposal_ids, proposal_count=args.proposal_count
        )
        guidance = restoration_guidance(archival_info["archived"])
        if args.json:
            out = {
                "ok": archival_info["status"] == ERR_OK,
                "archival": archival_info,
                "guidance": guidance,
                "exit_code": (
                    EXIT_OK if archival_info["status"] == ERR_OK else EXIT_CHECK_FAILED
                ),
            }
            print(json.dumps(out, indent=2))
            return out["exit_code"]

        print("Governance State Archival & Footprint Restoration Report")
        print("=" * 66)
        for pid, state in sorted(archival_info["states"].items()):
            print(f"  Proposal ID {pid}: {state}")
        if guidance["steps"]:
            print("\nRestoration Guidance:")
            for s in guidance["steps"]:
                print(f"  {s}")
        return (
            EXIT_OK if archival_info["status"] == ERR_OK else EXIT_CHECK_FAILED
        )

    if not GOV_LIB.exists():
        message = f"governance-gas-model: not found: {GOV_LIB}"
        if args.json:
            print(json.dumps({"ok": False, "report": {}, "problems": [message],
                              "exit_code": EXIT_COULD_NOT_RUN}, indent=2))
        else:
            print(message, file=sys.stderr)
        return EXIT_COULD_NOT_RUN

    try:
        report, problems = run_checks()
    except ParseError as exc:
        message = f"governance-gas-model: could not parse: {exc}"
        if args.json:
            print(json.dumps({"ok": False, "report": {}, "problems": [message],
                              "exit_code": EXIT_COULD_NOT_RUN}, indent=2))
        else:
            print(message, file=sys.stderr)
        return EXIT_COULD_NOT_RUN

    exit_code = EXIT_CHECK_FAILED if problems else EXIT_OK

    if args.json:
        print(json.dumps({"ok": not problems, "report": report,
                          "problems": problems, "exit_code": exit_code},
                         indent=2))
        return exit_code

    print("Governance operation counts (static; NOT instruction counts)")
    print("=" * 66)
    print()
    header = f"{'entrypoint':<12}{'reads':>7}{'writes':>8}{'ops':>6}  detail"
    print(header)
    print("-" * len(header))
    for entry, data in report.items():
        detail = ", ".join(f"{k}={v}" for k, v in sorted(data["counts"].items()))
        print(f"{entry:<12}{data['reads']:>7}{data['writes']:>8}"
              f"{data['total_ops']:>6}  {detail}")

    print()
    print("Event bytes for a voting round (analytical; Protocol 27 limit "
          f"= {MAX_EVENT_BYTES_PER_TX})")
    print("-" * 66)
    for signers in (PROPOSAL_SIGS, MAX_SIGNERS):
        r = voting_round_event_bytes(signers)
        print(f"  {signers:>2} signers: {r['vote_cast_each']} B per vote, "
              f"{r['votes_total']} B of votes + {r['proposal_queued']} B "
              f"queue = {r['total']} B "
              f"({r['total'] / MAX_EVENT_BYTES_PER_TX:.1%} of limit)")

    if problems:
        print()
        print(f"{len(problems)} structural problem(s):")
        for p in problems:
            print(f"  - {p}")
        return EXIT_CHECK_FAILED

    print()
    print("All structural checks passed.")
    print()
    print("These are operation counts derived from source, not measurements.")
    print("Instruction counts still require a host run; see the module docstring.")
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
