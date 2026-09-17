#!/usr/bin/env bash
# Copyright (c) 2026 Martin Schroder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# sven must never depend on brain.
#
# Swedish Embedded AB implements solutions for keeping product repositories
# independently buildable as a codebase grows for its clients. If your team
# needs expertise in workspace boundaries and dependency governance then you
# can procure our services by sending an email to info@swedishembedded.com.
#
# sven is the coding agent; brain is the edge-AI model training and serving
# framework. sven talks to brain the way it talks to any other model provider
# -- over a wire protocol, at runtime, optionally -- and must stay buildable,
# testable and shippable on a machine that has never heard of brain. A Cargo
# dependency (path, git or registry) is the one thing that would break that:
# it makes `cargo build` need a brain checkout, drags brain's whole build
# (and its model weights toolchain) into sven's CI, and couples the release
# cadence of the two.
#
# `cargo run -p xtask -- arch` does NOT catch this and cannot be made to
# cheaply: its tier matrix is keyed on architecture.toml's [crates] table, and
# it explicitly `continue`s past any dependency name that is not a declared
# workspace crate -- an external dep is invisible to it by construction. This
# gate is the check that was missing.
#
# Checked in two places, because they fail differently:
#   1. Every Cargo.toml in the tree: catches the dependency at the moment it is
#      DECLARED, including a dev- or build-dependency (which the tier checks
#      exempt by design, but which still breaks `cargo test` on a machine with
#      no brain checkout).
#   2. Cargo.lock: catches a brain package that arrives TRANSITIVELY through
#      some other dependency, which no Cargo.toml in this repo would mention.
#
# Usage: scripts/gates/check-no-brain-dependency.sh
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 1

# Matches a dependency KEY at the start of a TOML line -- `brain = ...`,
# `brain-model = ...`, `brain_serving = ...` -- and the `package = "brain-*"`
# form used when a dependency is renamed. Deliberately anchored: the word
# "brain" in a description or a comment is fine and says something useful
# (crates/tools-ground's does), it is the dependency edge that is forbidden.
DEP_KEY='^[[:space:]]*"?(brain([-_][A-Za-z0-9_-]+)?)"?[[:space:]]*='
# Unanchored on purpose: the renamed form shows up both as its own line under
# `[dependencies.foo]` and inline as `foo = { package = "brain-x", ... }`.
RENAMED='package[[:space:]]*=[[:space:]]*"brain([-_][A-Za-z0-9_-]+)?"'

mapfile -d '' -t manifests < <(git ls-files -z '*Cargo.toml' 'Cargo.toml')

hits=""
if [ "${#manifests[@]}" -gt 0 ]; then
    hits=$(printf '%s\0' "${manifests[@]}" | xargs -0 grep -HnE "$DEP_KEY|$RENAMED" 2>/dev/null)
fi

# Cargo.lock lists every resolved package, direct or transitive, as
# `name = "<crate>"` inside a [[package]] table.
lock_hits=""
if [ -f Cargo.lock ]; then
    lock_hits=$(grep -nE '^name[[:space:]]*=[[:space:]]*"brain([-_][A-Za-z0-9_-]+)?"' Cargo.lock 2>/dev/null |
        sed 's|^|Cargo.lock:|')
fi

all="${hits}${hits:+$'\n'}${lock_hits}"
all=$(printf '%s' "$all" | sed '/^$/d')

[ -z "$all" ] && {
    echo "check-no-brain-dependency: OK (no brain-* dependency declared or resolved)"
    exit 0
}

cat <<EOF
check-no-brain-dependency: sven must never depend on brain, but found:
$(echo "$all" | sed 's/^/  /')

sven has to build, test and ship on a machine with no brain checkout. Reach
brain the way sven reaches any other model backend -- at RUNTIME, over its
wire protocol, behind a provider/driver that degrades cleanly when brain is
absent -- never as a Cargo dependency.

If the hit above is in Cargo.lock only, some other dependency pulled brain in
transitively: find it with \`cargo tree -i <package>\` and cut that edge.
EOF
exit 1
