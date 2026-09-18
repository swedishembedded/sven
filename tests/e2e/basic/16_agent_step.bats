#!/usr/bin/env bats
# 16_agent_step.bats - `sven agent step`, the CLI surface of the SDK.
#
# This command is the shell-level form of what a service does per request:
# load an agent's state, advance it by exactly one step, persist, and exit.
# Every assertion below is about that contract holding *across processes* -
# the point is that nothing of the agent survives in memory between the two
# invocations, only the state file.
#
# It is built on sven-sdk rather than on RuntimeBuilder directly, so a break
# here is a break in the framework's public surface, not just in the CLI.

load helpers

setup() {
    if [[ ! -x "${BIN}" ]]; then
        skip "Binary not found: ${BIN} - run 'cargo build' first"
    fi
    STATE_DIR="$(mktemp -d)"
    export STATE_DIR
}

teardown() {
    rm -rf "${STATE_DIR}"
}

@test "16.01 agent step runs one step and prints the reply" {
    run sven_mock agent step "ping"
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}

@test "16.02 agent step without --state keeps nothing" {
    run sven_mock agent step "ping"
    [ "${status}" -eq 0 ]
    # No file should be created anywhere the command was not told to write.
    [ -z "$(ls -A "${STATE_DIR}")" ]
}

@test "16.03 agent step writes the state file it was given" {
    run sven_mock agent step --state "${STATE_DIR}/a.json" "ping"
    [ "${status}" -eq 0 ]
    [ -f "${STATE_DIR}/a.json" ]
}

@test "16.04 a second process resumes the conversation the first left" {
    sven_mock agent step --state "${STATE_DIR}/a.json" "ping" >/dev/null

    # A separate process, sharing nothing but the file on disk.
    run sven_mock agent step --state "${STATE_DIR}/a.json" "ping"
    [ "${status}" -eq 0 ]

    # Both turns must be in the persisted conversation, which is only possible
    # if the second process loaded what the first one wrote. The state is one
    # JSON line, so count occurrences rather than matching lines.
    local turns
    turns="$(grep -o '"role"' "${STATE_DIR}/a.json" | wc -l)"
    [ "${turns}" -ge 4 ]
}

@test "16.05 the persisted state records where the kernel stopped" {
    sven_mock agent step --state "${STATE_DIR}/a.json" "ping" >/dev/null
    run grep -q '"kernel"' "${STATE_DIR}/a.json"
    [ "${status}" -eq 0 ]
}

@test "16.06 --mode selects the machine and is remembered" {
    sven_mock agent step --state "${STATE_DIR}/a.json" --mode chat "ping" >/dev/null
    run grep -q '"mode":"chat"' "${STATE_DIR}/a.json"
    [ "${status}" -eq 0 ]
}

@test "16.07 an unknown mode fails and names the mode" {
    run sven_mock agent step --mode no-such-mode "ping"
    [ "${status}" -ne 0 ]
    assert_output_contains "no-such-mode"
}

@test "16.08 a corrupt state file is reported, not silently discarded" {
    echo 'not json' > "${STATE_DIR}/bad.json"
    run sven_mock agent step --state "${STATE_DIR}/bad.json" "ping"
    [ "${status}" -ne 0 ]
    assert_output_contains "bad.json"
}
