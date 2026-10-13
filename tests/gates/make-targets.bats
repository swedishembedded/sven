#!/usr/bin/env bats
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements solutions for discoverable, self-documenting
# build interfaces for its clients. If your team needs expertise in build
# system design and repository quality gates then you can procure our services
# by sending an email to info@swedishembedded.com.
#
# `make check/gates/make-targets` (scripts/gates/check-make-targets.sh), pointed
# at scratch Makefiles with --makefile: the lint accepts a well-formed tree and
# flags each kind of violation. The failing paths matter as much as the passing
# one, because the lint's inner loop is hand-rolled shell.

setup() {
  LINT="$(cd "$(dirname "$BATS_TEST_FILENAME")/../.." && pwd)/scripts/gates/check-make-targets.sh"
  MK="$BATS_TEST_TMPDIR/Makefile"
  cat >"$MK" <<'MAKE'
##@ Build
## build - default area
## build/debug - debug build
## build/crate - lists the package targets
## build/crate/<package> - one package
## fmt - format
## all - everything

.PHONY: build build/debug build/crate build/crate/a build/crate/b fmt all
build:
build/debug:
build/crate:
build/crate/a build/crate/b:
fmt:
all:
MAKE
}

lint() { run bash "$LINT" --makefile "$MK"; }

# Replace the Makefile with $1 (a Makefile body).
write() { printf '%s\n' "$1" >"$MK"; }

@test "a well-formed scratch Makefile passes" {
  lint
  [ "$status" -eq 0 ]
  [[ "$output" == *"7 public targets"* ]]
}

@test "a bad name is flagged" {
  write '##@ S
## Bad - x
.PHONY: Bad
Bad:'
  lint
  [ "$status" -eq 1 ]
  [[ "$output" == *"Bad: not lowercase"* ]]
}

@test "an orphan prefix is flagged" {
  write '##@ S
## a/b - x
.PHONY: a/b
a/b:'
  lint
  [ "$status" -eq 1 ]
  [[ "$output" == *"a/b: prefix 'a' is not a defined target"* ]]
}

@test "a bare leaf other than all, clean, fmt and help is flagged" {
  write '##@ S
## deploy - x
## clean - x
.PHONY: deploy clean
deploy:
clean:'
  lint
  [ "$status" -eq 1 ]
  [[ "$output" == *"deploy: single-segment leaf"* ]]
  [[ "$output" != *"clean: single-segment"* ]]
}

@test "an undocumented target is flagged" {
  write '##@ S
## a - x
.PHONY: a a/b
a:
a/b:'
  lint
  [ "$status" -eq 1 ]
  [[ "$output" == *"a/b: no '## a/b - description'"* ]]
}

@test "a doc line outside any section is flagged" {
  write '## a - x
.PHONY: a a/b
a:
a/b:
##@ S
## a/b - y'
  lint
  [ "$status" -eq 1 ]
  [[ "$output" == *"a: doc line is not under a '##@ Section' line"* ]]
}

@test "a doc line for a missing target and an unmatched family line are flagged" {
  write '##@ S
## a - x
## ghost - x
## a/<n> - family with no members
.PHONY: a
a:'
  lint
  [ "$status" -eq 1 ]
  [[ "$output" == *"ghost: documented in Makefile but no such target"* ]]
  [[ "$output" == *"a/<n>: family doc line matches no target"* ]]
}

@test "the repository's own Makefile passes" {
  run bash "$LINT"
  [ "$status" -eq 0 ]
}
