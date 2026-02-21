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

```toml
[model]
api_key_env = "MY_CUSTOM_KEY_VAR"
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

```toml
[tui]
ascii_borders = true
```

### The TUI is garbled or not rendering at all

- Check that your terminal reports a colour depth of at least 256: `echo $TERM`
  should show something like `xterm-256color`, `screen-256color`, or `tmux-256color`.
- If inside a multiplexer (tmux, screen), ensure it is configured for 256 colours.
- Try `sven --no-nvim` to disable the Neovim embed, which rules out Neovim as
  the source of the issue.

### Colours look wrong or washed out

Set the theme to match your terminal background:

```toml
[tui]
theme = "light"    # or "dark", "solarized"
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

```toml
[tools]
auto_approve_patterns = [
    "cat *",
    "ls *",
    # Remove "rg *" if you want to confirm grep-style searches
]
```

### A command I want to run is being blocked

Check the `deny_patterns` list. If the command matches a deny rule it will
always be blocked. Remove the relevant pattern:

```toml
[tools]
deny_patterns = [
    "rm -rf /*",
    # Remove any pattern that is blocking commands you want to allow
]
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

```toml
[agent]
compaction_threshold = 0.95    # compact only when 95% full
```

To see how full the context is at any time, check the status bar: `ctx:X%`.

### "max tool rounds reached"

sven stopped because it hit the configured limit on autonomous tool calls per
turn. Increase the limit if your tasks legitimately require more steps:

```toml
[agent]
max_tool_rounds = 100
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

```toml
[model]
provider = "mock"
mock_responses_file = "/path/to/responses.yaml"
```

---

## Conversation history

### sven chats returns "No saved conversations found"

Conversations are only saved when you run the interactive TUI. Headless runs do
not write history files. The history directory is:

```sh
ls ~/.config/sven/history/
```

### I lost a conversation

Conversations are written to disk on exit. If sven crashed before writing, the
session may be incomplete. The raw files are JSONL format and can be opened in
a text editor:

```sh
ls -t ~/.config/sven/history/ | head -5
cat ~/.config/sven/history/<id>.jsonl | python3 -m json.tool | less
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
