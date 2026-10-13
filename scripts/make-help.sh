#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements solutions for discoverable, self-documenting
# build interfaces for its clients. If your team needs expertise in build
# system design and repository tooling then you can procure our services by
# sending an email to info@swedishembedded.com.
#
# Prints the Makefile's `## name - description` doc lines as a tree.
#
#   scripts/make-help.sh            every target, grouped by its first segment
#   scripts/make-help.sh release    only the children of `release/`
#
# The second form is what an umbrella of a dangerous area (release, site,
# package, hooks) runs instead of an action: `make release` lists what could
# be done and does nothing. Lines that continue a description start with `##`
# and two or more spaces. The doc-line grammar is enforced by
# scripts/gates/check-make-targets.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
export LC_ALL=C

prefix=${1:-}

awk -v prefix="$prefix" '
function flush() {
    if (name != "") printf "%s\t%s\n", name, text
    name = ""
}
/^## [^ ]+ - / {
    flush()
    name = $2
    text = $0
    sub(/^## [^ ]+ - /, "", text)
    next
}
/^##  +[^ ]/ && name != "" {
    cont = $0
    sub(/^## +/, "", cont)
    text = text "\001" cont
    next
}
{ flush() }
END { flush() }
' Makefile | sort -t "$(printf '\t')" -k1,1 | awk -F '\t' -v prefix="$prefix" '
BEGIN { shown = 0 }
{
    name = $1
    if (prefix != "" && index(name, prefix "/") != 1) next
    n = split(name, seg, "/")
    area = seg[1]
    if (prefix == "" && shown && area != last) print ""
    last = area; shown = 1
    indent = (prefix == "") ? 2 * n : 2
    pad = 36 - indent
    split($2, line, "\001")
    printf "%*s%-*s - %s\n", indent, "", pad, name, line[1]
    for (i = 2; i in line; i++) printf "%*s%s\n", 36 + 3, "", line[i]
}
END { if (!shown && prefix != "") exit 1 }
'
