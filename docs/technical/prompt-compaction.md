# Prompt Compaction

Long-running agent sessions accumulate conversation history that eventually
exceeds the model's context window. This document explains how sven prevents
that from crashing workflows, what mechanisms fire and in what order, and how
to reason about the configuration knobs.

---

## The problem

Every LLM API enforces a hard ceiling on the number of tokens it will accept
in a single request. That ceiling is the **context window** — typically
expressed as a total of input + output tokens. When a request exceeds it, the
API returns a 400 error and the workflow fails.

In practice the ceiling is tighter than it looks:

- The model needs headroom for its reply (`max_output_tokens`). The safe input
  limit is `context_window − max_output_tokens`, not the full window.
- Tool schemas are sent with every request but are not stored in the session
  history, so they consume budget invisibly.
- Dynamic context (git branch, CI notes) is injected per-request for the same
  reason.
- The standard `chars / 4` token estimate is a rough approximation. Code files
  with many short identifiers are denser than prose; some providers tokenize
  differently.

Sven addresses all of these through a multi-layer compaction system.

---

## Architecture overview

```
submit()
  │
  ▼
ensure_fits_budget()          ← fires before every model call
  ├─ below threshold?  → no-op
  ├─ normal path       → rolling LLM compaction (Structured or Narrative)
  │    └─ model call fails / empty? → emergency_compact() fallback
  └─ fraction ≥ 0.95   → emergency_compact() (no model call)
  │
  ▼
stream_one_turn()             ← model call
  │  ResponseEvent::Usage
  └─ update_calibration()     ← EMA correction of token estimate
  │
  ▼
Phase 3: push tool results
  └─ smart_truncate()         ← cap large results before they enter the session
  │
  ▼
ensure_fits_budget()          ← fires again after every batch of tool results
```

---

## Layer 1 — Calibrated token accounting

### The budget

Sven maintains three numbers per session:

| Field | Meaning |
|---|---|
| `max_tokens` | Total context window (input + output) from the model catalog |
| `max_output_tokens` | Maximum output tokens per completion from the model catalog |
| `input_budget()` | `max_tokens − max_output_tokens` — the actual usable input ceiling |

All threshold comparisons use `input_budget()`, not the raw window size. This
means the compaction trigger fires with enough headroom for the model to
generate a full-length reply.

### Effective token count

The raw token estimate (`token_count`) is the sum of `approx_tokens()` for
every message in the session — each message's character count divided by 4.
The *effective* estimate adds two corrections:

```
effective_token_count = (token_count × calibration_factor) + schema_overhead
```

**`calibration_factor`** starts at `1.0` and is updated after every model call
via an exponential moving average (EMA):

```
ratio             = actual_input_tokens / (token_count + schema_overhead)
calibration_factor = 0.8 × calibration_factor + 0.2 × ratio
calibration_factor = clamp(calibration_factor, 0.5, 3.0)
```

`actual_input_tokens` is `input_tokens + cache_read_tokens` as reported by the
provider in the `Usage` event. Because cache-read tokens were still *sent*, they
count toward the real prompt size. The slow EMA (α = 0.2) resists per-turn
spikes; the `[0.5, 3.0]` clamp prevents runaway estimates on unusual payloads.

**`schema_overhead`** is recalculated before every model call:

```
schema_overhead = Σ len(name + description + parameters_json) / 4
                  for every tool in the current mode
                + len(dynamic_context_block) / 4
```

Tool schemas and the dynamic context block are sent with every request but
never stored in `session.messages`, so they would otherwise be invisible to
the budget gate.

### Context fraction

```
context_fraction = effective_token_count / input_budget
```

All threshold comparisons use this fraction.

---

## Layer 2 — The budget gate (`ensure_fits_budget`)

`ensure_fits_budget` is called:

1. **Before every user submit** — in `submit()`, `submit_with_cancel()`,
   `submit_with_parts()`, and `replace_history_and_submit()`.
2. **After every batch of tool results** — at the end of Phase 3 in both
   agentic-loop variants, so a single large tool output cannot cause the
   *next* model call to overflow.

The effective threshold is:

```
trigger_threshold = compaction_threshold − compaction_overhead_reserve
trigger_threshold = max(trigger_threshold, 0.1)   // never below 10%
```

The overhead reserve (default 10%) adds a safety margin so that the
compaction prompt itself fits within the window when it is constructed.

### Normal compaction path

When `context_fraction ≥ trigger_threshold` and `context_fraction < 0.95`:

1. **Snapshot** the current `session.messages` and `token_count` — if the
   model call for summary generation fails, these are restored.
2. Separate non-system messages into two groups:
   - `to_compact` — the older portion (all except the last `keep_n` messages)
   - `recent_messages` — the last `keep_n` messages, preserved verbatim
3. **Adjust the split boundary** to avoid breaking tool-use/tool-result pairs
   (see [Split safety](#split-safety-for-tool-callresult-pairs) below).
4. Reduce `to_compact` to a single compaction-prompt message using
   `compact_session_with_strategy`.
5. Make a **tool-free model call** (`run_single_turn`) to generate the summary.
6. Rebuild the session: `[system, assistant(summary), ...recent_messages]`.
7. Emit `AgentEvent::ContextCompacted` with `tokens_before`, `tokens_after`,
   `strategy`, and `turn`.

If the model call **fails** or returns an **empty string**, the original
messages are restored from the snapshot and the emergency path runs instead.
The agent never propagates a compaction error to the caller.

### Emergency path

When `context_fraction ≥ 0.95`:

- Drop all non-system messages except the last `keep_n`.
- Prepend a canned notice informing the model that earlier history was lost.
- No model call — always succeeds regardless of session size.
- Emits `CompactionStrategyUsed::Emergency`.

---

## Layer 3 — Compaction strategies

The strategy is selected by the `compaction_strategy` config key.

### Structured (default)

The compaction prompt instructs the model to produce exactly six Markdown
sections. The model is not allowed to add or remove sections:

```markdown
## Active Task
## Key Decisions & Rationale
## Files & Artifacts
## Constraints & Requirements
## Pending Items
## Session Narrative
```

Technical details — file paths, function names, error messages, code snippets,
test names — are preserved verbatim within those sections. The result is a
machine-parseable checkpoint that the model can reference reliably on
subsequent turns.

### Narrative

The legacy strategy. A free-form prose summary of the conversation. Useful for
highly conversational sessions where structured sections add little value.

---

## Layer 4 — Smart tool-result truncation

Large tool outputs are the primary cause of sudden context spikes. Before a
tool result is pushed into the session, `smart_truncate` applies a
content-aware extraction based on the tool's declared `OutputCategory`.

### OutputCategory

Each tool in the `Tool` trait declares its output category via
`fn output_category(&self) -> OutputCategory`. The default is `Generic`.
`sven-core` dispatches on the category — it never references tool names
directly.

| Category | Tools | Strategy |
|---|---|---|
| `HeadTail` | `shell`, `run_terminal_command`, `gdb_command`, `gdb_interrupt`, `gdb_wait_stopped` | Keep first 60 + last 40 lines; both the command preamble and the final result remain visible |
| `MatchList` | `grep`, `search_codebase`, `read_lints` | Keep leading matches only; later matches are less relevant |
| `FileContent` | `read_file`, `fs` | Balanced head + tail split; preserves imports/declarations and the most recent changes |
| `Generic` | all others | Hard-truncate at the nearest line boundary |

Every truncated result ends with an explicit notice:

```
[... 42 lines / 18340 bytes omitted ...]
[... use read_file with offset/limit to see more ...]
```

The token cap is controlled by `tool_result_token_cap` (default 4000 tokens).
The cap uses the same `chars / 4` approximation as `approx_tokens` — it is not
calibrated. This means a token-dense code file might be allowed slightly more
than 4000 tokens after truncation, but the budget gate will catch any remaining
excess before the next model call.

### Why separation of concerns matters here

By putting `output_category()` on the `Tool` trait rather than in
`compact.rs`, sven achieves clean crate independence:

- `sven-tools` owns *what shape* each tool's output has.
- `sven-core` owns *how to truncate* each shape.
- Adding a new tool never requires editing `sven-core`. The tool just overrides
  `output_category()`.

---

## Split safety for tool-call/result pairs

Anthropic (and other providers) require that every `tool_result` block in the
conversation has a corresponding `tool_use` block in the immediately preceding
assistant message. If rolling compaction summarises the `tool_use` messages but
preserves their `tool_result` messages in `recent_messages`, the next API call
fails with:

```
messages.2.content.0: unexpected `tool_use_id` found in `tool_result` blocks
```

After computing the raw `summarize_count`, sven walks the boundary backward:

```rust
while summarize_count > 0 && summarize_count < non_system.len() {
    match &non_system[summarize_count].content {
        ToolResult { .. } | ToolCall { .. } => summarize_count -= 1,
        _ => break,
    }
}
```

This moves the split past the entire tool-interaction group (all `ToolCall`
messages and all their `ToolResult` messages) into `recent_messages`. The split
always lands on a user message or an assistant-text message — never mid-batch.

---

## Event reporting

Every compaction emits `AgentEvent::ContextCompacted`:

```rust
ContextCompacted {
    tokens_before: usize,           // token_count before compaction
    tokens_after:  usize,           // token_count after rebuild
    strategy: CompactionStrategyUsed, // Structured | Narrative | Emergency
    turn: u32,                      // agentic loop round number (0 = pre-submit)
}
```

The TUI displays this in the chat pane. CI logs it as:

```
[sven:context:compacted:structured] 60674 → 7144 tokens (tool round 11)
```

The `turn` field distinguishes pre-submit compaction (`turn=0`) from mid-loop
compaction triggered by a large tool result.

---

## Configuration reference

All fields live under the `agent` section of `sven.yaml` or `.sven/config.yaml`.

| Key | Default | Description |
|---|---|---|
| `compaction_threshold` | `0.85` | Fraction of `input_budget` that triggers compaction |
| `compaction_keep_recent` | `6` | Number of non-system messages preserved verbatim (≈ 3 back-and-forth turns) |
| `compaction_strategy` | `structured` | `structured` or `narrative` |
| `tool_result_token_cap` | `4000` | Per-result token ceiling before truncation |
| `compaction_overhead_reserve` | `0.10` | Safety margin subtracted from `compaction_threshold` |

### Tuning for CI / long-running workflows

```yaml
agent:
  compaction_threshold: 0.65       # fire earlier, more margin for schema overhead
  compaction_keep_recent: 15       # keep more recent context for multi-step plans
  compaction_strategy: structured  # structured checkpoint survives many turns
  tool_result_token_cap: 2000      # tighter cap if build logs are large
  compaction_overhead_reserve: 0.12
```

### Tuning for interactive sessions

```yaml
agent:
  compaction_threshold: 0.80       # allow longer free-form conversation
  compaction_keep_recent: 5
  compaction_strategy: narrative   # prose summary reads more naturally in TUI
  tool_result_token_cap: 8000      # more context per file read
```

---

## Compaction failure modes and mitigations

| Failure | What sven does |
|---|---|
| Compaction model call fails (network, rate limit) | Restores original session from snapshot; falls back to emergency compaction; does not propagate error |
| Compaction returns empty summary | Same fallback as above |
| Tool result too large for cap | Truncated before being pushed; omission notice appended |
| Session grows too fast to compact normally (≥ 0.95) | Emergency compaction: deterministic drop with no model call |
| `ToolResult` would be orphaned by the split | Boundary walked backward to nearest safe split point |

---

## Data flow diagram

```
User submits message
        │
        ▼
[ensure_fits_budget: turn=0]
        │
        ├── not triggered ──────────────────────────────────────┐
        │                                                        │
        ├── normal compaction ─► run_single_turn                 │
        │       ├── success ─► rebuild session                   │
        │       └── failure ─► restore snapshot → emergency     │
        │                                                        │
        └── emergency (≥0.95) ─► drop old messages              │
                                                                 │
        ◄────────────────────────────────────────────────────────┘
        │
        ▼
session.push(system)          [if first turn]
session.push(user_message)
        │
        ▼
[stream_one_turn]
  schema_overhead recalculated
  model streaming call
  ResponseEvent::Usage → update_calibration()
        │
        ▼
[tool calls dispatched in parallel]
        │
        ▼
[Phase 3: push tool results]
  for each result:
    category = registry.output_category(tool_name)
    content  = smart_truncate(content, category, cap)
    session.push(tool_result)
        │
        ▼
[ensure_fits_budget: turn=N]   ← same logic, fires after every tool batch
        │
        ▼
[next model call or TurnComplete]
```
