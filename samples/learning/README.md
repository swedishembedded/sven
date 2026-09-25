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

## Why this is a separate cargo workspace

These samples link **both** halves of the loop directly: sven's SDK facade to
run the agent, and [brain](../../../edgeai/brain) to train the adapter and
gate its promotion. sven itself must never depend on brain -
`scripts/gates/check-no-brain-dependency.sh` enforces that, because sven has
to build, test and ship on a machine with no brain checkout - and a shared
workspace would defeat it by putting brain into sven's own `Cargo.lock`.

Two workspaces is what lets both facts hold at once. The cost is the one
[`samples/README.md`](../README.md) warns about - a sample outside the build is
a sample that rots - so `make samples/learning/check` builds and tests them,
and skips with a stated reason when brain is absent rather than failing a
clone that does not have one.

Reaching brain over its wire protocol instead would have avoided the split, at
the price of making each sample mostly a demonstration of the wire protocol.
The training half is the subject here, so it is linked.

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

## What a sample may depend on

`make check/samples` enforces it: `sven-sdk` (never anything else from sven's
workspace), `brain`, and `sample-learning-lab`. A sample that reaches past the
facade stops being evidence that the facade is sufficient, which is half of
why `samples/` exists at all.
