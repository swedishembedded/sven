# Command-line reference

## Configuration Reference

Config file path: `~/.config/sven/config.yaml`

```yaml
agent:
  default_mode: agent
  max_tool_rounds: 200
  max_step_timeout_secs: 0    # 0 = no limit
  max_run_timeout_secs: 14400 # 0 = no limit

model:
  provider: anthropic
  name: claude-opus-4-5
```

CLI flags always take precedence over config file and frontmatter.
Frontmatter takes precedence over config file.

### Full CLI reference for headless mode

| Flag | Default | Description |
|------|---------|-------------|
| `--file FILE` | - | Input workflow or conversation file |
| `--mode MODE` | `agent` | Default agent mode (`research`/`plan`/`agent`) |
| `--model MODEL` | config | Model override (e.g. `anthropic/claude-opus-4-5`) |
| `--output-format FMT` | `conversation` | `conversation`, `compact`, or `json` |
| `--output-last-message PATH` | - | Write final agent response to a file |
| `--output-trace PATH` | - | Write the complete ATIF trajectory document |
| `--load-trace PATH` | - | Load history from a saved ATIF trajectory |
| `--trace PATH` | - | Combined `--load-trace PATH --output-trace PATH` |
| `--artifacts-dir DIR` | - | Save per-step artifacts to directory |
| `--var KEY=VALUE` | - | Template variable (repeatable) |
| `--step-timeout SECS` | `agent.max_step_timeout_secs` (none) | Per-step wall-clock timeout; `0` = no limit |
| `--run-timeout SECS` | `agent.max_run_timeout_secs` (4 hours) | Total run wall-clock timeout; `0` = no limit |
| `--system-prompt-file PATH` | - | Replace default system prompt from file |
| `--append-system-prompt TEXT` | - | Append text to default system prompt |
| `--no-system` | off | Suppress Sven's built-in system prompt; zero system tokens unless `--system-prompt-file`/`--append-system-prompt` still supply text |
| `--no-tools` | off | Send zero tool schemas; refuse any tool call attempted anyway |
| `--bare` | off | Shorthand for `--no-system --no-tools` |
| `--tool-disclosure MODE` | `tools.disclosure.mode` (`deferred`) | `full` sends every tool's definition with every request; `deferred` sends the core tools' and the ones the model loaded |
| `--dry-run` | off | Validate workflow then exit without calling model |
| `--headless` | auto | Force headless mode (normally auto-detected) |
