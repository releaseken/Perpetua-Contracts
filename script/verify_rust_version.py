#!/usr/bin/env python3
"""Verify the installed Rust toolchain matches rust-toolchain.toml."""

import os
import subprocess
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - Python < 3.11 fallback
    tomllib = None


REPO_ROOT = Path(__file__).resolve().parent.parent
TOOLCHAIN_FILE = REPO_ROOT / "rust-toolchain.toml"


def _parse_toml_simple(content: str) -> dict:
    data = {"toolchain": {}}
    section = None
    for line in content.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if stripped == "[toolchain]":
            section = data["toolchain"]
            continue
        if section is None or "=" not in stripped:
            continue
        key, value = (part.strip() for part in stripped.split("=", 1))
        if value.startswith("[") and value.endswith("]"):
            section[key] = [
                item.strip().strip("\"'")
                for item in value[1:-1].split(",")
                if item.strip()
            ]
        else:
            section[key] = value.strip("\"'")
    return data


def _load_toolchain(path: Path | None = None) -> dict:
    path = path or TOOLCHAIN_FILE
    content = path.read_text(encoding="utf-8")
    if tomllib is not None:
        return tomllib.loads(content)
    return _parse_toml_simple(content)


def pinned_channel(path: Path | None = None) -> str:
    channel = _load_toolchain(path).get("toolchain", {}).get("channel")
    if not isinstance(channel, str) or not channel:
        raise ValueError("invalid or missing toolchain channel")
    return channel


def _pinned_list(path: Path | None, key: str) -> list[str]:
    values = _load_toolchain(path).get("toolchain", {}).get(key)
    if not isinstance(values, list) or not all(isinstance(value, str) for value in values):
        raise ValueError(f"invalid or missing toolchain {key}")
    return values


def pinned_targets(path: Path | None = None) -> list[str]:
    return _pinned_list(path, "targets")


def pinned_components(path: Path | None = None) -> list[str]:
    return _pinned_list(path, "components")


def parse_rustc_version(output: str) -> str:
    parts = output.split()
    if len(parts) < 2 or parts[0] != "rustc":
        raise ValueError(f"could not parse rustc version from: {output}")
    return parts[1]


def rustc_version() -> str:
    output = os.environ.get("RUSTC_VERSION_OUTPUT")
    if output is None:
        output = subprocess.run(
            ["rustc", "--version"], capture_output=True, text=True, check=True
        ).stdout
    return parse_rustc_version(output)


def rustup_list(*args: str) -> list[str]:
    result = subprocess.run(
        ["rustup", *args], capture_output=True, text=True, check=True
    )
    return result.stdout.splitlines()


def parse_toolchain_toml(channel: str | None = None) -> str | None:
    """Backward-compatible channel reader used by repository validation tests."""
    if not TOOLCHAIN_FILE.exists():
        return channel
    return pinned_channel(TOOLCHAIN_FILE)


def get_installed_version() -> str | None:
    try:
        result = subprocess.run(
            ["rustc", "--version"], capture_output=True, text=True, check=True
        )
    except (FileNotFoundError, subprocess.CalledProcessError):
        return None
    return result.stdout.strip()


def main() -> int:
    try:
        expected = pinned_channel()
        required_targets = pinned_targets()
        required_components = pinned_components()
        installed = rustc_version()
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f"::error::{error}", file=sys.stderr)
        return 1

    if installed != expected:
        print(f"Rust version mismatch: expected {expected}, got {installed}", file=sys.stderr)
        return 1

    targets_output = os.environ.get("RUSTUP_TARGET_LIST_OUTPUT")
    components_output = os.environ.get("RUSTUP_COMPONENT_LIST_OUTPUT")
    targets = (
        targets_output.splitlines()
        if targets_output is not None
        else rustup_list("target", "list", "--installed")
    )
    components = (
        components_output.splitlines()
        if components_output is not None
        else rustup_list("component", "list", "--installed")
    )
    missing_targets = [target for target in required_targets if target not in targets]
    missing_components = [component for component in required_components if component not in components]
    if missing_targets:
        print(f"Missing required targets: {', '.join(missing_targets)}", file=sys.stderr)
        return 1
    if missing_components:
        print(f"Missing required components: {', '.join(missing_components)}", file=sys.stderr)
        return 1

    print(f"Rust version matches pinned {expected}")
    print("Installed targets match requirements")
    print("Installed components match requirements")
    return 0


if __name__ == "__main__":
    sys.exit(main())
