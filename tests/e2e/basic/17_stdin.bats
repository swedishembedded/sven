#!/usr/bin/env bats
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# 17_stdin.bats - which headless runs read stdin.
#
# The decision comes from the arguments alone: with a PROMPT, stdin is read
# only when --stdin asks for it; without one, a non-terminal stdin is the task.
# So a run that already has its task never waits on an inherited pipe that
# nobody writes to or closes.

load helpers

setup() {
    if [[ ! -x "${BIN}" ]]; then
        skip "Binary not found: ${BIN} - run 'cargo build' first"
    fi
    WORKDIR="$(mktemp -d)"
    cd "${WORKDIR}"
}

teardown() {
    rm -rf "${WORKDIR}"
}

@test "17.01 a prompt with a silent, never-closed stdin runs without waiting for it" {
    # The process substitution holds the pipe open far longer than the run may take.
    run_split_output timeout 90 "$BIN" --headless --model mock "ping" < <(sleep 120)
    [ "${EXIT_CODE}" -eq 0 ]
    [[ "${STDERR_OUT}" != *"waiting for stdin"* ]]
}

@test "17.02 a prompt ignores piped stdin unless --stdin is given" {
    run_split_output timeout 90 "$BIN" --headless --model mock "please" \
        < <(echo "write a file for me")
    [ "${EXIT_CODE}" -eq 0 ]
    [[ "${STDERR_OUT}" != *'name="write_file"'* ]]
}

@test "17.03 --stdin appends stdin to the prompt, however slowly it arrives" {
    run_split_output timeout 90 "$BIN" --headless --model mock --stdin "please" \
        < <(sleep 4; echo "write a file for me")
    [ "${EXIT_CODE}" -eq 0 ]
    [[ "${STDERR_OUT}" == *'name="write_file"'* ]]
}

@test "17.04 without a prompt, stdin is the task and is waited for" {
    run_split_output timeout 90 "$BIN" --headless --model mock \
        < <(sleep 4; echo "write a file for me")
    [ "${EXIT_CODE}" -eq 0 ]
    [[ "${STDERR_OUT}" == *'name="write_file"'* ]]
}
