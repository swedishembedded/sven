# Agent Sven

A keyboard-driven AI coding agent for the terminal. Built in Rust, sven works in
two modes that share the same agent core:

- **Interactive TUI** — a full-screen terminal interface with a scrollable markdown
  chat log, vim-style navigation, and live-streamed responses.
- **Headless / CI** — reads instructions from stdin or a markdown file, writes
  clean text to stdout, and exits with a meaningful code. Designed to compose
  with other tools via pipes.

---

## Features

### Workflow files — unique to sven

sven treats markdown files as first-class workflow definitions.  No other
agent-CLI has an equivalent:

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

Run it with:

```sh
sven --file audit.md --var context="Focus on authentication."
```

Key capabilities the workflow format provides:

| Feature | sven | Codex | Claude Code | OpenClaw |
|---------|------|-------|-------------|----------|
| Markdown workflow files (`##` steps) | ✅ native | ❌ | ❌ | ❌ |
| YAML frontmatter (mode, timeouts, vars) | ✅ | ❌ | ❌ | ❌ |
| Per-step options (`<!-- sven: ... -->`) | ✅ | ❌ | ❌ | ❌ |
| Variable templating (`{{key}}`) | ✅ | ❌ | ❌ | ❌ |
| Pipeable conversation output | ✅ full conv. | last msg only | last msg only | ❌ |
| `sven validate --file` dry-run | ✅ | ❌ | ❌ | ❌ |
| Per-step artifacts directory | ✅ | ❌ | ❌ | ❌ |
| `conversation` / `json` / `compact` output | ✅ | `json` only | `stream-json` | `json` |
| Auto-detect CI environment | ✅ GA/GL/Jenkins | ❌ | ❌ | ❌ |
| Git context injection (branch/commit/dirty) | ✅ | ✅ | partial | ❌ |
| Auto-load `AGENTS.md` / `.sven/context.md` | ✅ | ✅ | `CLAUDE.md` | `AGENTS.md` |
| Zero runtime dependencies | ✅ native Rust | Node.js | Node.js | Node.js |
| TUI + headless in one binary | ✅ | separate | separate | separate |

### Pipeable conversations

sven headless output is valid sven conversation markdown — it can be piped
directly into another sven instance or loaded back with `--conversation`:

```sh
# Chain agents: plan → implement
sven --file plan.md | sven --mode agent "Implement the plan above."

# Save and resume
sven --file review.md > review.conv.md
sven --file review.conv.md --conversation  # continue from where you left off
```

### Project awareness

When a workflow runs, sven automatically:

1. Walks up the directory tree to find the `.git` root.
2. Injects the absolute project path into the system prompt — tools use it for
   all file operations.
3. Collects live git metadata (branch, commit, remote, dirty status) and injects
   it so the agent knows the repository context without being asked.
4. Reads `.sven/context.md`, `AGENTS.md`, or `CLAUDE.md` from the project root
   and injects the contents as project-level instructions.

---

## Model Providers

Sven supports **35+ model providers** natively in Rust — no external gateway
required.  Every provider is registered in the driver registry, so `sven
list-providers` always shows a complete, up-to-date list.

| Category | Providers |
|----------|-----------|
| Major cloud | OpenAI, Anthropic, Google Gemini, Azure OpenAI, AWS Bedrock, Cohere |
| Gateways | OpenRouter, LiteLLM, Portkey, Vercel AI, Cloudflare |
| Fast inference | Groq, Cerebras |
| Open models | Together AI, Fireworks, DeepInfra, Nebius, SambaNova, Hugging Face, NVIDIA NIM |
| Specialized | Mistral, xAI (Grok), Perplexity |
| Regional | DeepSeek, Moonshot, Qwen/DashScope, GLM, MiniMax, Baidu Qianfan |
| Local / OSS | Ollama, vLLM, LM Studio |

All drivers implement the same `ModelProvider` trait — tool calling, streaming,
and catalog metadata work consistently across providers.  See
[docs/providers.md](docs/providers.md) for configuration details and
[crates/sven-model/DRIVERS.md](crates/sven-model/DRIVERS.md) for adding new
drivers.

```sh
# List all registered providers
sven list-providers --verbose

# Switch provider on the fly
sven -M anthropic/claude-3-5-sonnet-20241022 "Refactor this code"
sven -M groq/llama-3.3-70b-versatile "Explain the algorithm"
sven -M ollama/llama3.2 "Quick local question"
```

## Documentation

The `docs/` directory contains the full user guide split into focused sections.
Build it locally with:

```sh
make docs        # single markdown file → target/docs/sven-user-guide.md
make docs-pdf    # PDF (requires pandoc) → target/docs/sven-user-guide.pdf
```

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

---

## Concepts

### Agent modes

Every invocation runs in one of three modes that constrain what the agent is
allowed to do:

| Mode | Behaviour |
|------|-----------|
| `research` | Read-only tools only. Good for exploration and analysis. |
| `plan` | No file writes. Produces structured plans without side effects. |
| `agent` | Full read/write access. The default for interactive use. |

The mode can be set on the command line (`--mode`) or cycled live in the TUI.

### Tools

The agent has access to three built-in tools:

- **shell** — runs arbitrary shell commands with a configurable timeout and an
  optional Docker sandbox.
- **fs** — reads, writes, appends, and lists files.
- **glob** — searches the filesystem by pattern.

Each tool call goes through an approval policy before it executes. Commands can
be auto-approved, denied, or presented for confirmation based on glob patterns
you configure.

### CI / pipeline mode

When stdin is not a TTY, or when `--headless` is passed, sven enters headless
mode. Input is parsed as a workflow markdown document:

- The first `#` H1 heading is the conversation title.
- Text between the H1 and the first `##` heading is appended to the system prompt.
- Each `##` H2 heading starts a step sent to the model as a user message.
- `<!-- sven: mode=X model=Y timeout=Z -->` directives inside a step set options for that step.

Output is full conversation markdown on stdout by default. Errors go to stderr.
A failed step exits non-zero, so the pipeline aborts naturally under `set -e`.

```sh
# Simple pipe
echo "Summarise the project" | sven

# Multi-step workflow file
sven --file plan.md

# Structured JSON output
sven --file audit.md --output-format json

# Save just the final answer to a file
sven --file plan.md --output-last-message answer.txt

# Chained agents: plan then implement
sven --file plan.md | sven --mode agent "Implement the plan above."

# Variable substitution
sven --file review.md --var branch=main --var pr=42

# Custom system prompt
sven --file tasks.md --system-prompt-file .sven/custom-prompt.md

# Append to default system prompt
sven --file tasks.md --append-system-prompt "Always write tests."

# Validate a workflow without running it
sven validate --file plan.md
sven --file plan.md --dry-run
```

**Exit codes:** `0` success · `1` agent error · `2` validation error · `124` timeout · `130` interrupt

### Conversation files

Conversation mode lets you use a single markdown file as a persistent,
human-editable conversation log. Run sven on it repeatedly, and each run loads
the existing history and appends the new agent turn back to the file.

The format uses H2 sections as turn boundaries:

| Section | Role |
|---------|------|
| `## User` | Your message or task |
| `## Sven` | Agent's text response |
| `## Tool` | A tool call (JSON code block) |
| `## Tool Result` | Output of the tool call |

An optional H1 line at the top is treated as the conversation title.

Everything inside a section is plain markdown — code blocks, lists, and
headings at H3 or below are all safe to use without escaping.

**Execution rule:** if the file ends with a `## User` section that has no
following `## Sven` section, sven treats it as the next instruction to execute
and appends the result.

```sh
# Create a conversation file
cat > work.md << 'EOF'
# Codebase Analysis

## User
Summarise the project structure.
EOF

# First run — sven executes the ## User section and appends ## Sven (+ any tool sections)
sven --file work.md --conversation

# You can then append a follow-up yourself
printf '\n## User\nNow list all public API entry points.\n' >> work.md

# Second run — history is loaded, only the new ## User is executed
sven --file work.md --conversation
```

After two runs the file might look like:

```markdown
# Codebase Analysis

## User
Summarise the project structure.

## Sven
The project is a Rust workspace with crates for config, model, tools ...

## User
Now list all public API entry points.

## Tool
```json
{
  "tool_call_id": "call_001",
  "name": "glob_file_search",
  "args": {"pattern": "**/*.rs"}
}
```

## Tool Result
```
src/main.rs
crates/sven-core/src/lib.rs
...
```

## Sven
The public entry points are `src/main.rs` (binary) and the `pub` items in ...
```

### Context management

The agent tracks token usage and compacts the conversation history
automatically before the context window fills up. The compaction threshold is
configurable.

---

## Building

```sh
# Debug build
make build

# Optimised release build
make release

# Debian package (uses cargo-deb if installed, otherwise scripts/build-deb.sh)
make deb
```

Requires a recent stable Rust toolchain. No other system dependencies are
needed for a basic build.

---

## Configuration

sven looks for YAML config files and merges them from lowest to highest priority:

1. `/etc/sven/config.yaml` (system-wide)
2. `~/.config/sven/config.yaml` (user-level)
3. `.sven/config.yaml` (workspace-local)
4. `sven.yaml` (project root)
5. Path given with `--config` (highest priority)

Run `sven show-config` to see the full resolved configuration with all defaults
filled in. The schema is defined in `crates/sven-config/src/schema.rs`.

Key sections:

- `model` — provider, model name, API key env var, base URL override for
  local proxies (e.g. LiteLLM), token limits.
- `agent` — default mode, maximum autonomous tool rounds, compaction
  threshold, optional system prompt override.
- `tools` — tool timeout, Docker sandbox toggle, auto-approve and deny glob
  patterns.
- `tui` — theme, markdown wrap width, ASCII-border fallback for terminals
  with limited font support.

### Listing available models

```sh
sven list-models                    # static built-in catalog
sven list-models --provider openai  # filter by provider
sven list-models --refresh          # query provider API for live list
sven list-models --json             # JSON output
```

### Model providers

Set `model.provider` to one of:

| Provider | Notes |
|----------|-------|
| `openai` | Default. Set `OPENAI_API_KEY`. |
| `anthropic` | Set `ANTHROPIC_API_KEY`. |
| `mock` | Returns scripted responses from a YAML file — useful for tests and offline work. |

The `base_url` field lets you point any provider at a compatible proxy without
changing anything else.

---

## The mock provider

The mock provider loads responses from a YAML file and matches them against
incoming messages by content. Match types include exact equality, substring,
prefix, regex, and a catch-all default. A rule can return plain text or trigger
a sequence of tool calls followed by a final reply.

```yaml
rules:
  - match: "ping"
    match_type: equals
    reply: "pong"

  - match: "write.*file"
    match_type: regex
    tool_calls:
      - round: 1
        name: fs
        args: '{"operation":"write","path":"/tmp/out.txt","content":"hello"}'
      - round: 2
        reply: "Done."

  - match: ".*"
    match_type: regex
    reply: "I don't know how to answer that."
```

Point sven at the file with `SVEN_MOCK_RESPONSES=/path/to/file.yaml` or the
`model.mock_responses_file` config key, then pass `--model mock`.

The end-to-end bats test suite in `tests/bats/` uses this mechanism so the
full CI pipeline can be validated without a live model API.

---

## Testing

```sh
make test       # unit and integration tests (cargo test)
make bats       # end-to-end tests (requires bats-core)
make check      # clippy lints
```

The bats suite covers CLI flags, headless mode behaviour, mock provider
matching, and multi-stage pipeline composition.

---

## Workspace layout

```
sven/
├── src/                    # binary entry-point
├── crates/
│   ├── sven-config/        # TOML schema and config loader
│   ├── sven-model/         # model provider trait + OpenAI, Anthropic, mock
│   ├── sven-core/          # agent loop, session, context compaction
│   ├── sven-tools/         # shell / fs / glob tools and approval policy
│   ├── sven-input/         # markdown step parser and message queue
│   ├── sven-ci/            # headless runner and output formatting
│   └── sven-tui/           # Ratatui TUI: layout, widgets, key bindings
└── tests/
    ├── bats/               # end-to-end bats tests
    └── fixtures/           # shared test data (mock responses, sample plans)
```

Each crate has a focused responsibility and its own unit tests. The dependency
graph is acyclic: `sven-config` and `sven-model` are leaves; `sven-core`
depends on both; `sven-ci` and `sven-tui` depend on `sven-core`; the root
binary wires everything together.
