<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# samples/agent - the loop agent

A delegated coding agent that runs sven's engine with a model it controls,
traces every observable action, and hands back a structured, independently
checkable result. Its distinguishing property is the model: the agent's
inference is LOCAL and IN-PROCESS - brain's Qwen3 stack is linked directly
into the sample, weights and adapters load from disk at startup, and no
separately running, separately versioned serving process is involved. A
rebuild of this sample runs the exact brain code it was built against.

Remote providers (OpenRouter and the other configured ones) are the
development bridge: opt in per run with `--model`. The default is local.

## Why this is a separate cargo workspace

Same reason as [`samples/learning/`](../README.md): these samples link both
halves - sven's SDK facade to run the agent, and brain's crates to serve the
model and train on verified experience. sven itself must never depend on
brain; `scripts/gates/check-no-brain-dependency.sh` enforces that, and a
shared workspace would defeat it by putting brain into sven's own
`Cargo.lock`. `make samples/agent/check` is the compensating control; it
skips with a stated reason when brain is not present.

## Build and check

```bash
make samples/agent/check        # fmt + clippy(-D warnings) + tests (needs brain)
make samples/agent/build        # release binaries
```

## Running the loop agent

```bash
# delegate a task to the agent in a target workspace (LOCAL model, default):
target/release/sample-agent-loop run \
    --workspace /path/to/target-repo \
    --task "Fix the failing test in tests/run.sh" \
    --check "sh tests/run.sh"

# with a locally trained adapter:
target/release/sample-agent-loop run --workspace DIR --task TEXT \
    --adapter /path/to/adapter.safetensors

# a remote model instead (development bridge; key from AGENT_OPENROUTER_KEY):
AGENT_OPENROUTER_KEY=... target/release/sample-agent-loop run \
    --workspace DIR --task TEXT --model openrouter/<model-id>

# inspect runs; resume an interrupted one:
target/release/sample-agent-loop show --list
target/release/sample-agent-loop show --run <run-id>
target/release/sample-agent-loop resume --run <run-id> --workspace DIR
```

Every run writes its complete record under `~/.sven/loop/runs/<run-id>/`:
`run.json` (manifest), `events.jsonl` (the append-only trace), 
`transcript.json`, `checkpoint/state.json` (a resumable agent state),
`outcome.json` (the structured result) and `artifacts/`. All of it survives
the process that wrote it.
