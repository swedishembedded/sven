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
sample-agent-loop learn --run ID
sample-agent-loop train [--dataset FILE] [--local-weights DIR]
                        [--steps N] [--rank N] [--alpha F]
sample-agent-loop explore --file FILE --out OUT.jsonl [--chunk-lines N]
                      [--model provider/name] [--local-weights DIR]
                      [--adapter FILE] [--ctx N] [--base-url URL]
                      [--api-key KEY]
sample-agent-loop ask --question TEXT [model options as for run]
sample-agent-loop eval-facts --file FILE [--adapter FILE] [--base]
                      [model options as for run]
sample-agent-loop facts --file FILE [--work-dir DIR]
                      [--holdout-one-in N] [--scope-negatives IDS]
                      [train options] [model options]
```

`explore` turns a markdown fact sheet (e.g.
`examples/stm32_datasheet.md`) into a question/answer training dataset:
the file is split at headings - or, once a section exceeds
`--chunk-lines N`, at paragraph boundaries - each section is asked for
EVERY factual claim as `{"facts": [{"question", "answer"}, ...]}`,
replies are parsed strictly (a non-conforming reply is counted as a
parse failure and its section skipped), and one JSONL record per fact is
written atomically to `--out` in the exact schema `learn` uses for the
experience pool. Facts are deduplicated by normalized question text.
Every chunk carries the document's title line, every question must name
a device the title names (the anchor gate refuses the rest - an
unanchored question would train this device's answers onto other
devices' questions), and `--scope-negatives ID1,ID2,...` adds negative
variants: the same question with an out-of-scope device substituted,
answered by a fixed abstention, so the adapter learns where its
knowledge ends instead of answering other chips with this chip's
numbers. The whole exploration is traced to its own run directory
(manifest, `events.jsonl` with one event per section, `outcome.json`
with the counts).

`ask` asks one question one-shot: the model must reply with exactly one
`{"answer": string}` JSON object, the reply is parsed strictly (optional
markdown code fences are stripped), and ONLY the parsed object is printed
to stdout. An unparseable reply exits with code 2. Like `run` and
`explore`, it traces to its own run directory.

`eval-facts` scores a question/answer file against bare model replies and
prints the exact-match rate. Questions without a trained answer shape are
useless to a wrapped training set, so the reference answers must be bare
strings or bare `{"answer": ...}` objects - a wrapped training record is
refused.

Question commands (`ask`, `eval-facts`) serve the PROMOTED adapter by
default: when no adapter is named, the pointer at
`~/.sven/loop/adapter.json` is used if it exists. `--base` asks the base
model for the contrast (a promoted adapter without it is the fastest way
to see what training bought); `--base` together with `--adapter` or
`--model` is refused as ambiguous. `run` and `explore` keep base-only
defaults: a facts adapter's reply shape leaks into a coding loop.

`facts` runs the whole document-learning pipeline as one command:

```bash
sample-agent-loop facts --file examples/stm32_datasheet.md --work-dir DIR
```

Document -> `explore` extracts every fact -> split into train and held-out
eval sets (`--holdout-one-in N`, default 5: every Nth fact is held out) ->
gated LoRA `train` on the training split (the same champion-bounded
promote/reject rule as `train` itself) -> recall scored on the trained
questions -> generalization scored on the held-out questions. Each stage
reuses its artifacts when they already exist in `--work-dir` (a second run
re-scores instead of re-exploring), and `facts-report.json` in the work
dir carries the counts and the promotion decision. Exit is non-zero when
the gate rejects the candidate, so a delegating script never reads a
rejection as success.

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

## The training gate (`learn` -> `train`)

`learn --run ID` appends one run's experience to
`~/.sven/loop/datasets/experience.jsonl` - but only a run the reviewer
could already trust: the attempt completed AND at least one completion
check passed AND the run has a final reply. Anything else is refused with
the reason. Learning the same run twice is a no-op.

`train` fine-tunes a LoRA adapter on the pool through brain's own trainer
and holds the newest record out as the held-out sample. The adapter is
promoted (a pointer written to `~/.sven/loop/adapter.json`) only when the
held-out loss strictly improved at the same weight tier on both sides of
the comparison; a rejected attempt keeps its scores on disk but no
pointer, and exits non-zero. Both scores land in the attempt's
`decision.json` either way, so a rejected adapter is evidence, not folklore.
`--adapter` serves the promoted one: it names either a LoRA safetensors file
or the promotion pointer itself (`~/.sven/loop/adapter.json`), so a serving
invocation stays valid as later trainings promote new adapters over it.
