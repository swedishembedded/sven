#!/usr/bin/env bats
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# 18_subagent_provider.bats - a task-tool sub-agent runs on the parent's
# provider, including everything a named `providers:` entry configures.
#
# The parent's model comes from a named provider entry. The sub-agent must be
# told that name, not just the driver, or it resolves the driver's defaults
# and silently loses the entry's settings (endpoint, key, and here the mock
# response script, which makes the difference observable).

load helpers

setup() {
    if [[ ! -x "${BIN}" ]]; then
        skip "Binary not found: ${BIN} - run 'cargo build' first"
    fi
    # helpers.bash points every test at the shared mock script; this test's
    # responses must come from the named provider entry instead.
    unset SVEN_MOCK_RESPONSES
    WORKDIR="$(mktemp -d)"
    export HOME="${WORKDIR}/home"
    mkdir -p "${HOME}/.config/sven" "${WORKDIR}/project"
    cat > "${WORKDIR}/responses.yaml" <<'EOF'
responses:
  - match_type: contains
    pattern: "delegate to the child"
    tool_calls:
      - id: tc-child
        tool: task
        args:
          prompt: "child question"
          description: "named-provider sub-agent"
    after_tool_reply: "parent done"
  - match_type: contains
    pattern: "child question"
    reply: "CHILD-SCRIPTED-ANSWER"
  - match_type: default
    reply: "unscripted"
EOF
    cat > "${HOME}/.config/sven/config.yaml" <<EOF
providers:
  scripted:
    name: mock
    mock_responses_file: ${WORKDIR}/responses.yaml
model:
  provider: scripted
  name: scripted-model
EOF
    cd "${WORKDIR}/project"
}

teardown() {
    rm -rf "${WORKDIR}"
}

@test "18.01 a sub-agent keeps the parent's named provider settings" {
    run timeout 180 "$BIN" --headless --output-trace "${WORKDIR}/trace.json" "delegate to the child" </dev/null
    [ "${status}" -eq 0 ]
    run python3 - "${WORKDIR}/trace.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
children = doc.get("subagent_trajectories") or []
text = json.dumps(children)
sys.exit(0 if children and "CHILD-SCRIPTED-ANSWER" in text else 1)
PY
    [ "${status}" -eq 0 ]
}
