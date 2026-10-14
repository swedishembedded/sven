#!/usr/bin/env bats
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# Swedish Embedded AB implements solutions for keeping the instruction catalog
# of an LLM coding agent from taxing every request for its clients. If your
# team needs expertise in agent context budgets then you can procure our
# services by sending an email to info@swedishembedded.com.
#
# 21_instruction_catalog.bats - what skills, subagents and knowledge documents
# cost a request, measured on the deterministic catalog fixture.
#
# Covers:
#   • the fixture is complete (40 skills, 12 agents, 30 knowledge documents)
#     and byte-identical between two generations
#   • the ledger charges each catalog listing to its own system section

load helpers

FIXTURE_SCRIPT="${_REPO_ROOT}/benchmarks/ledger/catalog_fixture.sh"

setup() {
    if [[ ! -x "${BIN}" ]]; then
        skip "Binary not found: ${BIN} - run 'cargo build' first"
    fi
    WORK="$(mktemp -d)"
    export HOME="${WORK}/home"
    mkdir -p "${HOME}"
}

teardown() {
    rm -rf "${WORK}"
}

@test "21.01 the catalog fixture has 40 skills, 12 agents and 30 knowledge documents" {
    bash "${FIXTURE_SCRIPT}" "${WORK}/a"
    [[ "$(find "${WORK}/a/.sven/skills" -name SKILL.md | wc -l)" -eq 40 ]]
    [[ "$(find "${WORK}/a/.sven/agents" -name '*.md' | wc -l)" -eq 12 ]]
    [[ "$(find "${WORK}/a/.sven/knowledge" -name '*.md' | wc -l)" -eq 30 ]]
    [[ -f "${WORK}/a/AGENTS.md" ]]
}

@test "21.02 the catalog fixture is byte-identical between two generations" {
    bash "${FIXTURE_SCRIPT}" "${WORK}/a"
    bash "${FIXTURE_SCRIPT}" "${WORK}/b"
    run diff -r "${WORK}/a" "${WORK}/b"
    [[ "${status}" -eq 0 ]]
}

@test "21.03 the ledger charges skills, subagents and knowledge to their own sections" {
    bash "${FIXTURE_SCRIPT}" "${WORK}/project"
    cd "${WORK}/project"
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --token-ledger'
    ledger="$(grep '^\[sven:ledger\] ' <<<"${STDERR_OUT}")"
    [[ "${ledger}" =~ \"Skills\":[1-9][0-9]* ]]
    [[ "${ledger}" =~ \"Subagents\":[1-9][0-9]* ]]
    [[ "${ledger}" =~ \"Knowledge\ Base\":[1-9][0-9]* ]]
    [[ "${ledger}" =~ \"Project\ Instructions\":[1-9][0-9]* ]]
}
