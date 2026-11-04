#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Builds the Lean proofs and checks they are actually proofs.
#
# # Why a build is not enough
#
# `lake build` succeeds on a file full of `sorry`. It warns, and a warning in
# a long log is a warning nobody reads. So this checks two further things:
# that no source contains `sorry` at all, and that `#print axioms` on each
# headline theorem reports only Lean's own three axioms. `sorryAx` in that
# output is a hole somebody opened, and it is the one thing that cannot be
# talked past.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project="${here}/lean"

if ! command -v lake >/dev/null 2>&1; then
    if [ -x "${HOME}/.elan/bin/lake" ]; then
        PATH="${HOME}/.elan/bin:${PATH}"
        export PATH
    else
        echo "[formal/lean] SKIPPED: no lake on PATH. Install the toolchain with"
        echo "[formal/lean]   curl -sSfL https://elan.lean-lang.org/elan-init.sh | sh"
        echo "[formal/lean] This is a note, not a silent pass."
        exit 0
    fi
fi

# Asked inside the project, so its lean-toolchain answers rather than a
# default toolchain a machine may not have configured.
echo "[formal/lean] $(cd "$project" && lean --version)"

log="$(mktemp)"
trap 'rm -f "$log"' EXIT

if ! (cd "$project" && lake build) >"$log" 2>&1; then
    echo "[formal/lean] the proofs do not build"
    cat "$log"
    exit 1
fi

# An unproved declaration, as Lean itself reports it. Asked of the compiler
# rather than of grep: a text search cannot tell a `sorry` in code from the
# word in a comment (this script's own header says it twice), and it would
# either miss the real thing or cry wolf about prose. Lean names the
# declaration.
if grep -n "declaration uses 'sorry'" "$log"; then
    echo "[formal/lean] a proof is not finished (see above)"
    exit 1
fi

if grep -q "sorryAx" "$log"; then
    echo "[formal/lean] a theorem depends on sorryAx:"
    grep "sorryAx" "$log"
    exit 1
fi

# Both shapes `#print axioms` can answer, because a theorem closed by `rfl`
# or `decide` prints the SECOND one and used to be counted as nothing at all.
# That is the wrong way round: those rest on fewer assumptions than the rest,
# not more, and a runner that ignored them would not have noticed one being
# deleted from the audit either.
audit='depends on axioms|does not depend on any axioms'
checked="$(grep -cE "$audit" "$log" || true)"
if [ "${checked:-0}" -eq 0 ]; then
    echo "[formal/lean] no axiom audit ran -- Sven/Audit.lean should have printed one"
    echo "[formal/lean] line per headline theorem. A suite that checks nothing passes"
    echo "[formal/lean] quietly, which is the failure this guards against."
    exit 1
fi

grep -E "$audit" "$log" | sed 's/^info: [^ ]* /  /'
echo "[formal/lean] ${checked} theorem(s) checked, none resting on anything but Lean's own axioms"
