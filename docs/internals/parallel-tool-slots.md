# Parallel Tool Execution

This document explains how sven turns the tool calls of one model response
into concurrently running tool tasks, and how the results find their way back
into the conversation.

Three components share the work:

| Stage | Component | Responsibility |
|-------|-----------|----------------|
| Accumulation | `sven_turn::stream_turn` | Collect streamed tool-call chunks into complete `ToolCall`s. Executes nothing. |
| Dispatch | the machine's loop (`sven-machines`, `loop_core`) + the kernel | Emit one `Effect::CallTool` per proposed call and gate each through the permission policy. |
| Execution | `ToolExecutor` (`crates/executors/src/tool.rs`) | Run each allowed call on its own tokio task and report the result. |

---

## Accumulation - `stream_turn`

Providers stream tool calls as indexed chunks (`ResponseEvent::ToolCall
{ index, id, name, arguments }`). `stream_turn` keeps one accumulation slot per
index and appends each argument chunk to that slot's buffer.

Whenever a slot's buffer ends with `}`, the slot probe-parses it. The first
time the buffer parses as JSON, the call is complete: `AgentEvent::ToolCallStarted`
is emitted immediately, while the rest of the response is still streaming.

When the stream ends, every slot that never parsed is finalized:

1. Parse the buffer as-is.
2. Otherwise run `attempt_json_repair` (`crates/turn/src/tool_slots.rs`):
   fix invalid escape sequences, split fused keys, and close an open string
   and object.
3. If every repair fails, substitute `{}` and log a warning.

A slot with an empty name is dropped (it cannot be dispatched); a slot with an
empty id gets a synthetic `tc_synthetic_N` id.

### `<invoke>` fallback

Some models write tool calls as inline Anthropic-style XML instead of using the
structured function-call API. When a response produced no native tool calls
and its text contains `<invoke `, `stream_turn` extracts every
`<invoke name="...">...</invoke>` block as a tool call and removes it from the
text.

---

## Dispatch - `TurnExecutor`, the machine and the kernel

After the stream finishes, `TurnExecutor`:

1. Appends the assistant turn (text plus one tool-call message per call) to the
   conversation thread in the shared `ThreadStore`.
2. Records `call_id → (thread, original id)` so `ToolExecutor` can later append
   each result to the right thread under the exact id the model assigned.
3. Posts `Event::LlmTurnComplete { thread, text, tool_calls }` to the machine.

The machine's loop (`loop_core::on_llm_turn_complete`) emits one
`Effect::CallTool` per proposed call, records them in its pending set, and
stays in its current state while they run. The kernel classifies each
`CallTool` individually against the session's permission policy; allowed calls
go straight to the executor.

---

## Execution - `ToolExecutor`

Every `CallTool` effect is spawned on its own tokio task (spawn-and-forget), so
the kernel's consumer loop returns immediately and all calls from one response
run concurrently. Each invocation is bounded by a wall-clock watchdog (600 s by
default) so a hung or panicking tool still produces a result instead of
leaving the call pending forever.

When a task finishes, `ToolExecutor`:

1. Emits `UiEvent::ToolCallFinished` with the full, untruncated output.
2. Appends the result to the call's thread, truncated by
   `sven_turn::smart_truncate` according to the tool's `OutputCategory` (see
   [prompt-compaction.md](prompt-compaction.md#layer-3---smart-tool-result-truncation)).
3. Posts `Event::ToolSucceeded` or `Event::ToolFailed` to the machine.

Results arrive in completion order, not call order. The machine removes each
call from its pending set; once the set is empty it emits the next
`CallLlm`.

---

## Thread ordering

The assistant's tool-call messages are appended by `TurnExecutor` before any
tool task can finish, so every tool result in a thread follows the call it
answers. Before the next turn is sent, `TurnExecutor` also answers any call the
kernel refused before it ran (`TurnRequest::refused_calls`), so every tool call
in the thread has a result when the model sees it again.

---

## Event ordering

`AgentEvent` consumers (TUI, CI runner, ACP) see, per turn:

```
ToolCallStarted(call A)      ← as soon as A's arguments parse, mid-stream
ToolCallStarted(call B)
  ... TurnComplete for the model turn ...
  ... tool progress events (TodoUpdate, ModeChanged, ...) ...
ToolCallFinished(whichever call finishes first)
ToolCallFinished(the other)
```
