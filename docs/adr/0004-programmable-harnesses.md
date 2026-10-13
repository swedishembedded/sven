# ADR 0004: Programmable harnesses — YAML agent definitions that compile through the HSM kernel

## Status

Accepted. Implemented by `crates/harness-spec` (the language and compiler) and
`crates/harness-runtime` (the machine that executes compiled plans).

## Context

ADR 0001 deleted the graph-machine DSL and recorded a revisit trigger: a
**shipping requirement for user-authored, non-Rust-authored control flow**.
That requirement has arrived. Teams building on the sven SDK want to define,
review, version, test and ship complete agent harnesses — multi-step programs
of model calls, tool calls, verifiers and child delegation — as data, without
recompiling sven or writing a Rust `Machine` impl for each one. The concrete
drivers:

- Harness authors are not necessarily Rust developers. They can write and
  review YAML; they cannot subclass `Machine`.
- A harness definition must be **a reviewable artifact**: diffable in git,
  hash-pinned at run time, validatable offline (`sven harness validate`) and
  testable standalone against mock models — none of which is true of a
  compiled `Machine` impl.
- The same harness definition must run unchanged against a local model, a
  mock model in CI and a frontier model in production, with authorization
  derived from the definition's declared capabilities intersected with host
  and parent policy.

ADR 0001's findings stand and this design is built to not repeat its failures.
The old DSL was deleted because it (a) had zero adoption, (b) was broken
(Latin-1 template decoding, guards that matched any completion, timers that
could never be cancelled, a referenced spawner that did not exist), and
(c) completing it meant a **second interpreter** with its own effect rendering,
policy derivation and retry semantics, drifting away from the HSM kernel it
paralleled. Point (c) is the one that kills naive designs, and it is why the
central decision below is about what this is *not*.

## Decision

Harness definitions are **programs that execute through sven's existing HSM
kernel**. There is no second execution engine, no second event vocabulary, no
second policy evaluator and no second tool-dispatch path. A YAML harness
document is parsed and compiled by `sven-harness-spec` into a deterministic,
content-hashed `HarnessPlan`; `sven-harness-runtime` implements one generic
`Machine` — `HarnessMachine` over a fixed `HarnessState` hierarchy — that
interprets the plan by emitting the kernel's existing typed `Effect`s and
consuming the kernel's existing `Event`s, exactly as the hardcoded machines
do. The kernel, the active-object runtime, the executors and the permission
checker are all reused untouched; a harness gains nothing at a trust boundary
that `sdlc` or `verified-task` does not already have.

### The two crates, and the dependency wall

- **`sven-harness-spec`** (foundation tier, zero sven dependencies): the YAML
  document format, its typed AST (`HarnessSpec`, `StepSpec` with twelve
  primitives), the expression/template system, static validation, and the
  compiler to `HarnessPlan` with a canonical content digest. It knows nothing
  about models, tools, the kernel, or execution — it is a pure data-plus-
  compiler crate, testable with no I/O.
- **`sven-harness-runtime`** (machines tier, depends on `sven-hsm` +
  `sven-vocab` only): `HarnessMachine`, durable run state, primitive dispatch
  to kernel effects, child-run contracts, correlation and outcomes. It is
  forbidden (by `architecture.toml`) from depending on any model, tool,
  turn or executor crate — the same `[[forbidden]]` wall sven-machines sits
  behind.

### The language

A harness document declares `api_version`, a name, an `entry` step, typed
`inputs`/`outputs`, required `capabilities` (tool-capability names from
`sven-hsm`), `limits`, and a step map. `StepSpec` is a closed enum of twelve
primitives: `predict` (one constrained model turn), `agent` (a tool-loop
turn), `tool` (one call), `verify` (a `VerifierSpec`), `assign`, `branch`,
`retry`, `parallel`, `delegate` (a child harness or registered machine),
`transform`, `return` and `fail`. Adding a thirteenth primitive is a
deliberate act across the full stack: typed schema, static validator, runtime
behavior, authorization rules, structured tracing, unit and integration tests,
and a runnable sample.

Expressions are a small, explicitly-bounded grammar over run inputs, step
results and the transcript — string interpolation (`{{ path.to.value }}`)
plus comparisons and boolean combinators — with no user-defined functions;
the `transform` primitive runs sandboxed WASM helpers for anything richer
(versioned ABI, no filesystem or network, fuel- and memory-capped).

### The fixed state hierarchy

The runtime state machine is **fixed**, not one state per YAML node. A step
id lives in the durable context; the states are
`Root / Ready / Advancing / WaitingForModel / WaitingForTool /
WaitingForVerifier / WaitingForChild / WaitingForJoin / Completed / Failed /
Cancelled` whether the plan has three steps or three hundred. This is what
keeps `HarnessMachine` a kernel citizen: `all_states()` is enumerable,
snapshots are restorable, and adding a step to a harness adds data, not
machine states. Pure primitives (`assign`, `branch`, `retry`, `return`,
`fail`) advance via internal events in a single dispatch; side-effecting
primitives emit kernel effects and wait for their completion events in a
later dispatch, which is the same discipline every existing machine follows.

### Determinism, digest and pinning

The compiler produces a canonical `HarnessPlan` whose digest is invariant to
YAML key order and whitespace and changes iff the program's meaning changes
(helper bytes included). A run pins its harness's digest; resuming against a
modified document is rejected rather than silently reinterpreted. Checkpoints
capture the machine snapshot plus plan digest plus attempt counters; replay
re-derives state from the event log with no side effects, and a
non-idempotent operation whose completion cannot be determined on resume
surfaces an explicit indeterminate outcome instead of re-running itself.

### Authorization

Registration becomes policy-bearing: `MachineRegistration { factory, policy }`
in the `ModeRegistry`, removing `mode_policy.rs`'s unknown-mode fallback to
the reactive policy — an unregistered mode is a construction error, never a
silent grant. A harness session's effective permissions are the intersection
`P_host ∩ P_machine ∩ P_harness ∩ P_parent`, evaluated against the plan's
declared capabilities; a harness may narrow what its delegate children inherit
and never widen it. This is the existing `ChildRunContract::narrow` path, not
a new one.

### What this deliberately is not

- Not a second engine: the kernel dispatches, executors perform I/O, the
  permission checker gates — all as before.
- Not a replacement for native Rust machines: `sdlc`, `verified-task`,
  `reactive_agent`, `predict` and friends stay; a harness *delegates* to them
  through the existing child-run mechanism.
- Not a general-purpose programming language: no loops beyond bounded retry,
  no unbounded recursion, no arbitrary code outside the WASM `transform`
  sandbox, no dynamic step dispatch — the compiler rejects unbounded cycles
  and dangling targets up front.
- Not an RL/evaluation substrate: no reward signal, no training loop.

## Consequences

- `ModeRegistry` grows policy metadata; every mode registration gains an
  explicit policy and the silent fallback dies. Existing modes keep their
  current policies exactly.
- `sven-vocab` grows `HarnessEvent` (`Started`, `StepStarted`,
  `StepCompleted`, `RetryScheduled`, `BranchSelected`, `ChildStarted`,
  `ChildCompleted`, `FanOut`, `Joined`, `Verified`, `BudgetExhausted`,
  `Failed`, `Completed`), carried by `SessionEvent::Harness`. The headless
  runner renders it and ACP does not forward it (see "As built").
- `Event::VerificationComplete` was expected to gain a correlation id before
  concurrent verifiers were allowed. It did not: see "As built".
- Every harness run is checkpoint-interruptible at step boundaries and resumable
  in a fresh process, which the SDK exposes as `Engine::load_harness` →
  `HarnessRun`.
- The graph-DSL failure modes are structurally excluded: templates are UTF-8
  correct by construction (typed AST, not byte casting), retry budgets are
  compiler-checked, timers do not exist, and the interpreter is exercised by
  the same mock-provider integration tests as the native machines.

## Revisit trigger

Revisit if a harness requirement cannot be expressed as one of the twelve
primitives composed with delegation and WASM transforms — the correct next
step is then a *typed* extension to the `StepSpec` enum plus its full-stack
checklist, never an interpreter escape hatch that bypasses the kernel's
effect, permission or audit planes.

## Addendum: as built

The design above shipped as described; this records what the implementation
settled, what it added beyond the text, and the decisions taken on the way.
The user guide is [docs/12-harnesses.md](../12-harnesses.md) and the reference
[docs/technical/harnesses.md](../technical/harnesses.md).

**Syntax.** A step is `id: { primitive: { ..., then: next } }`: exactly one
primitive key, its successor named by `then` (`branch` has `when_true` and
`when_false`, `retry` a `body` and `on_exhausted`, `parallel` a `join`). The
JSON Schema (`harness-document.schema.json`, printed by `sven harness
schema`) is lenient about unknown fields like serde, and stricter in a few
places; a test holds its primitive set to `PRIMITIVES`. Conditions have no
ordering comparison or arithmetic, so a threshold is a helper's business.

**Helpers are WASM only, carried in a bundle.** A bundle is the document plus
its helpers (`abi`, `language: wasm`, base64 content) under one digest.
Python and other process-isolated helpers are out of scope: they would need a
new spawn and sandbox executor and a host interpreter, and duplicate the WASM
path. Revisit if a requirement cannot be met by a fuel- and memory-capped
module.

**No correlation id on `VerificationComplete`.** Each parallel branch and
each delegated child is its own runtime (`spawn_child_run`) with its own
event sink and executor, and one machine has at most one verification in
flight, so a completion reaches exactly the machine that asked. The
`parallel_verifications_each_settle_their_own_branch` test proves the
routing. Revisit if a single machine ever holds two verifications in flight.
(The real defect was different: a child's executor had no verifier slot, so
any `verify` in a branch or child failed; it now has one, jailed to the
session's verify root.)

**Replay is over the recorded machine input log, not the trace.** A trace
holds no step values, so it cannot drive a machine. A `HarnessRecording`
is the events the machine was dispatched (user kickoff, model answers, tool
results, child verdicts, approvals), pinned to the plan and bundle digests;
`Harness::replay` re-dispatches them into a fresh machine, or into the
checkpoint a resumed run started from, and executes no effect. Approval ids
are derived from the call id so that a replay reproduces them. A checkpoint is
serializable JSON, which is what lets `sven harness resume` work across
processes.

**Authorization of a harness session** is the declared capabilities, bounded
by four rules the first implementation lacked:
- Host approval applies. A machine supplied to the builder used to skip the
  host's narrowing and the approval mode, so under manual approval a harness
  wrote without asking. It is now bounded like a registered machine, and
  `HarnessMachine` implements the approval flow (`RequestHumanApproval`,
  `HumanApproved`, `HumanRejected`), one call at a time.
- A child's policy is its inherited contract narrowed by its own declaration
  (`ChildRunContract::narrow`), not the inherited contract as is.
- Capability reconciliation: a harness derives a call's capability from the
  tool's name, so in harness sessions and harness children the tool executor
  also checks it against the capability the registry knows the tool to have
  and refuses a mismatch. It is not applied when a `tool_executor_override`
  replaces the tool slot, whose executor the override owns.
- An `agent` step offers its `tools` list on every turn, including the first
  (omitted means the session's agent-mode tools), and the list is an
  allowlist: a proposed call outside it is never run and the model is told
  why. Fixing this also fixed an older defect, an agent loop that never
  continued after a tool settled.

**Children.** `parallel` requires `SpawnChild`, as the kernel already did at
run time, and the validator now says so offline. Harness children carry their
depth in the spawn descriptor, can verify, delegate and fan out in turn, and
are bounded by `max_delegate_depth` for the whole chain. A run waiting on live
child runs is not quiescent, so a deadline or cancellation is honoured rather
than reported as a stall, and a run's tool calls are bound to its cancel
scope and end with it.

**Retry.** Every attempt of a body is a step start; exhaustion continues at
`on_exhausted`; the frame ends when its body completes.

**Tooling.** `sven-harness-testkit` (surface tier) runs case manifests on
fresh engines with mock models and fixture tools and checks each run by
replaying it; `sven harness` is its command line; eight samples
(`samples/harnesses`) and an SDK-only runner exercise it. Harness events are
rendered by the headless runner and ignored by ACP.

**Models are strictly opt-in.** Nothing here pulls, starts or loads a model.
A live model needs both `SVEN_HARNESS_LOCAL_MODEL=1` and
`SVEN_HARNESS_LOCAL_ENDPOINT` (or `--model-endpoint`) naming a server the
caller already runs; without them a live case is skipped and `--model` is
refused. `make test/harness/local-model` is the only target that can reach one, and
it refuses to start without both.
