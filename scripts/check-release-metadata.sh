#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
EXPECTED_TAG=${1:-}

python3 - "$ROOT" "$EXPECTED_TAG" <<'PY'
import json
import pathlib
import re
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
expected_tag = sys.argv[2]

workspace = tomllib.loads((root / "Cargo.toml").read_text())
version = workspace["workspace"]["package"]["version"]
release_tag = f"v{version}"

if expected_tag and expected_tag != release_tag:
    raise SystemExit(
        f"release tag {expected_tag!r} does not match workspace version {release_tag!r}"
    )

for package in ("memzoi-cli", "memzoi-mcp"):
    manifest = tomllib.loads((root / "crates" / package / "Cargo.toml").read_text())
    dependency = manifest["dependencies"]["memzoi-core"]["version"]
    if dependency != version:
        raise SystemExit(
            f"{package} depends on memzoi-core {dependency}, expected {version}"
        )

lock = tomllib.loads((root / "Cargo.lock").read_text())
workspace_packages = {
    package["name"]: package["version"]
    for package in lock["package"]
    if package["name"] in {"memzoi-core", "memzoi-cli", "memzoi-mcp"}
}
for package in ("memzoi-core", "memzoi-cli", "memzoi-mcp"):
    if workspace_packages.get(package) != version:
        raise SystemExit(
            f"Cargo.lock has {package} {workspace_packages.get(package)!r}, expected {version}"
        )

versions = json.loads((root / "website" / "versions.json").read_text())
if not versions or versions[0] != version:
    raise SystemExit(
        f"website/versions.json must list {version!r} first; got {versions!r}"
    )

config = (root / "website" / "docusaurus.config.js").read_text()
match = re.search(r"lastVersion:\s*'([^']+)'", config)
if not match or match.group(1) != version:
    raise SystemExit(
        f"Docusaurus lastVersion must be {version!r}; got {match.group(1) if match else None!r}"
    )

print(f"release metadata OK: {release_tag}")
PY
