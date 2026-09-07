#!/usr/bin/env bats
# 13_semantic_memory.bats - End-to-end coverage for the `semantic_memory`
# tool (sven-memory's SQLite + FTS5 store), registered by default since the
# `memory` cargo feature (see crates/bootstrap/Cargo.toml).
#
# Validates the full remember -> recall round-trip through the real registry
# (not a stub): a fact written by one headless invocation is found by a
# `recall` in a second, separate process, proving the store is real and
# persists to disk rather than being an in-memory no-op.
#
# `-v` is required for either assertion: at default verbosity
# `[sven:tool:result]` carries no `output=` snippet at all (see
# 08_trace_output.bats), so this is the only way to see the tool's real
# output rather than the mock's scripted `after_tool_reply` text (which the
# mock always emits once a tool result exists, success or not, and so proves
# nothing about the tool itself).

load helpers

@test "13.01 semantic_memory remember then recall round-trips a fact" {
    # Each invocation is a separate `sven` process; the store must be the
    # same real SQLite file on disk across both for recall to see what
    # remember wrote. HOME is pinned to a scratch directory so the test
    # never touches (or depends on) the real user's memory store.
    local home_dir
    home_dir="$(mktemp -d)"

    run_split_output env HOME="${home_dir}" bash -c \
        'echo "remember the launch code" | "$BIN" --headless --model mock -v'
    [ "${EXIT_CODE}" -eq 0 ]
    [[ "${STDERR_OUT}" == *"semantic_memory"* ]]

    run_split_output env HOME="${home_dir}" bash -c \
        'echo "recall the launch code" | "$BIN" --headless --model mock -v'
    [ "${EXIT_CODE}" -eq 0 ]
    [[ "${STDERR_OUT}" == *"sven-e2e-marker"* ]]

    rm -rf "${home_dir}"
}
