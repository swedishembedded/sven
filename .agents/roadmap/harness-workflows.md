# harness-workflows

Make version 2 harnesses expressive enough for long, multi-agent sweeps over a
repository: a reusable "refactor every Rust file" sample is the driving case,
but every feature here is generic (any repository, any goal). Backward
compatibility is not a goal: samples and tests are updated with the language.

## Why (what the current language cannot express)

Grounded in the source at the start of this work (`fffb3551`):

1. **One conversation for every tool-using task.** The agent step's thread is
   always `harness_thread(plan)` (`harness-runtime/src/agent_loop.rs`,
   `exec.rs` passes `conversation: None`); only `model_only` tasks get their
   own thread, keyed by task name, so even those are shared across `for_each`
   items (`harness-spec/src/v2/lower.rs`). A sweep's 500th file is worked in
   the conversation of the 499 before it, a reviewer has read the author's
   reasoning, and the trajectory is one interleaved transcript.
2. **A `for_each` loops one task.** Several roles per item can only be
   `follow_ups` in the same conversation with one tool list, i.e. parts of one
   instruction delivered as separate messages.
3. **No run-owned place for working files.** Backlogs and notes have to live
   in the repository, and sven writes `.sven/audit.jsonl` into the workspace
   (`bootstrap/src/session_executor.rs`), which a `git add -A` commits.
4. **No shared, typed state between agents.** Task outputs are text (or a
   schema-checked JSON for `model_only`), immutable, unaddressable by an agent,
   and `agent.instructions`/`system` cannot read them. Nothing lets an
   exploration agent *publish* discovered settings (how to build, test,
   format) that later trusted checks then use, nor forces an agent to fill in
   a value it must produce.
5. **Command gates are argv lists.** Real checks are shell pipelines; writing
   them as `[sh, -c, ...]` or as a script inside the workspace (editable by
   the agent) is the only option.
6. **A failed item fails the whole run.** A sweep needs "roll this item back
   and go on".
7. **No timing in the trace** (the machine has no clock), so a run cannot be
   measured from its own records.

## Design

### D1. Conversations: one per task execution, by default
- `AgentStepSpec` and `PredictStepSpec` carry `conversation: Option<Template>`;
  the runtime resolves it when the step starts and uses
  `<harness>.<resolved>` as the thread (`TurnShape.conversation`,
  `AgentLoop.thread`).
- v2 lowering gives every task its own conversation: `<task>`, or
  `<task>#{{ item_index }}` inside a `for_each`, `<task>#{{ iteration }}`
  inside an `until`. Follow-ups, repairs, corrections and variable reminders
  continue in it. `conversation: <name>` on a task opts into a named shared
  conversation (two tasks naming the same one share it).
- `agent.system` and a task's `system` are templates over inputs, variables
  and earlier task outputs.
- The trajectory exports every conversation the run opened (enumerated from
  the store by thread prefix, not from the plan), each as its own embedded
  ATIF trajectory.

### D2. `do:` bodies (several tasks per item)
- A task may carry `do: [tasks...]` instead of `task:`; with `for_each` (or
  `until`) the body is the sequence of those tasks, each with its own prompt,
  tools, conversation, checks and repairs; `item`, `item_id`, `item_index`
  (or `iteration`) are in scope in all of them, and `tasks.<sub>.output` reads
  within the same body.
- A task without `task:` (and without `do:`) that has `verify` is a **check
  task**: its checks run with no model turn (begin/commit/rollback steps of a
  sweep are written this way).

### D3. Scratchpad
- Every run owns a private directory: the host's `--scratchpad DIR`, else the
  document's `execution.scratchpad` (a template over inputs), else
  `$TMPDIR/sven-harness-<run id>` created mode 0700. It may not be inside the
  workspace (or hold it). It survives suspension (the checkpoint records it),
  is kept after the run, and its path is printed.
- Templates read `{{ run.scratchpad }}`; command gates also get
  `SVEN_SCRATCHPAD` in their environment.
- A harness run's audit log goes to `<scratchpad>/audit.jsonl`, never into the
  workspace.

### D4. Variables
- Declared at the top level:
  `variables: { name: { type: string|number|boolean|json, scope: run|item, schema?, description? } }`.
  `item` scope is reset when each `for_each` item starts.
- Each task names what it may see: `vars: { name: read | write | required }`
  (`required` implies `write`). Nothing is visible that is not named.
- Three machine-answered tools are offered to every agent task that names any
  variable: `harness_vars_list` (name, type, access, required, set, value
  size), `harness_vars_get {name}`, `harness_vars_set {name, value}`. The
  machine answers them itself (pure, checkpointed, recorded, replayable);
  they reach the conversation through the generalised machine-answered call
  path (`TurnRequest.answered_calls`, which replaces `refused_calls`). A set
  is checked against access, type and schema shape; a refusal says why.
- **Required:** when a task's turns end and a `required` variable is unset,
  the agent is told in the same conversation, once per missing variable and at
  most twice (`origin: variables`), to set it; still unset, the run fails with
  `missing_variable` naming it.
- Templates read `{{ vars.<name> }}` (and fields of a json variable).
- A command gate may `capture: <variable>`: when it passes, its stdout
  (parsed as JSON for a `json` variable, trimmed text otherwise) is stored -
  a trusted, executor-computed value.
- **Trust rule (validator):** a variable expanded into a gate's *script* is
  code; it must not be writable by any task that comes after the first task
  checked by that gate. Discovery may publish commands; nobody after it can
  change them. Data reaches scripts through `env:` instead, never by text
  substitution.

### D5. Script gates
- `run: |` (one multiline script) or `run: [{name, script}, ...]` (steps run
  in order, the first failure stops), with `shell` (default `bash`, run as
  `bash -eo pipefail -c`), `env: {NAME: template}`, `timeout`, `cwd`,
  `capture`, `output_bytes`. `command: [argv]` stays for programs.
- The gate tool (`shell_gate`) takes `steps` and `env`; the result names the
  failed step and each step's duration.

### D6. Item failure
- `for_each: { on_item_failure: { verify: [checks...] } }`: when an item's
  check is exhausted, the listed checks run (e.g. a rollback script) and the
  sweep continues with the next item, which is recorded `failed`. Without it
  a failed item fails the run, as before.
- `success.work_items` takes `all_completed: true` or `all_settled: true`
  (completed or failed), and the loop summary is
  `{total, completed, failed, pending}`.

### D7. Measurement and training data
- The host stamps each trace line it writes with `ts_ms` (wall clock), so
  per-step and per-item timings come from the trace file.
- `--trajectory-dir DIR` writes one ATIF file per conversation (per item and
  role), opt-in; `--trajectory FILE` keeps the combined form.

## Phases (each a self-contained commit with tests)

1. D1 conversations (spec, lowering, runtime, trajectory export).
2. D2 `do:` bodies and check tasks.
3. D3 scratchpad and audit relocation; CLI `--scratchpad`.
4. D5 script gates.
5. D4 variables (+ `answered_calls`), `capture`, trust rule.
6. D6 item failure policy.
7. D7 trace timestamps, `--trajectory-dir`.
8. **v2 is the only document language** (user request, 2026-10-21): `api_version`
   "1" documents are no longer accepted; the step language stays only as the
   internal plan that v2 lowers to (not a document format), the v2 module
   becomes the crate's main path, legacy v1-only code is removed, and v1
   samples are rewritten in v2. v1-only features are inventoried first and each
   is ported to a v2 form or removed on purpose (parallel branches, WASM
   `transform` helpers and bundles, `delegate` to a machine mode, `retry`,
   standalone `predict` schemas).
9. Sample `samples/harnesses/rust-refactor/`: offline cases (fixture model,
   real gates on a fixture Cargo repo) in `make test/harness`.
10. Live proof: Qwen3.8-27B on brain (CUDA), the sample over a disposable
   worktree of sven, 10 files, metrics, commit audit, estimate for the whole
   repository. Findings go back into the engine.

## Acceptance
- Three distinct conversations per item in the trajectory; nothing the harness
  writes appears in the target repository's `git status` except the code it
  committed; a missing required variable produces its reminder and then
  `missing_variable`; a failing check blocks the commit and (with
  `on_item_failure`) rolls the item back; resume does not repeat completed
  items; every existing sample and test passes (updated where the language
  changed).
