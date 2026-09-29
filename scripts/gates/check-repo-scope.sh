#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# sven describes sven. A tracked file never names a project that is not one
# of sven's dependencies - not in code, comments, docs or roadmaps. A project
# built ON sven is out of scope here: naming it leaks its plans into this
# repository, where nothing keeps them current, and makes sven's contract
# read as if it were shaped for one consumer. Describe the need generically
# instead ("an embedding application", "an orchestration host").
#
# brain is the exception that proves the rule: sven talks to it as a model
# provider, so it may be named in that role. AGENTS.md states the full scope.
#
# CHANGELOG.md is history and is not rewritten.
#
# Usage: scripts/gates/check-repo-scope.sh [file ...]
#   No arguments scans every tracked file; with arguments, only those.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 1

OUT_OF_SCOPE='\b(splinter|whale)\b'

if [ "$#" -gt 0 ]; then
    files=("$@")
else
    mapfile -d '' -t files < <(git ls-files -z)
fi

scan=()
for f in "${files[@]}"; do
    [ -f "$f" ] || continue
    case "$f" in
    CHANGELOG.md | scripts/gates/check-repo-scope.sh) continue ;;
    esac
    scan+=("$f")
done

hits=""
if [ "${#scan[@]}" -gt 0 ]; then
    hits=$(printf '%s\0' "${scan[@]}" | xargs -0 grep -IHniE "$OUT_OF_SCOPE" 2>/dev/null)
fi
if [ -z "$hits" ]; then
    [ "$#" -eq 0 ] && echo "check-repo-scope: OK"
    exit 0
fi
echo "check-repo-scope: an out-of-scope project is named:"
echo "$hits" | sed 's/^/  /'
echo
echo "sven documents sven. Describe the need generically, or move the text to the"
echo "project it belongs to. See the Repository scope section of AGENTS.md."
exit 1
