<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Harness samples

Eleven programmable harnesses, each a data directory: a workflow document
(`api_version: "2"`), the cases that state what it must do, and nothing to
compile, plus the SDK runner that runs the same cases from an application. The
table lists them from the simplest to the most involved; every default case runs
offline on mock models and fixture tools.

| Sample | What it shows |
|---|---|
| [ticket-routing](ticket-routing/) | schema-constrained model-only tasks and a `when` condition; its `live-local` case is the opt-in real-model run |
| [scoped-agent](scoped-agent/) | an imported workflow (`use:`), and an agent held to its tool list |
| [repair-loop](repair-loop/) | an independent file check after a tool, with one bounded repair |
| [repo-repair](repo-repair/) | the real tools on a real repository: a soft baseline gate, a diagnosis, an edit, a gate that decides, one repair |
| [suspend-resume](suspend-resume/) | suspend on a bound, resume the checkpoint, replay offline |
| [module-sweep](module-sweep/) | a workflow that loops: a frozen backlog worked item by item with progress that survives a resume, and an `until` on a measured coverage figure |
| [embedded-hardening](embedded-hardening/) | the whole pipeline on a disposable embedded C repository: bootstrap, plan, checkpointed modules, coverage `until`, architecture, production; gcov, clang-tidy, a pinned check |
| [reviewed-change](reviewed-change/) | a package: a workflow that imports another and `use`s it as a task, with the call checked against its interface and its limits shared |
| [planner-worker](planner-worker/) | three roles: a planner answering in a schema, a worker with tools, a supervisor checked by a gate; each decoded its own way, in conversations of their own |
| [verified-task](verified-task/) | a task, a command gate whose exit status decides, bounded repair, a success condition with a coverage threshold |
| [rust-refactor](rust-refactor/) | a reusable sweep over any Rust repository: an exploration agent publishes the context and the project's own check commands as variables, then per file an auditor, a fixer and a reviewer each in a fresh conversation, the executor commits after trusted gates, and a failed file is rolled back |
| [sdk-runner](sdk-runner/) | the same cases, run from an application with the SDK alone |

## Layout of a sample

```text
name/
  harness.yaml     the main document
  lib/*.yaml       workflows it imports (`imports:`), when a sample has any
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
sven harness test samples/harnesses/{ticket-routing,scoped-agent,repair-loop,repo-repair,suspend-resume,verified-task,module-sweep,planner-worker,reviewed-change,embedded-hardening,rust-refactor}   # all samples, offline
cargo test -p sven-harness-testkit --test samples
```

Only the `live-local` case of [ticket-routing](ticket-routing/) needs a model
that is really running, and it is skipped unless you pass `--model-endpoint`.
