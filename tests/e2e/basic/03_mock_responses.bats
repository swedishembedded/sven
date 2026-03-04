#!/usr/bin/env bats
# 03_mock_responses.bats – validate that the YAML mock model returns
# the correct responses for each scenario defined in mock_responses.yaml.
#
# Covers:
#   • equals / contains / starts_with / regex match types
#   • simple text replies
#   • tool-call sequences (tool executes then model replies)
#   • default fallback
#   • case-insensitivity

load helpers

# ── Equals match ──────────────────────────────────────────────────────────────

@test "03.01 exact 'ping' returns 'pong'" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}

@test "03.02 exact 'hello' returns expected greeting" {
    run bash -c 'echo "hello" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "hello from sven mock agent"
}

# ── Contains match ────────────────────────────────────────────────────────────

@test "03.03 input containing 'what is rust' returns Rust explanation" {
    run bash -c 'echo "Tell me: what is rust?" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Rust is a systems programming language"
}

@test "03.04 contains match is case-insensitive" {
    run bash -c 'echo "WHAT IS RUST?" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Rust is a systems programming language"
}

@test "03.05 'make a plan' triggers plan mode response" {
    run bash -c 'echo "make a plan for the project" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Plan"
}

@test "03.06 research input triggers research summary" {
    run bash -c 'echo "please research the codebase" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Research Summary"
}

# ── Starts-with match ─────────────────────────────────────────────────────────

@test "03.07 starts_with 'plan' triggers plan response" {
    run bash -c 'echo "plan the implementation" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Plan"
}

@test "03.08 starts_with 'summarize' returns summary" {
    run bash -c 'echo "summarize the output above" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Summary"
}

@test "03.09 british spelling 'summarise' also matches" {
    run bash -c 'echo "summarise the findings" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Summary"
}

# ── Default fallback ──────────────────────────────────────────────────────────

@test "03.10 unrecognised input falls back to default reply" {
    run bash -c 'echo "xyzzy frobnicator 42" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Mock response"
}

@test "03.11 default reply is non-empty" {
    run bash -c 'echo "completely_unrecognised_$$" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    [ -n "${output}" ]
}

# ── Tool-call sequences ───────────────────────────────────────────────────────

@test "03.12 'write a file' sequence exits 0" {
    run bash -c 'echo "write a file for me" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "03.13 'write a file' sequence produces stdout output" {
    run bash -c 'echo "write a file for me" | "$BIN" --headless --model mock 2>/dev/null'
    [ -n "${output}" ]
}

@test "03.14 'write a file' after-tool reply appears in stdout" {
    run bash -c 'echo "write a file for me" | "$BIN" --headless --model mock 2>/dev/null'
    assert_output_contains "written"
}

@test "03.15 'write a file' actually creates /tmp/sven_e2e_test.txt" {
    rm -f /tmp/sven_e2e_test.txt
    bash -c 'echo "write a file for me" | "$BIN" --headless --model mock 2>/dev/null' || true
    [ -f /tmp/sven_e2e_test.txt ]
}

@test "03.16 'run echo' sequence exits 0" {
    run bash -c 'echo "run echo sven_test" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "03.17 'run echo' after-tool reply appears in stdout" {
    run bash -c 'echo "run echo test" | "$BIN" --headless --model mock 2>/dev/null'
    assert_output_contains "executed"
}

@test "03.18 'find rust files' glob tool sequence exits 0" {
    run bash -c 'echo "find rust files in the project" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

# ── Markdown multi-step file ──────────────────────────────────────────────────

@test "03.19 plan.md file: both steps produce output" {
    run bash -c '"$BIN" --headless --model mock --file "$FIXTURES/plan.md" 2>/dev/null'
    [ "${status}" -eq 0 ]
    [ -n "${output}" ]
}

@test "03.20 plan.md file: 'codebase' step matches analysis reply" {
    run bash -c '"$BIN" --headless --model mock --file "$FIXTURES/plan.md" 2>/dev/null'
    assert_output_contains "analysis"
}

@test "03.21 three_steps.md: step one reply is present" {
    run bash -c '"$BIN" --headless --model mock --file "$FIXTURES/three_steps.md" 2>/dev/null'
    assert_output_contains "Step 1 complete"
}

@test "03.22 three_steps.md: step two reply is present" {
    run bash -c '"$BIN" --headless --model mock --file "$FIXTURES/three_steps.md" 2>/dev/null'
    assert_output_contains "Step 2 complete"
}

@test "03.23 three_steps.md: step three reply is present" {
    run bash -c '"$BIN" --headless --model mock --file "$FIXTURES/three_steps.md" 2>/dev/null'
    assert_output_contains "Step 3 complete"
}

# ── --mode flags change system prompt ─────────────────────────────────────────

@test "03.24 --mode research exits 0 with mock model" {
    run bash -c 'echo "research the project" | "$BIN" --headless --mode research --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Research"
}

@test "03.25 --mode plan exits 0 with mock model" {
    run bash -c 'echo "make a plan" | "$BIN" --headless --mode plan --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Plan"
}

@test "03.26 --mode agent exits 0 with mock model" {
    run bash -c 'echo "ping" | "$BIN" --headless --mode agent --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}
