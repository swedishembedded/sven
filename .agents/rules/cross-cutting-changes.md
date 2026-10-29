<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Cross-cutting changes

Swedish Embedded AB implements extensible agent frameworks for its clients. If
your team needs expertise in changing a multi-surface agent runtime without
regressions then you can procure our services by sending an email to
info@swedishembedded.com.

One kernel serves many surfaces, so a change to a core concept usually has to
land in several places at once. Find the axis of your change and touch every
listed site. When you add a new axis of extension, add a section here. Paths
are relative to `crates/`.

## Add an `Effect` variant

1. `hsm/src/effect.rs`: the `Effect` enum, its `EffectKind`, and
   `required_capability()` (return `Some` if and only if it must be
   permission-gated).
2. `executors/src/composite.rs`: route the new `EffectKind` to an executor slot
   in `CompositeExecutor::execute` (or a new slot).
3. The executor that performs the I/O (in `sven-executors`); it posts its result
   `Event`s back through the `EventSink`.
4. `hsm/src/audit.rs`: the effect is captured as its `EffectKind`; check
   `AuditOutcome` handling if it needs special treatment.
5. Every machine in `sven-machines` that should emit it.

## Add a tool

First ask whether it belongs in this repository at all: an application can
register its own tools (`sven_sdk::EngineBuilder::tool`) and machines
(`EngineBuilder::machine`) on an engine. Concrete tools live in the domain-tier
`sven-tools-*` crates, split by concern; `sven-tool-api` holds the `Tool` trait
and `sven-tool-registry` the registry, and neither holds a concrete tool.

1. Implement `Tool` in the right `sven-tools-*` crate: `summary` (at most
   `SUMMARY_MAX_CHARS` = 100 characters; it is all the model knows of a deferred
   tool until it loads it, and a test requires it of every registered tool),
   `parameters_schema`, `kernel_capability` (the widest effect any call has),
   `call_capability` when actions differ in effect, `default_policy`, `modes`,
   and `ToolDisplay`.
2. A new tool is deferred. Only `sven_tool_registry::CORE_TOOLS` is sent in full
   with every request; adding to it needs a measured reason
   (`docs/internals/tool-disclosure.md`).
3. Register it in `bootstrap/src/registry.rs`.
4. If it is exposed over MCP: `DEFAULT_TOOL_NAMES` in `mcp/src/registry.rs`, a
   curated allowlist (stateful and TUI-dependent tools stay out), not derived
   from the linked crates.
5. A new capability: see "Add a `ToolCapability`".
6. Tests: a unit test, and a bats case in `tests/e2e/basic/` if it has headless
   output.

## Change which tool definitions a request carries

`ToolRegistry::offer` (`tool-registry/src/disclosure.rs`) is the one place that
decides: the machine's named tools, else the mode's core tools followed by the
session's loaded tools in load order (deferred disclosure), else every tool of
the mode (`full`). The loaded set is `Context::active_tools`, grown by
`Event::ToolsActivated` in `Hsm::dispatch` and copied onto every turn request;
never keep it in a machine, an executor or a tool. `TurnExecutor` and the SDK's
`take_offered_tools` both go through `offer`, so a record of a turn and the
turn cannot disagree.

## Add a `ToolCapability` (permission bucket)

1. `hsm/src/permissions.rs`: the `ToolCapability` enum, `ALL`, `is_read_only`
   (what manual approval asks about) and the `classify` path.
2. Every machine's `permission_policy()` in `sven-machines`
   (`machines/src/machines/reactive_agent.rs`, `machines/src/machines/sdlc/mod.rs`):
   allow or ask, per state.
3. The tool's own `kernel_capability()` in its `sven-tools-*` crate.

## Add a machine or mode

1. `machines/src/machines/`: implement `Machine`, reusing `loop_core` for the
   tool-loop plumbing.
2. Implement `all_states()` and keep all durable state in `Context`, not in
   the machine's fields. `all_states()` maps a snapshot's state label back to a
   state, so a machine without it cannot be resumed
   (`machines/tests/restorable.rs` fails the suite); a field the machine owns is
   not captured by a snapshot and silently reverts on resume, which no test
   catches. See `docs/internals/resumable-agents.md`.
3. Register the mode in `machines/src/mode.rs` (`default_registry`). A machine
   that drives one specific tool registers instead in `bootstrap/src/modes.rs`
   (`mode_registry`, behind that tool's feature) and gives its sessions the tool
   there (`register_mode_tools`), so no build lists a mode whose tool it lacks.
4. Its `permission_policy()`.
5. `bootstrap/src/runtime_builder.rs`: any child-spawner wiring.
6. `sven_vocab::AgentMode` if it is user-selectable.

## Add a decision point

A decision model only advises; see `docs/internals/decisions.md`.

1. Declare its `DecisionPolicy` (`sven-decision`): the question shape, input
   budget, thresholds, impact (anything but `routine` is advisory-only by
   construction) and fallback. Thresholds belong to the decision point, never
   to a shared constant.
2. Ask through `decide_batched` (questions about one state travel in one call),
   judge with `evaluate` (and `agree` when options are asked in two orders), and
   on abstention or failure return exactly the result without the decision.
3. Record `DecisionTelemetry::record_call` and `record_trace` for every ask, and
   report a failure with `DecisionRuntime::diagnose` (once per session).
4. Gate it on a `decision.features.*` flag (`Feature` in `decision/src/config.rs`)
   and document the key in `docs/reference/configuration.md`.
5. Test it with `MockDecisionProvider`: an abstention, a failure and a promotion
   must each be proven.

## Add a harness primitive

Documents are workflows (ADR 0005, 0006); a primitive is a variant of the
closed `StepSpec` enum that a workflow construct lowers to. It is a deliberate
act across the stack, never an interpreter escape hatch.

1. Lowering and typed spec: the variant and its step struct in
   `harness-spec/src/steps.rs`, the workflow construct in
   `harness-spec/src/workflow.rs` (checks in `workflow_validate.rs`), the
   lowering in `lower*.rs`, and its shape in
   `harness-spec/schema/harness-document.schema.json` (the `workflow_schema`
   test holds schema and parser together).
2. Static validation in `harness-spec/src/steps_validate.rs`: references, the
   capability it exercises (`required_capability`), the limits it needs; plan
   form in `plan.rs`.
3. Runtime behaviour in `harness-runtime/src/exec.rs` or a new module beside it.
4. Authorization: the capability in the plan's permission policy, and for
   child-starting primitives the depth and contract rules
   (`bootstrap/src/child_spawner.rs`).
5. Tracing: a `TraceRecord` (`harness-runtime/src/run.rs`), its `HarnessEvent`
   in `vocab/src/harness.rs` and `harness_event`; renderers per "Add a
   `SessionEvent` variant".
6. Tests in `harness-spec/tests`, `harness-runtime/tests` and
   `sdk/tests/harness_*.rs`.
7. A runnable sample in `samples/harnesses/` (the `harness_cli` test validates
   every sample), documented in `docs/reference/harnesses.md` and
   `docs/guides/harnesses.md`.

## Add a model provider or driver

1. `model/src/registry.rs`: the `DRIVERS` table (id, key variable, default base
   URL, `requires_api_key`, `local`). Pure metadata; it stays in `sven-model`.
2. A driver module in `sven-model-drivers` (usually reusing `openai_compat`;
   bespoke only when the wire format differs).
3. Nothing in `model-catalog`: models, limits and prices are what the
   provider's endpoint serves. If its listing is not OpenAI-shaped, the driver's
   `list_models` reads it into `ModelCatalogEntry` and records it with
   `listing::record`.

## Change what a session looks like on a surface

`frontend/src/controller.rs` (`SessionController`) is where a TUI, headless or
ACP session is assembled from the configuration, where its gates are wired to
whoever answers them (`Gates`), and where a mode or model change rebuilds it
with the conversation and MCP servers carried over. Session assembly, rebuilds
and gates change there, once. What stays with each surface is what it does with
the events:

1. Headless: `sven-ci` (`KernelAgent`, `RuntimeRunner`): output formatting, exit
   codes, the trajectory.
2. Interactive TUI: `frontend/src/agent.rs` (`kernel_session_task` and the
   `AgentRequest` protocol), consumed by `sven-tui`.
3. ACP: `acp/src/agent.rs`: the `session/update` mapping and ACP's permission
   requests.

## Add a `SessionEvent` variant

1. The variant in `vocab/src/lib.rs`; its payload type goes to wherever it
   belongs in a foundation-tier leaf (payloads are pushed down, the enum is
   never pushed up).
2. Every renderer that matches on it: `sven-frontend` (projection and
   renderers), `sven-tui`, `sven-ci` output (`ci/src/runner/event.rs` trace
   tokens) and the `sven-acp` notification mapping. These keep an explicit arm
   per handled variant and a wildcard for the rest, so no lint or compiler error
   catches a missed one: a renderer you skip silently drops the event on that
   surface.

## Add identity, tenancy or auth

1. `Principal` in `hsm/src/context.rs`; stamped into `AuditRecord`.
2. `SessionSupervisor` in `sven-bootstrap` (principal-to-session ownership).

## Add a configuration field

`sven-config` knows no section; a field belongs to the crate that reads it:
provider and model settings to `sven-model-drivers` (`model-drivers/src/config.rs`),
MCP servers to `sven-mcp-client`, a tool's settings to that tool's crate, the
agent loop and tool assembly to `sven-bootstrap` (`bootstrap/src/config/`), the
interactive UI to `sven-frontend` (`frontend/src/settings.rs`).

1. The section's struct in that crate (`#[serde(default)]` for compatibility)
   and its key in the section's `schema()`: a key the schema lacks is reported
   as unrecognised when the file loads.
2. Layering and `${VAR}` expansion belong to `config/src/document.rs`; touch it
   only to change how files are found or merged.
3. If the file format changes: a fixture and its golden record in
   `frontend/tests/fixtures/config/` (`make generate/config/golden`, then review
   the diff).
4. Document the key in `docs/reference/configuration.md`.
