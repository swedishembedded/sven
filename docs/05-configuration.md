# Configuration

sven is configured through a YAML file. Most options have sensible defaults, so
the config file is optional — you only need it when you want to change something.

---

## Config file location

sven looks for its config file in the following locations, merging them from
lowest to highest priority (later files override earlier ones):

1. `/etc/sven/config.yaml` (system-wide)
2. `~/.config/sven/config.yaml` (user-level)
3. `.sven/config.yaml` (workspace-local)
4. `sven.yaml` (project root)
5. The path given with `--config /path/to/config.yaml` (highest priority)

---

## View your current configuration

To see the full resolved configuration with all defaults filled in:

```sh
sven show-config
```

This prints the effective YAML to standard output. It is a convenient way to
check the result after editing the file, or to generate a starting point:

```sh
sven show-config > ~/.config/sven/config.yaml
```

---

## List available models

To see all models in the built-in catalog:

```sh
sven list-models
```

Filter by provider:

```sh
sven list-models --provider anthropic
```

Query the provider API for a live list (requires API key):

```sh
sven list-models --refresh
```

Output as JSON:

```sh
sven list-models --json
```

---

## Full annotated example

The following example shows every available option with its default value and
an explanation. You do not need to include options you are not changing.

```yaml
# ── Model ──────────────────────────────────────────────────────────────────

model:
  # Provider to use. Supported values: "openai", "anthropic", "mock"
  provider: openai

  # Model name forwarded to the provider.
  name: gpt-4o

  # Environment variable that holds the API key.
  # The variable is read at runtime so it never needs to be in this file.
  api_key_env: OPENAI_API_KEY

  # Alternatively, embed the key directly (not recommended for shared files).
  # api_key: sk-...

  # Override the provider's API base URL.
  # Useful for local proxies (e.g. LiteLLM) or self-hosted models.
  # base_url: http://localhost:4000/v1

  # Maximum tokens to request in a single response.
  # When unset, the model's max output tokens from the built-in catalog is used.
  # max_tokens: 4096

  # Sampling temperature (0.0 = deterministic, 2.0 = very random).
  temperature: 0.2

  # Path to a YAML file of scripted mock responses (provider: "mock" only).
  # Can also be set with the SVEN_MOCK_RESPONSES environment variable.
  # mock_responses_file: /path/to/responses.yaml

  # ── Anthropic prompt caching ─────────────────────────────────────────────
  #
  # Sven uses all four of Anthropic's available cache breakpoints, making it
  # the most cache-efficient agent available:
  #
  #   Breakpoint 1 (tools)        – tool definitions, stable per session
  #   Breakpoint 2 (system)       – system prompt, stable per session
  #   Breakpoint 3 (images/tools) – oldest image or large tool result in history
  #   Breakpoint 4 (conversation) – automatic, advances each turn
  #
  # Reading from cache costs 10% of base input-token price; writing costs 125%.
  # For a 50-turn session caching 10,000 tokens: ~88% savings on those tokens.

  # Cache the stable system prompt (Anthropic only).  DEFAULT: true
  # Saves ~90% on system prompt tokens after the first request.
  # Charges a one-time 25% write premium, then 10% per read.
  # cache_system_prompt: true

  # Cache tool definitions (Anthropic only).  DEFAULT: true
  # All tool definitions are cached as a prefix when true.
  # Ideal for saving 5,000–10,000+ tokens per request when many tools are in use.
  # cache_tools: true

  # Enable automatic conversation caching (Anthropic only).  DEFAULT: true
  # Adds a top-level cache_control marker so Anthropic automatically caches
  # conversation history up to the last message.  The cache breakpoint advances
  # with each turn — no manual management required.
  # Delivers the largest savings for multi-turn agent sessions.
  # cache_conversation: true

  # Cache image content blocks in conversation history (Anthropic only).  DEFAULT: true
  # Images cost hundreds of tokens each, every single turn.  Marking them
  # with cache_control once reduces that cost by ~90% for all following turns.
  # The oldest images are cached first; the budget is bounded to 4 total slots.
  # cache_images: true

  # Cache large tool results in conversation history (Anthropic only).  DEFAULT: true
  # File reads, command output, and fetched documents that remain in context
  # for many turns are cached once their content exceeds 4096 characters.
  # Saves ~90% on those tokens every subsequent turn.
  # cache_tool_results: true

  # Use 1-hour cache TTL instead of the default 5-minute window.  DEFAULT: false
  # Applies to system prompt, tool definitions, images, and tool results when
  # their respective caching flags are enabled.  Sends the
  # extended-cache-ttl-2025-04-11 beta header automatically.  Best for
  # workflows where requests are spaced more than 5 minutes apart (e.g. CI).
  # extended_cache_time: false


# ── Agent ──────────────────────────────────────────────────────────────────

agent:
  # Default mode when --mode is not given on the command line.
  # Values: "research", "plan", "agent"
  default_mode: agent

  # Maximum number of tool-call rounds before sven stops and reports.
  # When the limit is reached, the model gets one final tool-free turn to
  # summarise progress before the turn ends.  Increase for very long tasks.
  max_tool_rounds: 200

  # Fraction of the input budget at which proactive compaction triggers.
  # The input budget is context_window − max_output_tokens (not the raw
  # context window), so this threshold is applied against the actual usable
  # space rather than the total model window.
  # 0.85 means compaction fires when 85% of the input budget is consumed.
  compaction_threshold: 0.85

  # Number of recent non-system messages to keep verbatim after compaction.
  # The oldest messages beyond this tail are summarised. Default: 6.
  # Set to 0 to summarise the full history (original behaviour).
  compaction_keep_recent: 6

  # Compaction checkpoint format.
  # "structured" (default): produces a typed Markdown checkpoint with fixed
  #   sections (Active Task, Key Decisions, Files & Artifacts, Constraints,
  #   Pending Items, Session Narrative). Easier for the model to navigate.
  # "narrative": uses the original free-form summarisation prompt.
  compaction_strategy: structured

  # Maximum tokens allowed for a single tool result before it is
  # deterministically truncated before entering the session.
  # Truncation is content-aware:
  #   shell / run_terminal_command : keeps first 60 + last 40 lines
  #   grep / search_codebase        : keeps leading matches
  #   read_file                      : keeps head + tail lines
  #   everything else                : hard-truncates at the character limit
  # A truncation notice is always appended so the model knows more exists.
  # Set to 0 to disable per-result truncation entirely.
  tool_result_token_cap: 4000

  # Fraction of the context window reserved for tool schemas, the dynamic
  # context block (git / CI info), and measurement error in the token
  # approximation. Reduces the effective compaction trigger threshold.
  #
  # Example: compaction_threshold=0.85, compaction_overhead_reserve=0.10
  # → compaction fires when calibrated session tokens reach 75% of the
  # input budget (effectively threshold − reserve = 0.75).
  compaction_overhead_reserve: 0.10

  # Override the system prompt sent to the model.
  # Leave unset to use the built-in prompt.
  # system_prompt: "You are a careful coding assistant..."


# ── Tools ──────────────────────────────────────────────────────────────────

tools:
  # Shell commands matching these glob patterns are approved automatically,
  # without asking for confirmation.
  auto_approve_patterns:
    - "cat *"
    - "ls *"
    - "find *"
    - "rg *"
    - "grep *"

  # Shell commands matching these patterns are always blocked.
  deny_patterns:
    - "rm -rf /*"
    - "dd if=*"

  # Timeout for a single tool call, in seconds.
  timeout_secs: 30

  # Run shell commands inside a Docker container for additional isolation.
  use_docker: false

  # Docker image to use when use_docker: true.
  # docker_image: ubuntu:22.04


# ── Web tools ──────────────────────────────────────────────────────────────

  web:
    # Maximum number of characters fetched from a URL.
    fetch_max_chars: 50000

    search:
      # API key for the Brave Search backend.
      # Can also be set with BRAVE_API_KEY environment variable.
      # api_key: BSA...


# ── Memory ─────────────────────────────────────────────────────────────────

  memory:
    # Path to the persistent memory JSON file.
    # Defaults to ~/.config/sven/memory.json
    # memory_file: /path/to/memory.json


# ── Lints ──────────────────────────────────────────────────────────────────

  lints:
    # Override the lint command for Rust projects.
    # Default: cargo clippy --message-format json
    # rust_command: cargo clippy --message-format json

    # Override the lint command for TypeScript/JavaScript projects.
    # typescript_command: npx eslint --format json .

    # Override the lint command for Python projects.
    # python_command: ruff check --output-format json .


# ── TUI appearance ─────────────────────────────────────────────────────────

tui:
  # Colour theme. Values: "dark", "light", "solarized"
  theme: dark

  # Show line numbers inside code blocks.
  code_line_numbers: false

  # Column at which markdown text wraps (0 = use terminal width).
  wrap_width: 0

  # Use plain ASCII characters instead of Unicode box-drawing characters.
  # Enable this if your terminal font renders Unicode as gibberish.
  # Can also be forced with SVEN_ASCII_BORDERS=1 environment variable.
  ascii_borders: false
```

---

## Section reference

### `model`

Controls which language model sven talks to and how.

| Key | Default | Description |
|-----|---------|-------------|
| `provider` | `"openai"` | Provider name: `"openai"`, `"anthropic"`, or `"mock"` |
| `name` | `"gpt-4o"` | Model identifier sent to the provider |
| `api_key_env` | `"OPENAI_API_KEY"` | Environment variable containing the API key |
| `api_key` | — | Inline API key (use `api_key_env` instead when possible) |
| `base_url` | — | Override the API endpoint (for proxies) |
| `max_tokens` | catalog max | Maximum tokens per response (defaults to model catalog value) |
| `temperature` | `0.2` | Sampling temperature (0.0–2.0) |
| `mock_responses_file` | — | Path to YAML mock responses (mock provider only) |
| `cache_system_prompt` | `true` | **(Anthropic)** Cache the stable system prompt prefix — breakpoint 2 |
| `cache_tools` | `true` | **(Anthropic)** Cache all tool definitions as a prefix — breakpoint 1 |
| `cache_conversation` | `true` | **(Anthropic)** Automatically cache full conversation history each turn — breakpoint 4 |
| `cache_images` | `true` | **(Anthropic)** Cache the oldest image blocks in conversation history — breakpoint 3 |
| `cache_tool_results` | `true` | **(Anthropic)** Cache large (>4 096 chars) tool results in conversation history — breakpoint 3 |
| `extended_cache_time` | `false` | **(Anthropic)** Use 1-hour TTL for system, tools, images, and tool-result caches instead of 5 minutes |

#### Provider caching behaviour

| Provider family | Cache mechanism | Notes |
|-----------------|-----------------|-------|
| **Anthropic** | Explicit `cache_control` breakpoints | Fully configured via the `cache_*` flags above. sven uses all 4 available breakpoints and separates volatile context (git/CI) into an uncached system block so the stable prefix always hits. |
| **OpenAI / Azure** | Automatic prefix caching | No config needed. sven keeps the system message stable across turns so the model's automatic prefix cache hits reliably. Cache-read tokens appear in the `cache_read` field of `TokenUsage` events. |
| **OpenRouter** | Automatic (gateway) + explicit cache key | sven sends the session UUID as `prompt_cache_key` in every request, pinning all turns in a session to the same cached prefix. |
| **DeepSeek** | Automatic prefix caching | sven reads `prompt_cache_hit_tokens` from the response and surfaces it the same as other providers. |
| **Google / Groq / Mistral / …** | Automatic or not supported | No explicit configuration required; cache savings are reflected in token usage where available. |

#### Supported providers

| Provider | `provider` value | API key variable |
|----------|-----------------|-----------------|
| OpenAI | `"openai"` | `OPENAI_API_KEY` |
| Anthropic | `"anthropic"` | `ANTHROPIC_API_KEY` |
| Mock (offline) | `"mock"` | — |

To use a proxy or local model that has an OpenAI-compatible API (such as
LiteLLM or Ollama), set `provider: openai` and override `base_url`:

```yaml
model:
  provider: openai
  name: llama3
  base_url: http://localhost:11434/v1
```

---

### `agent`

Controls the agent's autonomy and defaults.

| Key | Default | Description |
|-----|---------|-------------|
| `default_mode` | `"agent"` | Mode used when `--mode` is not passed |
| `max_tool_rounds` | `200` | Maximum autonomous tool-call rounds before stopping |
| `compaction_threshold` | `0.85` | Fraction of the input budget that triggers compaction |
| `compaction_keep_recent` | `6` | Recent non-system messages preserved verbatim during compaction |
| `compaction_strategy` | `"structured"` | Checkpoint format: `"structured"` or `"narrative"` |
| `tool_result_token_cap` | `4000` | Token cap per tool result before smart truncation; `0` disables |
| `compaction_overhead_reserve` | `0.10` | Fraction of context reserved for schemas and dynamic context |
| `system_prompt` | — | System prompt override (leave unset to use built-in) |

Increasing `max_tool_rounds` lets sven work on longer tasks without stopping.
Decreasing it gives you more control by forcing sven to pause and ask.

#### Context budget and compaction

sven uses a multi-layer system to keep sessions within the model's context
window at all times:

**Budget gate** — Before every model submission and after every batch of tool
results, sven checks an effective token count that accounts for:
- Calibrated message tokens (corrected from API-reported counts over time)
- Tool schema overhead (schemas sent with every request but not stored in history)
- Dynamic context (git branch, CI info) injected per-request
- A configurable overhead reserve (`compaction_overhead_reserve`)

The effective threshold is `compaction_threshold − compaction_overhead_reserve`.
For example, with defaults (0.85 − 0.10 = 0.75), compaction fires when
calibrated session tokens reach 75% of the input budget
(`context_window − max_output_tokens`).

**Rolling compaction** — When the budget gate fires:
1. The oldest `(total – compaction_keep_recent)` non-system messages are
   serialised and compacted by the model.
2. The system prompt is re-issued so the model has full instructions.
3. The `compaction_keep_recent` most recent messages are restored verbatim.

With `compaction_strategy: structured` (default), the model produces a typed
Markdown checkpoint with dedicated sections for Active Task, Key Decisions,
Files & Artifacts, Constraints, Pending Items, and a Session Narrative.
Set `compaction_strategy: narrative` to use the original free-form summary.

**Smart tool-result truncation** — Before any tool result enters the session,
`tool_result_token_cap` is applied. Truncation is content-aware:
shell output keeps the head and tail; grep output keeps leading matches;
file content keeps head and tail. A notice is always appended so the model
can retrieve more with a targeted follow-up call.

**Emergency fallback** — If the session is already too large to fit even the
compaction prompt, the oldest messages are dropped deterministically (no model
call needed). The model is notified via a canned notice and the session
continues without crashing.

**Calibration** — After every model turn, sven updates a running calibration
factor from the API-reported input token count. This exponential moving
average corrects the chars/4 approximation over time, so token estimates
improve automatically within a session.

Setting `compaction_keep_recent: 0` disables the rolling strategy and
summarises the full history, which produces the smallest sessions at the cost
of losing immediate context.

##### CI / long-running workflow tuning

For CI pipelines with many tool calls and large file outputs:

```yaml
agent:
  compaction_threshold: 0.80
  compaction_overhead_reserve: 0.12
  tool_result_token_cap: 2000   # tighter cap for CI workloads
  compaction_strategy: structured
```

For interactive sessions where you want to preserve more recent context:

```yaml
agent:
  compaction_threshold: 0.85
  compaction_keep_recent: 10
  tool_result_token_cap: 6000
```

---

### `tools`

Controls what the agent is allowed to do and how.

| Key | Default | Description |
|-----|---------|-------------|
| `auto_approve_patterns` | `["cat *", "ls *", …]` | Commands matching these run without confirmation |
| `deny_patterns` | `["rm -rf /*", …]` | Commands matching these are always blocked |
| `timeout_secs` | `30` | Per-tool-call timeout in seconds |
| `use_docker` | `false` | Sandbox shell execution in Docker |
| `docker_image` | — | Docker image for sandboxed execution |

**Adding auto-approve patterns:**

```yaml
tools:
  auto_approve_patterns:
    - "cat *"
    - "ls *"
    - "rg *"
    - "grep *"
    - "cargo test*"    # auto-approve test runs
    - "make check"     # auto-approve linting
```

**Blocking specific commands:**

```yaml
tools:
  deny_patterns:
    - "rm -rf /*"
    - "dd if=*"
    - "curl * | sh"    # block shell-pipe downloads
```

---

### `tools.web`

| Key | Default | Description |
|-----|---------|-------------|
| `fetch_max_chars` | `50000` | Maximum characters fetched from a URL |
| `search.api_key` | — | Brave Search API key (also `BRAVE_API_KEY` env var) |

---

### `tools.memory`

| Key | Default | Description |
|-----|---------|-------------|
| `memory_file` | `~/.config/sven/memory.json` | Where persistent memory is stored |

---

### `tools.lints`

These let you override the command sven runs when you ask it to check for lint
errors. The commands should produce JSON output that sven can parse.

| Key | Default | Description |
|-----|---------|-------------|
| `rust_command` | `cargo clippy --message-format json` | Rust lint command |
| `typescript_command` | `npx eslint --format json .` | TypeScript/JS lint command |
| `python_command` | `ruff check --output-format json .` | Python lint command |

---

### `tools.gdb`

Configures the integrated GDB debugging support.

| Key | Default | Description |
|-----|---------|-------------|
| `gdb_path` | `"gdb-multiarch"` | GDB executable name or absolute path |
| `command_timeout_secs` | `10` | Seconds to wait for a GDB command response |
| `server_startup_wait_ms` | `500` | Milliseconds to wait after spawning the GDB server |

**Example:**

```yaml
tools:
  gdb:
    gdb_path: /usr/bin/gdb-multiarch
    command_timeout_secs: 30
    server_startup_wait_ms: 1000
```

Increase `server_startup_wait_ms` if your GDB server (e.g. JLinkGDBServer) takes
more than half a second to open its TCP port before sven reports it as ready.

---

### `tui`

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

```yaml
model:
  provider: anthropic
  name: claude-opus-4-5
  api_key_env: ANTHROPIC_API_KEY
```

**Use Anthropic Claude — all four cache breakpoints are on by default:**

```yaml
model:
  provider: anthropic
  name: claude-sonnet-4-5
  api_key_env: ANTHROPIC_API_KEY
  # Nothing extra needed: sven enables comprehensive caching out of the box.
  # Add extended_cache_time: true for CI or any workflow with gaps > 5 minutes:
  extended_cache_time: true    # 1-hour TTL for system/tools/images/tool-results
```

> **Cost note**: On a 10-turn agent session with 30 000 tokens of stable context,
> full caching reduces per-turn input cost from ~$0.09 to ~$0.003 — roughly a 97%
> reduction after the first (cache-write) request.  To opt out of a specific
> layer, set the corresponding flag to `false` (e.g. `cache_images: false`).

**Disable all caching (e.g. for cost-sensitive one-shot runs):**

```yaml
model:
  provider: anthropic
  name: claude-sonnet-4-5
  api_key_env: ANTHROPIC_API_KEY
  cache_system_prompt: false
  cache_tools: false
  cache_conversation: false
  cache_images: false
  cache_tool_results: false
```

**Use a local Ollama model:**

```yaml
model:
  provider: openai
  name: codellama
  base_url: http://localhost:11434/v1
```

**Auto-approve all read and test commands:**

```yaml
tools:
  auto_approve_patterns:
    - "cat *"
    - "ls *"
    - "find *"
    - "rg *"
    - "grep *"
    - "cargo test*"
    - "make test"
    - "pytest *"
```

**Use ASCII borders (terminal font compatibility):**

```yaml
tui:
  ascii_borders: true
```

---

## Migration from TOML

If you have an existing `config.toml`, convert it to YAML and rename it to
`config.yaml`. Below is a quick reference for the most common changes:

| TOML | YAML equivalent |
|------|----------------|
| `[model]` section header | `model:` key |
| `provider = "openai"` | `provider: openai` |
| `name = "gpt-4o"` | `name: gpt-4o` |
| `max_tokens = 4096` | `max_tokens: 4096` |
| `auto_approve_patterns = ["cat *"]` | `auto_approve_patterns:\n  - "cat *"` |

Online converters such as [transform.tools/toml-to-yaml](https://transform.tools/toml-to-yaml)
can convert a full config file automatically.
