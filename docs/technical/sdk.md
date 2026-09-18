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
