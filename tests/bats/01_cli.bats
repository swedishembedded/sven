#!/usr/bin/env bats
# 01_cli.bats – CLI flag and subcommand validation.
#
# Validates that the binary's interface is complete and well-formed:
# flags, subcommands, exit codes on bad input, and generated artefacts.

load helpers

# ── Help / version ────────────────────────────────────────────────────────────

@test "01.01 --help exits 0" {
    run "${BIN}" --help
    [ "${status}" -eq 0 ]
}

@test "01.02 --help mentions 'coding agent'" {
    run "${BIN}" --help
    assert_output_contains "coding agent"
}

@test "01.03 -h short flag exits 0" {
    run "${BIN}" -h
    [ "${status}" -eq 0 ]
}

@test "01.04 --version exits 0" {
    run "${BIN}" --version
    [ "${status}" -eq 0 ]
}

@test "01.05 --version output contains version number" {
    run "${BIN}" --version
    # Should contain digits (e.g. "sven 0.1.0")
    [[ "${output}" =~ [0-9]+\.[0-9]+ ]]
}

# ── Unknown flags ─────────────────────────────────────────────────────────────

@test "01.06 unknown flag exits non-zero" {
    run "${BIN}" --no-such-flag
    [ "${status}" -ne 0 ]
}

@test "01.07 unknown long flag exits non-zero" {
    run "${BIN}" --definitely-not-a-real-flag-xyz
    [ "${status}" -ne 0 ]
}

# ── show-config subcommand ────────────────────────────────────────────────────

@test "01.08 show-config exits 0" {
    run "${BIN}" show-config
    [ "${status}" -eq 0 ]
}

@test "01.09 show-config outputs valid TOML with [model] section" {
    run "${BIN}" show-config
    assert_output_contains "[model]"
}

@test "01.10 show-config outputs [agent] section" {
    run "${BIN}" show-config
    assert_output_contains "[agent]"
}

@test "01.11 show-config outputs [tools] section" {
    run "${BIN}" show-config
    assert_output_contains "[tools]"
}

@test "01.12 show-config outputs [tui] section" {
    run "${BIN}" show-config
    assert_output_contains "[tui]"
}

# ── completions subcommand ────────────────────────────────────────────────────

@test "01.13 completions bash exits 0" {
    run "${BIN}" completions bash
    [ "${status}" -eq 0 ]
}

@test "01.14 completions bash generates a shell function" {
    run "${BIN}" completions bash
    # Bash completions always contain the word 'complete'
    assert_output_contains "complete"
}

@test "01.15 completions zsh exits 0" {
    run "${BIN}" completions zsh
    [ "${status}" -eq 0 ]
}

@test "01.16 completions fish exits 0" {
    run "${BIN}" completions fish
    [ "${status}" -eq 0 ]
}

# ── --mode flag ───────────────────────────────────────────────────────────────

@test "01.17 --mode research accepted" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock --mode research 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "01.18 --mode plan accepted" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock --mode plan 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "01.19 --mode agent accepted" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock --mode agent 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "01.20 invalid --mode value rejected" {
    run "${BIN}" --mode turbo --headless --model mock <<< "hi"
    [ "${status}" -ne 0 ]
}

# ── --model flag ──────────────────────────────────────────────────────────────

@test "01.21 --model mock accepted" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "01.22 --model mock/name form accepted" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock/test-model 2>/dev/null'
    [ "${status}" -eq 0 ]
}

# ── --verbose flag ────────────────────────────────────────────────────────────

@test "01.23 -v verbose flag accepted" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock -v 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "01.24 -vv trace flag accepted" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock -vv 2>/dev/null'
    [ "${status}" -eq 0 ]
}
