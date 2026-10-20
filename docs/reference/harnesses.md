<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Programmable harnesses

A harness is an agent program in YAML that compiles through the HSM kernel.
This page is the reference: the language, the runtime states, the
authorization model, checkpoints, recordings, trajectories and the SDK API.
For the commands see [Harnesses](../guides/harnesses.md); for the
reasons behind the design see [ADR 0004](../adr/0004-programmable-harnesses.md)
and, for the workflow language, [ADR 0005](../adr/0005-declarative-harness-v2.md).

| Crate | Tier | Role |
|-------|------|------|
| `sven-harness-spec` | foundation | document, typed AST, validator, compiler to a content-hashed `HarnessPlan`, JSON Schema |
| `sven-harness-transform` | foundation | the sandboxed WASM helper runtime |
| `sven-harness-runtime` | machines | `HarnessMachine`: the plan interpreted as a kernel `Machine` |
| `sven-sdk` | sdk | `Engine::load_harness`, `Harness`, `HarnessRun`, recording and replay |
| `sven-harness-testkit` | surface | case manifests, fixture models and tools, runner, comparer |

## The language

A document has these top-level keys. An unknown key is an error at every level
(document, `limits`, each primitive's body, declared inputs and outputs), named
in the message (`unknown field \`entrypoint\``, with the primitive for a step
body); the JSON Schema forbids them too. Every step must be reachable from
`entry`: an orphan step is rejected by name.

| Key | |
|-----|---|
| `api_version` | required; `"1"` is this language, `"2"` the [workflow language](#workflows-api_version-2) |
| `name` | required, non-empty; what `delegate` and `parallel` resolve |
| `description` | optional |
| `inputs`, `outputs` | optional maps of name to `string`, `number`, `boolean` or `json`; `name: string` or `name: { type: string, description: ... }` |
| `schemas` | optional map of name to JSON Schema object, used by `predict` |
| `capabilities` | optional list from `ReadFile`, `WriteFile`, `DeleteFile`, `ExecuteShell`, `NetworkAccess`, `GitOperation`, `RunVerifier`, `ControlDevice`, `SpawnChild` |
| `limits` | see below |
| `entry` | required, the first step |
| `steps` | required map of step id to step |

A **step** is a map with exactly one primitive key. The machine-readable
form is `sven harness schema`
(`crates/harness-spec/schema/harness-document.schema.json`).

| Primitive | Fields | Meaning |
|-----------|--------|---------|
| `predict` | `prompt`, `schema`?, `store_as`?, `then` | one model turn; with `schema` the answer must parse as JSON (a single markdown code fence around it is removed first) and is stored parsed; the `brain` driver is not sent `response_format`, which that server refuses |
| `agent` | `prompt`, `tools`?, `store_as`?, `then` | a model/tool loop until the model answers; `tools` is an allowlist |
| `tool` | `name`, `args`?, `store_as`?, `then` | one tool call |
| `verify` | `spec`, `store_as`?, `then` | an independent check of an artifact; the verdict is `passed`, `failed: <reason>`, `needs_human` or `unknown` |
| `assign` | `set`, `then` | store values in `facts` |
| `branch` | `on`, `when_true`, `when_false` | choose a successor |
| `retry` | `attempts`, `body`, `then`, `on_exhausted` | run `body` up to `attempts` times |
| `parallel` | `branches`, `join` | run the branch steps concurrently as child runs |
| `delegate` | `target`, `as_kind`?, `input`?, `store_as`?, `then` | run another harness (`as_kind: harness`, the default) or a registered machine mode (`machine`) |
| `transform` | `helper`, `input`?, `store_as`?, `then` | run a bundled WASM helper over a value |
| `return` | `value` | finish with a value matching `outputs` |
| `fail` | `code`?, `message`? | finish with a failure (`code` defaults to `harness_failed`) |

`verify.spec` is the kernel's verifier vocabulary: `file_exists` (`path`,
`min_bytes`?), `file_hash`, `json_predicate`, `http_predicate`, `ask_human`,
and `all`/`any` over `specs`.

### Data and expressions

Run data lives in four namespaces, addressed by dotted paths:

- `inputs.<name>` - the run's inputs;
- `facts.<name>` - what `store_as` and `assign` stored;
- `steps.<id>` - the value a step produced (`steps.<id>.value` is the same);
  `steps.<parallel id>.value.0` indexes a join's results;
- `env` is reserved and resolves to nothing.

A string containing `{{ path }}` is a template. A string that is exactly one
hole keeps the referenced value's type (`"{{ facts.discount }}"` is the
number); otherwise the result is text. A path that resolves to nothing renders
as the literal hole, visibly. A branch condition `on` is a path (truthy: not
null, not false, not an empty string, array or object; zero is truthy), `!path`,
a single `{{ path }}`, or a map:

```yaml
on: inputs.vip
on: { eq: inputs.plan, value: pro }          # ne likewise
on: { all: [inputs.vip, { not: facts.blocked }] }   # any likewise
```

There is no arithmetic or ordering comparison. Anything richer is a
`transform` helper.

### Limits

`limits` is required when any step can run away (`predict`, `agent`, `tool`,
`verify`, `retry`, `parallel`, `delegate`) and must set at least one of
`max_steps`, `max_model_turns` and `max_tool_calls`. The others are
`max_delegate_depth` (the top-level run is depth 1), `max_parallel` (a
`parallel` wider than it fails to compile), `max_helper_fuel` and
`max_helper_pages`. A bound the run exceeds fails it with `step_budget`,
`model_turn_budget`, `tool_call_budget` or `delegate_depth`. A harness with no
`max_steps` still stops after 4096 steps.

### What `validate` rejects

An unsupported `api_version`; an empty name or no steps; a missing or dangling
step reference; a step that cannot reach a `return` or `fail` (cycles are only
allowed through `retry`, which needs `attempts >= 1` and `on_exhausted`); a
primitive whose capability the harness does not declare (`verify` needs
`RunVerifier`, `delegate` and `parallel` need `SpawnChild`, an `agent` or
`tool` naming `shell*`, `run_terminal*` or `gdb*` needs `ExecuteShell`);
missing `limits`; a `return` value that does not satisfy `outputs`; a schema
that is not an object or that no `predict` names; a bad path or template; a
helper a `transform` names that the bundle lacks, or a bundle helper nothing
names. Other capabilities (`ReadFile`, `WriteFile`, ...) cannot be derived
from a tool's name in general, so declare them: the run-time gate refuses an
undeclared one.

### Failure codes

A `fail` step reports its own code. The runtime adds `invalid_answer` (a
`predict` answer that is not JSON), `side_effect_failed` (a tool call failed,
or a child could not run), `helper_*` (a helper's decode, ABI, fuel, memory,
trap or schema failure), the four bounds above, `cancelled`, and for workflows `gate_failed`, `success_check_failed` and `child_failed` (see below). A failed step inside a
`retry` frame re-enters the body while attempts remain, then continues at
`on_exhausted`; outside one it fails the run. A body that succeeds ends the
frame and the walk follows the body's own `then`. Every attempt is a
`StepStarted` with its attempt number.

### Digests

The compiler produces a canonical plan whose digest does not depend on key
order or whitespace and changes when the program's meaning does. A bundle's
digest additionally covers the helper bytes. Runs, checkpoints and
recordings pin these digests, and a mismatch is refused, never reinterpreted.

## Workflows (`api_version: "2"`)

A version 2 document describes *what a job is* instead of *which steps run*:
tasks, the trusted gates that decide whether each one succeeded, how failures
are repaired, and what counts as success. It is decoded by its own strict
parser, checked in the author's terms, and lowered into the step language above;
the lowered spec goes through the same validator and compiler, so a workflow
runs on the same machine and every tool that reads a plan, a checkpoint, a
recording or a trace works on it. The version is read from the raw document
before anything is deserialized; `"1"` and `"2"` are the supported values and a
document is never retried as the other one. The JSON Schema is
`crates/harness-spec/schema/harness-workflow.schema.json` (`WORKFLOW_SCHEMA`).

| Key | |
|-----|---|
| `name`, `description` | `name` is lowercase letters, digits, `-`, `_`; no `__`; at most 48 characters |
| `inputs` | typed per-run inputs, as in version 1 (`name: type` or `{type, description}`); templates read `{{ inputs.<name> }}` and an undeclared one is an error |
| `outputs` | `name: {type, value}`; the value is a template over inputs and task outputs; without `outputs` the run returns `{"status": "completed"}` |
| `agent` | `profile` (`none` or `coding`), `instructions` (prepended to every task prompt), `system` (the model's system message for the agent's conversation, a template over inputs), `tools` (an allowlist for every task, also enforced on proposed calls), and `sampling`, `thinking`, `turn` defaults for every task |
| `execution` | `limits` (`model_turns` is required; `steps`, `tool_calls`, `delegate_depth` optional), `workspace: repository`, `resume: true` |
| `capabilities` | the exact grant; omitted, it is derived and shown in the plan |
| `gates` | named checks, below |
| `workflow` | the tasks, in order |
| `success` | `gates: [...]`, `metrics: {<gate>.<metric>: {gte: N}}` and `work_items: {all_completed: true}`, evaluated after the last task |

**Tasks.** `name` (the task's identity: generated step ids derive from it, so
they survive reordering and reformatting), `task` (the prompt, a template),
`tools`, `when` (a condition; the task is skipped when it does not hold),
`follow_ups` (further user turns in the same conversation, each with an optional
`when`), `outputs` (files the task must leave; each is checked to exist),
`verify` (checks that must pass, in order), `repair: {attempts, prompt}`,
`model_only`, `system`, `schema`, `sampling`, `thinking`, `turn`, and at most one
of the loops below.

**Roles and conversations.** All agent tasks share one conversation (follow-ups
and repair turns continue it), which `agent.system` opens as a real system
message. A `model_only: true` task is a single model turn without tools, for a
planner or a supervisor, in a conversation of its own that `system` (a template
over inputs and earlier outputs) opens; its repair turns are model-only turns in
that conversation and its answer is `tasks.<name>.output`. With a `schema` (a
JSON Schema object) the answer must be JSON satisfying the schema's shape (a
malformed answer fails the run with `invalid_answer`) and later templates read
its fields, `{{ tasks.plan.output.goal }}`. Values substituted into a template
are data: braces or template syntax inside an input or an output are carried
verbatim and never expanded again.

**Decoding settings.** `sampling: {temperature, top_p, top_k, seed}`,
`thinking: {enabled, budget}` and `turn: {max_output_tokens,
first_response_timeout, deadline}` (seconds; `deadline` cuts a turn that is still
running after that long, whole, with an error naming the bound) on the `agent` set defaults and on a task
replace them field by field; repair turns decode like their task. Each is a
literal within its range or a single `{{ inputs.<name> }}` (a `seed` from an
input makes a rerun with the same inputs reproducible; no seed is ever
defaulted). The settings travel with each model request (`thinking.enabled` as
`chat_template_kwargs.enable_thinking` to servers that read it, never to the
`openai` driver), and the trajectory records, for every request, what it was sent
with under `extra.sven.decoding` of its step. A setting that does not resolve
fails the run with `invalid_setting`.
**Model roles.** `agent.model` and a task's `model` name a *role*, never an
endpoint or key. The host maps roles to providers
(`EngineBuilder::model_role(role, provider)`); a role it does not provide fails the
turn with an error naming it, and there is no fallback to the default model.

**Gate data.** A command gate may take `stdin` (a template, at most 32 MiB; it
reaches the program verbatim). After a check runs, templates read
`gates.<name>.passed`, `.exit_code`, `.stdout`, `.stderr`, `.kind` and `.detail`.
A check written `{gate: <name>, required: false}` is *soft*: it runs and its
result is readable, but it neither earns a repair nor fails the task. With
`on_failure: return` a failed check or exhausted loop ends the run by returning
its outputs with `status: failed` and `failure`, instead of failing it.

**Turn origins.** Every user turn the workflow adds to a conversation is labelled
in the trajectory (`extra.sven.origin`): `task`, `follow_up`, `repair` or
`correction`.

**Stop statuses and budgets.** `execution.limits.tool_calls` may be a single
`{{ inputs.<name> }}`. A task's `allowance` is the tool calls its first turn may
make; follow-ups and repairs draw on the rest (a follow-up's own `allowance`
sets it afresh) and a call beyond it is refused. `max_repeated_results: N` ends a
turn when the same call has been answered the same way N times. A turn ends
with a stop status readable as `tasks.<name>.stop` (`concluded`,
`stopped_without_conclusion`, `output_truncated` when the provider cut the final
reply off at its output-token limit, `tool_cap`, `repetition`) and `tasks.<name>.calls_left`; none of them fails
the run, and `when` conditions can branch on them. A follow-up's `max` (1 to 8)
repeats it while its `when` holds.

**Corrections.** `corrections: {max, count, check, while, author, accept, retries,
refusal, allowance}` on an agent task is a supervisor loop: the command gate
`check` judges the result; while `while` holds (default: the check failed) the
model-only `author` (`name`, `model`, `system`, `task`, decoding keys) writes a
message from `gates.<check>.*`, the command gate `accept` reads it on stdin
and decides whether it may be sent (a refused message is rewritten up to
`retries` times, then the loop ends without sending), the accepted message is
sent as a `correction` turn, and the check runs again. At most `max` (or the
`count` input, if lower) corrections are sent; exhausting them is not a
failure, the task's checks decide that afterwards.

Templates read `inputs.<name>` and `tasks.<name>.output` (the text of that
task's last agent turn) for tasks that have run; `facts` and `steps` are
internal and rejected. A repair prompt also reads `failure.gate`,
`failure.attempt`, `failure.kind`, `failure.detail`, `failure.exit_code`,
`failure.stdout` and `failure.stderr`; a default prompt is used when none is given.

**Loops.** `for_each: {source, id, max_items}` runs the task once per item of a
collection: `source` is `{json: {file, pointer}}` (an array in a workspace file)
or `{input: <name>}` (an input declared `json`); `id` names the field that
identifies an item (items that are strings or numbers name themselves; ids must
be unique and non-empty); `max_items` (default 500, at most 10000) bounds the
collection and a larger one fails the run. In the task's templates `item` is the
item (`{{ item.module }}`), `item_id` its id and `item_index` its position from
0; a gate's argv and a file check's path may read them too, in a `for_each`
task. `until: {metric: <gate>.<metric>, gte: N, max_iterations: M}` runs the task,
then measures the metric with a fresh run of the gate's command, and repeats
while it is not met; the task's templates read `{{ iteration }}` (the pass, from
1), and a run that uses all `max_iterations` passes fails with `until_exhausted`.

**Work items.** A `for_each` reads its collection **once**, when the loop
begins, and freezes it in the run state: each item's id, value and SHA-256, its
status (`pending`, `active`, `completed`), how often its body was started, and
the digest of the plan whose checks it passed. What the workspace holds
afterwards cannot add, drop or reorder an item (`backlog_mutation_does_not_
skip_items`), and the file source's digest is recorded. An item is completed
only when its body reaches the end of its checks; a failing item fails the run at
that item, leaving the later ones `pending`. The run state carries the whole
record, so a checkpoint resumes at the item it was on: completed items are not
run again, the collection is not read again, and the item that was in flight
starts its interrupted step again. The loop's summary `{total, completed,
pending}` is what `success.work_items.all_completed` checks; a `for_each` task
that did not run (its `when` was false) has not completed. Codes:
`iterate_source_unreadable`, `iterate_source_malformed`, `iterate_source_not_array`,
`iterate_missing_id`, `iterate_duplicate_id`, `iterate_too_many_items`. The trace
records `IterationStarted`, `ItemStarted` and `ItemCompleted`.
**Durable progress and a changed backlog.** `checkpoint: each_item` on a
`for_each` task hands the host a checkpoint every time an item completes
(`HarnessRun::on_item_checkpoint(sink)`; `sven harness run --checkpoint-out FILE`
keeps the latest one in FILE, replaced atomically), so a crash loses at most the
item in flight; the checkpoint resumes like one a bound left. The host sees the
machine after a dispatch, so the item that was in flight is settled as
interrupted on resume. `for_each: {reconcile: frozen | refresh}` says what a
*resumed* run does with a backlog file that changed meanwhile. `frozen` (the
default) keeps the list. `refresh` reads the file again at the first item
boundary after the resume and merges by item id, in the file's order: an
unchanged item keeps its status; a completed item whose content digest changed
is pending again (a result is evidence only about the content it was produced
for); new ids are appended pending; ids no longer in the file are dropped. The
trace records `ItemsReconciled {kept, invalidated, added, removed}`. A file the
agent can write decides the list under `refresh`, so use it only when that is
the intent; it needs a file source. A completed item records the plan digest it
was verified under; a changed gate is a changed plan, whose checkpoints are
refused, so invalidation by gate change needs no separate switch.
Item values from a file the agent wrote are data the agent controls: they reach
prompts, gate argv elements (never a shell command line) and file-check paths (which
the verifier confines to the workspace, so a path that escapes is `unknown`, not
passed).

**Gates and checks.** A gate is either `command: [argv...]` (with `timeout`
seconds, default 600, at most 3600; `cwd` inside the workspace; and `metrics`:
numbers the command leaves in JSON files, `{file, pointer, type: number}`) or
`artifact: <path>` (a file that must exist, with optional `min_bytes` and
`sha256`). A `verify` entry is a gate name, an inline `{artifact: ...}`, or
`{metric: <gate>.<metric>, gte: N}` (`eq`, `ne`, `gt`, `gte`, `lt`, `lte`).
A metric check *runs the gate's command first*, every time, and then compares
the number the run left: a figure from an earlier run is not evidence.

**Trust.** A command gate's argv is part of the compiled plan; the agent
cannot redefine it. It runs without a shell, so no string is interpolated into a
command line. The exit status decides: the gate passes only for status 0 with no
timeout and no start error, whatever the program printed and whatever the agent
claims. A script *inside the workspace* that the command runs is as editable as
the code under test, so such a gate is only as strong as the script; pin it with
an `artifact` check with `sha256`, or keep it outside the workspace.

**Failure is explicit.** A failed check earns a repair turn while attempts
remain (at most 16 per task) and then the checks run again from the first;
when the attempts are spent the run fails with `gate_failed` (a failed success
condition: `success_check_failed`), whose message names the task, the check and
the reason. Nothing advances past an unsatisfied check.

**Capabilities.** The profile `coding` needs `ReadFile`, `WriteFile`,
`DeleteFile` and `ExecuteShell`; every command gate needs `ExecuteShell`;
file and metric checks need `RunVerifier`. A declared `capabilities` list must
cover them (the error lists what is missing); an omitted one is derived. The
plan carries `profile` so a host knows which tools the agent steps expect.

**Lowering.** Each task becomes: an optional `<task>__when` branch, the agent
step `<task>__work`, `<task>__f<j>` follow-up steps, and for each check and
attempt `<task>__v<i>_<check>_a<k>` (a `gate` step, or a `verify` step followed
by a `_ok` branch; a metric check has a `_run` gate first), a `_repair` agent
step and a `_fail` step. Repair is unrolled rather than a `retry` (a failed
verification is a verdict, not a failed step), so the plan is acyclic and its
`max_steps` default is its step count. The plan's `source_map` names, for every
generated step, the task, role (`when`, `work`, `follow_up`, `check`, `repair`,
`failure`, `complete`), check and attempt. The digest covers the source map and
the stamp `harness-plan/2`; version 1 plans keep `harness-plan/1` and are
byte-for-byte what they were (`v1_compiled_plan_unchanged`).

### Imports (`imports:` and `use:`)

A version 2 workflow can run another as one of its tasks:

```yaml
imports:
  review: ./lib/review.yaml        # alias: a path relative to this document
workflow:
  - name: reviewed
    use: review                    # instead of `task:`
    with: { subject: "{{ tasks.change.output }}" }
outputs:
  verdict: { type: string, value: "{{ tasks.reviewed.output.verdict }}" }
```

A `use` task takes only `use`, `with`, `when` and `description`. It lowers to one
`delegate` step with `propagate_failure`: the imported workflow runs as a child
and what it returns is `tasks.<name>.output`, whose fields (`.verdict`) are the
outputs it declares; a child that fails fails the caller with `child_failed`.

*Checked when the package compiles:* the alias exists and every import is used;
`with` passes exactly the inputs the child takes, literals and `{{ inputs.x }}`
holes with a type that fits; later templates read only outputs the child
returns (and nothing but `output` of a `use` task); the capabilities the child
holds are held by the caller too (a derived grant adds them and `SpawnChild`; a
declared one that lacks one is an error).

*Package rules.* An import is a file of the package: a relative `.yaml` path,
resolved against the importing document's directory, that stays inside the
package root (no absolute path, no URL, no `..` out of it); `load_package` also
refuses a symlink that leaves the directory. An import cycle is an error that
names the chain; two documents of a package never share a name (`delegate`
resolves by name); a file nothing imports is an error; only version 2 documents
can be imported; at most 64 imports, 8 levels deep, 1 MiB each. There are no
remote imports.

*Pinning.* `HarnessBundle` carries `imports` (path to document text). The bundle
digest covers the compiled-plan digest of every imported file, so a changed
behaviour is a changed pin and a reformatting is not; a checkpoint taken under
one is refused under another. `sven harness validate|inspect|run|test` read a
directory whose `harness.yaml` has `imports` as a package automatically, and
`Engine::load_harness_from_str` registers the main workflow and every import
together or not at all.

*Shared limits.* A child starts with a ceiling of what its parent had left of
its model-turn and tool-call budgets (its own limits can only be tighter) and,
when it ends, its usage is added to the parent's counters: a tree of runs never
spends more than its root declared. A version 2 child answers its parent with an
envelope `{ok, value | code, message, usage}`; a delegate to a version 1 harness
is unchanged. Capabilities narrow as before: a child holds what its caller
holds, narrowed by its own declaration.

### The `gate` step

The thirteenth primitive, written only by the lowering and rejected in a
version 1 document: `gate: {command, timeout, cwd, store_as, then, on_fail}`. It
calls the reserved tool `shell_gate` (`sven_harness_spec::GATE_TOOL`; capability
`ExecuteShell`, so host approval policy applies) with `{argv, timeout_secs,
cwd}`. The tool, `sven_tools_exec::GateCommandTool`, runs the program in a new
session, confined to the workspace, kills the whole process group at the
deadline, and returns JSON `{exit_code, timed_out, error, stdout, stderr,
truncated, duration_ms}` (the last 16 KiB of each stream). No agent mode lists
the tool and the runtime refuses an agent step's proposed call to it. The machine
stores `{passed, kind, exit_code, timed_out, stdout, stderr, duration_ms,
detail}` under the step (`kind` is `passed`, `failed`, `timeout` or `exec_error`),
records a `GateEvaluated` trace record, and continues at `then` on a pass and
`on_fail` otherwise. A host that does not provide the tool fails every gate
closed. A gate interrupted by a suspension is run again on resume and costs
neither a repair attempt nor a step.

### The `iterate` step

The fourteenth primitive, written only by the lowering and rejected in a version
1 document: `iterate: {for_each | until, body, then, on_exhausted, store_as}`. The
body is ordinary steps whose last edge returns to the `iterate` step, which is
visited once per item or pass. A file source is read through the reserved tool
`read_json_snapshot` (`sven_harness_spec::SNAPSHOT_TOOL`, capability `ReadFile`;
`sven_tools_fs::ReadJsonSnapshotTool`, confined to the workspace, at most 8 MiB)
that answers `{digest, value}`; like `shell_gate` no agent mode lists it and the
runtime refuses an agent step's proposed call to it. A read interrupted by a
suspension is read again. Because an `iterate` plan is cyclic its default
`max_steps` is the bound computed from `max_items` and `max_iterations`; set
`execution.limits.steps` for a tighter one.

A workflow's model turns and verifications that a suspension interrupted start
again on resume (a version 1 document keeps the retry policy it declares).

A version 2 `delegate` can set `propagate_failure`: a child that returns
`ok: false` then fails the parent step (through its retry frame, if any) with
`child_failed` instead of being stored and continued past.

**Trajectory.** `HarnessRun::trajectory` holds the agent conversation, starting
with its system message as a `system` step, and each model-only conversation as
its own embedded trajectory (`extra.harness.conversation` names it, and
`extra.harness.conversations` lists them), so a planner's or supervisor's
exchange is never mixed into the agent's.

**Workspace per run, tools on every host.** `HarnessRun::in_workspace(dir)` roots
one run's file, search and shell tools, its gates and its snapshot reads in
`dir`, so one engine serves many concurrent runs, each in a tree the host laid
out and reads afterwards. `shell_gate` and `read_json_snapshot` are registered by
the SDK for every harness run, confined to the run's workspace, whatever toolset
the engine has; a host tool of the same name (a fixture) replaces them.

## Runtime

`HarnessMachine` is one `Machine` with a fixed hierarchy of eleven states,
whatever the plan's size; the current step and every result live in a durable
context fact, so a run snapshots and restores like any kernel machine.

| State | The run is |
|-------|------------|
| `Ready` | created, waiting for the message that starts it |
| `Advancing` | working off pure steps (`assign`, `branch`, `retry`, `return`, `fail`) |
| `WaitingForModel` | in a `predict` or `agent` turn |
| `WaitingForTool` | waiting for a tool result (also while a person is asked to approve it) |
| `WaitingForVerifier` | waiting for a verdict |
| `WaitingForChild` | in a `delegate` |
| `WaitingForJoin` | in a `parallel`, branches running |
| `Completed`, `Failed`, `Cancelled` | terminal; further events are absorbed |

(`Root` is the fixpoint superstate.) The machine only emits typed effects
(`CallLlm`, `CallTool`, `Verify`, `InstantiateSubmachine`,
`RequestHumanApproval`); executors perform them. A run waiting on live child
runs is not quiescent, so a deadline or cancellation is honoured rather than
reported as a stall.

### Agent steps

The first turn offers the step's `tools` list, or the session's agent-mode
tools when the step lists none. A list is also an allowlist: a proposed call
outside it is never run, is reported back to the model as refused with the
reason, and does not count as a tool call. A turn of only refused calls
continues at once. A tool that fails reports the failure to the model; it does
not fail the run.

### Children

`delegate` and `parallel` start child runs through the kernel's child spawner.
A harness child gets its own tools, executor and cancel scope; it can verify,
delegate and fan out in turn within `max_delegate_depth`, because the spawn
descriptor carries the child's depth. Branches of a `parallel` run the same
plan from their branch step with the run's inputs and their own facts; the
join's value is the branches' return values in declared order, whichever
finished first. A child that ends in `fail` does not fail its
parent: its result is `{code, message}` and the parent step stores it like any
value, so a parent that cares branches on it (`on: facts.child.code`).
Cancelling a run cancels its children, and a run's tool calls end with it
(the executors are bound to the run's cancel scope, so a call that outlives
its `execute` is dropped when the run ends).

### Authorization

Four rules bound every harness session; each is enforced by the kernel's
permission gate or its executors, not by the harness text.

1. **Declaration.** The permission policy of a run is its plan's declared
   capabilities. A child's policy is its inherited contract narrowed by its
   own declaration (`ChildRunContract::narrow`): it holds what both its
   parent and its own document allow, never more.
2. **Host approval.** The host's policy for the session still applies to a
   harness: the read-only agent modes narrow it, and under manual approval
   every call that is not read-only is held and a person is asked. The
   machine drives that flow (`RequestHumanApproval`, then
   `HumanApproved` or `HumanRejected`), one call at a time in the order the
   gate reported them; a refused call fails the step like any failed call.
3. **Registry reconciliation.** A harness derives a call's capability from the
   tool's name, which a tool can contradict. In harness sessions and harness
   children each call is also held to the capability its registered tool
   really has; a mismatch is answered `ToolFailed` and the tool does not run.
   Reconciliation is not applied when a `tool_executor_override` replaces the
   tool slot, because that executor is the override's own.
4. **Allowlist.** An `agent` step's `tools` list, as above.
5. **Host ceiling.** `EngineBuilder::host_capabilities` caps every harness
   run (and so every child) on an engine: the run's policy is the
   intersection of its declaration and the ceiling, so a ceiling narrows and
   never grants. `sven harness run --toolset coding` sets it from
   `--allow-capability`. Without a ceiling the declaration is the whole cap.

`parallel` and `delegate` need `SpawnChild`, and a child's depth is bounded by
`max_delegate_depth`.

### Observability

The run keeps a trace of `TraceRecord`s: `StepStarted` (with the attempt),
`StepCompleted`, `BranchSelected`, `Verified`, `RetryAttempt`, `ChildStarted`,
`ChildCompleted`, `FanOut`, `Joined`, `Failed` and `Outcome`. Each is
published as a `HarnessEvent` session event (`Started`, `StepStarted`,
`StepCompleted`, `RetryScheduled`, `BranchSelected`, `ChildStarted`,
`ChildCompleted`, `FanOut`, `Joined`, `Verified`, `BudgetExhausted`, `Failed`,
`Completed`). The headless runner prints them as `[sven:harness] ...` lines;
ACP does not forward them.

**In the author's terms.** A workflow's generated steps are named in its plan's
`source_map`. `sourced_trace(plan, trace)` (and `SourceTracker`, for a stream)
returns each record with a `source` object: `task`, `role` (`work`, `check`,
`repair`, ...), `gate`, `attempt` (the verification attempt, 0 the first),
`item_id` (the `for_each` item whose body the record belongs to) and `result`
(`passed`, `failed` or `completed`, for a gate, a verifier, a child or a
finished item). The record's own fields are unchanged. `sven harness run`
prints and writes (`--trace`) records this way; a version 1 plan has no source
map, so its records carry no `source`.

## Checkpoints, recordings and replay

**Checkpoint.** A run stopped by a bound (`RunConclusion::Timeout` or
`BudgetExhausted`) leaves a `HarnessCheckpoint`: the kernel snapshot, the plan
digest, the bundle digest and the run id. It is `Serialize`/`Deserialize`, so
it can be stored as JSON and resumed in another process with
`Harness::resume`. A checkpoint under another plan or bundle is refused. The
operation the stop interrupted did not complete; on resume it is settled
through the step's failure path, so the step's `retry` decides what repeating
it is worth. A resumed run keeps the run id and continues the trace. The restored machine
re-emits nothing for an operation in flight (model turn, tool call, child
batch, approval question), and a completion it has already consumed - a
repeated tool result, model turn, approval or branch completion - settles
nothing a second time; `harness_checkpoint_interruptions.rs` holds these
cases for each waiting state and for retries.

**Item checkpoints.** A host that must survive a kill it cannot catch (an OOM
kill, a power cut) asks for a checkpoint each time a `for_each` item completes:
`HarnessRun::on_item_checkpoint(sink)`. The run is tapped after every dispatch
(`ErasedRuntime::snapshots`), so the checkpoint arrives while the next item is
already underway, not when the run next comes to rest; it is a checkpoint like
the one a deadline leaves, resumable with `Harness::resume`, and the item in
flight starts its interrupted step again. `sven harness run --toolset coding
--checkpoint-out FILE` keeps the latest one in `FILE` (replaced atomically,
removed when the run ends with a verdict). The same tap publishes the trace
as the run goes: a step's records no longer wait for the step to end.

**Recording.** `HarnessRun::with_recording()` keeps the machine's input event
log - kickoff, model answers, tool results, child verdicts, approvals -
exposed as a `HarnessRecording` (JSON, format `sven-harness-recording/1`) by
`HarnessRun::recording()` after `execute`. A machine is deterministic in its
events, so that log, not the trace, is the run's complete input (a trace holds
no step values).

**Replay.** `Harness::replay(&recording)` re-dispatches the log into a fresh
machine (or the checkpoint a resumed run started from) and executes no
effect: nothing asks a model, runs a tool or starts a child. It returns a
`HarnessReplay { state, result, trace, steps, effects }` where `effects` are
those the machine emitted, never run. Two replays of one recording are
identical. A recording of another plan or bundle, or of an unknown format, is
refused.

**Trajectory.** `HarnessRun::trajectory()` exports the plan thread's messages
and tool calls as an ATIF trajectory, with the harness name, digests, run id
and trace under `extra.harness` and the token usage and step count in the
final metrics. Children's conversations are not included.

**Counts.** `HarnessOutcome::step_counts()` and `HarnessReplay::step_counts()`
count `StepStarted` per step (a retry body counts once per attempt).

## SDK API

```rust
use sven_sdk::{Engine, RunOptions};

let engine = Engine::builder().model_provider(provider).build()?;
let harness = engine.load_harness("samples/harnesses/ticket-routing/harness.yaml")?;

let mut run = harness
    .start(serde_json::json!({ "ticket": "crash on start" }))?
    .with_recording();
let outcome = run.execute(RunOptions::new()).await?;

match &outcome.result {
    Some(Ok(value)) => println!("returned {value}"),
    Some(Err(failure)) => println!("failed {}: {}", failure.code, failure.message),
    None => println!("stopped by a bound; checkpoint: {:?}", run.checkpoint().is_some()),
}

let replay = harness.replay(&run.recording().unwrap())?;   // offline, executes nothing
assert_eq!(replay.trace, outcome.trace);
```

| Item | |
|------|--|
| `Engine::load_harness(path)`, `load_harness_from_str(yaml)` | parse, validate, compile offline and register under the plan's name; a document or a bundle (top-level `document` and `helpers`); a duplicate name is refused |
| `Harness::{name, digest, plan, validate_inputs}` | the compiled plan (`sven_sdk::harness_plan`) and an input check |
| `Harness::start(inputs)` | a `HarnessRun`; inputs are checked against the plan first |
| `Harness::resume(checkpoint)` | continue a checkpoint under the same program |
| `Harness::replay(&recording)` | offline replay |
| `HarnessRun::execute(RunOptions)` | run to a verdict or a bound; once per run |
| `RunOptions::{cancel, deadline, max_output_tokens}` | cancellation is a `CancelToken`; a bound suspends |
| `HarnessRun::{events, subscribe_trace, run_id, checkpoint, with_recording, recording, trajectory}` | observation and export |
| `HarnessRun::on_item_checkpoint(sink)` | a checkpoint each time a `for_each` item completes |
| `HarnessOutcome { conclusion, result, usage, trace, steps }` | `result` is the plan's verdict, `None` when a bound stopped the run first |
| `TraceRecord`, `HarnessEvent`, `harness_plan::*` | re-exported so an application need not name the crates behind the facade |

Many runs can be in flight through one engine; they share the model client
and the registry and nothing else.

## Testing and the kit

`sven-harness-testkit` is the library behind `sven harness test`, `compare`,
`run` and `replay`. Its entry points are `HarnessDir::open`, `validate`,
`inspect`, `io_schema`, `run_once`, `resume_once`, `replay_once`, `run_case`,
`run_suite`, `compare` and `compare_paired` (matched seeds, per-task
pass rates, a seeded percentile bootstrap, cost totals, a promote / reject /
inconclusive verdict), with `RunnerOptions { opt_in, replay_check,
case_filter, seed }`. Every case runs on a new engine with `Config::default()`, no
built-in tools, an injected provider and automatic approval; it never reads
the user's configuration. A live model is built only under `OptIn::live(model, endpoint)`,
which the command line constructs from `--model` and `--model-endpoint`; the
testkit reads no environment variable. The case format is in
[Harnesses](../guides/harnesses.md#cases).

Samples are data directories; the SDK-only crate
`samples/harnesses/sdk-runner` shows the same cases run with `sven-sdk` alone.
`make test/harness` runs every harness suite on mock models, including a
thousand-run concurrent stress test (a fixed count).

## Not in scope

Python or other process-isolated helpers: a helper is WASM, carried in the
bundle, with no filesystem or network. A process-isolated helper would need a
new spawn and sandbox executor and a host interpreter, and duplicates what
the WASM path already provides.
