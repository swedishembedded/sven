#!/usr/bin/env bash
# Shared helpers for sven bats end-to-end tests.
#
# Source this from each .bats file:
#   load helpers
#
# All tests assume the binary has been built:
#   cargo build   (or `make build`)

# ── Binary path ──────────────────────────────────────────────────────────────

# Support both debug and release builds; prefer release if present.
_REPO_ROOT="$(cd "${BATS_TEST_DIRNAME}/../.." && pwd)"

if [[ -x "${_REPO_ROOT}/target/release/sven" ]]; then
    BIN="${_REPO_ROOT}/target/release/sven"
else
    BIN="${_REPO_ROOT}/target/debug/sven"
fi

export BIN

# ── Fixture paths ─────────────────────────────────────────────────────────────

export FIXTURES="${_REPO_ROOT}/tests/fixtures"
export MOCK_RESPONSES="${FIXTURES}/mock_responses.yaml"

# ── Environment for mock model ────────────────────────────────────────────────

# Export so every sven invocation picks up the mock responses file.
export SVEN_MOCK_RESPONSES="${MOCK_RESPONSES}"

# Dummy keys so the binary does not bail before reaching the mock provider.
export OPENAI_API_KEY="test-dummy-key"
export ANTHROPIC_API_KEY="test-dummy-key"

# ── Helper: run sven in headless/CI mode with the mock model ──────────────────
#
# Usage:  sven_mock [--mode <mode>] [extra args...] <<< "user input"
#      or: echo "input" | sven_mock [...]
#
# Note: does NOT pre-specify --mode so callers can supply their own.
#
sven_mock() {
    "${BIN}" --headless \
             --model mock \
             "$@"
}

# ── Helper: assert a string is present in output ─────────────────────────────
assert_output_contains() {
    local needle="$1"
    if [[ "${output}" != *"${needle}"* ]]; then
        echo "Expected output to contain: ${needle}"
        echo "Actual output: ${output}"
        return 1
    fi
}

# ── Helper: assert a string is NOT present in output ─────────────────────────
refute_output_contains() {
    local needle="$1"
    if [[ "${output}" == *"${needle}"* ]]; then
        echo "Expected output NOT to contain: ${needle}"
        echo "Actual output: ${output}"
        return 1
    fi
}

# ── Helper: capture stdout and stderr separately ──────────────────────────────
#
# Usage:  run_split_output <cmd> [args...]
#   sets: STDOUT_OUT, STDERR_OUT, EXIT_CODE
#
run_split_output() {
    local tmpout tmp_stderr
    tmpout="$(mktemp)"
    tmp_stderr="$(mktemp)"
    "$@" >"${tmpout}" 2>"${tmp_stderr}"
    EXIT_CODE=$?
    STDOUT_OUT="$(cat "${tmpout}")"
    STDERR_OUT="$(cat "${tmp_stderr}")"
    rm -f "${tmpout}" "${tmp_stderr}"
    export STDOUT_OUT STDERR_OUT EXIT_CODE
}

# ── Helper: generate a unique temp file path ──────────────────────────────────
tmp_file() {
    echo "/tmp/sven_bats_$$_${RANDOM}.tmp"
}

# ── Setup called before each test (override in test files if needed) ──────────
setup() {
    # Verify binary exists
    if [[ ! -x "${BIN}" ]]; then
        skip "Binary not found: ${BIN} — run 'cargo build' first"
    fi
}
