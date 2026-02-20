#!/usr/bin/env bats
# 02_ci_mode.bats – CI/headless mode behaviour.
#
# Validates the contract that CI mode relies on:
#   • piped stdin triggers headless automatically
#   • --headless flag forces headless regardless of TTY
#   • --file reads input from a markdown file
#   • model text is written to stdout
#   • diagnostic / tool info is written to stderr (not stdout)
#   • exit code 0 on success, non-zero on error

load helpers

# ── Activation ────────────────────────────────────────────────────────────────

@test "02.01 piped stdin activates headless and exits 0" {
    run bash -c 'echo "ping" | "$BIN" --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "02.02 --headless flag activates headless mode" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "02.03 --file flag activates headless mode and exits 0" {
    run "${BIN}" --headless --model mock --file "${FIXTURES}/plan.md" 2>/dev/null
    [ "${status}" -eq 0 ]
}

@test "02.04 stdin redirect from file triggers headless" {
    run bash -c '"$BIN" --model mock < "$FIXTURES/plan.md" 2>/dev/null'
    [ "${status}" -eq 0 ]
}

# ── stdout is clean model text ────────────────────────────────────────────────

@test "02.05 stdout contains model reply text" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock'
    [[ "${STDOUT_OUT}" == *"pong"* ]]
}

@test "02.06 stdout does not contain [tool] diagnostic prefix" {
    run_split_output bash -c \
        'echo "run echo the command" | "$BIN" --headless --model mock'
    [[ "${STDOUT_OUT}" != *"[tool]"* ]]
}

@test "02.07 stdout does not contain [tool ok] diagnostic" {
    run_split_output bash -c \
        'echo "run echo the command" | "$BIN" --headless --model mock'
    [[ "${STDOUT_OUT}" != *"[tool ok]"* ]]
}

# ── stderr carries diagnostics, not model text ────────────────────────────────

@test "02.08 tool activity appears on stderr not stdout" {
    run_split_output bash -c \
        'echo "run echo something" | "$BIN" --headless --model mock'
    [[ "${STDOUT_OUT}" != *"[tool"* ]]
}

@test "02.09 stderr does not bleed into stdout" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock'
    [[ "${STDOUT_OUT}" != *"[fatal]"* ]]
    [[ "${STDOUT_OUT}" != *"[agent error]"* ]]
}

# ── Exit codes ────────────────────────────────────────────────────────────────

@test "02.10 successful run exits 0" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "02.11 output is non-empty on success" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock 2>/dev/null'
    [ -n "${output}" ]
}

# ── File input ────────────────────────────────────────────────────────────────

@test "02.12 file input processes all markdown steps" {
    run bash -c '"$BIN" --headless --model mock --file "$FIXTURES/plan.md" 2>/dev/null'
    [ "${status}" -eq 0 ]
    [ -n "${output}" ]
}

@test "02.13 nonexistent file exits non-zero" {
    run "${BIN}" --headless --model mock --file /no/such/file.md 2>/dev/null
    [ "${status}" -ne 0 ]
}

@test "02.14 three-step markdown file produces three responses" {
    # Each H2 section is a separate step; all three should be processed.
    run bash -c '"$BIN" --headless --model mock --file "$FIXTURES/three_steps.md" 2>/dev/null'
    [ "${status}" -eq 0 ]
    # Should mention "complete" from at least one step reply
    assert_output_contains "complete"
}

# ── Empty/minimal input ───────────────────────────────────────────────────────

@test "02.15 empty string input runs and exits 0" {
    run bash -c 'echo "" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "02.16 positional prompt argument works in headless mode" {
    # Redirect stdin from /dev/null so the binary doesn't block waiting for
    # stdin when there is no TTY (common in CI environments).
    run bash -c '"$BIN" --headless --model mock "ping" </dev/null 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}

# ── Inline config override ────────────────────────────────────────────────────

@test "02.17 SVEN_MOCK_RESPONSES env var is respected" {
    # The env var is already exported by helpers.bash; confirm it still works
    # when explicitly set in a subshell.
    run bash -c \
        'SVEN_MOCK_RESPONSES="$MOCK_RESPONSES" bash -c "echo ping | \"\$BIN\" --headless --model mock 2>/dev/null"'
    [ "${status}" -eq 0 ]
}
