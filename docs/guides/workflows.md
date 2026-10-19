# Workflow files

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
| `run_timeout_secs` | integer | Total run timeout (0 = no limit; default `agent.max_run_timeout_secs`) |
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

A file or stdin that holds `## User` / `## Sven` sections is recognised as a
conversation; no flag is needed. Continue one like this:

```bash
# Run a workflow and save output as a conversation
sven --file workflow.md > my-conversation.md

# Add a follow-up and run it
printf '\n## User\nExplain step 2 in more detail.\n' >> my-conversation.md
sven --file my-conversation.md
```

sven:
1. Parses all previous `## User` / `## Sven` exchanges as history
2. Executes the trailing `## User` section (if any pending)
3. Writes the new exchange to stdout; the file is not modified

For a session that continues across runs without moving text between files,
use `--trace session.json` (see [Trace Output](headless.md#atif-trace-output)).

## Pipe Composition

Sven's headless output is designed to pipe cleanly into other tools or into
another sven instance.  Diagnostics go to stderr so stdout stays uncontaminated.

### Input format detection

When sven reads stdin (no positional prompt, or `--stdin` alongside one), it auto-detects the format:

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
`sven 'task1' | sven --stdin 'task2'` works as expected.

### Common pipe patterns

```bash
# Data transform: stdin is data, CLI arg is the operation (most idiomatic)
git diff HEAD~1 | sven --stdin 'write a commit message for these changes'
cat report.md   | sven --stdin 'summarise the key findings'

# Context seed: first conversation becomes history for the second agent
sven 'list all public APIs' | sven --stdin 'write tests for each API listed above'

# Compact relay: response text becomes next user message
sven 'find null-pointer bugs' --output-format compact \
  | sven --stdin 'fix each of the following bugs'

# Multi-stage pipeline with full-fidelity ATIF trace handoff
sven 'stage 1' --output-trace /tmp/run.json
sven 'stage 2' --load-trace  /tmp/run.json

# Pending-user relay: last ## User in output drives the next agent
sven --file plan-and-relay.md | sven
```

See **[Pipe Composition](pipe-composition.md)** for
the full reference including all patterns, error cases, and implementation
details.
