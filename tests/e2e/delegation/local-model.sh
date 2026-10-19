#!/usr/bin/env bash
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# Swedish Embedded AB implements delegation of coding work to any
# OpenAI-compatible model server for its clients. If your team needs expertise
# in running sub-agents on a model of your choice then you can procure our
# services by sending an email to info@swedishembedded.com.
#
# A real delegated coding task. A scripted parent agent delegates, through the
# `task` tool, the repair of a failing test to a sub-agent (`sven acp serve`)
# that runs on a model server you already run: a DeepSeek-compatible endpoint,
# or brain. The repository is the repo-repair sample's fixture. The result is
# judged by the repository's own tests after the run, never by what either
# agent says.
#
# Usage: local-model.sh --endpoint URL --model NAME [--driver ID] [--key-file FILE]
#                       [--timeout SECS] [--sven PATH] [--keep DIR] [--no-thinking]
#
#   --endpoint  base URL of the OpenAI-compatible server (e.g. http://127.0.0.1:8788/v1)
#   --model     the model name the server serves
#   --driver    sven's driver id for the endpoint's protocol (default: deepseek)
#   --key-file  a file holding just the API key, when the server needs one
#   --timeout   seconds the delegated run may take (default 1800)
#   --sven      the sven binary (default: target/{release,debug}/sven, newest)
#   --no-thinking  ask the server not to let the model reason before answering
#               (chat_template_kwargs.enable_thinking=false; for Qwen3 behind brain)
#   --keep      copy the run's trace, outputs and final tree into this new directory
#               (the temporary directory holds the key and is always removed)
#
# Nothing is started, pulled or configured outside a temporary directory; the
# key is written only into that directory's configuration and removed with it.
# Exit 0: the tests pass after the run. 1: they do not. 2: usage.
set -u

root="$(cd "$(dirname "$0")/../../.." && pwd)"
driver=deepseek
timeout_secs=1800
endpoint="" model="" key_file="" bin="" keep="" no_thinking=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --endpoint) endpoint=${2:?}; shift 2 ;;
        --model) model=${2:?}; shift 2 ;;
        --driver) driver=${2:?}; shift 2 ;;
        --key-file) key_file=${2:?}; shift 2 ;;
        --timeout) timeout_secs=${2:?}; shift 2 ;;
        --sven) bin=${2:?}; shift 2 ;;
        --keep) keep=${2:?}; shift 2 ;;
        --no-thinking) no_thinking=1; shift ;;
        *) echo "local-model.sh: unknown argument: $1" >&2; exit 2 ;;
    esac
done
[ -n "$endpoint" ] && [ -n "$model" ] || {
    echo "local-model.sh: --endpoint and --model are required" >&2
    exit 2
}
if [ -z "$bin" ]; then
    target="${CARGO_TARGET_DIR:-$root/target}"
    for candidate in "$target/release/sven" "$target/debug/sven"; do
        if [ -x "$candidate" ] && { [ -z "$bin" ] || [ "$candidate" -nt "$bin" ]; }; then
            bin=$candidate
        fi
    done
fi
[ -x "$bin" ] || { echo "local-model.sh: no sven binary; build one or pass --sven" >&2; exit 2; }
key=""
if [ -n "$key_file" ]; then
    key=$(tr -d '[:space:]' < "$key_file") || exit 2
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export HOME="$work/home"
mkdir -p "$HOME/.config/sven"
project="$work/project"
cp -R "$root/samples/harnesses/repo-repair/fixtures/repo" "$project"

instruction="The tests of this repository fail. Run 'sh tests/test_total.sh', find the defect in src/total.sh (the total must include the shipping argument) and fix it with the edit tools. Then run the tests again and stop."
{
    echo "responses:"
    echo "  - match_type: contains"
    echo "    pattern: \"fix the failing test\""
    echo "    tool_calls:"
    echo "      - id: tc-delegate"
    echo "        tool: task"
    echo "        args:"
    echo "          prompt: \"$instruction\""
    echo "          description: \"repair the failing test\""
    echo "          mode: agent"
    echo "          model: compat/$model"
    echo "    after_tool_reply: \"delegation finished\""
    echo "  - match_type: default"
    echo "    reply: \"unscripted\""
} > "$work/parent.yaml"
{
    echo "providers:"
    echo "  scripted:"
    echo "    name: mock"
    echo "    mock_responses_file: $work/parent.yaml"
    echo "  compat:"
    echo "    name: $driver"
    echo "    base_url: $endpoint"
    [ -n "$key" ] && echo "    api_key: $key"
    [ "$no_thinking" = 1 ] && echo "    driver_options: {chat_template_kwargs: {enable_thinking: false}}"
    echo "    models:"
    echo "      \"$model\": {}"
    echo "tools:"
    echo "  timeout_secs: $((timeout_secs + 60))"
    echo "model:"
    echo "  provider: scripted"
    echo "  name: scripted-model"
} > "$HOME/.config/sven/config.yaml"

if (cd "$project" && sh tests/test_total.sh > /dev/null 2>&1); then
    echo "local-model.sh: the fixture's tests already pass" >&2
    exit 2
fi
echo "tests before: fail (as intended)"

started=$(date +%s)
(cd "$project" && timeout "$timeout_secs" "$bin" --headless --output-trace "$work/trace.json" \
    "fix the failing test" < /dev/null > "$work/parent.out" 2> "$work/parent.err")
status=$?
wall=$(( $(date +%s) - started ))

if [ -n "$keep" ]; then
    mkdir -p "$keep" || exit 2
    cp "$work"/trace.json "$work"/parent.out "$work"/parent.err "$keep"/ 2> /dev/null
    cp -R "$project" "$keep/tree"
fi

python3 - "$work/trace.json" <<'PY'
import json, sys
try:
    doc = json.load(open(sys.argv[1]))
except (OSError, ValueError):
    print("trace: none written")
    sys.exit(0)
children = doc.get("subagent_trajectories") or []
print(f"sub-agent trajectories: {len(children)}")
for child in children:
    steps = child.get("steps") or []
    calls = sum(len(s.get("tool_calls") or []) for s in steps)
    metrics = child.get("final_metrics") or {}
    print(f"  steps {len(steps)}, tool calls {calls}, prompt tokens {metrics.get('total_prompt_tokens')}, completion tokens {metrics.get('total_completion_tokens')}, model {(child.get('agent') or {}).get('model_name')}")
PY
echo "sven exit $status after ${wall}s"

if (cd "$project" && sh tests/test_total.sh > "$work/tests.out" 2>&1); then
    echo "tests after: pass"
    exit 0
fi
echo "tests after: fail"
tail -n 5 "$work/tests.out"
exit 1
