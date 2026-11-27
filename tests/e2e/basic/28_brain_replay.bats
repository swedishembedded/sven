#!/usr/bin/env bats
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# Swedish Embedded AB implements agent runtimes whose recorded sessions can be
# trained on as they ran, for its clients. If your team needs expertise in
# agent trajectories as training data then you can procure our services by
# sending an email to info@swedishembedded.com.
#
# 28_brain_replay.bats - against a real brain server, every request of a tool
# session replays each earlier reply as it was sampled: the server reports
# `brain.history.append_only` on every call, which the trajectory records per
# request. Needs a running `brain serve` with a chat model that calls tools:
#   SVEN_E2E_BRAIN_URL=http://127.0.0.1:8910/v1
#   SVEN_E2E_BRAIN_KEY_FILE=/path/to/key      (the server's --api-key-file)
#   SVEN_E2E_BRAIN_MODEL=Qwen/Qwen3-0.6B
# Skipped when they are not set.

load helpers

setup() {
    if [[ -z "${SVEN_E2E_BRAIN_URL:-}" || -z "${SVEN_E2E_BRAIN_KEY_FILE:-}" || -z "${SVEN_E2E_BRAIN_MODEL:-}" ]]; then
        skip "no brain server (set SVEN_E2E_BRAIN_URL, SVEN_E2E_BRAIN_KEY_FILE, SVEN_E2E_BRAIN_MODEL)"
    fi
    command -v python3 >/dev/null || skip "python3 not installed"
    unset SVEN_MOCK_RESPONSES
    WORKDIR="$(mktemp -d)"
    mkdir -p "${WORKDIR}/project"
    printf 'one\ntwo\n' > "${WORKDIR}/project/a.txt"
    {
        echo "model:"
        echo "  provider: brain"
        echo "  name: ${SVEN_E2E_BRAIN_MODEL}"
        echo "  base_url: ${SVEN_E2E_BRAIN_URL}"
        echo "  api_key_file: ${SVEN_E2E_BRAIN_KEY_FILE}"
        echo "  max_output_tokens: 120"
        echo "  driver_options:"
        echo "    chat_template_kwargs:"
        echo "      enable_thinking: false"
        echo "agent:"
        echo "  max_tool_rounds: 4"
        echo "tools:"
        echo "  only: [\"shell\"]"
    } > "${WORKDIR}/project/.sven.yaml"
}

teardown() {
    if [[ -n "${WORKDIR:-}" ]]; then
        rm -rf "${WORKDIR}"
    fi
}

@test "every call of a tool session against brain is append-only" {
    cd "${WORKDIR}/project"
    run "${BIN}" -H --temperature 0 --output-trace trace.json \
        "Use the shell tool to run ls, then use it again to run wc -l on a.txt, then answer."
    [ "$status" -eq 0 ]
    run python3 -c '
import json
steps = json.load(open("trace.json"))["steps"]
calls = [s for s in steps if s["source"] == "agent" and s.get("metrics")]
histories = [s["metrics"]["extra"]["sven"].get("history") for s in calls]
print(len(calls), histories)
assert len(calls) >= 2, "a multi-call session"
assert all(h and h["append_only"] for h in histories), histories
'
    echo "$output"
    [ "$status" -eq 0 ]
}
