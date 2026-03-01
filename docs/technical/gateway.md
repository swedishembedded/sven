# Gateway — Architecture

This document explains the design concepts behind `sven-gateway` and `sven-p2p`:
why components are structured the way they are, and what trade-offs drove the
key decisions.

For configuration, commands, and usage see [../08-gateway.md](../08-gateway.md).

---

## Two separate P2P channels

The gateway runs two independent libp2p swarms with different protocols,
different keypairs, and different trust models:

**Operator control channel** — connects human operators (mobile apps, CLI
clients) to the agent.  Peers must be explicitly paired before they can send
any commands.  The protocol is a simple request/poll model: the operator sends
a command and collects any buffered events on the response.

**Agent task channel** — connects this agent to peer agents for task routing.
Peers announce themselves on connection and are added to a local roster.  Any
peer in the roster can send a task; the task runs through the full agent loop
and the result is sent back.

Keeping these two channels separate means operator trust (who can control the
agent) is entirely independent of agent-network trust (who this agent
collaborates with).  An operator key being compromised does not affect the
agent network, and vice versa.

---

## Single owner of the agent

All transports — P2P operator channel, HTTP/WebSocket, Slack — share a single
`ControlService` that owns the agent.  They communicate with it through a
cheap-to-clone handle backed by an mpsc channel for commands and a broadcast
channel for events.

This means the agent is never accessed concurrently.  Each session runs
sequentially inside `ControlService`; the transports just fan in commands and
fan out events.  Adding a new transport requires only implementing the
send/subscribe interface — the agent itself does not change.

---

## Inbound task execution

When a remote agent sends a task, the gateway runs it through the local agent
the same way a human operator would: it creates a session, submits the task
text as input, waits for the session to complete, and sends back the final
response.

The remote agent blocks waiting for the `TaskResult`.  This synchronous
request/response pattern (provided by libp2p's `request_response` behaviour)
keeps client code simple — send a task, await a response — without needing
streaming or polling.

The one subtlety is that libp2p's `request_response` requires the response to
be sent from inside the swarm event loop, but the agent runs asynchronously
outside it.  The solution is to park the response channel inside the event
loop when the request arrives, run the agent in a separate task, and send the
result back to the event loop via a command channel when done.

---

## Peer discovery

On a local network, mDNS handles discovery automatically — no configuration
needed.  Peers announce themselves, join the same room, and start routing
tasks within a few seconds.

For cross-network connectivity, a relay server acts as a rendezvous point.
Peers connect to the relay, register a circuit address, and discover each
other through it.  Once discovered, DCUtR (hole-punching) upgrades the
connection to a direct peer-to-peer link where possible, falling back to the
relayed path otherwise.

The relay is optional.  If no relays are configured the node starts in
mDNS-only mode.

---

## Security design

**Deny-all by default** — the allowlist starts empty.  No peer can send
operator commands until explicitly paired.  This is an opt-in model: the user
must take a positive action to authorise each device.

**Authentication without a server** — libp2p's Noise handshake verifies every
peer's Ed25519 identity before any application data is exchanged.  By the time
the application code sees a connection, the peer's identity is
cryptographically established.  Authorization (is this peer in the allowlist?)
is then a simple map lookup.

**TLS on by default** — the HTTP endpoint uses a self-signed ECDSA certificate
generated on first run.  `insecure_dev_mode` exists for local development but
is designed to be uncomfortable to leave enabled.

**Separate keys for separate concerns** — the operator keypair and the agent
task keypair are stored separately so they can be rotated independently and so
a compromise of one does not affect the other.

---

## Wire encoding

Both P2P protocols use CBOR (Concise Binary Object Representation) with a
4-byte big-endian length prefix.  CBOR was chosen over JSON for compactness
(task payloads can include binary image data) and over protobuf for simplicity
(no separate schema compilation step, Rust serde support via `ciborium`).

The operator control protocol uses a poll model: the operator sends a request
and receives any buffered events in the response.  This keeps the protocol
stateless from the server's perspective and works cleanly over the
`request_response` behaviour without needing bidirectional streaming.

The agent task protocol uses a true request/response: send a `TaskRequest`,
receive a `TaskResult` when the remote agent finishes.  The long timeout
(15 minutes) accommodates tasks that involve many tool calls.
