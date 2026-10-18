#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements agent trajectory capture for its clients. If
# your team needs expertise in turning agent runs into training data then you
# can procure our services by sending an email to info@swedishembedded.com.
"""Checks an ATIF trajectory against what the model server actually said.

usage: capture_check.py TRAJECTORY.json PROXY_LOG.jsonl SEED MODEL_ID

For every chat request the proxy saw, the matching agent step must carry the
server's own prompt token ids, sampled token ids, per-token log-probabilities,
the answering model, and the seed the server reports; the requests must have
carried the sampling the run asked for. Exit 0 when all hold; otherwise 1 and
every mismatch is printed.
"""
import json
import sys


def frames(sse):
    out = []
    for line in sse.splitlines():
        if line.startswith("data: ") and line[6:] != "[DONE]":
            out.append(json.loads(line[6:]))
    return out


def server_account(exchange):
    prompt, ids, logprobs, brain, model = [], [], [], {}, None
    for f in frames(exchange["response"]):
        model = f.get("model", model)
        prompt += f.get("prompt_token_ids") or []
        brain.update(f.get("brain") or {})
        for c in f.get("choices") or []:
            ids += c.get("token_ids") or []
            lp = c.get("logprobs") or {}
            logprobs += [t["logprob"] for t in lp.get("content") or []]
    return {"prompt": prompt, "ids": ids, "logprobs": logprobs, "brain": brain, "model": model}


def main():
    trajectory = json.load(open(sys.argv[1]))
    exchanges = [json.loads(l) for l in open(sys.argv[2]) if l.strip()]
    exchanges = [e for e in exchanges if e["path"].endswith("/chat/completions")]
    seed, model_id = int(sys.argv[3]), sys.argv[4]
    steps = [s for s in trajectory["steps"] if s.get("metrics")]
    errors = []
    if len(steps) != len(exchanges) or not steps:
        errors.append(f"{len(steps)} agent steps with metrics for {len(exchanges)} chat requests")
    for i, (step, ex) in enumerate(zip(steps, exchanges)):
        req, said = ex["request"], server_account(ex)
        m = step["metrics"]
        sven = (m.get("extra") or {}).get("sven") or {}
        if req.get("seed") != seed:
            errors.append(f"request {i}: sent seed {req.get('seed')}, asked {seed}")
        for key in ("temperature", "top_p", "top_k"):
            if key not in req:
                errors.append(f"request {i}: {key} not sent")
        if req.get("logprobs") is not True or req.get("return_token_ids") is not True:
            errors.append(f"request {i}: sampled-token capture not asked for")
        if not said["ids"] or len(said["logprobs"]) != len(said["ids"]):
            errors.append(f"request {i}: server returned {len(said['ids'])} ids / {len(said['logprobs'])} logprobs")
        if m.get("prompt_token_ids") != said["prompt"] or not said["prompt"]:
            errors.append(f"step {i}: prompt_token_ids differ from the server's")
        if m.get("completion_token_ids") != said["ids"]:
            errors.append(f"step {i}: completion_token_ids differ from the server's")
        if m.get("logprobs") != said["logprobs"]:
            errors.append(f"step {i}: logprobs differ from the server's")
        if any(lp > 0 for lp in said["logprobs"]):
            errors.append(f"step {i}: a positive log-probability")
        if step.get("model_name") != said["model"] or said["model"] != model_id:
            errors.append(f"step {i}: model {step.get('model_name')!r}, server said {said['model']!r}, listed {model_id!r}")
        if sven.get("seed") != said["brain"].get("seed"):
            errors.append(f"step {i}: seed {sven.get('seed')} but the server reports {said['brain'].get('seed')}")
        for key in ("backend", "engine"):
            if not said["brain"].get(key) or sven.get(key) != said["brain"][key]:
                errors.append(f"step {i}: {key} {sven.get(key)!r} vs the server's {said['brain'].get(key)!r}")
        # The base digest is hashed in the background after load: a reply may
        # predate it, but when the server names one the step must carry it.
        if said["brain"].get("base_digest") and sven.get("base_digest") != said["brain"]["base_digest"]:
            errors.append(f"step {i}: base_digest differs from the server's")
    for e in errors:
        print("MISMATCH:", e, file=sys.stderr)
    if errors:
        sys.exit(1)
    print(f"ok: {len(steps)} steps equal the server's account (seed {seed})")


if __name__ == "__main__":
    main()
