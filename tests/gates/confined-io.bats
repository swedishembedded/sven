#!/usr/bin/env bats
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements solutions for confining autonomous agents to
# the state and networks its clients give them. If your team needs expertise
# in isolating agent processes then you can procure our services by sending an
# email to info@swedishembedded.com.
#
# `make check/gates/confined-io` against temporary trees: a direct `dirs::`
# call or a hand-built reqwest client is refused outside the crate that owns
# it, and allowed there and in tests.

setup() {
  GATE="$(cd "$(dirname "$BATS_TEST_FILENAME")/../.." && pwd)/scripts/gates/check-confined-io.sh"
  TREE="$BATS_TEST_TMPDIR/tree"
  mkdir -p "$TREE/crates/roots/src" "$TREE/crates/http/src" "$TREE/crates/a/src" "$TREE/crates/a/tests"
  printf 'fn f() { let _ = dirs::config_dir(); }\n' >"$TREE/crates/roots/src/lib.rs"
  printf 'fn f() { let _ = reqwest::Client::builder(); }\n' >"$TREE/crates/http/src/lib.rs"
  printf 'fn f() { let _ = dirs::home_dir(); let _ = reqwest::get("x"); }\n' >"$TREE/crates/a/tests/t.rs"
}

@test "the owning crates and test code may use the raw APIs" {
  run bash "$GATE" --root "$TREE"
  [ "$status" -eq 0 ]
}

@test "a direct dirs call elsewhere is refused" {
  printf 'fn f() { let _ = dirs::data_dir(); }\n' >"$TREE/crates/a/src/lib.rs"
  run bash "$GATE" --root "$TREE"
  [ "$status" -eq 1 ]
  [[ "$output" == *"crates/a/src/lib.rs"* ]]
}

@test "a hand-built reqwest client elsewhere is refused" {
  printf 'fn f() { let _ = reqwest::Client::new(); }\n' >"$TREE/crates/a/src/lib.rs"
  run bash "$GATE" --root "$TREE"
  [ "$status" -eq 1 ]
  [[ "$output" == *"sven_http::client_builder"* ]]
}
