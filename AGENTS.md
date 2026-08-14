# Sven - AI Coding Agent

Sven is a keyboard-driven AI coding agent built in Rust. It runs as an
interactive TUI (`sven`), a headless CI runner, a networked P2P node, and a
**managed-agents cloud platform** ("cloud brain, local hands") - all from the
same multi-crate workspace. (A Slint desktop GUI, `crates/gui`, existed
briefly and was removed - the TUI is the only interactive local surface.)

## For AI Agents Working on This Codebase

- **Language**: Rust. Follow idiomatic patterns, ownership, and error handling.
- **The single execution engine is the HSM kernel.** There is exactly one agent
  loop: the Hierarchical State Machine in `sven-hsm`, assembled by
  `sven-bootstrap::RuntimeBuilder`. Every surface (headless CI, interactive
  TUI, P2P node, ACP, cloud) drives that kernel. There is **no** second
  "agent loop" - the legacy `sven_machines::Agent`/`AgentBuilder` has been retired.
- **Key principle**: The HSM is the deterministic process kernel; the LLM is an
  untrusted reasoning service; tools are invoked exclusively through typed
  `Effect` values emitted by HSM transitions. **Transition functions must stay
  pure (no I/O).** `sven-machines` (the `Machine` impls) has no path to
  `sven-model`/`sven-tools` at all. The outside world is touched only by
  `sven-executors`' `EffectExecutor`s and the impure turn primitives they call
  into (`sven-turn`'s `stream_turn`/`compact`) - never from a transition.
- **Cloud model**: the kernel (the "brain") can run in the cloud while tools
  (the "hands") execute on customer premises via a `RemoteToolExecutor` over an
  outbound WSS tether. Credentials never leave the customer boundary.
- **Skills** (load before writing code): Rust → `.cursor/skills/programming/rust/SKILL.md`;
  public API changes → `.cursor/skills/programming/rust-semver/SKILL.md`;
  TUI → `.cursor/skills/programming/ratatui/SKILL.md`.
- **New behaviour**: a new `Machine` impl in `machines/src/machines/` reusing
  `loop_core` for the tool-loop plumbing; register it in
  `mode.rs::default_registry()`. (A prior graph-DSL extension path was
  deleted — see `docs/adr/0001-delete-graph-dsl.md`.)
- **Tests**: `make test` (unit/integration), `make check` (clippy `-D warnings`,
  zero-warning policy), `make tests/e2e/basic` (bats E2E; needs `bats-core`).
- **Before any sweeping/cross-cutting change, read "Making cross-cutting
  changes" below** - it is the canonical map of every place each kind of
  change must touch.

## Task notes (`.todo/`)

Ad-hoc task briefs for AI agents live in `.todo/*.md` (gitignored — never part of
repo history). When a task is finished, move its file into `.todo/completed/`
(plain `mv`, not `git mv`, since the whole directory is ignored).

## Essential Commands

| Command | Purpose |
|---------|---------|
| `make build` | Debug build (all binaries) |
| `make release` | Optimised release build |
| `make test` | Unit + integration tests (root `sven` package) |
| `make check` | Clippy lint, `-D warnings` |
| `make fmt` | Format |
| `make tests/e2e/basic` | Bats end-to-end suite |
| `make docs` | Single-file user guide → `target/docs/sven-user-guide.md` |

## Binaries

| Binary | Entry point | Description |
|--------|-------------|-------------|
| `sven` | `src/main.rs` | Interactive TUI, headless CI runner, P2P node, CLI |
| `sven-companion` | `crates/companion/src/main.rs` | Customer-premises "local hands": dials out to the cloud, executes constrained tools locally |

The cloud **control plane** is not a separate binary — it is the `sven cloud`
subcommand of the `sven` binary: `sven cloud serve` (control plane),
`sven cloud tenant`/`token`/`demo-seed` (admin), `sven cloud session start`
(operator client). See `deploy/` for the docker-compose quickstart and
`docs/cloud/` for scenario walkthroughs.

## Crate table

The dependency spine: `sven-bootstrap` (RuntimeBuilder) → `sven-hsm` (kernel) →
{ `sven-machines` (machines), `sven-executors` (I/O), `sven-model` (LLM), `sven-tools` }.

| Crate | Purpose |
|-------|---------|
| `sven-hsm` | **HSM kernel**: `Machine` trait, Samek dispatch, `Runtime`/`ErasedRuntime` (Active Object), `Effect`/`Event`/`UiEvent` vocab, `PermissionPolicy` + `ToolCapability`, `Context` (+ `Principal`), `SessionSupervisor`, `AuditRecord` + hash-chained event-sourcing/replay |
| `sven-model` | **LLM reasoning service**: `ModelProvider` trait, `CompletionRequest`/`ResponseEvent` (incl. `Usage`), 34 driver impls, catalog. (Note: `sven-llm` is only a `ConversationStore` + fence helper - the provider abstraction lives here.) |
| `sven-executors` | **Effect executors** (the only I/O layer): `CompositeExecutor` (trait-object slots) wiring `TurnExecutor`, `ToolExecutor`, `UserExecutor`, `TimerExecutor`, `CheckpointExecutor`, `AuditExecutor`, `InternalExecutor`, and the cloud `RemoteToolExecutor` |
| `sven-config` | Config schema + layered loader (`sven.yaml`); env-var secret expansion |
| `sven-image` | Image reading helpers |
| `sven-audio` | WAV decoding, resampling, and audio data-URL helpers |
| `sven-session-store` | ATIF trajectory-backed session store (`trace_session`), legacy YAML chat import, markdown history, conversation parse/render (renamed from `sven-input` in Phase 4.3 - "input" didn't describe a session-persistence crate) |
| **`atif`** | **ATIF v1.7 trajectory format** (package name `atif`, deliberately without the `sven-` prefix; renamed from `trace` in Phase 4.3 - the old name read as the `tracing` crate, not "trajectory"): `Trajectory`/`TraceStep`/`AgentProfile`/etc. model, spec validation (`validate_trajectory`), atomic whole-document JSON persistence with opt-in concurrent-modification detection (the interactive surfaces currently pass `expected = None`), header-only fast reads, NDJSON step streaming. Zero dependencies on other sven crates; consumed by `sven-session-store::trace_session` (session persistence), `sven-ci` (workflow trajectory I/O), and `sven-tui` (session save/load). |
| `sven-tools` | Tool suite, `Tool`/`ToolDisplay` traits, `ApprovalPolicy`, `ToolPolicy`/`RolePolicy` (fs_root jail), `PermissionRequester`, `ToolRegistry` (`execute` / `execute_with_requester` / `execute_unattended`) |
| `sven-machines` | Pure HSM machines: `ReactiveAgentMachine`, `SdlcMachine`, `TaskMachine`, `ModeRegistry`, `loop_core`. Re-exports `sven-turn` and `sven_vocab::SessionEvent` (as `AgentEvent`) unchanged for existing call sites |
| `sven-turn` | Impure turn primitives, one tier below `sven-machines`/`sven-executors`: `stream_turn` (the real LLM streaming call), context compaction (`compact_session`/`smart_truncate`/...), tool-arg JSON repair, system-prompt assembly |
| `sven-workspace` | Workspace/project discovery: root detection, skill/agent/knowledge scanning (renamed from `sven-runtime` in Phase 4.3 - "runtime" already named the HSM's `Runtime<M>`/`ErasedRuntime`/`RuntimeBuilder`/`RuntimeContext`, and this crate is none of those) |
| `sven-bootstrap` | `RuntimeBuilder` (assembles the kernel from config + mode; `with_effect_executor`, `with_principal`), `SessionSupervisor`, `SessionBundle`/`RuntimeHandle` |
| `sven-ci` | Headless runner: `RuntimeRunner` (single-shot HSM driver) + workflow orchestration (`--file`, `--var`, jsonl/chat I/O, artifacts, output formats) driving the kernel |
| `sven-mcp-client` | MCP client (stdio + Streamable HTTP, OAuth); merges external tools into the registry |
| `sven-mcp` | MCP server: exposes sven tools (stdio); node-proxy mode forwards to a node |
| `sven-acp` | ACP server for IDEs (stdio JSON-RPC); wires `AcpPermissionRequester` |
| `sven-p2p` | libp2p: TCP+Noise+Yamux, mDNS, relay+dcutr+autonat (outbound-only), request_response task protocol, gossipsub rooms; peer allowlists + hop signatures |
| `sven-node` | HTTP/WS node + P2P + kernel wiring; `ControlService` (`ControlCommand`/`ControlEvent`); bearer auth, TLS, WebAuthn PTY, Slack/Telegram |
| `sven-node-client` | WS client for a running node (verified TLS by default) |
| `sven-team` | Agent-team coordination: shared task list, worktrees, P2P `TeamEvent` |
| `sven-frontend` | **Shared frontend layer**: `MachineProjection`, `agent`/`node_agent`/`operator` tasks, `AgentEvent` consumption, slash commands, markdown, queue, tool views |
| `sven-tui` | Ratatui TUI (`sven` binary): `UiMode`, projection consumer, keybindings |
| **`sven-wire`** | **Cloud tether protocol**: versioned serde types (`CompanionRegister`, `ToolCallRequest`/`Result`, `Progress`, `ApprovalRequest`/`Response`, `Heartbeat`) between control plane and companion |
| **`sven-companion`** | **Local hands**: dials out over WSS, registers a constrained tool manifest, executes under fs_root jail + shell allowlist + approval hooks; loopback mode for tests |
| **`sven-metering`** | Pricing catalog (per-model prices + tenant markup), usage events, append-only hash-chained credit ledger, statement export |
| **`sven-cloud`** | **Control plane**: `CloudServer` (tether endpoint), `IdentityService` (mint/authenticate/revoke scoped tokens), `SessionGate` (subscription+credit gating), `CompanionRegistry` (→ `RemoteToolExecutor`), portal/feed (reuses node control protocol), SQLite store |

## Architecture

```
 sven (CLI/TUI)        sven-cloud (control plane)   sven node (P2P/WS)
      │                        │                        │
 sven-tui                portal/feed +            ControlService
      │  MachineProjection / CompanionRegistry          │
      │  AgentEvent              │                      │
      └──── sven-frontend ───────┘                      │
                  │                                      │
              sven-bootstrap (RuntimeBuilder)  ◄─── one assembly point
                  │
            sven-hsm (kernel: pure transitions → Vec<Effect>)
           /        |          \                    \
     sven-machines  sven-model  sven-executors ──────► sven-tools
     (machines) (LLM svc)   (ONLY I/O layer)     RemoteToolExecutor ──WSS──► sven-companion
                                                                            (customer premises)
```

Everything above `EffectExecutor::execute` is pure and deterministic; everything
below it is I/O. `Effect` and `Event` are `serde`-serializable, which is what
makes the cloud split (remote tools, replay, audit ledger) possible.

## Frontend architecture

Interactive surfaces **share `sven-frontend`** - never duplicate logic inside
`sven-tui` that belongs in the shared layer; extract to `sven-frontend`. New
slash commands go in `sven-commands::builtin` (a lower-tier crate that
`sven-frontend` re-exports at its historical `commands` module path), never
in `sven-tui`.
Surfaces consume the `AgentEvent` stream (produced from the kernel's `UiEvent`
by the adapter in `sven-machines`) and the `MachineProjection` broadcast.

---

## Making cross-cutting changes (READ THIS FIRST for any sweeping change)

Sven's power is that one kernel serves many surfaces - which means a change to a
core concept usually has to land in **several** places at once. This table is
the canonical checklist: find your change's axis and touch **every** listed
site. When you add a new axis of extension, add a row here.

### Add a new `Effect` variant
1. `hsm/src/effect.rs` - the `Effect` enum, its `EffectKind`, and
   `required_capability()` (return `Some` iff it must be permission-gated).
2. `executors/src/composite.rs` - route the new `EffectKind` to an executor
   slot in `CompositeExecutor::execute` (or a new slot).
3. The executor that performs the I/O (new file in `sven-executors` or an
   existing one); it must post result `Event`s back via the `EventSink`.
4. `hsm/src/audit.rs` - it is captured automatically as an `EffectKind`, but
   check `AuditOutcome` handling if it needs special treatment.
5. Any machine in `sven-machines` that should emit it.

### Add a new tool
1. `tools/src/builtin/…` - implement `Tool` (+ `parameters_schema`,
   `kernel_capability`, `default_policy`, `modes`) and `ToolDisplay`.
2. Register it: `sven-bootstrap` tool-registry assembly + `sven-tools` registry.
3. If exposed over MCP: `mcp/src/registry.rs` (`DEFAULT_TOOL_NAMES`).
4. If it needs a new capability: see "Add a `ToolCapability`".
5. Constrained-hands profile: `sven-companion` manifest (its `main.rs` tool set).
6. Tests: unit test + a bats case in `tests/e2e/basic/` if it has headless output.

### Add a `ToolCapability` (permission bucket)
1. `hsm/src/permissions.rs` - the `ToolCapability` enum,
   `is_inherently_dangerous`, and the classify path.
2. Every machine's `permission_policy()` in `sven-machines` (`reactive_agent.rs`,
   `sdlc/mod.rs`) - decide allow/approval per state.
3. `node/src/p2p_kernel.rs::headless_policy()` and any cloud
   `SessionGate`/policy that enumerates capabilities.
4. `sven-tools` `kernel_capability()` of the tools that use it.

### Add a new HSM machine / mode
1. `machines/src/machines/…` - implement `Machine`, reusing `loop_core` for the
   tool-loop plumbing.
2. `machines/src/mode.rs::default_registry()` - register the mode string.
3. Its `permission_policy()`.
4. `bootstrap/src/runtime_builder.rs` - any child-spawner wiring.
5. Config: `sven-config` `AgentMode` if it's user-selectable.

### Add a new model provider / driver
1. `model/src/registry.rs` - the `DRIVERS` table (env var, base URL,
   `requires_api_key`).
2. A driver module (usually reuse `openai_compat`; bespoke only if the wire
   format differs).
3. `model/src/catalog.rs` - model metadata; **and pricing in
   `sven-metering`** if it should be billable.

### Change what a session/agent run looks like on a SURFACE
The kernel is one; the surfaces that drive it are the ones you must keep in sync.
A change to session lifecycle, event streaming, approval flow, or cancellation
must be applied to **each surface that constructs a kernel via
`RuntimeBuilder`**:
1. **Headless** - `sven-ci` (`RuntimeRunner` + workflow orchestration).
2. **Interactive TUI** - `frontend/src/agent.rs` (+ its `AgentEvent`
   adapter usage; the TUI consumes `AgentEvent`).
3. **P2P node** - `sven-node` (`control/service.rs`, `agent_builder.rs`,
   `node.rs`, `p2p_kernel.rs`).
4. **Local ACP** - `acp/src/agent.rs`.
5. **Cloud** - `sven-cloud` session path (+ `RemoteToolExecutor`).
   Grep guard: `grep -rn "RuntimeBuilder" crates` finds every construction site.

### Add a new `AgentEvent` / `UiEvent`
1. `sven-hsm` `UiEvent` (kernel-emitted) and/or `sven-machines` `AgentEvent`.
2. The `UiEvent`→`AgentEvent` adapter (`sven-machines`).
3. Consumers: `sven-frontend` (projection + renderers), `sven-tui`,
   `sven-ci` output (`runner/event.rs`, `conversation.rs` trace tokens),
   `sven-acp` notification mapping, `sven-node` `ui_event_to_control`.
   Missing one silently drops the event on that surface - check all.

### Add identity / tenancy / auth
1. `hsm/src/context.rs` `Principal`; stamped into `AuditRecord`.
2. `sven-bootstrap` `SessionSupervisor` (tenant→session ownership).
3. `sven-cloud` `identity.rs` (token roles/scopes), `auth.rs`, `gate.rs`.
4. `sven-node` control-plane auth if the surface is network-facing.

### Add a config field
1. `config/src/schema.rs` (+ `#[serde(default)]` for back-compat).
2. `loader.rs` if it needs env expansion or layering rules.
3. The consumer crate; document in `docs/` and the config example.

### Add metering / billing surface
1. `sven-metering` (pricing catalog, ledger event, statement).
2. The LLM gateway interception point (`sven-model` `base_url` override /
   `sven-cloud` metered provider) and `SessionGate`.

### Cloud tether protocol change
1. `sven-wire` (versioned - bump the protocol version constant).
2. Both ends: `sven-cloud` (`registry.rs`/`server.rs` tether handler) and
   `sven-companion` (`tether.rs`/`service.rs`). Keep back-compat or gate on
   version.

### Golden rules
- **One assembly point**: kernels are built by `RuntimeBuilder`. `grep -rn
  "RuntimeBuilder"` enumerates every surface - use it as the completeness check
  for any surface-spanning change.
- **The `EffectExecutor` trait is the I/O seam.** New I/O = new/extended
  executor, never I/O in a transition.
- **Never duplicate frontend logic inside `sven-tui`** - shared code to `sven-frontend`.
- **Trace/output is a public contract**: the bats suite in `tests/e2e/basic/`
  pins the headless stderr/stdout tokens (`[sven:tool:call]`,
  `[sven:tool:result]`, `[sven:tokens]`, `## Tool` / `## Tool Result`). Change
  the emitter to match the tests, not the reverse.

## Documentation
- [README.md](README.md), [docs/00-introduction.md](docs/00-introduction.md)
- [docs/technical/](docs/technical/) - HSM architecture, ACP, skill system, P2P,
  session protocol, cloud platform.
- [docs/adr/](docs/adr/) - architecture decision records.
