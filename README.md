# sven

A keyboard-driven AI coding agent for the terminal. Built in Rust, sven works in
two modes that share the same agent core:

- **Interactive TUI** — a full-screen terminal interface with a scrollable markdown
  chat log, vim-style navigation, and live-streamed responses.
- **Headless / CI** — reads instructions from stdin or a markdown file, writes
  clean text to stdout, and exits with a meaningful code. Designed to compose
  with other tools via pipes.

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
mode. Input is parsed as markdown: each `##` section becomes a separate step
that is queued and sent to the model only after the previous step finishes.
This lets a single markdown file describe a multi-step workflow.

Output is plain text on stdout. Errors go to stderr. A failed step exits
non-zero, so the pipeline aborts naturally under `set -e`.

```sh
# Simple pipe
echo "Summarise the project" | sven

# Multi-step file
sven --file plan.md

# Chained agents
echo "Design a REST API" | sven --mode plan | sven --mode agent
```

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

sven looks for a TOML config file at (first match wins):

1. Path given with `--config`
2. `$SVEN_CONFIG`
3. `$XDG_CONFIG_HOME/sven/config.toml`
4. `~/.config/sven/config.toml`

Run `sven show-config` to see the full resolved configuration with all defaults
filled in. The schema is defined in `crates/sven-config/src/schema.rs`.

Key sections:

- `[model]` — provider, model name, API key env var, base URL override for
  local proxies (e.g. LiteLLM), token limits.
- `[agent]` — default mode, maximum autonomous tool rounds, compaction
  threshold, optional system prompt override.
- `[tools]` — tool timeout, Docker sandbox toggle, auto-approve and deny glob
  patterns.
- `[tui]` — theme, markdown wrap width, ASCII-border fallback for terminals
  with limited font support.

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
