---
name: p2p-loop-analysis
description: "Analyse the sven P2P agent network for infinite message loops (echo loops, delegation storms, circular routing). Use when investigating unexpected runaway traffic between agents, infinite back-and-forth between nodes, task chains that never terminate, or when adding a new message channel/handler and needing to verify it cannot loop. Covers all three channels: Task (delegate_task), Session (send_message), and Room (post_to_room)."
---

# P2P Loop Analysis — sven

## The Three Invariants

Every message path in sven must satisfy one of three loop-breaking invariants.
If any invariant is missing or broken, an infinite loop is possible.

| Channel | Wire type | Primary invariant | Enforced in |
| ------- | --------- | ----------------- | ----------- |
| **Task** | `TaskRequest` | `depth` strictly increases; bounded by `MAX_HOP_DEPTH`; peer-ID `chain` detects cycles | `execute_inbound_task`, `DelegateTool::execute` |
| **Session** | `SessionMessageWire` | Auto-reply only to `role == User`; `depth` bounded by `MAX_HOP_DEPTH` (secondary) | `run_session_executor`, `execute_inbound_session_message` |
| **Room** | `RoomPost` | `depth` checked before store/emit; bounded by `MAX_ROOM_POST_DEPTH` | `on_gossipsub_message` in `sven-p2p` |

A loop requires an infinite message sequence. All invariants make that impossible:

- **Task**: depth increases by 1 per hop → reaches `MAX_HOP_DEPTH` → rejected before LLM runs.
- **Session**: every auto-reply carries `role: Assistant` → `Assistant` is never auto-replied to. Secondary depth guard catches tool-initiated ping-pong.
- **Room**: posts at depth ≥ `MAX_ROOM_POST_DEPTH` are dropped before emit.

## Unified Hop Budget

All three channels share **one constant**: `MAX_HOP_DEPTH = 4` in `sven-node/src/tools.rs`
(must equal `MAX_ROOM_POST_DEPTH = 4` in `sven-p2p/src/protocol/types.rs`).

When an agent **switches protocols**, the accumulated depth carries forward:

- A session agent at depth D is built with `task_depth = D` in `DelegationContext` AND
  `default_depth = D` in `SessionDepthTracker`. Any outbound task or session message
  continues from D, not from 0.
- A task agent at depth D is built with `session_depth.default_depth = D`. Its first
  `send_message` sends at depth D+1, not at 1.

This means the combined chain across any number of protocol switches cannot exceed `MAX_HOP_DEPTH`.

---

## Analysis Workflow

### Step 1 — Map all message handlers

Read these files in order:

```text
crates/sven-p2p/src/protocol/types.rs     — wire types; MAX_ROOM_POST_DEPTH
crates/sven-node/src/tools.rs             — MAX_HOP_DEPTH, SessionDepthTracker, all tool impls
crates/sven-node/src/agent_builder.rs     — how depth handles are initialised per agent type
crates/sven-node/src/node.rs              — run_task_executor, run_session_executor,
                                            execute_inbound_task, execute_inbound_session_message,
                                            build_session_agent
crates/sven-p2p/src/node.rs               — on_gossipsub_message (room depth guard)
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
[ ] MAX_HOP_DEPTH is used (not a stale MAX_DELEGATION_DEPTH reference)
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
[ ] SendMessageTool depth check uses per-peer tracker, not a global counter
[ ] WaitForMessageTool stores received depth to tracker.per_peer[peer], not globally
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

### Step 4 — Verify the Room invariant

Check every code path in `on_gossipsub_message`:

```text
[ ] post.depth is checked BEFORE store and BEFORE emit
[ ] guard is: if post.depth >= MAX_ROOM_POST_DEPTH { return; }
[ ] PostToRoomTool reads room_depth handle and sends at room_depth + 1
[ ] PostToRoomTool refuses to send if outgoing_depth >= MAX_HOP_DEPTH
[ ] A future reactive room handler MUST set room_depth = incoming_post.depth
    before running the agent — otherwise every reactive post goes out at depth 1
    and the guard at MAX_ROOM_POST_DEPTH is never reached
```

### Step 5 — Check cross-protocol depth seeding

When an agent is constructed, verify that the depth handles are seeded correctly:

```text
[ ] build_node_agent:           session_depth.default_depth = 0
[ ] build_task_agent:              session_depth.default_depth = task_depth
[ ] build_task_agent_with_runtime: session_depth.default_depth = initial_session_depth
[ ] build_session_agent (node.rs): calls build_task_agent_with_runtime(
                                       task_depth      = session_depth,  ← not 0
                                       initial_session = session_depth)
[ ] All three: room_depth = Arc::new(AtomicU32::new(0))
```

### Step 6 — Startup race audit

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
[ ] Does the agent builder seed its depth handle from the incoming message's depth?
    (For new reactive handlers, the depth must carry forward, not restart at 0.)
```

---

## Key Constants and Files

| Symbol | Location | Purpose |
| ------ | -------- | ------- |
| `MAX_HOP_DEPTH` | `crates/sven-node/src/tools.rs` | Unified cap — all channels share this budget |
| `MAX_ROOM_POST_DEPTH` | `crates/sven-p2p/src/protocol/types.rs` | Must equal `MAX_HOP_DEPTH`; enforced in `on_gossipsub_message` |
| `MAX_CONCURRENT_TASKS` | `crates/sven-node/src/node.rs` | Concurrency semaphore (separate from depth) |
| `SessionDepthTracker` | `crates/sven-node/src/tools.rs` | Per-peer session depth; `default_depth` seeds cross-protocol budget |
| `RoomDepthHandle` | `crates/sven-node/src/tools.rs` | Room post depth; set to incoming depth before reactive agent runs |
| `SessionRole::User` / `::Assistant` | `crates/sven-p2p/src/protocol/types.rs` | Session invariant signal |
| `TaskRequest::depth` / `::chain` | `crates/sven-p2p/src/protocol/types.rs` | Task invariant fields |
| `P2pHandle::local_peer_id_string()` | `crates/sven-p2p/src/node.rs` | Returns `""` until OnceLock set |

---

## Loop Bug Report Template

When filing a loop bug, capture:

```text
Channel:           Task / Session / Room / Other
Direction:         A→B→A (2-node) / A→B→C→A (3-node) / fan-out / cross-protocol
Trigger:           What message or tool call initiates the chain
Missing invariant: Which of the three invariants is absent or bypassed
Cross-protocol:    Yes / No — does the loop require switching between Task/Session/Room?
Startup race:      Yes / No — does the bug only appear during node startup?
Files:             List of files and line ranges involved
Fix:               Add role check / fix depth check / guard empty peer ID /
                   seed depth handle correctly / other
```
