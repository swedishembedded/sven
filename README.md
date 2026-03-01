# Agent Sven

A keyboard-driven AI coding agent for the terminal. Built in Rust, sven works
as an interactive TUI, a headless CI runner, and a networked node that teams
up with other sven instances — all from the same binary.

---

## What sven does

Give sven a task in plain English. It reads your code, runs commands, writes
files, searches the web, debugs hardware, and delegates subtasks to peer agents
— all autonomously, all in your terminal.

### Interactive TUI

A full-screen Ratatui interface with a scrollable markdown chat log, vim-style
navigation, and live-streamed responses. Swap between a plain ratatui view and
an embedded Neovim buffer with `--nvim`.

### Headless / CI

Reads instructions from stdin or a markdown file, writes clean text to stdout,
and exits with a meaningful code. Designed to compose with other tools via
pipes.

```sh
# Pipe a task
echo "Summarise the project" | sven

# Multi-step workflow file
sven --file plan.md

# Chain agents: plan → implement
sven --file plan.md | sven --mode agent "Implement the plan above."
```

### Node (agent + P2P + HTTP)

```sh
sven node start
```

Runs the full agent plus a P2P networking stack and an HTTPS/WebSocket
endpoint. The node IS the agent — not a proxy. Multiple nodes discover each
other automatically on a local network (or via a relay across the internet) and
can delegate work to each other.

```sh
# Send a task to a running node from the command line
export SVEN_NODE_TOKEN=<token from first startup>
sven node exec "delegate the database migration to backend-agent and the UI changes to frontend-agent"
```

---

## GDB-native hardware debugging

Sven is the **first AI agent with native GDB integration** for autonomous
embedded hardware debugging. Give it a plain-English task and it will start a
GDB server, connect to the target, load your firmware, set breakpoints, inspect
registers and variables, and report its findings — all without leaving your
terminal.

```
You: Use gdb tools to connect to the target, reset it, and set a breakpoint on
     nrf uart tx function. Then inspect the parameters that are passed to it the
     first time and show me what they are. Use zephyr.elf as the executable.
```

The agent then autonomously calls the GDB tools in sequence:

```
gdb_start_server → gdb_connect → gdb_command (×N) → gdb_stop
```

![sven GDB session start](docs/sven-gdb-1.png)

![sven GDB session result](docs/sven-gdb-2.png)

![sven GDB session in Neovim](docs/sven-gdb-nvim.png)

| Tool | What it does |
|------|-------------|
| `gdb_start_server` | Start JLinkGDBServer / OpenOCD (auto-discovers config from project files) |
| `gdb_connect` | Connect `gdb-multiarch --interpreter=mi3` and optionally load an ELF |
| `gdb_command` | Run any GDB/MI command and return structured output |
| `gdb_interrupt` | Send Ctrl+C to a running target |
| `gdb_wait_stopped` | Poll until the target halts (after a step, breakpoint, or interrupt) |
| `gdb_status` | Query the current run state and any pending stop events |
| `gdb_stop` | Kill the debug session and free the probe |

See [Example 11](docs/06-examples.md#example-11--embedded-gdb-debugging-session)
and the [User Guide GDB section](docs/03-user-guide.md#gdb-debugging-tools).

---

## Agent-to-agent task routing

Multiple sven nodes find each other on a local network via mDNS — or across
networks via a relay — and each node automatically gains two tools the LLM can
use during any session:

| Tool | What it does |
|------|-------------|
| `list_peers` | List connected peer agents with their name, description, and capabilities |
| `delegate_task` | Send a task to a named peer; the remote agent runs it through its own model+tool loop and returns the full result |

```
You are the orchestrator for a small team. Use list_peers to find who is
online, then:
1. Delegate the database migration to the backend-agent.
2. Delegate the UI changes to the frontend-agent.
3. Summarise what each agent did.
```

sven handles the rest — calling `list_peers`, picking the right peers, calling
`delegate_task` for each, and assembling the results.

See [docs/08-gateway.md](docs/08-gateway.md) for setup, security, and configuration.

---

## Features

### Workflow files — unique to sven

sven treats markdown files as first-class workflow definitions:

```markdown
# Security Audit

Systematic security review of the codebase.

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

| Feature | sven | Codex | Claude Code | OpenClaw |
|---------|------|-------|-------------|----------|
| Markdown workflow files (`##` steps) | ✓ native | — | — | — |
| YAML frontmatter (mode, timeouts, vars) | ✓ | — | — | — |
| Per-step options (`<!-- sven: ... -->`) | ✓ | — | — | — |
| Variable templating (`{{key}}`) | ✓ | — | — | — |
| Pipeable conversation output | ✓ full conv. | last msg only | last msg only | — |
| `sven validate --file` dry-run | ✓ | — | — | — |
| Per-step artifacts directory | ✓ | — | — | — |
| `conversation` / `json` / `compact` / `jsonl` output | ✓ | `json` only | `stream-json` | `json` |
| Auto-detect CI environment | ✓ GA/GL/Jenkins | — | — | — |
| Git context injection (branch/commit/dirty) | ✓ | ✓ | partial | — |
| Auto-load `AGENTS.md` / `.sven/context.md` | ✓ | ✓ | `CLAUDE.md` | `AGENTS.md` |
| **Native GDB hardware debugging** | ✓ first-class | — | — | — |
| **Agent-to-agent P2P task routing** | ✓ built-in | — | — | — |
| **Skills system** | ✓ built-in | — | — | — |
| Zero runtime dependencies | ✓ native Rust | Node.js | Node.js | Node.js |
| TUI + headless + node in one binary | ✓ | separate | separate | separate |

### Tool suite

The agent has a rich built-in toolset:

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
| **Session** | `switch_mode`, `todo_write`, `update_memory`, `ask_question`†, `read_lints`, `load_skill` |

†`ask_question` is only available in interactive TUI sessions.

Each tool call goes through an approval policy — auto-approved, denied, or
presented for confirmation based on glob patterns you configure.

### Skills system

Skills are markdown instruction files that the agent loads on demand to gain
specialized expertise for a task. Skills can encode coding standards, project
conventions, domain knowledge, or multi-step procedures. Load a skill with
`load_skill` or configure auto-loading in `.cursor/skills/`.

### Project awareness

When a session starts, sven automatically:

1. Walks up the directory tree to find the `.git` root.
2. Injects the absolute project path so tools use it for all file operations.
3. Collects live git metadata (branch, commit, remote, dirty status).
4. Reads `.sven/context.md`, `AGENTS.md`, or `CLAUDE.md` and injects the
   contents as project-level instructions.

### Persistent memory

`update_memory` writes durable facts to a memory file that is injected at the
start of every session. The agent can learn and remember things about your
project across conversations.

### Pipeable conversations

sven headless output is valid sven conversation markdown — it can be piped
directly into another sven instance:

```sh
# Chain agents: plan → implement
sven --file plan.md | sven --mode agent "Implement the plan above."

# Save the full trace for fine-tuning (includes system prompts, OpenAI format)
sven --file workflow.md --output-jsonl trace.jsonl
```

---

## Model Providers

Sven supports **35+ model providers** natively in Rust — no external gateway
required.

| Category | Providers |
|----------|-----------|
| Major cloud | OpenAI, Anthropic, Google Gemini, Azure OpenAI, AWS Bedrock, Cohere |
| Gateways | OpenRouter, LiteLLM, Portkey, Vercel AI, Cloudflare |
| Fast inference | Groq, Cerebras |
| Open models | Together AI, Fireworks, DeepInfra, Nebius, SambaNova, Hugging Face, NVIDIA NIM |
| Specialized | Mistral, xAI (Grok), Perplexity |
| Regional | DeepSeek, Moonshot, Qwen/DashScope, GLM, MiniMax, Baidu Qianfan |
| Local / OSS | Ollama, vLLM, LM Studio |

```sh
sven list-providers --verbose
sven -M anthropic/claude-opus-4-5 "Refactor this code"
sven -M groq/llama-3.3-70b-versatile "Explain the algorithm"
sven -M ollama/llama3.2 "Quick local question"
```

See [docs/providers.md](docs/providers.md) for configuration details.

---

## Documentation

| Section | Topic |
|---------|-------|
| [Introduction](docs/00-introduction.md) | What sven is and how it works |
| [Installation](docs/01-installation.md) | Getting sven onto your machine |
| [Quick Start](docs/02-quickstart.md) | Your first session in five minutes |
| [User Guide](docs/03-user-guide.md) | TUI navigation, modes, tools, conversations |
| [CI and Pipelines](docs/04-ci-pipeline.md) | Headless mode, scripts, and CI integration |
| [Configuration](docs/05-configuration.md) | All config options explained |
| [Examples](docs/06-examples.md) | Real-world use cases |
| [Troubleshooting](docs/07-troubleshooting.md) | Common issues and fixes |
| [Node / P2P](docs/08-gateway.md) | Remote access, device pairing, agent networking |

Build the full user guide locally:

```sh
make docs        # → target/docs/sven-user-guide.md
make docs-pdf    # → target/docs/sven-user-guide.pdf (requires pandoc)
```

---

## Agent modes

| Mode | Behaviour |
|------|-----------|
| `research` | Read-only tools. Good for exploration and analysis. |
| `plan` | No file writes. Produces structured plans without side effects. |
| `agent` | Full read/write access. Default for interactive use. |

Set with `--mode` or cycle live in the TUI with `F4`.

---

## CI / pipeline mode

When stdin is not a TTY, or when `--headless` is passed, sven enters headless
mode:

- The first `#` H1 heading is the conversation title.
- Text between H1 and the first `##` is appended to the system prompt.
- Each `##` H2 heading is a step sent to the model.
- `<!-- sven: mode=X model=Y timeout=Z -->` directives set per-step options.

```sh
sven --file plan.md                              # run a workflow
sven --file audit.md --output-format json        # structured JSON output
sven --file plan.md --output-last-message out.txt
sven --file workflow.md --output-jsonl trace.jsonl  # full trace for fine-tuning
sven --file review.md --var branch=main --var pr=42
sven validate --file plan.md                     # dry-run: parse only
```

**Exit codes:** `0` success · `1` agent error · `2` validation error · `124` timeout · `130` interrupt

---

## Node — remote access and agent networking

```sh
# Start the node (generates TLS cert and bearer token on first run)
sven node start [--config .node.yaml]

# Send a task to the running node from another terminal
export SVEN_NODE_TOKEN=<token>
sven node exec "What files are in the current directory?"

# Pair a mobile/native operator device
sven node pair "sven-pair://12D3KooW..."

# Rotate the bearer token
sven node regenerate-token

# List authorized operator devices (NOT agent peers — see below)
sven node list-operators
```

Security defaults — all on, none optional:

| What | Default |
|------|---------|
| HTTP TLS | On — ECDSA P-256, 90-day auto-generated cert |
| TLS version | TLS 1.3 only |
| P2P encryption | Noise protocol (Ed25519), always on |
| P2P authorisation | Deny-all — every operator device must be explicitly paired |
| HTTP binding | `127.0.0.1` — loopback only |
| Bearer token storage | SHA-256 hash only — plaintext never written to disk |

---

## Building

```sh
make build      # debug build
make release    # optimised release build
make deb        # Debian package
```

Requires a recent stable Rust toolchain. No other system dependencies.

---

## Configuration

sven merges YAML config files from lowest to highest priority:

1. `/etc/sven/config.yaml`
2. `~/.config/sven/config.yaml`
3. `.sven/config.yaml`
4. `sven.yaml`
5. `--config <path>`

Run `sven show-config` to see the full resolved config. Key sections:

- `model` — provider, model name, API key env var, base URL override, token limits.
- `agent` — default mode, max autonomous rounds, compaction threshold, system prompt override.
- `tools` — timeout, Docker sandbox, auto-approve and deny glob patterns.
- `tui` — theme, markdown wrap width, ASCII-border fallback.

---

## Workspace layout

```
sven/
├── src/                    # binary entry-point and CLI
└── crates/
    ├── sven-config/        # config schema and loader
    ├── sven-model/         # ModelProvider trait + 35+ drivers
    ├── sven-core/          # agent loop, session, context compaction
    ├── sven-tools/         # full tool suite + approval policy
    ├── sven-input/         # markdown step parser and message queue
    ├── sven-ci/            # headless runner and output formatting
    ├── sven-tui/           # Ratatui TUI: layout, widgets, key bindings
    ├── sven-p2p/           # libp2p node: Noise, mDNS, relay, task routing
    ├── sven-node/          # node: HTTP/WS gateway + P2P + Slack + agent wiring
    ├── sven-runtime/       # shared runtime utilities
    └── sven-bootstrap/     # first-run setup helpers
```

---

## Testing

```sh
make test       # unit and integration tests (cargo test)
make bats       # end-to-end tests (requires bats-core)
make check      # clippy lints
```
