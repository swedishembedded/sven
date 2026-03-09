# Parallel Tool Slots

This document explains how sven executes multiple tool calls concurrently and
why it can start executing a tool before the model finishes streaming its
response.

---

## The problem with sequential execution

When a model decides to call several tools in one turn it emits them all in a
single streaming response.  Under the old architecture sven had to wait for the
entire stream to finish before it could start executing anything:

```
stream fully done
  → execute tool A
  → execute tool B
  → execute tool C
  → push results to session
  → next model call
```

Long-running tools (shell commands, `context_query`, `delegate_task`) could
each take several seconds.  Running them one after another on top of the full
stream wait added up to noticeable latency.

---

## The new model — streaming dispatch

The new architecture splits tool execution into three overlapping phases:

```
LLM stream:
  slot 0 args stream in … { complete } ──→ spawn task A  ─────────────────┐
  slot 1 args stream in … { complete } ──→ spawn task B  ──────────────┐  │
  slot 2 args stream in … { complete } ──→ spawn task C  ─────────┐   │  │
  stream Done                                                      │   │  │
                                                                   │   │  │
  join_all: await C ◀────────────────────────────────────────────┘   │  │
            await B ◀────────────────────────────────────────────────┘  │
            await A ◀────────────────────────────────────────────────────┘
  → push ToolCall × 3, then ToolResult × 3 (index order)
  → next model call
```

Each tool slot is dispatched as soon as its JSON argument object is complete.
Slots run in parallel with the remainder of the LLM stream and with each
other.  `join_all` awaits them all using `FuturesUnordered` — completing in
arrival order — then sorts results back into the original slot index order
before pushing them to the session.

---

## ToolSlotManager

`ToolSlotManager` (in `crates/sven-core/src/tool_slots.rs`) owns the entire
lifecycle of a turn's tool calls.  One instance is created at the start of each
`stream_one_turn` call and consumed by `join_all` at the end.

### State machine per slot

```
Accumulating ──(JSON complete)──→ Dispatched(JoinHandle)
```

| State | Description |
|-------|-------------|
| `Accumulating(PendingSlot)` | Still receiving streaming argument chunks. |
| `Dispatched { tc, handle }` | `tokio::spawn` task is in flight. |

### Key methods

| Method | Purpose |
|--------|---------|
| `feed(index, id, name, args_chunk)` | Apply a streaming chunk.  Returns `Some(ToolCall)` the first time a slot's args form valid JSON — the caller emits `AgentEvent::ToolCallStarted` at that point. |
| `finalize_remaining()` | Called after `ResponseEvent::Done`.  Force-finalizes any slots whose JSON was still incomplete, using the JSON repair path.  Returns newly dispatched `ToolCall`s. |
| `insert_call(index, tc)` | Insert a pre-built `ToolCall` (used for the inline XML `<invoke>` fallback). |
| `is_empty()` | Returns `true` when no tool calls were seen — used to decide whether to check for the XML fallback. |
| `join_all(tx)` | Await every `JoinHandle` via `FuturesUnordered`.  Emits `AgentEvent::ToolCallFinished` for each as it completes.  Returns results sorted by slot index. |

### JSON readiness detection

Each incoming chunk is appended to the slot's `args_buf`.  After every append
the slot runs a probe-parse:

```
if args_buf.ends_with('}') {
    serde_json::from_str(&args_buf)?  // → dispatch if Ok
}
```

Parsing a partial JSON object fails in well under a microsecond.  The fast path
dispatches the moment the model emits a closing brace, not at `Done`.

When the stream ends and a slot is still `Accumulating`, `finalize_remaining`
runs three repair strategies in order:

1. Parse the buffer as-is.
2. Fix invalid escape sequences (e.g. `\c` → `\\c`), then re-parse.
3. Attempt structural repair (close open strings, append `}`), then re-parse.
4. Substitute `{}` if all repairs fail, and log a warning.

---

## Event ordering

`AgentEvent` consumers (TUI, CI runner, ACP) see this sequence per turn:

```
ToolCallStarted(slot 0)      ← emitted as soon as slot 0 args are complete
ToolCallStarted(slot 1)      ← slot 1 may complete before slot 2 or after
ToolCallStarted(slot 2)
  … tool progress events (ProgressUpdate, TodoUpdate, ModeChanged) …
ToolCallFinished(slot N)     ← whichever task finishes first
ToolCallFinished(slot M)
ToolCallFinished(slot K)
```

Progress events from in-flight tools arrive while the LLM is still streaming.
The agentic loop drains the `tool_event_rx` channel both inside the stream loop
and inside the `join_all` loop so these events reach the TUI in real time.

---

## Session ordering invariant

OpenAI's API requires all assistant `ToolCall` messages to precede any
`ToolResult` messages within a single turn.  Because tools may complete in
arbitrary order, the agent preserves this by:

1. Collecting all `(ToolCall, ToolOutput)` pairs from `join_all`.
2. Sorting by slot index.
3. Pushing all `Message::ToolCall` entries first.
4. Pushing all `Message::ToolResult` entries second.

This means the session history is always well-formed regardless of which tool
finishes first.

---

## Cancellation

`ToolSlotManager` implements `Drop`:

```rust
impl Drop for ToolSlotManager {
    fn drop(&mut self) {
        for (_, state) in self.slots.drain() {
            if let SlotState::Dispatched { handle, .. } = state {
                handle.abort();
            }
        }
    }
}
```

When the user cancels a running session (e.g. `Ctrl-C` in the TUI), the
agentic loop's `tokio::select!` takes the cancellation branch, which drops the
future that owns the `ToolSlotManager`.  The `Drop` impl aborts every
in-flight task immediately, so tools do not continue running detached in the
background.

---

## XML `<invoke>` fallback

Some providers emit tool calls as inline XML rather than the structured
function-call API.  After the stream ends, if no JSON tool calls were seen,
the agent parses `<invoke name="…">…</invoke>` blocks from the response text.
These are inserted into the same `ToolSlotManager` via `insert_call` and then
executed through `join_all` — the same parallel pipeline with the same session
ordering guarantees.

---

## Latency savings

The savings per turn are approximately:

```
saved ≈ Σ max(0, exec_time(slot_N) − time_remaining_in_stream_after_slot_N_ready)
```

For a turn where the model emits two tool calls and the first one's arguments
are complete halfway through the stream, that tool runs for its full execution
time in parallel with the second half of the stream and the second tool's
execution.  Empirically this eliminates most of the per-tool overhead for
workloads that combine a fast tool with a slow one in the same turn.
