# Headless and CI

Headless mode runs the agent without the terminal UI and writes the result to
stdout. Use it in scripts, pipelines and CI jobs.

## When sven runs headless

A run is headless when any of these holds:

| Trigger | Example |
|---------|---------|
| `--headless` (`-H`) | `sven --headless "Fix the bug"` |
| A positional prompt | `sven "List the TODO comments in src/"` |
| Stdin is not a terminal | `echo "analyse the codebase" \| sven` |
| Stdout is not a terminal | `sven "task 1" \| sven --stdin "task 2"` |

`--file` alone does not make a run headless: in a terminal,
`sven --file workflow.md` opens the TUI with the workflow's steps queued for
review. Add `--headless` to run it unattended.

Stdin is read only when it is the input: when there is no prompt, or when
`--stdin` asks for it alongside a prompt. A prompted run never waits on an
inherited pipe that nobody writes to or closes. If stdin is the input and
nothing arrives within two seconds, sven says so on stderr.

---

## Quick Start

```bash
# Single-step task
echo "List all TODO comments in src/" | sven --model mock

# Multi-step workflow file
sven --headless --file .sven/workflow/refactor.md

# Pipe output into a second instance for follow-up
sven "plan the refactor" | sven --stdin "implement step 1"
```

---

## Single-prompt and workflow runs

A headless run goes through one of two runners. Which one is not a flag; it
follows from the options.

The **single-prompt runner** drives one agent turn to completion. It handles a
fresh prompt and a prior conversation piped on stdin, and it is used when none
of the workflow-only options below is present and stdin is not JSONL steps or
a JSON trajectory. It also serves `--mode chat` and
`--mode sdlc`.

The **workflow runner** runs the steps of a [workflow file](workflows.md) one
after another and implements everything else. A run uses it as soon as any of
these is given: `--file`, `--output-format` other than `conversation`,
`--artifacts-dir`, `--dry-run`, `--output-last-message`, `--system-prompt-file`,
`--var`, `--attach`, `--output-trace`, `--trace`, `--load-trace` or `--resume`,
or when the stdin it reads is JSONL steps or a JSON trajectory.

What differs:

| | Single-prompt runner | Workflow runner |
|---|---|---|
| Steps, frontmatter, `{{var}}` templates, `cache_key` | no | yes |
| Stdin formats understood | prompt text, conversation markdown | prompt text, conversation markdown, JSONL steps, JSON trajectory (piped JSONL or JSON selects this runner) |
| Output formats | `conversation` | `conversation`, `compact`, `json`, `jsonl` |
| Time limit | the run limit | per-step and run limits |
| Exit code `3` (tool errors) | never | yes |
| Trajectory | written to `.sven/logs/` when a `.sven/` directory exists | same, or the `--output-trace` / `--trace` path |

Details of the stdin formats are in [Pipe composition](pipe-composition.md).

---

## Output Format

By default, headless runs write **conversation-format markdown** to stdout.
This is the same format sven reads back as conversation history (see
[Conversation Mode](workflows.md#conversation-mode)) and is always pipeable back into
another sven instance.

````markdown
## User
Analyse the project structure and list the top-level modules.

## Sven
I'll list the directory first.

## Tool
```json
{
  "tool_call_id": "call_1",
  "name": "find_file",
  "args": { "pattern": "*" }
}
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
````

Diagnostics (tool calls, step progress, token usage, errors) go to **stderr**
so the stdout pipeline stays clean.

### `--output-format`

| Value | Description |
|-------|-------------|
| `conversation` (default) | Full `## User` / `## Sven` / `## Tool` / `## Tool Result` markdown, streamed as it happens |
| `compact` | Plain text: the final response of each step, no headings or markup |
| `json` | The run's complete [ATIF trajectory](../internals/trajectories.md) as one pretty-printed JSON document, written at the end |
| `jsonl` | One ATIF step per JSON line, streamed as each step closes; the format for piping between sven instances with full fidelity |

Use `compact` whenever the caller only needs the agent's answer text - for
example when the response is a shell command to be executed, a value to be
parsed, or input for another tool. The non-default formats run on the workflow
runner.

```bash
# JSON output - a trajectory with one entry per step, for CI dashboards
sven --headless --file workflow.md --output-format json \
  | jq '.steps[] | select(.source == "agent") | .message'

# Compact output - only the agent's response, no markdown formatting
sven --headless --output-format compact "List the top 5 TODO files" 2>/dev/null

# Generate a shell command and execute it directly
sven --headless --output-format compact \
     "Write a one-liner that prints CPU usage. Reply with the command only." \
     2>/dev/null | sh
```

## Project Context

In headless mode, sven walks up the directory tree from the working directory
to the nearest `.git` and uses that directory as the project root; with no
`.git` above it, the working directory is the root. It adds the root to the
system prompt:

```
## Project Context
Project root: `<absolute path to your project>`
Workspace root: same as project root.
- When the user provides relative paths, resolve them relative to the project root.
- Use absolute paths for all file read/write operations.
```

When a `.west` or `.cursor` directory exists up to five levels above the
project root, that directory is named as the workspace root and relative paths
resolve against it instead.

### Git context

In addition to the project root path, sven collects git metadata and
adds it to the system prompt:

```
## Git Context
Branch: feat/headless-improvements
Commit: d3adb33
Remote: git@github.com:acme/myproject.git
Uncommitted changes: 3 file(s)
```

A clean tree prints `Working tree: clean` instead of the last line. Under a
recognised CI provider (GitHub Actions, GitLab CI, CircleCI, Travis CI,
Jenkins, Azure Pipelines, Bitbucket Pipelines, or `CI=true`) a
`## CI Environment` section names the provider, repository, branch, commit and
pull request.

### Project context file

sven looks for a project-level instructions file and tells the model where it
is, in a **Project Instructions** section. The file is **not** inlined: the
model reads it with its file tool when it is relevant to the task. The first
that exists is used:

| Path | Purpose |
|------|---------|
| `.sven/context.md` | sven-specific project instructions |
| `AGENTS.md` | Standard agent instructions |
| `CLAUDE.md` | Claude Code project file |

Example `.sven/context.md`:

```markdown
# Project conventions

- All Rust code must pass `cargo clippy -- -D warnings`.
- Write tests for every public function.
- Keep commits atomic; one logical change per commit.
- The project root is a Cargo workspace; always run cargo from the workspace root.
```

`--no-system` skips the lookup together with the rest of sven's own prompt.

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

These flags work alongside the configuration key `agent.system_prompt` and
take precedence over it. `--no-system` suppresses sven's own
identity/guidelines/context sections; if `--system-prompt-file`,
`--append-system-prompt` or `agent.system_prompt` still supply text, that text
is sent verbatim as the entire system message. With none of those,
`--no-system` means no system message is sent at all.

A workflow file's preamble (the text between its `#` title and the first `##`
step) is appended to the system prompt too, before `--append-system-prompt`.

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

To keep the tools but stop paying for every definition on every request, use
`--tool-disclosure deferred` (the default of a program that reads the
configuration file; `full` restores the complete listing for the process). The
request then carries the definitions of the core tools only, and the model
loads any other tool through the `tools` tool before it calls it. The
`--token-ledger` line shows the difference; see
[Deferred tool disclosure](../internals/tool-disclosure.md).

## Approval

A headless run uses `--approval auto`: every call the mode allows runs without
asking, and a question from the agent is answered at once ("No user is
available to answer this question. Proceed on your best judgement and state the
assumption you made."). Confine what an unattended run may do with `--mode
research` or `--mode plan` (they never write), `tools.disabled` and
`tools.deny_patterns` (see [Configuration](../reference/configuration.md#tools)).
`--approval manual` is refused in a headless run.

---

## Capturing Output

### Save the last agent response

Write only the final agent reply to a file (without conversation formatting):

```bash
sven --file review.md --output-last-message review-summary.txt
cat review-summary.txt
```

Unlike `--output-format compact`, this does not change stdout, so you can
still capture the full conversation there:

```bash
# Full conversation on stdout AND last message saved to file
sven --file review.md --output-last-message summary.txt > full-review.md
```

### ATIF Trace Output

Save the complete conversation trace as a standards-compliant [ATIF](https://github.com/harbor-framework/harbor/blob/main/rfcs/0001-trajectory-format.md)
trajectory document - steps, reasoning, tool calls, observations, and token
metrics, directly consumable by Harbor-style debugging, visualization, SFT,
and RL pipelines:

```bash
# Save the complete trajectory
sven --file workflow.md --output-trace trace.json

# Works with any run mode
sven --headless "analyze the code" --output-trace analysis-trace.json
sven --file conversation.md --output-trace continued-trace.json
```

The trace file is a single pretty-printed JSON document (an ATIF `Trajectory`
object), not line-delimited - see
**[Pipe Composition](pipe-composition.md)** for the formats sven reads and
writes. The workflow runner rewrites it after every step, so an interrupted
run leaves a valid partial document.

A step whose tool call the permission gate decided about, or whose arguments
broke the tool's parameter schema, carries `extra.sven.gate`; a step that read
from memory carries `extra.sven.memory`. Their fields are listed in
**[The Sven SDK](../reference/sdk.md)**, section "Trajectories".

Every headless run also writes a trajectory to `.sven/logs/<timestamp>.atif.json`
when a `.sven/` directory exists in the working directory or one of its
ancestors, whether or not `--output-trace` was given - a flat directory of
`*.json` documents an RL or SFT pipeline can ingest directly. With no `.sven/`
directory and no `--output-trace`, nothing is written.

### Under Harbor

`benchmarks/sven_agent.py` runs sven as a Harbor installed agent (`make
benchmark/terminal-bench`). It passes `--output-trace /logs/agent/trajectory.json`, so
the trial's `agent/` directory holds sven's ATIF trajectory next to Harbor's own
result, and it fills the trial context's token and cost totals from the trajectory's
final metrics (a total the trajectory does not state is left unset). The adapter is
written against the agent base class of harbor 0.1.45, which is the version
`benchmarks/requirements.txt` pins: harbor 0.24.0 replaced that base class, so the
adapter does not import there. `make test/benchmarks` tests the trajectory reader
without importing harbor. An embedding application reads the trial directories back
(`agent/trajectory.json` and `result.json`).

### Outcome rewards

A **concluded** trajectory carries the outcome of its run for a trainer to
weight it by, under `final_metrics.extra`:

```json
"final_metrics": {
  "extra": {
    "reward": 0.0,
    "outcome": "agent_error",
    "tool_calls": 10,
    "tool_errors": 2
  }
}
```

`reward` is a float in `[0.0, 1.0]`; `0.0` means "do not learn from this run".
The other keys are diagnostics for reading a training set by eye - only
`reward` is meant to be consumed programmatically.

| Run ended with | `outcome` | `reward` |
|----------------|-----------|----------|
| the agent reported an error | `agent_error` | `0.0` |
| interrupted (Ctrl+C) or aborted | `cancelled` | `0.0` |
| a step or run deadline elapsed | `timeout` | `0.0` |
| `--max-tokens` reached | `budget_exhausted` | `0.0` |
| finished, a verifier confirmed it (`sven task run`) | `verified_success` | `0.5`-`1.0`, lowered by the share of tool calls that failed |
| finished, a verifier refuted it | `verification_failed` | `0.0` |
| finished, nothing verified it | - | **absent** |

**An absent `reward` means the outcome is unknown, not that it was zero.** An
agent that reports "done" proves nothing, so a run that merely finishes carries
no reward; it records the reason under `sven_outcome_unknown_reason` instead
(`no_verifier`, or `waiting_for_human` for a parked run). A consumer must skip
such a trajectory rather than defaulting it. Failures are scored because they
need no verifier to be known. A trajectory written while a run is still going
is unstamped as well, so only concluded runs are ever trained on. See
[Trajectories](../internals/trajectories.md).

### Token ledger

Every model request is accounted for: which sources its input tokens came
from, reconciled with what the provider reported. The ledger is saved in the
trajectory of every run; `--token-ledger` also prints it as it happens.

```bash
# One JSON line per model request on stderr, next to the usual [sven:tokens]
sven --token-ledger --output-trace run.json 'fix the failing test'

# Summarize a saved trajectory, subagents included
sven ledger run.json          # short table
sven ledger run.json --json   # one JSON object
```

The `[sven:ledger]` line is additional output: `[sven:tokens]` is unchanged
and appears with or without the flag. See
**[Token ledger](../internals/token-ledger.md)** for the record
format and the estimator.

### Redirect by format

```bash
# Just the answers (compact)
sven --file plan.md --output-format compact > answer.txt

# Machine-readable trajectory for dashboards
sven --file plan.md --output-format json > run.json

# Full replayable conversation
sven --file plan.md > conversation.md
sven --file conversation.md  # answer the trailing ## User section
```

---

## Timeouts

Configure timeouts on the command line or in the configuration file; the
command line wins:

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

A step may also set its own limit with a `timeout=` directive, which wins over
`--step-timeout` and `max_step_timeout_secs` for that step (see
[Workflows](workflows.md#per-step-directives)). `timeout=0` removes the step
limit; the run limit still applies.

**A headless run is always bounded.** With no limit given, the run limit is
`agent.max_run_timeout_secs`, which defaults to four hours: generous for real
work, finite so that a job whose model server, tool, or approval or question
handler never answers still ends, with exit code `124`, instead of holding a CI
runner until the platform's own limit. The run limit covers every wait in the
run - a model that goes quiet, a tool that never returns, a gate nobody
answers - and it also ends a step in progress, not only the gaps between
steps; a step's limit is shortened to the time the run has left. `--run-timeout
0` (or `max_run_timeout_secs: 0`) removes the limit; do that only where
something else bounds the job. A single-prompt run takes `--step-timeout` as its
limit when `--run-timeout` is not given.

On a timeout sven writes the partial trajectory, when one is being written,
stamped `timeout` with reward `0.0`, before it exits.

Shorter limits apply inside a run: the model-server stall limits
(`agent.first_byte_timeout_secs` and friends, see
[Stalled model servers](../reference/configuration.md#stalled-model-servers)), the
per-command `tools.timeout_secs`, and each MCP server's `timeout_secs`. The
token budget `--max-tokens N` is a third bound: when input plus output tokens
reach `N` the run stops with exit code `4`.

---

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Success - all steps completed |
| `1` | Agent error (a failed step, API error, 20 consecutive tool errors) or an error before the run started (unreadable `--file`, missing `--load-trace` file) |
| `2` | Invalid input (unreadable `--system-prompt-file`, malformed `--load-trace` file, piped conversation with no task) |
| `3` | The run completed, but at least one tool call returned an error (workflow runner) |
| `4` | `--max-tokens` budget exhausted |
| `5` | Waiting for a person: a parked question or verified task; the trajectory is left unscored |
| `124` | Timeout exceeded (step or total run) |
| `130` | Interrupted (Ctrl+C) |

Exit `3` lets a script tell a clean run from a partly successful one; treat it
as success with `|| [ $? -eq 3 ]` where tool errors are tolerable.

---

## Artifacts

Save per-step and full-conversation outputs to a directory (workflow runner):

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

The step file name is the step number, then the heading with every character
other than letters, digits, `-` and `_` replaced by `_`.

---

## Progress Reporting

Sven writes structured progress lines to **stderr** using `[sven:...]`
prefixes that are easy to scrape with grep or awk:

```
[sven:settings] model=claude-sonnet-4-6 mode=agent
[sven:step:start] 1/3 label="Analyse codebase"
[sven:tool:call] id="call_1" name="find_file" args={"pattern":"*"}
[sven:tool:result] id="call_1" name="find_file" success=true size=812
[sven:tokens] input=1520 output=84 cache_read=0 cache_write=0 ...
[sven:step:complete] 1/3 label="Analyse codebase" duration_ms=4321 tools=1 success=true
[sven:step:start] 2/3 label="Propose improvements"
...
```

A failed tool call prints `success=false` and its output. At `-v`, a successful
result also carries a truncated `output=` snippet. The full set of prefixes is
in [Pipe composition](pipe-composition.md#stderr-stays-clean).

While the model has produced nothing real (no text, reasoning or tool-call
chunk; keepalives do not count), sven writes a line every 5 seconds so a long
wait does not look like a hang:

```
[sven:info] waiting for model 45 s (awaiting first token)
```

The phase is `connecting` (no response headers yet), `awaiting first token`, or
`idle` (the model produced output and went quiet). A server that stays silent
past its limit fails the step; see
[Stalled model servers](../reference/configuration.md#stalled-model-servers) for the
limits and how to raise them.

Filter progress from a CI log:

```bash
sven --headless --file workflow.md 2>&1 >/dev/null | grep '^\[sven:step:complete\]'
```

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

Inputs: `workflow-file` (required), `model`, `mode` (`agent`), `output-format`
(`conversation`), `artifacts-dir` (`.sven/artifacts`; each run writes to a
`<run id>` subdirectory), `step-timeout` and `run-timeout` (`0` omits the flag,
so the configured defaults apply), `dry-run`, `vars` (one `KEY=VALUE` per
line) and `sven-version` (downloads that release; empty uses `sven` from
`PATH`). Outputs: `conversation` (path of the transcript) and `exit-code`. The
transcript goes to `conversation.md` and the log to `run.log` in the run's
artifacts directory.

### GitLab CI

```yaml
ai-review:
  stage: review
  script:
    - sven --headless --file .sven/workflow/review.md
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

OUTPUT=$(sven --headless --file .sven/workflow/audit.md \
              --output-format compact \
              --step-timeout 120 \
              2>/dev/null)

echo "Audit result:"
echo "$OUTPUT"
```
