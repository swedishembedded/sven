# CI / Headless Mode

Sven runs without a TUI whenever it detects a non-interactive context:

| Trigger | Example |
|---------|---------|
| `--headless` flag | `sven --headless "Fix the bug"` |
| `--file` flag | `sven --file workflow.md` |
| Piped stdin | `echo "analyse the codebase" \| sven` |

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
| `compact` | Plain text responses only (legacy behaviour) |
| `json` | Structured JSON with step metadata |

```bash
# JSON output – useful for CI dashboards
sven --file workflow.md --output-format json | jq '.steps[].success'
```

---

## Workflow Files

Workflow files are plain markdown.  Each `##` heading starts a new step.

```markdown
# My Workflow

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

Override settings for individual steps using HTML comments:

```markdown
## Deep research
<!-- step: mode=research timeout=600 -->
Read and summarise every file in the codebase.

## Implement changes
<!-- step: mode=agent timeout=300 -->
Apply the changes identified in the research phase.
```

Supported per-step options:

| Option | Values | Description |
|--------|--------|-------------|
| `mode` | `research`, `plan`, `agent` | Agent mode for this step only |
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
commit, and whether the working tree is clean — without you having to tell it.

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
```

These flags work alongside config-file `agent.system_prompt` and take
precedence over it.

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

Or set defaults in `~/.config/sven/config.toml`:

```toml
[agent]
max_step_timeout_secs = 300
max_run_timeout_secs = 1800
```

---

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Success – all steps completed |
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

## Chaining Workflows

Because headless output is valid conversation markdown, you can chain
multiple workflows:

```bash
# Research → Plan → Implement pipeline
sven --file research.md \
  | sven --file plan.md --output-format compact \
  | sven --file implement.md
```

Each stage receives the full conversation from the previous stage on stdin
(or via a file), giving it complete context.

---

## Configuration Reference

Config file path: `~/.config/sven/config.toml`

```toml
[agent]
default_mode          = "agent"
max_tool_rounds       = 50
max_step_timeout_secs = 0      # 0 = no limit
max_run_timeout_secs  = 0      # 0 = no limit

[model]
provider = "anthropic"
name     = "claude-opus-4-5"
```

CLI flags always take precedence over config file and frontmatter.
Frontmatter takes precedence over config file.

### Full CLI reference for headless mode

| Flag | Default | Description |
|------|---------|-------------|
| `--file FILE` | — | Input workflow or conversation file |
| `--mode MODE` | `agent` | Default agent mode (`research`/`plan`/`agent`) |
| `--model MODEL` | config | Model override (e.g. `anthropic/claude-opus-4-5`) |
| `--output-format FMT` | `conversation` | `conversation`, `compact`, or `json` |
| `--output-last-message PATH` | — | Write final agent response to a file |
| `--artifacts-dir DIR` | — | Save per-step artifacts to directory |
| `--var KEY=VALUE` | — | Template variable (repeatable) |
| `--step-timeout SECS` | 0 (none) | Per-step wall-clock timeout |
| `--run-timeout SECS` | 0 (none) | Total run wall-clock timeout |
| `--system-prompt-file PATH` | — | Replace default system prompt from file |
| `--append-system-prompt TEXT` | — | Append text to default system prompt |
| `--dry-run` | off | Validate workflow then exit without calling model |
| `--headless` | auto | Force headless mode (normally auto-detected) |
