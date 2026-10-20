<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Harnesses

A **harness** is an agent program written as data: a YAML file whose steps are
model calls, tool calls, verifications, branches, retries, parallel fan-outs
and delegations to child harnesses. Sven compiles it offline, pins it by a
content digest and runs it on the same kernel as every other agent, so the
permission gate, approvals, cancellation and tracing all apply unchanged.

`sven harness` validates, inspects, runs, resumes, replays, tests and compares
harnesses without a model, a network or a configuration file. This page is the
user guide; the language, the runtime and the SDK are in
[Programmable harnesses](../reference/harnesses.md), and the design
decisions in [ADR 0004](../adr/0004-programmable-harnesses.md).

## A first harness

```yaml
api_version: "1"
name: decision
inputs: { plan: string, vip: boolean }
outputs: { tier: string, discount: number }
entry: top
steps:
  top:      { branch: { on: { all: [inputs.vip, { eq: inputs.plan, value: enterprise }] }, when_true: gold, when_false: middle } }
  middle:   { branch: { on: { any: [inputs.vip, { eq: inputs.plan, value: pro }] }, when_true: silver, when_false: standard } }
  gold:     { assign: { set: { tier: gold, discount: 20 }, then: finish } }
  silver:   { assign: { set: { tier: silver, discount: 10 }, then: finish } }
  standard: { assign: { set: { tier: standard, discount: 0 }, then: finish } }
  finish:   { return: { value: { tier: "{{ facts.tier }}", discount: "{{ facts.discount }}" } } }
```

This is [samples/harnesses/branching](../../samples/harnesses/branching). Each
step is a map with exactly one primitive key; `then` names the next step.

```sh
$ sven harness validate samples/harnesses/branching
OK samples/harnesses/branching (decision)
$ sven harness run samples/harnesses/branching --input '{"plan": "pro", "vip": false}' --quiet
status: completed
output: {"discount":10,"tier":"silver"}
steps: 4
```

## A workflow: tasks, gates and repair

A step map is the machine's language. For a job ("make this change and prove
it works") you rarely want to write steps; write the job. A document with
`api_version: "2"` declares tasks and the checks that decide them, and sven
lowers it into steps for you
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
  test: { command: [sh, run_tests.sh], timeout: 60 }
  coverage:
    command: [sh, run_tests.sh, --coverage]
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

- The agent works on the task. Then the **gate** `test` runs: a command, with no
  shell, whose exit status is the verdict. An agent that says "all tests pass" is
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

Roles are tasks too. A `model_only` task is one model turn without tools in a
conversation of its own, for a planner or a supervisor; give it a `schema` and
later tasks template the fields of its answer. `sampling`, `thinking` and `turn`
settings (temperature, seed, thinking on or off and its token budget, the most
output tokens, the wait for the first response) go on the agent for every task or
on one task, and the trajectory records what each request was sent with
([samples/harnesses/planner-worker](../../samples/harnesses/planner-worker)).

For work that has to be done many times, a task can loop. `for_each` runs it
once per item of a collection the agent (or the caller) produced, and
[samples/harnesses/module-sweep](../../samples/harnesses/module-sweep) shows the
shape: a planning task leaves `backlog.json`, a `for_each` task reads it **once**,
freezes the list and works through it, each module verified before the next
begins (editing the file mid-run changes nothing), and an `until` task repeats
until a freshly measured coverage figure reaches a threshold or its passes run
out. The run state records every item, so a suspended run resumes at the module
it was on; the finished ones are not run again.

`sven harness test` runs the sample on mock models, with a fixture standing in
for the `shell_gate` tool; a real run needs a host that provides the tool (the
SDK's `GateCommandTool`). The full language is in the
[reference](../reference/harnesses.md#workflows-api_version-2).

## Harness directories

A path argument is a harness file, or a directory:

```text
NN-name/
  harness.yaml     the main document (or bundle.yaml, never both)
  children/*.yaml  harnesses the main one delegates to, by name
  cases/*.yaml     test cases, one per file
  fixtures/        files a case refers to
```

`bundle.yaml` is a document plus its WASM helpers in one file (see
[custom-helper](../../samples/harnesses/custom-helper)); a bundle is
pinned by one digest covering both.

## Commands

```text
sven harness validate <PATH>... [--json]
sven harness inspect  <PATH> [--json | --graph dot|mermaid]
sven harness compile  <PATH> [--format json|yaml] [-o FILE]
sven harness explain  <PATH> [--step ID] [--input JSON | --input-file FILE] [--elide N]
sven harness status   <PATH> --checkpoint FILE [--json]
sven harness schema   [<PATH>]
sven harness run      <PATH> [--input JSON | --input-file FILE] [--case FILE]
                      [--model-fixture FILE] [--tool-fixture FILE]... [--workspace DIR]
                      [--model SPEC [--model-endpoint URL]] [--seed N]
                      [--toolset none | --toolset coding --approvals auto|deny|ask
                       [--allow-capability NAME]... [--api-key-file FILE]
                       [--first-byte-timeout SECS]]
                      [--timeout SECS] [--max-output-tokens N]
                      [--trace FILE] [--record FILE] [--trajectory FILE]
                      [--checkpoint-out FILE] [--json] [--quiet]
sven harness resume   <PATH> --checkpoint FILE [the flags of run]
sven harness replay   <PATH> --recording FILE [--expect-trace FILE] [--json]
sven harness test     <DIR>... [--case NAME] [--no-replay] [--json]
                      [--model SPEC --model-endpoint URL]
sven harness bundle   <DOCUMENT> --helper NAME=FILE.wasm... [-o FILE]
sven harness helper run <FILE.wasm | BUNDLE --helper NAME> --input JSON [--fuel N] [--max-pages N]
sven harness compare  <DIR_A> <DIR_B> --cases DIR [--fail-on-regression] [--json]
                      [--seed N]... [--bootstrap-seed N] [--model SPEC --model-endpoint URL]
```

- **validate** compiles the main document and its children; nothing runs. It
  reports every error with the step it belongs to.
- **inspect** shows the steps and their successors, the declared
  capabilities, limits, inputs and outputs, the plan digest (and bundle
  digest), helpers, children and cases.
- **schema** prints the JSON Schema of harness documents (for editors); with
  a PATH it prints that harness's input/output contract as JSON Schema.
- **run** executes once. `--input` or `--input-file` gives the inputs;
  `--case` takes input, model, tools, workspace and bounds from a case
  manifest, and any other flag overrides what it gives. `--model-fixture` is a
  model block (`sequence`, `rules`, `fail`) and `--tool-fixture` the `tools:`
  block of a case, both as YAML files. `--workspace` is an existing directory
  used as the project root; by default each run gets a fresh empty one.
  `--seed` selects the block a `by_seed` fixture model uses.
- **resume** continues a suspended run from the checkpoint `run
  --checkpoint-out` wrote. The checkpoint carries the inputs; a checkpoint
  taken under another program, or a changed bundle, is refused.
- **replay** feeds a recording back into a fresh machine and executes
  nothing. With `--expect-trace` the replayed trace must equal a trace file.
- **test** runs every case of each directory; **compare** runs one set of
  cases over two variants of a harness and reports what changed.
  With `--seed N` (repeatable) and `--bootstrap-seed N` the comparison is
  **paired**: both variants run every case under every seed, a case passes or
  not per seed, and the report gives each task's pass rates, the mean
  pass-rate change with a 95% bootstrap interval over tasks (reproducible
  from `--bootstrap-seed`), the tokens, steps and time each variant spent,
  and a verdict. `reject`: a task passes under fewer seeds than the baseline
  (also an invalid candidate); `promote`: no task regressed and the interval
  excludes zero; otherwise `inconclusive`. `--fail-on-regression` exits 1 on
  `reject`. A case skipped by either variant is listed and left out.

`--timeout` and `--max-output-tokens` do not fail a run: they **suspend** it
and leave a checkpoint.

### Reading a harness and a run

None of these starts a model; they work on version 1 plans and version 2
workflows alike.

- `compile` prints the canonical plan the digest is taken over, as JSON or
  YAML (`--format`, `-o FILE`). A workflow prints its lowered plan with the
  `profile` and the `source_map`.
- `inspect --graph dot|mermaid` draws the plan. A workflow's generated steps
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

### Real tools: `--toolset coding`

`--toolset none` (the default) runs on fixtures only. `--toolset coding` runs
sven's real tools (file read, write and edit, search, shell) on a real
directory with a real model. Nothing that widens a run is implied, so the
flags below are all required except the last three; a missing one is exit 2:

```sh
sven harness run ./c-repair --toolset coding --workspace ./repo \
  --approvals auto --model openai/<name> --model-endpoint http://127.0.0.1:8788/v1 \
  --api-key-file ~/keys.json
```

- `--workspace DIR` is where the file tools are confined and the shell
  starts. A shell is not a sandbox: a command can still reach outside the
  directory; run the whole command in a container when that matters. The
  run also writes `.sven/audit.jsonl` there.
- `--model` and `--model-endpoint` name a server you already run. The
  fixture flags (`--model-fixture`, `--tool-fixture`, `--case`) are refused.
- `--approvals` is `auto` (allowed calls run unasked), `deny` (every call
  that is not read-only is refused, including a harness's own `shell` tool
  steps, so a gate step fails the run rather than passing without running)
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
`tests/fixtures/harness/c-repair` is a working version 2 example: the agent
writes a C test, builds it with the real compiler and fixes the library, and
the run completes only when the pinned gate (which exits non-zero unless the
compiler and its test succeed) passes.

### Output

`run` prints `status:`, then `output:` or `failure: <code> - <message>`, then
`steps:` on stdout. Unless `--quiet`, a `[sven:harness] {...}` line per trace
record goes to stderr (the same token the headless runner uses): as the run
goes under `--toolset coding`, after the run ends on the fixture path. `--json` replaces the stdout text with one JSON document:

```text
harness, plan_digest, bundle_digest, run_id, status, conclusion, output,
failure, usage {input_tokens, output_tokens}, steps, step_counts,
duration_ms, corrections, refused_messages, checkpoint
```

`status` is `completed`, `failed`, `cancelled` or `suspended`. A suspended run
without `--checkpoint-out` prints a note saying its checkpoint was dropped.
SIGINT and SIGTERM suspend a `--toolset coding` run the same way: the run stops
where it rests, exits 3 and leaves its checkpoint for `harness resume`.
`test` and `validate` print `PASS`/`FAIL`/`SKIP` and `OK`/`INVALID` lines;
`--json` prints one array of reports.

### Exit codes

| Command | 0 | 1 | 2 | 3 | 4 | 5 |
|---------|---|---|---|---|---|---|
| `run`, `resume` | the plan returned | the plan `fail`ed | usage, invalid harness, input or file | suspended by a bound | cancelled | infrastructure |
| `validate`, `inspect`, `compile` | valid | invalid | unreadable path | | | |
| `explain` | explained | | unknown step, unreadable path | | | |
| `status` | the checkpoint fits the document | `resume` would refuse the checkpoint | not a checkpoint, unreadable path | | | |
| `test` | all passed or skipped | a case failed | usage, invalid harness or manifest | | | |
| `bundle` | bundle written | | usage, helper not usable, bundle does not compile | | | |
| `helper run` | output printed | helper broke a bound or its contract | usage, unusable module | | | |
| `compare` | report produced | regression (paired: `reject`) under `--fail-on-regression` | invalid variant or usage | | | |
| `replay` | matched | trace mismatch | recording refused or usage | | | |

### Files written

Nothing is written unless a flag asks for it: `--trace` (one trace record per
line, JSONL), `--record` (the recording, JSON), `--trajectory` (the ATIF
trajectory, JSON) and `--checkpoint-out` (the checkpoint of a suspended run,
JSON). Case workspaces are temporary directories removed with the run. No
sven configuration is read.

## Cases

A case file states what a harness must do. Keys are checked strictly: an
unknown key is an error, not an ignored typo.
The first block below lists every key once; it is a catalogue, not one
coherent case.

```yaml
name: bug                          # default: the file stem
input: { ticket: "The app crashes on start" }
model:                             # exactly one of sequence | rules | file | fail | by_seed | live
  sequence:                        # one item per model call, in order
    - text: '{"label": "bug"}'
      usage: { input: 20, output: 6 }
    - tool_calls: [ { id: c1, name: read_notes, args: { path: notes.md } } ]
  # rules: [ { match_type: contains, pattern: "history:", reply: "..." } ]
  # file: fixtures/model.yaml      # another model block, inside the directory
  # fail: "provider down"
  # by_seed: { 1: { sequence: [...] }, 2: { sequence: [...] } }  # one block per run --seed
  # live: { model: "ollama/qwen3:8b" }
tools:
  read_notes:
    capability: ReadFile           # must equal what the tool's name implies
    responses:                     # one per call, in order; running out is a case error
      - ok: "remember the milk"    # a string verbatim, anything else as JSON text
      - error: "disk full"
      - block: true                # never answers; ends by cancel or deadline
      - ok: "patched"
        write: { path: fixed.txt, content: "ok" }   # a file in the case workspace
workspace: { notes.md: "remember the milk" }
bounds: { deadline_ms: 300, max_output_tokens: 100, cancel_after_ms: 50 }
                                   # cancel_on_tool_start: <tool> cancels once that fixture tool has started a call
                                   # (no clock involved; prefer it to cancel_after_ms for blocking tools)
replay: true                       # default; record and replay offline, require the same result
expect:
  status: completed                # completed | failed | cancelled | suspended
  output: { queue: bugs }          # exact JSON equality
  output_includes: { queue: bugs } # recursive subset
  failure: { code: unroutable, message_contains: "valid route" }
  steps: { classify: 1, ask: 1 }   # exact start counts of the listed top-level steps
  max_steps: 6
  tools_invoked: [read_notes]      # sorted multiset of fixture calls, children included
  tools_not_invoked: [write_notes]
  forbidden_capabilities: [ExecuteShell, WriteFile]
  max_output_tokens: 100
```

Only `expect.status` is required. A case that stops on a bound and continues
from its checkpoint adds a `resume` phase, which needs `expect.status:
suspended` in the first phase (this is `suspend-resume`):

```yaml
bounds: { deadline_ms: 300 }
tools:
  read_report: { capability: ReadFile, responses: [ { block: true } ] }
expect: { status: suspended, steps: { call: 1 } }
resume:                            # a second phase on the checkpoint, with fresh fixtures
  tools:
    read_report: { capability: ReadFile, responses: [ { ok: "all green" } ] }
  expect: { status: completed, output: { report: "all green" }, steps: { call: 2 } }
```

Notes:

- `sequence` answers model calls in order and so suits one thread of control;
  a `parallel` step asks in no defined order, so use stateless `rules` there.
- A case without `model` has a model that only fails, which is how a
  model-free harness proves it never asks one. A sequence asked for more
  answers than it holds is a case error.
- Fixture tools are honest: the declared `capability` must equal the one the
  harness authorizes the tool's name under (`read_*` is `ReadFile`, `write_*`
  is `WriteFile`, `shell*` is `ExecuteShell`). A fixture that claims another
  capability has its calls refused, so the case fails with the tool never
  invoked. Files a case names (`model.file`) and files a tool writes are
  confined to the harness directory and the case workspace.
- After every case the runner checks that no fixture call is still in flight;
  a leaked call is a failed case.
- With replay on, each phase's recording is replayed offline and must end in
  the same result and trace.

A run that disagrees with a case is a question about which one is wrong. The
expectation is never loosened to make a run pass.

## Writing a helper

A `transform` step runs a helper: a WebAssembly module for the `helper-run/1`
ABI. It exports one memory and `run(input_ptr: i32, input_len: i32) -> i64`;
the host writes the input as JSON at `input_ptr` and the helper returns
`(out_ptr << 32) | out_len`, the range of its JSON output. The module imports
nothing (an import, a table, indirect calls, a start section or a second
memory is refused when the module is loaded), a fresh instance serves every
call, and each call is capped by fuel (`limits.max_helper_fuel`, one unit per
instruction) and memory (`limits.max_helper_pages`). Build the module with any
toolchain that emits such a module (for example a `no_std` Rust `cdylib` for
`wasm32-unknown-unknown`, or WebAssembly text assembled with a WAT assembler);
the loader says which feature it refuses if the module uses more than the
runtime implements.

The workflow, each step runnable on its own:

1. **Try the module** on inputs, under the bounds a harness would give it:
   `sven harness helper run reshape.wasm --input '{"customer": "acme"}' --fuel 100000`.
   Exit 0 prints the JSON output; exit 1 names the boundary that broke
   (`fuel`, `memory`, a trap, an output that is not JSON).
2. **Bundle** it with the document that uses it:
   `sven harness bundle document.yaml --helper reshape=reshape.wasm -o bundle.yaml`.
   The bundle is refused when a `transform` step names a helper you did not
   give, when you give one no step uses, or when a module does not load.
3. **Validate and test** the bundle directory like any harness:
   `sven harness validate DIR`, `sven harness test DIR` (cases state what the
   helper's output must make the harness do).

The [custom-helper](../../samples/harnesses/custom-helper) sample runs through
all three; its helper is the identity module, so
`sven harness helper run samples/harnesses/custom-helper/bundle.yaml --helper reshape --input '{"a": 1}'`
prints `{"a": 1}`. A changed helper is a changed program: the bundle digest,
and with it any checkpoint taken under the old bundle, changes.

## Samples tour

[samples/harnesses](../../samples/harnesses) holds thirteen harnesses, each a data
directory with its cases:

| Sample | What it shows |
|---|---|
| `branching` | `assign` and `branch` only: no model, no capability |
| `ticket-routing` | a schema-constrained `predict` inside a `retry`, then a branch; its `live-local` case is the opt-in real-model run |
| `scoped-agent` | delegation to a child; an `agent` step held to its tool list |
| `repair-loop` | `retry` around a failing tool, then an independent `verify` |
| `repo-repair` | the real shell and file tools on a repository: diagnose a failing test, edit, verify independently, retry once |
| `parallel-research` | `parallel` branches joined in declared order; cancellation |
| `suspend-resume` | suspend on a deadline, resume the checkpoint, replay offline |
| `custom-helper` | a sandboxed WASM helper carried in a bundle |
| `planner-worker` | a model-only planner answering in a schema, a tool-using worker and a model-only supervisor behind a gate, each with its own sampling and thinking settings |
| `reviewed-change` | a package: one workflow imports another (`imports:`) and runs it as a task (`use:`), the call checked at compile time and the child spending under the caller's limits |
| `module-sweep` | a v2 workflow with a `for_each` over a frozen backlog and an `until` on a coverage figure; a suspension resumes at the module it was on |
| `verified-task` | a v2 workflow: a command gate whose exit status decides, bounded repair, a coverage threshold in the success condition |
| `embedded-hardening` | the full pipeline on a disposable embedded C repository: bootstrap, plan, a checkpointed `for_each` over modules, coverage `until` 90 percent, architecture, production; real gcov and clang-tidy gates, a pinned check, a live-run recipe |
| `sdk-runner` | the same cases from an application, with the SDK alone |

```sh
sven harness test samples/harnesses/{branching,ticket-routing,scoped-agent,repair-loop,repo-repair,parallel-research,suspend-resume,custom-helper,verified-task,module-sweep,planner-worker,reviewed-change,embedded-hardening}   # every sample, offline
cargo run -p sample-harnesses-sdk-runner -- samples/harnesses/ticket-routing
```

### Suspend, resume, replay

The checkpoint flow of `suspend-resume`, from a shell. `blocked.yaml`
is a read that never answers and `answered.yaml` one that does:

```yaml
# blocked.yaml
read_report:
  capability: ReadFile
  responses: [ { block: true } ]
```

```yaml
# answered.yaml
read_report:
  capability: ReadFile
  responses: [ { ok: "all green" } ]
```

```sh
H=samples/harnesses/suspend-resume
sven harness run    $H --tool-fixture blocked.yaml --timeout 1 \
    --record run.json --trace trace.jsonl --checkpoint-out checkpoint.json   # exit 3
sven harness resume $H --checkpoint checkpoint.json --tool-fixture answered.yaml   # exit 0
sven harness replay $H --recording run.json --expect-trace trace.jsonl             # exit 0
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
sven harness test     samples/harnesses/embedded-hardening   # three mock cases, offline
```

The mock cases run the real workflow against fixture tools: a clean sweep, a
module that stays red (the run fails at that module and says which check), and
an agent that edits the check (its hash no longer matches). The repository is
tested too: `cargo test --test embedded_hardening_fixture` shows that every
module starts red and that the reference fix in `solution/` turns every gate
green with coverage over 90 percent.

**Production path.** Copy `repo/` somewhere you can lose, then:

```sh
sven harness run samples/harnesses/embedded-hardening --toolset coding \
  --workspace /path/to/copy --approvals auto \
  --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
  --api-key-file ~/keys.json --first-byte-timeout 900 --timeout 3000 \
  --checkpoint-out checkpoint.json --trace trace.jsonl --trajectory run.atif.json --json
# killed or out of time? resume at the module it was on:
sven harness resume samples/harnesses/embedded-hardening --checkpoint checkpoint.json \
  --toolset coding --workspace /path/to/copy --approvals auto --model ... --model-endpoint ...
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

- A **command gate** runs an argv with no shell, in the workspace (or in a
  directory a run input names, outside it: `cwd_input`), with a timeout, and
  only exit status 0 passes. What the program printed and what the agent said
  do not count. A timeout and a program that cannot start have failed.
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
  run's trajectory so far, written by the host outside the workspace before the
  command starts; `stdin` carries a diff or an answer too large for an
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
  bundle digest, with imports or helpers) must be the same, or it is refused
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

## Moving from version 1 to version 2

Version 1 documents keep working, byte for byte (`v1_compiled_plan_unchanged`
pins their digests). Version 2 is for jobs, not for control flow, and it lowers
onto the same machine: `harness inspect` lists the generated steps and the
source map says which task each belongs to. Rewrite by hand when a harness is a
job:

| In version 1 you wrote | In version 2 you write |
|---|---|
| an `agent` step, then a `tool` step running tests, a `verify`, a `branch` on the verdict | a task with `verify: [gate]` |
| a `retry` around the agent step with a repair prompt | `repair: {attempts, prompt}` (unrolled, not a retry) |
| an `assign` and a `branch` per condition | `when:` on a task, a `follow_ups` entry |
| a loop built from `retry` and a counter | `for_each` (a frozen list, per-item state) or `until` (a measured metric) |
| a `predict` with a named schema | a `model_only` task with `schema:` |
| a `delegate` to a harness in the directory | `imports:` and `use:` (a package, checked at compile time) |
| hand-rolled success `verify` steps at the end | `success: {gates, metrics, work_items}` |

What changes: the `tool_calls` and `model_turns` limits are under
`execution.limits`; a gate's argv is part of the plan and cannot be redefined by
the agent; templates read `inputs.x` and `tasks.t.output` (never `facts` or
`steps`); a failing check never advances. What does not: capabilities,
approvals, checkpoints, recordings, replay, traces, the SDK, `harness test`.
Step ids are stable across reformatting, so a v1 tool that reads a trace by step
id keeps working on the generated ones, and now also has the `source` of each.
A document is never retried as the other version: say `api_version: "2"`.

## Models: offline by default

Nothing in `sven harness` starts, pulls, loads or contacts a model unless you
opt in explicitly, because a local model can exhaust a machine and a test
suite must not depend on one.

- Every default path uses mock models (`sequence`, `rules`, `fail`) and
  fixture tools. A run with no model source has a model that fails every call.
- A **live** model needs the URL of a server you already run, given as
  `--model-endpoint URL` (with `--model SPEC` to choose the model) on
  `run`, `resume`, `test` or `compare`. No environment variable opts in. Sven
  never starts or downloads the model.
- Without `--model-endpoint`, `run --model SPEC` is refused (exit 2) and a
  case that names `live` is reported as `SKIP`, never failed.
- `make test`, `make check` and `make test/harness` never reach a live model.
  `make test/harness/local-model` runs the `live-local` case of
  [ticket-routing](../../samples/harnesses/ticket-routing) against `ENDPOINT=<url>` (optional `MODEL=<spec>`) and refuses to start
  without it.

## Authorization in one paragraph

A harness declares the capabilities it needs; a step whose primitive needs one
the harness does not declare fails `validate`. At run time a child harness
holds only what both its parent and its own document declare, an `agent`
step's `tools` list is an enforced allowlist, host approval applies, and a
tool is held to the capability it really has. See
[authorization](../reference/harnesses.md#authorization) for the model.
