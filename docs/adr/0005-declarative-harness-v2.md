<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

Swedish Embedded AB implements verifiable, declarative agent workflows for its
clients. If your team needs expertise in turning long-running agent work into
reviewable, resumable programs then you can procure our services by sending an
email to info@swedishembedded.com.
-->

# ADR 0005: Declarative harness v2 - tasks, gates and iteration compiled onto the v1 machinery

## Status

Accepted. Implemented by `crates/harness-spec` (the `v2` module: authoring
language, validation, lowering), `crates/harness-runtime` (the `gate` and
`iterate` primitives) and `crates/tools-exec` (the trusted gate command tool).

## Context

ADR 0004 gave sven programmable harnesses: a map of steps, each one of twelve
primitives, compiled to a digest-pinned plan and run by one generic machine.
That language is a faithful program for the machine and a poor one for a
person. A real workflow ("test, simplify and harden this repository: bootstrap
the build, inventory the modules, work through each module until the build and
tests pass, close coverage gaps until a threshold is met") needs a page of
steps per task: the work turn, the verification, the failure branch, the
repair turn, the re-verification, a counter-free way to say "twice", and one
`fail` step per way to give up. Authors copy these shapes by hand, and every
copy is a chance for a task that silently advances after its verification
failed.

Three requirements are not expressible at all in v1:

1. **Success is decided by something other than the model.** A harness can run
   a command and read its exit status only through a `tool` step whose result
   is text; nothing in the language says that a non-zero status is a failure.
2. **Iteration over data the run discovers.** v1 plans are static; a
   `for_each` over the items of a JSON file an earlier task wrote cannot be
   expanded at compile time.
3. **Durable per-item progress.** A run that fails on module 61 of 100 must
   resume at module 61, not redo 1 to 60 and not skip 61.

## Decision

**Harness v2 is an authoring language that lowers to the v1 machinery.** There
is still one machine, one kernel, one effect vocabulary and one permission
plane (ADR 0004). `api_version: "2"` documents are parsed by a separate typed
AST, validated with errors written for the author, and lowered into the same
`HarnessSpec` a v1 document parses into, which then goes through the same
static validator and the same plan compiler. No second interpreter exists.

### Version dispatch

The version is read from the raw YAML before any typed deserialization.
`"1"` takes the v1 path unchanged, `"2"` the v2 path, anything else is an
error that names the supported versions. A document is never tried as one
version and retried as another: a failed v1 parse says why it failed as v1.
v1 documents, plans and digests are unchanged and a golden-digest test over
every shipped v1 document holds that (`v1_compiled_plan_unchanged`).

### The language

A v2 document declares `inputs`, `outputs`, an `agent` (profile,
instructions, tool allowlist), `execution` (limits, workspace, resume),
named `gates`, an ordered `workflow` of tasks and a `success` condition. The
authoritative description is the JSON Schema
(`crates/harness-spec/schema/harness-workflow.schema.json`) and the
[reference](../reference/harnesses.md). Every object
rejects unknown keys; every reference (gate, task, metric, input) is checked at
compile time with the name of the offending task.

* A **task** is a prompt for the agent, the artifacts it must produce, the
  gates that must pass afterwards (`verify`), and how many repair attempts a
  failed gate earns (`repair.attempts`). A task never advances while one of
  its gates is unsatisfied: it either passes, is repaired, or fails the run
  with `gate_failed`.
* A **gate** is a trusted check. Three kinds: `command` (an argv run in the
  workspace; the exit status is authoritative), `artifact` (a file exists,
  optionally with a minimum size or a content hash), `metric` (a number read
  from a JSON file a command wrote, compared with a threshold). The model can
  say it finished editing; it cannot say the tests passed.
* **Success** is a list of gates and metric thresholds re-evaluated after the
  last task, from scratch. The run completes only if they all pass.

### Lowering and the 13th primitive

Each task lowers to ordinary v1 steps: an `agent` step for the work turn,
`agent` steps for conditional follow-up turns, and for each gate attempt a
`gate` or `verify` step followed by a `branch`. Generated step ids are
`<task>__<role>...` and a source map from every generated id back to its task,
role, gate and attempt is part of the plan (and therefore of its digest), so a
trace line, a failure and any tool that reads the plan can speak in terms of
the authored document.

v1 `retry` was considered for repair and rejected: a retry frame re-runs one
body step when it fails, but a repair is "run the repair turn, then re-run the
gates", and a failed verification is a verdict, not a failed step. Repair is
instead **unrolled**: `repair.attempts: 2` produces the verification of attempt
0, a repair step and the verification of attempt 1, a repair step and the
verification of attempt 2. The bound is static, visible in the plan, checked by
the same reachability and limit rules as everything else, and needs no change to
the machine. The cost is plan size, linear in attempts times gates, bounded by
a compile-time cap of 16 attempts.

v1 `verify` can only check what the vocabulary of `VerifierSpec` expresses, and
deliberately has no way to run a process (`RunVerifier` is the one capability
that is read-only by contract). Running a command is `ExecuteShell`, so the
gate that runs one is a new primitive in the closed `StepSpec` enum, `gate`,
added through the full checklist in AGENTS.md and available **only from
`api_version: "2"`**; a v1 document that uses it is rejected with a message
saying so. A `gate` step calls the harness-only tool `shell_gate` (below),
interprets its typed result in the machine, stores it under the step's value
and continues at `then` on a pass or at `on_fail` on anything else.

### Gate execution and the trust boundary

* The gate's command is part of the compiled plan. The agent cannot edit it:
  the plan is pinned by digest, and the command is not read from the
  workspace. (A script the command *runs* lives in the workspace and the agent
  may edit it; the documentation says plainly that such a gate is only as
  strong as the script. Pin the script's content with an `artifact` gate with
  `sha256`, or keep it outside the workspace.)
* The command is an argv list. No shell is involved and no string is
  interpolated into a shell; argv elements may be templates over run inputs
  and task outputs, which are substituted as whole elements.
* `shell_gate` is a tool that no agent mode lists (`modes()` is empty), and
  the harness runtime refuses to run it for an agent step even when the model
  names it. It runs under the harness's `ExecuteShell` capability like any other
  call, so host approval policy applies to it.
* The tool returns `{exit_code, timed_out, error, stdout, stderr, duration_ms}`
  as JSON text; the machine alone decides `passed`: no error, no timeout and
  exit status 0. A missing tool, an unreadable result, a timeout, a spawn error
  and a non-zero status are all failures, with distinct `kind`s
  (`failed`, `timeout`, `exec_error`), and a gate that cannot be shown to pass
  has not passed.
* stdout and stderr are bounded (the last 16 KiB of each) and kept in the
  step's value, where repair prompts, traces and checkpoints can read them.

### Failure propagation

A delegated child that fails returns a result with `ok: false`; in v1 the
parent stores it and continues. Plans compiled from v2 mark delegate steps
`propagate_failure`, and the machine then fails the parent step (through the
step's retry frame, if any) instead of continuing: a required piece of work
cannot be failed and silently succeed
(`child_failure_does_not_succeed_parent`). The v1 default is unchanged.

### Plan format

v2 plans carry `harness-plan/2` in their digest envelope and record the
lowering's source map; v1 plans keep `harness-plan/1` and serialize exactly as
before. A checkpoint pins the plan digest; resuming against another digest is
refused as before, and a v2 checkpoint is never reinterpreted under v1 rules
because the format stamp is part of the digest.

### Dynamic iteration

`for_each` (a collection) and `until` (a metric threshold with a maximum pass
count) lower to a 14th primitive, `iterate`, available from `api_version: "2"`
and added through the same checklist. It is one primitive with two variants
sharing one engine: the body is ordinary steps whose last edge returns to the
`iterate` step, which the walk visits once per item or pass and which decides
what runs next.

* **Work-item state belongs to sven and lives in the run state**, under the step
  id: for a `for_each`, the frozen items (id, value, SHA-256 of the value,
  status, attempts, the plan digest whose checks the item passed) and a cursor;
  for an `until`, the entry count. Run state is already what a checkpoint
  carries, so resume needs no second store.
* **Snapshot rule.** The collection is read once, on the first visit, from a run
  input or through the harness-only `read_json_snapshot` tool, and frozen. Edits
  to the file afterwards are not seen: they cannot add, drop or reorder an item.
  The run does not reconcile with the file on resume; a changed backlog is a new
  run. This is the conservative choice, taken because the alternative (merging by
  id and digest) makes "completed" depend on a file the agent can write. Content
  digests of items and of the file are recorded so that a later reconciliation
  policy has what it needs.
* **Completion.** An item is completed when its body reaches the end of its
  checks. A failing item fails the run at that item (the checks' own failure
  step); later items stay pending. `success.work_items.all_completed` checks
  the loop's summary, so a loop that never ran is not completed.
* **Resume.** A resumed run continues at the item it was on. An operation the
  suspension interrupted proves nothing, so a workflow's interrupted gate, file
  check, snapshot read and model turn start again without spending a step, a
  turn or a repair attempt; completed items are not touched. (Version 1 keeps its
  declared retry policy.)
* **Bounds.** `max_items` (default 500, at most 10000) and `max_iterations` (at
  most 1000) are part of the plan; the plan's default `max_steps` is computed from
  them.

Not decided here: invalidating a completed item when the code it was verified
against changes later. A completed item records the digest of the plan it passed
under, which is the hook for it.

## Consequences

* A 40-line v2 document replaces several hundred lines of v1 steps; the plan
  and the trace remain v1-shaped, so every existing tool (checkpoints,
  recordings, replay, ATIF export, `harness test`, comparison) works on v2
  without change.
* `StepSpec` grows from twelve to fourteen primitives (`gate` and `iterate`).
  The v1 JSON Schema still names twelve and a test holds it there; the v2
  schema describes the authoring language, not steps.
* Plans of v2 workflows are larger than the document. This is deliberate and
  inspectable (`sven harness inspect` lists the generated steps), and bounded by
  the compile-time caps.
* The authoring language has no expression language beyond v1's conditions. A
  threshold on a metric is a first-class `gate`/`success` form, so no
  arithmetic is needed in the language.

## Not decided here

Reusable sub-workflows (`use:`/`imports:`), the production coding toolset and
the CLI surface (`compile`, `explain`, `inspect --graph`) are separate
milestones; this record is amended when they land. Note on resume: a gate whose
answer never arrived because the run was suspended is run again, without
spending a repair attempt, because an interrupted check proves nothing either
way.
