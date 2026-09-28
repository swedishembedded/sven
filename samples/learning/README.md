<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# samples/learning - a small model learning from its own verified experience

Each sample here is a controlled experiment, not a demonstration. It ships a
frozen task catalog, an information boundary the agent cannot read around, a
verifier the agent cannot reach, and it prints a before/after table you can
reproduce with one command.

The question they exist to answer is narrow and checkable:

> does a sub-4B model, having failed a task, learn from its own verified
> experience and then succeed on a task it has never seen?

Each sample attacks that from a different angle - what is being learned, and
from which evidence - so a null result in one does not hide a real result in
another.

## Running one

```bash
make samples/learning/check            # build + test the harness
make samples/learning/build            # release binaries
```

Every sample takes the same four subcommands, so one habit works across all of
them:

| subcommand | what it does | needs a model? |
|---|---|---|
| `audit` | offline validation of the task catalog | no - no model, no GPU, no network |
| `baseline` | the *before* numbers | yes |
| `learn` | collect verified experience, derive a dataset, train, gate, promote | yes |
| `report` | the before/after table | no |

`audit` passing is the gate that lets the rest mean anything: it is what
establishes that the tasks are solvable, that they are not solvable by reading
alone, and that the verifier rejects the ways of appearing to have solved them.

## Pointing at a model

Two things must be right, and getting either wrong produces a convincing null
result rather than an error:

1. **The server must hold a resident.** brain's adapter watcher only exists
   when the process serves a Qwen3 resident, so `BRAIN_QWEN_WEIGHTS` has to
   name the checkpoint. Without it `--watch-adapters` is accepted, logs
   nothing, serves requests normally and never swaps anything in.
2. **The model id must be the resident's.** `brain/qwen3` (the manifest id)
   and `Qwen/Qwen3-0.6B` (the model-store spelling) are two serving paths to
   the same checkpoint, and **only the resident receives adapters**. The store
   spelling is served by the base weights forever.

```bash
export BRAIN_QWEN_WEIGHTS=~/.local/share/brain/models/Qwen/Qwen3-0.6B
brain serve --openai 8791 --watch-adapters ./adapters --ready-file ./ready
```

The harness derives both spellings from one `ServedModel` (`lab/src/model_id.rs`)
rather than leaving them to each sample, and every arm records the model it
actually measured so a number cannot be attributed to the wrong weights.

## Open findings, for whoever picks this up

Two things were measured here that are not defects in these samples but will
be met again by anything built on them.

**An SDK application gets no tracing.** `RUST_LOG` has no effect on a sample:
the subscriber is installed by the `sven` binary, not by the SDK. Diagnosing a
kernel stall from inside an embedding application therefore means decoding
`.sven/audit.jsonl` by hand, which is how the stall below was characterised.

**The agent loop stalls after two scripted rounds.** Driving the agent with a
scripted model at the wire, the kernel records `TOOL <name> Started` and then
nothing - no result, no further request - on the third round. It is not the
tools (each hung command runs instantly via `sven tool call`), not the scripted
server (a unit test drives it through six consecutive requests), and not step
alignment (a separate bug, fixed). The same loop reaches eleven tool calls
driven by a real model, so the difference is in what a real provider's stream
carries and this one omits - a `usage` chunk, or arguments delivered across
several deltas rather than one.

Neither is on the path to a result: demonstrations are performed through
`sven tool call`, and the measured arms use the real model, where the loop
works. Both are recorded rather than worked around silently.
