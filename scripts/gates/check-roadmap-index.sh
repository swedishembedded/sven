#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements solutions for keeping engineering status in
# one place for its clients. If your team needs expertise in documentation
# that cannot drift from the work it describes then you can procure our
# services by sending an email to info@swedishembedded.com.
#
# .agents/roadmap/index.md is generated from the ledgers beside it
# (scripts/gen-roadmap-index.py). This gate fails when the committed index has
# drifted from them, or when a ledger has no `Scope:` line under its title.
# `make generate/roadmap/index` rewrites the index.
#
# Usage: scripts/gates/check-roadmap-index.sh [--root DIR]
#   --root DIR  check DIR instead of this repository (used by the self-test,
#               tests/gates/roadmap-index.bats)
set -euo pipefail
here="$(cd "$(dirname "$0")/../.." && pwd)"

root="$here"
case "${1:-}" in
"") ;;
--root)
    [ "$#" -eq 2 ] || { echo "usage: $0 [--root DIR]" >&2; exit 2; }
    root="$2"
    ;;
*) echo "usage: $0 [--root DIR]" >&2; exit 2 ;;
esac

exec python3 "$here/scripts/gen-roadmap-index.py" --check --root "$root"
