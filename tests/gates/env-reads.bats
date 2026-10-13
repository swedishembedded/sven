#!/usr/bin/env bats
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements solutions for explicit, testable program
# configuration for its clients. If your team needs expertise in command-line
# interface design and removing hidden process-environment inputs then you can
# procure our services by sending an email to info@swedishembedded.com.
#
# `make check/gates/env-reads` (scripts/gates/check-env-reads.sh), run against
# temporary trees: a new environment read is refused, a stale allowlist entry
# is refused, a tree that matches its allowlist passes.

setup() {
  GATE="$(cd "$(dirname "$BATS_TEST_FILENAME")/../.." && pwd)/scripts/gates/check-env-reads.sh"
  TREE="$BATS_TEST_TMPDIR/tree"
  mkdir -p "$TREE/crates/a/src"
  printf 'fn main() { let _ = std::env::var("SVEN_ONE"); }\n' >"$TREE/crates/a/src/lib.rs"
  printf 'crates/a/src/lib.rs\tSVEN_ONE\n' >"$TREE/allow"
}

run_gate() { run bash "$GATE" --root "$TREE" --allowlist "$TREE/allow"; }

@test "a tree that matches its allowlist passes" {
  run_gate
  [ "$status" -eq 0 ]
}

@test "a new environment read is refused and the message says to add a flag" {
  printf 'fn f() { let _ = std::env::var("SVEN_TWO"); }\n' >"$TREE/crates/a/src/new.rs"
  run_gate
  [ "$status" -eq 1 ]
  [[ "$output" == *"new env reads are forbidden: add a CLI flag"* ]]
  [[ "$output" == *"crates/a/src/new.rs"*"SVEN_TWO"* ]]
}

@test "a second variable read in an already-listed file is refused" {
  printf 'fn g() { let _ = std::env::var_os("SVEN_TWO"); }\n' >>"$TREE/crates/a/src/lib.rs"
  run_gate
  [ "$status" -eq 1 ]
  [[ "$output" == *"SVEN_TWO"* ]]
}

@test "a clap env attribute is refused" {
  printf '#[arg(long, env = "CLAP_VAR")] x: u8;\n' >"$TREE/crates/a/src/cli.rs"
  run_gate
  [ "$status" -eq 1 ]
  [[ "$output" == *"CLAP_VAR"* ]]
}

@test "a split call, a computed name, a dotenv load and env! of a custom name are all found" {
  printf 'fn f() { std::env::var(\n  "SPLIT_VAR"); }\n' >"$TREE/crates/a/src/split.rs"
  printf 'fn f(k: &str) { let _ = std::env::var(k); }\n' >"$TREE/crates/a/src/dyn.rs"
  printf 'fn f() { let _ = dotenvy::dotenv(); }\n' >"$TREE/crates/a/src/dot.rs"
  printf 'const V: &str = env!("MY_BUILD_VAR");\n' >"$TREE/crates/a/src/build.rs"
  run_gate
  [ "$status" -eq 1 ]
  [[ "$output" == *"SPLIT_VAR"* && "$output" == *"<k>"* && "$output" == *"dotenvy"* && "$output" == *"MY_BUILD_VAR"* ]]
}

@test "reads under src/, xtask/ and samples/ count too" {
  mkdir -p "$TREE/src" "$TREE/xtask/src" "$TREE/samples/s/src"
  for d in src xtask/src samples/s/src; do
    printf 'fn f() { let _ = std::env::var("X_%s"); }\n' "${d//\//_}" >"$TREE/$d/m.rs"
  done
  run_gate
  [ "$status" -eq 1 ]
  [[ "$output" == *"X_src"* && "$output" == *"X_xtask_src"* && "$output" == *"X_samples_s_src"* ]]
}

@test "compile-time Cargo metadata, writes and comments are not environment reads" {
  {
    printf 'const V: &str = env!("CARGO_PKG_VERSION");\n'
    printf 'const O: &str = env!("OUT_DIR");\n'
    printf '// std::env::var("IN_COMMENT")\n'
    printf 'fn f() { std::env::set_var("W", "1"); std::process::Command::new("x").env("E", "1"); }\n'
  } >"$TREE/crates/a/src/v.rs"
  run_gate
  [ "$status" -eq 0 ]
}

@test "an allowlist entry whose read is gone is refused as stale" {
  printf 'crates/a/src/lib.rs\tSVEN_ONE\ncrates/a/src/gone.rs\tSVEN_GONE\n' >"$TREE/allow"
  run_gate
  [ "$status" -eq 1 ]
  [[ "$output" == *"stale"* && "$output" == *"SVEN_GONE"* ]]
}

@test "the committed allowlist matches the committed tree" {
  run bash "$GATE"
  [ "$status" -eq 0 ]
}
