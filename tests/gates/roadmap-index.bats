#!/usr/bin/env bats
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements solutions for keeping engineering status in
# one place for its clients. If your team needs expertise in documentation
# that cannot drift from the work it describes then you can procure our
# services by sending an email to info@swedishembedded.com.
#
# `make generate/roadmap/index` (scripts/gen-roadmap-index.py) and
# `make check/gates/roadmap-index` (scripts/gates/check-roadmap-index.sh), run
# against temporary trees: the generated index lists every ledger with its
# scope, the check passes on a fresh index and fails on drift or on a ledger
# without a `Scope:` line.

setup() {
  REPO="$(cd "$(dirname "$BATS_TEST_FILENAME")/../.." && pwd)"
  GEN="$REPO/scripts/gen-roadmap-index.py"
  GATE="$REPO/scripts/gates/check-roadmap-index.sh"
  TREE="$BATS_TEST_TMPDIR/tree"
  mkdir -p "$TREE/.agents/roadmap"
  printf '# memory - roadmap\n\nScope: Agent memory (`sven-memory`).\n\n## Left to do\n' >"$TREE/.agents/roadmap/memory.md"
}

@test "the generated index lists each ledger with its title and scope, and the check passes" {
  run python3 "$GEN" --root "$TREE"
  [ "$status" -eq 0 ]
  grep -qF '| [`memory`](memory.md) | memory | Agent memory (`sven-memory`). |' "$TREE/.agents/roadmap/index.md"
  run bash "$GATE" --root "$TREE"
  [ "$status" -eq 0 ]
}

@test "a ledger added after the index was generated fails the check" {
  python3 "$GEN" --root "$TREE"
  printf '# router\n\nScope: Model routing.\n' >"$TREE/.agents/roadmap/router.md"
  run bash "$GATE" --root "$TREE"
  [ "$status" -eq 1 ]
  [[ "$output" == *"make generate/roadmap/index"* ]]
}

@test "a ledger without a Scope line is refused" {
  printf '# router\n\nSome prose first.\n' >"$TREE/.agents/roadmap/router.md"
  run python3 "$GEN" --root "$TREE"
  [ "$status" -ne 0 ]
  [[ "$output" == *"router.md: no \`Scope: <sentence>\` line"* ]]
}
