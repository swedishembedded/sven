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

**TLS provisioning tiers** — the HTTP endpoint uses TLS 1.3 with ECDSA P-256.
The provisioning strategy is selected by `http.tls_mode` and defaults to
`auto`:

1. **Tailscale** (`tailscale cert`): calls the Tailscale CLI to fetch a real
   Let's Encrypt certificate for the machine's `*.ts.net` hostname.  Trust
   comes from the public Web PKI — no user setup required.
2. **Local CA** (`local-ca`): `rcgen` generates a 10-year ECDSA CA cert on
   first run (`ca-cert.pem`) and signs 90-day server certs with it.  The CA
   cert is stable across server-cert rotations; users install it once with
   `sven node install-ca`.  The CA key (`ca-key.pem`, `0o600`) is the only
   persistent secret.  On each run the CA `Certificate` object is reconstructed
   in-memory from the stored key and a fixed DN — `rcgen 0.13` does not have a
   `from_ca_cert_pem` API, but the trust chain holds because the public key
   (and therefore AKI/SKI) is identical.
3. **Self-signed**: pure self-signed cert.  Fingerprint printed at startup for
   TOFU pinning by native clients.
4. **Files**: user-supplied cert/key from `tls_cert_dir`.

`insecure_dev_mode` drops TLS entirely and is intentionally named to make it
uncomfortable to leave enabled.

**Separate keys for separate concerns** — the operator keypair, agent task
keypair, and TLS CA key are all stored separately so they can be rotated
independently and so a compromise of one does not affect the others.

---

## Web terminal (PTY over WebSocket)

The optional `/web` endpoint provides a browser-based terminal backed by a
server-side PTY.  Key design points:

**Authentication** uses [WebAuthn](https://webauthn.guide/) passkeys (FIDO2
resident keys / platform authenticators).  Credentials are device-bound
biometrics (Touch ID, Face ID, Windows Hello, Android fingerprint) — no
passwords are stored or transmitted.  First-time devices register a passkey and
enter `pending` state; an admin approves them via `sven node web-devices approve`
over the existing bearer-token WebSocket — no restart needed.

**PTY streaming** uses a binary WebSocket frame protocol:
- `0x00` prefix + raw bytes → PTY stdout/stderr to the browser (xterm.js)
- `0x01` prefix + JSON → control messages: `{"type":"resize","cols":N,"rows":M}`

**Session persistence** via tmux: the default PTY command is
`tmux new-session -A -s sven-{id}`.  `-A` reattaches an existing session of
that name, so closing and reopening the browser tab reconnects to the running
session rather than starting a fresh shell.

**`SessionSpawner` trait** abstracts PTY creation so local (fork) and
containerized (Docker / systemd-nspawn) implementations are interchangeable.
The local spawner uses `portable-pty` to fork a child process directly.

**SSE approval push**: a pending browser polls a Server-Sent Events stream.
When an admin approves the device, the SSE stream fires a `device-approved`
event and the browser transitions to the terminal without polling or page
refresh.

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
