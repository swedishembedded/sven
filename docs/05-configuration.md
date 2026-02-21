# Configuration

sven is configured through a TOML file. Most options have sensible defaults, so
the config file is optional — you only need it when you want to change something.

---

## Config file location

sven looks for its config file in the following order and uses the first one
it finds:

1. The path given with `--config /path/to/config.toml`
2. The path in the `SVEN_CONFIG` environment variable
3. `$XDG_CONFIG_HOME/sven/config.toml` (usually `~/.config/sven/config.toml`)
4. `~/.config/sven/config.toml`

---

## View your current configuration

To see the full resolved configuration with all defaults filled in:

```sh
sven show-config
```

This prints the effective TOML to standard output. It is a convenient way to
check the result after editing the file, or to generate a starting point:

```sh
sven show-config > ~/.config/sven/config.toml
```

---

## Full annotated example

The following example shows every available option with its default value and
an explanation. You do not need to include options you are not changing.

```toml
# ── Model ──────────────────────────────────────────────────────────────────

[model]

# Provider to use. Supported values: "openai", "anthropic", "mock"
provider = "openai"

# Model name forwarded to the provider.
name = "gpt-4o"

# Environment variable that holds the API key.
# The variable is read at runtime so it never needs to be in this file.
api_key_env = "OPENAI_API_KEY"

# Alternatively, embed the key directly (not recommended for shared files).
# api_key = "sk-..."

# Override the provider's API base URL.
# Useful for local proxies (e.g. LiteLLM) or self-hosted models.
# base_url = "http://localhost:4000/v1"

# Maximum tokens to request in a single response.
max_tokens = 4096

# Sampling temperature (0.0 = deterministic, 2.0 = very random).
temperature = 0.2

# Path to a YAML file of scripted mock responses (provider = "mock" only).
# Can also be set with the SVEN_MOCK_RESPONSES environment variable.
# mock_responses_file = "/path/to/responses.yaml"


# ── Agent ──────────────────────────────────────────────────────────────────

[agent]

# Default mode when --mode is not given on the command line.
# Values: "research", "plan", "agent"
default_mode = "agent"

# Maximum number of tool-call rounds before sven stops and reports.
# Increase this for very long autonomous tasks.
max_tool_rounds = 50

# Fraction of the context window at which proactive compaction triggers.
# 0.85 means sven starts compacting when 85% of the context is used.
compaction_threshold = 0.85

# Override the system prompt sent to the model.
# Leave unset to use the built-in prompt.
# system_prompt = "You are a careful coding assistant..."


# ── Tools ──────────────────────────────────────────────────────────────────

[tools]

# Shell commands matching these glob patterns are approved automatically,
# without asking for confirmation.
auto_approve_patterns = [
    "cat *",
    "ls *",
    "find *",
    "rg *",
    "grep *",
]

# Shell commands matching these patterns are always blocked.
deny_patterns = [
    "rm -rf /*",
    "dd if=*",
]

# Timeout for a single tool call, in seconds.
timeout_secs = 30

# Run shell commands inside a Docker container for additional isolation.
use_docker = false

# Docker image to use when use_docker = true.
# docker_image = "ubuntu:22.04"


# ── Web tools ──────────────────────────────────────────────────────────────

[tools.web]

# Maximum number of characters fetched from a URL.
fetch_max_chars = 50000

[tools.web.search]

# API key for the Brave Search backend.
# Can also be set with BRAVE_API_KEY environment variable.
# api_key = "BSA..."


# ── Memory ─────────────────────────────────────────────────────────────────

[tools.memory]

# Path to the persistent memory JSON file.
# Defaults to ~/.config/sven/memory.json
# memory_file = "/path/to/memory.json"


# ── Lints ──────────────────────────────────────────────────────────────────

[tools.lints]

# Override the lint command for Rust projects.
# Default: cargo clippy --message-format json
# rust_command = "cargo clippy --message-format json"

# Override the lint command for TypeScript/JavaScript projects.
# typescript_command = "npx eslint --format json ."

# Override the lint command for Python projects.
# python_command = "ruff check --output-format json ."


# ── TUI appearance ─────────────────────────────────────────────────────────

[tui]

# Colour theme. Values: "dark", "light", "solarized"
theme = "dark"

# Show line numbers inside code blocks.
code_line_numbers = false

# Column at which markdown text wraps (0 = use terminal width).
wrap_width = 0

# Use plain ASCII characters instead of Unicode box-drawing characters.
# Enable this if your terminal font renders Unicode as gibberish.
# Can also be forced with SVEN_ASCII_BORDERS=1 environment variable.
ascii_borders = false
```

---

## Section reference

### `[model]`

Controls which language model sven talks to and how.

| Key | Default | Description |
|-----|---------|-------------|
| `provider` | `"openai"` | Provider name: `"openai"`, `"anthropic"`, or `"mock"` |
| `name` | `"gpt-4o"` | Model identifier sent to the provider |
| `api_key_env` | `"OPENAI_API_KEY"` | Environment variable containing the API key |
| `api_key` | — | Inline API key (use `api_key_env` instead when possible) |
| `base_url` | — | Override the API endpoint (for proxies) |
| `max_tokens` | `4096` | Maximum tokens per response |
| `temperature` | `0.2` | Sampling temperature (0.0–2.0) |
| `mock_responses_file` | — | Path to YAML mock responses (mock provider only) |

#### Supported providers

| Provider | `provider` value | API key variable |
|----------|-----------------|-----------------|
| OpenAI | `"openai"` | `OPENAI_API_KEY` |
| Anthropic | `"anthropic"` | `ANTHROPIC_API_KEY` |
| Mock (offline) | `"mock"` | — |

To use a proxy or local model that has an OpenAI-compatible API (such as
LiteLLM or Ollama), set `provider = "openai"` and override `base_url`:

```toml
[model]
provider = "openai"
name = "llama3"
base_url = "http://localhost:11434/v1"
```

---

### `[agent]`

Controls the agent's autonomy and defaults.

| Key | Default | Description |
|-----|---------|-------------|
| `default_mode` | `"agent"` | Mode used when `--mode` is not passed |
| `max_tool_rounds` | `50` | Maximum autonomous tool-call rounds before stopping |
| `compaction_threshold` | `0.85` | Context fraction that triggers history compaction |
| `system_prompt` | — | System prompt override (leave unset to use built-in) |

Increasing `max_tool_rounds` lets sven work on longer tasks without stopping.
Decreasing it gives you more control by forcing sven to pause and ask.

---

### `[tools]`

Controls what the agent is allowed to do and how.

| Key | Default | Description |
|-----|---------|-------------|
| `auto_approve_patterns` | `["cat *", "ls *", …]` | Commands matching these run without confirmation |
| `deny_patterns` | `["rm -rf /*", …]` | Commands matching these are always blocked |
| `timeout_secs` | `30` | Per-tool-call timeout in seconds |
| `use_docker` | `false` | Sandbox shell execution in Docker |
| `docker_image` | — | Docker image for sandboxed execution |

**Adding auto-approve patterns:**

```toml
[tools]
auto_approve_patterns = [
    "cat *",
    "ls *",
    "rg *",
    "grep *",
    "cargo test*",   # auto-approve test runs
    "make check",    # auto-approve linting
]
```

**Blocking specific commands:**

```toml
[tools]
deny_patterns = [
    "rm -rf /*",
    "dd if=*",
    "curl * | sh",   # block shell-pipe downloads
]
```

---

### `[tools.web]`

| Key | Default | Description |
|-----|---------|-------------|
| `fetch_max_chars` | `50000` | Maximum characters fetched from a URL |
| `search.api_key` | — | Brave Search API key (also `BRAVE_API_KEY` env var) |

---

### `[tools.memory]`

| Key | Default | Description |
|-----|---------|-------------|
| `memory_file` | `~/.config/sven/memory.json` | Where persistent memory is stored |

---

### `[tools.lints]`

These let you override the command sven runs when you ask it to check for lint
errors. The commands should produce JSON output that sven can parse.

| Key | Default | Description |
|-----|---------|-------------|
| `rust_command` | `cargo clippy --message-format json` | Rust lint command |
| `typescript_command` | `npx eslint --format json .` | TypeScript/JS lint command |
| `python_command` | `ruff check --output-format json .` | Python lint command |

---

### `[tui]`

| Key | Default | Description |
|-----|---------|-------------|
| `theme` | `"dark"` | Colour theme: `"dark"`, `"light"`, or `"solarized"` |
| `code_line_numbers` | `false` | Show line numbers in code blocks |
| `wrap_width` | `0` | Markdown wrap column (0 = auto) |
| `ascii_borders` | `false` | Use ASCII instead of Unicode box-drawing characters |

The `ascii_borders` setting is also controlled by the `SVEN_ASCII_BORDERS=1`
environment variable, which is useful when you cannot edit the config file
(e.g. in a CI container with a limited font).

---

## Minimal config examples

**Use Anthropic Claude:**

```toml
[model]
provider = "anthropic"
name = "claude-opus-4-5"
api_key_env = "ANTHROPIC_API_KEY"
```

**Use a local Ollama model:**

```toml
[model]
provider = "openai"
name = "codellama"
base_url = "http://localhost:11434/v1"
```

**Auto-approve all read and test commands:**

```toml
[tools]
auto_approve_patterns = [
    "cat *", "ls *", "find *", "rg *", "grep *",
    "cargo test*", "make test", "pytest *",
]
```

**Use ASCII borders (terminal font compatibility):**

```toml
[tui]
ascii_borders = true
```
