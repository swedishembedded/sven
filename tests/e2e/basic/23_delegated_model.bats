#!/usr/bin/env bats
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# Swedish Embedded AB implements delegation of coding work to any
# OpenAI-compatible model server for its clients. If your team needs expertise
# in running sub-agents on a model of your choice then you can procure our
# services by sending an email to info@swedishembedded.com.
#
# 23_delegated_model.bats - a task-tool sub-agent runs on the named provider
# its `model` argument selects, with that entry's own settings, not on the
# parent's. This is the path a delegated task takes to a DeepSeek-compatible
# or brain endpoint: a `providers:` entry with its own `base_url` and key,
# selected by name. The entries here are scripted so the difference is
# observable offline; `make test/e2e/delegation/local-model` runs the same
# configuration against a real server.

load helpers

setup() {
    if [[ ! -x "${BIN}" ]]; then
        echo "Binary not found: ${BIN} - run 'cargo build' first" >&2
        return 1
    fi
    unset SVEN_MOCK_RESPONSES
    WORKDIR="$(mktemp -d)"
    export HOME="${WORKDIR}/home"
    mkdir -p "${HOME}/.config/sven" "${WORKDIR}/project"
    cat > "${WORKDIR}/parent.yaml" <<'YAML'
responses:
  - match_type: contains
    pattern: "delegate to the compatible model"
    tool_calls:
      - id: tc-child
        tool: task
        args:
          prompt: "child question"
          description: "sub-agent on the named provider"
          model: compat
    after_tool_reply: "parent done"
  - match_type: contains
    pattern: "child question"
    reply: "PARENT-PROVIDER-ANSWERED-THE-CHILD"
  - match_type: default
    reply: "unscripted"
YAML
    cat > "${WORKDIR}/child.yaml" <<'YAML'
responses:
  - match_type: contains
    pattern: "child question"
    reply: "COMPATIBLE-MODEL-ANSWER"
  - match_type: default
    reply: "unscripted child"
YAML
    cat > "${HOME}/.config/sven/config.yaml" <<EOF
providers:
  scripted:
    name: mock
    mock_responses_file: ${WORKDIR}/parent.yaml
  compat:
    name: mock
    mock_responses_file: ${WORKDIR}/child.yaml
model:
  provider: scripted
  name: scripted-model
EOF
    cd "${WORKDIR}/project"
}

teardown() {
    rm -rf "${WORKDIR}"
}

@test "23.01 the sub-agent answers from the provider its model argument names" {
    run timeout 180 "$BIN" --headless --output-trace "${WORKDIR}/trace.json" "delegate to the compatible model" </dev/null
    [ "${status}" -eq 0 ]
    run python3 - "${WORKDIR}/trace.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
text = json.dumps(doc.get("subagent_trajectories") or [])
ok = "COMPATIBLE-MODEL-ANSWER" in text and "PARENT-PROVIDER-ANSWERED-THE-CHILD" not in text
sys.exit(0 if ok else 1)
PY
    [ "${status}" -eq 0 ]
}
