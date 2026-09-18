# The Sven SDK

`sven-sdk` (directory `crates/sdk`) is the framework's public surface: what an
application depends on to run agents, of which the `sven` CLI is one consumer.

Design rationale is in [ADR 0003](../adr/0003-agents-as-typed-objects.md).
Suspend/resume mechanics at the kernel level are in
[resumable-agents.md](resumable-agents.md).

## Three layers, three lifetimes

| Layer | Owns | Lives for |
|-------|------|-----------|
| `Engine` | Model client and its connection pool, configuration, approval policy | The process |
| `Agent` | One conversation's history and kernel state | A task |
| `AgentState` | The same, serialized, holding no live handles | Storage |

The split is the point. Everything expensive is on the engine, so an agent is
cheap enough to create per request; everything durable is in `AgentState`, so an
agent can be put down and picked up somewhere else.

```rust
let engine = Engine::builder()
    .model_provider(provider)   // shared by every agent this engine makes
    .build()?;

let mut agent = engine.agent("agent");
let reply = agent.send("summarise the build failure").await?;

let stored = serde_json::to_string(&agent.suspend())?;
// …another request, another process…
let mut resumed = engine.resume(serde_json::from_str(&stored)?)?;
resumed.send("now propose a fix").await?;
```

`Engine` is cheap to clone - it is a bundle of handles - so a service builds one
at startup and clones it into each request.

## Typed model-driven methods

A `Method<T>` is a contract: instructions, a return type, and the limits on
obtaining it. The caller supplies typed input and gets a validated `T` back.

```rust
let triage = Method::<Triage>::new("triage")
    .role("You triage bug reports for a Rust systems project.")
    .task("Classify the report. Be conservative.")
    .max_repairs(2)
    .postcondition(|t: &Triage| {
        if (1..=5).contains(&t.severity) { Ok(()) }
        else { Err(format!("severity must be 1-5, got {}", t.severity)) }
    });

let result: Triage = engine.call(&triage, &report).await?;
```

The schema the model is constrained by is **derived from `T`** rather than
written by hand, which is what stops the description the model is given from
drifting away from the type the caller actually receives.

`Engine::call` is the lightweight form - no instance state, nothing accumulates
between calls. `Engine::agent_for(&method)` returns an `Agent` whose successive
calls build on each other.

### Strategies

| Strategy | Machine | For |
|----------|---------|-----|
| `Predict` (default) | `predict` | Interpretation without investigation: classification, extraction, assessment. **No tools at all.** |
| `Investigate` | `agent` | Work that must find things out first - reading, running, searching - before it can answer. |

Strategy is configuration, not a call-site argument: the same `Method` and the
same call site work either way. A cheap model can serve a narrow classification
while a stronger one investigates, without either contract changing.

### Correction is bounded, and failures keep their kind

A rejected answer stays in the thread and the diagnostic is appended after it,
so the model sees what it got wrong rather than being asked again from a clean
slate. `max_repairs` bounds this; zero means the first answer is the only one.

Three outcomes, deliberately not collapsed into one:

| Outcome | Means |
|---------|-------|
| `CallError::Invalid` | No answer could be read as `T` at all - a structural failure |
| `CallError::Postcondition` | Well-formed, but broke an invariant the type cannot express |
| `CallError::Infrastructure` | The kernel or provider failed. **Not** a model mistake |

The first two are different evidence. A postcondition failure proves the model
understood the shape it was asked for and got the *content* wrong; an `Invalid`
proves nothing of the sort. A structurally valid object can still carry a
fabricated citation, which is why return-type validation and postconditions are
separate checks rather than one.

### Why the repair loop is in the SDK, not in a machine

A pure transition cannot deserialise a candidate into the caller's return type -
the machine has no idea what that type is. So the `predict` machine does one
thing: it runs a constrained, tool-free turn and records the raw candidate.
Validating it, and deciding whether to spend another attempt, belongs to
whoever declared the return type.

This is not a second agent loop. The SDK posts messages and reads replies, the
way any surface does; it does not stream from the model or dispatch tools. The
one agent loop is still the kernel's.

## Sharing, and what is actually shared

`EngineBuilder::model_provider` is what makes many agents affordable. Without
it, each session constructs its own provider from config, which means a fresh
HTTP client and connection pool per turn. It is also the seam a metering or
gateway wrapper hangs off: wrap the provider once and no agent can reach the
model unwrapped.

Agents on one engine share those resources and **nothing else**. Histories are
per-instance; one agent cannot see another's conversation.

## Approval policy

A turn that reaches a human-approval gate blocks until the gate is answered, so
an agent running unattended must answer it. `ApprovalPolicy::Deny` is the
default: refusing is safe, and answering "yes" on nobody's behalf is not.
`ApprovalPolicy::AutoApprove` matches what the headless CI runner does and is
appropriate only where the workspace is already disposable.

## Errors keep their identity

`CallError` separates a caller mistake (`Precondition`), a state that cannot be
resumed (`Resume`), and a transport or kernel failure (`Infrastructure`).
Retrying and paging someone are not interchangeable responses, and an outage
must never be reported as the model having answered badly.

A model that simply answers poorly is not an error at all - it is the reply.

## Observing a run

`Agent::events()` yields the same `SessionEvent` stream the TUI and the headless
runner consume, so a custom surface renders progress without naming a kernel
crate. Subscribe before calling `send`; a receiver created afterwards sees only
what is still buffered.

## Where the state comes from

`AgentState` carries the conversation history and a kernel `Snapshot`. The
snapshot is taken through `ErasedRuntime::capture`, which is served only once
the kernel's event queue has drained - so it always shows the machine at rest
rather than part-way through a turn.

That guarantee leans on the turn executor's documented ordering: the inward
completion event is posted *before* the outward `TurnComplete` that tells a
caller the turn is over. The completion event is therefore already queued ahead
of any capture request, and the capture is served after the machine has
processed it. Without that ordering a snapshot could catch the machine mid-turn,
and resuming from one would land in a state that ignores the next user message -
intermittently, which is the worst way for it to fail.

## Configuration

An engine built without an explicit config uses `Config::default()`, not the
user's configuration file. An embedded agent should not silently inherit
whatever happens to be on the host's disk; a caller that wants the file loads it
with `sven_config::load` and passes it to `EngineBuilder::config`.

## Examples

`crates/sdk/examples/` holds runnable programs, one concept each:

| Example | Shows |
|---------|-------|
| `classify.rs` | A typed method with no agent instance |
| `suspend_resume.rs` | Advance one step, persist, free, resume |
| `watch_events.rs` | A custom surface built from the event stream |
| `verified_workflow.rs` | Deterministic orchestration around model judgement, where a code-level check can reject the model's claim |

## The CLI on the SDK

`sven agent step` is the shell-level form of the same lifecycle:

```sh
sven agent step --state ./review.json "read src/lib.rs and summarise it"
sven agent step --state ./review.json "now list its public types"
```

Each invocation is a separate process. It loads the agent from the state file,
advances it by exactly one step, persists, and exits - nothing of the agent
survives between the two commands except the file.

It is built on `sven-sdk` rather than on `RuntimeBuilder`, which makes it the
working proof that the published surface is sufficient: if `sven agent step`
cannot do something, neither can anyone else's application. Its contract is
pinned by `tests/e2e/basic/16_agent_step.bats`.

A state file that exists but cannot be parsed is an error, not a fresh start.
Silently discarding a conversation would surface much later, as an agent that
had inexplicably forgotten everything.
