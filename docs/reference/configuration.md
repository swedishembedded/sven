<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# Configuration

sven is configured through YAML files. Every option has a default, so a
configuration file is optional: write one only to change something.

---

## Config file location

sven reads every file it finds below and merges them from lowest to highest
priority; a later file overrides an earlier one. A file holds only the keys
written into it; every other key keeps its default.

1. `/etc/sven/config.yaml` (Linux only; system-wide)
2. The user's file: `~/.config/sven/config.yaml`, and `sven/config.yaml` in the
   platform configuration directory when that is not `~/.config`
   (`~/Library/Application Support` on macOS, `%APPDATA%` on Windows)
3. The workspace's file: the nearest configuration file in a directory above
   the current one
4. The project's file: the configuration file of the current directory
5. The file named by `--config FILE`, which overrides all of the above

A directory's configuration file is the first of `.sven/config.yaml`,
`.sven/config.yml`, `.sven.yaml`, `.sven.yml`, `sven.yaml` and `sven.yml` it
holds; the user's may be `.yml` as well.

With no configuration file at all, a session still starts (a local brain
server, a stored key) and says to run `sven init`, which writes a
`.sven.yaml` here naming an OpenRouter GLM model and no key.

### Changing a value from the command line

`sven config set <key> <value>` writes one key into a file, creating it as
needed, and `sven config get <key>` prints the value in force: as a file
writes it, else the default. A key is a dotted path (`model.name`); a segment
holding a dot is quoted (`providers.or.models."z-ai/glm-5.3".max_tokens`).
The value is read as YAML: `0.3` is a number, `true` a bool, `[a, b]` a list,
anything else text.

```sh
sven config set model.name z-ai/glm-5.3-flash      # ./.sven.yaml
sven config set --scope user model.temperature 0.3  # ~/.config/sven/config.yaml
sven config set --scope workspace agent.max_tool_rounds 50
sven config set model.api_key --stdin               # typed without echo, never in shell history
sven config get model.name
sven config get --scope user model.temperature      # only what that file says
```

`--scope` is `project` (the default: the file here, made as `.sven.yaml`),
`workspace` (the nearest file above here; refused when there is none) or
`user`. A key sven does not know, or a value of the wrong type, is refused and
the file left as it was. Files are rewritten whole by `set`, so comments in
them are not kept. A key stored with `sven auth set` is the better home for a
hosted provider's key: it is never in a file a repository holds.

A file that exists but is not valid YAML, and a `--config` file that cannot be
read, stop sven with an error. Files are YAML only.

---

## How the file is read

Every file found is read as YAML and merged into one document before anything
looks at it: mappings merge key by key, and where two files set the same key
the later (higher-priority) one wins. A list or a scalar is replaced whole,
not merged.

**Environment variables.** Before a file is parsed, `${VAR}` and
`${VAR:-default}` in its text are replaced from the process environment, and
`$$` is a literal `$`. An unset `${VAR}` without a default becomes an empty
string and logs a warning naming the variable and the file. A value that itself
contains `${...}` is not expanded again. Use it to keep a key out of a file
under version control:

```yaml
providers:
  work:
    name: anthropic
    api_key: ${WORK_ANTHROPIC_KEY:-}
```

**Mistakes are reported, not fatal.** A key sven does not recognise is
logged as a warning naming its path (``Unrecognised config field
`model.temprature` ``), and a section that is accepted but acted on by nothing
is reported once. A value of the wrong type is different: the file cannot be
read, so sven logs `config could not be decoded and is being IGNORED IN FULL`
and runs on the defaults rather than refusing to start. Read the warnings after
editing the file; `sven show-config` prints what was actually loaded. The
contents of `tools.context` are accepted as written and not checked for
misspelt keys.

**The model is chosen for you when the file names none.** With no `model:`
section, sven picks the model of a brain server running on this machine (found
from the connection record brain publishes), then OpenRouter, Anthropic or
OpenAI according to which key is stored (`sven auth set`). With no key for any
of them, sven looks for a local server (see
[Local model discovery](#local-model-discovery)). The order and the models
chosen are listed in [Providers](providers.md#default-provider-auto-detection).
A `model.provider` that names an entry of `providers:` is expanded into that
entry's endpoint, key and the model's own overrides when the file is loaded.

**Who reads what.** Each section is defined, defaulted and checked by the part
of sven that uses it, so a section's keys are documented with the feature they
configure:

| Section | Configures |
|---------|------------|
| `model`, `providers` | the active model and the named endpoints a model can be reached through |
| `agent` | the loop's bounds, compaction, timeouts and stall limits |
| `tools` | shell patterns, `disabled`, `timeout_secs`, `disclosure` and the settings of the web, memory, lint, GDB, context and speech-to-text tools |
| `mcp_servers` | external MCP servers |
| `context`, `cost`, `delegation` | context assembly, the cache-aware cost gate and the delegation policy |
| `decision` | the decision model that advises the harness |
| `skills`, `subagents`, `knowledge` | how much of each catalog the system prompt lists |
| `tui` | the interactive UI (default build only) |

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

To see every model that can be used now, asked live from each local server
that answers, each `providers:` entry and each hosted provider with a stored
key, with the name `--model` takes for it:

```sh
sven list-models
```

Only one provider or `providers:` entry:

```sh
sven list-models --provider openrouter
```

The bundled catalog instead:

```sh
sven list-models --catalog
```

Output as JSON:

```sh
sven list-models --json
```

`sven list-providers` prints the provider ids that `model.provider` accepts.

---

## Full annotated example

The following example shows every available option. Values are the defaults
unless a comment says otherwise; options that have no default, or whose default
depends on the model or environment, are commented out. You do not need to
include options you are not changing.

```yaml
# ── Model ──────────────────────────────────────────────────────────────────

model:
  # Provider id. Run `sven list-providers` for the supported values.
  # With no model: section sven picks one from what is running and the keys
  # stored with `sven auth set` (see above); the built-in fallback is
  # openrouter / openrouter/auto.
  provider: openai

  # Model name forwarded to the provider.
  name: gpt-4o

  # File that holds the API key (~/ is the home directory). Unset: the key
  # stored for the provider with `sven auth set openai`, so it never needs to
  # be in this file. Keys are never read from the environment.
  api_key_file: ~/.config/keys/openai

  # Alternatively, embed the key directly (not recommended for shared files).
  # api_key: sk-...

  # Override the provider's API base URL.
  # Useful for local proxies (e.g. LiteLLM) or self-hosted models.
  # base_url: http://localhost:4000/v1

  # Total context window in tokens (input + output). Used for compaction.
  # Unset: the window the built-in catalog or the local server reports.
  # With max_output_tokens unset it also limits the output of one response.
  # max_tokens: 128000

  # Output tokens per response, and an optional cap on the input side.
  # When more than one of the three is set, max_tokens must be at least
  # max_input_tokens + max_output_tokens (a warning is logged otherwise).
  # max_output_tokens: 4096
  # max_input_tokens: 100000

  # Sampling temperature (0.0 = deterministic, 2.0 = very random).
  temperature: 0.2

  # Provider-specific options forwarded as-is to the driver.
  # driver_options: {}
  # enable_thinking: false   # brain: sent as chat_template_kwargs.enable_thinking; unset sends nothing

  # Input modalities the model accepts: any of text, image, audio. Declare
  # them for a self-hosted endpoint the catalog does not know.
  # input_modalities: [text, image, audio]

  # Azure OpenAI (provider: azure)
  # azure_resource: my-resource
  # azure_deployment: my-deployment      # default: model.name
  # azure_api_version: "2024-02-01"

  # AWS Bedrock (provider: aws)
  # aws_region: us-east-1

  # Path to a YAML file of scripted mock responses (provider: "mock" only).
  # Can also be set with the SVEN_MOCK_RESPONSES environment variable.
  # mock_responses_file: /path/to/responses.yaml

  # ── Anthropic prompt caching ─────────────────────────────────────────────
  #
  # sven places up to four of Anthropic's cache breakpoints:
  #
  #   Breakpoint 1 (tools)        - tool definitions, stable per session
  #   Breakpoint 2 (system)       - system prompt, stable per session
  #   Breakpoint 3 (images/tools) - oldest image or large tool result in history
  #   Breakpoint 4 (conversation) - automatic, advances each turn
  #
  # Cache reads are billed below the base input price and cache writes above
  # it; see Anthropic's pricing for the current rates. Other providers cache
  # automatically and ignore these flags.

  # Cache the stable system prompt (Anthropic only).
  cache_system_prompt: true

  # Cache tool definitions (Anthropic only).
  cache_tools: true

  # Automatic conversation caching (Anthropic only). Adds a top-level
  # cache_control marker so the cache breakpoint advances with each turn.
  cache_conversation: true

  # Cache image content blocks in conversation history (Anthropic only).
  # The oldest images are cached first; the budget is bounded to 4 slots.
  cache_images: true

  # Cache large tool results in conversation history (Anthropic only).
  # Eligible once a result exceeds 4096 characters.
  cache_tool_results: true

  # Use the 1-hour cache TTL instead of 5 minutes (Anthropic only). Applies to
  # the system prompt, tool definitions, images and tool results when their
  # flags are on. Sends the extended-cache-ttl-2025-04-11 beta header. Best
  # for workflows whose requests are more than 5 minutes apart (e.g. CI).
  extended_cache_time: false


# ── Named endpoints ────────────────────────────────────────────────────────
#
# Reference one by name in model.provider and one of its models in model.name.
# An entry takes the model keys above (name is the driver id) plus a models:
# map of per-model overrides.
#
# providers:
#   local_vllm:
#     name: openai
#     base_url: http://localhost:8000/v1
#     models:
#       my-model:
#         max_tokens: 54272


# ── Agent ──────────────────────────────────────────────────────────────────

agent:
  # Mode of a run without --mode: research, plan, agent, chat or sdlc.
  default_mode: agent

  # Maximum number of tool-call rounds before sven stops and reports.
  # When the limit is reached, the model gets one final tool-free turn to
  # summarise progress before the turn ends.  Increase for very long tasks.
  max_tool_rounds: 200

  # Fraction of the input budget at which proactive compaction triggers,
  # less compaction_output_reserve (below). The input budget is
  # context_window − 256. Compaction also fires earlier when the request
  # that asks for the notes would stop fitting the window.
  compaction_threshold: 0.85

  # Fraction of the context window kept free for the model's response,
  # reasoning included. Compaction fires when the request plus this reserve
  # passes the threshold, so a normal turn always has room to think and
  # answer.
  compaction_output_reserve: 0.25

  # Most tokens the notes a compaction asks the model to write may take
  # (also capped at a quarter of the window).
  compaction_summary_tokens: 10000

  # Maximum tokens allowed for a single tool result before it is
  # deterministically truncated before entering the session.
  # Truncation is content-aware:
  #   shell                         : keeps first 60 + last 40 lines
  #   grep                          : keeps leading matches
  #   read_file                     : keeps head + tail lines
  #   everything else               : hard-truncates at the character limit
  # A truncation notice is always appended so the model knows more exists.
  # Set to 0 to disable per-result truncation entirely.
  tool_result_token_cap: 4000

  # A call that repeats an earlier call of the same run and gets the same
  # result carries a note saying so ("unchanged; take a different action").
  # Calls are compared without cosmetic differences: a `description` field,
  # whitespace, key order.
  note_repeated_results: true

  # Fraction of the input budget set aside for what the token count does not
  # see (a dynamic context block, estimation error). The count of everything
  # already sent is the server's own, so the default is 0.
  compaction_overhead_reserve: 0.0

  # Override the system prompt sent to the model.
  # Leave unset to use the built-in prompt.
  # system_prompt: "You are a careful coding assistant..."

  # Wall-clock limit of one headless workflow step, in seconds (0 = no limit).
  max_step_timeout_secs: 0

  # Wall-clock limit of a whole headless run, in seconds (0 = no limit).
  # The run ends with exit code 124. The default is four hours.
  max_run_timeout_secs: 14400

  # Wall-clock budget of each child agent run (a `task` sub-agent, a parallel
  # SDLC task), in seconds (0 = no limit).
  child_run_timeout_secs: 3600

  # Thinking-loop watchdog. No token cap by default (opt-in); a repetition
  # detector and a 600 second no-progress limit stop loops.
  # max_thinking_tokens: 20000
  # thinking_timeout_secs: 600

  # Stalled-server limits, in seconds (unset or 0 = the default; the first
  # value is for a hosted API, the second for a server on your own hardware).
  # stream_idle_timeout_secs: 300       # 1200
  # first_byte_timeout_secs: 120        # 600
  # first_token_timeout_secs: 300       # 900
  # no_progress_timeout_secs: 600


# ── Tools ──────────────────────────────────────────────────────────────────

tools:
  # Under --approval manual, shell commands matching these glob patterns run
  # without asking for confirmation. (Under the default auto approval nothing
  # asks anyway.)
  auto_approve_patterns:
    - "cat *"
    - "ls *"
    - "find *"
    - "rg *"
    - "grep *"

  # Shell commands matching these patterns are always refused, whatever the
  # approval mode.
  deny_patterns:
    - "rm -rf /*"
    - "dd if=*"

  # Tools, by name, a session never offers the model and never runs.
  disabled: []

  # Timeout for a single tool call, in seconds.
  timeout_secs: 30

  # Longest a single shell call may ask for through its own `timeout_secs`
  # argument, in seconds; a larger request is cut to it.
  max_timeout_secs: 1800

  # How much of each tool's definition every request carries: "full" sends all
  # of them; "deferred" sends the core tools and the ones the model has loaded
  # through the `tools` tool, and describes the rest in one line each.
  # (Deferred is the default of a program that reads a configuration file.)
  disclosure:
    mode: deferred


# ── Web tools ──────────────────────────────────────────────────────────────

  web:
    # Maximum number of characters fetched from a URL.
    fetch_max_chars: 20000

    search:
      # API key for the Brave Search backend.
      # Can also be set with BRAVE_API_KEY environment variable.
      # api_key: BSA...


# ── Memory ─────────────────────────────────────────────────────────────────

  memory:
    # Path to the persistent memory JSON file.
    # Defaults to memory.json in sven's configuration directory
    # (~/.config/sven/memory.json on Linux).
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


# ── GDB ────────────────────────────────────────────────────────────────────

  gdb:
    gdb_path: gdb-multiarch
    command_timeout_secs: 10
    connect_timeout_secs: 30
    server_startup_wait_ms: 500


# ── Large-content (context) tools ──────────────────────────────────────────

  context:
    max_parallel: 4
    default_chunk_lines: 500
    sub_query_max_chars: 120000
    sub_query_timeout_secs: 120


# ── Speech-to-text for attached audio ──────────────────────────────────────

  asr:
    model: brain/nemotronasr
    # D-Bus address of the brain server; unset = the session bus.
    # bus_address: unix:path=/run/brain/bus
    timeout_secs: 120


# ── MCP servers ────────────────────────────────────────────────────────────
#
# mcp_servers:
#   github:
#     transport:
#       type: stdio
#       command: npx
#       args: ["-y", "@modelcontextprotocol/server-github"]
#     env:
#       GITHUB_TOKEN: ${GITHUB_TOKEN:-}
#     enabled: true
#     timeout_secs: 30


# ── Context assembly ───────────────────────────────────────────────────────

context:
  assembly:
    mode: off                   # off | demote | reconstruct
    budget_tokens: 12000
    keep_recent_rounds: 2
    min_demote_tokens: 64
    resident_bytes: 16777216


# ── Cache-aware cost gate ──────────────────────────────────────────────────

cost:
  cache_aware: off              # off | advise | enforce
  # remaining_requests: 20
  # prices:
  #   openai/gpt-4o:
  #     input_per_mtok: 2.5
  #     output_per_mtok: 10.0


# ── Delegation policy ──────────────────────────────────────────────────────

delegation:
  policy: off                   # off | advise | enforce
  child_overhead_tokens: 4000
  report_tokens: 400
  min_samples: 2
  unknown_work_tokens: 6000
  replay_rounds: 3
  parallel_credit_tokens: 1500
  write_surcharge_tokens: 1000
  context_window_tokens: 128000
  assumed_parent_context_tokens: 8000


# ── Decision model ─────────────────────────────────────────────────────────

decision:
  enabled: false
  mode: auto                    # auto | required | off
  provider: brain
  model: auto
  timeout_ms: 500
  warn_if_unavailable: true
  brain:
    url: http://127.0.0.1:8788
    # api_key: ${BRAIN_API_KEY:-}
    max_concurrent_requests: 2
    cache_entries: 256
  features:
    context_selection: true
    tool_routing: true
    skill_selection: true
    delegation: true
    model_routing: false
    cache_routing: true
    permission_advisory: false
  policy:
    mode: apply                 # apply | shadow
    # max_decision_input_tokens: 512
    minimum_model_quality: validated   # validated | any


# ── Instruction catalogs ───────────────────────────────────────────────────
# full | deferred (deferred is the default of a program that reads a file)

skills:
  disclosure: deferred
subagents:
  disclosure: deferred
knowledge:
  disclosure: deferred


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

  # Show the model's thinking in the chat. When off, thinking is kept in the
  # session but not drawn; the status bar says "Thinking" while it streams.
  show_thinking: false
```

---

## Section reference

### `model`

Controls which language model sven talks to and how.

| Key | Default | Description |
|-----|---------|-------------|
| `provider` | `"openrouter"` | Provider id (`sven list-providers`) or the name of a `providers:` entry |
| `name` | `"openrouter/auto"` | Model identifier sent to the provider. Empty on a local server: the model the server serves |
| `api_key_file` | the key stored for the provider (`sven auth set`), sent only to the provider's own endpoint | File holding the API key; `~/` is the home directory |
| `api_key` | - | Inline API key (use `sven auth set` or `api_key_file` instead when possible) |
| `base_url` | provider default | Override the API endpoint (proxies, gateways, local servers) |
| `max_tokens` | catalog or server value | Total context window in tokens (input plus output), used for compaction. With `max_output_tokens` unset it also limits one response |
| `max_output_tokens` | catalog or server value | Output tokens per response |
| `max_input_tokens` | - | Cap on the input side of the window before compaction is forced |
| `temperature` | `0.2` | Sampling temperature (0.0-2.0) |
| `driver_options` | - | Provider-specific options forwarded to the driver |
| `enable_thinking` | unset | `brain` only: sent as `chat_template_kwargs.enable_thinking`; unset sends nothing and the served template decides |
| `input_modalities` | the driver's answer | Any of `text`, `image`, `audio`; declares what a self-hosted endpoint accepts |
| `azure_resource`, `azure_deployment`, `azure_api_version` | - | Azure OpenAI endpoint; the deployment defaults to `name` |
| `aws_region` | - | AWS Bedrock region (also `AWS_DEFAULT_REGION`) |
| `mock_responses_file` | - | Path to YAML mock responses (mock provider only; also `SVEN_MOCK_RESPONSES`) |
| `cache_system_prompt` | `true` | **(Anthropic)** Cache the stable system prompt prefix - breakpoint 2 |
| `cache_tools` | `true` | **(Anthropic)** Cache all tool definitions as a prefix - breakpoint 1 |
| `cache_conversation` | `true` | **(Anthropic)** Automatically cache full conversation history each turn - breakpoint 4 |
| `cache_images` | `true` | **(Anthropic)** Cache the oldest image blocks in conversation history - breakpoint 3 |
| `cache_tool_results` | `true` | **(Anthropic)** Cache large (>4 096 chars) tool results in conversation history - breakpoint 3 |
| `extended_cache_time` | `false` | **(Anthropic)** Use 1-hour TTL for system, tools, images, and tool-result caches instead of 5 minutes; conversation caching always uses 5 minutes |

When `max_tokens`, `max_input_tokens` and `max_output_tokens` are set together,
`max_tokens` must be at least the sum of the other two; sven logs a warning
naming the section when it is not.

#### Provider caching behaviour

| Provider family | Cache mechanism | Notes |
|-----------------|-----------------|-------|
| **Anthropic** | Explicit `cache_control` breakpoints | Fully configured via the `cache_*` flags above. sven uses all 4 available breakpoints and separates volatile context (git/CI) into an uncached system block so the stable prefix always hits. |
| **OpenAI / Azure** | Automatic prefix caching | No config needed. sven keeps the system message stable across turns so the model's automatic prefix cache hits reliably. Cache-read tokens appear in the `cache_read` field of `TokenUsage` events. |
| **OpenRouter** | Automatic (gateway) + explicit cache key | sven sends the session UUID as `prompt_cache_key` in every request, pinning all turns in a session to the same cached prefix. |
| **DeepSeek** | Automatic prefix caching | sven reads `prompt_cache_hit_tokens` from the response and surfaces it the same as other providers. |
| **Google / Groq / Mistral / ...** | Automatic or not supported | No explicit configuration required; cache savings are reflected in token usage where available. |

#### Supported providers

`sven list-providers` prints every provider id, and `-v` adds whether a key is
stored for each and its base URL; [Providers](providers.md) walks through the
setup of each. A key is read from `api_key`, else `api_key_file`, else the key
stored with `sven auth set <provider>` (at the provider's own endpoint only).

To use a proxy or local model that has an OpenAI-compatible API (such as
LiteLLM or Ollama), set `provider: openai` and override `base_url`:

```yaml
model:
  provider: openai
  name: llama3
  base_url: http://localhost:11434/v1
```

#### Local model discovery

For a server on your own machine or network, sven asks the server what it is
serving (`GET <base_url>/models`) instead of relying on a model name written
in the file. There are no extra keys for it.

- **An empty `name` is filled in.** On a local OpenAI-compatible server
  (`brain`, `ollama`, `vllm`, `lmstudio`, `sven`, or any `base_url` on
  `localhost`, a loopback address or a private network address) an empty
  `model.name` resolves to the model the server serves. With several models,
  the first one that reports a context length is used (the first listed when
  none do), and an info line `N models served, using <id>` is logged. A
  hosted provider is never asked, and a `name` you set is always sent as-is.
- **Nothing configured: the default local ports are probed.** When no
  `model:` section exists and no key is available for brain, OpenRouter,
  Anthropic or OpenAI, sven probes `brain` (8788), `ollama` (11434), `vllm`
  (8000) and `lmstudio` (1234) on localhost together, waiting at most one
  second, and uses the first in that order that serves a model. It logs
  `using <driver> at <url>, model <id>`. If none answers, you get the usual
  missing-key error. Cloud setups are unchanged: a key in the environment
  still selects its provider and that provider's default model, because a
  hosted account lists hundreds of models and no served model can stand in
  for that choice.
- **The context window follows the server.** At session start sven reads the
  window the server loaded the model with (`/props`, else `context_length` or
  vLLM's `max_model_len` from `/models`) and uses the smaller of that and
  `max_tokens`. On a local server the reading is refreshed in the background
  when it is older than 45 seconds at the start of a turn, and immediately
  when the server rejects a request as too large for its context. A turn never
  waits for a refresh.
- **Hot-swapped adapters are noticed.** A server that reports an
  `adapter: {id, digest}` for a model has it compared on every refresh. A
  changed digest is logged and reported to the session before its next turn
  (headless runs print `[sven:info] model changed: <model> adapter <old> ->
  <new> (context window N)`; the TUI shows it in the status bar; ACP sends it
  as a thought chunk), and the next request is budgeted against the new
  context window. The first probe is only a baseline, and several swaps
  between two turns are reported as one. Servers that report none work
  unchanged.
- **The server's output cap and tool support are used.** When neither
  `max_output_tokens` nor `max_tokens` is set, the output-token limit of a
  request is the one the server reports. When the server lists capabilities
  and tool calling is not among them, the first turn that offers tools warns
  once (`[sven:warn] <model> does not advertise tool calling ...`) and
  carries on; a server that lists no capabilities is never warned about.
- **Servers that do not answer cost at most two seconds per probe.** Every
  discovery call is bounded; failure leaves the configuration as written.

Optional fields a server may add to each `/models` entry, all ignored when
absent: `context_length` (or `max_model_len`, `context_window`), `max_output_tokens`,
`capabilities` (a list; tool calling is recognised from `tools`, `tool_use`,
`tool_calls` or `function_calling`) and `adapter`. The bundled catalog is used
only for what a server leaves out.

---

### `providers`

Named endpoints. Set `model.provider` to an entry's name and `model.name` to
one of its models; the entry is expanded into the active model when the file is
loaded, so a second sven process started with the same files reaches the same
endpoint.

| Key | Description |
|-----|-------------|
| `name` | Driver id that speaks the endpoint's protocol (default `openai`) |
| `base_url`, `api_key_file`, `api_key` | As for `model` |
| `max_tokens`, `max_output_tokens`, `max_input_tokens`, `temperature`, `driver_options`, `input_modalities` | Defaults for every model on the endpoint |
| `azure_resource`, `azure_deployment`, `azure_api_version`, `aws_region`, `mock_responses_file` | As for `model` |
| `models.<name>` | Per-model overrides: `max_tokens`, `max_output_tokens`, `max_input_tokens`, `temperature`, `driver_options`, `input_modalities`, `mock_responses_file` and the `cache_*` flags with `extended_cache_time` |

```yaml
model:
  provider: local_vllm
  name: my-model
providers:
  local_vllm:
    name: openai
    base_url: http://localhost:8000/v1
    models:
      my-model:
        max_tokens: 54272
```

### `agent`

Controls the agent's autonomy and defaults.

| Key | Default | Description |
|-----|---------|-------------|
| `default_mode` | `"agent"` | The mode of an interactive or headless run that gets no `--mode`: `research`, `plan`, `agent`, `chat` or `sdlc` |
| `max_tool_rounds` | `200` | Maximum autonomous tool-call rounds before stopping |
| `compaction_threshold` | `0.85` | Fraction of the input budget that triggers compaction |
| `compaction_output_reserve` | `0.25` | Fraction of the window kept free for the response (reasoning included) when judging the conversation full |
| `compaction_summary_tokens` | `10000` | Most tokens the compaction notes may take; also capped at a quarter of the window |
| `tool_result_token_cap` | `4000` | Token cap per tool result before smart truncation; `0` disables |
| `note_repeated_results` | `true` | Note on the result of a call that repeats an earlier one with the same result |
| `compaction_overhead_reserve` | `0.0` | Fraction of the input budget set aside for what the token count does not see |
| `system_prompt` | - | System prompt override (leave unset to use built-in) |
| `max_thinking_tokens` | none | Opt-in cap on estimated reasoning tokens per turn; see [Thinking-loop watchdog](#thinking-loop-watchdog) |
| `thinking_timeout_secs` | `600` | Seconds a model may reason with no forward progress before the turn aborts |
| `stream_idle_timeout_secs` | `300`; `1200` for a local server | Seconds a streamed response may stay silent before the connection is declared stale and the turn fails |
| `first_byte_timeout_secs` | `120`; `600` for a local server | Seconds to wait for the model server's response headers; see [Stalled model servers](#stalled-model-servers) |
| `first_token_timeout_secs` | `300`; `900` for a local server | Seconds, from the request, to wait for the first real output (text, reasoning or a tool-call chunk); keepalives do not count |
| `no_progress_timeout_secs` | `600` | Seconds of silence in real output, once it has started, before the turn fails; keepalives do not count |
| `max_step_timeout_secs` | `0` | Wall-clock limit of one headless workflow step; `0` = no limit. `--step-timeout` overrides it |
| `max_run_timeout_secs` | `14400` (4 hours) | Wall-clock limit of a whole headless run, covering every wait in it: model, tools, approvals and questions. The run ends with exit code `124`. `0` = no limit. `--run-timeout` overrides it; see [CI pipeline](../guides/headless.md#timeouts) |
| `child_run_timeout_secs` | `3600` | Wall-clock budget of each child agent run (a parallel SDLC task, a `task` sub-agent); the child is stopped when it runs out. `0` = no limit |

Increasing `max_tool_rounds` lets sven work on longer tasks without stopping.
Decreasing it gives you more control by forcing sven to pause and ask.

#### Context budget and compaction

sven keeps a session inside the model's context window in layers. At the
start of a session, and whenever it changes, it sends one `ContextBudget`
event (shown as a notice, `[sven:info]` in headless output) saying where it
stands, so that no compaction or cut-off comes as a surprise:

```
context budget: window 98304 tokens; compaction at ~58764 tokens of request; 24576 tokens kept free for the response; notes of at most 10000 tokens; thinking cap none
```

**When compaction fires** - Before every model request, including each round
of a tool loop after the tool results are appended. The size of the request
is the server's own token count of the previous request plus an estimate of
what was appended since (a chars/4 estimate with 10% headroom is used only
before the first count). Compaction fires at the earlier of:

- `compaction_threshold − compaction_overhead_reserve` of the input budget
  (`context_window − 256`), less the output reserve
  (`compaction_output_reserve` of the window), so that a normal turn always
  has room to think and answer;
- the last size at which the request asking for the notes still fits: the
  conversation, the instruction, `compaction_summary_tokens` (at most a
  quarter of the window) for the reply and a small template margin.

The window is the one the server reports (a live probe), or `max_tokens`
when you set it.

**What compaction does** - It forks the conversation. The model gets the
real messages (same system prompt, same tool calls and results, no
serialisation into text; the session's tool definitions are offered too, with
`tool_choice: none`, to share the prompt cache) followed by one instruction asking for everything
needed to continue: the goal, the constraints, decisions and failed
approaches, files and commands with their key facts, exact identifiers and
values, the current state, open questions and next steps. The reply, limited
to `compaction_summary_tokens`, replaces the history as one message. The
original task message and the todo list are kept verbatim. A question still
waiting for its answer stays after the notes. Token accounting starts over
from there. A tool call is never separated from its result. The model
writes the notes with reasoning switched off, so the reply budget cannot be
spent thinking; an empty or failed reply is retried once with the
provider's default.

A conversation too large for one fork is summarised in pieces: the largest
oldest part that fits first, then the rest. Nothing is dropped. A request the
server rejects as too long is answered the same way, calibrated by the
server's own count.

**Emergency fallback** - Only when the model cannot produce notes at all
(or a round of notes is no smaller than what it replaced), the oldest
messages are dropped without a model call, keeping the task, the todo list
and the six most recent messages. This is reported as a warning with the
number of tokens dropped, and recorded as an `emergency` compaction. If a
single exchange alone is larger than a fork can hold, it is clipped for the
summary and the tokens cut are reported.

**Output exhaustion** - If a response spends its whole output budget
thinking and produces no answer, sven reports the tokens used, compacts the
conversation, and asks again once. The reasoning of the failed attempt is not
resent.

**Smart tool-result truncation** - Before any tool result enters the session,
`tool_result_token_cap` is applied. Truncation is content-aware:
shell output keeps the head and tail; grep output keeps leading matches;
file content keeps head and tail. A notice is always appended so the model
can retrieve more with a targeted follow-up call.

The keys `compaction_keep_recent` and `compaction_strategy` no longer do
anything (a compaction summarises the whole conversation, with one prompt).
They are still accepted and reported at start-up as ignored.

##### CI / long-running workflow tuning

For CI pipelines with many tool calls and large file outputs:

```yaml
agent:
  compaction_threshold: 0.80
  tool_result_token_cap: 2000   # tighter cap for CI workloads
```

For interactive sessions where you want more detail kept in the notes:

```yaml
agent:
  compaction_summary_tokens: 16000
  tool_result_token_cap: 6000
```

#### Thinking-loop watchdog

Some models (local reasoning models such as Qwen are the common case) can
loop in their reasoning without ever converging to an answer. Thinking is part
of the response, so it is bounded like the rest of it: by the request's
output budget, the window less the prompt, which the server enforces. A
model that runs a local server may use all of it. There is no default token
cap: any default low enough to matter ended honest runs mid-thought (a tenth
of a 16k window did). Compaction, with its output reserve, keeps the window
in bounds instead. A loop is stopped by:

- **a repetition detector** on the reasoning stream: when the most recent
  400 characters already occur three times in the last 16 000, the turn is
  aborted and the error says how many thinking tokens were spent;
- **`thinking_timeout_secs`** - wall-clock time spent reasoning with *no*
  forward progress (a real text or tool-call delta). Reset on every sign of
  progress. Defaults to `600`.

and, if you want a hard ceiling, the opt-in **`max_thinking_tokens`**, an
estimate (chars/4) of reasoning tokens per turn.

A watchdog abort ends the turn like pressing `Esc` mid-generation (the partial
text streamed so far is kept, marked `[aborted: ...]`) but, because nobody asked
for it, it is also reported as an error naming the cause: a headless run exits
1 with outcome `agent_error`, and an embedding application's call fails with
that message. The notes of a compaction are not subject to the token cap.

```yaml
agent:
  max_thinking_tokens: 20000   # optional
  thinking_timeout_secs: 300
```

Both limits can also be adjusted live in a running session with `/think-limit`
(no args to show the current values).

#### Stalled model servers

A model server can go quiet in ways that look like a working connection. Each
has its own limit; a turn that hits one fails with an error naming the driver,
the server's host and the limit, and is handled like any other model failure.

| Server behaviour | Limit | Error starts with |
|------------------|-------|-------------------|
| Accepts the connection, never sends response headers | `first_byte_timeout_secs` | `no response headers from <driver> <host> after N s` |
| Sends headers, then nothing at all | `stream_idle_timeout_secs` | `model stream idle for >N s` |
| Sends only keepalives (SSE comments, pings), no output yet | `first_token_timeout_secs` | `no model output from <driver> <host> after N s (the server sent only keepalives)` |
| Produced output, then only keepalives | `no_progress_timeout_secs` | `no model progress from <driver> <host> for N s (the server sent only keepalives)` |

Only real output counts as progress: a text, reasoning or tool-call chunk.
Keepalives prove the connection is open, so they reset the idle clock, but
they never satisfy the first-token or no-progress limits. The first-token
limit is counted from the request and is never shorter than the first-byte
limit.

The first-byte and first-token defaults are larger for a server on your own
hardware (`brain`, `ollama`, `vllm`, `lmstudio`, `sven`), which may still be
prefilling a long prompt, and so is the stream-idle default (`1200` seconds
instead of `300`): a local server that sends neither data nor keepalives while
it prefills is cut off by the first-token limit before the idle one. A value
of `0` selects the default.

One-shot completions that are not part of a turn - the chat title and the
`context` tool's sub-queries - obey the same first-byte, first-token and idle
limits, so a quiet server cannot hang them either.

While no real output arrives, a `ModelWaiting` event is sent every 5 seconds
with the elapsed time and what the turn is waiting for (connecting, first
token, or a model gone quiet). The TUI shows it in the status bar, headless
runs write `[sven:info] waiting for model N s (<phase>)` to stderr, and an ACP
client receives an empty plan update as a heartbeat. Pressing `Esc` aborts a
waiting turn at once.

```yaml
agent:
  first_byte_timeout_secs: 240
  first_token_timeout_secs: 900
  no_progress_timeout_secs: 1200
```

---

### Approvals and questions

Whether a tool call waits for your approval is chosen per run, not in the
configuration: `sven --approval auto` (the default) runs every call the mode
allows without a prompt; `sven --approval manual` shows you each call that is
not read-only - with the tool and its command, path or arguments - before it
runs, sub-agents' calls included. Manual approval needs the interactive TUI;
sven refuses to start it headless, in a pipe, or as a team member. The mode
(`--mode`) still decides what may run at all.

A question the agent asks reaches you only in the interactive TUI. Everywhere
else it is answered at once: "No user is available to answer this question.
Proceed on your best judgement and state the assumption you made."

### `tools`

Controls what the agent is allowed to do and how.

| Key | Default | Description |
|-----|---------|-------------|
| `auto_approve_patterns` | `["cat *", "ls *", ...]` | Under `--approval manual`, a shell command whose text matches one of these runs without asking (sub-agents' and SDLC tasks' included). A command containing `;`, `\|`, `&`, `<`, `>`, a backtick, `$(` or a line break never matches. |
| `deny_patterns` | `["rm -rf /*", ...]` | A shell command whose text matches one of these is refused, in every approval mode - by the `shell` tool, `sven mcp serve`, sub-agents and the GDB tool's host commands (the shell side of `shell`, `!`, `pipe` and `\|`, and the server it starts) |
| `disabled` | `[]` | Tools, by name, a session never offers the model and never runs - built-in, MCP or application-supplied alike |
| `timeout_secs` | `30` | Per-tool-call timeout in seconds |
| `disclosure.mode` | `deferred` | How much of each tool's definition a request carries: `full` or `deferred` (below) |

Patterns are globs matched against the whole command text as written: `*`
matches anything, `?` one character. They are a convenience, not a security
boundary - a shell can say the same thing in many spellings (`rm -r -f /`,
a script, an alias) that no pattern foresees. Confine what the agent may do
with `--mode`, `tools.disabled` and a sandbox.

**Adding auto-approve patterns** (for `--approval manual`):

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

### `tools.disclosure`

| Key | Default | Description |
|-----|---------|-------------|
| `mode` | `deferred` | `full` sends the definition of every tool the mode offers with every request. `deferred` sends the definitions of the core tools (`read_file`, `write_file`, `edit_file`, `grep`, `find_file`, `shell`, `todo`) and of the tools the model has loaded, plus the `tools` tool; every other tool - including every MCP tool - is listed in the system prompt by a one-line summary until the model loads it. |

```yaml
tools:
  disclosure:
    mode: deferred
```

A program that reads the configuration file discloses tools on demand unless the
file says otherwise; a configuration an embedding application builds in code
(`Config::default()`) is `full`. `--tool-disclosure full|deferred` sets the mode for one process and overrides the
file. A `task` sub-agent is a separate process and reads the file. Under
`deferred` the model finds tools with `tools` (`search`, `list`, `help`) and
loads them with `tools` `load`; a loaded tool is called directly, under its own
approval rules. How it works, what it saves and what it costs:
[Deferred tool disclosure](../internals/tool-disclosure.md).

---

### `skills`, `subagents` and `knowledge`

Three sections of one shape, one per listing in the system prompt.

| Key | Default | Description |
|-----|---------|-------------|
| `disclosure` | `deferred` | `full` lists every entry's whole metadata in the system prompt. `deferred` lists a short line per entry and leaves the details to the tool that serves the catalog. |

```yaml
skills:
  disclosure: deferred
subagents:
  disclosure: deferred
knowledge:
  disclosure: deferred
```

| section | `deferred` lists | details through |
|---------|------------------|-----------------|
| `skills` | a command and a summary of at most 80 characters (package counts above 50 skills) | `skill` `load`, `list`, `search` |
| `subagents` | a name, `(readonly)` and a summary of at most 80 characters (a count above 50) | `task` `action: "agents"` |
| `knowledge` | a subsystem and the files it covers (a count above 50) | `search_knowledge`, `list_knowledge` |

A program that reads the configuration file defers the three listings unless
the file says otherwise; a configuration an embedding application builds in code
(`Config::default()`) is `full`. `--skill-disclosure`, `--subagent-disclosure`
and `--knowledge-disclosure` `full|deferred` set a mode for one process and
override the file. Under
`deferred` skills the model finds one the index does not make obvious with
`skill` `search` (ranked like tool search, and re-ranked by the decision model
under `decision.features.skill_selection`). A skill with `sven.always: true`
keeps its full entry; an instruction's `sven.activation` block decides when it
is listed at all. How it works and what it saves:
[Instruction disclosure](../internals/instruction-disclosure.md).

---

### `tools.web`

| Key | Default | Description |
|-----|---------|-------------|
| `fetch_max_chars` | `20000` | Maximum characters fetched from a URL |
| `search.api_key` | - | Brave Search API key (also `BRAVE_API_KEY` env var) |

---

### `tools.memory`

| Key | Default | Description |
|-----|---------|-------------|
| `memory_file` | `~/.config/sven/memory.json` | Where persistent memory is stored |

sven does not train models. A `learning:` section under `tools.memory` is
accepted and ignored, with one warning naming it at startup.

---

### `tools.asr`

Speech-to-text for audio the active model cannot take: `attach_file` and
`--attach` send it to a brain model over D-Bus and attach the transcript
instead.

| Key | Default | Description |
|-----|---------|-------------|
| `model` | `brain/nemotronasr` | Manifest id of the served ASR model, as brain's `ListModels` reports it |
| `bus_address` | session bus | D-Bus address of the brain server (`unix:path=/run/brain/bus`); a detached server has no session bus |
| `timeout_secs` | `120` | Hard timeout for one transcription |

Transcription exists only in a Unix build with the `coding` preset (the
default `sven` build). A build without it - `--no-default-features --features
minimal`, or an SDK application without the preset - refuses audio for a
model that cannot take it, naming the model, and its `attach_file` offers no
`force_transcribe`.

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
| `connect_timeout_secs` | `30` | Seconds allowed for the initial connect handshake, which includes loading symbols; keep it at least `command_timeout_secs` |
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

### `tools.context`

Controls the memory-mapped context tools used when analysing content that is
too large to fit in the model's context window.  See
[Working with Large Content](../guides/large-content.md) for a user guide.

| Key | Default | Description |
|-----|---------|-------------|
| `max_parallel` | `4` | Maximum concurrent sub-agent queries in a `context_query` call |
| `default_chunk_lines` | `500` | Lines per chunk when `chunk_lines` is not specified |
| `sub_query_max_chars` | `120000` | Character cap on the prompt sent to each sub-query (≈ 30 000 tokens) |
| `sub_query_timeout_secs` | `120` | Seconds before one sub-query is cancelled and recorded as an error for its chunk; `0` = no limit |

**Example:**

```yaml
tools:
  context:
    max_parallel: 8          # faster on high-core machines with generous rate limits
    default_chunk_lines: 300 # smaller chunks for dense files (e.g. generated C)
    sub_query_max_chars: 80000
```

---

### `decision`

A small decision model that advises the harness. It is off unless the file
turns it on, and while it is off nothing is contacted. The model only advises:
every answer passes a per-decision-point policy first, and an answer the policy
does not accept changes nothing. How it works, the policy table and the wire:
[Decision runtime](../internals/decisions.md).

| Key | Default | Description |
|-----|---------|-------------|
| `enabled` | `false` | Decisions are off, whatever `mode` says, unless this is `true`. |
| `mode` | `auto` | `auto` uses a decision model when one is usable and otherwise behaves exactly as without decisions. `required` refuses to start optimised execution without a suitable model, with the reason and the remedy. `off` never contacts anything. |
| `provider` | `brain` | Where decisions come from. Only `brain` exists. |
| `model` | `auto` | `auto` leaves the choice to the provider; otherwise a stable model id. |
| `timeout_ms` | `500` | The deadline of one decision, 1 to 600000. It covers finding the model, waiting for a free slot and the call, so a first call that has to load the model may exceed it and fall back. |
| `warn_if_unavailable` | `true` | Tell the user once per session that decisions are unavailable and why. |

#### `decision.brain`

| Key | Default | Description |
|-----|---------|-------------|
| `url` | `http://127.0.0.1:8788` | The server's base URL, with or without `/v1`. The default is where `brain serve --openai` listens. |
| `api_key` | none | Bearer token, when the server was started with a key. `${VAR}` is expanded when the file is loaded. |
| `max_concurrent_requests` | `2` | Most decisions in flight at once. |
| `cache_entries` | `256` | Most decisions the per-process cache holds. |

#### `decision.features`

| Key | Default | Acted on by |
|-----|---------|-------------|
| `tool_routing` | `true` | Re-ranks ambiguous `tools search` results. |
| `skill_selection` | `true` | Re-ranks ambiguous `skill search` results. |
| `delegation` | `true` | Says whether two near-identical read-only `task` goals are one ([delegation](../concepts/delegation.md#deduplication)). |
| `context_selection`, `cache_routing` | `true` | Nothing yet: the flag is read, no decision point asks. |
| `model_routing`, `permission_advisory` | `false` | Nothing yet. A permission decision is advisory-only and never grants. |

#### `decision.policy`

| Key | Default | Description |
|-----|---------|-------------|
| `mode` | `apply` | `apply` lets an accepted verdict change behaviour. `shadow` asks and records every decision in the trajectory but always behaves as without decisions. |
| `max_decision_input_tokens` | the served model's limit | Most tokens of state one decision sends. Unset, the limit the model reports in its manifest, or 256 when it reports none; set, the smaller of the two. |
| `minimum_model_quality` | `validated` | `validated` refuses a model with no recorded validation. `any` accepts any model that can answer. |

```yaml
decision:
  enabled: true
  mode: auto
  brain:
    url: http://127.0.0.1:8788
  policy:
    minimum_model_quality: validated
```

---

### `cost`

Whether a demotion of old tool results is weighed against the provider's prompt
cache. Off unless turned on; advisory unless `enforce`.

| key | default | meaning |
|-----|---------|---------|
| `cache_aware` | `off` | `off`, `advise` (record the comparison, demote anyway) or `enforce` (leave a demotion undone that does not pay) |
| `remaining_requests` | unset | requests still expected in a session; unset means about as many as it has made |
| `prices` | none | per million tokens by `provider/model`: `input_per_mtok`, `output_per_mtok`, optional `cache` with `read_per_mtok`, `write_per_mtok`, `ttl_secs` |

See [Cost model](../internals/cost-model.md#where-it-is-wired).

### `context.assembly`

How the context of a request is assembled from the conversation. With `off` the
conversation is sent exactly as it is. With `demote`, once the conversation
exceeds `budget_tokens`, the tool results of older rounds are sent in a smaller,
deterministic form that names the chunk it came from; the raw text is stored
outside the conversation and the model can ask for it with `context` and
`action=expand`. Failures and pinned chunks are never shortened below the
documented floors. How it works, what it saves and what it costs:
[Addressable context](../internals/context-assembly.md).

| Key | Default | Description |
|-----|---------|-------------|
| `mode` | `off` | `off`, `demote`, or `reconstruct`: `demote`, and a resumed session starts from a rebuilt history (the user's messages and a recap of the stored tool activity) instead of the whole transcript. |
| `budget_tokens` | `12000` | Estimated tokens of conversation above which results are demoted. |
| `keep_recent_rounds` | `2` | Rounds of tool calls, counted back from the newest, whose results are always sent in full. |
| `min_demote_tokens` | `64` | Results smaller than this are never demoted. |
| `resident_bytes` | `16777216` | Bytes of stored evidence held in memory; the rest is read back from disk. |

```yaml
context:
  assembly:
    mode: demote
    budget_tokens: 12000
    keep_recent_rounds: 2
```

The evidence is kept under `.sven/context/<id>/` in the project; the id is a
session fact, so a resumed session finds what its transcript refers to.
`--context-assembly off|demote|reconstruct` sets the mode for one process and overrides the file.

---

### `delegation`

Whether the `task` tool declines a sub-agent that would cost more than it
saves. A sub-agent pays for its own system prompt and tool definitions and for
the brief it reads, and hands a report back; work done inline stays in the
parent's context and is sent again with every later request. The estimate,
`expected_benefit > coordination_cost`, and what it does and does not measure:
[Delegation](../concepts/delegation.md#cost-policy). Every
count is estimated tokens.

| Key | Default | Description |
|-----|---------|-------------|
| `policy` | `off` | `off`: every call launches and nothing is estimated. `advise`: every call launches, and the result says when the estimate favoured doing the work inline. `enforce`: a call whose estimated cost exceeds its estimated benefit is declined with the reason and the instruction to do the work inline. |
| `child_overhead_tokens` | `4000` | What a sub-agent costs before it reads the task. Replaced by the measured average once `min_samples` sub-agents have finished and reported their usage. |
| `report_tokens` | `400` | What the parent receives back. Replaced by the measured average like the overhead. |
| `min_samples` | `2` | Finished sub-agents the measured averages need. |
| `unknown_work_tokens` | `6000` | The work of a subtask whose files are not all declared as literal paths (`task`'s `paths`): unknown, so assumed large enough to delegate. |
| `replay_rounds` | `3` | Later requests in which work done inline would be sent again. |
| `parallel_credit_tokens` | `1500` | What each further independent subgoal in flight is worth. |
| `write_surcharge_tokens` | `1000` | What a sub-agent that writes adds: review and conflict handling. |
| `context_window_tokens` | `128000` | The parent's context window, which its context size is compared with. |
| `assumed_parent_context_tokens` | `8000` | The parent's context size when the session records no chunks to measure it by. |

```yaml
delegation:
  policy: advise
  child_overhead_tokens: 4000
```

---

### `mcp_servers`

MCP (Model Context Protocol) servers extend sven with additional tools, prompts,
and resources. Each server is identified by its key in the map (e.g. `atlassian`).

| Key | Description |
|-----|-------------|
| `transport` | How to connect, selected by `type`: `stdio` (a subprocess; `command` and `args`) or `http` (Streamable HTTP; `url` and `headers`, e.g. a static bearer token) |
| `enabled` | Whether the server is active (default: `true`); a disabled server is not connected and its tools are not offered |
| `env` | Environment variables for stdio servers; values accept `${VAR}` and `${VAR:-default}` |
| `oauth` | OAuth 2.0 config for HTTP servers that require authentication |
| `timeout_secs` | Longest a single call to the server may take, in seconds (default: `30`, also for `0`). It bounds the whole call - connecting, the response and, for HTTP, the response body - so a tool that needs longer needs a larger value. A call that exceeds it fails with an error naming the method |

A server's tools are named `<server>-<tool>` (server `github`, tool
`list_repos`: `github-list_repos`).

**Stdio transport:**

```yaml
mcp_servers:
  github:
    transport:
      type: stdio
      command: npx
      args: ["-y", "@modelcontextprotocol/server-github"]
    env:
      GITHUB_TOKEN: ${GITHUB_TOKEN}
```

**HTTP transport:**

```yaml
mcp_servers:
  atlassian:
    transport:
      type: http
      url: "https://mcp.atlassian.com/v1/mcp"
    oauth: {}   # scopes auto-discovered from the server
    enabled: true
```

**OAuth options** (under `oauth:`):

| Key | Description |
|-----|-------------|
| `scopes` | OAuth scopes (leave empty for auto-discovery) |
| `client_id` | Pre-registered client ID (omit for default) |
| `client_secret` | Client secret for confidential clients |
| `redirect_uri` | Custom redirect URI (omit for default; see below) |
| `callback_port` | Port for local callback when using custom redirect (default: 5598) |

**Default OAuth redirect: `sven://sven.mcp/callback`**

When `redirect_uri` is not set, sven uses:

- **`sven://sven.mcp/callback`** - when running natively. The Debian package installs
  a protocol handler that forwards to the local callback server. Works out of the box.
- **`http://127.0.0.1:5598/callback`** - when running in a container (Docker, Podman).
  The `sven://` handler on the host cannot reach the container, so sven falls back to
  localhost. Ensure port 5598 is forwarded (e.g. `-p 5598:5598`).

**Using `cursor://cursor.mcp` for Atlassian MCP**

Atlassian MCP pre-allowlists `cursor://cursor.mcp`. To use it:

1. Set `redirect_uri: "cursor://cursor.mcp/callback"` in your oauth config.
2. Configure your OS to forward the `cursor://` protocol to Sven's local callback server.
3. Use the provided handler script or register a custom protocol handler.

Example config:

```yaml
mcp_servers:
  atlassian:
    transport:
      type: http
      url: "https://mcp.atlassian.com/v1/mcp"
    oauth:
      redirect_uri: "cursor://cursor.mcp/callback"
      callback_port: 5598
```

The protocol handler must forward `cursor://cursor.mcp/callback?code=...&state=...`
to `http://127.0.0.1:5598/callback?code=...&state=...`. A helper script is at
`scripts/oauth-callback-handler.sh`. See [MCP OAuth troubleshooting](../guides/troubleshooting.md#mcp-oauth-authentication) for setup instructions per platform.

**Credentials storage**

OAuth tokens are stored in `~/.config/sven/mcp-credentials.json` and refreshed
automatically before expiry. The same credentials are reused across projects
that use the same server URL.

---

### `tui`

| Key | Default | Description |
|-----|---------|-------------|
| `theme` | `"dark"` | Colour theme: `"dark"`, `"light"`, or `"solarized"` |
| `code_line_numbers` | `false` | Show line numbers in code blocks |
| `wrap_width` | `0` | Markdown wrap column (0 = auto) |
| `ascii_borders` | `false` | Use ASCII instead of Unicode box-drawing characters |
| `show_thinking` | `false` | Show the model's thinking in the chat; when off it is kept in the session and the status bar says "Thinking" while it streams |

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
```

**Use Anthropic Claude - all cache layers are on by default:**

```yaml
model:
  provider: anthropic
  name: claude-sonnet-4-5
  # Nothing extra needed: sven enables comprehensive caching out of the box.
  # Add extended_cache_time: true for CI or any workflow with gaps > 5 minutes:
  extended_cache_time: true    # 1-hour TTL for system/tools/images/tool-results
```

**Disable all caching (e.g. for cost-sensitive one-shot runs):**

```yaml
model:
  provider: anthropic
  name: claude-sonnet-4-5
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

**Under manual approval, auto-approve all read and test commands:**

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

## Environment variables

| Variable | Effect |
|----------|--------|
| `XDG_RUNTIME_DIR` | Where a running brain server's connection record is looked for (`$XDG_RUNTIME_DIR/brain`, else `<tmpdir>/brain-<uid>`) |
| `SVEN_MODEL` | Default for `--model` |
| `SVEN_MOCK_RESPONSES` | Responses file for the mock provider |
| `BRAVE_API_KEY` | Key of the `web_search` tool when `tools.web.search.api_key` is unset |
| `AWS_DEFAULT_REGION`, `AWS_REGION`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` | Credentials and region of the Bedrock provider |
| `SVEN_ASCII_BORDERS=1` | Same as `tui.ascii_borders: true` |
| `SVEN_LOG_FILE`, `RUST_LOG` | Logging; see the [command-line reference](cli.md#environment) |
| `SVEN_NO_PATH_ASCENT` | Any value: `read_file` reads exactly the path asked for instead of resolving a near miss |

Provider keys are not read from the environment: store them with
`sven auth set <provider>` or name a file with `api_key_file`. Any string in a
configuration file may still read the environment with `${VAR}`; see
[How the file is read](#how-the-file-is-read).
