#!/usr/bin/env bats
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# 19_approvals.bats - a headless run never waits for a person.
#
# By default a shell call runs without any prompt, with stdin not a
# terminal. Manual approval needs the interactive TUI: asked for anywhere
# else, sven refuses at once instead of waiting for an answer nobody can give.

load helpers

setup() {
    if [[ ! -x "${BIN}" ]]; then
        skip "Binary not found: ${BIN} - run 'cargo build' first"
    fi
    WORKDIR="$(mktemp -d)"
    cat > "${WORKDIR}/responses.yaml" <<'EOF'
responses:
  - match_type: contains
    pattern: "leave a marker"
    tool_calls:
      - id: tc-marker
        tool: shell
        args:
          shell_command: "touch marker_from_shell"
    after_tool_reply: "The marker is in place."
  - match_type: default
    reply: "unscripted"
EOF
    export SVEN_MOCK_RESPONSES="${WORKDIR}/responses.yaml"
    mkdir -p "${WORKDIR}/project"
    cd "${WORKDIR}/project"
}

teardown() {
    rm -rf "${WORKDIR}"
}

@test "19.01 a headless shell call runs without a prompt, stdin not a terminal" {
    run timeout 120 "${BIN}" --headless --model mock "leave a marker" </dev/null
    [ "${status}" -eq 0 ]
    [ -f "${WORKDIR}/project/marker_from_shell" ]
    refute_output_contains "Allow ExecuteShell"
}

@test "19.02 manual approval in a headless run is refused at start" {
    run timeout 10 "${BIN}" --headless --approval manual --model mock "leave a marker" </dev/null
    [ "${status}" -ne 0 ]
    [ "${status}" -ne 124 ]
    assert_output_contains "--approval manual needs the interactive TUI"
    [ ! -f "${WORKDIR}/project/marker_from_shell" ]
}

@test "19.03 manual approval with a piped stdin is refused at start" {
    run bash -c "echo 'leave a marker' | timeout 10 '${BIN}' --approval manual --model mock"
    [ "${status}" -ne 0 ]
    [ "${status}" -ne 124 ]
    assert_output_contains "--approval manual needs the interactive TUI"
}
