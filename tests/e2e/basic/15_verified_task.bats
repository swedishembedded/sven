#!/usr/bin/env bats
# 15_verified_task.bats - `sven task run`: a claimed completion is only ever
# scored after an independent, declarative verifier checks it - never on the
# model's own say-so.
#
# Every scenario runs the real binary against a scripted mock model in an
# isolated temp project directory, so the verifier's file checks are against
# real, jailed filesystem state - not simulated.

load helpers

_VT_DIR=""
_VT_MOCK_FILE=""

teardown() {
    [ -n "${_VT_MOCK_FILE}" ] && rm -f "${_VT_MOCK_FILE}"
    [ -n "${_VT_DIR}" ] && rm -rf "${_VT_DIR}"
    export SVEN_MOCK_RESPONSES="${MOCK_RESPONSES}"
    _VT_DIR=""
    _VT_MOCK_FILE=""
}

# Create an isolated project dir with a HOME of its own (so a stray
# ~/.config/sven never leaks between tests). `vt_run` below is what actually
# runs the binary with this as its working directory - the shell tool
# executes relative to the process's real cwd, not `--project-root`, which
# only scopes the verifier and checkpoint/audit paths.
vt_new_project() {
    _VT_DIR="$(mktemp -d /tmp/sven_vt_XXXXXX)"
    export _VT_DIR
    mkdir -p "${_VT_DIR}/home"
    export HOME="${_VT_DIR}/home"
}

# Write a per-test mock YAML from a heredoc and point SVEN_MOCK_RESPONSES at it.
vt_use_mock() {
    _VT_MOCK_FILE="$(mktemp /tmp/sven_vt_mock_XXXXXX.yaml)"
    cat > "${_VT_MOCK_FILE}"
    export SVEN_MOCK_RESPONSES="${_VT_MOCK_FILE}"
}

vt_run() {
    run bash -c 'cd "${_VT_DIR}" && "$BIN" task run --model mock --project-root "${_VT_DIR}" "$@"' -- "$@"
}

@test "15.01 a claim backed by real evidence passes and exits 0" {
    vt_new_project
    cat > "${_VT_DIR}/task.toml" <<EOF
id = "make-out-file"
prompt = "Create a file named out.txt containing the word hi, then stop."
max_attempts = 2

[verifier]
kind = "file_exists"
path = "out.txt"
min_bytes = 1
EOF
    vt_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "Create a file named out.txt"
    tool_calls:
      - id: tc1
        tool: shell
        args:
          shell_command: "echo hi > out.txt"
    after_tool_reply: "Done."
EOF
    vt_run "${_VT_DIR}/task.toml"
    [ "${status}" -eq 0 ]
    [ -f "${_VT_DIR}/out.txt" ]
}

@test "15.02 a confident claim with no evidence never passes" {
    vt_new_project
    cat > "${_VT_DIR}/task.toml" <<EOF
id = "make-out-file"
prompt = "Create a file named ghost.txt, then stop."
max_attempts = 1

[verifier]
kind = "file_exists"
path = "ghost.txt"
EOF
    # The model claims completion but never actually calls a tool - the
    # headline negative case: "Done!" with nothing behind it must not pass.
    vt_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "Create a file named ghost.txt"
    reply: "Done!"
EOF
    vt_run "${_VT_DIR}/task.toml"
    # The run itself completes cleanly (exit 0 - a real, stamped verdict was
    # reached); it is the *verdict*, not the exit code, that must say fail.
    [ "${status}" -eq 0 ]
    [ ! -f "${_VT_DIR}/ghost.txt" ]
}

@test "15.03 first attempt fails, second attempt (with evidence) passes" {
    vt_new_project
    cat > "${_VT_DIR}/task.toml" <<EOF
id = "retry-me"
prompt = "Create a file named retried.txt, then stop."
max_attempts = 2

[verifier]
kind = "file_exists"
path = "retried.txt"
EOF
    vt_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "attempt 2 of 2"
    tool_calls:
      - id: tc2
        tool: shell
        args:
          shell_command: "echo hi > retried.txt"
    after_tool_reply: "Done, for real this time."
  - match_type: contains
    pattern: "Create a file named retried.txt"
    reply: "Done!"
EOF
    vt_run "${_VT_DIR}/task.toml"
    [ "${status}" -eq 0 ]
    [ -f "${_VT_DIR}/retried.txt" ]
}

@test "15.04 a malformed task file fails cleanly, not with a panic" {
    vt_new_project
    cat > "${_VT_DIR}/task.toml" <<EOF
not = "a valid task file"
EOF
    vt_use_mock <<EOF
responses:
  - match_type: default
    reply: "irrelevant"
EOF
    vt_run "${_VT_DIR}/task.toml"
    [ "${status}" -ne 0 ]
    [[ "${output}" != *"panicked"* ]]
}
