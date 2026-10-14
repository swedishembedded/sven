# Sven - AI Coding Agent

Sven is a keyboard-driven AI coding agent built in Rust. It runs as an
interactive TUI (`sven`) and as a headless CI runner - both from the same
multi-crate workspace. The TUI is the only interactive local surface.

## Repository scope

sven is an **agent execution framework** and the coding agent built on it.
Everything in this repository serves one of those two.

**In scope**
- The execution kernel: the HSM, typed effects, permissions, cancellation,
  delegation, sessions, persistence, traces (ATIF) and audit.
- The embedding surface (`sven-sdk`) and the adapters behind it: model
  providers, MCP, ACP, process execution, media input.
- Tool packages an agent is given explicitly: files, search, shell, web,
  context, GDB, Android device control.
- The coding application: TUI, headless CI runner, slash commands, skills,
  project knowledge, teams.
- Verification of a task's outcome (verifier effects and executors).

**Out of scope** - lives in the application or service that needs it:
- Training models, curating training data, deciding what a model should
  learn, and releasing model artifacts.
- Personal-assistant integrations: messaging channels, email, calendar,
  voice calls, schedulers and proactive jobs.
- Fleet orchestration: placement, scheduling and remote resource management.

**Naming other projects.** A tracked file names another project only when
sven depends on it. brain may be named in its one role here - a model
provider sven talks to at runtime - and never as a build dependency (see
`check/gates/no-brain-dependency`). A project built on sven is never named: describe the need
generically ("an embedding application"). `make check/gates/repo-scope` enforces this;
CHANGELOG.md is history and exempt.

## For AI Agents Working on This Codebase

- **Language**: Rust. Follow idiomatic patterns, ownership, and error handling.
- **The single execution engine is the HSM kernel.** There is exactly one agent
  loop: the Hierarchical State Machine in `sven-hsm`, assembled by
  `sven-bootstrap::RuntimeBuilder`. Every surface (headless CI, interactive
  TUI, ACP, MCP) drives that kernel. There is **no** second
  "agent loop".
- **Key principle**: The HSM is the deterministic process kernel; the LLM is an
  untrusted reasoning service; tools are invoked exclusively through typed
  `Effect` values emitted by HSM transitions. **Transition functions must stay
  pure (no I/O).** `sven-machines` (the `Machine` impls) has no path to
  `sven-model`/`sven-tool-registry` at all. The outside world is touched only by
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
  list for every workspace crate - not this document. `cargo run -p xtask
  -- arch` (wired into `make check`) fails the build on an illegal
  cross-tier edge, an unused declared dependency, or a file over the 800-line
  ratchet. Read `architecture.toml` directly for "what tier is crate X in" or
  "what can X legally depend on"; the crate table below is a human-readable
  summary of the same data; when they disagree, `architecture.toml` wins. See
  `.claude/skills/programming/rust/architecture.md` for the layering method.
- **Skills** (load before writing code): Rust → `.cursor/skills/programming/rust/SKILL.md`;
  public API changes → `.cursor/skills/programming/rust-semver/SKILL.md`;
  TUI → `.cursor/skills/programming/ratatui/SKILL.md`.
- **New behaviour**: a new `Machine` impl in `machines/src/machines/` reusing
  `loop_core` for the tool-loop plumbing; register it in
  `machines/src/mode.rs::default_registry()` - or, when the machine drives one
  specific tool, in `bootstrap/src/modes.rs::mode_registry()` behind that
  tool's feature, so no build lists a mode whose tool it lacks. (A prior graph-DSL extension path
  was deleted - see `docs/adr/0001-delete-graph-dsl.md`.)
- **Tests**: `make test` (unit/integration), `make check` (rustfmt + text
  gates + `xtask arch` + clippy `-D warnings`, zero-warning policy),
  `make test/e2e/basic` (bats E2E; needs `bats-core`).
- **The kernel's load-bearing claims are model-checked**, not only tested:
  `formal/tla/` holds TLA+ models of `sven-hsm`'s dispatch algorithm, its
  submachine host and `sven-kernel`'s effect gate, run by `make test/formal`.
  Several configurations are *expected to fail* - they pin down designs the
  engine rejected, a tracked gap and a priced trade-off - so read
  `formal/README.md` before changing `dispatch.rs`, `submachine.rs` or
  `run_effects`: a change that makes an expected failure pass, or an expected
  pass fail, is telling you something.
- **Repo hygiene is gated, not documented.** `make check/gates` refuses an
  absolute machine path (`/data`, `/home`, `/opt`, `/mnt`, `/root` anywhere;
  `/tmp` in a `.rs` file) and any `brain-*` Cargo dependency. Both also run as
  a git pre-commit hook after `make hooks/install` - but the hook is the fast
  extra guard, never the only path: a gate reachable only from a
  manually-installed hook enforces nothing on a fresh clone, so everything is
  in `make check` too.
- **Env reads are phased out: add a flag, not a variable.** Settings come in
  as CLI flags (and the config file), never as new `std::env::var`, `env!` or
  clap `env = "..."` reads. `make check/gates/env-reads` is a ratchet over
  `scripts/gates/env-reads.allow`: a read not listed fails, and so does a
  listed read that is gone (delete the line). Never add an entry to bless a
  new read.
- **Before any sweeping/cross-cutting change, read "Making cross-cutting
  changes" below** - it is the canonical map of every place each kind of
  change must touch.

## Task notes (`.todo/`)

Ad-hoc task briefs for AI agents live in `.todo/*.md` (gitignored — never part of
repo history). When a task is finished, move its file into `.todo/completed/`
(plain `mv`, not `git mv`, since the whole directory is ignored).

## Roadmaps (`.agents/roadmap/`)

A roadmap file describes work that is **planned or in flight**. It is a
working document, not an archive.

When the work lands, the knowledge in it moves into `docs/` as real
documentation and the roadmap file (or the finished section of it) is
**deleted in the same commit that finishes the work**. Never leave a completed
initiative sitting in `.agents/roadmap/` as a de-facto manual: a reader cannot
tell a plan from a description, so a stale roadmap silently becomes
documentation nobody maintains and nobody trusts.

Prefer folding into an existing page over adding a new one -- if `docs/`
already covers the area, extend that page rather than leaving two documents
disagreeing about the same feature. When the existing page documents
behaviour that turned out not to work, correct it rather than writing around
it.

## Docs describe what works

`docs/` is a description of the current behaviour, not a record of intent. A
change that alters, removes, or disables user-visible behaviour updates its
page **in the same commit**; a feature found to be unreachable gets its page
corrected rather than left standing.

This is not housekeeping. A voice-integration page once described an
ElevenLabs/Whisper/Twilio subsystem in full, with configuration and worked
examples, for a tool whose provider fields were populated by no caller
anywhere -- so every example in it was unrunnable and the config block it
documented did nothing at all. Documentation that cannot be distinguished from working behaviour is
worse than a gap, because a reader has no way to find out except by trying
it.

If you find a documented feature that does not work, either fix the feature
or fix the page, in that commit. Leaving it for later is what produced that
one.

## Make targets

HARD RULE. Every public make target is `make <concept>/<subconcept>/.../<action>`:

- The concept comes first (a noun: `build`, `test`, `check`, `docs`, `package`,
  `release`, `site`, `samples`, `hooks`, `generate`), the verb or variant comes last, and
  there are as many levels as the thing needs, usually two or three. A test or
  sample hierarchy with subdirectories gets one segment per directory level:
  `test/e2e/basic/cli`, `samples/study/svf/run`. Never put the verb or area
  second (`test/harness`, not `harness/test`).
- Names are lowercase `[a-z0-9-]` segments joined by `/`. Private helper
  targets start with `_`.
- Every prefix is itself a target, so `make <TAB>` descends one level at a time.
  An area target is the default action for its area (`build`, `test`, `check`,
  `docs`, `benchmark`, `papers`: it runs its default set, named in its doc line)
  or, where no default is safe or sensible (`release`, `site`, `package`,
  `hooks`, `samples`, `generate`, `test/crate`), it only lists its children and does
  nothing else. An umbrella of anything that tags, pushes or uploads never
  acts.
- A set that follows from the tree is generated, never hand-listed: explicit
  targets from a static pattern rule over a computed list (never `%` pattern
  rules, which bash completion cannot see). Here: `check/gates/<gate>`,
  `test/e2e/<suite>[/<test>]`, `test/crate/<package>`, `build/crate/<package>`,
  `samples/<category>/<name>/{build,run}`, `papers/<name>/pdf`. Adding a
  directory adds a target.
- A bare single-segment target is allowed only for an area, or for the leaves
  `all`, `clean`, `fmt` and `help` (one action each).
- Every target has a `## name - description` line (a generated family has one
  line with a `<placeholder>`), and sits under a `##@ Section` line; `make help`
  is generated from them. `scripts/gates/check-make-targets.sh`
  (`make check/gates/make-targets`, also a pre-commit hook) enforces the
  machine-checkable part.

## Essential Commands

| Command | Purpose |
|---------|---------|
| `make build` | Debug build (all binaries) |
| `make build/release` | Optimised release build |
| `make test` | Unit + integration tests (whole workspace) |
| `make check` | rustfmt + text gates + `xtask arch` + clippy, `-D warnings` + rustdoc links |
| `make check/fmt` | `cargo fmt --all -- --check`, checked not applied |
| `make check/gates` | text gates: no machine paths, no brain dependency, repo scope, sample layering, make target naming, no new environment reads (one generated `check/gates/<gate>` per `scripts/gates/check-<gate>.sh`) |
| `make check/arch` | architecture ratchet only |
| `make check/clippy` | clippy only |
| `make check/doc` | `cargo doc`, broken intra-doc links are errors |
| `make test/gates` | Bats self-tests of the text gates (env-reads, make-targets) on scratch trees; no build needed, part of `make test` |
| `make fmt` | Format |
| `make test/harness` | Every harness suite (one `test/harness/<suite>` each) (spec, runtime, SDK, testkit, samples, CLI, 1000-run stress), mock models only |
| `make test/harness/local-model` | OPT-IN: the `live-local` case of the `ticket-routing` sample against a model you already serve (`ENDPOINT=<url>`, optional `MODEL=<spec>`; refuses without ENDPOINT); never part of `make test` or `make check` |
| `make generate/harness/sample-bundle`, `make generate/config/golden` | Rewrite committed derived files (`generate/<area>/<file>`); the matching tests only check them |
| `make help` | Every target as a tree; names are concept first (`test/harness`, `check/gates/no-machine-paths`), so `make <TAB>` descends level by level (`check/gates/make-targets` enforces it) |
| `make hooks/install` | Install the git pre-commit hooks (one-time per clone) |
| `make test/e2e/basic` | Bats end-to-end suite (CLI/CI/mock behaviour); one `test/e2e/basic/<test>` per bats file |
| `make test/crate/<package>`, `make build/crate/<package>` | One workspace package, on demand (`make test/crate` lists them) |
| `make test/e2e/basic/deferred` | The same suite with `--tool-disclosure deferred` on every invocation |
| `make test/e2e/basic/instructions-deferred` | The same suite with the skill, subagent and knowledge listings deferred on every invocation |
| `make test/formal` | TLA+ models of the HSM kernel (needs a JRE; see `formal/README.md`) |
| `make docs` | Single-file user guide → `target/docs/sven-user-guide.md` |

## Binaries and cargo features

| Binary | Crate | Default? | Description |
|--------|-------|----------|-------------|
| `sven` | `sven` (root) | yes | Interactive TUI, headless CI runner, CLI - everything |
| `sven-mcp` | `sven-mcp` | n/a (own crate) | Standalone MCP server - the same `sven mcp serve` |
| `sven-acp` | `sven-acp` | n/a (own crate) | Standalone ACP agent server - the same `sven acp serve` |

The `sven` binary itself has cargo features controlling what's linked in:
`tui` (ratatui + the interactive UI), `network` (ACP, MCP, team - bundled
together, not independently toggleable), `gdb` (the GDB/MI tool suite),
`memory` (semantic memory, SQLite), the tool presets `coding` (attach images
and audio, transcribe speech) and `research` (read images), `dbus` (brain's
D-Bus model transport), `android` (the `android` tool and the `ui-test` mode),
and `minimal` (none of the above - headless CI +
`tool`/`index`/`map`/`tee`/`reduce` only, the portability target).
`default` is everything but `minimal`. `cargo run -p xtask -- arch --profile
minimal` asserts the `minimal` build's resolved dependency closure excludes
`ratatui`/`libp2p`/`git2`/`webauthn-rs`/`portable-pty`/`gdbmi`/`axum`/
`rusqlite`/`nvim-rs`/`slint`/`zbus`.

The same presets are features of `sven-bootstrap` and `sven-sdk`; each crate
built with `default-features = false` is its minimal assembly. Every crate in
that chain depends on the next without default features and forwards the
feature explicitly, so a preset can never come back through unification.
`make check/features` lints, and `make test/features` tests, each
feature-gated crate with its features off, which no workspace build ever does.

## Crate table

The dependency spine: `sven-bootstrap` (RuntimeBuilder) → `sven-hsm` (kernel) →
{ `sven-machines` (machines), `sven-executors` (I/O), `sven-model` (LLM),
`sven-tool-api`/`sven-tool-registry` }. Every workspace crate is
organized into 11 tiers (`foundation` < `kernel` < `services` < `domain` <
`machines` < `assembly` < `wiring` < `sdk` < `surface` < `composite` <
`binary`) -
**`architecture.toml` is authoritative**; this table groups the same crates by
tier with a one-line purpose each.

### foundation (zero sven-\* dependencies)
`sven-vocab` (shared nouns + `SessionEvent`) · `sven-chain` (hash-chained
append-only JSONL) · `sven-config` (finds, layers, `${VAR}`-expands and
merges the configuration file and reports the keys no section recognises;
knows no section itself) · `sven-hsm`
(HSM kernel types) · `sven-image` (image reading) · `sven-audio` (WAV
decoding, resampling, audio data-URL helpers) · `sven-workspace`
(project/skill/agent/knowledge discovery) · `atif` (ATIF v1.7 trajectory
format) · `sven-tui-nvim` (embedded Neovim client, ratatui-rendered)

Harness language crates: `sven-harness-spec` (document, validator, compiler
to a content-hashed plan, JSON Schema) and `sven-harness-transform` (the
sandboxed WASM helper runtime of `transform` steps).

### kernel
`sven-model` (`ModelProvider` trait, request/response vocab, driver metadata
registry; no transport, no media decoding, no configuration) · `sven-model-catalog` (static model catalog data) · `sven-control`
(transport-agnostic control protocol + kernel mappings) · `sven-session-model`
(`SessionFold`, `ChatSegment`, tool-view formatting - the one `## User`/`##
Sven` codec) · `sven-tool-api` (`Tool` trait + `ToolDisplay`)

### services
`sven-session-store` (ATIF trajectory-backed session store, legacy YAML chat
import, `sven migrate-sessions`) · `sven-chunks`
(addressable context: content-addressed raw evidence with provenance, bounded
resident payloads that spill to disk, staleness against the source file,
deterministic representations at four visibilities) · `sven-tool-registry`
(`ToolRegistry`, `ApprovalPolicy`, fs_root jail) · `sven-mcp-client` (MCP
client: stdio + Streamable HTTP, OAuth; the `mcp_servers:` section,
`McpServerConfig`) · `sven-kernel` (`ErasedRuntime`,
`EffectExecutor`, `EventSink`, `ChildSpawner`) · `sven-model-drivers` (34
provider driver impls, `openai_compat`, model-string resolution:
`ModelResolver`/`resolve_model_from_config`; the `model:` and `providers:`
sections - `ModelConfig`, `ProviderEntry` - and how an unconfigured model is
detected; the D-Bus transport - the `dbus`
provider and the `ActionClient` speech-to-text uses - behind its `dbus`
feature, off by default) · `sven-model-mock` (`--model mock`
test/dev providers) · `sven-decision` (the decision runtime: typed questions a
harness asks a small non-generative decision model, the full ranked
distribution it answers with, distinct failure classes, a scripted mock
provider; the decision model only advises)

### domain (concrete tool implementations + integrations)
`sven-tools-fs` (file I/O + output-buffer tools; images and audio behind
`media`, speech-to-text behind `asr`; the `tools.asr` section) · `sven-tools-exec` (`shell`) · `sven-tools-ctx` (RLM context store, knowledge,
`memory`; the `tools.memory` section) · `sven-tools-agent` (`system`, `todo`, `ask_question`, `skill` -
agent self-management) · `sven-tools-web` (`web_fetch`/`web_search`, `grep`,
`read_lints`; the `tools.web` and `tools.lints` sections) · `sven-tools-gdb`
(GDB/MI debugging, unix only, behind its `mi` feature; the `tools.gdb` section
is always compiled) · `sven-turn` (impure turn primitives: `stream_turn`,
`compact`/`smart_truncate`, `CompactionStrategy`, prompt assembly,
`AgentRuntimeContext`) · `sven-team` (agent-team
coordination) · `sven-memory` (semantic memory store and `semantic_memory`
tool, parked-question ledger; the `memory` feature of `sven-bootstrap`)

### machines
`sven-machines` (pure `Machine` impls: `ReactiveAgentMachine`, `SdlcMachine`,
`TaskMachine`, `ModeRegistry`, `loop_core`; depends only on `sven-hsm` and
`sven-vocab`) · `sven-harness-runtime` (the YAML-harness machine:
`HarnessMachine` + `HarnessRunData`/`HarnessRunState`; compiles through
`sven-harness-spec` and the same kernel, depends only on `sven-hsm`,
`sven-vocab` and `sven-harness-spec`) · `sven-executors` (the real I/O layer:
`CompositeExecutor` + its executor slots, `ThreadStore`)

### assembly
`sven-bootstrap` (`RuntimeBuilder` - the one kernel-assembly point;
`Config` - the sections a session is built from, with `AgentConfig` and
`ToolsConfig`; `mode_registry` - the modes this build can run; the feature
presets) ·
`sven-commands` (`SlashCommand` trait + builtins)

### wiring
`sven-frontend` (shared frontend layer: `SessionController` - the one
controller of a session's lifecycle, which the TUI, the headless runner and
the ACP server all open their sessions through; `Settings` - the whole
configuration file, with the `tui:` section's `TuiConfig`; the `agent`
session task and `SessionEvent` consumption)

### sdk
`sven-sdk` (the public framework surface: `Engine`, `Agent`, `AgentState`,
`Method<T>` - what an application depends on to run agents; see
[docs/technical/sdk.md](docs/technical/sdk.md)) · `sven-sdk-macros`
(foundation tier: the `#[agent]` attribute, re-exported as `sven_sdk::agent`)

### surface
`sven-ci` (headless runner: `RuntimeRunner` + workflow orchestration) ·
`sven-mcp` (MCP server) · `sven-tui` (ratatui TUI)

`sven-harness-testkit` (offline case manifests, fixture models and tools,
runner and comparer for harnesses; mock models unless explicitly opted in -
behind `sven harness`, see [docs/12-harnesses.md](docs/12-harnesses.md)).

### composite
`sven-acp` (ACP server for IDEs)

### binary
`sven` (the root crate: CLI parsing + dispatch into `run::*`/`cli::*`
modules, one per subcommand group)

## Architecture

```
 sven (CLI/TUI)          sven-ci (headless)        sven-acp / sven-mcp
      │                        │                          │
 sven-tui                RuntimeRunner              per-session kernel
      │  SessionEvent          │                          │
      └──── sven-frontend ─────┴──────────────────────────┘
          (SessionController: open / send / rebuild)
                  │
              sven-bootstrap (RuntimeBuilder)  ◄─── one assembly point
                  │
            sven-hsm (kernel: pure transitions → Vec<Effect>)
           /        |          \
     sven-machines  sven-model  sven-executors ──────► sven-tool-registry
     (machines)   (LLM svc)   (ONLY I/O layer)
```

Everything above `EffectExecutor::execute` is pure and deterministic; everything
below it is I/O. `Effect` and `SessionEvent` are `serde`-serializable, which is
what makes replay and the audit ledger possible.

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
**First ask whether it belongs in this repo at all.** An application can
register its own tools on an engine (`sven_sdk::EngineBuilder::tool`) without
touching any crate here, and the same goes for machines
(`EngineBuilder::machine`). Add it below only if it is genuinely part of sven's
own capability set.

Tool implementations live in the domain-tier `sven-tools-*` crates, split by
concern (see the crate table above). `sven-tool-api` holds the `Tool` trait
and `sven-tool-registry` the registry; neither holds a concrete tool.
1. Pick (or create) the right `sven-tools-*` crate for the new tool's concern;
   implement `Tool` (+ `summary` - a line of at most 100 characters, which is
   all the model knows of the tool until it loads it, and which a test
   requires of every registered tool -, `parameters_schema`,
   `kernel_capability` - required, the widest effect any call has -
   `call_capability` when actions differ in effect, `default_policy`,
   `modes`) and `ToolDisplay`. A new tool is *deferred* by default; only
   `sven_tool_registry::CORE_TOOLS` is sent in full with every request, and
   adding to it needs a measured reason (see
   [docs/technical/tool-disclosure.md](docs/technical/tool-disclosure.md)).
2. Register it: `sven-bootstrap`'s tool-registry assembly
   (`crates/bootstrap/src/registry.rs`).
3. If exposed over MCP: `mcp/src/registry.rs`'s `DEFAULT_TOOL_NAMES` -
   deliberately a curated allowlist (stateful/TUI-dependent tools are
   intentionally excluded), not something to auto-derive from linked crates.
4. If it needs a new capability: see "Add a `ToolCapability`".
5. Tests: unit test + a bats case in `tests/e2e/basic/` if it has headless output.

### Change which tool definitions a request carries
`ToolRegistry::offer` (`tool-registry/src/disclosure.rs`) is the one place
that decides: the machine's named tools, else the mode's core tools followed by
the session's loaded tools in load order (deferred disclosure), else every
tool of the mode (`full`). The loaded set is `Context::active_tools`, grown by
`Event::ToolsActivated` in `Hsm::dispatch` and copied onto every turn request
as it leaves a dispatch; never keep it in a machine, an executor or a tool.
`TurnExecutor` and the SDK's `take_offered_tools` both go through `offer`, so a
record of a turn and the turn cannot disagree.

### Add a decision point
A decision model only advises; see [docs/technical/decisions.md](docs/technical/decisions.md).
1. Declare its `DecisionPolicy` (`sven-decision::policy`): the one question
   shape, input budget, thresholds, impact (anything but `routine` is
   advisory-only by construction) and fallback. Thresholds belong to the
   decision point, never to a shared constant.
2. Ask through `decide_batched` (questions about one state travel in one
   call), judge with `evaluate` (and `agree` when options are asked in two
   orders), and on abstention or failure return exactly the legacy result.
3. Record `DecisionTelemetry::record_call` and `record_trace` for every ask,
   and report a failure with `DecisionRuntime::diagnose` (once per session).
4. Gate it on a `decision.features.*` flag (`Feature`, `sven-decision::config`)
   and document the key in `docs/05-configuration.md`.
5. Test it with `MockDecisionProvider`: an abstention, a failure and a
   promotion must each be proven.

### Add a `ToolCapability` (permission bucket)
1. `hsm/src/permissions.rs` - the `ToolCapability` enum, `ALL`,
   `is_read_only` (what manual approval asks about), and the classify path.
2. Every machine's `permission_policy()` in `sven-machines` (`reactive_agent.rs`,
   `sdlc/mod.rs`) - decide allow/approval per state.
3. The tool's own `kernel_capability()` in its `sven-tools-*` crate.

### Add a new HSM machine / mode
1. `machines/src/machines/…` - implement `Machine`, reusing `loop_core` for the
   tool-loop plumbing.
2. **Implement `all_states()`, and keep all durable state in `Context` rather
   than in the machine's own fields.** Both are load-bearing, not optional
   hygiene: `all_states()` is how a snapshot's state label is mapped back to a
   state value, so a machine without it cannot be resumed at all, and a field
   the machine owns is not captured by a snapshot and silently reverts on
   resume. `crates/machines/tests/restorable.rs` fails the suite if you skip
   the first; nothing catches the second but a corrupted production session.
   See [docs/technical/resumable-agents.md](docs/technical/resumable-agents.md).
3. `machines/src/mode.rs::default_registry()` - register the mode string; a
   machine that drives one specific tool registers instead in
   `bootstrap/src/modes.rs` (`mode_registry`, behind the tool's feature) and
   gives its sessions that tool there (`register_mode_tools`).
4. Its `permission_policy()`.
5. `bootstrap/src/runtime_builder.rs` - any child-spawner wiring.
6. Config: `sven_vocab::AgentMode` if it's user-selectable.

### Add a harness primitive
A new primitive is a variant added to the closed `StepSpec` enum (ADR 0004);
it is a deliberate act across the whole stack, never an interpreter escape
hatch:
1. **Typed schema** - the variant and its step struct in
   `harness-spec/src/spec.rs`, its name in `PRIMITIVES`, and its shape in
   `harness-spec/schema/harness-document.schema.json` (the
   `document_schema` test holds the three together).
2. **Static validator** - `harness-spec/src/validate.rs`: references, the
   capability it exercises (`required_capability`), the limits it needs.
   Plan form in `plan.rs`.
3. **Runtime behaviour** - `harness-runtime/src/exec.rs` (or a new module
   beside it; `exec.rs` and `machine.rs` are near the file-size ratchet).
4. **Authorization** - the capability it needs in the plan's permission
   policy, and for child-starting primitives the depth and contract rules
   (`bootstrap/src/child_spawner.rs`).
5. **Structured tracing** - a `TraceRecord`, its `HarnessEvent` in
   `vocab/src/harness.rs` and `harness_event`; renderers per "Add a new
   `SessionEvent` variant".
6. **Unit and integration tests** - `harness-spec/tests`,
   `harness-runtime/tests`, `sdk/tests/harness_*.rs`.
7. **A runnable sample with cases** in `samples/harnesses/`, run by the
   testkit's `samples` test; document it in
   `docs/technical/harnesses.md` and `docs/12-harnesses.md`.

### Add a new model provider / driver
1. `model/src/registry.rs` - the `DRIVERS` table (env var, base URL,
   `requires_api_key`) - pure metadata, stays in `sven-model` even though the
   concrete implementations live in `sven-model-drivers`.
2. A driver module in `sven-model-drivers` (usually reuse `openai_compat`;
   bespoke only if the wire format differs).
3. `model-catalog/models.yaml` - model metadata.

### Change what a session/agent run looks like on a SURFACE
The kernel is one, and so is the controller of a session's lifecycle:
`frontend/src/controller.rs` (`SessionController`) is where a TUI, headless
or ACP session is assembled from the configuration, where its gates are wired
to whoever answers them (`Gates`), and where a mode or model change rebuilds it
with the conversation and the MCP servers carried over. A change to session
assembly, rebuilds or gates belongs there, once. What stays with each surface
is what it does with the events:
1. **Headless** - `sven-ci` (`KernelAgent`, `RuntimeRunner` + workflow
   orchestration): output formatting, exit codes, the trajectory.
2. **Interactive TUI** - `frontend/src/agent.rs` (`kernel_session_task`: the
   `AgentRequest` protocol; the TUI consumes `SessionEvent` as `AgentEvent`).
3. **Local ACP** - `acp/src/agent.rs`: the `session/update` mapping, ACP's
   permission requests.
   Grep guard: `grep -rn "RuntimeBuilder::new" crates` finds the one
   construction site outside `sven-bootstrap` and the SDK's per-turn `Agent`.

### Add a new `SessionEvent` variant (aliased `AgentEvent`/`UiEvent`)
`SessionEvent` lives in `sven-vocab` (foundation tier); `sven_hsm::UiEvent`
and `sven_machines::AgentEvent` are both plain aliases for it, not separate
types requiring a translator.
1. `vocab/src/lib.rs` (or wherever the variant's payload type
   belongs - payloads are pushed down to foundation-tier leaves, never the
   enum pushed up).
2. Every renderer that matches on it: `sven-frontend` (projection +
   renderers), `sven-tui`, `sven-ci` output (`runner/event.rs` trace
   tokens), and `sven-acp` notification mapping.
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
2. `sven-bootstrap` `SessionSupervisor` (principal→session ownership).

### Add a config field
`sven-config` knows no section; a field belongs to the crate that reads it:
provider and model settings to `sven-model-drivers` (`config.rs`), the MCP
servers to `sven-mcp-client`, a tool's settings to the crate of that tool
(`sven-tools-fs`, `-gdb`, `-web`, `-ctx`), the agent loop and the tools'
assembly to `sven-bootstrap` (`config/`), the interactive UI to `sven-frontend`
(`settings.rs`).
1. The section's struct in that crate (+ `#[serde(default)]` for back-compat)
   and its key in the section's `schema()`: a key the schema lacks is reported
   as unrecognised when the file loads.
2. Layering and `${VAR}` expansion are `sven-config`'s (`document.rs`); touch
   it only to change how files are found or merged.
3. A fixture and its golden record in `crates/frontend/tests/fixtures/config/`
   if the file format changes (`make generate/config/golden`, then review the
   diff); document the key in
   `docs/05-configuration.md`.

### Golden rules
- **One assembly point**: kernels are built by `RuntimeBuilder`, and the
  surfaces reach it only through `SessionController`. `grep -rn
  "RuntimeBuilder::new"` finds every construction site - use it as the
  completeness check for any change to how a session is assembled.
- **The `EffectExecutor` trait is the I/O seam.** New I/O = new/extended
  executor, never I/O in a transition.
- **Never duplicate frontend logic inside `sven-tui`** - shared code to `sven-frontend`.
- **Sven is a framework, and the CLI is one of its consumers.** The public
  surface belongs in `sven-sdk`; the kernel stays embeddable and resumable.
  `sven agent step` is built on that surface rather than on `RuntimeBuilder`,
  deliberately: it is the standing proof that the surface is sufficient for
  real work, so keep it that way rather than reaching past the SDK to fix
  something in it.
  Read [docs/adr/0003-agents-as-typed-objects.md](docs/adr/0003-agents-as-typed-objects.md)
  before changing the public API, adding a machine, or touching session
  lifecycle - it records which design principles are adopted, which were already
  satisfied by the kernel, and which are **rejected** (notably code-as-action
  execution, which routes around the typed-`Effect` permission gate). Rejections
  are there so they are not re-litigated or re-implemented by accident.
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
- [docs/technical/](docs/technical/) - HSM architecture, ACP, skill system,
  state machines, and the deliberation engine.
- [docs/adr/](docs/adr/) - architecture decision records. Start with
  [0003 - agents as typed objects](docs/adr/0003-agents-as-typed-objects.md),
  which defines the framework model the workspace is moving toward.
- [docs/technical/resumable-agents.md](docs/technical/resumable-agents.md) -
  suspending and resuming a session; snapshot vs. replay.
- [docs/technical/sdk.md](docs/technical/sdk.md) - the public framework
  surface: engines, agents, and suspended agent state.
- [docs/technical/harnesses.md](docs/technical/harnesses.md) and
  [docs/12-harnesses.md](docs/12-harnesses.md) - YAML agent harnesses: the
  language, runtime, authorization, replay and the `sven harness` CLI.
- [docs/technical/tool-disclosure.md](docs/technical/tool-disclosure.md) -
  deferred tool disclosure: core tools, the `tools` discovery tool, loading,
  ordering for the prompt cache, and the measured savings.
- [docs/technical/instruction-disclosure.md](docs/technical/instruction-disclosure.md) -
  skills, subagents and knowledge documents: stable ids, activation conditions,
  the active set in the kernel context, and what the system prompt lists.
- [docs/technical/token-ledger.md](docs/technical/token-ledger.md) - the
  per-request token ledger: where a request's input tokens come from, the
  `--token-ledger` line, `sven ledger`, and the baseline scenarios.
- [docs/technical/context-assembly.md](docs/technical/context-assembly.md) -
  addressable context: chunks, representations at four visibilities, the
  assembly policy, expansion as a recovery event and how to measure it.
- [docs/technical/delegation.md](docs/technical/delegation.md) -
  the `task` tool: briefs from typed evidence, structured results,
  deduplication of subgoals, the cost policy and the measured effect.
- [docs/technical/tool-definitions.md](docs/technical/tool-definitions.md) -
  the size of the tool definitions every request carries, how it is measured
  and the ceiling that pins it.
- [docs/technical/decisions.md](docs/technical/decisions.md) -
  the decision runtime: typed questions for a small decision model, policy,
  failure classes and the mock provider; the model only advises.
- `architecture.toml` - authoritative crate tiers, dependency legality, file-size
  ratchet. `.claude/skills/programming/rust/architecture.md` - the layering
  method this workspace follows, generalized for reuse elsewhere.
