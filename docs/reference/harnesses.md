<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Programmable harnesses

A harness is an agent workflow in YAML that compiles through the HSM kernel.
This page is the reference: the language, the runtime states, the
authorization model, checkpoints, recordings, trajectories and the SDK API.
For the commands see [Harnesses](../guides/harnesses.md); for the
reasons behind the design see [ADR 0004](../adr/0004-programmable-harnesses.md)
for the workflow language [ADR 0005](../adr/0005-declarative-harness-v2.md) and for
the removal of the earlier step-map language [ADR 0006](../adr/0006-v2-is-the-only-harness-language.md).

| Crate | Tier | Role |
|-------|------|------|
| `sven-harness-spec` | foundation | the workflow document, typed AST, lowering to the step specification, validator, compiler to a content-hashed `HarnessPlan`, JSON Schema |
| `sven-harness-runtime` | machines | `HarnessMachine`: the plan interpreted as a kernel `Machine` |
| `sven-sdk` | sdk | `Engine::load_harness`, `Harness`, `HarnessRun`, recording and replay |

## The language

A harness document describes *what a job is* instead of *which steps run*:
tasks, the trusted gates that decide whether each one succeeded, how failures
are repaired, and what counts as success. It is decoded by a strict parser (an
unknown key is an error at every level, named in the message; the JSON Schema
forbids them too), checked in the author's terms, and lowered into a step
specification; that specification goes through a validator and a compiler to a
content-hashed plan, and the machine runs the plan. Every tool that reads a
plan, a checkpoint, a recording or a trace therefore works on any document.
The version is read from the raw document before anything is deserialized:
`api_version` is required and `"2"` is the only value. `"1"` (the earlier
step-map language) is refused with an error that says so
([ADR 0006](../adr/0006-v2-is-the-only-harness-language.md)); a document is
never retried as another version. The machine-readable form is `sven harness
schema` (`crates/harness-spec/schema/harness-document.schema.json`,
`DOCUMENT_SCHEMA`).

| Key | |
|-----|---|
| `api_version` | required, `"2"` |
| `name`, `description` | `name` is lowercase letters, digits, `-`, `_`; no `__`; at most 48 characters |
| `inputs` | typed per-run inputs (`name: type` or `{type, description}`, type `string`, `number`, `boolean` or `json`); templates read `{{ inputs.<name> }}` and an undeclared one is an error |
| `outputs` | `name: {type, value}`; the value is a template over inputs and task outputs; without `outputs` the run returns `{"status": "completed"}` |
| `agent` | `profile` (`none` or `coding`), `instructions` (prepended to every task prompt), `system` (the model's system message for the agent's conversation, a template over inputs; or a list of `{when, text}` variants, see below), `tools` (an allowlist for every task, also enforced on proposed calls), and `sampling`, `thinking`, `turn` defaults for every task |
| `execution` | `limits` (`model_turns` is required; `steps`, `tool_calls`, `delegate_depth` optional), `workspace: repository`, `resume: true`, `scratchpad` (the run's private directory, below) |
| `capabilities` | the exact grant; omitted, it is derived and shown in the plan |
| `gates` | named checks, below |
| `workflow` | the tasks, in order |
| `success` | `gates: [...]`, `metrics: {<gate>.<metric>: {gte: N}}` and `work_items: {all_completed: true}`, evaluated after the last task |

**Tasks.** `name` (the task's identity: generated step ids derive from it, so
they survive reordering and reformatting), `task` (the prompt, a template),
`conversation`, `do`, `tools`, `when` (a condition; the task is skipped when it does not hold),
`follow_ups` (further user turns in the same conversation, each with an optional
`when`), `outputs` (files the task must leave; each is checked to exist),
`verify` (checks that must pass, in order), `repair: {attempts, prompt}`,
`model_only`, `system`, `schema`, `sampling`, `thinking`, `turn`, and at most one
of the loops below.

**Roles and conversations.** Every task runs in a conversation of its own,
which `agent.system` (or the task's `system`) opens as a real system message;
its follow-ups, repair turns and corrections continue it. Inside a `for_each`
each item gets a new one (`<task>#<item_index>`), inside an `until` each pass
(`<task>#<iteration>`), so no agent reads another task's or another item's
history: what a task needs from an earlier one reaches it through templates
(`{{ tasks.audit.output }}`). `conversation: <name>` (a template) puts a task
in a named conversation instead; tasks that name the same one share it. `agent.system` may instead be a list of `{when, text}` entries: the
first whose `when` (a condition over inputs) holds is chosen when the run starts
and is both what the model is sent and the trajectory's system step, so one
document serves runs with different standing instructions. The last entry has no
`when` (it is the default; an entry without `when` before the end is rejected as
leaving the rest unreachable). A `model_only: true` task is a single model turn without tools, for a
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
reply off at its output-token limit, `tool_cap`, `repetition`, `watchdog`) and `tasks.<name>.calls_left`; none of them fails
the run, and `when` conditions can branch on them. A follow-up's `max` (1 to 8)
repeats it while its `when` holds.

**Counts, skipped tasks and output limits.** `repair.attempts` and a
follow-up's `max` are a whole number or a single `{{ inputs.<name> }}` (a number
input) given with `at_most`, the bound the plan is unrolled to: the run uses as
many as the input says, up to `at_most` (`repair: {attempts: "{{ inputs.tries
}}", at_most: 4}`). A task whose `when` did not hold leaves `tasks.<name>.output`
as the empty string (an object of empty strings for a structured or imported
task) and `tasks.<name>.stop` as `skipped`, so later templates read empty text
and not a hole. `thinking.budget` read from an input that is empty at run time
(null or empty text) is unbounded. A gate's `output_bytes` (1024 to 1048576,
default 16384) says how much of the end of each output stream its result keeps.

**Replies that go wrong.** A reply with no text and no tool call is not an
answer: the turn is asked again, twice at most, after a pause that doubles
from 2 s, each retry a `TurnRetried` trace record (`empty_reply`), and when the
replies stay empty the run fails with `empty_reply`. A server failure that may
pass (a dropped connection, a timeout, an overloaded or unavailable server) is
asked again up to four times after 5, 10, 20 and 40 s (`transient`) and then
fails with `model_unavailable`. A prompt that does not fit the model's context fails with
`context_exhausted`, and a reply cut off at the output limit is the stop status
`output_truncated` (a thinking model can use a small `turn.max_output_tokens`
entirely on reasoning; raise it).
A turn that sven's own thinking watchdog cuts short (the model loops in its
reasoning, or passes `agent.max_thinking_tokens`, `agent.thinking_timeout_secs`
or the turn deadline) is not a cancel: an agent task ends with the stop status
`watchdog` and its checks decide, a `predict` step (which has no answer to
fall back on) fails with `turn_aborted` and the watchdog's reason. Only a
person's or a signal's cancel ends the run as `cancelled`.

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

A gate's arguments read the tasks that have run, so a check can be told why the
agent stopped (`command: [check, --stopped, "{{ tasks.work.stop }}"]`), and
`while` can follow the stop status (`while: {eq: tasks.work.stop, value:
concluded}` corrects only a run that really concluded, not one that was cut off
by `output_truncated`, `tool_cap`, `repetition`, `watchdog` or `stopped_without_conclusion`).

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

**Variables.** `variables: {<name>: {type, scope, schema, description}}`
declares typed values shared between tasks (`type` is `string`, `number`,
`boolean` or `json`; `scope` is `run`, the default, or `item`, cleared when
each loop item or pass begins; `schema`, for a `json` variable, is the shape
its value must have: `type`, `properties`, `required`, `items`, `enum`). A
task names what it may see with `vars: {<name>: read | write | required}`;
nothing else is visible to it. An agent task that names any variable is
offered three tools that the machine answers itself (they never reach an
executor, and what they do is checkpointed and replayed with the run):
`harness_vars_list` (each visible variable's name, type, description, schema,
access and whether it is set), `harness_vars_get {name}` and
`harness_vars_set {name, value}`. A set is refused, with the reason, when the
task may only read the variable or the value does not fit its type or schema
(a `json` variable given a string that holds a JSON object or array stores the
parsed value; a value is at most 256 KiB). Such a task needs a `tools` list,
to which the three are added. A `required` variable still unset when the
task's turns end earns a reminder in the task's own conversation (`origin:
variables`), twice at most; still unset, the run fails with
`missing_variable`. Templates anywhere read `{{ vars.<name> }}` and the fields
of a `json` variable (`{{ vars.checks.test }}`), so an exploration task can
publish what later prompts and gates use. A command gate with `capture:
<variable>` stores its standard output in that variable when it passes (parsed
as JSON for a `json` variable, as a number or a boolean for those types,
trimmed text for a string; output that does not fit, or that was cut at
`output_bytes`, fails the gate): a value a trusted command computed, such as a
backlog (`for_each: {source: {variable: files}}` iterates a `json` array
variable, read once when the loop begins) or a base commit. **Trust rule:** a
gate may not read (in its command, script, `env` or `stdin`) a variable that
an agent can set in a task that runs after a task the gate checks - so an
exploration task may publish the commands later checks run, but no later agent
can change them; the validator names the gate, the variable and both tasks.

**Loop bodies and check tasks.** A loop task may carry `do: [tasks...]`
instead of a prompt: for each item (or pass) the tasks of the body run in
order, each with its own prompt, tools, conversation, checks and repairs, and
`item`, `item_id`, `item_index` (or `iteration`) in scope in all of them; a
body task reads the outputs of the body tasks before it in the same item. The
loop task itself then takes only `name`, `description`, `when`, `for_each` or
`until`, and `checkpoint`, and the tasks of its body cannot loop themselves.
An item completes when the last task of its body has passed its checks. A
task with `verify` and no `task` prompt is a *check task*: its checks run with
no model turn and its output is empty (useful for steps such as recording a
base commit or committing).

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
pending}` (and `failed`, `unfinished`) is what `success.work_items` checks; a `for_each` task
that did not run (its `when` was false) has not completed. Codes:
`iterate_source_unreadable`, `iterate_source_malformed`, `iterate_source_not_array`,
`iterate_missing_id`, `iterate_duplicate_id`, `iterate_too_many_items`. The trace
records `IterationStarted`, `ItemStarted` and `ItemCompleted`.
**Failed items.** `for_each: {on_item_failure: {verify: [gates]}}` turns an
item whose check fails for good (repairs spent, or a required variable never
set) into a failed item instead of a failed run: its reason is recorded, the
listed gates run in order (a rollback, say - if one fails, the run fails with
`rollback_failed`), and the loop goes on with the next item. A failed item is
not started again on resume. The loop's summary is `{total, completed,
failed, pending, unfinished}`; `success.work_items.all_completed` needs every
item completed, `all_settled` every item completed or failed. The trace
records `ItemFailed` with the reason.

**Durable progress and a changed backlog.** `checkpoint: each_item` on a
`for_each` task hands the host's sink a checkpoint every time one of *its*
items completes (`HarnessRun::on_item_checkpoint(sink)`; `sven harness run
--checkpoint-out FILE` keeps the latest one in FILE, replaced atomically); a
loop without the key hands it nothing. A crash loses at most the item in
flight; the checkpoint resumes like one a bound left. The host sees the
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
the intent; it needs a file source. `for_each: {covers: [paths]}` (templates over `item`, workspace-relative) names
the files an item covers: when the item completes, a digest of their contents
is recorded (the harness-only `digest_files` tool, capability `ReadFile`), and
at the first item boundary after a resume the digests are taken again: an item
whose files changed, appeared or vanished is pending again (trace
`ItemsInvalidated`). Any difference re-pends; nothing the agent says counts.
A completed item records the plan digest it
was verified under; a changed gate is a changed plan, whose checkpoints are
refused, so invalidation by gate change needs no separate switch.
Item values from a file the agent wrote are data the agent controls: they reach
prompts, gate argv elements and `env` values (never a shell command line: text spliced into a gate `run` script is code, so pass such values through `env`) and file-check paths (which
the verifier confines to the workspace, so a path that escapes is `unknown`, not
passed).

**Gates and checks.** A gate is either a command gate, written `command:
[argv...]` (a program, run without a shell) or `run:` (a script), or `artifact:
<path>`. Beside `command` or `run` a command gate takes `timeout` seconds (default
600, at most 3600, for the whole gate), `cwd` inside the workspace (or
`cwd_input`), `stdin`, `output_bytes`, `env`, and `metrics` (numbers the command
leaves in JSON files, `{file, pointer, type: number}`). Exactly one of `command`
and `run` is written. `run` is one multiline script, or a list of `{name,
script}` steps run in order, the first failure stopping the gate; `shell` is
`bash` (the default; the script runs as `bash -eo pipefail -c`) or `sh` (`sh -e
-c`). `env` is a map of variable names (`[A-Z_][A-Z0-9_]*`) to templates, set for
the command or each script, and works on a `command` gate too. A script is a
template like an argument (inputs, item variables, `run.scratchpad`, task
outputs and earlier gate results are in scope), but text substituted into a script
is code: pass data through `env` and read it as `"$NAME"`. A file check is
`artifact: <path>` (a file that must exist, with optional `min_bytes` and
`sha256`). A `verify` entry is a gate name, an inline `{artifact: ...}`, or
`{metric: <gate>.<metric>, gte: N}` (`eq`, `ne`, `gt`, `gte`, `lt`, `lte`).
A metric check *runs the gate's command first*, every time, and then compares
the number the run left: a figure from an earlier run is not evidence.

**Trust.** A command gate's argv or script is part of the compiled plan; the
agent cannot redefine it. A `command` gate runs without a shell, so no string is
interpolated into a command line; a `run` script is a shell command line by
design, which is why data belongs in `env`. The exit status decides: the gate passes only for status 0 with no
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
step and a `_fail` step. Repair is unrolled (a failed
verification is a verdict, not a failed step), so a plan without loops is
acyclic and its `max_steps` default is its step count. The plan's `source_map` names, for every
generated step, the task, role (`when`, `work`, `follow_up`, `check`, `repair`,
`failure`, `complete`), check and attempt. The digest covers the source map and
the stamp `harness-plan/2`.

### Failure codes

A run that fails ends with a code and a message. The check and loop codes are
`gate_failed`, `success_check_failed`, `until_exhausted`, `rollback_failed`,
`missing_variable`, `child_failed` and the `iterate_*` codes above. The bounds
are `step_budget`, `model_turn_budget`, `tool_call_budget` and `delegate_depth`.
The rest report the environment: `invalid_answer` (a schema'd answer that is not
JSON of that shape), `side_effect_failed` (a tool call failed or a child could not
run), `invalid_setting`, `empty_reply`, `model_unavailable`, `context_exhausted`,
`trajectory_unavailable`, `scratchpad_unavailable` and `cancelled`.

### Digests

The compiler produces a canonical plan whose digest does not depend on key
order or whitespace and changes when the program's meaning does. A bundle's
digest additionally covers the digest of every import. Runs, checkpoints and
recordings pin these digests, and a mismatch is refused, never reinterpreted.

### Imports (`imports:` and `use:`)

A workflow can run another as one of its tasks:

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
resolves by name); a file nothing imports is an error; at most 64 imports, 8 levels deep, 1 MiB each. There are no
remote imports.

*Pinning.* `HarnessBundle` carries the main `document` and its `imports` (path
to document text). The bundle digest covers the compiled-plan digest of every imported file, so a changed
behaviour is a changed pin and a reformatting is not; a checkpoint taken under
one is refused under another. `sven harness validate|inspect|run|test` read a
directory whose `harness.yaml` has `imports` as a package automatically, and
`Engine::load_harness_from_str` registers the main workflow and every import
together or not at all.

*Shared limits.* A child starts with a ceiling of what its parent had left of
its model-turn and tool-call budgets (its own limits can only be tighter) and,
when it ends, its usage is added to the parent's counters: a tree of runs never
spends more than its root declared. A child answers its parent with an
envelope `{ok, value | code, message, usage}`. Capabilities narrow as before: a child holds what its caller
holds, narrowed by its own declaration.

### Gate inputs: the trajectory and a trusted directory

A command gate can judge what the agent *did*, not only what it left behind.
`{{ run.trajectory_file }}` in a gate's argv or `stdin` is the path of a file
holding the run's ATIF trajectory so far, written by the host just before the
command starts (`trajectory.json` in the run's scratchpad, not the workspace).
The file is removed when the run ends; a host that asked to keep it
(`HarnessRun::keep_trajectory_file`) finds the final trajectory there. A host
that cannot provide it fails the run with `trajectory_unavailable`.

`cwd_input: <input>` runs the command in the absolute directory a `string` run
input names (a pinned checkout, a ledger) instead of the workspace. The host
sets inputs; the agent cannot. The gate tool refuses a directory that is
relative, missing, inside the workspace or holding it (the agent can write
there), and `cwd` and `cwd_input` are exclusive.

After a run `HarnessRun::corrections()` lists the messages the supervisors of
`corrections` blocks sent the agent (each with the tool calls made before it)
and `HarnessRun::refused_messages()` the supervisor messages an accept gate
refused, every one: a refusal sent back for a rewrite carries the text the
supervisor was sent, and the last one, when the block's `retries` are spent
and nothing was sent back, carries the gate's own account (its stdout, else
stderr, else its exit status); `sven harness run --json`
reports both, and `HarnessRun::trajectory()` is available after a failed run
too.

### The scratchpad

Every run owns a private directory. Its path is `{{ run.scratchpad }}` in
prompts, system texts and gate arguments and `stdin`, and a command gate also
finds it in its environment as `SVEN_SCRATCHPAD`. It holds what the run needs
to keep but must not put in the workspace: the run's audit log
(`audit.jsonl`; nothing of the run's own is written to `<workspace>/.sven`),
the trajectory file a gate reads, and whatever the workflow's gates leave there.

Where it is, in order of precedence:

1. the host's choice: `--scratchpad DIR` on `sven harness run` and `resume`,
   `HarnessRun::with_scratchpad(dir)` in the SDK;
2. on a resume, the directory the checkpoint recorded (the run state carries
   it, so a resumed run works where the first episode did);
3. `execution.scratchpad`, a template over `inputs` only (the directory exists
   before the first step, so nothing else can be known): an absolute path;
4. `sven-harness-<run id>` under the system temporary directory.

The directory is created with mode 0700 if it is missing. A path that is not
absolute, that contains `..`, that lies inside the workspace (symbolic links
resolved) or that holds the workspace is refused before the run starts: the
agent can write the workspace, so a directory there would let it change what
its own checks read. The directory is kept after the run; the CLI prints
`scratchpad: <dir>` on stderr (a `scratchpad` field with `--json`). A workflow
that reads `run.scratchpad` fails with `scratchpad_unavailable` under a host that
provides none (a delegated child run has none of its own). `run.scratchpad` is
also allowed in a file check's path, but a check resolves paths in the workspace
and refuses one outside it.

### The `gate` step

A primitive of the step specification that only the lowering writes: `gate: {command, timeout, cwd, store_as, then, on_fail}`. It
calls the reserved tool `shell_gate` (`sven_harness_spec::GATE_TOOL`; capability
`ExecuteShell`, so host approval policy applies) with `{steps, timeout_secs, cwd,
env, stdin}`: `steps` is a list of `{name, argv}` (a `command` gate has one step,
named for its program; a script gate's argv is the shell and the script), `env`
holds the gate's variables and, when the run has a scratchpad,
`SVEN_SCRATCHPAD`. The tool, `sven_tools_exec::GateCommandTool`, runs the steps in order, each
in a new session, confined to the workspace, under one deadline for the whole gate,
stops at the first step that does not exit 0, kills the whole process group at the
deadline, and returns JSON `{exit_code, timed_out, error, stdout, stderr,
truncated, duration_ms, failed_step, steps}` (the last 16 KiB of each stream): the
first five fields are those of the step that ended the gate (the last one when all
passed), `failed_step` names it (null when every step passed) and `steps` lists
`{name, exit_code, duration_ms}` for each step that ran. No agent mode lists
the tool and the runtime refuses an agent step's proposed call to it. The machine
stores `{passed, kind, exit_code, timed_out, stdout, stderr, duration_ms,
detail, failed_step, steps, json}` under the step (`gates.<name>.failed_step`,
and `failure.failed_step` in a repair prompt) (`kind` is `passed`, `failed`, `timeout` or `exec_error`),
records a `GateEvaluated` trace record, and continues at `then` on a pass and
`on_fail` otherwise. A host that does not provide the tool fails every gate
closed. A gate interrupted by a suspension is run again on resume and costs
neither a repair attempt nor a step.

### The `iterate` step

A primitive of the step specification that only the lowering writes: `iterate: {for_each | until, body, then, on_exhausted, store_as}`. The
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
again on resume.

A `use` task lowers to a `delegate` with `propagate_failure`: a child that
returns `ok: false` fails the parent step with `child_failed` instead of being
stored and continued past.

**Trajectory.** `HarnessRun::trajectory` holds every conversation of the run
as its own embedded trajectory, starting with its system message as a `system`
step (`extra.harness.conversation` names it, and `extra.harness.conversations`
lists them in the order each began), so no task's exchange is mixed into
another's; `HarnessRun::conversations()` returns them as `(name, trajectory)`
pairs. A response's reasoning is the `reasoning_content` of its step; it is
recorded only, and no later request carries it.

**Workspace per run, tools on every host.** `HarnessRun::in_workspace(dir)` roots
one run's file, search and shell tools, its gates and its snapshot reads in
`dir`, so one engine serves many concurrent runs, each in a tree the host laid
out and reads afterwards. `shell_gate` and `read_json_snapshot` are registered by
the SDK for every harness run, confined to the run's workspace, whatever toolset
the engine has; a host tool of the same name (a fixture) replaces them.

## Runtime

`HarnessMachine` is one `Machine` with a fixed hierarchy of ten states,
whatever the plan's size; the current step and every result live in a durable
context fact, so a run snapshots and restores like any kernel machine.

| State | The run is |
|-------|------------|
| `Ready` | created, waiting for the message that starts it |
| `Advancing` | working off pure steps (`assign`, `branch`, `return`, `fail`) |
| `WaitingForModel` | in a `predict` or `agent` turn |
| `WaitingForTool` | waiting for a tool result (also while a person is asked to approve it) |
| `WaitingForVerifier` | waiting for a verdict |
| `WaitingForChild` | in a `delegate` |
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

`delegate` starts a child run through the kernel's child spawner (a `use` task
lowers to one). A child gets its own tools, executor and cancel scope; it can
verify and delegate in turn within `delegate_depth`, because the spawn
descriptor carries the child's depth. A child that ends in `fail` does not fail
its parent by itself: its result is the envelope `{ok: false, code, message,
usage}` and the parent step stores it like any value, or fails with
`child_failed` when it propagates failure, as a `use` task does. Cancelling a
run cancels its children, and a run's tool calls end with it (the executors are
bound to the run's cancel scope, so a call that outlives its `execute` is
dropped when the run ends).

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
   never grants. `sven harness run` sets it from
   `--allow-capability`. Without a ceiling the declaration is the whole cap.

`delegate` needs `SpawnChild`, and a child's depth is bounded by
`execution.limits.delegate_depth`.

### Observability

The run keeps a trace of `TraceRecord`s: `StepStarted`,
`StepCompleted`, `BranchSelected`, `Verified`, `ChildStarted`,
`ChildCompleted`, `GateEvaluated`, `TurnRetried`, the item records of loops,
`Failed` and `Outcome`. Each is
published as a `HarnessEvent` session event (`Started`, `StepStarted`,
`StepCompleted`, `BranchSelected`, `ChildStarted`, `ChildCompleted`,
`Verified`, `BudgetExhausted`, `Failed`, `Completed`). The headless runner prints them as `[sven:harness] ...` lines;
ACP does not forward them.

**In the author's terms.** A workflow's generated steps are named in its plan's
`source_map`. `sourced_trace(plan, trace)` (and `SourceTracker`, for a stream)
returns each record with a `source` object: `task`, `role` (`work`, `check`,
`repair`, ...), `gate`, `attempt` (the verification attempt, 0 the first),
`item_id` (the `for_each` item whose body the record belongs to) and `result`
(`passed`, `failed` or `completed`, for a gate, a verifier, a child or a
finished item). The record's own fields are unchanged. `sven harness run`
prints and writes (`--trace`) records this way.

**Timestamps.** The machine is clock-free, so a record does not say when it
happened. The CLI adds `"ts_ms"` to every record it writes (`--trace`) or prints
(`[sven:harness]`): the wall clock, in milliseconds since the Unix epoch, at the
moment it observed the record, so per-step and per-item timings are differences
between stamps. A run that resumes a checkpoint stamps the records the
checkpoint carried with the time the resume began. `harness replay
--expect-trace` ignores the stamp.

## Checkpoints, recordings and replay

**Checkpoint.** A run stopped by a bound (`RunConclusion::Timeout` or
`BudgetExhausted`) leaves a `HarnessCheckpoint`: the kernel snapshot, the plan
digest, the bundle digest and the run id. It is `Serialize`/`Deserialize`, so
it can be stored as JSON and resumed in another process with
`Harness::resume`. A checkpoint under another plan or bundle is refused. The
operation the stop interrupted did not complete; on resume it starts again
(a gate, a model turn or a verification costs no repair attempt). A resumed run keeps the run id and continues the trace. The restored machine
re-emits nothing for an operation in flight (model turn, tool call, child
batch, approval question), and a completion it has already consumed - a
repeated tool result, model turn, approval or branch completion - settles
nothing a second time; `harness_checkpoint_interruptions.rs` holds these
cases for each waiting state.

**Item checkpoints.** A host that must survive a kill it cannot catch (an OOM
kill, a power cut) asks for a checkpoint each time a `for_each` item completes:
`HarnessRun::on_item_checkpoint(sink)`. The run is tapped after every dispatch
(`ErasedRuntime::snapshots`), so the checkpoint arrives while the next item is
already underway, not when the run next comes to rest; it is a checkpoint like
the one a deadline leaves, resumable with `Harness::resume`, and the item in
flight starts its interrupted step again. `sven harness run
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

**Trajectory.** `HarnessRun::trajectory()` exports the run's conversations
(the shared plan thread's messages, then each named conversation embedded) as
an ATIF trajectory, with the harness name, digests, run id and trace under
`extra.harness` and the token usage and step count in the final metrics.
Children's conversations are not included.

**Counts.** `HarnessOutcome::step_counts()` and `HarnessReplay::step_counts()`
count `StepStarted` per step.

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
| `Engine::load_harness(path)`, `load_harness_from_str(yaml)` | parse, validate, compile offline and register under the plan's name; a document or a bundle (top-level `document` and `imports`); a duplicate name is refused |
| `Harness::{name, digest, plan, validate_inputs}` | the compiled plan (`sven_sdk::harness_plan`) and an input check |
| `Harness::start(inputs)` | a `HarnessRun`; inputs are checked against the plan first |
| `Harness::resume(checkpoint)` | continue a checkpoint under the same program |
| `Harness::replay(&recording)` | offline replay |
| `HarnessRun::execute(RunOptions)` | run to a verdict or a bound; once per run |
| `RunOptions::{cancel, deadline, max_output_tokens}` | cancellation is a `CancelToken`; a bound suspends |
| `HarnessRun::{events, subscribe_trace, run_id, checkpoint, with_recording, recording, trajectory, conversations}` | observation and export |
| `HarnessRun::on_item_checkpoint(sink)` | a checkpoint each time a `for_each` item completes |
| `HarnessRun::{with_scratchpad, scratchpad}` | choose the run's private directory; read it once the run executed |
| `HarnessOutcome { conclusion, result, usage, trace, steps }` | `result` is the plan's verdict, `None` when a bound stopped the run first |
| `TraceRecord`, `HarnessEvent`, `harness_plan::*` | re-exported so an application need not name the crates behind the facade |

Many runs can be in flight through one engine; they share the model client
and the registry and nothing else.

## Testing

The engine is tested where it lives, with models written in Rust:
`sven-harness-spec` (parsing, validation, lowering, plan digests),
`sven-harness-runtime` (the machine, with plans compiled through the `testing`
feature) and `sven-sdk` (runs, replay, authorization, children, item
checkpoints and a thousand-run concurrent stress test) script the model and
the tools they need. The command line is tested against the real binary and a
loopback chat-completions server that plays a script, so a run's model is
always one the test controls (`make test/harness/cli`). `make test/harness`
runs all of these and reaches no live model.

A harness itself is observed by running it: `sven harness run` with
`--trace`, `--record` and `--trajectory` shows what the run did, and
`sven harness replay` re-derives its verdict and trace offline from the
recording. The samples are data directories; the repository's `harness_cli`
test checks that every one of them validates.
