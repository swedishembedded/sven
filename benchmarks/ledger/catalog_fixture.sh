#!/usr/bin/env bash
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# Swedish Embedded AB implements solutions for keeping the instruction catalog
# of an LLM coding agent from taxing every request for its clients. If your
# team needs expertise in agent context budgets then you can procure our
# services by sending an email to info@swedishembedded.com.
#
# A deterministic project that carries a large instruction catalog: 40 skills,
# 12 subagents and 30 knowledge documents, plus a project instructions file.
# The same invocation always writes the same bytes, so a token-ledger figure
# measured on it can be compared between commits.
#
# Usage: catalog_fixture.sh DIR
#   DIR  the project root to populate (created if absent; the catalog is
#        written under DIR/.sven and DIR/AGENTS.md)

set -euo pipefail

[[ $# -eq 1 ]] || { echo "usage: $0 DIR" >&2; exit 2; }
root="$1"

areas=(auth billing search ingest scheduler notifications reporting storage gateway telemetry)
skill_kinds=(review debug test)
agent_roles=(architect auditor debugger migrator perf-analyst reviewer tester docs-writer release-manager triage verifier refactorer)
knowledge_kinds=(design runtime operations)

title() { local s="$1"; printf '%s%s' "$(tr '[:lower:]' '[:upper:]' <<<"${s:0:1}")" "${s:1}"; }

mkdir -p "${root}/.sven/skills" "${root}/.sven/agents" "${root}/.sven/knowledge"

# 10 parent skills and 3 sub-skills each.
for area in "${areas[@]}"; do
    dir="${root}/.sven/skills/${area}"
    mkdir -p "${dir}"
    cat >"${dir}/SKILL.md" <<EOF
---
name: ${area}
description: |
  This skill should be used when the user asks to "work on the ${area} service",
  "change ${area} behaviour", or "explain how ${area} is wired". It covers the
  ${area} module layout, its public interfaces, the invariants its callers rely
  on and the commands that build and exercise it.
---

# $(title "${area}")

Start from \`src/${area}/mod.rs\`. The module owns its state; callers go through
the interfaces in \`src/${area}/api.rs\` and never touch storage directly.

1. Read the interface before the implementation.
2. Keep state transitions pure and put I/O in the adapters.
3. Run \`make test/${area}\` after every change.
EOF
    for kind in "${skill_kinds[@]}"; do
        mkdir -p "${dir}/${kind}"
        cat >"${dir}/${kind}/SKILL.md" <<EOF
---
name: ${area}-${kind}
description: |
  This skill should be used when the user asks to "${kind} the ${area} module",
  "${kind} a ${area} change", or "prepare a ${kind} of ${area}". It lists the
  checks, files and commands for a ${kind} of the ${area} service and the
  mistakes reviewers have caught there before.
---

# $(title "${area}") ${kind}

Procedure for a ${kind} of \`src/${area}/\`:

1. Collect the diff and the tests that cover \`src/${area}/\`.
2. Compare behaviour against the interface in \`src/${area}/api.rs\`.
3. Report findings by severity, each with the file and line.
EOF
    done
done

# 12 subagents.
for role in "${agent_roles[@]}"; do
    cat >"${root}/.sven/agents/${role}.md" <<EOF
---
name: ${role}
description: Use this subagent when the task calls for a ${role}. It works on one bounded question, reads before it writes, and returns a short report with the files it relied on and what it could not verify.
readonly: false
---

You are a ${role}. Work only on the question you were given, read the code
before you form an opinion and report evidence rather than impressions.
EOF
done

# 30 knowledge documents.
for area in "${areas[@]}"; do
    for kind in "${knowledge_kinds[@]}"; do
        cat >"${root}/.sven/knowledge/${area}-${kind}.md" <<EOF
---
subsystem: $(title "${area}") ${kind}
files:
  - src/${area}/**
  - tests/${area}/**
updated: 2026-01-01
---

## $(title "${area}") ${kind}

The ${kind} of the ${area} service. Interfaces live in \`src/${area}/api.rs\`;
tests live in \`tests/${area}/\`. Invariants: state is owned by the module and
every external effect goes through an adapter.
EOF
    done
done

cat >"${root}/AGENTS.md" <<'EOF'
# Project instructions

Build with `make build`, test with `make test`. Keep functions short and put
every external effect behind an adapter. Never commit generated files.
EOF
