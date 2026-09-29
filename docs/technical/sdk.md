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
let reply = agent.send("summarise the build failure").await?.reply;

let stored = serde_json::to_string(&agent.suspend())?;
// …another request, another process…
let mut resumed = engine.resume(serde_json::from_str(&stored)?)?;
resumed.send("now propose a fix").await?;
```

`Engine` is cheap to clone - it is a bundle of handles - so a service builds one
at startup and clones it into each request.

## Tools

An engine's agents get exactly the tools the application names. The default
`Toolset` is none: no built-in tools and no MCP servers, only what is
registered with `EngineBuilder::tool`. A preset opts into sven's own tools:

```rust
let engine = Engine::builder()
    .toolset(Toolset::coding())   // read/write/edit files, search, shell, todo, ask_question
    .tool(Arc::new(MyTool))       // registered on top; wins a name collision
    .build()?;
```

`Toolset::research()` is the read-only preset. A registered tool is gated and
audited like a built-in one: its `kernel_capability` picks the permission
bucket and its `default_policy` whether it needs approval.

## Human gates

When the kernel needs a human - an inherently dangerous capability such as
`ExecuteShell`, or a question - the engine's `ApprovalPolicy` answers:
`Deny` (the default), `AutoApprove` (disposable workspaces only), or `Ask`,
which hands each gate to the application:

```rust
let engine = Engine::builder()
    .approvals(ApprovalPolicy::ask(|gate| match gate {
        HumanGate::Approval { capability, prompt, reply_tx } => {
            // ask someone; reply now or later
            let _ = reply_tx.send(policy_allows(capability, &prompt));
        }
        HumanGate::Question { prompt, reply_tx } => {
            let _ = reply_tx.send(answer_for(&prompt));
        }
    }))
    .build()?;
```

The run waits for the reply, bounded by its cancel token and deadline; a
gate dropped without a reply leaves the turn waiting. An approved call runs
exactly as the model proposed it.
A refused call is answered in the conversation with the reason, so the next
request is valid for every provider and the model learns why.

## Questions that wait

The `ask_question` tool in the coding and research presets does not block:
the run parks on the question and ends with `RunConclusion::Waiting`, carrying
it in `outcome.question`. The answer may come from someone else, much later,
in another process:

```rust
let outcome = agent.send("start a web service").await?;
if let Some(question) = outcome.question {
    let state = agent.suspend();                 // store it anywhere
    // ... later, anywhere ...
    let mut agent = engine.resume(state)?;
    let outcome = agent.answer(&question.id, "Axum").await?;
}
```

The answer becomes the result of the call that asked, so the model reads it as
if it had been given on the spot. A parked run is neither a success nor a
failure; its trajectory carries no reward until it concludes.

## Bounded runs

`send` returns a `RunOutcome`: how the run ended (`conclusion`), the `reply`,
and the tokens it used (`usage`, `None` for a count the provider did not
report). `send_with` adds bounds:

```rust
let cancel = CancelToken::new();
let outcome = agent
    .send_with("fix the failing test", RunOptions::new()
        .cancel(cancel.clone())                  // cancel.cancel() from anywhere
        .deadline(Duration::from_secs(300))
        .max_output_tokens(20_000))
    .await?;
match outcome.conclusion {
    RunConclusion::Success => println!("{}", outcome.reply),
    RunConclusion::BudgetExhausted => { /* out of tokens or tool rounds */ }
    RunConclusion::Cancelled | RunConclusion::Timeout => { /* stopped from outside */ }
    RunConclusion::Waiting => { /* see outcome.question */ }
    other => { /* not produced by send */ }
}
```

A bound that fires interrupts the model call in flight and cancels the turn;
the agent keeps the history and kernel state it had, so it can be sent to
again. The tool-round budget is `AgentConfig.max_tool_rounds`; a turn the
budget cut short still ends with an answer, and reports `BudgetExhausted`.

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

## Declaring an agent as a trait

`#[agent]` turns a Rust trait into an agent type. The role is the trait's
documentation, each task is its method's documentation, and each schema is
derived from the method's return type.

```rust
/// You are a meticulous Rust reviewer who never speculates.
#[sven_sdk::agent]
trait Reviewer {
    /// Assess the change for correctness risk.
    async fn assess(&self, change: Change) -> Assessment;

    /// Whether this may merge unattended. Policy, so it is code.
    fn may_merge(&self, a: &Assessment) -> bool {
        a.risk < 50
    }
}

let mut reviewer = Reviewer::new(&engine);
let assessment = reviewer.assess(change).await?;
```

**A method with a body is deterministic; a method without one is model-driven.**
That is the whole split, expressed as something the compiler already tracks -
no marker attribute to forget and no second list to keep in sync. A model-driven
method becomes `async fn … -> Result<T, CallError>`; a method with a body is
emitted unchanged and never reaches the model.

The generated type owns an `Agent`, so it suspends and resumes like any other:
`new`, `resume`, `suspend`, `state`, `events`.

Parameters are passed to the model as a named object (`{"change": {…}}`) rather
than positionally, so the model is told what each value *is*. A trait or method
without a doc comment is a compile error: a model told only a method's name has
been told almost nothing, and failing at build time is better than discovering
it in an answer.

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

## Extending sven from outside it

A framework whose capabilities can only be added by editing it is not a
framework. Both extension points take types defined in *your* crate.

### A tool of your own

```rust
let engine = Engine::builder()
    .tool(Arc::new(StockPrice))   // repeatable
    .build()?;
```

Registered on top of the built-in set, so the agent keeps everything it already
had. From then on it is permission-gated and audited exactly like a built-in:
`kernel_capability()` decides which bucket the kernel gates it under, and
`default_policy()` whether it needs approval. Declaring those honestly is what
keeps the permission model meaningful - a tool that reaches the network and
claims otherwise has disabled a guarantee for everyone.

Caller-supplied tools are registered last, so a deliberate shadowing of a
built-in name wins. Silently ignoring it would be the more surprising choice.

Everything needed to implement one is re-exported from `sven_sdk::tool`, so an
application never names a kernel crate.

### A machine of your own

```rust
let engine = Engine::builder()
    .machine("echo", Box::new(|| Box::new(Hsm::new(EchoMachine::new()))))
    .build()?;

let mut agent = engine.agent("echo");
```

The kernel drives it exactly as it drives the built-in machines: permissions,
audit, and suspend/resume all apply. Registering an existing mode name replaces
it; registering a new one extends the registry rather than replacing it.

**Implement `all_states()`** or agents running your machine cannot be suspended
or resumed - see [resumable-agents.md](resumable-agents.md).

`sven_sdk::machine` re-exports `Machine`, `Reaction`, `Context`, `Effect` and
the rest of what an implementation needs.

## When a step ends

A step ends when the kernel has nothing left to do, not when a model turn
completes. Those coincide for a machine that drives the model, but a machine
that answers from its own state never completes a turn at all, and waiting for
one would hang forever.

`ErasedRuntime::capture` is served only once the event queue has drained, which
makes it exactly the "nothing left to do" signal. `Agent::send` races it against
the observation stream, preferring observations so that a model turn still ends
on `TurnComplete` with its text collected.

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
| `declared_agent.rs` | An agent declared as a trait, with the deterministic/model-driven split |
| `custom_tool.rs` | Giving an agent a capability the kernel has never heard of |

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
