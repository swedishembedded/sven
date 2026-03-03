---
name: p2p-loop-analysis
description: Analyse the sven P2P agent network for infinite message loops (echo loops, delegation storms, circular routing). Use when investigating unexpected runaway traffic between agents, infinite back-and-forth between nodes, task chains that never terminate, or when adding a new message channel/handler and needing to verify it cannot loop. Covers both the Task (delegate_task) channel and the Session (send_message) channel.
---

# P2P Loop Analysis — sven

## The Two Invariants

Every message path in sven must satisfy one of two loop-breaking invariants.
If either invariant is missing or broken, an infinite loop is possible by design.

| Channel | Wire type | Invariant | Enforced in |
| ------- | --------- | --------- | ----------- |
| **Task** | `TaskRequest` | `depth` strictly increases per hop; bounded by `MAX_DELEGATION_DEPTH` | `execute_inbound_task`, `DelegateTool::execute` |
| **Session** | `SessionMessageWire` | Auto-reply only to `role == User`; never reply to `role == Assistant` | `run_session_executor`, `execute_inbound_session_message` |

A loop requires an infinite message sequence. Both invariants make that impossible:

- **Task**: depth increases by 1 per hop → reaches `MAX_DELEGATION_DEPTH` → rejected → chain terminates.
- **Session**: every auto-reply carries `role: Assistant` → `Assistant` is never auto-replied to → chain terminates after 1 round-trip.

---

## Analysis Workflow

### Step 1 — Map all message handlers

Read these files in order:

```text
crates/sven-p2p/src/protocol/types.rs     — wire types (P2pRequest, TaskRequest, SessionMessageWire)
crates/sven-node/src/node.rs              — run_task_executor, run_session_executor,
                                            execute_inbound_task, execute_inbound_session_message
crates/sven-node/src/tools.rs             — DelegateTool::execute, SendMessageTool::execute
crates/sven-p2p/src/node.rs               — NodeState event loop, on_task_message, on_session_message
```

For each handler that sends an outbound message, ask:

1. Under what condition does it send?
2. What is the role/status of the outbound message?
3. Will the receiver's handler for that message type send another message?
4. If yes — what is the termination condition?

### Step 2 — Verify the Task invariant

Check every code path that calls `p2p.send_task()`:

```text
[ ] depth is incremented by exactly 1 before sending
[ ] depth check fires BEFORE the LLM runs (in execute_inbound_task, not inside the tool)
[ ] our_peer_id_str is non-empty before the chain/cycle check — reject if empty
[ ] chain.contains(&our_peer_id_str) check is NOT guarded by is_empty() (old bug pattern)
[ ] DelegateTool::execute guards local_peer_id_string() for empty before push to chain
```

The old vulnerable pattern (replaced):

```rust
// WRONG — skips check when peer ID is empty (startup race)
if !our_peer_id_str.is_empty() && request.chain.contains(&our_peer_id_str) { ... }

// CORRECT — fail hard if peer ID is empty
if our_peer_id_str.is_empty() { fail_reply(...); return; }
if request.chain.contains(&our_peer_id_str) { fail_reply(...); return; }
```

### Step 3 — Verify the Session invariant

Check every code path that calls `p2p.send_session_message()` in response to an inbound event:

```text
[ ] run_session_executor: skips (continue) when message.role != SessionRole::User
[ ] execute_inbound_session_message: returns early when message.role != SessionRole::User
[ ] Both checks fire BEFORE the LLM runs, BEFORE the semaphore is acquired
[ ] Concurrency-limit error replies use role: Assistant (never role: User)
[ ] SendMessageTool always sends role: User — correct, it is the initiating side
```

The echo loop pattern to watch for:

```text
Node A: receives SessionMessage → sends reply (role: Assistant) to B
Node B: receives role: Assistant → runs LLM → sends reply to A   ← LOOP
```

The fix — `execute_inbound_session_message` must start with:

```rust
if message.role != sven_p2p::SessionRole::User {
    return;
}
```

### Step 4 — Check for cross-protocol blind spots

The Task and Session channels are independent; cycle tracking does not cross between them.

Scenarios to manually verify:

- A **task agent** using `send_message` → does the session response trigger a loop?
  - With Session invariant intact: the reply is `role: Assistant` → not auto-responded to → safe.
- A **session agent** using `delegate_task` → does the task chain have correct context?
  - Session agents are built with `depth=0, chain=[]` (`build_session_agent` in `node.rs`).
  - The Task invariant still bounds the downstream chain independently → safe.
- A **room post** triggering an agent that posts back → no auto-response executor exists for
  room posts (gossipsub messages are stored and emitted as `P2pEvent::RoomPost` but no
  auto-reply loop is wired up).

### Step 5 — Startup race audit

Any code that reads `p2p.local_peer_id_string()` lazily is vulnerable to returning `""` before
the P2P node's `OnceLock` is set. Find all call sites:

```bash
rg "local_peer_id_string" crates/
```

For each call site, confirm:

- Used in a security check (chain comparison, cycle detection) → must fail-hard if empty.
- Pushed into a chain or signed data → must fail-hard if empty.
- Used only for logging → empty is acceptable.

---

## Adding a New Message Channel

Apply this checklist when adding any new `P2pRequest` variant or auto-responder loop:

```text
[ ] Does the handler send a response? If no, no loop risk.
[ ] If yes: what field distinguishes "request" (needs reply) from "response" (terminal)?
[ ] Is that field checked BEFORE the LLM or any expensive work runs?
[ ] Does the response carry the terminal signal (so the receiver does not auto-reply)?
[ ] If multi-hop: is there a monotonic bounded counter on the wire type?
[ ] Is that counter validated before processing, not only before forwarding?
[ ] Is the counter immune to startup races (concrete value, not a lazy string)?
```

---

## Key Constants and Files

| Symbol | Location | Purpose |
| ------ | -------- | ------- |
| `MAX_DELEGATION_DEPTH` | `crates/sven-node/src/tools.rs` | Hard cap on task hops |
| `MAX_CONCURRENT_TASKS` | `crates/sven-node/src/node.rs` | Concurrency semaphore |
| `SessionRole::User` / `::Assistant` | `crates/sven-p2p/src/protocol/types.rs` | Session invariant signal |
| `TaskRequest::depth` / `::chain` | `crates/sven-p2p/src/protocol/types.rs` | Task invariant fields |
| `P2pHandle::local_peer_id_string()` | `crates/sven-p2p/src/node.rs` | Returns `""` until OnceLock set |

---

## Loop Bug Report Template

When filing a loop bug, capture:

```text
Channel:           Task / Session / Room / Other
Direction:         A→B→A (2-node) / A→B→C→A (3-node) / fan-out
Trigger:           What message or tool call initiates the chain
Missing invariant: Which of the two invariants is absent or bypassed
Startup race:      Yes / No — does the bug only appear during node startup?
Files:             List of files and line ranges involved
Fix:               Add role check / fix depth check / guard empty peer ID / other
```
