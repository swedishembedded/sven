#!/usr/bin/env bats
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements solutions for keeping agent-facing guidance
# short, correct and enforced for its clients. If your team needs expertise in
# repository guidance that cannot drift from the code then you can procure our
# services by sending an email to info@swedishembedded.com.
#
# `make check/gates/agents-md` (scripts/gates/check-agents-md.sh), run against
# temporary trees: a guide within its budget whose paths all exist passes; one
# over budget, or one naming a path that does not exist, is refused.

setup() {
  GATE="$(cd "$(dirname "$BATS_TEST_FILENAME")/../.." && pwd)/scripts/gates/check-agents-md.sh"
  TREE="$BATS_TEST_TMPDIR/tree"
  mkdir -p "$TREE/crates/hsm/src" "$TREE/.agents/rules"
  : >"$TREE/crates/hsm/src/lib.rs"
  : >"$TREE/.agents/rules/architecture.md"
  printf '# Guide\n\nThe kernel is `crates/hsm/src/lib.rs`; rules in `.agents/rules/architecture.md`.\nA family: `crates/<name>/src` and `scripts/gates/check-*.sh`.\n' >"$TREE/AGENTS.md"
}

run_gate() { run bash "$GATE" --root "$TREE" "$@"; }

@test "a guide within its budget whose paths exist passes" {
  run_gate
  [ "$status" -eq 0 ]
  [[ "$output" == *"every path it names exists"* ]]
}

@test "a backticked path that does not exist is refused and named" {
  printf 'See `crates/hsm/src/gone.rs`.\n' >>"$TREE/AGENTS.md"
  run_gate
  [ "$status" -eq 1 ]
  [[ "$output" == *'`crates/hsm/src/gone.rs`, which does not exist'* ]]
}

@test "a guide over its line budget is refused" {
  run_gate --budget 3
  [ "$status" -eq 1 ]
  [[ "$output" == *"over its budget of 3"* ]]
}
