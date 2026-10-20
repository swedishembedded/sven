<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Harness samples

Eleven programmable harnesses, each a data directory: a YAML document, the
cases that state what it must do, and nothing to compile. The table lists
them from the simplest program to the most involved; every default case runs
offline on mock models and fixture tools.

| Sample | What it shows |
|---|---|
| [branching](branching/) | pure branching: `assign` and `branch`, no model, no capability |
| [ticket-routing](ticket-routing/) | one schema-constrained model turn, retried, then routed; its `live-local` case is the opt-in real-model run |
| [scoped-agent](scoped-agent/) | delegation to a child, and an agent held to its tool list |
| [repair-loop](repair-loop/) | retry around a tool, then an independent verification |
| [repo-repair](repo-repair/) | the real tools on a real repository: diagnose a failing test, edit, verify independently, retry once |
| [parallel-research](parallel-research/) | `parallel` branches joined in declared order, and cancellation |
| [suspend-resume](suspend-resume/) | suspend on a bound, resume the checkpoint, replay offline |
| [custom-helper](custom-helper/) | a sandboxed WASM helper carried in a bundle |
| [module-sweep](module-sweep/) | a v2 workflow that loops: a frozen backlog worked item by item with progress that survives a resume, and an `until` on a measured coverage figure |
| [embedded-hardening](embedded-hardening/) | the whole v2 pipeline on a disposable embedded C repository: bootstrap, plan, checkpointed modules, coverage `until`, architecture, production; gcov, clang-tidy, a pinned check |
| [reviewed-change](reviewed-change/) | a v2 package: a workflow that imports another and `use`s it as a task, with the call checked against its interface and its limits shared |
| [planner-worker](planner-worker/) | three roles: a planner answering in a schema, a worker with tools, a supervisor checked by a gate; each decoded its own way, in conversations of their own |
| [verified-task](verified-task/) | a v2 workflow: a task, a command gate whose exit status decides, bounded repair, a success condition with a coverage threshold |
| [sdk-runner](sdk-runner/) | the same cases, run from an application with the SDK alone |

## Layout of a sample

```text
name/
  harness.yaml     the main document (custom-helper ships bundle.yaml and document.yaml instead)
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
sven harness test samples/harnesses/{branching,ticket-routing,scoped-agent,repair-loop,repo-repair,parallel-research,suspend-resume,custom-helper,verified-task,module-sweep,planner-worker,reviewed-change,embedded-hardening}   # all samples, offline
cargo test -p sven-harness-testkit --test samples
```

Only the `live-local` case of [ticket-routing](ticket-routing/) needs a model
that is really running, and it is skipped unless you pass `--model-endpoint`.
