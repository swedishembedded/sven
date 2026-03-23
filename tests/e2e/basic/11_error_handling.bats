#!/usr/bin/env bats
# 11_error_handling.bats – Error handling and graceful failure scenarios.
#
# Validates:
#   • Missing API keys produce clear, actionable error messages before any
#     network request is made (pre-flight validation in sven-model)
#   • Error messages name the exact environment variable to set
#   • Error messages include an `export VAR=<key>` example
#   • Errors go to stderr; stdout remains clean
#   • Exit code is non-zero on all hard failures
#   • Missing --file path exits non-zero with a descriptive message
#   • Unknown --model provider exits non-zero
#
# NOTE: These tests deliberately do NOT load helpers.bash for the API-key tests
# so that the dummy keys exported there do not interfere.  Each test creates its
# own isolated environment by unsetting the relevant variables in a subshell.

load helpers

# ── Missing API key — OpenAI ──────────────────────────────────────────────────

@test "11.01 missing OPENAI_API_KEY exits non-zero" {
    run bash -c 'unset OPENAI_API_KEY; echo "hi" | "$BIN" --headless --model openai 2>&1'
    [ "${status}" -ne 0 ]
}

@test "11.02 missing OPENAI_API_KEY error names the environment variable" {
    run bash -c 'unset OPENAI_API_KEY; echo "hi" | "$BIN" --headless --model openai 2>&1'
    [ "${status}" -ne 0 ]
    assert_output_contains "OPENAI_API_KEY"
}

@test "11.03 missing OPENAI_API_KEY error includes export instruction" {
    run bash -c 'unset OPENAI_API_KEY; echo "hi" | "$BIN" --headless --model openai 2>&1'
    [ "${status}" -ne 0 ]
    assert_output_contains "export"
}

@test "11.04 missing OPENAI_API_KEY error goes to stderr not stdout" {
    local stderr_file stdout_file
    stderr_file="$(mktemp)"
    stdout_file="$(mktemp)"
    bash -c 'unset OPENAI_API_KEY; echo "hi" | "$BIN" --headless --model openai' \
        >"${stdout_file}" 2>"${stderr_file}" || true
    # Error must be in stderr
    grep -q "OPENAI_API_KEY" "${stderr_file}"
    # Stdout must not contain the error (it is not a model response)
    local out
    out="$(cat "${stdout_file}")"
    [[ "${out}" != *"OPENAI_API_KEY"* ]]
    rm -f "${stderr_file}" "${stdout_file}"
}

# ── Missing API key — Anthropic ───────────────────────────────────────────────

@test "11.05 missing ANTHROPIC_API_KEY exits non-zero" {
    run bash -c 'unset ANTHROPIC_API_KEY; echo "hi" | "$BIN" --headless --model anthropic 2>&1'
    [ "${status}" -ne 0 ]
}

@test "11.06 missing ANTHROPIC_API_KEY error names the environment variable" {
    run bash -c 'unset ANTHROPIC_API_KEY; echo "hi" | "$BIN" --headless --model anthropic 2>&1'
    [ "${status}" -ne 0 ]
    assert_output_contains "ANTHROPIC_API_KEY"
}

@test "11.07 missing ANTHROPIC_API_KEY error includes export instruction" {
    run bash -c 'unset ANTHROPIC_API_KEY; echo "hi" | "$BIN" --headless --model anthropic 2>&1'
    [ "${status}" -ne 0 ]
    assert_output_contains "export"
}

# ── Missing --file path ───────────────────────────────────────────────────────

@test "11.08 --file pointing to nonexistent path exits non-zero" {
    run "${BIN}" --headless --model mock \
        --file /tmp/sven_bats_no_such_file_xyz_99.md 2>/dev/null
    [ "${status}" -ne 0 ]
}

@test "11.09 --file nonexistent path error mentions the file path" {
    run "${BIN}" --headless --model mock \
        --file /tmp/sven_bats_no_such_file_xyz_99.md 2>&1
    [ "${status}" -ne 0 ]
    assert_output_contains "sven_bats_no_such_file_xyz_99"
}

@test "11.10 --file nonexistent path error is on stderr" {
    local stderr_file stdout_file
    stderr_file="$(mktemp)"
    stdout_file="$(mktemp)"
    "${BIN}" --headless --model mock \
        --file /tmp/sven_bats_no_such_file_xyz_99.md \
        >"${stdout_file}" 2>"${stderr_file}" || true
    grep -q "sven_bats_no_such_file_xyz_99" "${stderr_file}"
    rm -f "${stderr_file}" "${stdout_file}"
}

# ── Invalid --mode value ──────────────────────────────────────────────────────

@test "11.11 invalid --mode value exits non-zero" {
    run "${BIN}" --headless --model mock --mode notamode <<< "hi"
    [ "${status}" -ne 0 ]
}

# ── Config file export includes all required sections ─────────────────────────
# Verify that show-config is a complete, consistent snapshot of the running
# configuration — useful as a canary for config regressions.

@test "11.12 show-config is non-empty" {
    run "${BIN}" show-config
    [ "${status}" -eq 0 ]
    [ -n "${output}" ]
}

@test "11.13 show-config contains all top-level sections" {
    run "${BIN}" show-config
    [ "${status}" -eq 0 ]
    assert_output_contains "model:"
    assert_output_contains "agent:"
    assert_output_contains "tools:"
    assert_output_contains "tui:"
}

# ── sven validate subcommand (from 06_headless_enhancements.bats): error exit ─

@test "11.14 validate with missing --file exits non-zero" {
    run "${BIN}" validate --file /tmp/sven_bats_validate_no_such_file_xyz.md 2>/dev/null
    [ "${status}" -ne 0 ]
}

# ── Empty stdin handling ──────────────────────────────────────────────────────

@test "11.15 empty stdin in headless mode completes without hanging" {
    # An empty workflow has nothing to run — sven must not hang and must exit.
    run timeout 10 bash -c 'echo "" | "$BIN" --headless --model mock 2>/dev/null'
    # status 124 means timeout killed the process (hang detected).
    [ "${status}" -ne 124 ]
}
