<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# rust-refactor

Refactor any Rust repository file by file without changing what it does. The
harness discovers how the project is built and tested, then gives every file
an auditor, a fixer and a reviewer, each a fresh agent, and commits each file's
change on the current branch only after the project's own checks pass.

Swedish Embedded AB implements agent-driven refactoring of large Rust codebases
that is judged by the build and the tests, not by the agent, for its clients.
If your team needs expertise in behaviour-preserving refactoring at scale then
you can procure our services by sending an email to info@swedishembedded.com.

## What runs

| Task | Agent | What it does | Checked by |
|---|---|---|---|
| `explore` | read-only tools and a shell | surveys the repository and publishes two variables: `context` (purpose, layout, public surface, conventions, shared helpers, hazards) and `checks` (the bash commands that build, test, lint and format-check this project) | `clean_start` (no uncommitted changes), `inventory` (captures the tracked `.rs` files under `paths` as the backlog), `baseline` (the published commands pass on the untouched tree; captures the test count) |
| `files` / `begin` | none | captures the commit the file starts from | |
| `files` / `audit` | read-only tools | reads the file in full and publishes `findings` | |
| `files` / `repair` | file tools and a shell | fixes the findings across the repository and publishes `repair_summary` (skipped when the audit found nothing) | `quick` (build, lint, format) and `scope` (at most `max_files` files changed, no junk files), 2 repairs |
| `files` / `review` | file tools and a shell | reviews the whole diff, corrects it and publishes `commit_message` (skipped when the audit found nothing) | `verify` (build, tests, lint, format, and no fewer tests than the baseline) and `scope`, 2 repairs |
| `files` / `commit` | none | the executor commits with the reviewer's message, if anything changed | the message's shape |

Each role of each file runs in a conversation of its own and sees only the
variables it names: the auditor cannot edit, no agent can change the check
commands once exploration is over (the validator's trust rule enforces it),
and no agent commits. A file whose checks stay red after its repairs is
rolled back to its base commit (`rollback`) and the sweep goes on; the run
succeeds when every file was worked (`all_settled`).

Nothing but the refactoring is written into the repository: the backlog, the
base commits and the baseline are run variables, and sven's own audit log goes
to the run's scratchpad.

## Running it

On a clean checkout (a disposable worktree is best), against a model you
serve:

```sh
sven harness run samples/harnesses/rust-refactor \
  --toolset coding --approvals auto --workspace /path/to/repo \
  --model openai/<name> --model-endpoint http://127.0.0.1:8798/v1 \
  --api-key-file /path/outside/repo/keys.json \
  --input '{"paths": "crates src", "max_files": 12, "max_items": 10000, "tool_calls": 200000}' \
  --checkpoint-out /path/outside/repo/checkpoint.json \
  --trace /path/outside/repo/trace.jsonl
```

- `paths` are git pathspecs whose tracked `.rs` files are refactored, in path
  order.
- `max_items` caps how many of them this run takes.
- `max_files` is the most files one file's refactor may touch.

A run that stops resumes at the file it was on (`sven harness resume ...
--checkpoint`). Add `--conversations DIR` to keep each agent's conversation as
an ATIF trajectory: tool-call training data, written only when asked for.

## Cases

`sven harness test samples/harnesses/rust-refactor` runs two offline cases on
a fixture model and fixture gates:

- `refactors-every-file`: one file is refactored and committed, one is clean
  and skipped after its audit.
- `a-file-that-cannot-pass-is-rolled-back`: a file whose build stays red is
  rolled back and the sweep goes on.
