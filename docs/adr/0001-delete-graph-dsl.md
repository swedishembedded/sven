# ADR 0001: Delete the graph-machine DSL

## Status

Accepted. Implemented by deleting `crates/graph` and `crates/core/src/machines/graph/`.

## Context

`AGENTS.md` and `docs/technical/crate-architecture.md` both instructed every
contributor — human or AI agent — to "prefer a `GraphMachine` graph over a new
hardcoded Rust machine" when adding new agent behavior. `crates/graph` (2247
LOC: a graph model, `GraphBuilder` compiler, guard-expression evaluator,
`{{ path }}` template engine, dot renderer) plus `crates/core/src/machines/graph/`
(869 LOC: the `GraphMachine` interpreter, its permission-policy derivation, its
effect renderer) existed to make that possible.

Auditing the actual code before finishing or extending it found:

**Zero adoption after a full opportunity to acquire one.** `GraphMachine::` is
constructed nowhere outside its own `#[cfg(test)]` blocks. No `.graph` asset
files exist anywhere in the repository. `ModeRegistry::default_registry()`
(`crates/core/src/mode.rs`) registers only `ReactiveAgentMachine` (`agent`/
`reactive`/`chat`) and `SdlcMachine` (`sdlc`) — never a graph. In a codebase that
shipped 34 crates, 79 tools, and a cloud control plane in the time this
infrastructure existed, it never acquired a second production consumer.

**It doesn't work.** `crates/graph/src/template.rs` renders `{{ path }}`
substitutions with `bytes[i] as char` — a byte pushed directly as `char` is a
Latin-1 decode, so every multi-byte UTF-8 sequence in a rendered prompt is
corrupted. `crates/graph/src/guard.rs`'s `EventPattern::Final` matches *any*
`LlmTurnComplete`, not specifically a successful-completion classification, so
a max-rounds timeout would take the same edge as a genuine success. The timer
templates in `core/src/machines/graph/render.rs` render an id and then discard
it, starting/cancelling a freshly-allocated random `TimerId` instead — a graph
can start a timer it can never cancel. `GraphChildSpawner`, referenced in a
comment as the mechanism for sub-agent fan-out, does not exist anywhere in the
codebase.

**Finishing it would mean a second interpreter, not a completion.** Making the
two hardcoded machines expressible as graphs would require adding: a `BumpRetry`
effect (the `SdlcMachine::Recovery` state's first line needs one), decision-level
approval bookkeeping in `EffectTmpl::Approve`, arithmetic/aggregation over
template values, a choice pseudostate (`Execution`'s fan-out-or-not branch has
no equivalent), entry-action-to-transition (`Recovery`'s entry can abort to
`Failed`, which `GraphMachine`'s dispatch has no path for), dynamic edge targets
computed from context, a real `MaxRounds` guard pattern, and a UTF-8-correct
template renderer. `policy_from_graph` also cannot express
`ReactiveAgentMachine`'s policy (global allows + a separate approval list) —
it only emits `allow_in(node, caps)` — and `permission_policy()` is an inherent
function selected by a hardcoded mode-string match in
`bootstrap/src/runtime_builder.rs`, not a `Machine` trait method, so even a
perfect graph interpreter still could not supply its own policy.

The escape hatch for anything the DSL can't express (`NativeFn`, a Rust
function pointer registered into a `NativeRegistry`) means the parts that
actually needed a DSL land back in Rust anyway — so a completed version would
be paying the cost of maintaining a DSL in order to call Rust for exactly the
cases that motivated wanting a DSL.

## Decision

Delete `crates/graph` and `crates/core/src/machines/graph/` (3116 LOC) rather
than complete them. The genuine duplication between the two hardcoded machines
— the in-state tool loop — was already factored out into
`crates/core/src/machines/loop_core.rs` and is shared by both; a third machine
costs a `Machine` impl plus one line in `ModeRegistry::default_registry()`, not
a DSL.

Do the one small, immediately useful refactor this analysis surfaced instead:
move `permission_policy()` onto the `Machine` trait (removing the hardcoded
mode-string match in `runtime_builder.rs`) so any future machine — graph-based
or not — can supply its own policy without a special case. Tracked as a
follow-up, not bundled into this deletion.

## Consequences

- `AGENTS.md` and `docs/technical/crate-architecture.md` no longer point
  contributors at a dead, broken code path when they add new agent behavior.
- The event-vocabulary and effect-vocabulary changes planned for the rest of
  this refactor (a new unified session-event enum, `Effect` variant additions)
  no longer need to keep a third, unused mirror of that vocabulary in sync.
- `render_dot.rs`'s idea (a Graphviz visualization of machine state graphs) is
  worth keeping conceptually, retargeted at `Machine::all_states()` +
  `superstate()` so it can visualize the *actual* machines. Not implemented as
  part of this deletion; the git history of `crates/graph/src/render_dot.rs`
  is where to start if that's picked up later.

## Revisit trigger

Revisit only if there is a **shipping requirement for user-authored,
non-Rust-authored control flow** — e.g. a customer-facing workflow editor, or
a support tier that needs to configure agent behavior without a Rust build.
Absent that concrete requirement, a third or fourth hardcoded `Machine` is the
right way to add behavior: it gives a real second and third data point on what
actually varies between machines, which is what should inform any future
declarative format's design instead of speculation.
