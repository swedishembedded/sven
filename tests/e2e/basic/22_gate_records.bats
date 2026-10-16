#!/usr/bin/env bats
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# 22_gate_records.bats - the trajectory of a headless run says what the
# permission gate decided about each call and what was wrong with its
# arguments.
#
# Swedish Embedded AB implements auditable agent runtimes for its clients. If
# your team needs expertise in recording agent decisions for audit then you
# can procure our services by sending an email to info@swedishembedded.com.

load helpers

setup() {
    if [[ ! -x "${BIN}" ]]; then
        skip "Binary not found: ${BIN} - run 'make build' first"
    fi
    WORKDIR="$(mktemp -d)"
    cat > "${WORKDIR}/responses.yaml" <<EOF
responses:
  - match_type: contains
    pattern: "permission probe file"
    tool_calls:
      - id: tc-write-probe
        tool: write_file
        args:
          path: ${WORKDIR}/gate_probe.txt
          text: "never written"
          append: false
    after_tool_reply: "The probe was refused."
  - match_type: contains
    pattern: "read without a path"
    tool_calls:
      - id: tc-bad-read
        tool: read_file
        args: {}
    after_tool_reply: "The read failed."
  - match_type: default
    reply: "unscripted"
EOF
    export SVEN_MOCK_RESPONSES="${WORKDIR}/responses.yaml"
    mkdir -p "${WORKDIR}/project"
    cd "${WORKDIR}/project"
}

teardown() {
    rm -rf "${WORKDIR}"
}

@test "22.01 a call the policy refuses is recorded as denied by policy, under its call id" {
    run timeout 120 "${BIN}" --headless --model mock --mode plan \
        --output-trace "${WORKDIR}/trace.json" "please write the permission probe file" </dev/null
    [ "${status}" -eq 0 ]
    [ ! -e "${WORKDIR}/gate_probe.txt" ]
    grep -q '"schema": "sven.gate.v1"' "${WORKDIR}/trace.json"
    grep -q '"call_id": "tc-write-probe"' "${WORKDIR}/trace.json"
    grep -q '"decision": "denied"' "${WORKDIR}/trace.json"
    grep -q '"decided_by": "policy"' "${WORKDIR}/trace.json"
    grep -q '"policy": "state_allow_set"' "${WORKDIR}/trace.json"
}

@test "22.02 a call with arguments that break the tool's schema records a typed violation" {
    run timeout 120 "${BIN}" --headless --model mock \
        --output-trace "${WORKDIR}/trace.json" "read without a path" </dev/null
    # The run completes, with the tool error reported as exit 3.
    [ "${status}" -eq 3 ]
    grep -q '"schema": "sven.gate.v1"' "${WORKDIR}/trace.json"
    grep -q '"call_id": "tc-bad-read"' "${WORKDIR}/trace.json"
    grep -q '"rule": "required"' "${WORKDIR}/trace.json"
}

@test "22.03 a run in which no call is gated records no gate record" {
    run timeout 120 "${BIN}" --headless --model mock \
        --output-trace "${WORKDIR}/trace.json" "ping" </dev/null
    [ "${status}" -eq 0 ]
    ! grep -q 'sven.gate' "${WORKDIR}/trace.json"
}
