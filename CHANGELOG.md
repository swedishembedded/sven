# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **`trace`**: new crate (package name `trace`, deliberately without the `sven-` prefix) implementing the [ATIF v1.7](https://github.com/harbor-framework/harbor/blob/main/rfcs/0001-trajectory-format.md) agent-trajectory format end to end — full schema (multimodal content, subagent trajectory embedding/refs, context-management convention, RL-oriented token/logprob fields), spec validation, atomic whole-document JSON persistence with concurrent-modification detection, header-only fast reads for listing, and NDJSON step streaming.
- **sven-input**: `trace_session` module — ATIF-backed session storage (save/load/list) and bidirectional turn assembly between `sven_model::Message` streams and ATIF `TraceStep`s, replacing the previous YAML `ChatDocument`/JSONL `ConversationRecord` formats as the canonical persistence layer for headless runs, the TUI, and the GUI.

### Changed
- **BREAKING (sven-ci, CLI)**: `--jsonl`/`--load-jsonl`/`--output-jsonl` and `--chat`/`--load-chat`/`--output-chat` are replaced by a single `--trace`/`--load-trace`/`--output-trace` flag family backed by ATIF. `--output-format json` now emits the full ATIF trajectory document instead of the old bespoke JSON summary; `--output-format jsonl` streams ATIF `TraceStep` objects (one per line) instead of the old `ConversationRecord` shape. The auto-log path is now `.sven/logs/<timestamp>.atif.json` (a single JSON document) instead of `.sven/logs/<timestamp>.jsonl` (an append-only line stream).
- **BREAKING (sven-tui, sven-gui)**: interactive session storage moved from `~/.local/share/sven/chats/*.yaml` to ATIF trajectory documents under `~/.local/share/sven/sessions/*.json`. Pre-existing `.yaml` sessions are still discoverable and openable (read-only import); once reopened, subsequent saves write the new ATIF format.
- All crate directories dropped the `sven-` prefix (`crates/sven-hsm` → `crates/hsm`, etc.); Cargo package names are unchanged (`sven-hsm` still depends on `sven-hsm = { path = "../hsm" }`), so this does not affect downstream consumers of the published crates.

### Removed
- **sven-ci**: deleted the dead, never-wired `jsonl_export`/`write_jsonl_trace` fine-tuning-export module (zero callers outside its own tests).
- **E2E (sven-cloud)**: firmware wedge scenario (`crates/cloud/tests/e2e_firmware.rs`, bats `tests/e2e/cloud/02_firmware_wedge.bats`) — a companion exposing `shell` + the GDB tool suite completes a build → test → debug task end-to-end through the cloud against a hermetic in-process GDB remote-protocol target (real `gdb-multiarch`, no hardware), proving the "local hands with real tooling" wedge; includes an adversarial test that a hijacked control plane cannot repurpose the local hands past the companion allowlist/jail.
- **Companion (sven-companion)**: `CompanionPolicy` now allowlists command-less `ExecuteShell` tools (the GDB session tools `gdb_connect`/`gdb_stop`/…) by **tool name** instead of by shell command line, so a customer can opt into them explicitly (`gdb_connect`, `gdb_*`) while deny-by-default still holds.
- **Telegram (sven-node)**: scaffolding for Telegram integration.
- **Audit (sven-hsm/sven-executors)**: the kernel runtime now flushes the hash-chained audit log (`.sven/audit.jsonl`) after **every dispatch**, including the terminal one — machines no longer need to emit `PersistAudit` for records to reach disk.
- **Audit (sven-hsm)**: `ToolAuditRecord` now carries tenant/actor attribution (stamped via the new `Context::push_tool_audit`); `AuditTrailHandle` gained cursor-based `records_from`/`tool_records_from` accessors.

### Changed
- **BREAKING (sven-mcp 2.0.0, sven-acp 2.0.0)**: version bumps corrected from minor to major to reflect the behavior breaks already shipped in this cycle — `call_tool` deny-by-default for `Ask`-policy tools without a wired `PermissionRequester`, and TLS verification on by default in `serve_stdio_node_proxy`. Existing 1.x integrations that relied on unattended shell/write tools or self-signed nodes must opt in explicitly (`with_permission_requester`, `ConnectOptions::insecure_dev`/`extra_ca_pem`).
- **BREAKING (sven-node)**: `headless_policy()` no longer grants `WriteFile` or `GitOperation` to unattended P2P sessions — unsandboxed writes plus hook-executing git operations were equivalent to the `ExecuteShell` the policy denies. Unattended sessions are now read-only plus network; callers needing more must build an explicit `PermissionPolicy`.
- **Audit (sven-executors)**: `AuditExecutor` now serializes concurrent writers through an advisory `<log>.lock` file and re-reads the chain tip on every flush, so concurrent sessions on one workspace can no longer fork/corrupt the chain; partial writes are truncated and retried (at-least-once) instead of permanently breaking verification; legacy pre-chain log files are rotated to `<log>.legacy-<timestamp>` so new records remain verifiable; entry hashing uses an explicitly key-sorted canonical JSON form independent of serde_json build features.

### Fixed
- **Audit docs (sven-executors)**: module documentation no longer overclaims tamper-evidence — the hash chain detects accidental corruption and non-adaptive tampering only; an actor with write access can recompute the whole chain (no HMAC/anchoring is implemented).

### Added
- **Workspace**: `xtask` architecture-ratchet checker (`cargo run -p xtask -- arch`, wired into `make check`) plus `architecture.toml`, enforcing crate-tier dependency legality, dead/unused internal dependency detection, and a file-size ratchet across all workspace crates. Method documented at `.claude/skills/programming/rust/architecture.md`.

### Changed
- **Workspace**: all crate `version` fields now inherit `version.workspace = true` instead of drifting independently (was `sven-hsm` 0.5.0 next to `sven-acp`/`sven-node` 2.0.0 next to the workspace's own 1.10.2 — nothing is published separately, so per-crate versions carried no information).
- **sven-tools**: `SystemTool::new` takes an extra `Vec<ModelCatalogEntry>` parameter (the `switch_model` fuzzy-search catalog). `sven-tools` no longer depends on `sven-model` in production — that was its only call site (`static_catalog()`, used to fuzzy-match a `/model` query against provider/id/name). `sven-bootstrap` (which already depends on both) builds the catalog and passes it in; `sven-model` moves to `sven-tools`'s `[dev-dependencies]` for the tests that exercise real match scoring.

### Removed
- **Workspace**: removed 13 internal dependencies the architecture checker found with zero real use sites — 3 from a manual audit (`sven-acp`→`sven-node`, `sven-frontend`→`sven-executors`) plus 11 more the checker itself surfaced on its first run: `sven`→`sven-core`; `sven-acp`→`sven-model`; `sven-channels`/`sven-integrations`/`sven-memory`/`sven-scheduler`→`sven-config`; `sven-llm`→`sven-hsm`; `sven-memory`→`sven-model`; `sven-node`→`sven-runtime`; `sven-team`→`sven-p2p`; `sven-ci`→`sven-frontend`. Moved `sven-node`'s dependency on `sven-frontend` to `[dev-dependencies]` (used only by a test).
- **BREAKING (CLI, sven-gui)**: deleted `crates/gui` (the Slint desktop GUI), the `sven --gui`/`-g` CLI flag, the `sven.desktop` application-menu launcher, and the `.agents/skills/slint-gui` skill. The TUI (`sven` with no flags, in a terminal) is the only interactive local surface; headless (`--headless`/`-H`) and the P2P node/cloud surfaces are unaffected. `libfontconfig-dev` is no longer required to cross-compile for aarch64 (`Cross.toml`).
- **sven-graph, sven-core**: deleted the graph-machine DSL (`crates/graph` + `core/src/machines/graph/`, 3116 LOC) that `AGENTS.md` had instructed contributors to prefer for new agent behavior. It was never adopted (`GraphMachine::` was constructed only in its own tests, no `.graph` assets ever existed, `ModeRegistry` never registered one) and had active correctness bugs (its template renderer corrupted non-ASCII text; its "final" event pattern matched any completion including timeouts; its timer cancellation cancelled the wrong timer). See `docs/adr/0001-delete-graph-dsl.md`. `core/src/machines/loop_core.rs` (already shared by `ReactiveAgentMachine` and `SdlcMachine`) remains the way to add a new machine.
- **sven-core**: deleted `AgentEventVisitor` (24 methods, `core/src/events.rs`) — a visitor trait written to solve the same "adding an event variant silently drops it in some consumer" problem the parallel `UiEvent`/`AgentEvent` enums have, with zero implementors anywhere in the workspace. Its all-default-no-op-methods shape reproduces the exact silent-drop failure mode it was meant to prevent, just less visibly than a wildcard match arm. `AgentEvent` and its consumers are unaffected; a real fix for the underlying problem is tracked for the event-model-unification phase of the current refactor.
- **sven-frontend**: deleted `frontend/src/control.rs`, a hand-maintained, stringly-typed mirror of the node/cloud control protocol, kept only because (per its own doc comment) `sven-frontend` "sits below `sven-node` in the crate graph, so it cannot import the canonical types" — stale: `sven-control` was extracted as a standalone crate specifically so `sven-frontend` could depend on it directly, which `share.rs` already did while `operator.rs`/`node_agent.rs` kept using the mirror. The mirror's `SessionInfo.state: String` had forced a third parallel enum (`SessionPhase`, now deleted) to re-parse the wire string that `sven_control::SessionState` already models directly. `sven_control::ControlEvent` gains a `#[serde(other)] Unknown` catch-all (the mirror's resilience property for a real network boundary, now available to every consumer, not just the frontend) so this loses no behavior. `crates/node/tests/control_mirror.rs`, a drift guard between the two, is deleted as moot.
- **Workspace**: merged 10 copies of the same auto-approve `tokio::select!` loop (CI's two headless runners had byte-identical same-named `auto_approve` functions; the node, `sven share`, and 6 test/demo call sites each hand-rolled the same 15-line block inline) into `sven_bootstrap::KernelChannels::auto_approve()`. Every unattended-session call site now reads `tokio::spawn(bundle.channels.auto_approve())`.
- **sven-bootstrap, sven-ci**: fixed two `RuntimeContext` duplication bugs. `RuntimeContext::auto_detect()`'s skill/agent/knowledge/git/CI detection is now reachable at an already-resolved project root via the new `auto_detect_at`/`auto_detect_with`, replacing two hand-written `RuntimeContext { .. }` literals in `sven-ci` that had drifted from the real detection logic: `RuntimeRunner`'s copy hardcoded `knowledge_drift_note: None`, so `--output-trace`/one-shot runs never warned about stale knowledge docs while `CiRunner` runs did (both now share one code path and both warn). Separately, the TUI's `kernel_session_task` accepted `shared_skills`/`shared_agents` parameters and silently discarded them (`_`-prefixed) in favor of calling `RuntimeContext::auto_detect()` fresh — a second full skill/agent discovery pass on every TUI startup. It now reuses what the TUI already discovered via `RuntimeContext::auto_detect_with`. Regression tests added in `bootstrap::context::tests` for both.

### Added
- **sven-bootstrap**: `KernelChannels::auto_approve()` — the canonical unattended-session gate consumer (see above).

### Fixed
- **sven-node**: a session that ended via a kernel-internal `UiEvent::Aborted` (a cancellation or watchdog timeout that didn't go through the explicit `CancelSession` command) was marked `Completed` in the node's session table and never broadcast a `ControlEvent::SessionState` at all — every connected operator saw the session simply stop updating, indistinguishable from a normal finish, instead of `Cancelled`. `cloud/src/runtime.rs` already hand-compensated for the equivalent gap in `ui_event_to_control` (which has no mapping for `Aborted`); the node's observation-drain task now carries an `aborted` flag through its internal completion channel and applies the same compensation `handle_cancel`'s explicit path already did. Regression test: `aborted_completion_sets_cancelled_and_broadcasts_it`.

### Added
- **sven-chain**: new crate — the sha256 hash-chain primitives (`append_chain`/`read_chain`/`verify_chain`, `ChainedLine`, `ChainError`, `GENESIS_HASH`) extracted verbatim out of `sven-executors`' `audit.rs`, with zero dependencies on other sven crates. This was the entire reason `sven-metering` (pricing/billing) and `sven-companion` (customer-premises local hands) depended on the effects/I/O executor layer: neither needed anything else from it.

### Changed
- **sven-executors, sven-metering, sven-companion**: `AuditExecutor` now builds on `sven_chain` instead of defining the chain format itself; `sven-metering`'s credit ledger and `sven-companion`'s local audit copy do the same. Deletes the `sven-metering → sven-executors` and `sven-companion → sven-executors` (production) dependency edges — companion's remaining need for `sven-executors::RemoteToolExecutor` is test-only (verifying the cloud-sandbox loopback path), so that dependency moves to `[dev-dependencies]`. `sven-executors` drops its now-unused `sha2`/`serde`/`thiserror` dependencies (they backed only the code that moved).
- **sven-vocab**: new crate — `ToolCall`, `ToolOutput`/`ToolOutputPart`, `ToolSchema`, and `OutputCategory` moved out of `sven-tools` into a new zero-dependency crate below it. `sven-tools` re-exports all four at its crate root unchanged, so every existing `sven_tools::ToolCall` (etc.) call site across the workspace needed no changes. `sven-wire` and `sven-control` — both of which needed only these pure data types, not the tool-execution/registry machinery — now depend on `sven-vocab` instead of `sven-tools`, deleting both `wire → tools` and `control → tools` upward-tier edges (they were the last two `[[allow.upward]]` exceptions in `architecture.toml`; none remain).
- **sven-llm**: `ConversationStore` renamed to `ThreadStore` (`crates/llm/src/conversation.rs` and all 11 call sites). `sven-p2p` has its own, unrelated `ConversationStore` (a P2P message store); the two shared a name but not a concept — a "one name, one concept" violation caught during the original audit. `sven-p2p`'s type is untouched.

### Changed
- **sven-vocab**: `AgentMode` (from `sven-config`), `CompactionStrategyUsed`/`PeerInfo` (from `sven-core`), `TodoItem`/`TodoStatus`/`SubagentUpdate` (from `sven-tools`), and `CollabEvent` (from `sven-core`) moved down into `sven-vocab`, alongside `ToolCall`/`ToolOutput`/`ToolSchema`. Every old location re-exports its type unchanged, so no downstream call sites needed to change. This is the prerequisite for Phase 3 of the refactor plan (unifying `AgentEvent`/`UiEvent` into one `SessionEvent`): that enum has to live in a foundation-tier crate below both `sven-core` and `sven-hsm`, so its payload types have to live there first. `sven-config` gains a `sven-vocab` dependency (a new same-tier `[[same_layer]]` exception in `architecture.toml`: both are leaf "foundation" crates with no dependents at that tier, so it can never become a cycle).
- **BREAKING (sven-core, sven-hsm)**: `sven_core::AgentEvent` and `sven_hsm::UiEvent` are now both re-exports of one type, `sven_vocab::SessionEvent`. These had been two independently hand-maintained enums bridged by a translator pair (`agent_event_to_ui` / `ui_event_to_agent_event`) that had already drifted — `ui_event_to_agent_event`'s catch-all silently dropped any event neither side had gotten around to mapping. `SessionEvent` keeps `AgentEvent`'s typed payloads (so consumers pattern-match on real types, e.g. `ToolCall`, `Vec<TodoItem>`, `AgentMode`, `SubagentUpdate`, instead of opaque `serde_json::Value`/`String`) plus `Transition`, the kernel's per-dispatch trace event that had no `AgentEvent` equivalent. Both translators are now the identity function `Some(ev)` — kept in place rather than deleted outright, per the migration plan's identity-transform pattern for a risky enum merge: land the type unification first with everything still compiling, verify green, delete the now-pointless indirection in a follow-up commit. Every direct consumer of the old shapes was migrated in this same commit: `sven-control`'s `ui_event_to_control`, `sven-executors`' turn/tool/remote-tool emitters, `sven-bootstrap`'s tool-event forwarder, `sven-ci`'s headless runners, and both ACP bridge functions.
- **sven-acp**: `ui_event_to_session_update` now delegates to `agent_event_to_session_update` instead of duplicating it against a separate, already-diverged `UiEvent` shape. The duplicate had a `ModeChanged` handler that pattern-matched on a lowercase-only string (`"research"`, `"plan"`, else `Agent`) — since `ModeChanged` now carries a typed `AgentMode` with no string round-trip at all, this also fixes the live bug where a kernel-driven Research or Plan session reported itself to the IDE as Agent mode.

### Removed
- **sven-executors, sven-bootstrap**: deleted `agent_event_to_ui`/`ui_event_to_agent_event`, the two `AgentEvent`↔`UiEvent` translators, now that both names are re-exports of the same `sven_vocab::SessionEvent` and the previous commit had already degenerated both to `Some(ev)`. Their three call sites (`sven-executors`' turn-stream forwarder, `sven-bootstrap`'s observation bridge, `sven-ci`'s `KernelAgent`) now forward the event directly. Deleted the three `kernel_bridge` tests that only exercised the translator round-trip (`maps_full_turn_ui_event_sequence`, `subagent_started_survives_kernel_round_trip`, `collab_event_survives_kernel_round_trip`) — with no translation happening, they had degenerated into asserting a value equals itself.

## [1.9.0] - 2026-03-22

### Added
- **sven-frontend**: shared agent-wiring layer extracted from `sven-tui` for reuse across frontends.
- **sven-gui**: Slint desktop GUI; **`sven-ui` merged into `sven --gui`** (single binary).
- **GUI**: session persistence; **lazy chat loading**; **per-session token usage** persisted; full **markdown** in the chat view; markdown in **thinking and tool result** bubbles; scrollable tool bubbles; **todo** tool results in chat; vertical chat layout; sidebar **search** filter; SearchInput/SearchBar styling; Slint GUI skill.
- **Channels (`sven-channels`)**: `Channel` trait and multi-platform adapters; E2E integration tests for channel → manager → reply.
- **Scheduler (`sven-scheduler`)**: cron, interval, and one-shot jobs.
- **Integrations (`sven-integrations`)**: email, calendar, and voice.
- **Memory (`sven-memory`)**: SQLite store with FTS5 semantic search.
- **Config**: schema for channels, scheduler, email, calendar, voice, memory, and webhooks.
- **Node**: generic **webhook** endpoints; proactive agent integrations wired through bootstrap and tool registry.
- **Skills**: load system-installed skills from `/usr/share` and `/usr/local`; **always reload skill content from disk** when the agent loads a skill.

### Fixed
- **GUI**: nested-runtime panic on `--gui`; Slint layout, interaction, display, ask-question UX, picker, queue, spinners, markdown wrapping, session list busy indicator, completion, errors, clear, highlighting (multiple rounds of fixes).
- **TUI**: markdown aligned with GUI (block quote vs list); first completed todo item no longer shows a stray bullet.

### Changed
- **GUI**: large UI refactor; removed standalone "current tool" display in favor of clearer chat-centric UX.
- **Docs**: README condensed and expanded with accurate feature lists; user guides for channels, scheduler, email, calendar, voice, memory, webhooks, and use cases; **AGENTS.md** updated for sven-frontend, sven-gui, and dual-binary layout.

## [1.8.1] - 2026-03-15

### Added
- **Tests (`sven-model`)**: coverage that the full MCP tool schema is passed through to the model API.

### Fixed
- **MCP**: omit `null` `capabilities` in `initialize` for strict servers; show **disabled** status when `enabled: false` in config.
- **MCP**: wait for tools in headless mode and refresh on `ToolsChanged`.
- **TUI**: compile fixes for **ratatui 0.30**; `CompletionOverlay` viewport now driven by `ListState` (removed fixed `max_visible`).

## [1.8.0] - 2026-03-14

### Added
- **MCP client**: broad client-side MCP server support (SSE, sessions, OAuth integration).
- **MCP OAuth**: PKCE flow with auto-auth, token lifecycle, and **scope discovery** (no manual scope lists).
- **CLI**: `sven mcp` auth flow; default OAuth redirect **`sven://sven.mcp`** with container fallback; **`cursor://`** support.
- **Release**: `make` release targets accept **`--no-confirm`** for non-interactive runs.

### Fixed
- **MCP / OAuth**: RFC-compliant PKCE; stop perpetual auth loops; trigger OAuth on **401**; skip OAuth flows in **headless/CI**; improved SSE and session handling.
- **Config**: recognize `mcp_servers`; fix **SKILL.md** frontmatter parsing.

### Changed
- **Headless**: apply settings on startup; reduce unlabelled noise; show context path.
- **TUI**: focus the input pane when it is clicked.

## [1.7.5] - 2026-03-13

### Added
- **TUI**: **conversation cost** in the status bar.
- **GDB**: `gdb_start_server` Makefile target made target-agnostic.
- **Docs**: `AGENTS.md` added.

### Fixed
- **Tokens / UI**: status bar token display; subagents run at correct depth; restore subagent chat hierarchy on restart with new chat at top; suppress noisy tracing when a subagent runs `acp serve`.
- **Subagents & sessions**: subagent UX, agent error handling, and background session completeness.
- **sven-config**: default model auto-detection priority (**OpenRouter** first) with regression test.

### Changed
- **CLI**: auto-enable **headless** mode when a **positional prompt** is provided.
- **TUI**: focus the chat list on click so keyboard navigation applies immediately.

## [1.7.4] - 2026-03-12

### Added
- **OpenRouter Auto & Free routers**: `openrouter/auto` and `openrouter/free`
  are registered in the model catalog and resolvable via `--model
  openrouter/auto` / `--model openrouter/free`. `openrouter/auto` is the
  default out-of-the-box model (replacing `openai/gpt-4o`) when
  `OPENROUTER_API_KEY` is available. `auto_router_allowed_models` in
  `driver_options` maps to the nested `plugins` structure expected by the
  OpenRouter Auto Router API.
- **Benchmark**: Terminal-Bench 2.0 evaluation via Harbor.

### Fixed
- **TUI drag/resize**: unified system - `SplitPrefs` extracted from `LayoutCache` for durable split dimensions; `anchor_offset` on `ResizeDrag` so borders track the grab point; `PeersSplitBorder` in `HitArea`; single `hit_test()` path.
- **Peers-split border drag**: use `peers_pane.y + peers_pane.height` as the sidebar bottom (fixes upward-drag snap to minimum).
- **Subagents**: inherit the **live model** from the parent.
- **Cache / tokens**: cache hit rate capped near ~49% from double-counted tokens - corrected.
- **CI**: record resolved model in chat output documents.

### Changed
- **Model catalog**: `models.yaml` parsed once per process via `OnceLock` `catalog_ref()`; lookups use the cached slice instead of cloning on every call.
- **Model provider**: `check_api_key_requirement` and `transform_openrouter_options` split into private helpers in `lib.rs`.
- **Repository**: `.gitignore` extended for Python `__pycache__` paths.

## [1.7.3] - 2026-03-11

### Added
- **Compound system tools**: built-in tools consolidated into action-dispatched
  compound tools for cleaner tool surface and fewer tool slots.

### Fixed
- Live timestamp removed from stable system prompt to avoid unnecessary prompt
  churn and improve caching.
- Model and mode transitions now apply immediately in the TUI instead of
  waiting for the next user message.
- OpenRouter: Anthropic prompt caching enabled for Anthropic models.
- `switch_model` takes effect in the next model turn (correct staging behavior).
- Mode upgrades from within a conversation are now allowed (tools no longer
  block mode changes).
- Grey-on-grey rendering artifacts in pager and chat view eliminated.
- Command prompt for `/command` is loaded from disk on each run so edits are
  picked up without restart.
- Modifier+click on chat content is ignored so the terminal can open links.
- OpenSSL cross-compilation for macOS and Linux aarch64 in CI.

## [1.7.2] - 2026-03-11

### Fixed
- E2E tests: replaced hardcoded local paths and forward `context_open` args in CI.

## [1.7.1] - 2026-03-11

### Fixed
- Clippy: remove empty line after doc comment in `task_tool.rs`.
- CI: centralize build commands via Makefile and fix macOS OpenSSL cross-compilation.

## [1.7.0] - 2026-03-10

### Added
- **ACP (Agent Client Protocol)**: full protocol compliance across server and
  client roles; `--model`/`--provider` for `acp serve`, model inheritance.
- **Unified todo tool**: replaces `todo_write` with a single tool supporting
  read/add/update/set actions.
- **Chat tree view**: subagent sessions shown as children in the TUI.
- **ToolDisplay trait**: chat view tool labels and summaries use shared display logic.
- **LLM-generated chat titles** and delete-active-chat; animation updates.
- **Compound tools**: 42 built-in tools consolidated into 14 compound tools.
- **P2P peers pane** in TUI; keyboard-first segment actions (per-line icons removed).
- Website: overhauled copy, SEO, and section SVGs; logo and hero font (JetBrains Mono).
- Comprehensive adversarial test suite (64 Rust + 25 Bats tests).

### Fixed
- Peers pane resize drag, peer list population, and P2P dial noise.
- Subagent exit code, chat pane keybindings, peers-split drag direction.
- Full tool result shown at expand level 2 in TUI.
- Task tool returns result immediately when PromptResponse arrives.
- Thought block display and subagent user message.
- Subagent inactivity timeout during long tool calls (bootstrap).
- Chat display and welcome screen after streaming refactor.
- Tool rendering, shell intent, subagent blocking, and character width (CJK ambiguous).
- Chat title race so LLM-generated title is used when available.
- Merge positional prompt with stdin in CI runner (not in main); workflow only for `-f`.
- File deletion (sven-input); trailing empty lines in segments; delete as default button.
- Build errors: `From<anyhow::Error>` for `FileModifiedError`, `ToolDisplayInfo` export.
- Pre-commit runs clippy only on staged files.

### Changed
- Streaming seasoning/thinking shown without backticks in gray dim text; thinking
  content preview instead of word count; tool scan animation sinusoidal;
  streaming cursor blink ~500ms; timeouts to prevent indefinite hangs.
- ACP used for subagent communication with structured streaming.
- Refactor: eliminate duplication and decouple crate architecture.

## [1.6.0] - 2026-03-09

### Added
- **Multi-session TUI**: run several conversations simultaneously without
  restarting sven. Collapsible chat list sidebar (toggle with `Ctrl+B`) with
  live status; `n` new session, `Enter` switch, `d` delete, `a` archive.
- **Background sessions**: switching away does not interrupt the agent; spinner
  shows running sessions; events buffered when you return.
- **YAML persistence**: sessions saved to `~/.config/sven/history/<id>.yaml`,
  restored on launch; last active session restored on startup.
- Per-session model and mode; mouse drag to resize chat list sidebar.
- **Inspector overlay**: skills, subagents, peers, context; `/tools` inspector
  with node-proxy support.
- **Parallel tool slots** (sven-core): streaming dispatch with multiple
  concurrent tool slots.
- Vim-style pane navigation (focus chat list / main pane).

### Fixed
- `/new` now creates a proper new session in the sidebar with isolated agent
  (no bleed from old session).
- Per-session `/model` and `/mode` state; queued messages and `abort_pending`
  cleared on new session; message-edit state cleared on new session.
- JSONL log path tracked per-session; state leakage on new sessions fixed
  (input, queue, edit state, agent state isolated).
- Chat list click no longer triggers wrong session's segment actions (HitArea
  hit-test); duplicate agent on initial session removed; message loss on exit
  fixed (session state saved).
- `wait_for_message` no longer drops replies when peer responds before waiter
  registers.
- Neovim double-response bug; inspector and pager UX improved.
- TodoUpdate segment ordering in tool output; chat pane selection and
  scrollbar ghost/stuck rendering.

### Changed
- Chat document formatting (sven-input); mouse routing centralized (HitArea);
  inspector overlay dead `kind` field removed; docs for parallel tool slots and
  multi-session TUI.

## [1.5.0] - 2026-03-09

### Added
- **Team orchestration** in sven-node with layered tool architecture (sven-team
  crate + TUI).

### Fixed
- Team tool confusion that caused the model to duplicate work and misuse APIs.
- Teammate task execution and node restart resilience.

## [1.4.0] - 2026-03-08

### Added
- **ACP (Agent Client Protocol) server** for IDE integration (e.g. Zed).
- **Agent team orchestration** (sven-team crate + TUI); multi-agent orchestration
  roadmap.
- Site: install script at install route; install action and package updates.

### Fixed
- ACP bridge: drop TextComplete/ThinkingComplete to prevent double output.
- Chat view display and find_file glob matching; subagent model wiring.
- Team picker keys through dispatch and missing key bindings.
- Clippy lints; pre-commit fails on format changes.
- CI and installation route fixes.

### Changed
- Docs: Zed ACP config snippet (`agent_servers`, not `assistant.provider`);
  README mentions ACP IDE integration.

## [1.3.2] - 2026-03-07

### Added
- `sven tool list` and `sven tool call` commands for direct tool invocation from the CLI
- Support for executing inline `<invoke>` tool calls emitted by MiniMax and similar models
- Subagent streaming via process-based TaskTool (replaces in-process execution)
- `/clear` and `/new` TUI commands for conversation management
- Cache hit percentage displayed inline after token counts in the status bar
- Cumulative session token totals shown instead of per-turn counts
- Context percentage now uses cumulative tokens for accuracy

### Fixed
- Unicode corruption when multi-byte characters span SSE chunk boundaries
- Token tracking for multi-call turns; simplified completion menu
- Correct `ctx%` denominator; exact provider token counts shown in status bar
- Grouped segment rendering and hardcoded test paths in TUI
- `/install` endpoint now served as plain text instead of SPA fallback

### Changed
- Site container runtime switched from nginx to `serve`
- Built-in tools refactored into categorised modules
- Types hardened and boilerplate eliminated across the core agent codebase
- Site placeholder images replaced with real sven SVG illustrations

## [1.3.0] - 2026-03-05

### Added
- **RLM context tools**: memory-mapped large-content analysis with `context_query`, per-sub-query timeouts, and real-time UI drain
- **TUI UX overhaul**: clean hierarchical agentic interface with improved input handling, multiline paste, and rendering fixes
- Clock-driven animations for thinking indicator, tool-scan, and stream cursor in TUI
- Shell-style input history in TUI
- Markdown table rendering in TUI
- Mouse drag selection in TUI
- `max_output_tokens` and `max_input_tokens` config fields per model
- Provider-first config structure with environment variable expansion
- React marketing landing page for [agentsven.com](https://agentsven.com)
- Install script served at `/install` from the site container
- Latest release version injected at site build time
- 70 integration tests for RLM context tools on real data
- 39-test end-to-end suite for `edit_file` tool
- bats end-to-end tests for context tools and error handling

### Fixed
- Garbled welcome logo - normalised row widths and fixed connector colours
- Welcome screen logo colours and tagline URL
- Multiline paste display, completion double-slash, and rendering artefacts
- Mid-turn mode consistency and mode display in status bar
- aarch64 OpenSSL build in CI (non-fatal fallback)

## [1.2.3] - 2026-03-03

### Fixed
- TUI node-proxy mode: connect, stream, and lock model/mode to node correctly
- Node-proxy TUI now forwards Resubmit events to the node (streamed responses)
- Stale waiter slots in the session multiplexer
- Session depth accumulation regression; aarch64 release build

### Changed
- Renamed `gateway` → `node` across the entire codebase for naming consistency

## [1.2.2] - 2026-03-03

### Fixed
- All circular message-loop paths across P2P channels (three separate loop vectors closed)
- Infinite session echo loops and delegation chain corruption in P2P network
- Dual-delivery of messages on the P2P session bus

### Changed
- Removed model auto-nudge from sven-core
- Updated dependency versions across Cargo workspace

## [1.2.1] - 2026-03-03

### Fixed
- Node model defaults, error messages, and CLI ergonomics improvements

## [1.2.0] - 2026-03-03

### Added
- **Agent-to-agent collaboration** via libp2p session messaging and named rooms
- **Browser web terminal** on sven-node with WebAuthn passkey authentication and PTY sessions
- **Plug-and-play TLS** for sven-node using Tailscale and a local CA

### Changed
- Complete overhaul of sven-tui with a modern ratatui architecture

## [1.1.0] - 2026-03-01

### Added
- MCP server support via `sven mcp serve`
- Node-proxy mode for `sven mcp serve` (proxies MCP requests through a remote node)

### Changed
- Updated SPDX licence tags across all crates

## [1.0.4] - 2026-03-01

### Fixed
- macOS OpenSSL build linkage

## [1.0.3] - 2026-03-01

### Fixed
- CI pipeline and release workflow errors

## [1.0.2] - 2026-03-01

### Added
- Initial public release
- Multi-arch release pipeline with CI builds for Linux x86_64/aarch64 and macOS, plus curl-pipe install script
- Codified context infrastructure: three-tier knowledge base for large codebases
- Agent-to-agent task routing over P2P with named rooms
- sven-node heartbeat, separate control-plane configuration, and configurable agent listen address
- Peer allow-list and mDNS local discovery for P2P networks
- `find_file` tool (unified from `glob` / `glob_file_search`)
- Apache 2.0 licence

### Fixed
- 12 P2P security vulnerabilities for hostile network deployment
- Circular delegation false-positive caused by empty peer ID in P2P
- Agent stall nudge firing on legitimate single-tool-call + answer patterns

[Unreleased]: https://github.com/bosun-ai/sven/compare/v1.9.0...HEAD
[1.9.0]: https://github.com/bosun-ai/sven/releases/tag/v1.9.0
[1.8.1]: https://github.com/bosun-ai/sven/releases/tag/v1.8.1
[1.8.0]: https://github.com/bosun-ai/sven/releases/tag/v1.8.0
[1.7.5]: https://github.com/bosun-ai/sven/releases/tag/v1.7.5
[1.7.4]: https://github.com/bosun-ai/sven/releases/tag/v1.7.4
[1.7.3]: https://github.com/bosun-ai/sven/releases/tag/v1.7.3
[1.7.2]: https://github.com/bosun-ai/sven/releases/tag/v1.7.2
[1.7.1]: https://github.com/bosun-ai/sven/releases/tag/v1.7.1
[1.7.0]: https://github.com/bosun-ai/sven/releases/tag/v1.7.0
[1.6.0]: https://github.com/bosun-ai/sven/releases/tag/v1.6.0
[1.5.0]: https://github.com/bosun-ai/sven/releases/tag/v1.5.0
[1.4.0]: https://github.com/bosun-ai/sven/releases/tag/v1.4.0
[1.3.2]: https://github.com/bosun-ai/sven/releases/tag/v1.3.2
[1.3.0]: https://github.com/bosun-ai/sven/releases/tag/v1.3.0
[1.2.3]: https://github.com/bosun-ai/sven/releases/tag/v1.2.3
[1.2.2]: https://github.com/bosun-ai/sven/releases/tag/v1.2.2
[1.2.1]: https://github.com/bosun-ai/sven/releases/tag/v1.2.1
[1.2.0]: https://github.com/bosun-ai/sven/releases/tag/v1.2.0
[1.1.0]: https://github.com/bosun-ai/sven/releases/tag/v1.1.0
[1.0.4]: https://github.com/bosun-ai/sven/releases/tag/v1.0.4
[1.0.3]: https://github.com/bosun-ai/sven/releases/tag/v1.0.3
[1.0.2]: https://github.com/bosun-ai/sven/releases/tag/v1.0.2
