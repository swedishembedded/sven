# Workflow files

A workflow file is a markdown document whose `##` sections are sent to the
agent one after another, in one conversation. Run it with
`sven --headless --file workflow.md` (see [Headless and CI](headless.md)); in a
terminal, `sven --file workflow.md` queues the steps in the TUI instead.

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
| `##` H2 heading | Starts a new step; the heading is its label, the section is the user message |
| `<!-- sven: key=value ... -->` | Per-step options; stripped from the body |

A file with no `##` heading is one unlabelled step holding the whole text. A
step with a heading and no body is sent as its heading. `#` and `###` or deeper
headings inside a step stay in its text. Other HTML comments are dropped. A
positional prompt on the command line runs first, as an extra step before the
file's steps.

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

Add optional metadata between `---` delimiters at the very top of the file:

```markdown
---
title: Code Refactoring Workflow
models:
  agent: anthropic/claude-sonnet-4-5
  research: anthropic/claude-opus-4-5
vars:
  branch: main
  ticket: PROJ-123
---

## Analyse {{branch}} branch
Find all TODO comments related to {{ticket}}.

## Fix the issues
Resolve each TODO found in the previous step.
```

Frontmatter is a small YAML subset, not full YAML: top-level `key: value`
lines, and two sections (`vars:` and `models:`) whose entries are indented
`key: value` lines. Values may be wrapped in single or double quotes. The
supported fields are:

| Field | Type | Description |
|-------|------|-------------|
| `title` | string | Conversation title; overrides the H1 and names the saved trajectory |
| `models` | map | Model per agent mode (`agent`, `research`, `plan`), see below |
| `vars` | map | Template variables (`{{key}}` substitution) |

Any other key is ignored without a warning. The mode, the model and the
timeouts are not frontmatter fields: set them with `--mode`, `--model`,
`--step-timeout` and `--run-timeout`, in the configuration file, or per step
with a directive.

Each step runs on the first of these that applies: the step's own `model` (or
`provider` and `model`) directive; the `models` entry for the step's `mode`
directive, else the `models` entry `agent`; the model from `--model` or the
configuration. A `models` entry therefore takes effect for every step of its
mode, including when `--model` is given.

### Per-step directives

Put `<!-- sven: ... -->` anywhere inside a step to override settings for that
step:

```markdown
## Deep research
<!-- sven: mode=research timeout=600 -->
Read and summarise every file in the codebase.

## Implement changes
<!-- sven: mode=agent timeout=300 -->
Apply the changes identified in the research phase.
```

The `sven:` prefix may be left out when every word of the comment is a
`key=value` pair and at least one key is known: `<!-- mode=plan -->`.

| Option | Values | Description |
|--------|--------|-------------|
| `mode` | `research`, `plan`, `agent` | Agent mode, from this step on. Another value is ignored with a warning |
| `model` | `anthropic/claude-sonnet-4-5`, or a bare name with `provider` | Model, from this step on |
| `provider` | e.g. `anthropic` | Provider for `model=`; use with a bare model name |
| `timeout` | seconds | Time limit of this step only; overrides `--step-timeout`; `0` removes the step limit |
| `cache_key` | string | Reuse the step's stored answer, see [Caching a step](#caching-a-step) |

`mode` and `model` are not reset after the step: later steps keep them until a
step sets another. A model that cannot be built is reported and the current one
stays in use.

### Template Variables

`{{key}}` in a step's text is replaced before the step is sent. The text of the
title and the preamble is not substituted. Keys are matched exactly:
`{{ key }}` with spaces is left as written.

Sources, from lowest to highest priority:

1. **CI variables**, set when sven runs under a recognised CI provider:
   `branch`, `commit`, `repo` and `pr` (pull or merge request number), and
   `CI_<name>` for each provider variable that is set, with the name in
   lowercase (`{{CI_github_sha}}`, `{{CI_ci_commit_ref_name}}`, ...). The
   covered variables are those of GitHub Actions, GitLab CI, CircleCI, Travis
   CI, Azure Pipelines and Bitbucket Pipelines, plus `GIT_COMMIT`, `GIT_BRANCH`,
   `BUILD_NUMBER` and `BRANCH_NAME`.
2. **Built-ins**: `PROJECT_ROOT` (the nearest ancestor with `.git`, else the
   working directory) and `WORKSPACE_ROOT`.
3. **Frontmatter `vars`.**
4. **`--var KEY=VALUE`** on the command line. A value may itself contain `=`.

A `{{NAME}}` that is still unresolved is looked up in the process environment
(only when `NAME` is letters, digits and underscores). One that is still
unresolved after that is sent unchanged.

After each step finishes, its answer is available to the steps after it as
`{{step.<label>.output}}`, where `<label>` is the heading in lowercase with each
run of other characters replaced by one `_` (`Gather Information` becomes
`{{step.gather_information.output}}`), and as `{{step.<N>.output}}` by step
number from 1.

```bash
# CLI variables take precedence over frontmatter
sven --headless --file deploy.md --var env=staging --var version=1.2.3
```

### Caching a step

A step with `cache_key=NAME` is answered from `.sven/cache/NAME.md` under the
project root when that file exists, without calling the model (stderr shows
`[sven:cache:hit]`). Otherwise it runs, and a successful non-empty answer is
written to that file (`[sven:cache:write]`). Characters of `NAME` other than
letters, digits, `-` and `_` become `_`. A cached step uses no tools and nothing
invalidates the file: change the key or delete it to run the step again.

## Validation and dry-run

Check a workflow file without running it:

```bash
# Print what the file contains and exit
sven validate --file workflow.md

# Dry-run: the same parse, reported on stderr, in the context of a run
sven --headless --file workflow.md --dry-run
```

Neither calls the model. `sven validate` fails (exit `1`) only when the file
cannot be read: the parser accepts any text, so a typo in a directive or an
unknown frontmatter key is not an error. Check the printed modes and models
instead. It prints the title (frontmatter first, then H1), the frontmatter,
the preamble size and every step, with the text of templates unexpanded:

```
Title: Code Refactoring Workflow
Frontmatter: OK
  models (2):
    agent: anthropic/claude-sonnet-4-5
    research: anthropic/claude-opus-4-5
  vars (2):
    branch = main
    ticket = PROJ-123
Steps: 2
  Step 1/2: "Analyse {{branch}} branch"  mode=(inherit)  provider=(inherit)  model=(inherit)  timeout=(inherit)
    Find all TODO comments related to {{ticket}}.
  Step 2/2: "Fix the issues"  mode=agent  provider=(inherit)  model=(inherit)  timeout=300s
    Resolve each TODO found in the previous step.

Workflow is valid.
```

`--dry-run` prints `[sven:dry-run] Workflow validated - 2 step(s)`, the title
and one `[sven:dry-run] Step i/N: label=... mode=... provider=... model=...
timeout=...` line per step on stderr, then exits `0`.

---

## Conversation Mode

A file or stdin that holds `## User` / `## Sven` sections is recognised as a
conversation; no flag is needed. Continue one like this:

```bash
# Run a workflow and save output as a conversation
sven --headless --file workflow.md > my-conversation.md

# Add a follow-up and run it
printf '\n## User\nExplain step 2 in more detail.\n' >> my-conversation.md
sven --headless --file my-conversation.md
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
| Plain text / no `##` headings | Plain text | Entire input becomes a single message |
| Contains `## User`, `## Sven`, `## Tool`, or `## Tool Result` | Conversation markdown | History seeded; step = CLI prompt or pending user turn |
| Every non-empty line starts with `{` | JSONL steps | History seeded from full-fidelity JSONL |
| One JSON object with a `steps` array | JSON trajectory | History seeded from the steps |

JSONL and trajectory input are recognised by the workflow runner; see
[Pipe composition](pipe-composition.md#which-runner-reads-which-format).

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
sven --headless --file plan-and-relay.md | sven
```

See **[Pipe Composition](pipe-composition.md)** for
the full reference including all patterns, error cases, and implementation
details.
