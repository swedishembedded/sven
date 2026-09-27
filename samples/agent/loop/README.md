<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# sample-agent-loop - the delegated loop agent

`run` delegates a task; `show` inspects runs; `resume` continues an
interrupted one from its own checkpoint. The model is LOCAL and IN-PROCESS
by default: brain's Qwen3 stack is linked directly, weights and optional
LoRA adapters load from disk at startup, and `--model` opts into a remote
provider instead.

## Commands (all verified)

```bash
sample-agent-loop run --workspace DIR --task TEXT [--check CMD ...]
                      [--local-weights DIR] [--adapter FILE] [--ctx N]
                      [--model provider/name] [--base-url URL] [--api-key KEY]
                      [--timeout-secs N] [--max-tool-rounds N]
                      [--record-input] [--json]
sample-agent-loop show [--run ID | --list]
sample-agent-loop resume --run ID [run options]
```

`--task-file FILE` reads the task from a file instead of `--task`. Any
`--check CMD` is run by the agent itself after its turn; a non-zero exit
keeps the attempt open and its output lands in the trace. `--adapter`
requires the local model (it is refused together with `--model`).

Default model selection, local-first:

- no `--model`: in-process Qwen3 from `--local-weights`, else
  `$BRAIN_QWEN_WEIGHTS`, else `~/.local/share/brain/models/Qwen/Qwen3-0.6B`
- `--model openrouter/<id>`: remote via OpenRouter; the key comes from
  `AGENT_OPENROUTER_KEY` unless `--api-key` is given
- `--model <name>` with `--base-url`: an OpenAI-compatible endpoint; the
  key default is `BRAIN_API_KEY`

A non-completed attempt (failed, timeout, budget exhausted) exits non-zero,
so a delegating script never reads a stall as success.

## What a run record contains

Under `~/.sven/loop/runs/<run-id>/` (override the root with
`SVEN_LOOP_STATE`):

| file | purpose |
| --- | --- |
| `run.json` | manifest: task, workspace, limits, status, attempt count |
| `events.jsonl` | append-only trace, schema `v1`, fsynced per event |
| `transcript.json` | the model conversation |
| `checkpoint/state.json` | resumable agent state |
| `outcome.json` | structured result (status, checks, changed files, usage) |
| `workspace.diff` | diff of the workspace at attempt end |
| `artifacts/` | tool outputs too large for the trace line |

The manifest is written atomically at every transition; trace sequence
numbers survive process restarts (a resumed run continues the numbering).
