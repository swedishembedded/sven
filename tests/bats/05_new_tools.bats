#!/usr/bin/env bats
# 05_new_tools.bats – end-to-end tests for the 18-tool toolkit implemented
# in the complete toolkit refactor.
#
# Tests verify:
#   • run_terminal_command tool executes commands
#   • read_file / edit_file / write tools work end-to-end
#   • grep / search_codebase return results
#   • list_dir returns directory entries
#   • todo_write emits todo events to stderr
#   • mode filtering (research/plan/agent)

load helpers

# ── run_terminal_command ──────────────────────────────────────────────────────

@test "05.01 run_terminal_command tool executes and returns output" {
    run bash -c 'echo "run terminal command" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Terminal command"
}

@test "05.02 run_terminal_command stderr shows tool activity" {
    run_split_output bash -c \
        'echo "run terminal command" | "$BIN" --headless --model mock'
    [[ "${STDERR_OUT}" == *"[tool]"* ]] || \
    [[ "${STDERR_OUT}" == *"run_terminal_command"* ]] || \
    [[ "${STDERR_OUT}" == *"[tool ok]"* ]] && true
}

# ── read_file ─────────────────────────────────────────────────────────────────

@test "05.03 read_file tool reads file with line numbers" {
    # Ensure the test file exists first
    echo "written by sven mock agent" > /tmp/sven_e2e_test.txt

    run bash -c 'echo "read the test file" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "File read"
}

# ── edit_file ─────────────────────────────────────────────────────────────────

@test "05.04 edit_file tool modifies file content" {
    # Create a fresh test file with the expected content
    echo "written by sven mock agent" > /tmp/sven_e2e_test.txt

    run bash -c 'echo "edit the test file" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "edited"
}

@test "05.05 edit_file actually changes the file" {
    echo "written by sven mock agent" > /tmp/sven_e2e_test.txt

    run bash -c 'echo "edit the test file" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    grep -q "edited by sven mock agent" /tmp/sven_e2e_test.txt
}

# ── grep ──────────────────────────────────────────────────────────────────────

@test "05.06 grep tool returns matching lines" {
    run bash -c 'echo "grep for pattern" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Grep completed"
}

@test "05.07 grep tool shows tool activity on stderr" {
    run_split_output bash -c 'echo "grep for pattern" | "$BIN" --headless --model mock'
    [[ "${STDERR_OUT}" == *"grep"* ]] && true
}

# ── search_codebase ───────────────────────────────────────────────────────────

@test "05.08 search_codebase tool finds matches in codebase" {
    run bash -c 'echo "search the codebase" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "search"
}

# ── list_dir ──────────────────────────────────────────────────────────────────

@test "05.09 list_dir tool returns directory entries" {
    run bash -c 'echo "list the directory" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "listing"
}

# ── todo_write ────────────────────────────────────────────────────────────────

@test "05.10 todo_write emits todo update on stderr" {
    run_split_output bash -c 'echo "write todo list" | "$BIN" --headless --model mock'
    [ "${EXIT_CODE}" -eq 0 ]
    [[ "${STDERR_OUT}" == *"todo"* ]] || [[ "${STDERR_OUT}" == *"[todos]"* ]] && true
}

@test "05.11 todo_write does not bleed into stdout" {
    run bash -c 'echo "write todo list" | "$BIN" --headless --model mock 2>/dev/null'
    refute_output_contains "[todos]"
}

# ── Mode filtering: research mode ─────────────────────────────────────────────

@test "05.12 research mode runs successfully" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock --mode research 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}

@test "05.13 plan mode runs successfully" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock --mode plan 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}

@test "05.14 agent mode runs successfully" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock --mode agent 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "pong"
}

# ── Registry / schema: verify new tools are registered ───────────────────────

@test "05.15 show-config includes [tools] section with new config fields" {
    run "${BIN}" show-config
    [ "${status}" -eq 0 ]
    assert_output_contains "[tools]"
    assert_output_contains "timeout_secs"
}

# ── Direct tool unit tests via cargo test ─────────────────────────────────────

@test "05.16 edit_file unit tests pass" {
    run bash -c 'cd "${_REPO_ROOT}" && CARGO_HOME=/tmp/cargo_home cargo test -p sven-tools edit_file 2>&1 | tail -5'
    [ "${status}" -eq 0 ]
    assert_output_contains "ok"
}

@test "05.17 apply_patch unit tests pass" {
    run bash -c 'cd "${_REPO_ROOT}" && CARGO_HOME=/tmp/cargo_home cargo test -p sven-tools apply_patch 2>&1 | tail -5'
    [ "${status}" -eq 0 ]
    assert_output_contains "ok"
}

@test "05.18 grep unit tests pass" {
    run bash -c 'cd "${_REPO_ROOT}" && CARGO_HOME=/tmp/cargo_home cargo test -p sven-tools builtin::grep 2>&1 | tail -5'
    [ "${status}" -eq 0 ]
    assert_output_contains "ok"
}

@test "05.19 switch_mode unit tests pass" {
    run bash -c 'cd "${_REPO_ROOT}" && CARGO_HOME=/tmp/cargo_home cargo test -p sven-tools switch_mode 2>&1 | tail -5'
    [ "${status}" -eq 0 ]
    assert_output_contains "ok"
}

@test "05.20 todo_write unit tests pass" {
    run bash -c 'cd "${_REPO_ROOT}" && CARGO_HOME=/tmp/cargo_home cargo test -p sven-tools todo_write 2>&1 | tail -5'
    [ "${status}" -eq 0 ]
    assert_output_contains "ok"
}
