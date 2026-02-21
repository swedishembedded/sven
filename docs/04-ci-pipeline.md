# CI and Pipelines

sven's headless mode turns the agent into a composable command-line tool. Input
comes from a file or standard input; output is plain text on standard output;
errors go to standard error. A non-zero exit code means the task failed.

This makes sven a natural fit for CI pipelines, Makefile automation, and
multi-step shell scripts.

---

## When headless mode activates

sven enters headless mode automatically when any of the following is true:

- Standard input is not a TTY (e.g. you piped something into it)
- `--headless` is passed on the command line
- `--file` is passed

In headless mode there is no TUI. Progress information goes to standard error
and the final answer goes to standard output.

---

## Single-step usage

The simplest form: pipe one task in, get the answer out.

```sh
echo "Summarise the last three commits." | sven
```

```sh
sven --headless "List all TODO comments in the src/ directory."
```

The output is plain text and can be captured or piped freely:

```sh
echo "List all public functions in lib.rs." | sven > api-list.txt
```

---

## Multi-step workflows with markdown files

For tasks that have more than one step, write them as a markdown file where
each `##` section is a separate step.

```markdown
## User
Summarise the project structure in two sentences.

## User
List all files modified in the last seven days.

## User
Based on what you found, identify the three areas most likely to contain bugs.
```

Run it with `--file`:

```sh
sven --file analysis.md
```

sven executes each `## User` section in order. The output of one step is
available to the next because the conversation history is carried forward. If
any step fails, sven exits with a non-zero code and subsequent steps are
skipped.

---

## Conversation file format

A conversation file is a superset of the multi-step format. It records both
your messages and sven's responses, so you can read the full history and
continue where you left off.

| Section heading | Role |
|----------------|------|
| `## User` | Your message or task |
| `## Sven` | Agent's text response |
| `## Tool` | A tool call (JSON) |
| `## Tool Result` | Output of that tool call |

An optional `# Title` line at the very top is treated as the conversation title.

**Example file:**

```markdown
# Database refactor

## User
List all the database queries in the codebase.

## Sven
I found 14 queries across 3 files ...

## Tool
{"tool_call_id":"c1","name":"glob_file_search","args":{"pattern":"**/*.rs"}}

## Tool Result
src/db/queries.rs
src/api/handlers.rs
...

## User
Which of those queries are not using parameterised inputs?
```

**Execution rule:** sven runs the file if, and only if, the last section is a
`## User` section with no following `## Sven` section. This lets you append
new instructions and run the file again without re-executing the history.

```sh
# Execute the pending ## User section and append the result
sven --file work.md --conversation

# Append a follow-up
printf '\n## User\nFix the unsafe queries you found.\n' >> work.md

# Run again — only the new ## User section is executed
sven --file work.md --conversation
```

---

## Chaining agents with pipes

Because output is plain text, you can pipe the output of one sven invocation
directly into another.

```sh
# Stage 1: research and plan (no writes)
echo "Design a test strategy for the payment module." \
  | sven --mode plan \
  | sven --mode agent --headless
```

In this example the first agent produces a plan and writes it to standard
output. The second agent receives that plan as its input and implements it.

```sh
# Multi-provider chain: use GPT-4o for planning, Claude for implementation
echo "Propose an API for a rate limiter." \
  | sven --mode plan --model gpt-4o \
  | sven --mode agent --model anthropic/claude-opus-4-5
```

---

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | All steps completed successfully |
| `1` | One or more steps failed (error details on stderr) |

Use this in shell scripts with `set -e` or explicit checks:

```sh
set -e

sven --file plan.md

echo "All steps passed."
```

---

## CI integration

### GitHub Actions

```yaml
- name: Run sven analysis
  env:
    OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
  run: |
    echo "Summarise any security issues introduced in this PR." | sven --headless
```

```yaml
- name: Run multi-step validation
  env:
    OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
  run: sven --file .github/workflows/validate.md --mode research
```

### GitLab CI

```yaml
sven-review:
  stage: test
  image: debian:bookworm-slim
  before_script:
    - apt-get update && apt-get install -y sven
  script:
    - sven --file ci/review.md --mode research
  variables:
    OPENAI_API_KEY: $OPENAI_API_KEY
```

### Makefile

```makefile
review:
    echo "Review the last commit and flag any obvious bugs." | sven --mode research

plan-release:
    sven --file scripts/release-plan.md --mode plan > target/release-plan.txt
```

---

## Setting the model for a CI run

Override the model for any single run without changing the config file:

```sh
# Use a specific model name
sven --model gpt-4o-mini "List the test files."

# Switch provider
sven --model anthropic/claude-3-5-haiku-latest "List the test files."
```

The `--model` flag also accepts just the provider name to use that provider's
default model:

```sh
sven --model anthropic "Explain this module."
```

---

## Setting the mode for a CI run

```sh
sven --mode research --file audit.md
sven --mode plan     --file feature-plan.md
sven --mode agent    --file implement.md
```

---

## Testing without an API key

Use the built-in mock provider for testing CI jobs offline or in environments
where a real API key is not available:

```sh
export SVEN_MOCK_RESPONSES=/path/to/mock-responses.yaml
sven --model mock --headless "ping"
```

See [Examples](06-examples.md) and [Troubleshooting](07-troubleshooting.md) for
more on mock responses.
