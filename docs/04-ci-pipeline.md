# CI / Headless Mode

Sven runs without a TUI whenever it detects a non-interactive context:

| Trigger | Example |
|---------|---------|
| `--headless` flag | `sven --headless "Fix the bug"` |
| `--file` flag | `sven --file workflow.md` |
| Piped stdin | `echo "analyse the codebase" \| sven` |

> **`--headless` is required** when running sven from a terminal prompt without
> piping stdin.  Without it, sven opens the interactive TUI instead of writing
> to stdout.  When stdin is already a pipe (e.g. in a CI script or shell
> pipeline), headless mode is detected automatically and the flag is optional.

---

## Quick Start

```bash
# Single-step task
echo "List all TODO comments in src/" | sven --model mock

# Multi-step workflow file
sven --file .sven/workflow/refactor.md

# Pipe output into a second instance for follow-up
sven --file step1.md | sven --file step2.md
```

---

## Output Format

By default, headless runs write **conversation-format markdown** to stdout.
This is the same format used by `--conversation` history files and is always
pipeable back into another sven instance.

```markdown
## User

Analyse the project structure and list the top-level modules.

## Sven

The project contains the following top-level modules: ...

## Tool

```json
{"name": "list_dir", "args": {"path": "."}}
```

## Tool Result

```
src/
crates/
tests/
...
```

## Sven

Here is a summary of the project structure: ...
```

Diagnostics (tool calls, step progress, errors) go to **stderr** so the
stdout pipeline stays clean.

### `--output-format`

| Value | Description |
|-------|-------------|
| `conversation` (default) | Full `## User` / `## Sven` / `## Tool` markdown |
| `compact` | Plain text responses only - no headings, no markup |
| `json` | Structured JSON with step metadata |

Use `--output-format compact` whenever the caller only needs the agent's answer
text and nothing else - for example when the response is a shell command to be
executed, a value to be parsed, or input for another tool.

```bash
# JSON output - useful for CI dashboards
sven --file workflow.md --output-format json | jq '.steps[].success'

# Compact output - only the agent's response, no markdown formatting
sven --headless --output-format compact "List the top 5 TODO files" 2>/dev/null

# Generate a shell command and execute it directly
sven --headless --output-format compact \
     "Write a one-liner that prints CPU usage. Reply with the command only." \
     2>/dev/null | sh
```

---

## Workflow Files

**Workflow structure (H1, preamble, `##` steps) applies only when you pass a
file with `-f`/`--file`.**  When input comes from stdin (e.g. a pipe), it is
never parsed as a workflow; it is treated as conversation/JSONL or as a single
plain-text user message.

Workflow files are plain markdown with a defined structure:

| Element | Meaning |
|---------|---------|
| `#` H1 heading | Conversation title (not sent to the model) |
| Text between H1 and first `##` | Appended to the agent system prompt |
| `##` H2 heading | Starts a new step (user message) |
| `<!-- sven: key=value ... -->` | Per-step options; stripped from body |

```markdown
# Codebase Improvement

Review and improve the codebase in three passes.

## Analyse codebase
Read the top-level directory and summarise what each folder contains.

## Propose improvements
Based on the analysis, suggest three specific improvements.

## Implement the first improvement
Implement only the first improvement from your proposal.
```

### YAML Frontmatter

Add optional metadata between `---` delimiters at the top of the file:

```markdown
---
title: Code Refactoring Workflow
mode: agent
model: anthropic/claude-opus-4-5
step_timeout_secs: 300
run_timeout_secs: 1800
vars:
  branch: main
  ticket: PROJ-123
---

## Analyse {{branch}} branch
Find all TODO comments related to {{ticket}}.

## Fix the issues
Resolve each TODO found in the previous step.
```

Supported frontmatter fields:

| Field | Type | Description |
|-------|------|-------------|
| `title` | string | Conversation title (used in history and artifacts) |
| `mode` | string | Default agent mode for all steps |
| `model` | string | Model override (e.g. `anthropic/claude-opus-4-5`) |
| `step_timeout_secs` | integer | Per-step timeout (0 = no limit) |
| `run_timeout_secs` | integer | Total run timeout (0 = no limit) |
| `vars` | map | Template variables (`{{key}}` substitution) |

### Per-Step Configuration

Use `<!-- sven: ... -->` directives immediately after the `##` heading to
override settings for that step:

```markdown
## Deep research
<!-- sven: mode=research timeout=600 -->
Read and summarise every file in the codebase.

## Implement changes
<!-- sven: mode=agent timeout=300 -->
Apply the changes identified in the research phase.
```

Supported directive options:

| Option | Values | Description |
|--------|--------|-------------|
| `mode` | `research`, `plan`, `agent` | Agent mode for this step only |
| `model` | e.g. `anthropic/claude-opus-4-5` | Model override for this step |
| `timeout` | integer (seconds) | Step-level timeout override |
| `cache_key` | string | Cache key for step result reuse (future) |

### Template Variables

Variables from frontmatter `vars`, CLI `--var`, or environment are
substituted as `{{key}}` in step content.

```bash
# CLI variables take precedence over frontmatter
sven --file deploy.md --var env=staging --var version=1.2.3
```

CLI format: `--var KEY=VALUE`

---

## Project Context

In headless mode, sven automatically walks up the directory tree to find the
nearest `.git` directory.  It then injects the absolute path into the agent's
system prompt as the **Project Context**:

```
## Project Context
Project root directory: `/home/user/my-project`
- Use this absolute path for all file operations.
- Pass this path as the `workdir` argument to `run_terminal_command`
  so shell commands execute in the correct directory.
- Prefer absolute paths over relative paths in every tool call.
```

This eliminates the common class of bugs where the agent uses relative
paths that resolve against the current working directory rather than the
project root.

### Git context

In addition to the project root path, sven collects live git metadata and
injects it into the system prompt:

```
## Git Context
Branch: feat/headless-improvements
Commit: d3adb33
Remote: git@github.com:acme/myproject.git
Uncommitted changes: 3 file(s)
```

The agent therefore always knows which branch it is working on, the current
commit, and whether the working tree is clean - without you having to tell it.

### Project context file

sven automatically reads a project-level instructions file and injects it as
a **Project Instructions** section in the system prompt.  Files are tried in
this order:

| Path | Purpose |
|------|---------|
| `.sven/context.md` | sven-specific project instructions |
| `AGENTS.md` | Standard agent instructions (compatible with OpenAI Codex) |
| `CLAUDE.md` | Claude Code project file (compatible with Anthropic Claude Code) |

Example `.sven/context.md`:

```markdown
# Project conventions

- All Rust code must pass `cargo clippy -- -D warnings`.
- Write tests for every public function.
- Keep commits atomic; one logical change per commit.
- The project root is a Cargo workspace; always run cargo from the workspace root.
```

This is injected verbatim into every sven run against that project, so you
never have to repeat project conventions in individual workflow files.

---

## System Prompt Customisation

Override or extend the default system prompt on the command line:

```bash
# Replace the default system prompt entirely
sven --file workflow.md --system-prompt-file .sven/custom-prompt.md

# Append extra rules after the default Guidelines section
sven --file workflow.md \
  --append-system-prompt "Always create a branch before making changes."

# Both at once: load file and append extra text
sven --file workflow.md \
  --system-prompt-file .sven/base-prompt.md \
  --append-system-prompt "Extra rule for this run only."

# No system prompt at all: zero tokens spent before the first message
sven --file workflow.md --no-system

# --no-system combined with --system-prompt-file: send exactly the file's
# content as the system message, with none of Sven's built-in identity,
# guidelines, or project/git/CI context mixed in
sven --file workflow.md \
  --no-system \
  --system-prompt-file .sven/exact-prompt.md
```

These flags work alongside config-file `agent.system_prompt` and take
precedence over it. `--no-system` suppresses Sven's own identity/guidelines/
context sections; if `--system-prompt-file`/`--append-system-prompt` (or
config-file `agent.system_prompt`) still supply text, that text is sent
verbatim as the entire system message. With none of those, `--no-system`
means no system message is sent at all.

## Tool Suppression

Disable all tools for a session with `--no-tools`: no tool schemas are sent
to the model (it has no way to know any tool exists), and any tool call it
attempts anyway is refused rather than executed. Useful for models with a
small context window where the tool schemas alone would exceed the budget,
or for a pure text-in/text-out run where tool use makes no sense.

```bash
# No tools at all - just the conversation
sven --no-tools "explain what this error message means: <paste>"

# The absolute minimum request: no system prompt, no tools
sven --bare "hi"
```

`--bare` is shorthand for `--no-system --no-tools` together - the smallest
possible request, useful for fitting very small local/test models.

---

## Capturing Output

### Save the last agent response

Write only the final agent reply to a file (without conversation formatting):

```bash
sven --file review.md --output-last-message review-summary.txt
cat review-summary.txt
```

This is equivalent to `--output-format compact` but saves to a file without
polluting stdout, so you can still capture the full conversation on stdout:

```bash
# Full conversation on stdout AND last message saved to file
sven --file review.md --output-last-message summary.txt > full-review.md
```

### ATIF Trace Output

Save the complete conversation trace as a standards-compliant [ATIF](https://github.com/harbor-framework/harbor/blob/main/rfcs/0001-trajectory-format.md)
trajectory document — steps, reasoning, tool calls, observations, and token
metrics, directly consumable by Harbor-style debugging, visualization, SFT,
and RL pipelines:

```bash
# Save the complete trajectory
sven --file workflow.md --output-trace trace.json

# Works with any run mode
sven --headless "analyze the code" --output-trace analysis-trace.json
sven --file conversation.md --conversation --output-trace continued-trace.json
```

The trace file is a single pretty-printed JSON document (an ATIF `Trajectory`
object), not line-delimited — see
**[docs/technical/pipe-composition.md](technical/pipe-composition.md)** for
the full field reference.

Every headless run also writes an automatic trajectory to `.sven/logs/` when a
`.sven/` project directory exists, whether or not `--output-trace` was given —
a flat directory of `*.json` documents an RL or SFT pipeline can ingest
directly.

### Outcome rewards

A **concluded** trajectory carries the outcome of its run as a training weight:

```json
"final_metrics": {
  "extra": {
    "reward": 0.9,
    "outcome": "success_with_tool_errors",
    "tool_calls": 10,
    "tool_errors": 2
  }
}
```

`reward` is a float in `[0.0, 1.0]`; `0.0` means "do not learn from this run".
The other keys are diagnostics for reading a training set by eye — only
`reward` is meant to be consumed programmatically.

| `outcome` | `reward` | when |
|-----------|----------|------|
| `success` | `1.0` | the run finished with no failing tool call |
| `success_with_tool_errors` | `0.5`–`1.0` | finished, but some tool calls errored; scaled by the error ratio |
| `agent_error` | `0.0` | the agent reported a fatal error |
| `cancelled` | `0.0` | interrupted (Ctrl+C) or aborted |
| `timeout` | `0.0` | a step or run deadline elapsed |
| `budget_exhausted` | `0.0` | `--max-tokens` was reached mid-run |

**An absent `reward` means the outcome is unknown, not that it was zero.** A
consumer must skip such a trajectory rather than defaulting it. This is what
makes the mid-run flushes safe: a still-running session is written unstamped,
so only concluded runs are ever trained on.

### Redirect by format

```bash
# Just the answers (compact)
sven --file plan.md --output-format compact > answer.txt

# Machine-readable JSON for dashboards
sven --file plan.md --output-format json | jq '.steps[].agent_response'

# Full replayable conversation
sven --file plan.md > conversation.md
sven --file conversation.md --conversation  # continue where you left off
```

---

## Timeouts

Configure timeouts at multiple levels (CLI > frontmatter > config file):

```bash
# Per-step: abort any step that runs longer than 5 minutes
sven --file workflow.md --step-timeout 300

# Total run: abort if the entire workflow takes longer than 30 minutes
sven --file workflow.md --run-timeout 1800

# Both together
sven --file workflow.md --step-timeout 300 --run-timeout 1800
```

Or set defaults in `~/.config/sven/config.yaml`:

```yaml
agent:
  max_step_timeout_secs: 300
  max_run_timeout_secs: 1800
```

---

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Success - all steps completed |
| `1` | Agent error (tool failure, API error, etc.) |
| `2` | Validation error (bad workflow file, config error) |
| `124` | Timeout exceeded (step or total run) |
| `130` | Interrupted (Ctrl+C) |

---

## Artifacts

Save per-step and full-conversation outputs to a directory:

```bash
sven --file workflow.md --artifacts-dir .sven/artifacts/run-$(date +%s)
```

Directory layout:

```
.sven/artifacts/run-1234567890/
├── conversation.md          # Full conversation output
├── 01-Analyse_codebase.md   # Per-step conversation turn
├── 02-Propose_improvements.md
└── ...
```

---

## Progress Reporting

Sven writes structured progress lines to **stderr** using `[sven:...]`
prefixes that are easy to scrape with grep or awk:

```
[sven:step:start] 1/3 label="Analyse codebase"
[sven:tool:call] name="list_dir" args={"path":"."}
[sven:tool:ok] name="list_dir"
[sven:step:complete] 1/3 label="Analyse codebase" duration_ms=4321 tools=2 success=true
[sven:step:start] 2/3 label="Propose improvements"
...
```

Filter progress from a CI log:

```bash
sven --file workflow.md 2>&1 >/dev/null | grep '^\[sven:step:complete\]'
```

---

## Validation and Dry-Run

Check a workflow file for syntax errors without running it:

```bash
# Full validation report
sven validate --file workflow.md

# Dry-run: show what would execute and exit
sven --file workflow.md --dry-run
```

Example output:

```
Frontmatter: OK
  title: My Workflow
  mode: agent
  step_timeout_secs: 300
Steps: 3
  Step 1/3: "Analyse codebase"  mode=(inherit)  timeout=300s
    Read and summarise the project structure...
  Step 2/3: "Propose improvements"  mode=(inherit)  timeout=(inherit)
    Based on the analysis, suggest improvements...
  Step 3/3: "Implement"  mode=agent  timeout=300s
    Implement the first improvement...

Workflow is valid.
```

---

## Conversation Mode

Resume and continue a conversation interactively:

```bash
# Run a workflow and save output as a conversation
sven --file workflow.md > my-conversation.md

# Load it as a conversation and ask a follow-up
echo "## User\n\nExplain step 2 in more detail." >> my-conversation.md
sven --file my-conversation.md --conversation
```

With `--conversation`, sven:
1. Parses all previous `## User` / `## Sven` exchanges as history
2. Executes the trailing `## User` section (if any pending)
3. Appends the new response to the same file

---

## CI/CD Integration

### GitHub Actions

Use the provided action in `.github/actions/sven/`:

```yaml
jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Run code review workflow
        uses: ./.github/actions/sven
        with:
          workflow-file: .sven/workflow/code-review.md
          model: anthropic/claude-opus-4-5
          step-timeout: 300
          run-timeout: 1800
          artifacts-dir: .sven/artifacts
          vars: |
            pr_number=${{ github.event.number }}
            branch=${{ github.head_ref }}
        env:
          ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
```

### GitLab CI

```yaml
ai-review:
  stage: review
  script:
    - sven --file .sven/workflow/review.md
        --step-timeout 300
        --run-timeout 1800
        --output-format json
        --artifacts-dir $CI_PROJECT_DIR/.sven/artifacts
        --var branch=$CI_COMMIT_REF_NAME
        --var mr_number=$CI_MERGE_REQUEST_IID
  artifacts:
    paths:
      - .sven/artifacts/
    expire_in: 7 days
```

### Shell Script

```bash
#!/usr/bin/env bash
set -euo pipefail

OUTPUT=$(sven --file .sven/workflow/audit.md \
              --output-format compact \
              --step-timeout 120 \
              2>/dev/null)

echo "Audit result:"
echo "$OUTPUT"
```

---

## Pipe Composition

Sven's headless output is designed to pipe cleanly into other tools or into
another sven instance.  Diagnostics go to stderr so stdout stays uncontaminated.

### Input format detection

When stdin is not a terminal, sven auto-detects the format:

| stdin content | Detected as | Effect |
|---------------|-------------|--------|
| Plain text / no `##` headings | Workflow / plain text | Entire input becomes a single step |
| Contains `## User`, `## Sven`, `## Tool`, or `## Tool Result` | Conversation markdown | History seeded; step = CLI prompt or pending user turn |
| Every non-empty line starts with `{` | JSONL conversation | History seeded from full-fidelity JSONL |

### Step content when a conversation is piped

When conversation markdown or JSONL is piped in, the step content for the
new turn resolves in this order:

```
CLI positional prompt  →  piped pending user turn  →  error (exit 2)
```

This means `sven 'task1' | sven` exits with a clear error message, while
`sven 'task1' | sven 'task2'` works as expected.

### Common pipe patterns

```bash
# Data transform: stdin is data, CLI arg is the operation (most idiomatic)
git diff HEAD~1 | sven 'write a commit message for these changes'
cat report.md   | sven 'summarise the key findings'

# Context seed: first conversation becomes history for the second agent
sven 'list all public APIs' | sven 'write tests for each API listed above'

# Compact relay: response text becomes next user message
sven 'find null-pointer bugs' --output-format compact \
  | sven 'fix each of the following bugs'

# Multi-stage pipeline with full-fidelity ATIF trace handoff
sven 'stage 1' --output-trace /tmp/run.json
sven 'stage 2' --load-trace  /tmp/run.json

# Pending-user relay: last ## User in output drives the next agent
sven --file plan-and-relay.md | sven
```

See **[docs/technical/pipe-composition.md](technical/pipe-composition.md)** for
the full reference including all patterns, error cases, and implementation
details.

---

## Configuration Reference

Config file path: `~/.config/sven/config.yaml`

```yaml
agent:
  default_mode: agent
  max_tool_rounds: 200
  max_step_timeout_secs: 0    # 0 = no limit
  max_run_timeout_secs: 0     # 0 = no limit

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
| `--step-timeout SECS` | 0 (none) | Per-step wall-clock timeout |
| `--run-timeout SECS` | 0 (none) | Total run wall-clock timeout |
| `--system-prompt-file PATH` | - | Replace default system prompt from file |
| `--append-system-prompt TEXT` | - | Append text to default system prompt |
| `--no-system` | off | Suppress Sven's built-in system prompt; zero system tokens unless `--system-prompt-file`/`--append-system-prompt` still supply text |
| `--no-tools` | off | Send zero tool schemas; refuse any tool call attempted anyway |
| `--bare` | off | Shorthand for `--no-system --no-tools` |
| `--dry-run` | off | Validate workflow then exit without calling model |
| `--headless` | auto | Force headless mode (normally auto-detected) |
