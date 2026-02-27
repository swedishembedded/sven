# Pipe Composition

Sven follows the Unix philosophy: **pipes carry data, CLI arguments specify
operations**.  Every headless run writes structured text to stdout and
diagnostics to stderr, which keeps the stdout pipeline clean for downstream
tools or a second sven instance.

---

## Input format detection

When stdin is not a terminal, sven reads it entirely and applies the following
detection rules in priority order:

| Priority | Detection criterion | Interpretation |
|----------|---------------------|----------------|
| 1 | Every non-empty line starts with `{` | **JSONL conversation** — produced by `--output-jsonl` |
| 2 | Any line is exactly `## User`, `## Sven`, `## Tool`, or `## Tool Result` | **Conversation markdown** — produced by `--output-format conversation` (default) |
| 3 | Everything else | **Workflow / plain text** — H2 sections become steps; no `##` sections → single step with the full text as body |

The rules are mutually exclusive and checked top-down.

---

## Output formats and what they produce downstream

| `--output-format` | stdout content | Piped into sven | Typical use |
|-------------------|---------------|-----------------|-------------|
| `conversation` (default) | Full `## User` / `## Sven` / `## Tool` markdown | Detected as *conversation*, history seeded | Archiving, context passing, continuation |
| `compact` | Agent response text only | Detected as *plain text*, becomes user message | Relay / transform chains |
| `json` | Structured JSON object | Detected as *plain text* (starts with `{`; each line varies) | CI dashboards, parsing with `jq` |

> **Note on JSON output and JSONL detection**: `--output-format json` emits a
> single multi-line JSON object, not one-object-per-line JSONL.  The first line
> starts with `{` but subsequent lines do not, so the JSONL heuristic returns
> false and the output is treated as plain text — which is correct.

---

## Step content resolution for piped input

When conversation markdown or JSONL is piped in, sven bypasses the workflow
parser entirely (to avoid misreading `## Sven` as a step label).  The step
content for the new turn is resolved in this order:

```
CLI positional prompt   →   piped pending user turn   →   hard error
```

1. **CLI positional prompt** (`sven 'task'`): the explicit task to run against the seeded history.
2. **Piped pending user turn**: a trailing `## User` section in conversation markdown, or a trailing user message in a JSONL file, that has not yet received an agent response.  When present, it is used as the step content automatically.
3. **Neither present**: sven exits with code `2` and prints a diagnostic message explaining how to fix it.

This resolves the `sven | sven` bug where the second instance previously
sent an empty message to the model.

---

## Pipe patterns

### Pattern 1 — Data transform (most idiomatic)

```bash
cat report.md | sven 'summarise the key findings'
find . -name '*.rs' | sven 'count lines in each file and sort by size'
git diff HEAD~1 | sven 'write a commit message for these changes'
```

The piped content is plain text (no `## User`/`## Sven` markers) so it is
treated as a single workflow step.  The CLI argument is the task; stdin is
the data.  This mirrors `grep`, `sed`, and `awk`: CLI arguments specify the
operation, the pipe carries the data.

### Pattern 2 — Context seed with explicit task

```bash
sven 'analyse the codebase and list all public APIs' \
  | sven 'write integration tests for each API listed above'
```

The first sven's conversation markdown is piped into the second.  The second
sven detects it as conversation format, seeds the first exchange into its
history, and runs `'write integration tests...'` as a fresh user turn with
that context available.

```
stdin (conversation markdown)
        │
        ▼
  parse_conversation()
        │
        ├─── .history ──────► agent.seed_history()
        │
        └─── .pending_user_input (None here)
                                │
CLI prompt "write tests..."─────► step content
```

### Pattern 3 — Relay via pending user turn

A conversation file or output can end with an unanswered `## User` section.
When such output is piped to a second sven with no CLI prompt, the pending
user turn is automatically used as the step content:

```bash
# First sven produces a plan ending with:
#   ## User
#   Now implement step 1 of the plan.
sven --file plan-and-relay.md | sven
```

Workflow file `plan-and-relay.md`:
```markdown
## Create a plan
Analyse the codebase and write a three-step improvement plan.
Output only the plan, then append exactly this line:
## User
Now implement step 1 of the plan.
```

This pattern lets one agent drive another without repeating the handoff
instruction on the CLI.

### Pattern 4 — Compact relay

`--output-format compact` emits only the agent's response text.  Because
there are no `## User`/`## Sven` markers, the receiving instance treats it
as a plain-text user message:

```bash
sven 'find all null-pointer dereferences in src/' --output-format compact \
  | sven 'fix each of the following bugs'
```

The second sven receives the bug list as its user message and runs from a
clean context (no seeded history).  This is the correct pattern when you
want the second agent to act on the *result* of the first, not have access
to how the first agent arrived at it.

### Pattern 5 — Full-fidelity JSONL chaining

For long pipelines where you want every agent in the chain to have access
to the complete history including tool calls and thinking blocks:

```bash
sven 'task1' --output-jsonl /tmp/run.jsonl
sven 'task2' --load-jsonl /tmp/run.jsonl
```

Or in a two-stage pipeline using a temporary file:

```bash
TMP=$(mktemp /tmp/sven-XXXX.jsonl)
sven 'stage 1' --output-jsonl "$TMP"
sven 'stage 2' --load-jsonl "$TMP"
```

Direct stdin JSONL detection also works when the JSONL is produced inline:

```bash
# Every non-empty line of the first sven's JSONL output starts with '{'
# so the second instance auto-detects it as JSONL and seeds history.
sven 'task1' --output-format jsonl | sven 'task2'
```

> **Note**: `--output-format json` (structured step metadata) is different
> from JSONL conversation format.  Use `--output-jsonl PATH` to write
> conversation JSONL.

---

## Stderr stays clean

Sven always writes diagnostics to **stderr** so that stdout pipelines are
unaffected:

```bash
# Capture just the conversation on stdout; discard diagnostics
sven 'task' > result.md

# See only the progress lines
sven 'task' 2>&1 >/dev/null | grep '^\[sven:'

# Pass stdout downstream while monitoring stderr in the terminal
sven 'task' 2>/dev/null | sven 'follow-up'
```

Stderr lines use structured `[sven:tag]` prefixes:

| Prefix | Meaning |
|--------|---------|
| `[sven:step:start]` | Step beginning |
| `[sven:step:complete]` | Step finished with timing and tool count |
| `[sven:tool:call]` | Tool invocation |
| `[sven:tool:result]` | Tool result (success or error) |
| `[sven:tokens]` | Token usage for the turn |
| `[sven:info]` | Informational (e.g. history loaded) |
| `[sven:warn]` | Non-fatal warning |
| `[sven:error]` | Fatal error before exit |

---

## Error cases and exit codes

| Situation | Exit code | Stderr message |
|-----------|-----------|----------------|
| Conversation piped, no CLI prompt, no pending user turn | `2` | `[sven:error] Piped conversation has no pending task.` |
| JSONL piped, no CLI prompt, no pending user turn | `2` | `[sven:error] Piped JSONL has no pending task.` |
| Piped conversation fails to parse | warning + fallback to workflow | `[sven:warn] Failed to parse piped input as conversation (…)` |
| Piped JSONL fails to parse | warning + fallback to workflow | `[sven:warn] Failed to parse piped input as JSONL (…)` |

The error message for the "no pending task" case also prints an example
showing how to fix it:

```
[sven:error] Piped conversation has no pending task.

To continue a piped conversation provide a prompt:

    sven 'task1' | sven 'task2'

Or end the piped output with an unanswered ## User section
so the next sven instance picks it up automatically.
```

---

## Multi-stage pipeline example

```bash
#!/usr/bin/env bash
set -euo pipefail

# Stage 1: research (read-only, faster model)
sven 'Read src/ and list all exported public functions with their signatures' \
    --output-format compact \
    --model anthropic/claude-haiku-4-5 \
    2>/dev/null \
  > /tmp/public-api.txt

# Stage 2: generate tests (uses stage 1 output as user message)
cat /tmp/public-api.txt \
  | sven 'Write a comprehensive test file for each function listed above' \
    --output-last-message tests/generated_tests.rs \
    2>/dev/null

# Stage 3: review (full conversation context from stage 2)
sven --load-jsonl .sven/logs/$(ls -t .sven/logs/*.jsonl | head -1 | xargs basename) \
    'Review the generated tests for correctness and suggest improvements' \
    --output-format compact
```

---

## Relationship to `--resume` and `--jsonl`

| Flag | Use case |
|------|----------|
| `--resume ID` | Continue a saved TUI or CI conversation by ID (interactive or headless) |
| `--jsonl PATH` | Load + save JSONL to the same file (read on start, append on complete) |
| `--load-jsonl PATH` | Load JSONL history as context seed; does not write back |
| `--output-jsonl PATH` | Write JSONL after run; does not read |
| Piped JSONL (stdin) | Auto-detected; seeds history same as `--load-jsonl` but from stdin |

Pipe-based JSONL seeding behaves identically to `--load-jsonl` at runtime;
the only difference is the source (stdin vs file).

---

## Implementation notes

The detection and routing live in `crates/sven-ci/src/runner.rs`:

- `is_conversation_format(s)` — scans lines for reserved H2 headings
- `is_jsonl_format(s)` — checks up to 10 non-empty lines for `{` prefix
- Detection order: JSONL → conversation → workflow (first match wins)
- Both conversation and JSONL parsers return `(history, pending_user_input)`
- Step content = `extra_prompt` OR `pending_user_input` OR exit(2)

The 91 unit tests in `crates/sven-ci/src/tests.rs` cover every detection
branch, the priority chain, round-trips, tool-call preservation, thinking
block handling, and all documented error cases.
