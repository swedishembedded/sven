# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Multi-session TUI**: run several conversations simultaneously without
  restarting sven. A collapsible chat list sidebar (toggle with `Ctrl+B`)
  shows all open sessions with live status indicators. Press `n` to create a
  new session, `Enter` to switch, `d` to delete, `a` to archive.
- **Background sessions**: switching away from a session with an active agent
  does not interrupt it. A spinner in the sidebar shows which sessions are
  still running. Background events are buffered and displayed when you return.
- **YAML-based persistence**: every session is automatically saved to
  `~/.config/sven/history/<id>.yaml` and restored on next launch; last active
  session is restored on startup.
- Per-session model and mode: each chat remembers its own `/model` and `/mode`
  settings; changes in one session do not affect others.
- Mouse drag to resize the chat list sidebar.
- **Inspector overlay**: view skills, subagents, peers, and context in a
  dedicated overlay; `/tools` inspector with node-proxy support.
- **Parallel tool slots** (sven-core): streaming dispatch with multiple
  concurrent tool slots.
- Vim-style pane navigation (e.g. focus chat list / main pane).

### Fixed
- `/new` was clearing the active chat in-place instead of creating a new
  session. It now creates a proper new session visible in the sidebar, with its
  own isolated agent task (old agent events no longer bleed into the new chat).
- `/model` and `/mode` state was global across sessions; switching sessions
  would carry the staged model or mode override into the new chat. Each session
  now saves and restores its own model/mode state independently.
- Queued messages and `abort_pending` were not cleared when creating a new
  session, causing the old session's queue to be sent to the new session's agent.
- Active message-edit state (`e` key) was not cleared on new session creation.
- JSONL log path was not tracked per-session; history writes after a session
  switch could land in the wrong session's file.
- State leakage when creating new sessions (input, queue, edit state and
  agent state now isolated per chat session).
- Chat list click could trigger segment actions in the wrong session; mouse
  routing is now centralized with HitArea hit-test.
- Duplicate agent spawned on initial session; duplicate no longer created.
- Message loss on exit; session state is saved so messages are not dropped.
- `wait_for_message` could drop replies when a peer responded before the
  waiter registered.
- Neovim double-response bug; inspector and pager UX improved.
- TodoUpdate segment ordering in tool output.
- Chat pane selection and scrollbar ghost/stuck rendering.

### Changed
- Chat document formatting improved (sven-input).
- Mouse routing refactored to centralize hit-testing (HitArea).
- Inspector overlay: removed dead `kind` field.
- Documentation for parallel tool slots and multi-session TUI.

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
