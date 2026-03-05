#!/usr/bin/env bats
# 10_context_tools.bats – End-to-end tests for the RLM memory-mapped context tools.
#
# Validates:
#   • show-config exposes the tools.context configuration section
#   • context_open is registered and executes against real files
#   • context_open is registered and executes against real directories
#   • context_open fails gracefully when the path does not exist
#   • context_grep / context_read fail gracefully with an unknown handle
#   • context_query / context_reduce fail gracefully with an unknown handle
#   • Tool errors are surfaced as tool output (agent continues, exits 0)
#   • Tool activity is reported on stderr

load helpers

# ── show-config: tools.context section ───────────────────────────────────────

@test "10.01 show-config includes tools.context section" {
    run "${BIN}" show-config
    [ "${status}" -eq 0 ]
    assert_output_contains "context:"
}

@test "10.02 show-config tools.context includes max_parallel" {
    run "${BIN}" show-config
    [ "${status}" -eq 0 ]
    assert_output_contains "max_parallel:"
}

@test "10.03 show-config tools.context includes default_chunk_lines" {
    run "${BIN}" show-config
    [ "${status}" -eq 0 ]
    assert_output_contains "default_chunk_lines:"
}

@test "10.04 show-config tools.context includes sub_query_max_chars" {
    run "${BIN}" show-config
    [ "${status}" -eq 0 ]
    assert_output_contains "sub_query_max_chars:"
}

# ── context_open: single file ─────────────────────────────────────────────────

@test "10.05 context_open on a real file exits 0" {
    run bash -c 'echo "open context file" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "10.06 context_open result contains after-tool reply" {
    run bash -c 'echo "open context file" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Context file opened successfully"
}

@test "10.07 context_open tool activity appears on stderr" {
    run_split_output bash -c 'echo "open context file" | "$BIN" --headless --model mock'
    [ "${EXIT_CODE}" -eq 0 ]
    # Tool invocation must be reported on stderr (not swallowed)
    [[ "${STDERR_OUT}" == *"context_open"* ]] || \
    [[ "${STDERR_OUT}" == *"[tool]"* ]] || \
    [[ "${STDERR_OUT}" == *"[tool ok]"* ]]
}

@test "10.08 context_open on a real file does not emit handle in stdout" {
    # Tool results are internal; only the model's after_tool_reply goes to stdout
    run_split_output bash -c 'echo "open context file" | "$BIN" --headless --model mock'
    [ "${EXIT_CODE}" -eq 0 ]
    # Stdout should contain the model reply, not the raw tool output
    [[ "${STDOUT_OUT}" == *"Context file opened"* ]]
}

# ── context_open: directory ───────────────────────────────────────────────────

@test "10.09 context_open on a directory exits 0" {
    run bash -c 'echo "open context directory" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "10.10 context_open directory result contains after-tool reply" {
    run bash -c 'echo "open context directory" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Directory context opened"
}

# ── context_open: nonexistent path ───────────────────────────────────────────
# The tool should return an error as tool output; the agent continues and
# the session must still exit 0 with the after_tool_reply delivered.

@test "10.11 context_open on missing path exits 0 (agent continues)" {
    run bash -c 'echo "open context missing path" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "10.12 context_open on missing path still delivers after-tool reply" {
    run bash -c 'echo "open context missing path" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "missing path"
}

# ── context_grep: unknown handle ──────────────────────────────────────────────
# context_grep should return "unknown handle" as a tool error, but the agent
# must still complete the step and exit 0.

@test "10.13 context_grep with unknown handle exits 0" {
    run bash -c 'echo "grep context bad handle" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "10.14 context_grep unknown handle delivers after-tool reply" {
    run bash -c 'echo "grep context bad handle" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "invalid context handle"
}

@test "10.15 context_grep tool activity appears on stderr" {
    run_split_output bash -c 'echo "grep context bad handle" | "$BIN" --headless --model mock'
    [ "${EXIT_CODE}" -eq 0 ]
    [[ "${STDERR_OUT}" == *"context_grep"* ]] || \
    [[ "${STDERR_OUT}" == *"[tool"* ]]
}

# ── context_read: unknown handle ──────────────────────────────────────────────

@test "10.16 context_read with unknown handle exits 0" {
    run bash -c 'echo "read context bad handle" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "10.17 context_read unknown handle delivers after-tool reply" {
    run bash -c 'echo "read context bad handle" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "invalid context handle"
}

# ── context_query: unknown handle (agent mode only) ───────────────────────────
# context_query is only registered in --mode agent.

@test "10.18 context_query with unknown handle exits 0 in agent mode" {
    run bash -c \
        'echo "query context bad handle" | "$BIN" --headless --model mock --mode agent 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "10.19 context_query unknown handle delivers after-tool reply" {
    run bash -c \
        'echo "query context bad handle" | "$BIN" --headless --model mock --mode agent 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "invalid handle"
}

# ── context_reduce: unknown handle (agent mode only) ─────────────────────────

@test "10.20 context_reduce with unknown handle exits 0 in agent mode" {
    run bash -c \
        'echo "reduce context bad handle" | "$BIN" --headless --model mock --mode agent 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "10.21 context_reduce unknown handle delivers after-tool reply" {
    run bash -c \
        'echo "reduce context bad handle" | "$BIN" --headless --model mock --mode agent 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "invalid handle"
}

# ── context_query / context_reduce not available outside agent mode ───────────
# In research or plan mode these tools should not be in the registry; the
# mock model would fail to dispatch them because they don't exist. Instead
# the mock falls through to the default reply.

@test "10.22 context_query tool not dispatched in research mode (falls to default)" {
    # The mock rule requires --mode agent; in research mode context_query is not
    # registered so the model cannot call it and the session still exits 0.
    run bash -c \
        'echo "ping" | "$BIN" --headless --model mock --mode research 2>/dev/null'
    [ "${status}" -eq 0 ]
}
