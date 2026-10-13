#!/usr/bin/env bash
# Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# Token-ledger baseline: replays fixed scenarios headless against the `mock`
# model provider and writes each run's ledger summary, plus a report naming
# the exact sven commit, to a directory outside the repository.
#
# Swedish Embedded AB implements solutions for measuring and cutting the token
# cost of LLM agents for its clients. If your team needs expertise in
# agent context budgets then you can procure our services by sending an email
# to info@swedishembedded.com.
#
# Usage: benchmarks/ledger/run.sh [--out DIR] [--bin PATH]
#   --out DIR   where results go (default: $TMPDIR/sven-ledger-bench or /tmp/...)
#   --bin PATH  the sven binary to measure (default: target/release/sven, else
#               target/debug/sven)
#
# Scenarios (one trajectory, summary table and summary JSON each):
#   minimal      a single prompt, default tool set: the fixed cost of a request
#   tool_heavy   four user turns that fan out into reads, searches, a write
#   noisy_output one shell command that prints 3000 log lines
#   long_history a 60-turn conversation replayed ahead of one more question

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "${here}/../.." && pwd)"
out="${TMPDIR:-/tmp}/sven-ledger-bench"
bin=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --out) out="$2"; shift 2 ;;
        --bin) bin="$2"; shift 2 ;;
        -h|--help) sed -n '2,/^set -e/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

if [[ -z "${bin}" ]]; then
    if [[ -x "${repo}/target/release/sven" ]]; then
        bin="${repo}/target/release/sven"
    else
        bin="${repo}/target/debug/sven"
    fi
fi
[[ -x "${bin}" ]] || { echo "sven binary not found: ${bin} (run 'make build')" >&2; exit 1; }

case "${out}" in
    "${repo}"|"${repo}"/*) echo "--out must be outside the repository: ${out}" >&2; exit 2 ;;
esac

commit="$(git -C "${repo}" rev-parse HEAD)"
dirty=false
[[ -z "$(git -C "${repo}" status --porcelain --untracked-files=no)" ]] || dirty=true

# Results replace those of an earlier run, but never an unrelated directory.
marker="${out}/.sven-ledger-bench"
if [[ -e "${out}" && ! -e "${marker}" && -n "$(ls -A "${out}" 2>/dev/null)" ]]; then
    echo "refusing to overwrite ${out}: not a previous ledger-bench result" >&2
    exit 2
fi
rm -rf "${out}"
mkdir -p "${out}"
touch "${marker}"

export SVEN_MOCK_RESPONSES="${here}/mock_responses.yaml"
export OPENAI_API_KEY=bench-dummy-key
export ANTHROPIC_API_KEY=bench-dummy-key

# Skills, agents, knowledge and configuration are discovered from the user's
# home, and each adds to the system prompt and the tool schemas. An empty home
# keeps the baseline about sven itself rather than about whoever runs it.
export HOME="${out}/home"
export XDG_CONFIG_HOME="${HOME}/.config"
export XDG_DATA_HOME="${HOME}/.local/share"
mkdir -p "${HOME}"

# A deterministic scratch tree the scenarios read, search and write.
make_workspace() {
    local dir="$1"
    mkdir -p "${dir}/src" "${dir}/docs"
    for module in a b; do
        for i in $(seq 1 120); do
            echo "module ${module} line ${i}: marker ${i} for the ledger baseline"
        done >"${dir}/src/module_${module}.txt"
    done
    echo "baseline docs" >"${dir}/docs/readme.txt"
}

# A fixed N-turn conversation whose last turn is the question to answer.
make_history() {
    local file="$1" turns="$2"
    {
        echo "# Long history"
        for i in $(seq 1 "${turns}"); do
            printf '\n## User\nQuestion %d: how does component %d handle retries after a timeout?\n' "${i}" "${i}"
            printf '\n## Sven\nComponent %d retries a failed call three times with exponential backoff, ' "${i}"
            printf 'starting at 200 ms and capping at 5 s, then reports the failure to its caller.\n'
        done
        printf '\n## User\nledger-bench long history: what did we conclude about retries?\n'
    } >"${file}"
}

# run_scenario NAME STDIN_FILE [SVEN_ARG...]: STDIN_FILE is /dev/null unless the
# scenario pipes a conversation in.
run_scenario() {
    local name="$1" input="$2"
    shift 2
    local work="${out}/work/${name}"
    make_workspace "${work}"
    (cd "${work}" && "${bin}" --headless --model mock --token-ledger \
        --output-trace "${out}/${name}.trajectory.json" "$@" \
        <"${input}" >"${out}/${name}.stdout" 2>"${out}/${name}.stderr")
    "${bin}" ledger "${out}/${name}.trajectory.json" --json >"${out}/${name}.summary.json"
    "${bin}" ledger "${out}/${name}.trajectory.json" >"${out}/${name}.summary.txt"
}

run_scenario minimal /dev/null "ledger-bench minimal"
run_scenario tool_heavy /dev/null --file "${here}/tool_heavy.md"
run_scenario noisy_output /dev/null "ledger-bench noisy"
make_history "${out}/long_history.conversation.md" 60
run_scenario long_history "${out}/long_history.conversation.md"

python3 - "${out}" "${commit}" "${dirty}" "${bin}" <<'PY'
import json, sys, pathlib
out, commit, dirty, binary = sys.argv[1:5]
out = pathlib.Path(out)
scenarios = {}
for path in sorted(out.glob("*.summary.json")):
    scenarios[path.name.removesuffix(".summary.json")] = json.loads(path.read_text())
report = {
    "sven_commit": commit,
    "working_tree_dirty": dirty == "true",
    "binary": binary,
    "provider": "mock",
    "scenarios": scenarios,
}
(out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
lines = [
    f"sven commit: {commit}" + (" (working tree had uncommitted changes)" if dirty == "true" else ""),
    "provider: mock (its usage numbers are synthetic, so reported/residual are in the JSON but not compared here)",
    "",
    f"{'scenario':<14}{'requests':>9}{'estimated':>11}{'tool calls':>12}{'duplicates':>12}",
]
for name, s in scenarios.items():
    lines.append(
        f"{name:<14}{s['requests']:>9}{s['estimated_total']:>11}"
        f"{s['tool_calls']:>12}{s['duplicate_tool_calls']:>12}"
    )
(out / "report.txt").write_text("\n".join(lines) + "\n")
print("\n".join(lines))
PY
echo
echo "results: ${out}"
