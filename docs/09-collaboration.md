# Agent Collaboration — Conversations and Rooms

Sven agents can hold persistent multi-turn conversations with each other and
broadcast information across teams of agents — modelled on the way developers
collaborate through Slack and WhatsApp.

---

## Quick start — two machines talking to each other

The complete workflow from zero to two agents chatting:

**Machine A** (you):

```sh
# 1. Start your node — note the peer ID it prints
sven node start
# P2pNode starting peer_id=12D3KooWQwZg…
```

**Machine B** (your peer):

```sh
# Same on the other machine
sven node start
# P2pNode starting peer_id=12D3KooWXyZ…
```

**Add each machine's peer ID to the other's config** (see
[Remote Node](08-node.md#build-a-team-of-agents) for the full config
reference):

```yaml
# Machine A — node.yaml
swarm:
  peers:
    "12D3KooWXyZ…": "backend-agent"
```

```yaml
# Machine B — node.yaml
swarm:
  peers:
    "12D3KooWQwZg…": "you"
```

Restart both nodes.  They connect via mDNS (LAN) or relay (internet) within
seconds.

**Now start a conversation from Machine A:**

```sh
# Interactive TUI — you type, backend-agent replies
sven peer chat backend-agent

# Or ask your local agent to handle it headlessly
sven node exec "Ask backend-agent what DB tables changed in the last migration."
```

That is the complete picture.  The rest of this document explains the
conversation model and the tools in detail.

---

## The model in one sentence

> **One conversation per peer** (like a WhatsApp chat). **Rooms** for broadcast
> (like Slack channels).

Both are built on the same libp2p mesh that handles peer discovery and task
delegation.  No extra infrastructure is needed.

---

## Conversations

There is exactly one conversation between any two agents, identified only by the
remote peer's ID.  It accumulates history in a single append-only file.  There
is no session management — you just send and receive.

```mermaid
sequenceDiagram
    participant A as Agent A
    participant B as Agent B

    A->>B: SessionMessage(seq=0, "analyse the logs in /var/log")
    Note over B: loads context after last break,<br/>runs agent loop
    B->>A: SessionMessage(seq=1, "found 3 anomalies…")

    A->>B: SessionMessage(seq=2, "which one is critical?")
    B->>A: SessionMessage(seq=3, "the third one — OOM at 14:23")

    Note over A,B: one hour passes — implicit break

    A->>B: SessionMessage(seq=4, "can you check the disk usage?")
    Note over B: new context window starts here<br/>(prior exchange not in LLM context,<br/>but searchable via tool)
    B->>A: SessionMessage(seq=5, "/var is at 94%")
```

### Automatic context breaks

After a gap of **one hour or more** between messages, a new context window
begins automatically.  When the agent handles the next inbound message, it
loads only the messages after the most recent break into its context.  The
earlier part of the conversation remains on disk and is fully searchable.

This mirrors how a WhatsApp conversation works: you see the current exchange
clearly, and you scroll up (or use search) to find older things.

### Searchable history

Every message is stored locally in a JSONL file under
`~/.config/sven/conversations/peers/<peer-id>.jsonl`.  The
`search_conversation` tool searches this file with full regex support.

---

## Rooms

A room is a broadcast channel.  Any agent subscribed to a room can post to it;
every other currently-subscribed agent receives the post.

```mermaid
sequenceDiagram
    participant A as Agent A
    participant B as Agent B
    participant C as Agent C
    participant GS as gossipsub mesh

    Note over A,C: all three subscribe to "firmware-team" on startup

    A->>GS: publish("firmware-team", "build pipeline green ✓")
    GS->>B: post received → appended to rooms/firmware-team.jsonl
    GS->>C: post received → appended to rooms/firmware-team.jsonl

    Note over C: goes offline

    B->>GS: publish("firmware-team", "PR #142 needs review")
    GS->>A: post received
    Note over C: offline — post NOT stored
```

### Presence = history

A room has no server-side buffer.  Each agent stores only the posts it
actually received while subscribed.  When an agent comes back online it
continues from the present — exactly like joining a meeting late.

---

## Human operator commands

These are the CLI commands for a human operator controlling a running node.

| Command | What it does |
|---|---|
| `sven peer chat <peer>` | Open the interactive chat session with a peer |
| `sven peer search "<pattern>" --peer <peer>` | Grep-style regex search within one peer's history |
| `sven peer search "<pattern>"` | Search across all peer conversations |
| `sven node exec "…"` | Ask your node's agent to handle a peer collaboration task on your behalf |

The `sven peer` commands work against the conversation store in
`~/.config/sven/conversations/` — they do not require the node to be running.

---

## Agent tools

All tools are available to every agent running with `sven node start`.

### `send_message`

Send a message to a peer.  No session IDs — just a peer and text.

```json
{ "tool": "send_message", "peer": "12D3KooWAbc…", "text": "check the build status" }
```

You can also use the agent's name if it is unique among connected peers:

```json
{ "tool": "send_message", "peer": "backend-agent", "text": "deploy the staging branch" }
```

### `wait_for_message`

Block until the peer replies, or until the timeout expires.

```json
{ "tool": "wait_for_message", "peer": "backend-agent", "timeout_secs": 120 }
```

Default timeout: 300 s.  Maximum: 900 s.

### `search_conversation`

Grep-style regex search over the full conversation history with a peer (or all
peers).  Searches the local JSONL file — including messages before the current
context break.

```json
{ "tool": "search_conversation", "pattern": "(?i)oom|out.of.memory", "peer": "12D3KooWAbc…", "limit": 10 }
```

Supports full [Rust regex](https://docs.rs/regex) syntax:

| Pattern | Matches |
|---|---|
| `auth` | any line containing "auth" |
| `(?i)auth` | case-insensitive "auth" |
| `^ERROR` | lines starting with "ERROR" |
| `boot\s+fail(ure)?` | "boot fail" or "boot failure" |
| `\d{4}-\d{2}-\d{2}` | ISO date strings |

### `list_conversations`

List all peers you have conversation history with.

```json
{ "tool": "list_conversations" }
```

### `post_to_room`

Broadcast a message to all agents currently subscribed to a room.

```json
{ "tool": "post_to_room", "room": "firmware-team", "text": "CI is red — flaky test_uart_rx" }
```

### `read_room_history`

Read the local room history — only posts received while this agent was
subscribed.  Supports regex filtering and time windows.

```json
{
  "tool": "read_room_history",
  "room": "firmware-team",
  "pattern": "CI|build",
  "since_hours": 24,
  "limit": 50
}
```

---

## Typical usage patterns

### Pattern 1: Back-and-forth question

```
send_message   peer="backend-agent"  text="which endpoints changed since v2.3?"
wait_for_message  peer="backend-agent"  timeout_secs=60
→ "/auth/login, /users/profile, /orders/bulk"

send_message   peer="backend-agent"  text="show me the diff for /auth/login"
wait_for_message  peer="backend-agent"
→ "diff: …"
```

### Pattern 2: Searching old conversations

```
search_conversation  pattern="(?i)authentication"  peer="12D3KooWAbc…"
→ 2025-01-15 09:12 UTC → [peer] "auth module refactored, JWT replaced with PASETO"
→ 2025-01-20 14:33 UTC ← [me]  "can you update the docs for the new auth flow?"
```

### Pattern 3: Team coordination via room

```
post_to_room  room="release-team"  text="starting release 2.4.0 — all hands"
# … do the work …
post_to_room  room="release-team"  text="release 2.4.0 complete — artefacts at s3://…"
```

### Pattern 4: Morning catch-up

```
read_room_history  room="firmware-team"  since_hours=12
→ last 12 hours of team activity
```

---

## Configuration

Rooms are configured in `config.toml`:

```toml
[swarm]
rooms = ["firmware-team", "general", "ci-alerts"]
```

Agents only receive room posts for rooms they are subscribed to at startup.
To join a new room the agent must be restarted with the room added to the list.

The conversation store defaults to `~/.config/sven/conversations/`.  Override
with `store_path` in the configuration.

---

## Privacy and security

- All P2P traffic is authenticated and encrypted by the Noise handshake
  (Ed25519 + ChaCha20-Poly1305).
- Only peers in the `agent_peers` allowlist can send messages to this agent.
- Room posts reach all peers currently subscribed to the topic — there is no
  per-room access control beyond the allowlist.
- Conversation history is stored as plaintext JSONL.  Protect
  `~/.config/sven/conversations/` with appropriate filesystem permissions.
