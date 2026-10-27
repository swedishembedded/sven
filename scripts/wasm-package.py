#!/usr/bin/env python3
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
"""Writes the package.json of one sven-wasm build (`make build/wasm`).

The package names exactly what it was built from - the workspace version,
the git commit (and whether the tree had changes), the remote-control
protocol - so an application installing it can refuse a build it was not
written against instead of discovering the mismatch at run time.

    wasm-package.py OUT_DIR {web|nodejs}
"""

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def workspace_version() -> str:
    manifest = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]+)"', manifest, re.M | re.S)
    if not match:
        sys.exit("wasm-package: no [workspace.package] version in Cargo.toml")
    return match.group(1)


def protocol_version() -> int:
    source = (ROOT / "crates/control/src/remote.rs").read_text(encoding="utf-8")
    match = re.search(r"pub const REMOTE_PROTOCOL_VERSION: u32 = (\d+);", source)
    if not match:
        sys.exit("wasm-package: REMOTE_PROTOCOL_VERSION not found in sven-control")
    return int(match.group(1))


def git(*args: str) -> str:
    return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True, check=True).stdout.strip()


def main() -> None:
    if len(sys.argv) != 3 or sys.argv[2] not in ("web", "nodejs"):
        sys.exit("usage: wasm-package.py OUT_DIR {web|nodejs}")
    out, flavour = Path(sys.argv[1]), sys.argv[2]
    entry = "sven_wasm.js"
    package = {
        "name": "sven-wasm",
        "version": workspace_version(),
        "description": "A sven agent session for JavaScript (built from sven-wasm).",
        "license": "Apache-2.0",
        "files": sorted(p.name for p in out.iterdir()),
        "types": "sven_wasm.d.ts",
        ("module" if flavour == "web" else "main"): entry,
        "sven": {
            "commit": git("rev-parse", "HEAD"),
            "dirty": bool(git("status", "--porcelain", "--untracked-files=no")),
            "protocol": protocol_version(),
            "target": flavour,
        },
    }
    if flavour == "web":
        package["type"] = "module"
    (out / "package.json").write_text(json.dumps(package, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
