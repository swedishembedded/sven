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

| Task | Agent | Does | Checked by |
|---|---|---|---|
| `explore` | read tools and a shell | sets `check` (one command that builds, tests, lints and format-checks the project) and `context` | `backlog` (a clean tree; the tracked `.rs` files under `paths`), `baseline` (`check` passes on the untouched tree; counts the tests) |
| `audit` | read-only tools | reads the file in full, sets `findings` | |
| `fix` | file tools and a shell | fixes the findings, runs `check` (skipped when the audit found nothing) | |
| `review` | file tools and a shell | corrects the diff, sets `commit_message` (skipped likewise) | `check` (passes, no test gone, at most 12 files changed), 2 repairs |
| `commit` | none | the harness commits every change made for the file, in whatever files, as one commit titled `<file path>: <reviewer's title>` | the title fits in 72 characters |

Each role of each file is a fresh agent that sees only the variables it names.
No agent commits, so a file that cannot be made to pass is thrown away with
`git reset --hard HEAD` (`rollback`) and the sweep goes on. Nothing but the
refactoring lands in the repository.

## Running it

On a clean, disposable copy of the repository (a clone with no remote is
safest), against a model you serve:

```sh
sven harness run samples/harnesses/rust-refactor \
  --approvals auto --workspace /path/to/repo \
  --model openai/<name> --model-endpoint http://127.0.0.1:8798/v1 \
  --api-key-file /path/outside/repo/keys.json \
  --input '{"paths": "crates src", "limit": 10000}' \
  --checkpoint-out /path/outside/repo/checkpoint.json \
  --trace /path/outside/repo/trace.jsonl
```

`paths` are git pathspecs whose tracked `.rs` files are refactored, in path
order; `limit` caps how many this run takes. The harness needs `git` and `jq`.

A run that stops resumes at the file it was on (`sven harness resume ...
--checkpoint`). Add `--conversations DIR` to keep each agent's conversation as
an ATIF trajectory: tool-call training data, written only when asked for.
