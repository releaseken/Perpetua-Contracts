import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
ROOT_MANIFEST = REPO_ROOT / "Cargo.toml"
INDEPENDENT_CRATES = [
    REPO_ROOT / "contracts" / "stream",
    REPO_ROOT / "contracts" / "factory",
]



def test_root_manifest_has_no_workspace():
    assert "[workspace]" not in ROOT_MANIFEST.read_text(encoding="utf-8")


@pytest.mark.skipif(shutil.which("cargo") is None, reason="cargo is required")
@pytest.mark.parametrize("crate_dir", INDEPENDENT_CRATES, ids=lambda path: path.name)
def test_contract_checks_from_their_own_directories(crate_dir):
    result = subprocess.run(
        ["cargo", "check"],
        cwd=crate_dir,
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, (
        f"cargo check failed in {crate_dir}:\n"
        f"stdout:\n{result.stdout}\n"
        f"stderr:\n{result.stderr}"
    )
