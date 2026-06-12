# Graph Machine

`sven-graph` + `GraphMachine` replace every hardcoded Rust state machine in
`sven-core` with a single generic interpreter driven by a compiled graph.
Instead of writing a Rust `match` tree for each new agent behaviour, you build
a `Graph` programmatically (or, once the parser is complete, via a `.graph` DSL
file) and hand it to `GraphMachine`.

---

## Architecture overview

```
sven-graph (pure model layer — depends only on sven-hsm)
  model.rs       Graph, NodeData, EdgeData, GuardExpr, EffectTmpl, …
  compile.rs     GraphBuilder  →  validated Graph
  guard.rs       GuardExpr evaluation against GuardCtx
  template.rs    {{ path }} interpolation against TemplateCtx
  render_dot.rs  Graph  →  Graphviz dot string (visualisation only)
  native.rs      NativeRegistry, NativeFn, NativeOutcome

sven-core/machines/graph/ (interpreter — depends on sven-graph + loop_core)
  mod.rs         GraphMachine: impl Machine<State = NodeId>
  policy.rs      policy_from_graph: per-node caps → PermissionPolicy
  render.rs      render_effects: EffectTmpl slice → Vec<Effect>
```

`GraphMachine` is a drop-in `Machine` implementation.  It can be wrapped in
`Hsm<GraphMachine>` and driven exactly like any hand-written machine.

---

## Graph model

### NodeId

`NodeId` is a fixed-size inline byte array (`[u8; 47]` + `u8` length) so it
satisfies `Machine::State: Copy`.  Its `Debug` impl prints the bare name string
(no quotes), which is the key used by `PermissionPolicy`.

```rust
let id = NodeId::new("Generating");
assert_eq!(format!("{id:?}"), "Generating");
```

### Node kinds

| Kind | Description |
|------|-------------|
| `Composite` | Has an `initial` child; delegates unhandled events up. |
| `Leaf` | No children; evaluates edges directly. |
| `Loop` | Leaf that owns a `LoopSpec`; runs the `loop_core` inner loop automatically. |
| `Native { fn_name }` | Delegates dispatch to a registered `NativeFn`. |
| `Terminal` | Signals `Hsm::is_done()` = true. |

### LoopSpec

`Loop` nodes carry a `LoopSpec` that wires them into `loop_core`:

```rust
LoopSpec {
    thread: "chat".into(),
    tools: ToolsSpec::AllMode("agent".into()),
    max_rounds: 16,
    schema: None,  // Some(SchemaRef) for decision-producing loops
}
```

On `Entry`, `init_loop` initialises the `LoopState` fact.  On every subsequent
event, `handle_tool_event` drains tool responses and `on_llm_turn_complete`
classifies `LlmTurnComplete` as one of `CallTools / EmptyTurn / MaxRoundsReached
/ FinalAnswer`.  The DSL author writes only the exit conditions (`on final →`
edges); the inner loop machinery is invisible.

### Edge matching

Edges are evaluated in declaration order.  The first edge whose `EventPattern`
matches and whose optional `GuardExpr` evaluates to `true` fires:

- `target: None` → `Reaction::Handled` (internal / stay-in-state)
- `target: Some(id)` → `Reaction::Transition(id, effects, rationale)`

If no edge matches, the node returns `Reaction::Super(parent)` (implicit
hierarchical bubbling).  At the root (where `parent == self`), it returns
`Reaction::Ignored` to terminate the Super chain.

---

## GuardExpr

Guards are a small, total, side-effect-free expression language.  They are
evaluated against a read-only `GuardCtx`:

```rust
pub struct GuardCtx<'a> {
    pub facts:      &'a serde_json::Map<String, Value>,
    pub retry:      &'a HashMap<String, u32>,
    pub decision:   Option<&'a Value>,   // parsed LLM decision (loop nodes only)
    pub event:      &'a Value,           // event as JSON
    pub loop_state: Option<&'a Value>,   // lc_state fact
}
```

**Path roots:**

| Root | Resolves to |
|------|-------------|
| `fact.<k>` | `ctx.facts["k"]` |
| `decision.<k>` | the decision value at key `k` |
| `event.<k>` | the event JSON at key `k` |
| `retry.<k>` | `ctx.retry_counters["k"]` (u32 as JSON number) |
| `loop.<k>` | the `lc_state` fact at key `k` |

**Operators:** `== != > >= < <=` with numeric coercion, `&&`, `||`, `!`,
`len(path)`, parentheses.  Missing path = `null`.  Not Turing-complete —
anything requiring mutation or iteration belongs in a `Native` node.

**Compiler rule:** any node that references `decision.*` in a guard must have
at least one catch-all `on Final` edge (no guard, or a guard that is always
true for any non-null decision).  This prevents a malformed LLM response from
wedging the machine.

---

## EffectTmpl

Effect templates are declared on edges (`on_entry`, `on_exit`).
`render_effects` converts them to concrete `Effect` values at dispatch time.
Prompts and arg blobs support `{{ path }}` interpolation against the same roots
as guards plus `const.<k>` for named constants injected at construction time.

| Template | Produces |
|----------|----------|
| `CallLlm { thread, prompt_tmpl, tools, max_rounds, schema, model }` | `Effect::CallLlm` |
| `CallTool { name, capability, args_tmpl }` | `Effect::CallTool` |
| `AskUser { prompt_tmpl }` | `Effect::AskUser` |
| `Approve { capability, description_tmpl }` | `Effect::RequestHumanApproval` |
| `Checkpoint { label }` | `Effect::CreateCheckpoint` |
| `Rollback { label }` | `Effect::RollbackToCheckpoint` |
| `Emit { name, payload_tmpl }` | `Effect::EmitInternal` |
| `Spawn { graph_name, descriptor_tmpl }` | `Effect::InstantiateSubmachine` (one child) |
| `SpawnEach { path, graph_name, descriptor_tmpl }` | one `InstantiateSubmachine` per array element at `path` |
| `Timer { timer_id_tmpl, duration }` | `Effect::ScheduleTimeout` |
| `CancelTimer { .. }` | `Effect::CancelTimeout` |
| `StartLoop { thread, tools, max_rounds }` | mutates `ctx` via `init_loop`; no kernel effect |
| `SetFact { key, value_tmpl }` | mutates `ctx.facts`; no kernel effect |
| `Native { fn_name, params }` | calls the registered `NativeFn`; inlines its effects |

---

## NativeRegistry

The native escape hatch for genuinely imperative logic (~20% of real machines).

```rust
pub type NativeFn = fn(&mut Context, &Event, &NativeArgs) -> NativeOutcome;

pub enum NativeOutcome {
    Handled(Vec<Effect>),
    Goto { target: String, effects: Vec<Effect>, rationale: String },
    Bubble,
    Ignore,
}
```

`NativeFn` receives `&mut Context` so it can read/write facts and retry
counters; all I/O still goes through the returned `Effect` values.  Register
functions at construction time:

```rust
let mut reg = NativeRegistry::new();
reg.register("my_fn", my_native_fn);
let machine = GraphMachine::new(graph, Arc::new(reg), HashMap::new());
```

A `Native { fn_name }` node dispatches every non-lifecycle event to the
registered function.  A `Native` template (inside an effect list) calls the
function as a side-effecting action within a transition.

---

## Building a graph

Use `GraphBuilder` for programmatic construction (tests, built-in graphs):

```rust
use sven_graph::compile::GraphBuilder;
use sven_graph::model::{EdgeData, EventPattern, EffectTmpl};
use sven_hsm::event::EventKind;

let mut b = GraphBuilder::new("chat");

// Composite root
b.add_composite("Top", "Top", Some("Session"));

// Session composite under Top
b.add_composite("Session", "Top", Some("Idle"));
b.edge(EdgeData {
    pattern: EventPattern::Kind(EventKind::UserCancelled),
    guard: None,
    target: Some(NodeId::new("Idle")),
    effects: vec![],
    rationale: "user cancelled".into(),
});

// Idle leaf — starts the loop on UserMessage
b.add_leaf("Idle", "Session");
b.edge(EdgeData {
    pattern: EventPattern::Kind(EventKind::UserMessage),
    guard: None,
    target: Some(NodeId::new("Generating")),
    effects: vec![EffectTmpl::StartLoop {
        thread: "chat".into(),
        tools: ToolsSpec::AllMode("agent".into()),
        max_rounds: 16,
    }],
    rationale: "start generating".into(),
});

// Generating loop node — exits on FinalAnswer
b.add_loop("Generating", "Session", LoopSpec {
    thread: "chat".into(),
    tools: ToolsSpec::AllMode("agent".into()),
    max_rounds: 16,
    schema: None,
});
b.edge(EdgeData {
    pattern: EventPattern::Final,
    guard: None,
    target: Some(NodeId::new("Idle")),
    effects: vec![EffectTmpl::SetFact {
        key: "last_response".into(),
        value_tmpl: "{{ event.text }}".into(),
    }],
    rationale: "answer ready".into(),
});

let graph = b.build()?;
```

The compiler validates:
- Exactly one root (node whose parent is itself)
- Every non-root node reaches the root
- Every `Composite` has a reachable `initial` descendant
- No `Loop` node has an `initial` (loops are leaves)
- Loop nodes with `decision.*` guards have a catch-all `on Final` edge
- No duplicate node names

---

## Wiring into the runtime

```rust
use std::{collections::HashMap, sync::Arc};
use sven_core::machines::graph::{policy::policy_from_graph, GraphMachine};
use sven_hsm::dispatch::Hsm;

let graph = Arc::new(build_my_graph());
let native = Arc::new(NativeRegistry::new());
let machine = GraphMachine::new(graph.clone(), native, HashMap::new());

// Derive a PermissionPolicy from the per-node `caps` annotations.
let policy = policy_from_graph(&graph);

// Drive it exactly like any other Machine.
let mut hsm = Hsm::new(machine);
let mut ctx = Context::new();
hsm.init(&mut ctx);
```

### Per-node capability annotations

Mark nodes with the `ToolCapability` values they are allowed to emit:

```rust
b.add_leaf("Generating", "Session");
b.caps(vec![ToolCapability::ReadFile, ToolCapability::WriteFile]);
```

`policy_from_graph` converts these into a `PermissionPolicy` keyed by node
name (matching `NodeId`'s `Debug` output).  States with no annotations receive
no per-state grants — the global policy still applies.

---

## Dot rendering

`render_dot` produces a Graphviz `digraph` string for visual inspection:

```rust
use sven_graph::render_dot::render_dot;

let dot = render_dot(&graph);
// pipe to: dot -Tsvg -o graph.svg
```

Node shapes by kind:
- `Composite` → `cluster_` subgraph
- `Leaf` → box
- `Loop` → dotted self-loop annotation
- `Native` → grey component shape
- `Terminal` → Msquare

The rendering is one-way (no round-trip from dot back to a graph).

---

## Migration status

The three existing machines are being migrated to built-in `.graph` files in
phases.  Until a phase is complete both the old machine and the new graph
coexist; after passing the parity test the old file is deleted.

| Phase | Target | Status |
|-------|--------|--------|
| 1 | `sven-graph` crate + `GraphMachine` skeleton | ✅ complete |
| 2 | `chat.graph` — port `ReactiveAgentMachine`, delete `reactive_agent.rs` | pending |
| 3 | `task.graph` — port `TaskMachine`, delete `sdlc/task.rs` | pending |
| 4 | `sdlc.graph` — port `SdlcMachine`, delete `sdlc/mod.rs` | pending |
| 5 | Lexer/parser for the concrete DSL syntax, load builtins via `include_str!` | pending |
| 6 | Command/agent graph loading + subagent fan-out + `sven graph validate\|render` CLI | pending |

**Parity test strategy:** before deleting each old machine, snapshot
`ctx.audit` over a fixed event script against the old machine, then assert
`GraphMachine` produces an identical trace.  `Hsm::dispatch` is deterministic
and effects are payload-comparable, so the comparison is exact.
