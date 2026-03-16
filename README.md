# Agent Sven

**A keyboard-driven AI agent for the terminal and desktop.** Built in Rust, sven
works as an interactive TUI, a Slint desktop GUI (`sven-ui`), a headless CI
runner, a networked node that teams up with other sven instances, and a
proactive personal automation platform — two binaries, one agent.

[![CI](https://github.com/swedishembedded/sven/actions/workflows/ci.yml/badge.svg)](https://github.com/swedishembedded/sven/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Changelog](https://img.shields.io/badge/changelog-CHANGELOG.md-informational)](CHANGELOG.md)

![sven TUI showing a live chat session with streamed markdown response and vim-style navigation](docs/sven-landing.png)

Give sven a task in plain English. It reads your code, runs commands, writes
files, searches the web, and delegates subtasks to peer agents — all
autonomously, all in your terminal. Beyond interactive sessions, sven runs 24/7
as a proactive agent: checking email and calendar, sending briefings via
Telegram, making voice calls, and running scheduled workflows.

## Key Features

- **Interactive TUI** — Full-screen Ratatui interface with scrollable markdown chat, vim-style navigation, and live-streamed responses. Swap to an embedded Neovim buffer with `--nvim`.
- **Desktop GUI** — `sven-ui` is a native Slint window with the full agent and tool suite, no terminal required.
- **Headless / CI** — Reads from stdin or a markdown workflow file, writes clean text to stdout. Pipeable: chain sven instances to build multi-agent pipelines.
- **Markdown workflow files** — `##`-headed steps, YAML frontmatter, per-step directives, and variable templating make `.md` files first-class agent programs (unique to sven).
- **Agent networking** — Multiple sven nodes discover each other via mDNS (or a relay), and the LLM gains `list_peers` and `delegate_task` tools to route work across machines.
- **GDB hardware debugging** — First AI agent with native GDB integration: connects to a target, loads firmware, sets breakpoints, and inspects registers, all autonomously.
- **Proactive automation** — Scheduler, email (IMAP/Gmail), calendar (CalDAV/Google), voice (TTS/STT/calls), semantic memory, and 6 messaging channels run 24/7 as a node.
- **Skills system** — Markdown instruction files the agent loads on demand for coding standards, project conventions, or multi-step procedures.
- **32 model providers** — OpenAI, Anthropic, Gemini, Ollama, and 28 more — no external gateway, pure Rust.
- **MCP + ACP** — Expose sven's tools to Cursor, Claude Desktop, and other MCP hosts; or drive sven from Zed, VS Code, or JetBrains via the Agent Client Protocol.
- **Terminal-native, zero runtime deps** — Structured text in, structured text out. No Node.js, no Python, no screenshots, no pixel-clicking.

## Quick Start

**Prerequisites:** Rust toolchain, API key for at least one supported provider (e.g. `OPENROUTER_API_KEY` for zero-config start with `openrouter/auto`).

```sh
# Build and run
make release && ./target/release/sven

# Pipe a one-shot task
echo "Summarise the project" | sven

# Run a multi-step workflow file
sven --file plan.md
```

See [Installation](docs/01-installation.md) and [Quick Start](docs/02-quickstart.md) for full setup details.

## Agent modes

| Mode | Behaviour |
|------|-----------|
| `research` | Read-only tools. Good for exploration and analysis. |
| `plan` | No file writes. Produces structured plans without side effects. |
| `agent` | Full read/write access. Default for interactive use. |

Set with `--mode` or cycle live in the TUI with `F4`.

## GDB-native hardware debugging

Sven is the **first AI agent with native GDB integration** for autonomous
embedded hardware debugging. Give it a plain-English task and it will start a
GDB server, connect to the target, load your firmware, set breakpoints, inspect
registers and variables, and report its findings — all without leaving your
terminal.

![sven GDB session showing autonomous breakpoint inspection on an embedded target](docs/sven-gdb-1.png)

| Tool | What it does |
|------|-------------|
| `gdb_start_server` | Start JLinkGDBServer / OpenOCD / pyocd (auto-discovers config from project files) |
| `gdb_connect` | Connect `gdb-multiarch --interpreter=mi3` and optionally load an ELF |
| `gdb_command` | Run any GDB/MI command and return structured output |
| `gdb_interrupt` | Send Ctrl+C to a running target |
| `gdb_wait_stopped` | Poll until the target halts (after a step, breakpoint, or interrupt) |
| `gdb_status` | Query the current run state and any pending stop events |
| `gdb_stop` | Kill the debug session and free the probe |

See [Example 11](docs/06-examples.md#example-11--embedded-gdb-debugging-session) and the [GDB section of the User Guide](docs/03-user-guide.md#gdb-debugging-tools).

## Agent-to-agent task routing

Multiple sven nodes find each other on a local network via mDNS — or across
networks via a relay — and each node automatically gains two tools the LLM can
use during any session:

| Tool | What it does |
|------|-------------|
| `list_peers` | List connected peer agents with their name, description, and capabilities |
| `delegate_task` | Send a task to a named peer; the remote agent runs it through its own model+tool loop and returns the full result |

**Declarative agent teams** are defined in `.sven/teams/*.yaml`. Manage them with:

```sh
sven team start --file .sven/teams/myteam.yaml   # spawn all team members
sven team status myteam                           # show live task board
sven peer chat backend-agent                      # interactive session with any peer
```

See [docs/08-node.md](docs/08-node.md) for setup, relay configuration, and security.

## Proactive agent capabilities

When running as a node (`sven node start`), sven gains a full automation stack:

| Integration | What it does | Docs |
|-------------|--------------|------|
| **Messaging** (Telegram, Discord, WhatsApp, Signal, Matrix, IRC) | Reach your agent or let it reach you via any channel | [docs/12-channels.md](docs/12-channels.md) |
| **Scheduler** (cron, intervals, one-shot) | Run prompts on a schedule; the agent can also schedule jobs at runtime | [docs/13-scheduler.md](docs/13-scheduler.md) |
| **Email** (IMAP/SMTP, Gmail API) | List, read, send, reply to, and search email | [docs/14-email.md](docs/14-email.md) |
| **Calendar** (CalDAV, Google Calendar) | Query schedule, create and update events | [docs/15-calendar.md](docs/15-calendar.md) |
| **Voice** (ElevenLabs TTS, Whisper STT, Twilio calls) | Synthesize speech, transcribe audio, make outbound phone calls | [docs/16-voice.md](docs/16-voice.md) |
| **Semantic memory** (SQLite + FTS5 + embeddings) | Remember anything; recall with natural-language queries | [docs/17-memory.md](docs/17-memory.md) |
| **Webhooks** | Trigger the agent from any external system via a generic HTTP hook | [docs/18-webhooks.md](docs/18-webhooks.md) |

See [docs/19-use-cases.md](docs/19-use-cases.md) for seven complete real-world automation patterns.

## Workflow files — unique to sven

sven treats markdown files as first-class workflow definitions:

```markdown
# Security Audit

## Understand the codebase
<!-- sven: timeout=60 -->
Read the project structure and summarise the architecture.

## Identify risks
{{context}}
Look for OWASP Top-10 issues and insecure defaults.

## Write report
Produce a structured security report with severity ratings.
```

```sh
sven --file audit.md --var context="Focus on authentication."
```

Each `##` heading is a step. YAML frontmatter sets mode and model. Per-step
`<!-- sven: ... -->` directives control timeouts. Variable templating with
`{{key}}` fills values at runtime. See [docs/04-ci-pipeline.md](docs/04-ci-pipeline.md).

## Tool suite

| Category | Tools |
|----------|-------|
| **Files** | `read_file`, `write_file`, `edit_file`, `delete_file`, `list_dir` |
| **Search** | `find_file`, `grep`, `search_codebase` |
| **Shell** | `run_terminal_command`, `shell` |
| **Web** | `web_fetch`, `web_search` |
| **Images** | `read_image` |
| **Sub-agents** | `task` — spawn a focused sub-agent for a self-contained subtask |
| **GDB / hardware** | `gdb_start_server`, `gdb_connect`, `gdb_command`, `gdb_interrupt`, `gdb_wait_stopped`, `gdb_status`, `gdb_stop` |
| **Agent networking** | `list_peers`, `delegate_task` *(node mode only)* |
| **Messaging** | `send_message` — send to any configured channel |
| **Scheduler** | `schedule` — create, list, enable, disable, delete jobs |
| **Email** | `email` — list, read, send, reply to, and search email |
| **Calendar** | `calendar` — query schedule, create/update/delete events |
| **Voice** | `voice` — TTS, STT, outbound calls |
| **Memory** | `semantic_memory` — remember, recall (BM25 + vector), forget, list, get |
| **Session** | `switch_mode`, `todo`, `update_memory`, `ask_question`†, `read_lints`, `load_skill` |

†`ask_question` is only available in interactive TUI sessions.

Each tool call goes through a configurable approval policy — auto-approved, denied, or presented for confirmation based on glob patterns.

## Model Providers

Sven supports **32 model providers** natively in Rust — no external gateway required.

| Category | Providers |
|----------|-----------|
| Major cloud | OpenAI, Anthropic, Google Gemini, Azure OpenAI, AWS Bedrock, Cohere |
| Gateways | OpenRouter, LiteLLM, Portkey, Vercel AI, Cloudflare |
| Fast inference | Groq, Cerebras |
| Open models | Together AI, Fireworks, DeepInfra, Nebius, SambaNova, Hugging Face, NVIDIA NIM |
| Specialized | Mistral, xAI (Grok), Perplexity |
| Regional | DeepSeek, Moonshot, Qwen/DashScope, GLM, MiniMax, Baidu Qianfan |
| Local / OSS | Ollama, vLLM, LM Studio |

See [docs/providers.md](docs/providers.md) for configuration details.

## IDE integration — ACP

Sven implements the [Agent Client Protocol (ACP)](https://agentclientprotocol.org),
letting ACP-aware editors drive it directly over stdio. No daemon, no relay, no
IDE API key required — sven manages its own model.

```json
// Zed: add to ~/.config/zed/settings.json
{
  "agents": {
    "sven": { "command": "sven", "args": ["acp", "serve"] }
  }
}
```

The same `sven acp serve` command works for VS Code (ACP extension) and JetBrains (AI Assistant plugin). See the [IDE integration guide](docs/03-user-guide.md#ide-integration-via-acp).

## MCP integration

Sven can expose its full tool suite as an [MCP](https://modelcontextprotocol.io) server,
letting Cursor, Claude Desktop, opencode, and others call sven's tools directly.

```json
{
  "mcpServers": {
    "sven": { "command": "sven", "args": ["mcp", "serve"] }
  }
}
```

## Documentation

| Section | Topic |
|---------|-------|
| [Introduction](docs/00-introduction.md) | What sven is and how it works |
| [Installation](docs/01-installation.md) | Getting sven onto your machine |
| [Quick Start](docs/02-quickstart.md) | Your first session in five minutes |
| [User Guide](docs/03-user-guide.md) | TUI navigation, modes, tools, conversations |
| [CI and Pipelines](docs/04-ci-pipeline.md) | Headless mode, workflow files, and CI integration |
| [Configuration](docs/05-configuration.md) | All config options explained |
| [Examples](docs/06-examples.md) | Real-world use cases |
| [Troubleshooting](docs/07-troubleshooting.md) | Common issues and fixes |
| [Node / P2P](docs/08-node.md) | Remote access, device pairing, agent networking |
| [Messaging Channels](docs/12-channels.md) | Telegram, Discord, WhatsApp, Signal, Matrix, IRC |
| [Scheduler](docs/13-scheduler.md) | Cron jobs, intervals, heartbeat |
| [Email](docs/14-email.md) | IMAP/SMTP and Gmail integration |
| [Calendar](docs/15-calendar.md) | CalDAV and Google Calendar integration |
| [Voice](docs/16-voice.md) | TTS, STT, and outbound voice calls |
| [Semantic Memory](docs/17-memory.md) | SQLite + FTS5 "second brain" knowledge store |
| [Webhooks](docs/18-webhooks.md) | Generic HTTP hooks for external integrations |
| [Automation Use Cases](docs/19-use-cases.md) | Seven complete real-world automation patterns |
| [Providers](docs/providers.md) | Model provider configuration |

Build the full user guide locally:

```sh
make docs        # → target/docs/sven-user-guide.md
make docs-pdf    # → target/docs/sven-user-guide.pdf (requires pandoc)
```

## Building

```sh
make build      # debug build
make release    # optimised release build
make deb        # Debian package
```

Requires a recent stable Rust toolchain. No other system dependencies.

```sh
make test           # unit and integration tests
make tests/e2e      # end-to-end tests (requires bats-core)
make check          # clippy lints
```

## Configuration

sven merges YAML config from `/etc/sven/config.yaml` → `~/.config/sven/config.yaml` → `.sven/config.yaml` → `sven.yaml` → `--config <path>`. Run `sven show-config` to inspect the resolved result.

See [docs/05-configuration.md](docs/05-configuration.md) for all options.

## Contributing

Contributions are welcome. Open an issue or pull request on [GitHub](https://github.com/swedishembedded/sven). For larger changes, open an issue first to discuss the approach.

## License

Licensed under the [Apache License 2.0](LICENSE). See [CHANGELOG.md](CHANGELOG.md) for version history.
