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
- `sven-vocab` grows harness session events (`HarnessStarted`,
  `StepStarted`, `StepCompleted`, `BranchSelected`, `ChildStarted`,
  `ChildCompleted`, `VerificationCompleted`, `RetryScheduled`,
  `BudgetExhausted`, `HarnessCompleted`, `HarnessFailed`), each rendered by
  every frontend per the one-event-vocabulary rule.
- `Event::VerificationComplete` gains a correlation id before concurrent
  verifiers are allowed.
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
