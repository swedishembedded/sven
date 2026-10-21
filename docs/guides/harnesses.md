<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Harnesses

A **harness** is an agent workflow written as data: a YAML file that declares
tasks, the trusted gates that decide whether each one succeeded, how failures
are repaired and what counts as success. Sven compiles it offline, pins it by a
content digest and runs it on the same kernel as every other agent, so the
permission gate, approvals, cancellation and tracing all apply unchanged.

`sven harness` validates, inspects, compiles, explains, runs, resumes and
replays harnesses. Reading a harness needs no model, network or configuration
file; a run uses a model server you name and reads no configuration. This page is the
user guide; the language, the runtime and the SDK are in
[Programmable harnesses](../reference/harnesses.md), and the design
decisions in [ADR 0004](../adr/0004-programmable-harnesses.md) and
[ADR 0005](../adr/0005-declarative-harness-v2.md). The workflow language is
the only harness language; the earlier step-map language was removed
([ADR 0006](../adr/0006-v2-is-the-only-harness-language.md)).

## A first harness

One schema-constrained model turn, then a second turn only for bugs
([samples/harnesses/ticket-routing](../../samples/harnesses/ticket-routing)):

```yaml
api_version: "2"
name: model-routing
inputs: { ticket: string }
outputs:
  label: { type: string, value: "{{ tasks.classify.output.label }}" }
  summary: { type: string, value: "{{ tasks.summarize.output }}" }
execution: { limits: { model_turns: 3 } }
workflow:
  - name: classify
    model_only: true
    schema:
      type: object
      required: [label]
      properties: { label: { type: string, enum: [bug, feature, question] } }
    task: "Classify as bug, feature or question: {{ inputs.ticket }}"
  - name: summarize
    model_only: true
    when: { eq: tasks.classify.output.label, value: bug }
    task: "Write a one-line summary of this bug for the bugs queue: {{ inputs.ticket }}"
```

A task is a prompt plus what is checked afterwards; `model_only` makes it one
model turn without tools, `schema` makes the answer JSON of that shape that
later templates read, and `when` skips a task. The model fills values; it never
decides control flow.

```sh
$ sven harness validate samples/harnesses/ticket-routing
OK samples/harnesses/ticket-routing (model-routing)
$ sven harness run samples/harnesses/ticket-routing --workspace . \
    --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
    --approvals auto --input '{"ticket": "The app crashes on start"}'
```

## A workflow: tasks, gates and repair

For a job ("make this change and prove it works") you write the job, not the
steps: tasks and the checks that decide them, which sven lowers into the
machine's steps for you
([samples/harnesses/verified-task](../../samples/harnesses/verified-task)):

```yaml
api_version: "2"
name: verified-task
inputs:
  feature: { type: string, description: "What to implement." }
agent:
  profile: coding
  tools: [read_file, edit_file]
execution:
  limits: { model_turns: 8, tool_calls: 12 }
gates:
  test:
    run: sh run_tests.sh
    timeout: 60
  coverage:
    run: sh run_tests.sh --coverage
    metrics:
      line_pct: { file: coverage.json, pointer: /line_pct, type: number }
workflow:
  - name: implement
    task: "Implement: {{ inputs.feature }}. Edit the source so that the tests pass."
    verify: [test]
    repair: { attempts: 1 }
success:
  gates: [test]
  metrics: { coverage.line_pct: { gte: 80 } }
```

What this means, and why it can be trusted:

- The agent works on the task. Then the **gate** `test` runs: a script (or a
  `command:` argv, which runs without a shell) whose exit status is the verdict. An agent that says "all tests pass" is
  not believed, and neither is a program that prints it and exits 1.
- A failed gate earns up to `attempts` **repair turns**. The agent is shown what
  failed (exit status, the end of stdout and stderr) and the checks run again.
  When the attempts are spent the run fails with `gate_failed` and a message
  naming the task and the check. Nothing advances past a check that did not pass.
- After the last task the **success** condition runs from scratch, including the
  coverage command, and compares the number that run just wrote.

Everything is declared up front, so `sven harness validate` fails with the task
or gate at fault (a dangling gate, a duplicate name, an unknown metric, a
capability the declared list misses, a template that reads a task that has not
run), and `sven harness inspect` lists the generated steps. A script the gate
runs *inside the repository* is as editable by the agent as the code under test:
pin it with an `{artifact: ..., sha256: ...}` check, or keep it outside the
workspace.

Roles are tasks too, and every task works in a conversation of its own: it
reads no other task's history, only what templates hand it
(`{{ tasks.plan.output }}`). A `model_only` task is one model turn without
tools, for a planner or a supervisor; give it a `schema` and later tasks
template the fields of its answer. `sampling`, `thinking` and `turn`
settings (temperature, seed, thinking on or off and its token budget, the most
output tokens, the wait for the first response) go on the agent for every task or
on one task, and the trajectory records what each request was sent with
([samples/harnesses/planner-worker](../../samples/harnesses/planner-worker)).

Tasks share what they learn through **variables**: declare
`variables: {checks: {type: json}}`, let an exploration task set it
(`vars: {checks: required}` - its agent gets `harness_vars_set` and is reminded
if it forgets), and read `{{ vars.checks.test }}` in later prompts and gates;
a task sees only the variables it names, and `read` keeps it from changing them.

For work that has to be done many times, a task can loop (and with `do:` a
loop runs several roles per item: an auditor that only reads, a fixer that
edits, a reviewer, then a check task - `verify` and no prompt - that commits,
each in a fresh conversation for every item). `for_each` runs it
once per item of a collection the agent (or the caller) produced, and
[samples/harnesses/module-sweep](../../samples/harnesses/module-sweep) shows the
shape: a planning task leaves `backlog.json`, a `for_each` task reads it **once**,
freezes the list and works through it, each module verified before the next
begins (editing the file mid-run changes nothing), and an `until` task repeats
until a freshly measured coverage figure reaches a threshold or its passes run
out. The run state records every item, so a suspended run resumes at the module
it was on; the finished ones are not run again.

Every `sven harness run` provides the gate tool (`shell_gate`), confined to the
workspace; an SDK host registers it the same way. The full language is in the
[reference](../reference/harnesses.md#the-language).

## Harness directories

A path argument is a harness file, or a directory:

```text
NN-name/
  harness.yaml     the main document
  lib/*.yaml       workflows it imports (`imports:`), any path inside the directory
  fixtures/        input repositories and other files a live run uses
```

A main document with `imports` is a package: it is pinned by one digest that
covers every imported workflow ([reviewed-change](../../samples/harnesses/reviewed-change)).

## Commands

```text
sven harness validate <PATH>... [--json]
sven harness inspect  <PATH> [--json | --graph dot|mermaid]
sven harness compile  <PATH> [--format json|yaml] [-o FILE]
sven harness explain  <PATH> [--step ID] [--input JSON | --input-file FILE] [--elide N]
sven harness status   <PATH> --checkpoint FILE [--json]
sven harness schema   [<PATH>]
sven harness run      <PATH> [--input JSON | --input-file FILE] --workspace DIR
                      --model SPEC --model-endpoint URL --approvals auto|deny|ask
                      [--allow-capability NAME]... [--api-key-file FILE]
                      [--first-byte-timeout SECS] [--timeout SECS] [--max-output-tokens N]
                      [--scratchpad DIR] [--trace FILE] [--record FILE]
                      [--trajectory FILE] [--conversations DIR]
                      [--checkpoint-out FILE] [--json] [--quiet]
sven harness resume   <PATH> --checkpoint FILE [the flags of run]
sven harness replay   <PATH> --recording FILE [--expect-trace FILE] [--json]
```

- **validate** compiles the main document and its imports; nothing runs. It
  reports every error with the task or gate it belongs to.
- **inspect** shows the generated steps and their successors, the declared
  capabilities, limits, inputs and outputs, the plan digest (and bundle
  digest) and imports.
- **schema** prints the JSON Schema of harness documents (for editors); with
  a PATH it prints that harness's input/output contract as JSON Schema.
- **run** executes once on a real model. `--input` or `--input-file` gives the
  inputs. `--workspace` is an existing directory used as the project root,
  `--model` and `--model-endpoint` name the model server you already run, and
  `--approvals` says who is asked before a call that is not read-only runs.
  All four are required: nothing that widens a run is implied.
- **resume** continues a suspended run from the checkpoint `run
  --checkpoint-out` wrote. The checkpoint carries the inputs; a checkpoint
  taken under another program, or a changed import, is refused.
- **replay** feeds a recording back into a fresh machine and executes
  nothing. With `--expect-trace` the replayed trace must equal a trace file.
`--timeout` and `--max-output-tokens` do not fail a run: they **suspend** it
and leave a checkpoint.

### Reading a harness and a run

None of these starts a model.

- `compile` prints the canonical plan the digest is taken over, as JSON or
  YAML (`--format`, `-o FILE`). The plan carries the
  `profile` and the `source_map`.
- `inspect --graph dot|mermaid` draws the plan. The generated steps
  are grouped into one cluster (Graphviz) or subgraph (Mermaid) per task and
  captioned by role, check and attempt; edges say which outcome takes them
  (`passes`, `fails`, `true`, `false`). Render with `dot -Tsvg` or paste the
  Mermaid text into any Mermaid renderer.
- `explain` describes a workflow task by task: its work turn, checks, repair
  attempts and the success condition. `explain --step ID` gives one generated
  step's task, check and attempt, what it does, where it leads and from where
  it is reached. With `--input` or `--input-file` it also prints every prompt,
  system message and gate command as a run with those inputs would send it
  (substituted values are data; what a run only learns later, such as a task's
  output, stays a visible `{{...}}` hole), each cut to `--elide` characters, so
  a workflow can be edited and read without a model.
- `status --checkpoint FILE` says where a suspended run rests (step, task,
  check, attempt), its counters, the steps it completed, the progress of
  `for_each` loops, and whether `resume` would accept the checkpoint under the
  document as it is now (exit 1 when an edit changed the plan digest).

### The run's world

`run` and `resume` use sven's real tools (file read, write and edit, search,
shell) on a real directory with a real model. Nothing that widens a run is
implied, so the flags below are all required except the last three; a missing
one is exit 2:

```sh
sven harness run ./c-repair --workspace ./repo \
  --approvals auto --model openai/<name> --model-endpoint http://127.0.0.1:8788/v1 \
  --api-key-file ~/keys.json
```

- `--workspace DIR` is where the file tools are confined and the shell
  starts. A shell is not a sandbox: a command can still reach outside the
  directory; run the whole command in a container when that matters. The
  run writes nothing of its own there: its audit log is
  `<scratchpad>/audit.jsonl`.
- `--scratchpad DIR` is the run's private directory (mode 0700, created if
  missing; kept after the run and printed as `scratchpad: DIR`, a
  `scratchpad` field with `--json`). Without the flag a workflow's
  `execution.scratchpad` is used, else `sven-harness-<run id>` under the
  temporary directory; a resume goes back to the directory its checkpoint
  recorded. A directory inside `--workspace`, or one that holds it, is
  refused.
- `--model` and `--model-endpoint` name a server you already run; sven never
  starts or pulls a model, and no environment variable supplies an endpoint.
- `--approvals` is `auto` (allowed calls run unasked), `deny` (every call
  that is not read-only is refused, including a harness's own
  gate steps, so a gate step fails the run rather than passing without running)
  or `ask` (each such call is put to you on the terminal; end of input
  refuses). There is no default.
- Three caps bound the agent, and it holds a capability only when all agree:
  the capabilities the harness declares, the `tools` list of the agent step,
  and the host ceiling `--allow-capability NAME` (repeatable). Without the
  flag the ceiling is ReadFile, WriteFile, DeleteFile, ExecuteShell,
  RunVerifier and SpawnChild: never NetworkAccess, GitOperation or
  ControlDevice. A ceiling never grants what the harness did not declare.
- `--api-key-file` holds the server's key as `{"<provider>": "<key>"}` or
  the bare key. It is handed to the model client only, so no trace,
  recording, checkpoint or audit record carries it. A file inside the
  workspace is refused, because the agent could read it.
- `--checkpoint-out FILE` also keeps the latest checkpoint while a `for_each`
  sweep goes (written atomically after each completed item, removed when the
  run ends with a verdict), so a run killed without warning resumes with
  `harness resume --checkpoint FILE` at the item it was on.
- `--first-byte-timeout SECS` raises how long a model call waits for the
  server's first bytes, for a server that queues requests.

Progress lines go to stderr as the run goes: the trace records of finished
steps, and `tool_started` / `tool_finished` lines (the tool, its arguments cut
to 160 characters, and whether it failed) while a long agent step is still
working. Verification is not the agent's:
declare it as gates, pin the gate files with `artifact` gates that carry a
`sha256` so the agent cannot edit them, and list them under the task's
`verify`. A script inside the editable repository is a weak gate on its own.
`tests/fixtures/harness/c-repair` is a working example: the agent
writes a C test, builds it with the real compiler and fixes the library, and
the run completes only when the pinned gate (which exits non-zero unless the
compiler and its test succeed) passes.

### Output

`run` prints `status:`, then `output:` or `failure: <code> - <message>`, then
`steps:` on stdout. Unless `--quiet`, a `[sven:harness] {...}` line per trace
record goes to stderr (the same token the headless runner uses) as the run
goes. `--json` replaces the stdout text with one JSON document:

```text
harness, plan_digest, bundle_digest, run_id, status, conclusion, output,
failure, usage {input_tokens, output_tokens}, steps, step_counts,
duration_ms, corrections, refused_messages, checkpoint
```

`status` is `completed`, `failed`, `cancelled` or `suspended`. A suspended run
without `--checkpoint-out` prints a note saying its checkpoint was dropped.
SIGINT and SIGTERM suspend a run the same way: the run stops
where it rests, exits 3 and leaves its checkpoint for `harness resume`.
`validate` prints `OK`/`INVALID` lines; `--json` prints one array of reports.

### Exit codes

| Command | 0 | 1 | 2 | 3 | 4 | 5 |
|---------|---|---|---|---|---|---|
| `run`, `resume` | the workflow returned | the workflow failed | usage, invalid harness, input or file | suspended by a bound | cancelled | infrastructure |
| `validate`, `inspect`, `compile` | valid | invalid | unreadable path | | | |
| `explain` | explained | | unknown step, unreadable path | | | |
| `status` | the checkpoint fits the document | `resume` would refuse the checkpoint | not a checkpoint, unreadable path | | | |
| `replay` | matched | trace mismatch | recording refused or usage | | | |

### Files written

Nothing is written unless a flag asks for it: `--trace` (one trace record per
line, JSONL, each with `ts_ms`, the Unix time in milliseconds at which sven
observed it), `--record` (the recording, JSON), `--trajectory` (the ATIF
trajectory, JSON), `--conversations DIR` (each conversation of the run - one
task's agent, or one role of one loop item - as its own ATIF file
`NNNN-<name>.json`, the form tool-call training data wants) and
`--checkpoint-out` (the checkpoint of a suspended run, JSON). A run keeps its
scratchpad and prints the path. No sven configuration is read.

## Samples tour

[samples/harnesses](../../samples/harnesses) holds eleven workflows, each a data
directory with its README:

| Sample | What it shows |
|---|---|
| `ticket-routing` | schema-constrained model-only tasks and a `when` condition |
| `scoped-agent` | an imported workflow (`use:`) held to its own tools and capabilities |
| `repair-loop` | a task whose independent file check fails until a bounded repair makes it pass |
| `repo-repair` | the real shell and file tools on a repository: a soft baseline gate, a diagnosis, an edit, a gate that decides, one repair |
| `suspend-resume` | suspend on a bound, resume the checkpoint, replay a recording offline |
| `planner-worker` | a model-only planner answering in a schema, a tool-using worker and a model-only supervisor behind a gate, each with its own sampling and thinking settings |
| `reviewed-change` | a package: one workflow imports another (`imports:`) and runs it as a task (`use:`), the call checked at compile time and the child spending under the caller's limits |
| `module-sweep` | a `for_each` over a frozen backlog and an `until` on a coverage figure; a suspension resumes at the module it was on |
| `verified-task` | a command gate whose exit status decides, bounded repair, a coverage threshold in the success condition |
| `embedded-hardening` | the full pipeline on a disposable embedded C repository: bootstrap, plan, a checkpointed `for_each` over modules, coverage `until` 90 percent, architecture, production; real gcov and clang-tidy gates, a pinned check, a live-run recipe |
| `rust-refactor` | a reusable sweep over any Rust repository: variables published by an exploration task, a `do:` body per file in fresh conversations, trusted gates and a rollback |

```sh
sven harness validate samples/harnesses/*/   # every sample compiles
```

### Suspend, resume, replay

A run stopped by a bound (`--timeout`, `--max-output-tokens`) or by SIGINT or
SIGTERM is suspended, not lost. The flow of `suspend-resume`, from a shell:

```sh
H=samples/harnesses/suspend-resume
sven harness run    $H --workspace DIR --model ... --model-endpoint ... --approvals auto \
    --timeout 1 --record run.json --trace trace.jsonl --checkpoint-out checkpoint.json   # exit 3
sven harness status $H --checkpoint checkpoint.json     # where it rests
sven harness resume $H --checkpoint checkpoint.json --workspace DIR --model ... \
    --model-endpoint ... --approvals auto               # exit 0
sven harness replay $H --recording run.json --expect-trace trace.jsonl   # offline; exit 0
```

## Walkthrough: hardening an embedded repository

[samples/harnesses/embedded-hardening](../../samples/harnesses/embedded-hardening)
is the whole language on one job. `repo/` is a disposable firmware repository:
a ring buffer with an off-by-one, a CRC-8 with the wrong polynomial, a UART
sender that ignores partial writes, a host stand-in for the HAL, tests that
fail on purpose and a `.hardening/run.sh` with `build`, `test [module]`,
`coverage` (gcov through gcovr), `lint` (clang-tidy) and `layering` (the HAL is
reached from the UART module only). The workflow runs, in order:

1. **bootstrap**: read the repository and write down the toolchain. Verified
   by the build, the pins and an artifact.
2. **plan**: write `.hardening/backlog.json` and `plan.md`; nothing else.
3. **modules**: a `for_each` over the backlog, frozen when the loop begins,
   `checkpoint: each_item` and `covers` naming each module's files. Each module
   passes the build and its own tests before the next starts.
4. **coverage**: `until` the freshly measured line coverage reaches 90.
5. **architecture** and **production**: layering, lint and a changelog entry,
   then the success condition (build, test, lint, layering, coverage 90, every
   item completed) is checked from scratch.

```sh
sven harness validate samples/harnesses/embedded-hardening
```

The repository is tested too: `cargo test --test embedded_hardening_fixture`
shows that the workflow pins the hashes of the check and of the given tests,
and that the build refuses a weakened test.

Copy `repo/` somewhere you can lose, then:

```sh
sven harness run samples/harnesses/embedded-hardening \
  --workspace /path/to/copy --approvals auto \
  --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
  --api-key-file ~/keys.json --first-byte-timeout 900 --timeout 3000 \
  --checkpoint-out checkpoint.json --trace trace.jsonl --trajectory run.atif.json --json
# killed or out of time? resume at the module it was on:
sven harness resume samples/harnesses/embedded-hardening --checkpoint checkpoint.json \
  --workspace /path/to/copy --approvals auto --model ... --model-endpoint ...
```

For your own repository, keep the shape: a pinned check script that is the
only judge, the given tests pinned by a hash list, the agent confined to the
code and to new test files, and a success condition that re-measures
everything. The hashes in `harness.yaml` are those of the shipped files; change
the script and you re-pin it.

## Verification and the trust boundary

One rule: **a model never decides that something worked.** An agent may decide
it finished editing; whether the build passes, the tests pass or the coverage
is 90 percent is decided by a gate, which is a program sven runs.

- A **command gate** runs an argv with no shell (`command: [argv]`) or a
  script (`run:`), in the workspace (or in a directory a run input names,
  outside it: `cwd_input`), with a timeout, and only exit status 0 passes. What
  the program printed and what the agent said do not count. A timeout and a
  program that cannot start have failed.
- A **script gate** is one multiline `run: |` script, or a list of named
  steps, `run: [{name: build, script: make}, {name: test, script: make
  check}]`, run in order and stopping at the first failure; the result names
  it (`gates.<name>.failed_step`, and `{{ failure.failed_step }}` in a repair
  prompt). The shell is `bash -eo pipefail` unless `shell: sh` (`sh -e`). The
  script is a template, but text spliced into a script is code: hand data in
  through `env: {NAME: "{{ inputs.x }}"}` and read `"$NAME"`, as
  `SVEN_SCRATCHPAD` is handed to every gate. The timeout covers all steps.
- A **metric** is a number a command leaves in a JSON file. A check on it runs
  the command again first, every time; a figure from an earlier run is not
  evidence.
- An **artifact** gate checks that a file exists, is big enough and, with
  `sha256`, is exactly the bytes you pinned. Pin the scripts and tests that
  make the verdict. A script inside the repository the agent can edit is a weak
  gate on its own; this is how it becomes a strong one.
- The agent cannot call the gate machinery: `shell_gate`, `read_json_snapshot`
  and `digest_files` are reserved to the harness's own steps, and a proposed
  call to one is refused.
- **Gates can judge what the agent did.** `{{ run.trajectory_file }}` is the
  run's trajectory so far, written by the host in the run's scratchpad, outside
  the workspace, before the command starts; `stdin` carries a diff or an answer too large for an
  argument. A gate that reads either is only as honest as its program, so keep
  that program outside the agent's reach.
- **Substituted values are data.** Braces in an input, an output or a model's
  answer are never expanded again.
- **Limits are enforced by sven.** Model turns, tool calls, steps, deadlines
  and delegate depth are bounds of the plan; a child spends under its
  parent's. A tool call beyond a task's allowance is refused and ends the
  turn with a status (`tool_cap`), a reply cut off at the output limit stops
  with `output_truncated`, and neither fails the run silently or passes it.
- Capabilities, the agent's `tools` and the host's `--allow-capability` ceiling
  all have to agree before a call runs; `--approvals` decides who is asked.

Failures are explicit: a check that fails earns a repair turn while attempts
remain and then fails the run with a code (`gate_failed`, `until_exhausted`,
`success_check_failed`, `child_failed`, ...). `on_failure: return` turns a
failed check into a returned result with `status: failed` and the failure
instead, for a caller that wants the trajectory of an attempt that did not
succeed.

## Resuming a long run

Everything a run knows is in its run state, and a checkpoint is that state: a
run stopped by a deadline, a budget, SIGINT or SIGTERM, or one that was killed,
resumes where it stood.

- `--checkpoint-out FILE` keeps the latest checkpoint (atomically) and, for a
  loop marked `checkpoint: each_item`, a new one after every completed item.
  `harness resume PATH --checkpoint FILE` continues; the plan digest (and the
  bundle digest, with imports) must be the same, or it is refused
  before anything runs. `harness status --checkpoint FILE` says where the run
  stands and whether `resume` would accept it.
- Work in flight when the run stopped proves nothing. A model turn, a gate, a
  verification or a read that never answered is done again, without costing a
  repair attempt or a step. Completed items are not run again, and the
  collection is not read again.
- A `for_each` freezes its list when the loop begins. The agent editing the
  backlog file afterwards changes nothing. `reconcile: refresh` is the
  explicit alternative: after a resume the file is read again at the next item
  boundary and merged by item id, and a completed item whose content changed is
  pending again.
- `covers: [paths]` names the files an item is about. Their digest is
  recorded when it completes and compared after a resume; an item whose files
  changed, appeared or vanished runs again. A changed gate is a changed plan, so
  a checkpoint from the old one is refused rather than reinterpreted.
- `harness replay` re-dispatches a recording into a fresh machine and executes
  nothing; two replays are identical.

## Moving from the step-map language

The earlier language, a map of steps with a primitive each (`api_version:
"1"`), is gone: a document that says so is refused with an error that points
to [ADR 0006](../adr/0006-v2-is-the-only-harness-language.md). Rewrite by hand;
the jobs it was used for are tasks:

| In the step-map language you wrote | Now you write |
|---|---|
| an `agent` step, then a `tool` step running tests, a `verify`, a `branch` on the verdict | a task with `verify: [gate]` |
| a `retry` around the agent step with a repair prompt | `repair: {attempts, prompt}` |
| an `assign` and a `branch` per condition | `when:` on a task, a `follow_ups` entry |
| a loop built from `retry` and a counter | `for_each` (a frozen list, per-item state) or `until` (a measured metric) |
| a `predict` with a named schema | a `model_only` task with `schema:` |
| a `delegate` to a harness in `children/` | `imports:` and `use:` (a package, checked at compile time) |
| `parallel` branches | tasks in order; a `for_each` over items |
| a `transform` step running a WASM helper | a command gate, or a `model_only` task |
| hand-rolled success `verify` steps at the end | `success: {gates, metrics, work_items}` |

The `tool_calls` and `model_turns` limits are under `execution.limits`; a
gate's argv is part of the plan and cannot be redefined by the agent;
templates read `inputs.x` and `tasks.t.output` (never `facts` or `steps`); a
failing check never advances.

## Models: only the one you name

Nothing in `sven harness` starts, pulls, loads or contacts a model unless a
run is given one, because a local model can exhaust a machine.

- `validate`, `inspect`, `compile`, `explain`, `status`, `schema` and `replay`
  never contact a model.
- `run` and `resume` need `--model SPEC` and `--model-endpoint URL`, the URL of
  a server you already run. No environment variable supplies either. Sven
  never starts or downloads the model.
- `make test`, `make check` and `make test/harness` never reach a live model:
  the engine's tests script their models in Rust, and the command line's tests
  talk to a loopback server that plays a script.

## Authorization in one paragraph

A harness declares the capabilities it needs (or has them derived); a task or
gate that needs one the declared list lacks fails `validate`. At run time a child harness
holds only what both its parent and its own document declare, an `agent`
step's `tools` list is an enforced allowlist, host approval applies, and a
tool is held to the capability it really has. See
[authorization](../reference/harnesses.md#authorization) for the model.
