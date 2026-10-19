<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Programmable harnesses

A harness is an agent program in YAML that compiles through the HSM kernel.
This page is the reference: the language, the runtime states, the
authorization model, checkpoints, recordings, trajectories and the SDK API.
For the commands see [Harnesses](../guides/harnesses.md); for the
reasons behind the design see [ADR 0004](../adr/0004-programmable-harnesses.md).

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
| `api_version` | required, `"1"` is the only supported value |
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
trap or schema failure), the four bounds above, and `cancelled`. A failed step inside a
`retry` frame re-enters the body while attempts remain, then continues at
`on_exhausted`; outside one it fails the run. A body that succeeds ends the
frame and the walk follows the body's own `then`. Every attempt is a
`StepStarted` with its attempt number.

### Digests

The compiler produces a canonical plan whose digest does not depend on key
order or whitespace and changes when the program's meaning does. A bundle's
digest additionally covers the helper bytes. Runs, checkpoints and
recordings pin these digests, and a mismatch is refused, never reinterpreted.

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
