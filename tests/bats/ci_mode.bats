#!/usr/bin/env bats
# ci_mode.bats – legacy smoke tests (kept for backwards compatibility).
# Uses the same mock-model infrastructure as the numbered test suites.

load helpers

@test "sven --help exits 0" {
    run "$BIN" --help
    [ "$status" -eq 0 ]
    [[ "$output" =~ "coding agent" ]]
}

@test "sven --version exits 0" {
    run "$BIN" --version
    [ "$status" -eq 0 ]
}

@test "headless mode outputs mock response to stdout" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock 2>/dev/null'
    [ "$status" -eq 0 ]
    [ -n "$output" ]
}

@test "file input mode processes markdown steps" {
    run "$BIN" --headless --model mock \
        --file "${BATS_TEST_DIRNAME}/../fixtures/plan.md" 2>/dev/null
    [ "$status" -eq 0 ]
}

@test "errors go to stderr not stdout" {
    run_split_output bash -c '"$BIN" --headless --model mock <<< "test"'
    [[ "${STDOUT_OUT}" != *"[fatal]"* ]]
}

@test "pipeline: sven output piped to sven" {
    run bash -c \
        'echo "make a plan" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "summarise the above" 2>/dev/null'
    [ "$status" -eq 0 ]
    [ -n "$output" ]
}

@test "completions subcommand generates bash script" {
    run "$BIN" completions bash
    [ "$status" -eq 0 ]
    [[ "$output" =~ "complete" ]]
}
