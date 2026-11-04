#!/usr/bin/env bats
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# 25_goal.bats - a headless run whose prompt is a `/goal` line.
#
# Validates:
#   • The line sets the goal (the same grammar as the terminal and `sven rc`)
#     and the run takes the goal's turns, reporting each on stderr
#   • The run ends as the goal does: the mock model never acts, so the
#     supervisor blocks the goal after three idle turns and the run exits 5
#     (needs a person)
#   • A `/goal` line used wrongly is refused before anything runs (exit 2)

load helpers

@test "25.01 a goal line takes the goal's turns and exits as the goal ends" {
    run bash -c 'echo "/goal --turns 6 tidy the readme" | "$BIN" --headless --model mock 2>&1'
    [ "${status}" -eq 5 ]
    assert_output_contains "[sven:goal] Goal set (6 turns)."
    assert_output_contains "Goal blocked"
}

@test "25.02 a goal line used wrongly is refused before anything runs" {
    run bash -c 'echo "/goal check" | "$BIN" --headless --model mock 2>&1'
    [ "${status}" -eq 2 ]
    assert_output_contains "Usage:"
}
