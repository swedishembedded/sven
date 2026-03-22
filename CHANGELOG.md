# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Telegram (sven-node)**: scaffolding for Telegram integration.

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
- **GUI**: large UI refactor; removed standalone “current tool” display in favor of clearer chat-centric UX.
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
- **TUI drag/resize**: unified system — `SplitPrefs` extracted from `LayoutCache` for durable split dimensions; `anchor_offset` on `ResizeDrag` so borders track the grab point; `PeersSplitBorder` in `HitArea`; single `hit_test()` path.
- **Peers-split border drag**: use `peers_pane.y + peers_pane.height` as the sidebar bottom (fixes upward-drag snap to minimum).
- **Subagents**: inherit the **live model** from the parent.
- **Cache / tokens**: cache hit rate capped near ~49% from double-counted tokens — corrected.
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
- Chat list click no longer triggers wrong session’s segment actions (HitArea
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
- Garbled welcome logo — normalised row widths and fixed connector colours
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
