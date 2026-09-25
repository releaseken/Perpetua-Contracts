#!/usr/bin/env python3
"""Audit Rust enum discriminants and documented error-code tables."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent


class Entry:
    def __init__(self, code: int, name: str, line_no: int):
        self.code = code
        self.name = name
        self.line_no = line_no

    def __repr__(self) -> str:
        return f"Entry(code={self.code!r}, name={self.name!r}, line_no={self.line_no!r})"

    def __eq__(self, other: object) -> bool:
        return (
            isinstance(other, Entry)
            and self.code == other.code
            and self.name == other.name
            and self.line_no == other.line_no
        )


_ENUM_RE = re.compile(
    r"\b(?:pub\s+)?enum\s+(DataKey|Error|[A-Za-z_][A-Za-z0-9]*Error)\s*\{"
)
_VARIANT_RE = re.compile(
    r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*(?:\([^)]*\)|\{[^}]*\})?\s*"
    r"(?:=\s*(\d+))?\s*,?\s*(?://.*)?$"
)


def _section_for_heading(stripped: str) -> str | None:
    if not stripped.startswith("## "):
        return None
    if "FactoryError" in stripped:
        return "FactoryError (factory)"
    if "GovernanceError" in stripped:
        return "GovernanceError (governance)"
    if "Error Code Reference" in stripped:
        return "ContractError (stream)"
    return None


def _parse_docs(path: Path) -> dict[str, list[Entry]]:
    """Parse the repository's error-code tables into section entries."""
    sections: dict[str, list[Entry]] = {}
    current: str | None = None
    table_active = False

    for line_no, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        stripped = line.strip()
        heading = _section_for_heading(stripped)
        if stripped.startswith("## "):
            current = heading
            table_active = False
            if current is not None:
                sections.setdefault(current, [])
            continue
        if current is None:
            continue
        if stripped.startswith("|"):
            if set(stripped.replace("|", "").replace("-", "").replace(":", "").strip()) == set():
                table_active = True
                continue
            cells = [cell.strip().strip("`") for cell in stripped.strip("|").split("|")]
            if len(cells) < 2:
                continue
            numbers = [(index, cell) for index, cell in enumerate(cells) if cell.isdigit()]
            if not numbers:
                continue
            code_index, code_text = numbers[0]
            name_candidates = [
                cell for index, cell in enumerate(cells)
                if index != code_index and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", cell)
            ]
            if not name_candidates:
                continue
            sections[current].append(Entry(int(code_text), name_candidates[0], line_no))
            table_active = True
            continue
        if stripped and table_active:
            table_active = False
            current = None

    sections = {name: entries for name, entries in sections.items() if entries}
    if not sections:
        raise ValueError(f"No discriminant tables found in {path}")
    return sections


def _parse_rust_enum(path: Path, enum_match: re.Match[str]) -> tuple[str, list[Entry]]:
    lines = path.read_text(encoding="utf-8").splitlines()
    start = next(index for index, line in enumerate(lines) if enum_match.group(0) in line)
    enum_name = enum_match.group(1)
    entries: list[Entry] = []
    next_code = 0
    depth = 0

    for index in range(start, len(lines)):
        line = lines[index]
        depth += line.count("{") - line.count("}")
        if index == start:
            continue
        if depth <= 0:
            break
        match = _VARIANT_RE.match(line)
        if not match or line.lstrip().startswith("//"):
            continue
        name, explicit_code = match.groups()
        code = int(explicit_code) if explicit_code is not None else next_code
        entries.append(Entry(code, name, index + 1))
        next_code = code + 1
    return enum_name, entries


def parse_rust_sources(root: Path = REPO_ROOT / "contracts") -> dict[str, list[Entry]]:
    """Parse every DataKey and *Error enum in contract Rust sources."""
    sections: dict[str, list[Entry]] = {}
    for path in sorted(root.glob("*/src/**/*.rs")):
        content = path.read_text(encoding="utf-8")
        for enum_match in _ENUM_RE.finditer(content):
            enum_name, entries = _parse_rust_enum(path, enum_match)
            if not entries:
                continue
            crate = path.relative_to(root).parts[0]
            sections[f"{crate}::{enum_name}"] = entries
    return sections


def _find_intra_collisions(section: str, entries: list[Entry]) -> list[str]:
    by_code: dict[int, list[Entry]] = {}
    for entry in entries:
        by_code.setdefault(entry.code, []).append(entry)
    messages = []
    for code, same_code in by_code.items():
        names = {entry.name for entry in same_code}
        if len(names) > 1:
            details = ", ".join(f"{entry.name} (line {entry.line_no})" for entry in same_code)
            messages.append(f"INTRA-COLLISION in {section}: code {code}: {details}")
    return messages


def _find_cross_collisions(sections: dict[str, list[Entry]]) -> list[str]:
    by_code: dict[int, list[str]] = {}
    for section, entries in sections.items():
        for code in {entry.code for entry in entries}:
            by_code.setdefault(code, []).append(section)
    return [
        f"CROSS-SECTION overlap: code {code}: {', '.join(section_names)}"
        for code, section_names in sorted(by_code.items())
        if len(section_names) > 1
    ]


def _find_ordering_issues(section: str, entries: list[Entry]) -> list[str]:
    messages = []
    previous: Entry | None = None
    for entry in entries:
        if previous is not None and entry.code < previous.code:
            messages.append(
                f"OUT-OF-ORDER in {section}: code {entry.code} at line {entry.line_no}"
            )
        elif previous is not None and entry.code == previous.code:
            # Duplicate codes are reported by the intra-collision check.
            pass
        previous = entry
    return messages


def _audit(sections: dict[str, list[Entry]]) -> int:
    intra = []
    ordering = []
    for section, entries in sections.items():
        print(f"{section}: {len(entries)} variants")
        intra.extend(_find_intra_collisions(section, entries))
        ordering.extend(_find_ordering_issues(section, entries))

    for message in ordering:
        print(f"WARNING: {message}")
    for message in _find_cross_collisions(sections):
        print(f"WARNING: {message}")
        print(f"SHARED-DECODER FINDING: {message}")
    if not _find_cross_collisions(sections):
        print("No cross-section numeric overlaps detected")

    for message in intra:
        print(message)
    if intra:
        print("ACTION REQUIRED: resolve discriminant collisions before merging.")
        return 1
    print("No intra-section collisions found")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--docs", type=Path, help="audit a documentation table instead of Rust sources")
    parser.add_argument("--source-root", type=Path, default=REPO_ROOT / "contracts")
    args = parser.parse_args(argv)

    try:
        sections = _parse_docs(args.docs) if args.docs else parse_rust_sources(args.source_root)
    except FileNotFoundError as error:
        print(f"ERROR: File not found: {error.filename}", file=sys.stderr)
        return 2
    except ValueError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2
    return _audit(sections)


if __name__ == "__main__":
    sys.exit(main())
