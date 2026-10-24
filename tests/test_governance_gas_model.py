"""tests/test_governance_gas_model.py

Tests for script/governance-gas-model.py, the static cost model behind issue
#54.

The model does not measure CPU instructions - that needs a host run. What it
does is count the storage/ledger/event operations each governance entrypoint
performs, and assert the structural properties that keep per-vote cost
independent of the signer count.

The negative cases matter most here. A structural check that cannot fail is
the same failure mode this script exists to avoid (see
script/validate-doc-alignment.py, whose every check returns True), so each
check is exercised against a deliberately regressed copy of the source and
must report the problem.
"""

from __future__ import annotations

import importlib.util
import json
import re
import subprocess
import sys
from pathlib import Path

import pytest

_ROOT = Path(__file__).resolve().parent.parent
_SCRIPT = _ROOT / "script" / "governance-gas-model.py"


def _load_module():
    spec = importlib.util.spec_from_file_location("governance_gas_model", _SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


ggm = _load_module()


# ---------------------------------------------------------------------------
# Parsing
# ---------------------------------------------------------------------------


class TestStripComments:
    def test_line_comment_removed(self):
        assert ggm.strip_comments("let x = 1; // note").strip() == "let x = 1;"

    def test_block_comment_removed(self):
        src = "let a = 1;\n/* hides\n   { ( stuff\n*/\nlet b = 2;"
        out = ggm.strip_comments(src)
        assert "hides" not in out
        assert "let b = 2;" in out

    def test_double_slash_in_string_preserved(self):
        assert 'let u = "http://x";' in ggm.strip_comments('let u = "http://x";')

    def test_block_comment_markers_in_string_preserved(self):
        src = 'let s = "/* not a comment */";'
        assert "/* not a comment */" in ggm.strip_comments(src)

    def test_line_comment_marker_in_string_preserved(self):
        src = 'let s = "// not a comment";'
        assert "// not a comment" in ggm.strip_comments(src)

    def test_comment_only_source_becomes_empty(self):
        assert ggm.strip_comments("// nothing\n/* here */\n").strip() == ""

    def test_unterminated_block_comment_does_not_hang(self):
        # A malformed source must terminate rather than loop forever.
        out = ggm.strip_comments("let a = 1; /* never closed")
        assert "let a = 1;" in out


class TestExtractFunction:
    SOURCE = """
fn helper(env: &Env) -> u32 {
    env.ledger().timestamp()
}

fn caller(env: Env) -> Result<(), Error> {
    helper(&env);
    Ok(())
}
"""

    def test_method_with_return_type(self):
        assert "helper(&env)" in ggm.extract_function(self.SOURCE, "caller")

    def test_free_function(self):
        assert "timestamp" in ggm.extract_function(self.SOURCE, "helper")

    def test_unknown_function_raises(self):
        with pytest.raises(ggm.ParseError):
            ggm.extract_function(self.SOURCE, "does_not_exist")

    def test_unterminated_body_raises(self):
        broken = "fn half(env: &Env) -> u32 {\n    env.ledger().timestamp()\n"
        with pytest.raises(ggm.ParseError):
            ggm.extract_function(broken, "half")

    def test_nested_braces_are_balanced(self):
        src = "fn f() {\n    if x { g(); }\n    h();\n}\nfn g() {}\n"
        body = ggm.extract_function(src, "f")
        assert "h();" in body
        assert "fn g" not in body

    def test_public_function_is_found(self):
        src = "pub fn propose(env: Env) -> u32 {\n    1\n}\n"
        assert "1" in ggm.extract_function(src, "propose")


# ---------------------------------------------------------------------------
# Operation counting
# ---------------------------------------------------------------------------


class TestCountOps:
    def test_counts_each_leaf_kind(self):
        body = """
        env.storage().instance().get::<DataKey, u32>(&DataKey::A);
        env.storage().persistent().set(&DataKey::B, &1u32);
        env.storage().persistent().extend_ttl(&DataKey::B, 1, 1);
        env.ledger().timestamp();
        env.events().publish((symbol_short!("x"),), 1u32);
        """
        counts = ggm.count_ops(body)
        assert counts["instance_read"] == 1
        assert counts["persistent_write"] == 1
        assert counts["persistent_bump"] == 1
        assert counts["ledger_read"] == 1
        assert counts["event_emit"] == 1

    def test_empty_body_counts_nothing(self):
        counts = ggm.count_ops("")
        assert all(v == 0 for v in counts.values())

    def test_multiple_occurrences_are_summed(self):
        body = (
            "env.storage().instance().get(&DataKey::A);\n"
            "env.storage().instance().get(&DataKey::B);\n"
            "env.storage().instance().get(&DataKey::C);\n"
        )
        assert ggm.count_ops(body)["instance_read"] == 3

    def test_instance_write_is_counted(self):
        body = "env.storage().instance().set(&DataKey::A, &1u32);"
        assert ggm.count_ops(body)["instance_write"] == 1

    def test_persistent_read_is_counted(self):
        body = "env.storage().persistent().get(&DataKey::A);"
        assert ggm.count_ops(body)["persistent_read"] == 1

    def test_temporary_storage_is_counted(self):
        body = (
            "env.storage().temporary().set(&DataKey::A, &1u32);\n"
            "env.storage().temporary().get(&DataKey::A);\n"
        )
        counts = ggm.count_ops(body)
        assert counts["temporary_write"] == 1
        assert counts["temporary_read"] == 1

    def test_ledger_write_is_counted(self):
        body = "env.ledger().set(LedgerInfo { timestamp: 1 });"
        assert ggm.count_ops(body)["ledger_write"] == 1

    def test_unknown_operations_are_ignored(self):
        counts = ggm.count_ops("let x = 1 + 1;")
        assert all(v == 0 for v in counts.values())


class TestCalledHelpers:
    def test_known_helper_is_detected(self):
        assert "get_signer_index" in ggm.called_helpers(
            "let i = get_signer_index(env);"
        )

    def test_unknown_helpers_are_ignored(self):
        assert ggm.called_helpers("let x = 1;") == set()

    def test_multiple_helpers_are_detected(self):
        found = ggm.called_helpers("a(); b(); c();")
        assert {"a", "b", "c"} <= found


class TestWalk:
    def test_visits_every_node(self):
        tree = {"a": 1, "b": {"c": 2, "d": [3, 4]}}
        seen = list(ggm.walk(tree))
        assert 1 in seen and 2 in seen and 3 in seen and 4 in seen

    def test_cycle_guard_terminates(self):
        tree: dict = {"a": 1}
        tree["self"] = tree
        seen = list(ggm.walk(tree))
        assert 1 in seen

    def test_scalar_yields_itself(self):
        assert list(ggm.walk(7)) == [7]


class TestEnclosingIf:
    def test_inside_if_block(self):
        assert ggm.enclosing_if("{ if x { f(); } }", "f();")

    def test_inside_if_let_block(self):
        assert ggm.enclosing_if("{ if let Some(v) = o { g(v); } }", "g(v);")

    def test_outside_if_block(self):
        assert not ggm.enclosing_if("{ f(); if x { g(); } }", "f();")

    def test_outside_any_block(self):
        assert not ggm.enclosing_if("f();", "f();")

    def test_nested_inside_if(self):
        assert ggm.enclosing_if("{ if a { match b { _ => { f(); } } } }", "f();")

    def test_inside_else_block(self):
        assert ggm.enclosing_if("{ if x { a(); } else { f(); } }", "f();")

    def test_absent_snippet_is_not_enclosed(self):
        assert not ggm.enclosing_if("{ if x { f(); } }", "not_present();")


# ---------------------------------------------------------------------------
# Structural checks, including the negative cases
# ---------------------------------------------------------------------------

GOOD = """
const MAX_SIGNERS: u32 = 20;
const MAX_CALLDATA_BYTES: u32 = 4_096;
pub const MAX_PAGE_SIZE: u32 = 100;

fn get_signer_index(env: &Env) -> Map<Address, bool> {
    env.storage().instance().get(&DataKey::SignerIndex)
}

fn is_registered_signer(env: &Env, addr: &Address) -> bool {
    let index = get_signer_index(env);
    index.contains_key(addr.clone())
}

fn approve(env: Env, approver: Address, proposal_id: u32) -> Result<(), E> {
    if !is_registered_signer(&env, &approver) { return Err(E::NotASigner); }
    let mut idx = get_approval_index(&env, proposal_id);
    if idx.contains_key(approver.clone()) { return Err(E::AlreadyApproved); }
    env.storage().persistent().set(&DataKey::P(id), &proposal);
    if approval_count == threshold {
        env.storage().persistent().set(&DataKey::QuorumReachedAt(id), &info);
        env.events().publish((symbol_short!("proposal_queued"), id), info);
    }
    Ok(())
}
"""


def _stripped(text: str) -> str:
    return ggm.strip_comments(text)


class TestCheckConstant:
    def test_found_constant_returns_none(self):
        assert ggm.check_constant("const MAX_SIGNERS: u32 = 20;", "MAX_SIGNERS", 20) is None

    def test_missing_constant_is_reported(self):
        problem = ggm.check_constant("const OTHER: u32 = 1;", "MAX_SIGNERS", 20)
        assert problem is not None
        assert "MAX_SIGNERS" in problem

    def test_mismatched_constant_is_reported(self):
        problem = ggm.check_constant("const MAX_SIGNERS: u32 = 5;", "MAX_SIGNERS", 20)
        assert problem is not None
        assert "20" in problem

    def test_underscored_literal_is_parsed(self):
        src = "const MAX_CALLDATA_BYTES: u32 = 4_096;"
        assert ggm.check_constant(src, "MAX_CALLDATA_BYTES", 4096) is None


class TestVoteCostIsSignerIndependent:
    def test_good_source_passes(self):
        assert ggm.check_vote_cost_is_signer_independent(_stripped(GOOD)) == []

    def test_linear_signer_scan_is_caught(self):
        bad = GOOD.replace(
            "    let index = get_signer_index(env);\n"
            "    index.contains_key(addr.clone())",
            "    let signers: Vec<Address> = env.storage().instance()"
            ".get(&DataKey::Signers);\n"
            "    for s in signers.iter() { if s == *addr { return true; } }\n"
            "    false",
        )
        assert bad != GOOD, "fixture replacement did not apply"
        problems = ggm.check_vote_cost_is_signer_independent(_stripped(bad))
        assert problems, "a linear scan over Signers must be reported"

    def test_dropping_the_membership_check_is_caught(self):
        bad = GOOD.replace("if !is_registered_signer(&env, &approver)", "if false")
        problems = ggm.check_vote_cost_is_signer_independent(_stripped(bad))
        assert any("is_registered_signer" in p for p in problems)


class TestDuplicateApprovalIsO1:
    def test_good_source_passes(self):
        assert ggm.check_duplicate_approval_is_o1(_stripped(GOOD)) == []

    def test_approvals_vec_scan_is_caught(self):
        bad = GOOD.replace(
            "if idx.contains_key(approver.clone())",
            "if proposal.approvals.iter().any(|a| a == approver)",
        )
        assert bad != GOOD, "fixture replacement did not apply"
        problems = ggm.check_duplicate_approval_is_o1(_stripped(bad))
        assert any("approvals Vec" in p for p in problems)

    def test_dropping_the_index_is_caught(self):
        bad = GOOD.replace("get_approval_index(&env, proposal_id)", "Map::new(&env)")
        problems = ggm.check_duplicate_approval_is_o1(_stripped(bad))
        assert any("approval index" in p for p in problems)


class TestQuorumBranchIsConditional:
    def test_good_source_passes(self):
        assert ggm.check_quorum_branch_is_conditional(_stripped(GOOD)) == []

    def test_unconditional_quorum_write_is_caught(self):
        bad = GOOD.replace(
            "    if approval_count == threshold {\n"
            "        env.storage().persistent().set"
            "(&DataKey::QuorumReachedAt(id), &info);\n"
            "        env.events().publish"
            "((symbol_short!(\"proposal_queued\"), id), info);\n"
            "    }\n",
            "    env.storage().persistent().set"
            "(&DataKey::QuorumReachedAt(id), &info);\n"
            "    env.events().publish"
            "((symbol_short!(\"proposal_queued\"), id), info);\n",
        )
        assert bad != GOOD, "fixture replacement did not apply"
        problems = ggm.check_quorum_branch_is_conditional(_stripped(bad))
        assert any("no longer inside a conditional" in p for p in problems)

    def test_missing_quorum_anchor_is_caught(self):
        bad = GOOD.replace("QuorumReachedAt", "SomethingElse")
        assert ggm.check_quorum_branch_is_conditional(_stripped(bad))


class TestAnalyse:
    def test_analyse_returns_counts_and_totals(self):
        source = _stripped(GOOD)
        result = ggm.analyse(source, "approve")
        assert result["counts"]["persistent_write"] >= 1
        assert result["total_ops"] == result["reads"] + result["writes"]

    def test_analyse_of_unknown_function_raises(self):
        with pytest.raises(ggm.ParseError):
            ggm.analyse(_stripped(GOOD), "no_such_fn")


class TestRunChecks:
    def test_returns_report_and_no_problems(self):
        report, problems = ggm.run_checks()
        assert problems == []
        assert set(report) >= {"propose", "approve", "execute"}

    def test_report_entries_are_well_formed(self):
        report, _ = ggm.run_checks()
        for data in report.values():
            assert data["total_ops"] == data["reads"] + data["writes"]
            assert data["total_ops"] > 0


# ---------------------------------------------------------------------------
# The real contract
# ---------------------------------------------------------------------------


class TestRealGovernanceContract:
    def test_all_structural_checks_pass(self):
        source = _stripped(ggm.GOV_LIB.read_text(encoding="utf-8"))
        problems = []
        problems.extend(ggm.check_vote_cost_is_signer_independent(source))
        problems.extend(ggm.check_duplicate_approval_is_o1(source))
        problems.extend(ggm.check_quorum_branch_is_conditional(source))
        assert problems == [], f"unexpected problems: {problems}"

    def test_constants_match_the_model(self):
        source = _stripped(ggm.GOV_LIB.read_text(encoding="utf-8"))
        for name, expected in (
            ("MAX_SIGNERS", ggm.MAX_SIGNERS),
            ("MAX_CALLDATA_BYTES", ggm.MAX_CALLDATA_BYTES),
            ("MAX_PAGE_SIZE", ggm.MAX_PAGE_SIZE),
        ):
            assert ggm.check_constant(source, name, expected) is None

    def test_wrong_constant_is_reported(self):
        source = _stripped(ggm.GOV_LIB.read_text(encoding="utf-8"))
        assert ggm.check_constant(source, "MAX_SIGNERS", 999) is not None

    def test_every_entrypoint_is_analysed(self):
        report, problems = ggm.run_checks()
        assert problems == []
        for entry in ("propose", "approve", "execute"):
            assert report[entry]["total_ops"] > 0, f"{entry} counted no operations"

    def test_vote_emits_two_events_in_the_model(self):
        """The model reports 2 because it cannot evaluate the condition.

        `approve` always emits `vote_cast`, and additionally emits
        `proposal_queued` on the vote that reaches threshold. The structural
        check is what proves the second emit is conditional.
        """
        report, _ = ggm.run_checks()
        assert report["approve"]["counts"]["event_emit"] == 2

    def test_execute_is_the_most_read_heavy_path(self):
        report, _ = ggm.run_checks()
        assert report["execute"]["reads"] >= report["approve"]["reads"]


class TestEventByteSizing:
    def test_vote_cast_size_is_pinned(self):
        assert ggm.VOTE_CAST_BYTES == 284

    def test_proposal_queued_size_is_pinned(self):
        assert ggm.PROPOSAL_QUEUED_BYTES == 232

    def test_sizes_are_four_byte_aligned(self):
        assert ggm.VOTE_CAST_BYTES % 4 == 0
        assert ggm.PROPOSAL_QUEUED_BYTES % 4 == 0

    def test_ten_signer_round_fits_the_event_budget(self):
        r = ggm.voting_round_event_bytes(10)
        assert r["total"] == 3072
        assert r["total"] < ggm.MAX_EVENT_BYTES_PER_TX

    def test_max_signer_round_fits_the_event_budget(self):
        """Even at MAX_SIGNERS=20 the round stays well inside the limit."""
        r = ggm.voting_round_event_bytes(ggm.MAX_SIGNERS)
        assert r["total"] < ggm.MAX_EVENT_BYTES_PER_TX

    def test_event_budget_check_passes(self):
        assert ggm.check_event_budget() == []

    def test_budget_check_fails_when_events_grow(self, monkeypatch=None):
        """A much larger per-vote event must be reported, not silently pass."""
        original = ggm.VOTE_CAST_BYTES
        try:
            ggm.VOTE_CAST_BYTES = 2_000
            problems = ggm.check_event_budget()
        finally:
            ggm.VOTE_CAST_BYTES = original
        assert problems, "an oversized voting round must be reported"

    def test_independent_hand_check_agrees(self):
        """verify_governance_event_bytes.py recomputes both figures from the
        XDR definitions without importing the model."""
        result = subprocess.run(
            [
                sys.executable,
                str(_ROOT / "script" / "verify_governance_event_bytes.py"),
            ],
            capture_output=True,
            text=True,
        )
        assert result.returncode == 0, result.stderr
        assert "MATCHES governance-gas-model.py (284 / 232)" in result.stdout


class TestDocumentationAgreesWithTheModel:
    """docs/governance-gas.md must not drift from the numbers the model emits.

    A hand-edited table would rot silently, so the document is checked against
    the model on every run.
    """

    @staticmethod
    def _doc() -> str:
        return (_ROOT / "docs" / "governance-gas.md").read_text(encoding="utf-8")

    def test_operation_table_matches(self):
        doc = self._doc()
        report, problems = ggm.run_checks()
        assert problems == []
        for entry, data in report.items():
            row = (f"| `{entry}` | {data['reads']} | {data['writes']} "
                   f"| {data['total_ops']} |")
            assert row in doc, f"docs/governance-gas.md is stale: missing {row}"

    def test_round_arithmetic_matches(self):
        import re

        doc = re.sub(r"\s+", " ", self._doc())
        report, _ = ggm.run_checks()
        total_ops = (
            report["propose"]["total_ops"]
            + report["approve"]["total_ops"] * 10
            + report["execute"]["total_ops"]
        )
        assert f"{total_ops} ops" in doc
        r10 = ggm.voting_round_event_bytes(10)
        assert f"{r10['total']:,}" in doc

    def test_event_byte_figures_match(self):
        doc = self._doc()
        assert f"| `vote_cast` | {ggm.VOTE_CAST_BYTES} |" in doc
        assert f"| `proposal_queued` | {ggm.PROPOSAL_QUEUED_BYTES} |" in doc

    def test_doc_does_not_claim_instruction_counts(self):
        import re

        flat = re.sub(r"\s+", " ", self._doc())
        assert "does **not** contain them" in flat, (
            "the document must state that measured instruction counts are "
            "absent, so nobody reads the operation counts as instructions"
        )


class TestScriptRuns:
    def test_exits_zero_against_the_real_contract(self):
        result = subprocess.run(
            [sys.executable, str(_SCRIPT)], capture_output=True, text=True
        )
        assert result.returncode == 0, result.stdout + result.stderr
        assert "NOT instruction counts" in result.stdout

    def test_json_output_is_machine_readable(self):
        result = subprocess.run(
            [sys.executable, str(_SCRIPT), "--json"], capture_output=True, text=True
        )
        assert result.returncode == 0
        payload = json.loads(result.stdout)
        assert payload["ok"] is True
        assert "approve" in payload["report"]

    def test_json_schema_is_stable(self):
        result = subprocess.run(
            [sys.executable, str(_SCRIPT), "--json"], capture_output=True, text=True
        )
        payload = json.loads(result.stdout)
        assert set(payload) >= {"ok", "report", "problems"}
        assert payload["problems"] == []
        for entry, data in payload["report"].items():
            assert set(data) >= {"reads", "writes", "total_ops", "counts"}
            assert isinstance(data["counts"], dict)

    def test_missing_gov_lib_exits_two(self, tmp_path, monkeypatch):
        """A missing contract source is a configuration error (exit 2)."""
        import types

        stub = types.ModuleType("governance_gas_model_missing")
        stub.__file__ = str(_SCRIPT)
        source = _SCRIPT.read_text(encoding="utf-8")
        source = source.replace(
            'GOV_LIB = _ROOT / "contracts" / "governance" / "src" / "lib.rs"',
            f'GOV_LIB = Path({str(tmp_path / "missing_lib.rs")!r})',
        )
        assert "missing_lib.rs" in source, "GOV_LIB override did not apply"
        missing_script = tmp_path / "governance-gas-model.py"
        missing_script.write_text(source, encoding="utf-8")
        result = subprocess.run(
            [sys.executable, str(missing_script)], capture_output=True, text=True
        )
        assert result.returncode == 2, result.stdout + result.stderr


# ---------------------------------------------------------------------------
# Cross-module integration: factory / governance / SDK surface
# ---------------------------------------------------------------------------


class TestCrossModuleIntegration:
    """The model's figures must agree with the other governance artefacts.

    These tests exercise the interaction surface between the gas model, the
    governance contract, the factory that deploys it, and the SDK helpers that
    build the same events, without importing any of them directly.
    """

    def test_governance_contract_source_is_present(self):
        assert ggm.GOV_LIB.exists(), f"missing {ggm.GOV_LIB}"

    def test_factory_references_the_governance_contract(self):
        factory = _ROOT / "contracts" / "factory" / "src" / "lib.rs"
        if not factory.exists():
            pytest.skip("factory contract not present in this checkout")
        text = factory.read_text(encoding="utf-8")
        assert "governance" in text.lower()

    def test_event_bytes_agree_with_verifier(self):
        result = subprocess.run(
            [
                sys.executable,
                str(_ROOT / "script" / "verify_governance_event_bytes.py"),
            ],
            capture_output=True,
            text=True,
        )
        assert result.returncode == 0, result.stderr
        assert (
            f"MATCHES governance-gas-model.py "
            f"({ggm.VOTE_CAST_BYTES} / {ggm.PROPOSAL_QUEUED_BYTES})"
            in result.stdout
        )

    def test_operation_counts_agree_with_docs(self):
        doc = (_ROOT / "docs" / "governance-gas.md").read_text(encoding="utf-8")
        report, problems = ggm.run_checks()
        assert problems == []
        for entry, data in report.items():
            assert f"| `{entry}` | {data['reads']} | {data['writes']} " in doc

    def test_sdk_event_names_match_the_model(self):
        doc = (_ROOT / "docs" / "governance-gas.md").read_text(encoding="utf-8")
        assert "vote_cast" in doc
        assert "proposal_queued" in doc

    def test_doc_alignment_script_passes(self):
        script = _ROOT / "script" / "validate-doc-alignment.py"
        if not script.exists():
            pytest.skip("validate-doc-alignment.py not present")
        result = subprocess.run(
            [sys.executable, str(script)], capture_output=True, text=True
        )
        assert result.returncode == 0, result.stdout + result.stderr
        assert "warning" not in result.stdout.lower()


# ---------------------------------------------------------------------------
# Archival Detection & Footprint Restoration Guidance
# ---------------------------------------------------------------------------


class TestArchivalDetectionAndRestorationGuidance:
    """Verifies that archival detection identifies archived entries deterministically
    and provides actionable footprint restoration guidance.
    """

    def test_detects_live_and_archived_proposals(self):
        res = ggm.detect_archival_state(
            [0, 1, 2],
            liveness={0: True, 1: False},
            proposal_count=2,
        )
        assert res["status"] == ggm.ERR_ARCHIVED_ENTRY
        assert res["states"][0] == ggm.STATE_LIVE
        assert res["states"][1] == ggm.STATE_ARCHIVED
        assert res["states"][2] == ggm.STATE_MISSING
        assert res["archived"] == [1]

    def test_restoration_guidance_structure(self):
        guidance = ggm.restoration_guidance([1, 3])
        assert guidance["status"] == ggm.ERR_ARCHIVED_ENTRY
        assert guidance["proposal_ids"] == [1, 3]
        assert "Proposal(1)" in guidance["footprint_keys"]
        assert "Proposal(3)" in guidance["footprint_keys"]
        assert len(guidance["commands"]) == 2
        assert any("stellar contract restore" in c for c in guidance["commands"])
        assert len(guidance["steps"]) == 4

    def test_rejects_invalid_inputs_early(self):
        assert ggm.detect_archival_state("not a list")["status"] == ggm.ERR_INVALID_INPUT
        assert ggm.detect_archival_state([-1])["status"] == ggm.ERR_INVALID_INPUT
        assert ggm.restoration_guidance("bad")["status"] == ggm.ERR_INVALID_INPUT

    def test_cli_check_archival_flag(self):
        result = subprocess.run(
            [sys.executable, str(_SCRIPT), "--check-archival", "--proposal-ids", "0", "--proposal-count", "1", "--json"],
            capture_output=True,
            text=True,
        )
        assert result.returncode == 1  # 0 < 1 without liveness => archived => exit 1
        data = json.loads(result.stdout)
        assert data["ok"] is False
        assert data["archival"]["status"] == ggm.ERR_ARCHIVED_ENTRY


