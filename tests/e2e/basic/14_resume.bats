#!/usr/bin/env bats
# 14_resume.bats - headless `sven chats` / `--resume` against the real ATIF
# session store (crates/session-store/src/session_resolve.rs), including
# subagent trajectories embedded by the `task` tool.
#
# `--resume <id>` maps onto the same semantics as `--trace <path>` (see
# src/run/ci.rs): the resolved session file is both the load source and the
# write-back target, so resuming appends to the same file rather than
# forking a new session under an auto-log path - and any subagent
# trajectories the session already had must still be there afterwards (see
# crates/ci/src/runner/mod.rs's `loaded_subagents` seeding of
# `completed_subagents`, without which the first flush of a resumed run
# silently dropped every embedded child).
#
# XDG_DATA_HOME is isolated to a fresh temp dir per test so the real user's
# ~/.local/share/sven/sessions is never touched, and `sven chats`/`--resume`
# see only the session this test creates.

load helpers

setup() {
    if [[ ! -x "${BIN}" ]]; then
        skip "Binary not found: ${BIN} - run 'cargo build' first"
    fi
    export XDG_DATA_HOME
    XDG_DATA_HOME="$(mktemp -d)"
}

teardown() {
    rm -rf "${XDG_DATA_HOME}"
}

@test "14.01 headless --resume round-trips through the ATIF session store, subagents included" {
    local sessions_dir session_path session_id
    sessions_dir="${XDG_DATA_HOME}/sven/sessions"
    mkdir -p "${sessions_dir}"
    session_id="resume-test-session"
    session_path="${sessions_dir}/${session_id}.json"

    # Step 1: create a session with a subagent, written straight into the
    # canonical session store via --output-trace (the same file --resume
    # will load from and write back to later).
    run bash -c 'echo "delegate a subtask to a subagent" | "$BIN" --headless --model mock --output-trace "$1"' -- "${session_path}"
    [ "${status}" -eq 0 ]
    run python3 -c "import json,sys; d=json.load(open(sys.argv[1])); sys.exit(0 if d.get('subagent_trajectories') else 1)" "${session_path}"
    [ "${status}" -eq 0 ]

    # Step 2: `sven chats` lists it by id (list_all_sessions, not the retired
    # markdown history archive).
    run "${BIN}" chats
    [ "${status}" -eq 0 ]
    [[ "${output}" == *"${session_id}"* ]]

    # Step 3: `--resume <id>` appends to the SAME file - earlier steps and
    # the embedded subagent must both survive the round trip.
    run bash -c 'echo "ping" | "$BIN" --headless --model mock --resume "$1"' -- "${session_id}"
    [ "${status}" -eq 0 ]
    run python3 -c "
import json, sys
d = json.load(open(sys.argv[1]))
messages = [s.get('message') for s in d['steps']]
assert 'delegate a subtask to a subagent' in messages, messages
assert 'ping' in messages, messages
assert 'pong' in messages, messages
assert d.get('subagent_trajectories'), 'subagent_trajectories must survive a resume'
" "${session_path}"
    [ "${status}" -eq 0 ]
}

@test "14.02 --resume with an unknown id fails clearly instead of silently starting fresh" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock --resume "nonexistent-session-id"'
    [ "${status}" -ne 0 ]
    [[ "${output}" == *"no session found"* ]]
}

@test "14.03 --load-trace of a missing file fails clearly instead of silently starting fresh" {
    run bash -c '"$BIN" --headless --model mock --load-trace "${XDG_DATA_HOME}/missing.json" "ping" </dev/null'
    [ "${status}" -ne 0 ]
    [[ "${output}" == *"missing.json"* ]]
}

@test "14.04 --trace on a fresh path starts a new session and creates the file" {
    run bash -c '"$BIN" --headless --model mock --trace "${XDG_DATA_HOME}/fresh.json" "ping" </dev/null'
    [ "${status}" -eq 0 ]
    [ -s "${XDG_DATA_HOME}/fresh.json" ]
}
