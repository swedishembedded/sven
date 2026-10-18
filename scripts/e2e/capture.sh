#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements agent trajectory capture for its clients. If
# your team needs expertise in turning agent runs into training data then you
# can procure our services by sending an email to info@swedishembedded.com.

# A real model server (brain serve, Qwen3-0.6B) answers an agent run made with
# sampling and sampled-token capture on; the recorded ATIF trajectory must
# equal what the server said, byte for byte where it is a list of ids or
# log-probabilities: prompt token ids, sampled token ids, per-token
# log-probabilities, the answering model and the seed the server reports. A
# recording proxy between the two keeps the server's own words to compare with.
#
# Exit status: 0 passed; 1 failed (every mismatch is printed); 3 a prerequisite
# is missing, so nothing was tested. A 3 is never a pass.
#
# Usage: scripts/e2e/capture.sh <brain-binary> <capture_run-binary> <models-dir> [<device> [<keep-trajectory-file>]]
set -uo pipefail

if [ "$#" -lt 3 ] || [ "$#" -gt 5 ]; then
    echo "usage: $0 <brain-binary> <capture_run-binary> <models-dir> [<device> [<keep-trajectory-file>]]" >&2
    exit 2
fi
brain=$1
runner=$2
models=$3
device=${4:-cpu}
keep=${5:-}
model=Qwen/Qwen3-0.6B
seed=424242
here=$(cd "$(dirname "$0")" && pwd)

skip() { echo "SKIPPED (nothing was tested): $*" >&2; exit 3; }
fail() { echo "FAILED: $*" >&2; exit 1; }

[ -x "$brain" ] || skip "no brain binary at $brain"
[ -x "$runner" ] || skip "no capture_run binary at $runner"
"$brain" doctor --models-dir "$models" --model "$model" >/dev/null 2>&1 \
    || skip "brain doctor says $model cannot be served from $models"

scratch=$(mktemp -d)
pids=()
cleanup() {
    for p in "${pids[@]}"; do kill "$p" 2>/dev/null; done
    rm -rf "$scratch"
}
trap cleanup EXIT

port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
backend=()
case "$device" in gpu*) backend=(--backend cuda) ;; esac
"$brain" serve --device "$device" "${backend[@]}" --openai "$port" --models-dir "$models" \
    --api-keys-out "$scratch/keys.json" --ready-file "$scratch/ready" >"$scratch/brain.log" 2>&1 &
pids+=($!)
for _ in $(seq 1 240); do
    [ -e "$scratch/ready" ] && break
    kill -0 "${pids[0]}" 2>/dev/null || { tail -20 "$scratch/brain.log" >&2; fail "brain serve exited before it was ready"; }
    sleep 5
done
[ -e "$scratch/ready" ] || { tail -20 "$scratch/brain.log" >&2; fail "brain serve was not ready after 20 minutes"; }
python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["openai"])' "$scratch/keys.json" >"$scratch/key" \
    || fail "no api key"

python3 "$here/capture_proxy.py" --upstream "http://127.0.0.1:$port" --log "$scratch/proxy.jsonl" \
    --ready-file "$scratch/proxy-ready" &
pids+=($!)
for _ in $(seq 1 50); do [ -s "$scratch/proxy-ready" ] && break; sleep 0.2; done
proxy_port=$(cat "$scratch/proxy-ready") || fail "the recording proxy did not start"

served=$(curl -fsS -H "Authorization: Bearer $(cat "$scratch/key")" "http://127.0.0.1:$port/v1/models" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"][0]["id"])') || fail "brain lists no model"

"$runner" --endpoint "http://127.0.0.1:$proxy_port/v1" --model "$served" --api-key-file "$scratch/key" \
    --seed "$seed" --temperature 0.7 --top-p 0.9 --top-k 40 --out "$scratch/trajectory.json" \
    "Reply with one short sentence about the sea." "Now one about the sky." \
    >"$scratch/run.out" 2>"$scratch/run.err" || { tail -20 "$scratch/run.err" >&2; fail "the agent run failed"; }

python3 "$here/capture_check.py" "$scratch/trajectory.json" "$scratch/proxy.jsonl" "$seed" "$served" || exit 1
echo "PASSED: the ATIF trajectory of a run through $served equals the server's account"
[ -n "$keep" ] && cp "$scratch/trajectory.json" "$keep"
exit 0
