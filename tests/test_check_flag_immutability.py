"""
tests/test_check_flag_immutability.py

Test suite for script/check-flag-immutability.py — the static verification that
the packed capability flags cannot be mutated post-creation.

Builds a tiny synthetic stream crate tree (lib.rs/types.rs) in tmp_path and
asserts the script's pass/fail behaviour for each structural violation class.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

_SCRIPT = (
    Path(__file__).resolve().parent.parent / "script" / "check-flag-immutability.py"
)


def _load_module():
    spec = importlib.util.spec_from_file_location(
        "check_flag_immutability", _SCRIPT
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


cfi = _load_module()


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

GOOD_LIB = """\
#[contract]
pub struct FluxoraStream;

#[contractimpl]
impl FluxoraStream {
    pub fn create_stream(
        env: Env, cancellable: bool, pausable: bool, transferable: bool,
    ) -> Result<u64, Error> {
        let stream = Stream { flags: Stream::flags_from_parts(cancellable, pausable, transferable) };
        Ok(0)
    }

    pub fn pause(env: Env, stream_id: u64) -> Result<(), Error> {
        let mut stream = storage::load_stream(&env, stream_id)?;
        if !stream.pausable {
            return Err(Error::NotPausable);
        }
        Ok(())
    }
}
"""

GOOD_TYPES = """\
pub struct Stream {
    /// Fixed at creation, never mutable. Packed capability bits.
    pub flags: u8,
}
"""


@pytest.fixture()
def crate_tree(tmp_path: Path) -> Path:
    src = tmp_path / "src"
    src.mkdir()
    (src / "lib.rs").write_text(GOOD_LIB, encoding="utf-8")
    (src / "types.rs").write_text(GOOD_TYPES, encoding="utf-8")
    return tmp_path


@pytest.fixture()
def patch_src_root(monkeypatch: pytest.MonkeyPatch, crate_tree: Path) -> Path:
    monkeypatch.setattr(cfi, "STREAM_SRC", crate_tree / "src")
    return crate_tree / "src"


def _run() -> int:
    return cfi.main()


def _write_lib(_src: Path, content: str) -> None:
    (_src / "lib.rs").write_text(content, encoding="utf-8")


# ---------------------------------------------------------------------------
# Baseline
# ---------------------------------------------------------------------------

def test_clean_source_passes(patch_src_root: Path) -> None:
    assert _run() == 0


# ---------------------------------------------------------------------------
# Assignments anywhere are rejected
# ---------------------------------------------------------------------------

def test_assignment_in_a_mutating_fn_fails(patch_src_root: Path) -> None:
    bad = GOOD_LIB.replace(
        "            return Err(Error::NotPausable);\n        }\n        Ok(())",
        "            return Err(Error::NotPausable);\n        }\n        stream.flags = 0;\n        Ok(())",
    )
    _write_lib(patch_src_root, bad)
    assert _run() == 1


def test_assignment_in_a_view_fn_fails(patch_src_root: Path) -> None:
    bad = GOOD_LIB.replace(
        "        Ok(0)",
        "        stream.flags = 1;\n        Ok(0)",
    )
    _write_lib(patch_src_root, bad)
    assert _run() == 1


def test_assignment_without_spaces_fails(patch_src_root: Path) -> None:
    bad = GOOD_LIB.replace(
        "        Ok(0)",
        "        stream.flags=false;\n        Ok(0)",
    )
    _write_lib(patch_src_root, bad)
    assert _run() == 1


def test_only_assignments_fail_not_comparisons(patch_src_root: Path) -> None:
    # A comparison (==) must not trip the assignment detector.
    bad = GOOD_LIB.replace(
        "        if !stream.pausable {",
        "        if stream.flags == 1 || !stream.pausable {",
    )
    _write_lib(patch_src_root, bad)
    assert _run() == 0


# ---------------------------------------------------------------------------
# Struct-literal initializers must live in create_stream only
# ---------------------------------------------------------------------------

def test_initialization_outside_create_stream_fails(patch_src_root: Path) -> None:
    bad = GOOD_LIB.replace(
        "    pub fn pause(env: Env, stream_id: u64) -> Result<(), Error> {\n        let mut stream = storage::load_stream(&env, stream_id)?;",
        "    pub fn pause(env: Env, stream_id: u64) -> Result<(), Error> {\n        let rebuilt = Stream { flags: 0 };\n        let mut stream = storage::load_stream(&env, stream_id)?;",
    )
    _write_lib(patch_src_root, bad)
    assert _run() == 1


def test_missing_initializer_fails(patch_src_root: Path) -> None:
    bad = GOOD_LIB.replace(
        "        let stream = Stream { flags: Stream::flags_from_parts(cancellable, pausable, transferable) };\n",
        "        let stream = Stream { };\n",
    )
    _write_lib(patch_src_root, bad)
    assert _run() == 1


# ---------------------------------------------------------------------------
# types.rs must keep immutable doc comments
# ---------------------------------------------------------------------------

def test_removing_immutable_doc_fails(patch_src_root: Path) -> None:
    types = patch_src_root / "types.rs"
    content = types.read_text(encoding="utf-8")
    types.write_text(content.replace("Fixed at creation, never mutable.", "mutable"), encoding="utf-8")
    assert _run() == 1


def test_removing_a_field_declaration_fails(patch_src_root: Path) -> None:
    types = patch_src_root / "types.rs"
    content = types.read_text(encoding="utf-8")
    types.write_text(content.replace("    pub flags: u8,\n", ""), encoding="utf-8")
    assert _run() == 1


# ---------------------------------------------------------------------------
# Module helpers
# ---------------------------------------------------------------------------

def test_production_sources_skips_test_dir(patch_src_root: Path) -> None:
    (patch_src_root / "test").mkdir()
    (patch_src_root / "test" / "x.rs").write_text("stream.cancellable = true;", encoding="utf-8")
    assert _run() == 0


def test_missing_source_dir_skips(tmp_path: Path) -> None:
    cfi.STREAM_SRC = tmp_path / "nope"
    assert _run() == 0