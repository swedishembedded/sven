#!/usr/bin/env bats
# 04_pipeline.bats – pipeline and terminal-setting tests.
#
# Validates:
#   • sven stdout pipes cleanly to a second sven instance
#   • pipeline output is non-empty and contains model text
#   • set -e causes the pipeline to exit on non-zero exit
#   • piped-in content from another tool is processed correctly
#   • multi-hop chains work
#   • stdin from here-document, file redirect, and process substitution

load helpers

# ── Basic piping ──────────────────────────────────────────────────────────────

@test "04.01 output of sven pipes to sven – both succeed" {
    run bash -c \
        'echo "make a plan" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "summarize the above" 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "04.02 pipeline output is non-empty" {
    run bash -c \
        'echo "make a plan" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "summarize the above" 2>/dev/null'
    [ -n "${output}" ]
}

@test "04.03 pipeline output contains final summary text" {
    run bash -c \
        'echo "make a plan" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "summarize the above" 2>/dev/null'
    assert_output_contains "Summary"
}

@test "04.04 three-stage pipeline succeeds" {
    run bash -c \
        'echo "make a plan" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "implement the plan above" 2>/dev/null \
           | "$BIN" --headless --model mock "summarize the above" 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Summary"
}

# ── stdin sources ─────────────────────────────────────────────────────────────

@test "04.05 here-string input works" {
    run bash -c '"$BIN" --headless --model mock <<< "ping" 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}

@test "04.06 here-document input works" {
    run bash -c '"$BIN" --headless --model mock 2>/dev/null <<EOF
ping
EOF'
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}

@test "04.07 file redirect as stdin works" {
    run bash -c '"$BIN" --headless --model mock < "$FIXTURES/plan.md" 2>/dev/null'
    [ "${status}" -eq 0 ]
    [ -n "${output}" ]
}

@test "04.08 process substitution as input works" {
    run bash -c '"$BIN" --headless --model mock 2>/dev/null < <(echo "ping")'
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}

# ── Pipeline stdout isolation ─────────────────────────────────────────────────

@test "04.09 no [tool] noise leaks into pipeline stdout" {
    run_split_output bash -c \
        'echo "run echo test" \
           | "$BIN" --headless --model mock \
           | "$BIN" --headless --model mock "summarize the above"'
    [[ "${STDOUT_OUT}" != *"[tool"* ]]
}

@test "04.10 stdout of first stage is non-empty" {
    local first_output
    first_output="$(bash -c 'echo "make a plan" | "$BIN" --headless --model mock 2>/dev/null')"
    [ -n "${first_output}" ]
}

@test "04.11 stdout of first stage feeds cleanly to second stage" {
    run bash -c \
        'first_out=$(echo "make a plan" | "$BIN" --headless --model mock 2>/dev/null)
         echo "summarize the above: $first_out" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    [ -n "${output}" ]
}

# ── set -e pipeline abort ─────────────────────────────────────────────────────

@test "04.12 successful pipeline with set -e exits 0" {
    run bash -euc \
        'echo "ping" | "$BIN" --headless --model mock > /dev/null 2>&1'
    [ "${status}" -eq 0 ]
}

@test "04.13 two-stage pipeline with set -e exits 0 on success" {
    run bash -euc \
        'echo "ping" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "summarize" 2>/dev/null \
           > /dev/null'
    [ "${status}" -eq 0 ]
}

# ── echo | sven – classic unix idiom ─────────────────────────────────────────

@test "04.14 echo pipe with explicit prompt arg" {
    run bash -c \
        'echo "ping" | "$BIN" --headless --model mock "context: reply above" 2>/dev/null'
    [ "${status}" -eq 0 ]
    [ -n "${output}" ]
}

# ── Different shell invocation styles ─────────────────────────────────────────

@test "04.15 works when invoked via sh -c" {
    run sh -c \
        'echo "ping" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}

@test "04.16 works when invoked via bash -c" {
    run bash -c \
        'echo "ping" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}

# ── stdin is not a TTY (bats always provides non-TTY stdin) ───────────────────

@test "04.17 non-TTY stdin triggers headless mode automatically" {
    # In bats, run subcommands never have a TTY stdin, so --headless is implicit
    run bash -c 'echo "ping" | "$BIN" --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}

@test "04.18 stdin from subshell is treated as piped input" {
    run bash -c '(echo "ping") | "$BIN" --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}

# ── Positional prompt + stdin ─────────────────────────────────────────────────

@test "04.19 positional prompt combined with piped stdin" {
    run bash -c \
        'echo "make a plan" \
           | "$BIN" --headless --model mock "summarize the above" 2>/dev/null'
    [ "${status}" -eq 0 ]
    [ -n "${output}" ]
}

# ── Stdin from another tool ───────────────────────────────────────────────────

@test "04.20 sven accepts output from printf" {
    run bash -c 'printf "ping\n" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}

@test "04.21 sven accepts multiline output from printf" {
    run bash -c 'printf "line1\nping\n" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    [ -n "${output}" ]
}
