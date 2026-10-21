<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Harness samples

Eleven programmable harnesses, each a data directory: a workflow document
(`api_version: "2"`) and its README, with nothing to compile. The table lists
them from the simplest to the most involved. They are judged by running them:
`sven harness validate` checks that each one compiles (the repository's
`harness_cli` test does so for all of them), and `sven harness run` drives one
with a real model on a workspace you name. The engine's behaviour (gates,
repair, loops, resume, replay) is tested in `sven-harness-spec`,
`sven-harness-runtime` and `sven-sdk` with scripted models.

| Sample | What it shows |
|---|---|
| [ticket-routing](ticket-routing/) | schema-constrained model-only tasks and a `when` condition |
| [scoped-agent](scoped-agent/) | an imported workflow (`use:`), and an agent held to its tool list |
| [repair-loop](repair-loop/) | an independent file check after a tool, with one bounded repair |
| [repo-repair](repo-repair/) | the real tools on a real repository: a soft baseline gate, a diagnosis, an edit, a gate that decides, one repair |
| [suspend-resume](suspend-resume/) | suspend on a bound, resume the checkpoint, replay a recording offline |
| [module-sweep](module-sweep/) | a workflow that loops: a frozen backlog worked item by item with progress that survives a resume, and an `until` on a measured coverage figure |
| [embedded-hardening](embedded-hardening/) | the whole pipeline on a disposable embedded C repository: bootstrap, plan, checkpointed modules, coverage `until`, architecture, production; gcov, clang-tidy, a pinned check |
| [reviewed-change](reviewed-change/) | a package: a workflow that imports another and `use`s it as a task, with the call checked against its interface and its limits shared |
| [planner-worker](planner-worker/) | three roles: a planner answering in a schema, a worker with tools, a supervisor checked by a gate; each decoded its own way, in conversations of their own |
| [verified-task](verified-task/) | a task, a command gate whose exit status decides, bounded repair, a success condition with a coverage threshold |
| [rust-refactor](rust-refactor/) | a reusable sweep over any Rust repository: an exploration agent publishes the context and the project's own check commands as variables, then per file an auditor, a fixer and a reviewer each in a fresh conversation, the executor commits after trusted gates, and a failed file is rolled back |

## Layout of a sample

```text
name/
  harness.yaml     the main document
  lib/*.yaml       workflows it imports (`imports:`), when a sample has any
  fixtures/        input repositories for a live run, when a sample has any
  README.md
```

## Running them

```sh
sven harness validate samples/harnesses/*/
sven harness run samples/harnesses/<name> --input '{...}' --workspace DIR \
    --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 --approvals auto
```

Each sample's README says what its inputs are and what `DIR` must hold.
`embedded-hardening/repo/` and `repo-repair/fixtures/repo/` are disposable
repositories to point `--workspace` at (copy them first).
