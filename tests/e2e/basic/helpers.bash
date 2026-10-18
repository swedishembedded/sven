#!/usr/bin/env bash
# Shared helpers for sven bats end-to-end tests.
#
# Source this from each .bats file:
#   load helpers
#
# All tests need the binary built; a missing binary FAILS the test (a skipped
# suite would report success without testing anything):
#   cargo build   (or `make build`)

# ── Binary path ──────────────────────────────────────────────────────────────

# Support both debug and release builds; prefer release if present.
_REPO_ROOT="$(cd "${BATS_TEST_DIRNAME}/../../.." && pwd)"
export _REPO_ROOT

# Prefer the newest binary so tests always run against the most recently built
# code.  In CI the release build is preferred; in development the debug build
# may be newer after an incremental `cargo build`.
# cargo writes under CARGO_TARGET_DIR when that is set (shared build trees).
_target="${CARGO_TARGET_DIR:-${_REPO_ROOT}/target}"
_release="${_target}/release/sven"
_debug="${_target}/debug/sven"

if [[ -x "${_release}" && -x "${_debug}" ]]; then
    # Both exist - use whichever was modified more recently.
    if [[ "${_release}" -nt "${_debug}" ]]; then
        BIN="${_release}"
    else
        BIN="${_debug}"
    fi
elif [[ -x "${_release}" ]]; then
    BIN="${_release}"
else
    BIN="${_debug}"
fi

# SVEN_E2E_TOOL_DISCLOSURE=full|deferred (set by `make test/e2e/basic/deferred`)
# runs every invocation of the suite under `--tool-disclosure MODE`. This is a
# switch of the test harness, not of sven: the binary reads no such variable.
if [[ -n "${SVEN_E2E_TOOL_DISCLOSURE:-}" ]]; then
    _wrapper="${BATS_RUN_TMPDIR:-${TMPDIR:-/tmp}}/sven-${SVEN_E2E_TOOL_DISCLOSURE}"
    printf '#!/usr/bin/env bash\nexec "%s" --tool-disclosure "%s" "$@"\n' \
        "${BIN}" "${SVEN_E2E_TOOL_DISCLOSURE}" >"${_wrapper}"
    chmod +x "${_wrapper}"
    BIN="${_wrapper}"
fi

# SVEN_E2E_CONTEXT_ASSEMBLY=off|demote (set by `make tests/e2e/basic/demote`)
# runs every invocation under `--context-assembly MODE`; the same kind of
# harness switch as above.
if [[ -n "${SVEN_E2E_CONTEXT_ASSEMBLY:-}" ]]; then
    _wrapper="${BATS_RUN_TMPDIR:-${TMPDIR:-/tmp}}/sven-context-${SVEN_E2E_CONTEXT_ASSEMBLY}"
    printf '#!/usr/bin/env bash\nexec "%s" --context-assembly "%s" "$@"\n' \
        "${BIN}" "${SVEN_E2E_CONTEXT_ASSEMBLY}" >"${_wrapper}"
    chmod +x "${_wrapper}"
    BIN="${_wrapper}"
fi

# SVEN_E2E_INSTRUCTION_DISCLOSURE=full|deferred (set by
# `make test/e2e/basic/instructions-deferred`) runs every invocation with the
# skill, subagent and knowledge listings under that mode; the same kind of
# harness switch as above.
if [[ -n "${SVEN_E2E_INSTRUCTION_DISCLOSURE:-}" ]]; then
    _wrapper="${BATS_RUN_TMPDIR:-${TMPDIR:-/tmp}}/sven-instructions-${SVEN_E2E_INSTRUCTION_DISCLOSURE}"
    printf '#!/usr/bin/env bash\nexec "%s" --skill-disclosure "%s" --subagent-disclosure "%s" --knowledge-disclosure "%s" "$@"\n' \
        "${BIN}" "${SVEN_E2E_INSTRUCTION_DISCLOSURE}" "${SVEN_E2E_INSTRUCTION_DISCLOSURE}" \
        "${SVEN_E2E_INSTRUCTION_DISCLOSURE}" >"${_wrapper}"
    chmod +x "${_wrapper}"
    BIN="${_wrapper}"
fi

export BIN

# ── Fixture paths ─────────────────────────────────────────────────────────────

export FIXTURES="${_REPO_ROOT}/tests/fixtures"
export MOCK_RESPONSES="${FIXTURES}/mock_responses.yaml"

# ── Cargo environment ─────────────────────────────────────────────────────────

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
    # Capture exit code explicitly so bats set -e doesn't abort the function
    # when the command exits non-zero (e.g. exit 3 = tool warnings).
    EXIT_CODE=0
    "$@" >"${tmpout}" 2>"${tmp_stderr}" || EXIT_CODE=$?
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
        echo "Binary not found: ${BIN} - run 'cargo build' first" >&2
        return 1
    fi
}
