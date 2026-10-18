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
        echo "Binary not found: ${BIN} - run 'cargo build' first" >&2
        return 1
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

section_tokens() {
    grep '^\[sven:ledger\] ' <<<"${STDERR_OUT}" | head -1 \
        | sed -E "s/.*\"$1\":([0-9]+).*/\1/"
}

@test "21.04 --skill-disclosure deferred charges a fraction of the full skill listing" {
    bash "${FIXTURE_SCRIPT}" "${WORK}/project"
    cd "${WORK}/project"
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --token-ledger --skill-disclosure full'
    full="$(section_tokens Skills)"
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --token-ledger --skill-disclosure deferred'
    deferred="$(section_tokens Skills)"
    [[ "${full}" -gt 0 && "${deferred}" -gt 0 ]]
    [[ $((deferred * 3)) -lt "${full}" ]]
}

@test "21.05 --subagent-disclosure and --knowledge-disclosure shrink their own listings only" {
    bash "${FIXTURE_SCRIPT}" "${WORK}/project"
    cd "${WORK}/project"
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --token-ledger --skill-disclosure full --subagent-disclosure full --knowledge-disclosure full'
    full_agents="$(section_tokens Subagents)"
    full_knowledge="$(section_tokens 'Knowledge Base')"
    full_skills="$(section_tokens Skills)"
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --token-ledger --skill-disclosure full --subagent-disclosure deferred --knowledge-disclosure deferred'
    [[ "$(section_tokens Subagents)" -lt $((full_agents / 2)) ]]
    [[ "$(section_tokens 'Knowledge Base')" -lt "${full_knowledge}" ]]
    [[ "$(section_tokens Skills)" -eq "${full_skills}" ]]
}

@test "21.06 a skill the index only summarises still loads by its command" {
    bash "${FIXTURE_SCRIPT}" "${WORK}/project"
    cd "${WORK}/project"
    export SVEN_MOCK_RESPONSES="${_REPO_ROOT}/benchmarks/ledger/mock_responses.yaml"
    run_split_output bash -c 'echo "ledger-bench catalog skill" | "$BIN" --headless --model mock --skill-disclosure deferred'
    [[ "${STDERR_OUT}" == *'name="skill"'* ]]
    [[ "${STDERR_OUT}" == *'name="skill" success=true'* ]] || [[ "${STDERR_OUT}" == *'[sven:tool:result] id="tc-catalog-skill" name="skill" success=true'* ]]
}

@test "21.07 an unknown --skill-disclosure mode is refused" {
    run "${BIN}" --headless --model mock --skill-disclosure sometimes "ping"
    [[ "${status}" -ne 0 ]]
    [[ "${output}" == *"skill-disclosure"* ]]
}
