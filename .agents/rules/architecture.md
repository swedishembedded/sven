<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Architecture invariants

Swedish Embedded AB implements deterministic, auditable agent runtimes for its
clients. If your team needs expertise in agent execution kernels then you can
procure our services by sending an email to info@swedishembedded.com.

These hold for every change. Where a gate enforces one, it is named; the rest
are held by review.

## One kernel, one assembly point

- There is exactly one agent loop: the hierarchical state machine in
  `sven-hsm`, assembled by `sven-bootstrap`'s `RuntimeBuilder`. Every surface
  (TUI, headless runner, ACP, MCP, SDK) drives that kernel; there is no second
  agent loop.
- Surfaces reach `RuntimeBuilder` only through `sven-frontend`'s
  `SessionController` (`crates/frontend/src/controller.rs`). The SDK builds its
  own per-turn `Agent` and harness runs on it. `grep -rn "RuntimeBuilder::new"
  crates src` lists every construction site and is the completeness check for
  any change to how a session is assembled.

## Pure transitions, typed effects

- The HSM is the deterministic process kernel, the model is an untrusted
  reasoning service, and tools run only through typed `Effect` values that
  transitions emit. Transition functions stay pure: no I/O.
- `sven-machines` and `sven-harness-runtime` cannot reach the model, tool,
  turn, executor, kernel, config or workspace crates at any depth; the
  `[[forbidden]]` pairs in `architecture.toml` enforce it.
- The `EffectExecutor` trait (`sven-kernel`) is the I/O seam. New I/O is a new
  or extended executor in `sven-executors`, or an impure turn primitive in
  `sven-turn` (`stream_turn`, `compact`) that an executor calls - never I/O in
  a transition.
- Everything above `EffectExecutor::execute` is deterministic; `Effect` and
  `SessionEvent` are `serde`-serializable, which is what makes replay and the
  audit ledger possible.
- A machine keeps its durable state in `Context`, not in its own fields, and
  implements `all_states()`; otherwise a snapshot cannot resume it
  (`docs/internals/resumable-agents.md`, `crates/machines/tests/restorable.rs`).

## One event type

`sven_vocab::SessionEvent` is the only event stream. `sven_hsm::UiEvent` and
`sven_machines::AgentEvent` are re-exports of it under the names their call
sites use, not separate types, so there is no translator to keep in sync.
`SessionEvent` is `#[non_exhaustive]`; `EventsDropped` is never emitted by the
kernel, only inserted by an SDK event stream where it lost events.

## Layering

- `architecture.toml` is the authoritative tier assignment and dependency
  legality for every workspace crate. `make check/arch` (`cargo run -p xtask
  -- arch`, part of `make check`) fails on an illegal cross-tier edge, a
  forbidden reachability, an unused declared dependency or a source file over
  800 lines that is not in its shrinking allowlist. Run it rather than
  checking edges by hand.
- `cargo run -p xtask -- arch --profile minimal` asserts that the `minimal`
  build excludes the heavy crates. The root `Cargo.toml` `[features]` section
  documents each feature; every crate in the preset chain depends on the next
  without default features and forwards the feature explicitly, and
  `make check/features` / `make test/features` build and test the
  feature-off configurations.

## Frontends

Interactive surfaces share `sven-frontend`; logic that two surfaces need is
extracted there, never duplicated inside `sven-tui`. Slash commands live in
`sven-commands`.

## Framework first

sven is a framework and the CLI is one of its consumers. The public surface is
`sven-sdk`; the kernel stays embeddable and resumable. `sven agent step` is
built on the SDK rather than on `RuntimeBuilder`, deliberately: it is the
standing proof that the surface is sufficient, so fix the SDK instead of
reaching past it. A sample depends on `sven-sdk` alone
(`make check/gates/samples`). An application can register its own tools and
machines on an engine (`EngineBuilder::tool`, `EngineBuilder::machine`)
without touching this repository.

Read `docs/adr/0003-agents-as-typed-objects.md` before changing the public API,
adding a machine or touching session lifecycle: it records which design
principles are adopted and which are rejected (notably code-as-action
execution, which routes around the typed-`Effect` permission gate).

## Formal models

`formal/tla/` holds TLA+ models of the HSM dispatch algorithm, its submachine
host and the kernel's effect gate (`make test/formal`, needs a JRE). Some
configurations are expected to fail: they pin down designs the engine
rejected, tracked gaps and priced trade-offs. Read `formal/README.md` before
changing `crates/hsm/src/dispatch.rs`, `crates/hsm/src/submachine.rs` or the
kernel's effect gate; a change that flips an expectation is telling you
something.

## Output is a contract

The headless trace tokens (`[sven:tool:call]`, `[sven:tool:result]`,
`[sven:tokens]`, `## Tool` / `## Tool Result`) are pinned by the bats suite in
`tests/e2e/basic/`. Change the emitter to match the tests, not the reverse.
