# Troubleshooting

## Start here: can sven use the model?

```sh
sven doctor provider            # add --json for a machine-readable report
sven -M brain/qwen3 doctor provider
```

It builds the provider the way a session would and reports each step as `ok`,
`warn` or `FAIL`, with what to change when it is not `ok`:

| Check | What it tells you |
|---|---|
| `provider` | the provider and model sven resolved, or the missing key |
| `endpoint` | the endpoint answered with its model list and accepted the key |
| `model` | the model you named is one the server serves, and what it serves instead |
| `tools` | the server advertises tool calling for it (a `warn` if it says it does not) |

A hosted provider is asked for its model list only, so the check costs nothing
on the account and sends no prompt. Google, AWS, Azure, Cohere and D-Bus stop
after the provider check. The exit status is non-zero when a
check fails, so it can gate a CI job.

## API key issues

### sven exits immediately with "no API key"

sven does not read keys from the environment. Store the provider's key once:

```sh
sven auth set openai     # the key is read from stdin, without echo
sven auth list           # the providers with a stored key
```

A stored key is used for that provider's own endpoint only. An endpoint you
name with `base_url` (a proxy, a gateway, a self-hosted server) takes its own
key, from a file:

```yaml
model:
  provider: openai
  base_url: https://gateway.example/v1
  api_key_file: ~/.config/keys/gateway
```

A key file that does not exist stops sven with a message naming it rather
than sending a request with no key.

### "No running brain server was found"

The `brain` provider found no connection record of a running server. Start
one with `brain serve`; sven reads its address and key from the record it
publishes in `$XDG_RUNTIME_DIR/brain` while it runs.

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
- The default mode is plain ratatui (no Neovim embed).  If you launched with
  `--nvim`, try omitting that flag to rule out Neovim as the source of the issue.

---

## Neovim integration issues

### "nvim not found" or "failed to start embedded Neovim"

The embedded Neovim requires `nvim` to be available on your `PATH`.

```sh
which nvim     # should print a path
nvim --version # should print the Neovim version
```

If Neovim is not installed, either install it or simply omit `--nvim` (the
default ratatui mode does not require Neovim):

```sh
sven          # runs in ratatui mode by default
sven --nvim   # opt-in Neovim embed
```

### `:q` does not quit sven

Make sure the Neovim buffer has focus (chat pane), not the input box. Switch
focus with `Ctrl+W K`, then type `:q`.

---

## Tool approval and execution

### A command runs without asking me first

That is the default: `--approval auto` runs every call the mode allows. Start
sven with `--approval manual` to be asked about each call that is not
read-only. Under manual approval a shell command matching
`tools.auto_approve_patterns` still runs without asking; remove the patterns
you want to be prompted for:

```yaml
tools:
  auto_approve_patterns:
    - "cat *"
    - "ls *"
    # Remove "rg *" if you want to confirm grep-style searches
```

### A command I want to run is being blocked

Check the `deny_patterns` list. A shell command matching a deny pattern is
refused in every approval mode ("refused: the command matches
tools.deny_patterns"). Remove the relevant pattern:

```yaml
tools:
  deny_patterns:
    - "rm -rf /*"
    # Remove any pattern that is blocking commands you want to allow
```

### The agent keeps running commands I did not expect

Use `--mode research` for tasks that do not require changes: it has no `shell`
tool and the kernel refuses every file write. `--mode plan` also refuses the file
tools (`write_file`, `edit_file`), but keeps `shell`, so a command can still change
files. To be asked about every write, shell command, network call and git
operation, start with `--approval manual`. See
[Agents and modes](../concepts/agents.md).

---

## Context and compaction

### sven seems to "forget" earlier parts of the conversation

When the context window fills up, sven asks the model to write notes on the
whole conversation and replaces the history with them (the task and the todo
list are kept verbatim). The raw text is no longer directly visible to the
model, but the notes carry the decisions, files, values and next steps. This is
expected behaviour. At the start of a session sven prints a `context budget:`
line saying at what size it compacts.

To compact later, or to allow longer notes, adjust the settings:

```yaml
agent:
  compaction_threshold: 0.90        # compact when 90% of the input budget is used
  compaction_output_reserve: 0.15   # keep less of the window free for the response
  compaction_summary_tokens: 16000  # allow longer notes (default: 10000)
```

If compaction triggers very frequently due to large tool outputs, reduce the
tool result cap so individual outputs don't fill the context:

```yaml
agent:
  tool_result_token_cap: 2000   # trim tool outputs to ~2000 tokens each
```

To see how full the context is at any time, check the status bar's `ctx` gauge.
When emergency compaction fires (the model could not write notes), the TUI
shows `⚠ Context emergency-compacted`, the CI log shows
`[sven:compact] strategy=emergency` and a warning says how many tokens were
dropped without a summary.

### The agent stops calling tools and wraps up early

A turn is limited to `agent.max_tool_rounds` autonomous tool-call rounds (200 by
default). When the budget runs out, sven asks the model for a final answer
instead of letting it continue. Increase the limit if your tasks legitimately
require more steps:

```yaml
agent:
  max_tool_rounds: 400
```

---

## Headless mode and CI

### sven opens the TUI instead of running headlessly

sven runs headless when any of these holds: a PROMPT is given, stdin is not a
terminal, stdout is not a terminal, or `--headless` is passed. Without any of
them, in a terminal, it opens the TUI. The case that surprises people is
`--file`, which on its own queues the workflow's steps in the TUI:

```sh
sven "my task"                   # headless - a prompt was given
echo "my task" | sven            # headless - stdin is a pipe
sven --headless --file plan.md   # headless - explicit flag needed here
sven --file plan.md              # opens the TUI with the steps queued
```

To also suppress conversation-format markdown and get only the agent's plain
response text, add `--output-format compact`:

```sh
sven --output-format compact "my task" 2>/dev/null
```

### Output contains ANSI colour codes in a CI log

sven writes clean text without colour codes in headless mode. If you are
seeing colour codes, check whether another tool in the pipeline is adding them.

### The pipeline exits non-zero even though the task completed

Check standard error for error messages, and match the exit code against this
table:

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | The agent run failed |
| `2` | Invalid workflow, arguments or input |
| `3` | The run completed, but at least one tool call returned an error |
| `4` | The `--max-tokens` budget was exhausted |
| `5` | A question is parked waiting for a human answer (see `sven questions`) |
| `124` | A step or run timeout expired |
| `130` | Interrupted (Ctrl+C) |

Add `--verbose` to get more detail:

```sh
sven --file plan.md --verbose 2>&1 | less
```

---

## Mock provider

### The mock model replies "[no mock rule matched]"

The responses file had no rule that matches the last user message. Rules are
tried in order; add a catch-all `default` rule at the end:

```yaml
responses:
  - match_type: default
    reply: "No mock rule matched this input."
```

`match_type` is one of `contains`, `equals`, `starts_with`, `regex` or `default`;
all but `default` also need a `pattern`.

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

Without a responses file the mock provider replies `MOCK: ` followed by your last message.

---

## Headless / CI mode issues

### Agent uses wrong paths or "directory does not exist"

Sven detects the project root by walking up to the nearest `.git` directory
(falling back to the current directory) and injects it into the system prompt.
If you see path-related failures:

1. Make sure you are running sven from within the project (or a subdirectory).
2. Check that a `.git` directory exists at or above your working directory.
3. Confirm the injected root with verbose output:

```bash
sven --file workflow.md -v 2>&1 | grep "Project Context"
```

### Workflow hangs and never finishes

A headless run ends after `agent.max_run_timeout_secs` (four hours by default)
even if nothing else bounds it. Lower the limits to fail sooner:

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

To keep a headless run's conversation for later, give it a trace file with
`--trace session.json` (loads it if present, writes it back after the run), or
resume a session saved by the TUI:

```bash
sven chats          # list saved sessions
sven --resume <id>  # resume in the TUI, or headless with an explicit id
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

The run was stopped by Ctrl+C.

---

## Conversation history

### sven chats returns "No saved sessions found"

`sven chats` lists the sessions the TUI saves, and prints the directory it looked
in (`~/.local/share/sven/sessions/` on Linux). Headless runs do not write there.
A headless run is recorded only when you ask for it with `--trace` or
`--output-trace`, or when a `.sven/` project directory exists, in which case each
run also writes a trajectory to `.sven/logs/<timestamp>.atif.json`.

### I lost a conversation

The TUI saves a session after each turn, so a crash loses at most the turn in
progress. Saved sessions are ATIF trajectory files (JSON):

```sh
ls -t ~/.local/share/sven/sessions/ | head -5
```

Open one with `sven --resume <id>` or `/resume`. To capture the complete
trajectory of a headless run for debugging, use `--output-trace` (writes a single
ATIF trajectory document):

```sh
sven --file workflow.md --output-trace trace.json
jq . trace.json | less
```

---

## MCP OAuth authentication

### OAuth browser opens repeatedly (perpetual loop)

If the OAuth flow completes successfully (you see "Authentication successful" in
the browser) but the browser keeps opening again, the server is rejecting your
credentials. Sven stops after the first failure instead of retrying
indefinitely.

**Atlassian MCP:** Custom clients must be allowlisted by an Atlassian admin.
Even with valid OAuth tokens, `mcp.atlassian.com` returns 401 for clients whose
domain is not in the allowlist. See [Available Atlassian Rovo MCP server
domains](https://support.atlassian.com/security-and-access-policies/docs/available-atlassian-rovo-mcp-server-domains/)
for how to add your client.

**Using `cursor://cursor.mcp`:** This protocol is pre-allowlisted. Configure your
config with `redirect_uri: "cursor://cursor.mcp/callback"` and set up a
protocol handler to forward the callback to Sven (see [Configuration](../reference/configuration.md#mcp_servers)).

### "Server rejected credentials after authentication"

This means you had valid OAuth tokens but the MCP server returned 401. For
Atlassian MCP, ensure your client's domain is allowlisted. For other servers,
check that the token has the correct scopes and audience.

### Setting up the cursor:// protocol handler

When using `redirect_uri: "cursor://cursor.mcp/callback"`, you must register a
handler so the OAuth redirect reaches Sven's local callback server.

**Linux (xdg-open):**

1. Create `~/.local/share/applications/sven-oauth-cursor.desktop`:

```ini
[Desktop Entry]
Type=Application
Name=Sven OAuth Callback
Exec=/path/to/sven/scripts/oauth-callback-handler.sh %u
MimeType=x-scheme-handler/cursor;
```

2. Register it:

```sh
xdg-mime default sven-oauth-cursor.desktop x-scheme-handler/cursor
```

**macOS:** Use Automator or a small app to handle `cursor://` URLs and run the
handler script with the URL as an argument.

**Windows:** Add a registry entry so `cursor://` URLs invoke the handler script
with the URL as `%1`.

The handler script (`scripts/oauth-callback-handler.sh`) forwards the callback
to `http://127.0.0.1:5598/callback` by default. Set `SVEN_OAUTH_CALLBACK_PORT`
if you use a different `callback_port` in your config.

### Running Sven in a container (Docker, Podman)

When Sven runs inside a container, the `sven://` protocol handler (installed on
the host) cannot reach the callback server inside the container. Sven detects
this and automatically falls back to `http://127.0.0.1:5598/callback`, binding
to `0.0.0.0:5598` so forwarded traffic is accepted.

**Required:** Forward port 5598 when running the container:

```sh
docker run -p 5598:5598 ... sven ...
```

The OAuth browser opens on the host; the redirect goes to `http://127.0.0.1:5598`,
which Docker forwards to the container. No `redirect_uri` config is needed.

### OAuth fails and browser does not open again

When OAuth fails (user cancels, error, etc.), Sven sets the server status to
Failed and does not retry automatically. This prevents an annoying loop of
browser windows. Run `/mcp auth <server>` manually to try again.

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
