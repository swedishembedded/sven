<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Harness samples

Eight programmable harnesses, each a data directory: a YAML document, the
cases that state what it must do, and nothing to compile. They are ordered
from the simplest program to the most involved, and every one of them runs
offline on mock models and fixture tools.

| Sample | What it shows |
|---|---|
| [01-decision](01-decision/) | pure branching: `assign` and `branch`, no model, no capability |
| [02-model-routing](02-model-routing/) | one schema-constrained model turn, retried, then routed |
| [03-tool-agent](03-tool-agent/) | delegation to a child, and an agent held to its tool list |
| [04-repair-loop](04-repair-loop/) | retry around a tool, then an independent verification |
| [05-parallel-research](05-parallel-research/) | `parallel` branches joined in declared order, and cancellation |
| [06-checkpoint-replay](06-checkpoint-replay/) | suspend on a bound, resume the checkpoint, replay offline |
| [07-custom-transform](07-custom-transform/) | a sandboxed WASM helper carried in a bundle |
| [08-local-qwen](08-local-qwen/) | a real local model, strictly opt-in, never run by default |
| [sdk-runner](sdk-runner/) | the same cases, run from an application with the SDK alone |

## Layout of a sample

```text
NN-name/
  harness.yaml     the main document (07 ships bundle.yaml and document.yaml instead)
  children/*.yaml  documents the main one delegates to, by name
  cases/*.yaml     one case per file: input, model, tools, expectations
  fixtures/        files a case refers to (model scripts), when a sample has any
  README.md
```

## Cases are specifications

A case states what the harness is *for*, not what it happened to do. It names
the input, a fixture model (`sequence` for one answer per model call in order,
`rules` for prompt-matched answers where call order is not defined), fixture
tools with the capability each honestly exercises, and the expectations: the
status, the exact output, the failure code, step counts, the tools that ran and
the ones that must not. If a run disagrees with a case, the first question is
which of the two is wrong - the expectation is never loosened to make a run
pass.

Every case is also replay-checked: the run is recorded and replayed offline,
and must reach the same verdict and trace.

## Running them

```sh
sven harness test samples/harnesses/0*/          # all samples, offline
cargo test -p sven-harness-testkit --test samples
```

Only [08-local-qwen](08-local-qwen/) needs a model that is really running, and
it is skipped unless you opt in explicitly.
