# Sven — AI Coding Agent

Sven is a keyboard-driven AI coding agent built in Rust. It runs as an
interactive TUI (`sven`), a Slint desktop GUI (`sven-ui`), a headless CI
runner, and a networked P2P node — all from the same workspace.

## For AI Agents Working on This Codebase

- **Language**: Rust. Follow idiomatic Rust patterns, ownership rules, and error handling conventions.
- **Architecture**: Multi-crate workspace. See the crate table below for the full layout.
- **Skills**: Load the relevant skill before writing code:
  - TUI work → `.cursor/skills/programming/ratatui/SKILL.md`
  - GUI work → `.cursor/skills/programming/ratatui/SKILL.md` (patterns section) — Slint uses a declarative `.slint` DSL compiled to Rust bindings
  - Rust code → `.cursor/skills/programming/rust/SKILL.md`
  - Public API changes → `.cursor/skills/programming/rust-semver/SKILL.md`
- **Tests**: Run `make test` before committing. E2E tests require `bats-core`: `make tests/e2e/basic`.
- **Linting**: `make check` runs clippy with `-D warnings`. Zero warnings policy — all new code must be warning-free.

## Essential Commands

| Command | Purpose |
|---------|---------|
| `make build` | Debug build (both `sven` and `sven-ui` binaries) |
| `make release` | Optimised release build |
| `make test` | Run all unit and integration tests |
| `make check` | Clippy lint (no build) |
| `make fmt` | Format code |
| `make deb` | Build Debian package (output in `target/debian/`) |
| `make docs` | Build single-file user guide → `target/docs/sven-user-guide.md` |

## Binaries

| Binary | Entry point | Description |
|--------|-------------|-------------|
| `sven` | `src/main.rs` | Interactive TUI, CI runner, P2P node |
| `sven-ui` | `src/ui_main.rs` | Slint desktop GUI |

## Key Directories

### Root
- `src/` — binary entry points and shared CLI parsing (`cli.rs`, `main.rs`, `ui_main.rs`)
- `docs/` — user-facing documentation
- `docs/technical/` — ACP, skill system, P2P, session protocol, knowledge base
- `tests/` — unit/integration tests; `tests/e2e/` — bats E2E tests
- `benchmarks/` — Terminal-Bench 2.0 via Harbor
- `site/` — marketing/landing site (Docker)
- `scripts/` — build-deb, release-build, etc.
- `.agents/skills/` — agent skills for this project
- `.github/` — CI workflows and actions

### Crates

| Crate | Purpose |
|-------|---------|
| `sven-config` | Config schema and loader (`sven.yaml`) |
| `sven-model` | `ModelProvider` trait, 32+ driver implementations, catalog |
| `sven-image` | Image reading helpers |
| `sven-input` | Chat document model, history, title generation heuristics |
| `sven-tools` | Full tool suite, approval policy, `Tool`/`ToolDisplay` traits |
| `sven-core` | Agent loop, session state, context compaction, `AgentEvent` |
| `sven-runtime` | Shared runtime utilities (workspace root, skill/agent discovery) |
| `sven-bootstrap` | First-run setup helpers, `AgentBuilder` |
| `sven-ci` | Headless CI runner and output formatting |
| `sven-mcp-client` | MCP client — connects to external MCP servers over stdio/HTTP |
| `sven-mcp` | MCP server — exposes sven tools to MCP clients |
| `sven-acp` | ACP (Agent Client Protocol) server for IDE integration |
| `sven-p2p` | libp2p: Noise, mDNS, relay, task routing |
| `sven-node` | HTTP/WS node + P2P + agent wiring |
| `sven-node-client` | WebSocket client for connecting to a running node |
| `sven-team` | Agent team coordination: task lists, config, lifecycle |
| `sven-frontend` | **Shared frontend layer** — agent wiring, slash commands, markdown, queue, tool views (used by both TUI and GUI) |
| `sven-tui` | Ratatui TUI: layout, widgets, key bindings (`sven` binary) |
| `sven-gui` | Slint desktop GUI: `.slint` UI files + Rust bridge (`sven-ui` binary) |

## Frontend Architecture

Both the TUI and the GUI share a common layer in `sven-frontend`. Never duplicate
logic between `sven-tui` and `sven-gui` — extract it to `sven-frontend` instead.

```
sven (CLI/TUI)            sven-ui (Desktop GUI)
      │                           │
 sven-tui (ratatui)       sven-gui (slint)
      │                           │
      └──────── sven-frontend ────┘
                     │
          ┌──────────┼──────────┐
    sven-bootstrap  sven-core  sven-tools
```

### `sven-frontend` modules

| Module | Contents |
|--------|----------|
| `agent` | `AgentRequest` enum, `agent_task` background task |
| `node_agent` | `node_agent_task` — WebSocket bridge to a sven node |
| `segment` | `ChatSegment` — display-layer chat data model |
| `types` | `ModelDirective`, `QueuedMessage`, `NodeBackend`, `FrontendOptions`, `SessionMeta` |
| `commands` | Slash command system: parser, registry, completion, all built-in commands |
| `commands::builtin` | `/model`, `/mode`, `/new`, `/abort`, `/clear`, `/inspect`, `/provider`, `/quit`, `/refresh`, `/team` |
| `commands::skill` | `SkillCommand` and `AgentCommand` — dynamic commands from markdown skill files |
| `commands::mcp` | `McpPromptCommand` — dynamic commands from MCP prompt registrations |
| `queue` | `QueueState` — buffer user messages while the agent is busy |
| `tool_view` | `ToolViewData`, `extract_tool_view()` — UI-agnostic tool display extraction |
| `markdown` | `MarkdownBlock` enum, `parse_markdown_blocks()` via pulldown-cmark |

### `sven-gui` layout

| Path | Contents |
|------|----------|
| `src/bridge.rs` | `SvenApp` + `SvenAppOptions`: wires agent events → Slint model |
| `src/models.rs` | Conversion helpers: `ChatSegment` → Slint `ChatMessage` |
| `ui/main-window.slint` | Root window, layout, properties, callbacks |
| `ui/chat-pane.slint` | Message rendering: paragraphs, headings, code blocks, tool calls, thinking blocks |
| `ui/input-pane.slint` | Chat input, queue badge, slash-command completion popup |
| `ui/status-bar.slint` | Model name, mode, context fill bar, token counts, cost |
| `ui/sidebar.slint` | Session list, new-chat button |
| `ui/picker-popup.slint` | Generic selection popup (model/mode switching) |
| `ui/completion-popup.slint` | Slash command auto-completion overlay |
| `ui/theme.slint` | Design tokens: colours, spacing, typography, code styling |
| `ui/toast.slint` | Toast notification overlay |
| `ui/question-modal.slint` | Agent question/approval modal |

### `sven-tui` re-exports

`sven-tui/src/commands/mod.rs` re-exports the entire slash command system from
`sven_frontend::commands`. Do **not** add new command implementations inside
`sven-tui`; add them to `sven-frontend::commands::builtin` instead.

## Documentation

- [README.md](README.md) — overview, features, building
- [docs/00-introduction.md](docs/00-introduction.md) — what Sven is and how it works
- [docs/technical/](docs/technical/) — ACP, skill system, P2P, session protocol
