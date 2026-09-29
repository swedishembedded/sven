#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Enforces the rules in samples/README.md:
#
#   1. The package name is derived from the path: samples/<a>/<b> is package
#      `sample-<a>-<b>`. The Makefile's samples/%/{build,run} rules resolve the
#      package that way, so a mismatch is a target that silently builds nothing.
#   2. A sample reaches this workspace only through `sven-sdk`. A sample that
#      reaches past the facade stops being evidence that the facade is
#      sufficient, which is the only reason samples/ exists.
#   3. No sample depends on brain. Samples are workspace members, so a brain
#      dependency would enter sven's own build and lockfile, which
#      check-no-brain-dependency.sh exists to prevent. An application that
#      links both sven and brain lives outside this repository.
#
# Manifests are parsed as TOML rather than sliced with a regex. The previous
# version scanned from `^\[dependencies\]` to `^\[[^d]`, which is wrong in two
# directions at once: `[dependencies.sven-kernel]` does not terminate the
# slice, and a renamed dependency (`foo = { path = "../../crates/kernel" }`)
# is invisible to a scan that only matches on the key. Both would be missed.
#
# Usage: scripts/gates/check-samples.sh
set -euo pipefail

cd "$(dirname "$0")/../.."

python3 - "$@" <<'PY'
import pathlib
import sys
import tomllib

ROOT = pathlib.Path.cwd()
DEP_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")

failures = []
manifests = sorted(ROOT.glob("samples/*/*/Cargo.toml"))

for manifest in manifests:
    rel = manifest.parent.relative_to(ROOT / "samples")
    try:
        doc = tomllib.loads(manifest.read_text())
    except tomllib.TOMLDecodeError as exc:
        failures.append(f"{manifest}: not valid TOML: {exc}")
        continue

    # Rule 1 - the package name is the path.
    expected = "sample-" + "-".join(rel.parts)
    actual = doc.get("package", {}).get("name")
    if actual != expected:
        failures.append(f"{manifest}: package is {actual!r}, path implies {expected!r}")

    # Collect every declared dependency, across every dependency table and
    # every target-specific override, resolving renames to the real package.
    declared = {}

    def collect(table, where):
        for name, spec in (table or {}).items():
            real = spec.get("package", name) if isinstance(spec, dict) else name
            declared.setdefault(real, where)

    for key in DEP_TABLES:
        collect(doc.get(key), key)
    for target, table in (doc.get("target") or {}).items():
        for key in DEP_TABLES:
            collect(table.get(key), f"target.{target}.{key}")

    # Rule 2 - this workspace is reachable only through the facade.
    past_facade = sorted(d for d in declared if d.startswith("sven-") and d != "sven-sdk")
    if past_facade:
        failures.append(f"{manifest}: reaches past the facade: {' '.join(past_facade)}")

    # Rule 3 - no sample depends on brain.
    brain = sorted(d for d in declared if d == "brain" or d.startswith(("brain-", "brain_")))
    if brain:
        failures.append(
            f"{manifest}: depends on brain ({' '.join(brain)}) - "
            "that would put brain into sven's own workspace, which "
            "check-no-brain-dependency.sh exists to prevent"
        )

if failures:
    for f in failures:
        print(f"check-samples: {f}")
    print("check-samples: FAILED - see samples/README.md")
    sys.exit(1)

print(f"check-samples: OK ({len(manifests)} sample(s))")
PY
