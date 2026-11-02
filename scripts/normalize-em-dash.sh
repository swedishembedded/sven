#!/usr/bin/env bash
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# Replace em dashes (U+2014) with plain hyphens. Runs as a pre-commit hook;
# receives staged file paths as arguments.
#
# The em dash is the one character the repository does not carry. Every
# other character is left as written: icons and symbols in the TUI and the
# docs (a check mark, a lock) are meant to be there, and so are names
# spelled with their own letters.
#
# Modifies files in place and exits 1 when any file was changed so that
# pre-commit reports "files were modified" and the user re-stages them.
# Do NOT call git-add from here - pre-commit runs hooks in parallel and
# concurrent git-add calls deadlock on the index lock.
set -euo pipefail

if [[ $# -eq 0 ]]; then
    exit 0
fi

changed=0
for f in "$@"; do
    [[ -f "$f" ]] || continue
    if grep -q $'\xe2\x80\x94' "$f"; then
        sed -i 's/\xe2\x80\x94/-/g' "$f"
        echo "normalize-em-dash: replaced em dashes in $f"
        changed=1
    fi
done

exit "$changed"
