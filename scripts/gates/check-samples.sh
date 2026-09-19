#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Enforces the two rules in samples/README.md:
#
#   1. A sample depends on `sven-sdk` and nothing else from this workspace.
#      A sample that reaches past the facade stops being evidence that the
#      facade is sufficient, which is the only reason samples/ exists.
#   2. The package name is derived from the path: samples/<a>/<b> is package
#      `sample-<a>-<b>`. The Makefile's samples/%/{build,run} rules resolve the
#      package that way, so a mismatch is a target that silently builds nothing.
set -euo pipefail

cd "$(dirname "$0")/../.."

fail=0
found=0

while IFS= read -r manifest; do
    dir="$(dirname "$manifest")"
    rel="${dir#samples/}"
    found=$((found + 1))

    expected="sample-$(echo "$rel" | tr '/' '-')"
    actual="$(sed -n 's/^name = "\(.*\)"/\1/p' "$manifest" | head -1)"
    if [ "$actual" != "$expected" ]; then
        echo "check-samples: $manifest: package is '$actual', path implies '$expected'"
        fail=1
    fi

    # Any sven-* dependency other than sven-sdk, declared in this manifest.
    if offenders="$(sed -n '/^\[dependencies\]/,/^\[[^d]/p' "$manifest" \
            | grep -oE '^sven-[a-z0-9-]+' | grep -v '^sven-sdk$' || true)"; then
        if [ -n "$offenders" ]; then
            echo "check-samples: $manifest: reaches past the facade: $(echo "$offenders" | tr '\n' ' ')"
            fail=1
        fi
    fi
done < <(find samples -mindepth 3 -maxdepth 3 -name Cargo.toml 2>/dev/null | sort)

if [ "$fail" -ne 0 ]; then
    echo "check-samples: FAILED - see samples/README.md"
    exit 1
fi
echo "check-samples: OK ($found sample(s))"
