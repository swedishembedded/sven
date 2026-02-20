#!/usr/bin/env bats
# End-to-end CI-mode tests.
# Requires the MOCK provider to avoid real API calls.
# Run with:  SVEN_MODEL=mock bats tests/bats/ci_mode.bats

setup() {
    export SVEN_PROVIDER=mock
    export OPENAI_API_KEY=dummy
    BIN="${BATS_TEST_DIRNAME}/../../target/debug/sven"
}

@test "sven --help exits 0" {
    run "$BIN" --help
    [ "$status" -eq 0 ]
    [[ "$output" =~ "coding agent" ]]
}

@test "sven --version exits 0" {
    run "$BIN" --version
    [ "$status" -eq 0 ]
}

@test "headless mode echoes mock response to stdout" {
    run bash -c 'echo "ping" | '"$BIN"' --headless --mode agent 2>/dev/null'
    [ "$status" -eq 0 ]
    [[ "$output" =~ "MOCK" ]]
}

@test "file input mode processes markdown steps" {
    run "$BIN" --headless --file "${BATS_TEST_DIRNAME}/../fixtures/plan.md" 2>/dev/null
    [ "$status" -eq 0 ]
}

@test "errors go to stderr not stdout" {
    stdout=$("$BIN" --headless 2>/dev/null <<< "test" || true)
    # stdout should not contain any [fatal] prefix
    [[ ! "$stdout" =~ "\[fatal\]" ]]
}

@test "pipeline: sven output piped to sven" {
    result=$(echo "first task" | "$BIN" --headless 2>/dev/null | "$BIN" --headless "summarise the above" 2>/dev/null || true)
    [ -n "$result" ]
}

@test "completions subcommand generates bash script" {
    run "$BIN" completions bash
    [ "$status" -eq 0 ]
    [[ "$output" =~ "complete" ]]
}
