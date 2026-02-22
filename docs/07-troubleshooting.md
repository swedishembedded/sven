# Troubleshooting

## API key issues

### sven exits immediately with "no API key"

sven reads the API key from an environment variable. Make sure it is exported
before running sven.

```sh
# Check the variable is set
echo $OPENAI_API_KEY    # should print your key, not blank

# If it is blank, export it
export OPENAI_API_KEY="sk-..."

# For a permanent fix, add the line to your shell profile:
echo 'export OPENAI_API_KEY="sk-..."' >> ~/.bashrc
source ~/.bashrc
```

The default variable name is `OPENAI_API_KEY` for the OpenAI provider and
`ANTHROPIC_API_KEY` for Anthropic. You can change which variable is read in the
config file:

```yaml
model:
  api_key_env: MY_CUSTOM_KEY_VAR
```

### "401 Unauthorized" or "invalid API key"

- Double-check the key has not expired or been revoked in your provider's
  dashboard.
- Make sure there are no leading or trailing spaces in the key value.
- If you are using a proxy with `base_url`, confirm the proxy itself is healthy
  and accepting your key.

---

## TUI rendering problems

### Boxes show as `+--+` instead of rounded corners, or garbled characters appear

Your terminal font does not fully support Unicode box-drawing characters. Enable
the ASCII fallback:

```sh
# Set it for a single run
SVEN_ASCII_BORDERS=1 sven

# Or add to your config file permanently
```

```yaml
tui:
  ascii_borders: true
```

### The TUI is garbled or not rendering at all

- Check that your terminal reports a colour depth of at least 256: `echo $TERM`
  should show something like `xterm-256color`, `screen-256color`, or `tmux-256color`.
- If inside a multiplexer (tmux, screen), ensure it is configured for 256 colours.
- Try `sven --no-nvim` to disable the Neovim embed, which rules out Neovim as
  the source of the issue.

### Colours look wrong or washed out

Set the theme to match your terminal background:

```yaml
tui:
  theme: light    # or "dark", "solarized"
```

---

## Neovim integration issues

### "nvim not found" or "failed to start embedded Neovim"

The embedded Neovim requires `nvim` to be available on your `PATH`.

```sh
which nvim     # should print a path
nvim --version # should show Neovim 0.9 or later
```

If Neovim is not installed, either install it or disable the embed:

```sh
sven --no-nvim
```

To always use the plain ratatui view, set an alias:

```sh
alias sven='sven --no-nvim'
```

### `:q` does not quit sven

Make sure the Neovim buffer has focus (chat pane), not the input box. Switch
focus with `Ctrl+W K`, then type `:q`.

---

## Tool approval and execution

### A command runs without asking me first

Check the `auto_approve_patterns` list in your config. If the command matches a
pattern there, it is approved automatically. Adjust the list to remove patterns
you want to be prompted for:

```yaml
tools:
  auto_approve_patterns:
    - "cat *"
    - "ls *"
    # Remove "rg *" if you want to confirm grep-style searches
```

### A command I want to run is being blocked

Check the `deny_patterns` list. If the command matches a deny rule it will
always be blocked. Remove the relevant pattern:

```yaml
tools:
  deny_patterns:
    - "rm -rf /*"
    # Remove any pattern that is blocking commands you want to allow
```

### The agent keeps running commands I did not expect

Consider running in `research` or `plan` mode for tasks that do not require
writes. This restricts the agent to read-only tools by design.

---

## Context and compaction

### sven seems to "forget" earlier parts of the conversation

When the context window fills up, sven compacts older messages into a summary.
The raw text is no longer visible to the model, but the summary captures the
key points. This is expected behaviour.

To preserve more history before compaction triggers, increase the threshold:

```yaml
agent:
  compaction_threshold: 0.95    # compact only when 95% full
```

To see how full the context is at any time, check the status bar: `ctx:X%`.

### "max tool rounds reached"

sven stopped because it hit the configured limit on autonomous tool calls per
turn. Increase the limit if your tasks legitimately require more steps:

```yaml
agent:
  max_tool_rounds: 100
```

---

## Headless mode and CI

### sven opens the TUI instead of running headlessly

Headless mode requires that standard input is not a TTY. Make sure you are
piping input in:

```sh
echo "my task" | sven          # headless
sven "my task"                  # opens TUI (stdin is a TTY)
sven --headless "my task"       # headless (explicit flag)
```

### Output contains ANSI colour codes in a CI log

sven writes clean text without colour codes in headless mode. If you are
seeing colour codes, check whether another tool in the pipeline is adding them.

### The pipeline exits non-zero even though the task completed

Check standard error for error messages. A non-zero exit means at least one
step failed. Add `--verbose` to get more detail:

```sh
sven --file plan.md --verbose 2>&1 | less
```

---

## Mock provider

### "no mock responses matched"

sven fell through all your rules without finding a match. Add a catch-all rule
at the end of your YAML file:

```yaml
rules:
  - match: ".*"
    match_type: regex
    reply: "No mock rule matched this input."
```

### Mock responses are not being used

Make sure the path is correct and the environment variable is exported:

```sh
export SVEN_MOCK_RESPONSES="/absolute/path/to/responses.yaml"
echo "ping" | sven --model mock --headless
```

Or specify the file in the config:

```yaml
model:
  provider: mock
  mock_responses_file: /path/to/responses.yaml
```

---

## Headless / CI mode issues

### Agent uses wrong paths or "directory does not exist"

Sven automatically detects the project root by walking up to the nearest `.git`
directory and injects it into the system prompt.  If you see path-related
failures:

1. Make sure you are running sven from within the project (or a subdirectory).
2. Check that a `.git` directory exists at or above your working directory.
3. Confirm the injected root with verbose output:

```bash
sven --file workflow.md -v 2>&1 | grep "Project Context"
```

### Workflow hangs and never finishes

Add timeouts to prevent indefinite runs:

```bash
sven --file workflow.md --step-timeout 300 --run-timeout 1800
```

Or set defaults in `~/.config/sven/config.yaml`:

```yaml
agent:
  max_step_timeout_secs: 300
  max_run_timeout_secs: 1800
```

Sven exits with code `124` when a timeout is exceeded.

### Output is not valid conversation markdown

By default, headless runs produce conversation-format markdown.  If you
see raw text without `## User` / `## Sven` headers, check that you are not
using `--output-format compact`.

If a step fails mid-way, the partial conversation is saved to history and
you can inspect it:

```bash
sven chats          # list recent conversations
sven --resume <id>  # resume in the TUI or headless mode
```

### Validate a workflow file before running

```bash
sven validate --file workflow.md
```

This parses the frontmatter, counts steps, and checks inline step options
without calling the model.

### Dry-run to preview what will execute

```bash
sven --file workflow.md --dry-run
```

Prints each step label, mode, and timeout without running anything.

### Exit code 130 (interrupted)

The run was stopped by Ctrl+C.  Any partial conversation has been saved to
history and can be resumed.

---

## Conversation history

### sven chats returns "No saved conversations found"

Conversations are only saved when you run the interactive TUI. Headless runs do
not write history files. The history directory is:

```sh
ls ~/.local/share/sven/history/
```

### I lost a conversation

Conversations are written to disk on exit. If sven crashed before writing, the
session may be incomplete. Conversation files are stored as markdown:

```sh
ls -t ~/.local/share/sven/history/ | head -5
cat ~/.local/share/sven/history/<timestamp>_<slug>.md
```

To capture the raw API trace for debugging or fine-tuning, use `--jsonl-output`:

```sh
sven --file workflow.md --jsonl-output trace.jsonl
cat trace.jsonl | python3 -m json.tool | less
```

---

## Debugging and logs

### Enable verbose logging

Pass `-v` for debug output or `-vv` for trace output. Logs go to standard error
so they do not mix with pipeline output:

```sh
sven -v "explain this error"
sven -vv --file plan.md 2>debug.log
```

### View the resolved configuration

```sh
sven show-config
```

This prints every setting including defaults, which helps confirm that your
config file is being read and that the values are what you expect.
