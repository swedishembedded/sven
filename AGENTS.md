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
  "agent loop".
- **Key principle**: The HSM is the deterministic process kernel; the LLM is an
  untrusted reasoning service; tools are invoked exclusively through typed
  `Effect` values emitted by HSM transitions. **Transition functions must stay
  pure (no I/O).** `sven-machines` (the `Machine` impls) has no path to
  `sven-model`/`sven-tools` at all. The outside world is touched only by
  `sven-executors`' `EffectExecutor`s and the impure turn primitives they call
  into (`sven-turn`'s `stream_turn`/`compact`) - never from a transition.
- **One event type.** `sven_hsm::UiEvent` and `sven_machines::AgentEvent` are
  both plain type aliases for `sven_vocab::SessionEvent` - the same value,
  not a translation. There is no adapter to keep in sync; every surface
  consumes `SessionEvent` under whichever alias its call sites historically
  used. See "Add a new `AgentEvent` / `UiEvent`" below for what this means for
  new variants (the compiler does **not** yet catch a dropped variant - see
  that section's caveat).
- **The architecture is mechanically enforced.** `architecture.toml` at the
  repo root is the **authoritative** tier assignment and dependency-legality
  list for all 60+ workspace crates - not this document. `cargo run -p xtask
  -- arch` (wired into `make check`) fails the build on an illegal
  cross-tier edge, an unused declared dependency, or a file over the 800-line
  ratchet. Read `architecture.toml` directly for "what tier is crate X in" or
  "what can X legally depend on"; the crate table below is a human-readable
  summary of the same data; when they disagree, `architecture.toml` wins. See
  `.claude/skills/programming/rust/architecture.md` for the layering method.
- **Cloud model**: the kernel (the "brain") can run in the cloud while tools
  (the "hands") execute on customer premises via a `RemoteToolExecutor` over an
  outbound WSS tether. Credentials never leave the customer boundary.
- **Skills** (load before writing code): Rust → `.cursor/skills/programming/rust/SKILL.md`;
  public API changes → `.cursor/skills/programming/rust-semver/SKILL.md`;
  TUI → `.cursor/skills/programming/ratatui/SKILL.md`.
- **New behaviour**: a new `Machine` impl in `machines/src/machines/` reusing
  `loop_core` for the tool-loop plumbing; register it in
  `machines/src/mode.rs::default_registry()`. (A prior graph-DSL extension path
  was deleted - see `docs/adr/0001-delete-graph-dsl.md`.)
- **Tests**: `make test` (unit/integration), `make check` (`xtask arch` +
  clippy `-D warnings`, zero-warning policy), `make tests/e2e/basic` (bats
  E2E; needs `bats-core`), `make tests/e2e/cloud` (managed-agents platform E2E;
  needs `bats-core` + `gdb-multiarch`).
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
| `make test` | Unit + integration tests (whole workspace) |
| `make check` | `xtask arch` (architecture ratchet) + clippy, `-D warnings` |
| `make fmt` | Format |
| `make tests/e2e/basic` | Bats end-to-end suite (CLI/CI/mock behaviour) |
| `make tests/e2e/cloud` | Bats end-to-end suite (managed-agents platform) |
| `make docs` | Single-file user guide → `target/docs/sven-user-guide.md` |

## Binaries and cargo features

| Binary | Crate | Default? | Description |
|--------|-------|----------|-------------|
| `sven` | `sven` (root) | yes | Interactive TUI, headless CI runner, P2P node client, CLI - everything |
| `sven-companion` | `sven-companion` | n/a (own crate) | Customer-premises "local hands": dials out to the cloud, executes constrained tools locally |
| `svend` | `sven-node` | n/a (own crate) | Standalone node daemon - the same `sven node` subcommand tree, without the TUI/MCP/ACP/cloud closure |
| `sven-cloudd` | `sven-cloud` | n/a (own crate) | Standalone cloud control-plane daemon - `serve`/`tenant`/`token`/`session`/`demo-seed`; `connect`'s interactive mode needs the full `sven` binary (see `sven_cloud::cli`'s doc comment) |
| `sven-mcp` | `sven-mcp` | n/a (own crate) | Standalone MCP server - the same `sven mcp serve` |
| `sven-acp` | `sven-acp` | n/a (own crate) | Standalone ACP agent server - the same `sven acp serve` |

The `sven` binary itself has cargo features controlling what's linked in:
`tui` (ratatui + the interactive UI), `network` (P2P node, cloud, ACP, MCP,
team - bundled together because they share a `--node` proxy/dial mode, not
independently toggleable), `gdb` (the GDB/MI tool suite), and `minimal`
(none of the above - headless CI + `tool`/`index`/`map`/`tee`/`reduce` only,
the portability target). `default = ["tui", "network", "gdb"]`, so a plain
`cargo build` is unchanged from before these existed. `cargo run -p xtask --
arch --profile minimal` asserts the `minimal` build's resolved dependency
closure excludes `ratatui`/`libp2p`/`git2`/`webauthn-rs`/`portable-pty`/
`gdbmi`/`axum`/`rusqlite`/`nvim-rs`/`slint`.

The cloud **control plane** is not only a separate binary — it is also the
`sven cloud` subcommand of the main `sven` binary: `sven cloud serve` (control
plane), `sven cloud tenant`/`token`/`demo-seed` (admin), `sven cloud session
start` (operator client). See `deploy/` for the docker-compose quickstart and
`docs/cloud/` for scenario walkthroughs.

## Crate table

The dependency spine: `sven-bootstrap` (RuntimeBuilder) → `sven-hsm` (kernel) →
{ `sven-machines` (machines), `sven-executors` (I/O), `sven-model` (LLM),
`sven-tool-api`/`sven-tool-registry`/`sven-tools` }. 61 workspace crates total,
organized into 10 tiers (`foundation` < `kernel` < `services` < `domain` <
`machines` < `assembly` < `wiring` < `surface` < `composite` < `binary`) -
**`architecture.toml` is authoritative**; this table groups the same crates by
tier with a one-line purpose each.

### foundation (zero sven-\* dependencies)
`sven-vocab` (shared nouns + `SessionEvent`) · `sven-chain` (hash-chained
append-only JSONL) · `sven-config` (config schema + loader) · `sven-hsm`
(HSM kernel types) · `sven-image` (image reading) · `sven-audio` (WAV
decoding, resampling, audio data-URL helpers) · `sven-workspace`
(project/skill/agent/knowledge discovery) · `atif` (ATIF v1.7 trajectory
format) · `sven-p2p` (libp2p transport) · `sven-node-client` (WS client for a
running node) · `sven-node-config` (node config schema) · `sven-node-web`
(WebAuthn + PTY web-terminal types) · `sven-tui-nvim` (embedded Neovim
client, ratatui-rendered)

### kernel
`sven-model` (`ModelProvider` trait, request/response vocab, driver metadata
registry) · `sven-model-catalog` (static model catalog data) · `sven-wire`
(cloud tether protocol DTOs) · `sven-control` (transport-agnostic control
protocol + kernel mappings) · `sven-session-model` (`SessionFold`,
`ChatSegment`, tool-view formatting - the one `## User`/`## Sven` codec) ·
`sven-tool-api` (`Tool` trait + `ToolDisplay`) · `sven-cloud-identity`
(token roles/scopes types)

### services
`sven-session-store` (ATIF trajectory-backed session store, legacy YAML chat
import, `sven migrate-sessions`) · `sven-llm` (`ThreadStore` + fence helper)
· `sven-tools` (thin re-export shim over `sven-tool-api`/`sven-tool-registry`
- **not** where tool implementations live, see below) · `sven-tool-registry`
(`ToolRegistry`, `ApprovalPolicy`, fs_root jail) · `sven-mcp-client` (MCP
client: stdio + Streamable HTTP, OAuth) · `sven-kernel` (`ErasedRuntime`,
`EffectExecutor`, `EventSink`, `ChildSpawner`) · `sven-node-chat` (ephemeral
P2P for `sven peer`, no HTTP/TLS/agent loop) · `sven-model-drivers` (34
provider driver impls, `openai_compat`) · `sven-model-mock` (`--model mock`
test/dev providers)

### domain (concrete tool implementations + integrations)
`sven-tools-fs` (file I/O + output-buffer tools) · `sven-tools-exec` (`shell`,
`run_terminal_command`) · `sven-tools-ctx` (RLM context store, knowledge,
`memory`) · `sven-tools-agent` (`system`, `todo`, `ask_question`, `skill` -
agent self-management) · `sven-tools-web` (`web_fetch`/`web_search`, `grep`,
`search_codebase`, `read_lints`) · `sven-tools-gdb` (GDB/MI debugging, unix
only) · `sven-tools-p2p` (`delegate_task`, `list_peers`, room/session
collaboration) · `sven-turn` (impure turn primitives: `stream_turn`,
`compact`/`smart_truncate`, prompt assembly) · `sven-team` (agent-team
coordination) · `sven-metering` (pricing catalog, credit ledger) ·
`sven-channels`/`sven-integrations`/`sven-memory`/`sven-scheduler`
(optional integration tool providers, feature-gated in `sven-bootstrap`)

### machines
`sven-machines` (pure `Machine` impls: `ReactiveAgentMachine`, `SdlcMachine`,
`TaskMachine`, `ModeRegistry`, `loop_core`) · `sven-executors` (the real I/O
layer: `CompositeExecutor` + its executor slots) · `sven-cloud-metering`
(`UsageMeter`/`SessionGate`)

### assembly
`sven-bootstrap` (`RuntimeBuilder` - the one kernel-assembly point) ·
`sven-commands` (`SlashCommand` trait + builtins) · `sven-companion` (local
hands: dials out over WSS, constrained tool manifest) · `sven-cloud-tether`
(`CompanionRegistry`, tool-call routing)

### wiring
`sven-frontend` (shared frontend layer: `agent`/`node_agent`/`operator`
tasks, `SessionEvent` consumption) · `sven-node-control` (`ControlService`) ·
`sven-node-http` (HTTP/WS router assembly) · `sven-node-p2p`
(`p2p_kernel`, `headless_policy`)

### surface
`sven-ci` (headless runner: `RuntimeRunner` + workflow orchestration) ·
`sven-mcp` (MCP server; node-proxy mode forwards to a node) · `sven-node`
(startup orchestrator, ~400 LOC, composing `node-{config,control,http,p2p,
chat,web}`) · `sven-tui` (ratatui TUI)

### composite
`sven-acp` (ACP server for IDEs) · `sven-cloud` (control plane: `CloudServer`,
`IdentityService`, `SessionGate`, portal/feed; composes `cloud-{store,
identity,tether,portal,metering}`) · `sven-cloud-portal` (`PortalState`,
`CloudSessionLauncher`, `SessionFeed`)

### binary
`sven` (the root crate: CLI parsing + dispatch into `run::*`/`cli::*`
modules, one per subcommand group)

## Architecture

```
 sven (CLI/TUI)        sven-cloud (control plane)   sven node (P2P/WS)
      │                        │                        │
 sven-tui                portal/feed +            ControlService
      │  SessionEvent      CompanionRegistry             │
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
below it is I/O. `Effect` and `SessionEvent` are `serde`-serializable, which is
what makes the cloud split (remote tools, replay, audit ledger) possible.

## Frontend architecture

Interactive surfaces **share `sven-frontend`** - never duplicate logic inside
`sven-tui` that belongs in the shared layer; extract to `sven-frontend`. New
slash commands go in `sven-commands::builtin` (an assembly-tier crate that
`sven-frontend` re-exports at its historical `commands` module path), never
in `sven-tui`.
Surfaces consume the `SessionEvent` stream (aliased as `AgentEvent` in
`sven-machines`/`sven-frontend`/`sven-tui` call sites, `UiEvent` in
`sven-hsm`/kernel-facing code - same type, no translation) and the
`MachineProjection` broadcast.

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
Tool implementations live in the domain-tier `sven-tools-*` crates, split by
concern (see the crate table above) - **not** in `sven-tools` itself, which is
now only a thin re-export shim over `sven-tool-api`/`sven-tool-registry`.
1. Pick (or create) the right `sven-tools-*` crate for the new tool's concern;
   implement `Tool` (+ `parameters_schema`, `kernel_capability`,
   `default_policy`, `modes`) and `ToolDisplay`.
2. Register it: `sven-bootstrap`'s tool-registry assembly
   (`crates/bootstrap/src/registry.rs`).
3. If exposed over MCP: `mcp/src/registry.rs`'s `DEFAULT_TOOL_NAMES` -
   deliberately a curated allowlist (stateful/TUI-dependent/P2P tools are
   intentionally excluded), not something to auto-derive from linked crates.
4. If it needs a new capability: see "Add a `ToolCapability`".
5. Constrained-hands profile: `sven-companion`'s hand-registered tool set in
   its `main.rs` - also a deliberate least-privilege allowlist, same reasoning.
6. Tests: unit test + a bats case in `tests/e2e/basic/` if it has headless output.

### Add a `ToolCapability` (permission bucket)
1. `hsm/src/permissions.rs` - the `ToolCapability` enum,
   `is_inherently_dangerous`, and the classify path.
2. Every machine's `permission_policy()` in `sven-machines` (`reactive_agent.rs`,
   `sdlc/mod.rs`) - decide allow/approval per state.
3. `node-p2p/src/p2p_kernel.rs::headless_policy()` and any cloud
   `SessionGate`/policy that enumerates capabilities.
4. The tool's own `kernel_capability()` in its `sven-tools-*` crate.

### Add a new HSM machine / mode
1. `machines/src/machines/…` - implement `Machine`, reusing `loop_core` for the
   tool-loop plumbing.
2. `machines/src/mode.rs::default_registry()` - register the mode string.
3. Its `permission_policy()`.
4. `bootstrap/src/runtime_builder.rs` - any child-spawner wiring.
5. Config: `sven-config` `AgentMode` if it's user-selectable.

### Add a new model provider / driver
1. `model/src/registry.rs` - the `DRIVERS` table (env var, base URL,
   `requires_api_key`) - pure metadata, stays in `sven-model` even though the
   concrete implementations live in `sven-model-drivers`.
2. A driver module in `sven-model-drivers` (usually reuse `openai_compat`;
   bespoke only if the wire format differs).
3. `model-catalog/src/catalog.rs` - model metadata; **and pricing in
   `sven-metering`** if it should be billable.

### Change what a session/agent run looks like on a SURFACE
The kernel is one; the surfaces that drive it are the ones you must keep in sync.
A change to session lifecycle, event streaming, approval flow, or cancellation
must be applied to **each surface that constructs a kernel via
`RuntimeBuilder`**:
1. **Headless** - `sven-ci` (`RuntimeRunner` + workflow orchestration).
2. **Interactive TUI** - `frontend/src/agent.rs` (`kernel_session_task`/
   `run_kernel_session_task`; the TUI consumes `SessionEvent` as `AgentEvent`).
3. **P2P node** - `sven-node-control` (`service.rs`), `sven-node`
   (`agent_builder.rs`, `node.rs`), `sven-node-p2p` (`p2p_kernel.rs`).
4. **Local ACP** - `acp/src/agent.rs`.
5. **Cloud** - `sven-cloud` session path (+ `RemoteToolExecutor`).
   Grep guard: `grep -rn "RuntimeBuilder" crates` finds every construction site.

### Add a new `SessionEvent` variant (aliased `AgentEvent`/`UiEvent`)
`SessionEvent` lives in `sven-vocab` (foundation tier); `sven_hsm::UiEvent`
and `sven_machines::AgentEvent` are both plain aliases for it, not separate
types requiring a translator.
1. `vocab/src/session_event.rs` (or wherever the variant's payload type
   belongs - payloads are pushed down to foundation-tier leaves, never the
   enum pushed up).
2. Every renderer that matches on it: `sven-frontend` (projection +
   renderers), `sven-tui`, `sven-ci` output (`runner/event.rs`,
   `conversation.rs` trace tokens), `sven-acp` notification mapping,
   `sven-node-control`'s `ui_event_to_control`.
   **Missing one silently drops the event on that surface - check all.** The
   plan originally intended `#[deny(clippy::wildcard_enum_match_arm)]` on
   `sven-tui`/`sven-ci`/`sven-acp`/`sven-frontend` to make this a compile
   error instead of a manual checklist item; **this was never actually wired
   in** (confirmed: `grep -rn wildcard_enum_match_arm` finds nothing in the
   repo) - the lint fires on *any* wildcard match over *any* enum, not just
   `SessionEvent`, so turning it on requires first auditing the ~34 files with
   an existing `_ =>` arm in those four crates to see which are genuinely
   exhaustive-worthy vs. legitimately open-ended (`Result`/`Option`/etc.).
   Tracked as an open follow-up, not yet done.

### Add identity / tenancy / auth
1. `hsm/src/context.rs` `Principal`; stamped into `AuditRecord`.
2. `sven-bootstrap` `SessionSupervisor` (tenant→session ownership).
3. `sven-cloud-identity` (token roles/scopes) and `sven-cloud`'s
   `SessionGate`.
4. `sven-node-control`'s auth if the surface is network-facing.

### Add a config field
1. `config/src/schema.rs` (+ `#[serde(default)]` for back-compat).
2. `loader.rs` if it needs env expansion or layering rules.
3. The consumer crate; document in `docs/` and the config example.

### Add metering / billing surface
1. `sven-metering` (pricing catalog, ledger event, statement).
2. The LLM gateway interception point (`sven-model` `base_url` override /
   `sven-cloud` metered provider) and `sven-cloud-metering`'s `SessionGate`.

### Cloud tether protocol change
1. `sven-wire` (versioned - bump the protocol version constant).
2. Both ends: `sven-cloud`/`sven-cloud-tether` (tether handler) and
   `sven-companion` (`tether.rs`/`service.rs`). Keep back-compat or gate on
   version.

### Golden rules
- **One assembly point**: kernels are built by `RuntimeBuilder`. `grep -rn
  "RuntimeBuilder"` enumerates every surface - use it as the completeness check
  for any surface-spanning change.
- **The `EffectExecutor` trait is the I/O seam.** New I/O = new/extended
  executor, never I/O in a transition.
- **Never duplicate frontend logic inside `sven-tui`** - shared code to `sven-frontend`.
- **The layering is enforced, not just documented.** `make check` runs
  `xtask arch` before clippy; an illegal cross-tier dependency or an unused
  declared one fails the build immediately, with the file/line and a fix
  hint. Don't hand-verify the crate table above against a real dependency
  edge - just run it.
- **Trace/output is a public contract**: the bats suite in `tests/e2e/basic/`
  pins the headless stderr/stdout tokens (`[sven:tool:call]`,
  `[sven:tool:result]`, `[sven:tokens]`, `## Tool` / `## Tool Result`). Change
  the emitter to match the tests, not the reverse.

## Documentation
- [README.md](README.md), [docs/00-introduction.md](docs/00-introduction.md)
- [docs/technical/](docs/technical/) - HSM architecture, ACP, skill system, P2P,
  session protocol, cloud platform.
- [docs/adr/](docs/adr/) - architecture decision records.
- `architecture.toml` - authoritative crate tiers, dependency legality, file-size
  ratchet. `.claude/skills/programming/rust/architecture.md` - the layering
  method this workspace follows, generalized for reuse elsewhere.
