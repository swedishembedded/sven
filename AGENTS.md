# Sven — AI Coding Agent for the Terminal

This is the Sven agent codebase: a keyboard-driven AI coding agent built in Rust. It runs as an interactive TUI, a headless CI runner, and a networked P2P node — all from the same binary.

## For AI Agents Working on This Codebase

- **Language**: Rust. Follow idiomatic Rust patterns, ownership rules, and error handling conventions.
- **Architecture**: Multi-crate workspace. Core logic in `sven-core`, tools in `sven-tools`, TUI in `sven-tui`, P2P in `sven-p2p`, node in `sven-node`. See `README.md` for the full layout.
- **Skills**: Use `.cursor/skills/programming/ratatui/SKILL.md` for TUI work, `.cursor/skills/programming/rust/SKILL.md` for Rust code, `.cursor/skills/programming/rust-semver/SKILL.md` when changing public APIs.
- **Tests**: Run `make test` before committing. E2E tests require `bats-core`: `make tests/e2e/basic`.
- **Linting**: `make check` runs clippy with `-D warnings`.

## Essential Commands

| Command | Purpose |
|---------|---------|
| `make build` | Debug build |
| `make release` | Optimised release build |
| `make test` | Run all unit and integration tests |
| `make check` | Clippy lint (no build) |
| `make fmt` | Format code |
| `make deb` | Build Debian package (output in `target/debian/`) |
| `make docs` | Build single-file user guide → `target/docs/sven-user-guide.md` |

## Key Directories

### Root
- `src/` — binary entry-point and CLI
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
| `sven-config` | Config schema and loader |
| `sven-model` | ModelProvider trait + 32 drivers |
| `sven-image` | Image handling (read_image) |
| `sven-input` | Markdown step parser and message queue |
| `sven-tools` | Full tool suite + approval policy |
| `sven-core` | Agent loop, session, context compaction |
| `sven-runtime` | Shared runtime utilities |
| `sven-bootstrap` | First-run setup helpers |
| `sven-ci` | Headless runner and output formatting |
| `sven-tui` | Ratatui TUI: layout, widgets, key bindings |
| `sven-p2p` | libp2p: Noise, mDNS, relay, task routing |
| `sven-node` | HTTP/WS node + P2P + agent wiring |
| `sven-node-client` | WebSocket client for connecting to a node |
| `sven-mcp` | MCP server — exposes sven tools to MCP clients |
| `sven-acp` | ACP (Agent Client Protocol) server for IDE integration |
| `sven-team` | Agent team coordination: task lists, config, lifecycle |

## Documentation

- [README.md](README.md) — overview, features, building
- [docs/00-introduction.md](docs/00-introduction.md) — what Sven is and how it works
- [docs/technical/](docs/technical/) — ACP, skill system, P2P, session protocol
