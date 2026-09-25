#!/usr/bin/env python3
"""Verify the packed capability flags are immutable.

Issue #104. README.md's *Immutable guarantees* promises the three flags are
fixed at creation and can never change. This is a static analysis over the
production source (contracts/stream/src, excluding test/) and fails if:

1. Any assignment to the packed flags field exists anywhere, e.g. `stream.flags = ...`
   (the one shape that would let a setter exist).
2. The packed flags field is not initialized exactly once inside `create_stream`.
3. The `types.rs` declarations are not documented as immutable.

The same structural rules are compiled into the crate's test suite
(test/immutability.rs via include_str!); this script is the CI side of the
check, independent of the Rust toolchain.

Exit status: 0 = clean, 1 = violation.
"""

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
STREAM_SRC = REPO_ROOT / "contracts" / "stream" / "src"

FLAGS = ("cancellable", "pausable", "transferable")

# `=` not followed by `=`, so `stream.flags == x` is not mistaken for an
# assignment.
ASSIGNMENT = re.compile(r"\.\s*flags\s*=(?!=)")
FLAGS_INIT = re.compile(r"^\s*flags\s*:")
FN_DECL = re.compile(r"^\s*(?:pub\s+)?fn\s+(\w+)\(")


def production_sources() -> list[Path]:
    if not STREAM_SRC.exists():
        return []
    return [
        p
        for p in sorted(STREAM_SRC.glob("*.rs"))
        if p.name != "test" and not p.is_dir()
    ]


def iter_functions(path: Path):
    """Yield (line_no, function_name) per source line of a production file."""
    current = None
    for no, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        m = FN_DECL.match(line)
        if m:
            current = m.group(1)
        yield no, current, line


def check_file(path: Path, inits: dict[str, list[tuple[str, int]]]):
    problems: list[str] = []
    for no, func, line in iter_functions(path):
        if ASSIGNMENT.search(line):
            problems.append(
                f"{path.name}:{no}: assignment to a capability flag inside "
                f"`{func}` — flags are immutable after create_stream: {line.strip()!r}"
            )
            continue
        m = FLAGS_INIT.match(line)
        if m:
            if func != "create_stream":
                problems.append(
                    f"{path.name}:{no}: packed capability flags initialized "
                    f"inside `{func}`, expected create_stream"
                )
            else:
                inits["flags"].append((path.name, no))
    return problems


def check_types(types_path: Path) -> list[str]:
    """The packed field must be `pub flags: u8` with an immutable doc."""
    problems: list[str] = []
    lines = types_path.read_text(encoding="utf-8").splitlines()
    decl = "pub flags: u8"
    hits = [i for i, l in enumerate(lines) if decl in l]
    if len(hits) != 1:
        problems.append("types.rs: packed flags should be declared exactly once")
        return problems
    line_no = hits[0]
    documentation = "\n".join(lines[max(0, line_no - 6) : line_no])
    if "never mutable" not in documentation:
        problems.append(
            f"types.rs:{line_no + 1}: packed flags are not documented as immutable "
            f"(line above should say 'Fixed at creation, never mutable')"
        )
    return problems


def count_guard_reads() -> int:
    """Count the documented `!stream.<flag>` guards in lib.rs (informational)."""
    lib = STREAM_SRC / "lib.rs"
    if not lib.exists():
        return 0
    return sum(
        1
        for _, _, line in iter_functions(lib)
        if any(f"!stream.{flag}" in line for flag in FLAGS)
    )


def main() -> int:
    if not STREAM_SRC.exists():
        print(f"SKIP: {STREAM_SRC} not found")
        return 0

    problems: list[str] = []
    inits: dict[str, list[tuple[str, int]]] = {"flags": []}
    for path in production_sources():
        problems.extend(check_file(path, inits))

    for field, where in inits.items():
        if len(where) != 1:
            problems.append(
                f"{field}: expected exactly one create_stream initializer, "
                f"found {len(where)} in {where or 'no file'}"
            )

    types_path = STREAM_SRC / "types.rs"
    if types_path.exists():
        problems.extend(check_types(types_path))

    if problems:
        print("FAIL: capability flags are not immutable.")
        for p in problems:
            print(f"  - {p}")
        return 1

    print(
        "OK: cancellable/pausable/transferable are initialized only in "
        "create_stream and never assigned afterwards "
        f"({count_guard_reads()} capability-guard reads)."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())