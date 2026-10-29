<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# harness-performance

Scope: Making sven a fast, efficient, precise and robust coding harness for any model, and proving it with measured runs.

**Status: in flight. Findings below are measured; the plan is ordered.**

## What is optimised

Not the system prompt or any one tool, but the whole model-facing context and
the behaviour it produces, judged by tasks solved under a fixed protocol.

| Property | Metric |
|---|---|
| Effective | tasks passed on held-out splits, several samples per task |
| Fast | wall time per solved task, steps to first edit, timeouts as a share of failures |
| Efficient | output tokens (reasoning included) and requests per solved task, uncached prompt tokens |
| Precise | edits outside the intended place, stray files, final diff size against the reference fix |
| Robust | runs ended by the harness rather than the task (watchdogs, malformed calls, stalls); results across model families |

## Measured so far

One locally served 27B model (Qwen3.8 27B, GGUF, through brain's
OpenAI-compatible surface), headless runs with a run timeout, repair tasks from
SWE-bench Verified (12 dev), Terminal-Bench 2 hard (6 dev) and embedded firmware
tasks (20 train), one sample per task unless noted. Small samples: read close
results as noise.

Changes that helped (landed):

| Change | Evidence |
|---|---|
| Reasoning-loop detector requires a block repeated back to back | the old rule aborted 5 of 12 SWE runs on honest reasoning (code quoted again to compare); SWE dev 6 -> 9 of 12, the three gains were the aborted tasks |
| A loop is asked again once instead of ending the run | no aborted run since |
| Short, tool-agnostic core prompt (no instructions for tools a session lacks) | embedded 6 -> 9 of 20, paired 4 won 1 lost; the old prompt sent the model to `memory`, `gdb`, `context`, `task` it did not have |
| `edit_file` accepts a diff without `@@` and unprefixed context | edit failures 16% -> 9% |
| `edit_file` refuses a hunk matching several places without a line number | it edited the first match silently |
| `find_file` deferred | 7 calls in 64 runs against 220 searches by `grep`/`shell`; about 230 tokens per request |

No measurable effect:

| Change | Evidence |
|---|---|
| Thinking budget (unbounded / 8k / 2k) | SWE 9 / 9 / 8 of 12, Terminal-Bench 0 / 1 / 1 of 6 |
| `old_text`/`new_text` edit interface instead of diff hunks | edit failures 19% -> 0%, yet tasks passed 9 of 15 -> 5 of 13 (all losses timeouts); not adopted - tool-call success is not task success |
| Tool-definition tokens | about 1.7k of a ~25k-token request, in the cached prefix: trimming them barely moves cost |
| Agent scaffold, once harness defects are gone | sven, a bash-only agent and two other coding agents on the same model and tasks: 6-7 of 12 each |

What limits success now:

- Nearly every failure is the run timeout, not a wrong fix. Reasoning is ~83%
  of output tokens; decode throughput is the bottleneck, so output tokens and
  steps are the cost, prompt tokens are cheap (prefix cache).
- sven over-explores: 21 tool calls before the first edit where another agent
  needed 6; one SWE run read code for its whole 58 minutes and never edited
  the source.

## Protocol

- Dev splits for tuning; eval splits only to confirm.
- Arms run concurrently on the same server (load is a confounder); at least 3
  samples per task; paired wins and losses per task with a sign test.
- Fast inner loop: replay a recorded request at a failure point with one
  section or tool description swapped, many samples
  (`docs/guides/request-replay.md`); confirm a winner on tasks.
- Gates: CI pins prompt bytes (`crates/turn/tests/system_prompt_golden.rs`),
  request cost and cache-prefix stability, zero harness aborts on scripted
  runs; a small real-model suite before a release must not lose to the
  previous release on paired tasks.
- External reference: the same tasks run by other coding agents on the same
  model, per release. A gain on one model family is not a harness gain until a
  second family confirms it.

## Plan (in order)

1. Exploration discipline in the core prompt (A/B running) and a time-budget
   notice at the end of each request, which keeps the cached prefix
   byte-identical (implemented, A/B next). Land what wins.
2. Outcome classes for every run (solved, wrong, timeout, harness abort,
   server error) and a zero-harness-abort gate.
3. One-call navigation (definition, callers, file outline) and project facts
   found by a script (test runner, single-test command, build), each A/B'd.
4. Multi-sample runs on the eval splits against the reference agents, then a
   second model family.
