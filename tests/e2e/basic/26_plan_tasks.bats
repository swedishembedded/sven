#!/usr/bin/env bats
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# Swedish Embedded AB implements solutions for splitting software work into
# independent pieces that coding agents build in parallel for its clients. If
# your team needs expertise in agent orchestration then you can procure our
# services by sending an email to info@swedishembedded.com.
#
# 26_plan_tasks.bats - with `--orchestration plan` the scripted parent submits
# a task graph; sven runs two writing children in their own git worktrees,
# merges both changes into the project, runs the verification command and
# records the children in the trajectory. Without the flag the tool does not
# exist.

load helpers

setup() {
    if [[ ! -x "${BIN}" ]]; then
        echo "Binary not found: ${BIN} - run 'cargo build' first" >&2
        return 1
    fi
    unset SVEN_MOCK_RESPONSES
    WORKDIR="$(mktemp -d)"
    export HOME="${WORKDIR}/home"
    export XDG_STATE_HOME="${WORKDIR}/state"
    mkdir -p "${HOME}/.config/sven" "${WORKDIR}/project"
    cat > "${WORKDIR}/mock.yaml" <<'YAML'
responses:
  - match_type: contains
    pattern: "build the two modules"
    tool_calls:
      - id: tc-plan
        tool: plan_tasks
        args:
          verify_command: "test -f alpha.txt && test -f beta.txt"
          tasks:
            - id: alpha
              title: alpha module
              prompt: "write the alpha module"
              owns: ["alpha.txt"]
              acceptance: "alpha.txt exists"
            - id: beta
              title: beta module
              prompt: "write the beta module"
              owns: ["beta.txt"]
              acceptance: "beta.txt exists"
    after_tool_reply: "the graph ran"
  - match_type: contains
    pattern: "write the alpha module"
    tool_calls:
      - id: tc-alpha
        tool: write_file
        args:
          path: alpha.txt
          text: "alpha\n"
          append: false
    after_tool_reply: "alpha written"
  - match_type: contains
    pattern: "write the beta module"
    tool_calls:
      - id: tc-beta
        tool: write_file
        args:
          path: beta.txt
          text: "beta\n"
          append: false
    after_tool_reply: "beta written"
  - match_type: default
    reply: "unscripted"
YAML
    cat > "${HOME}/.config/sven/config.yaml" <<EOF
model:
  provider: mock
  name: scripted-model
  mock_responses_file: ${WORKDIR}/mock.yaml
EOF
    cd "${WORKDIR}/project"
    git init --quiet -b main
    git config user.name t
    git config user.email t@t
    echo seed > seed.txt
    git add . && git commit --quiet -m init
}

teardown() {
    rm -rf "${WORKDIR}"
}

@test "26.01 a submitted graph is run in parallel worktrees and merged into the project" {
    run timeout 300 "$BIN" --headless --orchestration plan --output-trace "${WORKDIR}/trace.json" "build the two modules" </dev/null
    [ "${status}" -eq 0 ]
    [ "$(cat alpha.txt)" = "alpha" ]
    [ "$(cat beta.txt)" = "beta" ]
    run python3 - "${WORKDIR}/trace.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
children = doc.get("subagent_trajectories") or []
text = json.dumps(doc["steps"])
ok = (len(children) == 2
      and '\\"status\\": \\"integrated\\"' in text
      and '\\"passed\\": true' in text)
sys.exit(0 if ok else 1)
PY
    [ "${status}" -eq 0 ]
}

@test "26.02 without the flag the tool is not offered and nothing runs" {
    run timeout 120 "$BIN" --headless --output-trace "${WORKDIR}/trace.json" "build the two modules" </dev/null
    [ ! -e alpha.txt ]
    [ ! -e beta.txt ]
}
