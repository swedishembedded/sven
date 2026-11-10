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
#   • --goal-turns takes the whole prompt verbatim as the objective (quotes,
#     blank lines and spacing kept), with --goal-check as its check; wrong
#     use is refused before anything runs (exit 2)

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

@test "25.03 --goal-turns takes the whole prompt verbatim as the objective" {
    task=$'Fix "the" parser:\n\n  keep   spaces\n/goalpost stays text'
    run bash -c 'printf "%s\n" "$1" | "$BIN" --headless --model mock --goal-turns 4 -vv 2>&1' _ "${task}"
    [ "${status}" -eq 5 ]
    assert_output_contains "[sven:goal] Goal set (4 turns)."
    assert_output_contains "<goal>"$'\n'"${task}"$'\n'"</goal>"
    assert_output_contains "Goal blocked"
}

@test "25.04 --goal-check decides the --goal-turns goal" {
    run bash -c 'echo "tidy the readme" | "$BIN" --headless --model mock --goal-turns 2 --goal-check false 2>&1'
    [ "${status}" -eq 4 ]
    assert_output_contains "[sven:goal] The check fails; turn 2 of 2."
}

@test "25.05 --goal-turns and --goal-check wrongly used are refused before anything runs" {
    for args in "--goal-turns 0" "--goal-turns 501" "--goal-turns x" "--goal-check true" \
                "--goal-turns 3 --goal-check ' '" "--goal-turns 3 --dry-run"; do
        run bash -c 'echo "tidy the readme" | "$BIN" --headless --model mock '"${args}"' 2>&1'
        [ "${status}" -eq 2 ] || { echo "${args}: ${status}: ${output}"; return 1; }
        refute_output_contains "[sven:goal]"
    done
}

@test "25.06 --goal-turns with a /goal line as the prompt is refused" {
    run bash -c 'echo "/goal --turns 3 tidy the readme" | "$BIN" --headless --model mock --goal-turns 3 2>&1'
    [ "${status}" -eq 2 ]
    assert_output_contains "sets its own goal"
    refute_output_contains "[sven:goal]"
}

@test "25.07 a --goal-turns run writes its trajectory where --output-trace says" {
    local trace="${BATS_TEST_TMPDIR}/goal.atif.json"
    run bash -c 'echo "tidy the readme" | "$BIN" --headless --model mock --goal-turns 2 --goal-check false --output-trace "$0" 2>&1' "${trace}"
    [ "${status}" -eq 4 ]
    assert_output_contains "[sven:goal] The check fails; turn 2 of 2."
    [ -s "${trace}" ]
    grep -q '"schema_version": *"ATIF-' "${trace}"
}
