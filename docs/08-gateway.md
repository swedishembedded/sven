# Remote Gateway

`sven node start` starts the agent together with everything it needs to be
reachable remotely and to work alongside other agents.

**The gateway is the agent.** It runs the language model, all tools, and a P2P
stack in a single process.  There is no separate "agent" to start.

It gives you two things at once:

- **Operator access** — control the agent from a mobile app, Slack, or any
  browser over a secure HTTPS + P2P channel.
- **Agent networking** — the agent automatically finds other sven agents on the
  same network (or via a relay) and can delegate work to them.

---

## Quick start (5 minutes)

### 1. Start the agent

```sh
sven node start
```

On first run sven generates a TLS certificate, a bearer token (printed once —
save it), and a cryptographic identity for P2P.

```
0.003s  INFO  =======================================================
0.003s  INFO  HTTP bearer token (shown once — save it now!):
0.003s  INFO    eyJ0eXAiOiJKV1QiLCJhbGc...
0.003s  INFO    export SVEN_NODE_TOKEN=eyJ0eXAiOiJKV1QiLCJhbGc...
0.003s  INFO  =======================================================
0.005s  INFO  No P2P operator devices paired yet (optional).
0.008s  INFO  starting HTTP node bind=127.0.0.1:18790 tls=true
```

The peer ID printed in the logs is this agent's P2P identity on the **operator
control channel**.  It is used only for pairing human operator devices — it has
nothing to do with agent-to-agent connectivity (that is automatic).

### 2. Connect as an operator

There are **two ways** to send commands to a running node.  They are completely
independent — use whichever fits your setup.

#### Option A: HTTP bearer token (recommended for CLI use)

This is the primary path and requires no extra setup:

```sh
export SVEN_NODE_TOKEN=<token-shown-at-first-startup>
sven node exec "write a hello world in Rust"
```

The token was printed once when the node started.  If you lost it, rotate it:

```sh
sven node regenerate-token
```

#### Option B: P2P operator channel (for native/mobile clients)

This path is for native applications (e.g. a mobile app) that connect via
libp2p rather than HTTP.  It uses `sven node authorize` to add a device to
the allowlist.

> **This has nothing to do with agent-to-agent connections.**
> Node-to-node connections happen automatically via mDNS or relay — there is
> no command to run and no pairing needed.

The operator device displays a `sven://` URI.  Paste it:

```sh
sven node authorize "sven://12D3KooWAbCdEfGhIjKlMnOpQrStUvWxYz"
```

sven shows the peer ID and a short fingerprint for visual confirmation, then
asks `[y/N]` before writing to the allowlist.

> **`list-operators` vs `list_peers` (agent tool) — don't confuse them:**
>
> - `sven node list-operators` — human operator devices added with
>   `sven node authorize`. These send commands to the agent.
> - The `list_peers` **agent tool** — other sven nodes that found each other
>   via mDNS or relay. These receive delegated tasks.
>
> A device in `list-operators` cannot receive delegated tasks.

The allowlist is saved to `~/.config/sven/gateway/authorized_peers.yaml`.  You
can also edit it by hand — useful for pre-provisioning or revoking access
without running the CLI:

```yaml
# ~/.config/sven/gateway/authorized_peers.yaml

operators:
  # peer_id (base58): human-readable label
  "12D3KooWAbCdEfGhIjKlMnOpQrStUvWxYz": "my-phone"
  "12D3KooWXyZaBcDeFgHiJkLmNo12345678": "work-laptop"

observers:
  # observers can read output but cannot send input or approve tools
  "12D3KooW11223344556677889900aAbBcC": "ci-runner"
```

The file is reloaded automatically on change — no restart needed.

### 3. Build a team of agents

Agent-to-agent connections use a **deny-all allowlist** — each node must
explicitly list the peer IDs it is willing to connect to.

**Step 1 — Start each node and note its agent peer ID.**

The agent peer ID is printed on startup:

```
P2pNode starting peer_id=12D3KooWQwZgQPdd4TZeputvRdmaoq2whU358qYULJMiNGvJcB98
```

> **Note:** there are two peer IDs in the log.  Use the one from the
> `P2pNode starting` line — that is the agent mesh identity.  The other
> (`P2P control node identity`) is for the operator control channel.

**Step 2 — Add each node's peer ID to the other's config.**

```yaml
# machine A — .gateway.yaml
swarm:
  peers:
    "12D3KooW<machine-B-agent-peer-id>": "machine-b"
```

```yaml
# machine B — .gateway.yaml
swarm:
  peers:
    "12D3KooW<machine-A-agent-peer-id>": "machine-a"
```

Authorization is not automatic — both sides must list each other.

**Step 3 — (Re)start both nodes.**  They will connect within seconds via mDNS.

To give each agent a distinct identity, also set their names in the config:

```yaml
# machine A — .gateway.yaml
swarm:
  agent:
    name: "backend-agent"
    description: "Rust and PostgreSQL specialist"
    capabilities: ["rust", "postgres"]

# machine B — .gateway.yaml
swarm:
  agent:
    name: "frontend-agent"
    description: "React and TypeScript specialist"
    capabilities: ["typescript", "react"]
```

---

## Talking to peers

There are two ways to interact with peer agents: **interactive peer chat** (a
direct back-and-forth conversation with one remote peer) and **orchestrated
delegation** (your local agent manages the collaboration on your behalf).

### Interactive peer chat

`sven peer chat` opens the TUI connected to a specific remote peer.  Every
message you type is sent as a conversation message over P2P, and the remote
agent's replies stream back into the chat pane — exactly like a WhatsApp
conversation, except the other party is a sven agent.

```sh
# By agent name (must be unique among connected peers)
sven peer chat backend-agent

# By peer ID (unambiguous, always works)
sven peer chat 12D3KooWAbCdEfGhIjKlMnOpQrStUvWxYz
```

The conversation is stored locally in
`~/.config/sven/conversations/peers/<peer-id>.jsonl` so you can search it
later:

```sh
# Grep through everything you've discussed with this peer
sven peer search "authentication" --peer backend-agent
sven peer search "^ERROR" --peer backend-agent
sven peer search "(?i)out.of.memory" --peer backend-agent

# Search across all peer conversations
sven peer search "^ERROR"
```

The remote agent automatically loads the conversation history up to the most
recent 1-hour break as context.  Older history stays on disk and is accessible
via the `search_conversation` tool on both sides.

Both nodes must list each other in their `swarm.peers` config (see *Build a
team of agents* above).

### Asking the node to handle peer collaboration

When you want the local agent to manage a multi-agent workflow on your behalf —
including the back-and-forth with one or more peers — use `sven node exec`:

```sh
sven node exec "Chat with backend-agent about the DB migration plan.
Ask whether the foreign-key constraints should be deferred.
Wait for its answer, then ask for the migration SQL.
Summarise what it said."
```

The node's agent uses `send_message` and `wait_for_message` autonomously.  You
get the final summary when it is done.  This is the right approach when you
want the agent to coordinate multiple peers or when you do not need to steer
the conversation in real time.

### Which to use?

| Situation | Recommended approach |
|---|---|
| You want to type messages and see replies in real time | `sven peer chat <peer>` |
| You want the agent to coordinate a workflow across peers | `sven node exec "…"` |
| You want to recall what was discussed with a peer | `sven peer search "<pattern>" --peer <peer>` |
| You want to broadcast a status update to the whole team | `sven node exec "post to room firmware-team: …"` |

---

## Agent-to-agent task routing

When two gateways are connected, each agent gets two new tools it can use
autonomously during any session.

### `list_peers` — discover connected agents

```
2 peer(s) connected:

**backend-agent**
  Peer ID:      12D3KooWAbCdEfGhIjKlMnOpQrStUvWxYz
  Description:  Rust and PostgreSQL specialist
  Capabilities: rust, postgres, api-design

**frontend-agent**
  Peer ID:      12D3KooWXyZaBcDeFgHiJkLmNo12345678
  Description:  React and TypeScript specialist
  Capabilities: typescript, react, css
```

### `delegate_task` — send work to a peer

The agent names the peer and describes the task.  The remote agent runs it
through its own model+tool loop and returns the full result — the calling agent
sees it as a regular tool response and can keep reasoning with it.

### How to prompt for delegation

```
You are the orchestrator for a small team.  Use list_peers to find who is
online, then:
1. Delegate the database migration to the backend-agent.
2. Delegate the UI changes to the frontend-agent.
3. Summarise what each agent did.
```

sven handles the rest: it calls `list_peers`, picks the right peers, calls
`delegate_task` for each, and assembles the results.

---

## HTTPS / TLS

TLS is **on by default**.  Three provisioning modes are available, controlled
by `http.tls_mode`.  The default (`auto`) tries them in order:

### Mode 1 — Tailscale (zero setup, browser-trusted)

If [Tailscale](https://tailscale.com) is installed and the machine is enrolled
in a tailnet, sven calls `tailscale cert` automatically to obtain a real
Let's Encrypt certificate for your `<machine>.ts.net` hostname.  The certificate
is issued by Let's Encrypt and trusted by every browser with no additional steps.

```
INFO  HTTPS gateway listening mode="Tailscale (mybox.example.ts.net)"
```

Access the node at `https://mybox.example.ts.net:18790`.

This is the recommended setup for LAN machines — Tailscale is free, provides
end-to-end encrypted access from anywhere, and the certs just work.

### Mode 2 — Local CA (trust once per device)

When Tailscale is not available, sven generates a local ECDSA P-256 Certificate
Authority (10-year validity) and signs a 90-day server certificate with it.
The CA cert lives at `~/.config/sven/gateway/tls/ca-cert.pem`.

Install the CA on each device that will access the node:

```sh
sven node install-ca          # prints platform-specific commands
```

For example, on Linux:

```sh
sudo cp ~/.config/sven/gateway/tls/ca-cert.pem \
        /usr/local/share/ca-certificates/sven-ca.crt
sudo update-ca-certificates
```

After that one-time step, every future 90-day rotation is completely transparent
— no browser interaction ever again.  The same CA cert is reused across
rotations, so you only install it once.

To distribute the CA cert to a phone:

```sh
sven node export-ca > ca.pem
python3 -m http.server 8080     # serve it; open http://<ip>:8080/ca.pem on the phone
```

### Mode 3 — Self-signed (fingerprint pinning)

The browser shows a warning on every new cert rotation.  You can accept the
warning once (click through "Advanced → Proceed"), or pin the fingerprint
printed at startup in any native client that supports TOFU.

Explicitly opt in:

```yaml
http:
  tls_mode: self-signed
```

### Mode 4 — Your own certificates

```yaml
http:
  tls_mode: files
  tls_cert_dir: "/etc/sven/tls"    # must contain gateway-cert.pem + gateway-key.pem
```

Bring your own certs from any ACME client, internal PKI, or Let's Encrypt
with a DNS-01 challenge.

---

### Making HTTPS work from LAN IPs

Generated certificates include `localhost` and `127.0.0.1` by default.  To
also cover your LAN IP or hostname, add it to `tls_san_extra`:

```yaml
http:
  bind: "0.0.0.0:18790"
  tls_san_extra:
    - "192.168.1.42"
    - "mybox.local"
```

The next cert rotation (or a cert delete + restart) picks up the new SANs.

---

## Security defaults

Everything is secure out of the box.  These defaults are hardcoded and cannot
be weakened by accident:

| What | Default |
|------|---------|
| HTTP TLS | On — ECDSA P-256, 90-day auto-generated cert |
| TLS provisioning mode | `auto` — Tailscale if available, else local CA |
| TLS version | TLS 1.3 only |
| P2P encryption | Noise protocol (Ed25519), always on |
| Agent mesh authorisation | Deny-all — every agent peer must be in `swarm.peers` |
| Operator control node | **Disabled** by default — add `control:` section to enable |
| Control node bind | `127.0.0.1` — loopback only by default |
| HTTP binding | `127.0.0.1` — loopback only |
| Rate limiting | 5 failures/min locks out the source for 60 s |
| Bearer token storage | SHA-256 hash only — plaintext never written to disk |
| Secret file permissions | `0o600` on Unix |
| Task timeout | 15 minutes per inbound delegated task |

To expose the gateway beyond loopback, set `http.bind: "0.0.0.0:18790"` in
your config.  `bind` must be an address actually assigned to a local interface
— binding to an IP the machine does not own causes an immediate startup error
with a clear message explaining the fix.

---

## Configuration

The gateway config is YAML, merged in order from:

1. `/etc/sven/gateway.yaml`
2. `~/.config/sven/gateway.yaml`
3. `.sven/gateway.yaml`
4. Path given with `--config`

### Minimal example

```yaml
http:
  bind: "127.0.0.1:18790"

swarm:
  keypair_path: "~/.config/sven/gateway/agent-keypair"
```

### LAN / mobile access example

```yaml
http:
  bind: "0.0.0.0:18790"        # listen on all interfaces
  tls_san_extra:
    - "192.168.1.42"            # your LAN IP — added to the cert's SAN list
  # tls_mode defaults to "auto": uses Tailscale if present, else local CA

web:                            # browser web terminal (optional)
  rp_id: "192.168.1.42"        # must match the hostname/IP in the browser bar
  rp_origin: "https://192.168.1.42:18790"

swarm:
  keypair_path: "~/.config/sven/gateway/agent-keypair"
```

### Full example

```yaml
http:
  bind: "0.0.0.0:18790"
  insecure_dev_mode: false  # only set true for local development
  tls_mode: auto            # tailscale → local-ca → (or set tailscale/local-ca/self-signed/files)
  tls_san_extra: []         # extra IPs/hostnames to include in generated cert SANs
  tls_cert_dir: "~/.config/sven/gateway/tls"
  token_file: "~/.config/sven/gateway/token.yaml"

# Agent-to-agent mesh
swarm:
  listen: "/ip4/0.0.0.0/tcp/4010"  # fixed port — open this in your firewall
  keypair_path: "~/.config/sven/gateway/agent-keypair"

  # Identity this agent shows to other agents
  agent:
    name: "backend-agent"
    description: "Rust and PostgreSQL specialist"
    capabilities: ["rust", "postgres", "api-design"]

  # Rooms group agents together for discovery
  rooms: ["default", "team-alpha"]

  # Agent peers allowed to join this node's mesh (deny-all if omitted).
  # Use the peer_id from the other node's "P2pNode starting peer_id=…" log line.
  peers:
    "12D3KooWXyZaBcDeFgHiJkLmNo12345678": "frontend-agent"
    "12D3KooW11223344556677889900aAbBcC": "devops-agent"

# Operator control node — omit this entire section to disable native/mobile access
# control:
#   listen: "/ip4/0.0.0.0/tcp/4009"  # open in firewall only if needed
#   keypair_path: "~/.config/sven/gateway/control-keypair"
#   authorized_peers_file: "~/.config/sven/gateway/authorized_peers.yaml"

slack:
  accounts:
    - mode: socket      # outbound WebSocket, no inbound port needed
      app_token: "xapp-..."
      bot_token: "xoxb-..."
```

### All config keys

#### `http`

| Key | Default | Description |
|-----|---------|-------------|
| `bind` | `127.0.0.1:18790` | Address and port to listen on.  Use `0.0.0.0:18790` to listen on all interfaces.  Must be an IP the machine actually owns — binding to a non-local address causes an immediate startup error. |
| `insecure_dev_mode` | `false` | Disable TLS entirely — for local development only |
| `tls_mode` | `auto` | TLS provisioning: `auto`, `tailscale`, `local-ca`, `self-signed`, or `files` |
| `tls_san_extra` | `[]` | Extra hostnames or IPs to add to generated cert SANs (e.g. your LAN IP) |
| `tls_cert_dir` | `~/.config/sven/gateway/tls` | Where to store / load certificates |
| `token_file` | `~/.config/sven/gateway/token.yaml` | Hashed bearer token storage |
| `max_body_bytes` | `4194304` | Max request body size (4 MiB) |

#### `web` *(optional — disabled by default)*

Browser-based web terminal served at `/web`.  Authentication uses WebAuthn
passkeys (biometric / platform authenticator).  New devices are held in
`pending` state until approved with `sven node web-devices approve`.

**WebAuthn requires HTTPS.**  `rp_id` and `rp_origin` must match the hostname
or IP address the browser uses to reach the node — if they don't match,
registration and login will be rejected by the browser.

| Key | Default | Description |
|-----|---------|-------------|
| `rp_id` | `localhost` | WebAuthn relying party ID — the hostname/IP in the browser bar |
| `rp_origin` | `https://localhost:18790` | Full origin shown in the browser address bar |
| `rp_name` | `Sven Node` | Human-readable name shown during passkey ceremony |
| `devices_file` | `~/.config/sven/gateway/web_devices.yaml` | Registered device registry |
| `session_ttl_secs` | `86400` | Session JWT lifetime (24 h) |
| `pty_command` | `["tmux", "new-session", "-A", "-s", "sven-{id}"]` | Command run in the PTY.  `{id}` is replaced with the first 8 chars of the device UUID. |

#### `swarm`

The agent-to-agent mesh.  Handles task delegation between sven nodes.

| Key | Default | Description |
|-----|---------|-------------|
| `listen` | `/ip4/0.0.0.0/tcp/0` (random) | Listen address for the agent mesh — **set a fixed port and open it in your firewall for cross-machine use** |
| `keypair_path` | `~/.config/sven/gateway/agent-keypair` | Persist the agent mesh keypair; ephemeral if unset |
| `agent.name` | system hostname | Name shown to peer agents |
| `agent.description` | `"General-purpose sven agent"` | Free-form description |
| `agent.capabilities` | `[]` | Tags other agents use to choose this agent |
| `rooms` | `["default"]` | Discovery namespaces; peers in the same room find each other |
| `peers` | `{}` (deny-all) | Agent peers allowed to join the mesh — maps peer ID → label. Both nodes must list each other. |

#### `control` *(optional — disabled by default)*

The operator control node.  Carries commands from native/mobile operator
clients.  **Omit this section entirely** to run without a control node.

The operator control channel is completely separate from the agent mesh and
has nothing to do with agent-to-agent task delegation.

| Key | Default | Description |
|-----|---------|-------------|
| `listen` | `/ip4/127.0.0.1/tcp/0` | Listen address — defaults to loopback only. Set to `/ip4/0.0.0.0/tcp/4009` (and open the port) to allow mobile/native clients |
| `keypair_path` | — (ephemeral) | Persist the operator control keypair; ephemeral if unset means operators must re-pair after restart |
| `authorized_peers_file` | `~/.config/sven/gateway/authorized_peers.yaml` | YAML file of authorized operator peer IDs |

#### `slack`

| Key | Default | Description |
|-----|---------|-------------|
| `accounts[].mode` | `socket` | `socket` (outbound) or `http` (inbound webhook) |
| `accounts[].app_token` | — | Slack app-level token (`xapp-…`), required for Socket Mode |
| `accounts[].bot_token` | — | Slack bot token (`xoxb-…`) |
| `accounts[].signing_secret` | — | Signing secret for HMAC verification, required for HTTP mode |
| `accounts[].webhook_path` | `/slack/events` | Path for incoming Slack events in HTTP mode |

---

## Commands

```sh
# Start the agent
sven node start [--config PATH]

# Send a task to the running agent and stream the response (primary CLI path)
export SVEN_NODE_TOKEN=<token-from-first-startup>
sven node exec "delegate a task to say hi to the frontend-agent"

# Authorize a mobile/native operator device (P2P path — paste the sven:// URI it shows)
sven node authorize "sven://12D3KooW..." [--label "my-phone"]

# Revoke an authorized device
sven node revoke 12D3KooW...

# List authorized operator devices (NOT the same as agent peers)
sven node list-operators [--config PATH]

# Rotate the HTTP bearer token
sven node regenerate-token [--config PATH]

# Print the resolved configuration
sven node show-config [--config PATH]
```

### TLS certificate commands

```sh
# Print platform-specific instructions to trust the local CA (run once per device)
sven node install-ca [--config PATH]

# Print the local CA certificate PEM to stdout (pipe it to a phone, bundle it, etc.)
sven node export-ca [--config PATH]
sven node export-ca > ca.pem

# Serve the CA cert over HTTP so a phone can import it
sven node export-ca | { mkdir -p /tmp/ca && cat > /tmp/ca/ca-cert.pem; \
  python3 -m http.server --directory /tmp/ca 8080; }
# Then open http://<your-ip>:8080/ca-cert.pem on the phone
```

### Web terminal commands

The web terminal (`/web`) uses WebAuthn passkeys for authentication.  New
devices start in `pending` state and must be approved before gaining access.

```sh
# List all registered browser devices (pending, approved, revoked)
sven node web-devices list --token <token>
sven node web-devices list --token <token> --filter pending

# Approve a pending device (no restart required)
sven node web-devices approve <device-uuid> --token <token>

# Revoke an approved device (PTY session is terminated immediately)
sven node web-devices revoke <device-uuid> --token <token>
```

**Typical first-use workflow:**

```
1.  Start node:        sven node start --config .node.yaml
2.  Open browser:      https://<node-ip>:18790/web
3.  Register passkey:  follow the on-screen prompts (fingerprint / Face ID)
4.  Browser shows:     "Device <uuid> is waiting for approval"
5.  Admin runs:        sven node web-devices approve <uuid> --token <token>
6.  Browser:           immediately opens a full-screen tmux terminal
```

On subsequent visits the browser logs in with the stored passkey — no approval
needed.  The terminal session (tmux) persists across browser disconnects and
can be reattached from any approved device.

### `sven node exec` in detail

`exec` is the primary way to interact with a running gateway from the command
line.  It connects to the local gateway over WebSocket, submits the task, and
streams the agent's response to stdout.

```sh
# Set the token once (it was shown the first time the gateway started)
export SVEN_NODE_TOKEN=eyJ0eXAiOiJKV1QiLCJhbGc...

# Ask the agent a question
sven node exec "What files are in the current directory?"

# Trigger delegation to a peer agent
sven node exec "Use list_peers to find connected agents, then delegate \
  a hello-world task to whichever one is available"

# Use a different gateway URL or config
sven node exec "summarise recent changes" \
  --url wss://192.168.1.10:18790/ws \
  --config /etc/sven/gateway.yaml
```

The token is the one printed **once** when the gateway first started.  If you
lost it, rotate it with `sven node regenerate-token`.

---

## Relay server (cross-network connectivity)

mDNS only works on the same LAN.  To connect agents across networks, run a
relay server on any publicly reachable machine:

```sh
cargo run --bin sven-relay -- \
  --repo /path/to/git-repo \
  --listen /ip4/0.0.0.0/tcp/9000 \
  --keypair ~/.config/sven/relay-keypair
```

The relay publishes its address to the git repository so other agents can
discover it.  Copy the printed multiaddr (including `/p2p/<peer-id>`) into
your gateway config:

```yaml
swarm:
  relays:
    - "/ip4/relay.example.com/tcp/9000/p2p/12D3KooW..."
```

---

## Troubleshooting

### "P2P error: not authorized"

The peer is not in the allowlist.  Authorize it with:

```sh
sven node authorize "sven://..."
```

### "Handshake failed: input error" between two nodes

This is almost always a **firewall** issue.  Two separate ports are involved:

| Port | Config key | Purpose |
|------|-----------|---------|
| `swarm.listen` (e.g. 4010) | Agent mesh | Node-to-node task delegation |
| `control.listen` (e.g. 4009) | Operator control | Mobile/native operator clients only |

Agents dial the **agent mesh port** (`swarm.listen`) to delegate tasks.
Without a fixed `swarm.listen` the OS assigns a random port on every restart,
making firewall rules impossible.

Recommended config for cross-machine deployments:

```yaml
swarm:
  listen: "/ip4/0.0.0.0/tcp/4010"  # open this in your firewall on every machine
```

If you also need native/mobile operator access:

```yaml
control:
  listen: "/ip4/0.0.0.0/tcp/4009"  # open this too, only where needed
```

### Peers not appearing after `list_peers`

- **swarm.peers not configured**: Each node must list the other's agent peer ID
  under `swarm.peers` — the mesh is deny-all by default.  Check the startup log
  for the line `P2pNode starting peer_id=…` and add that ID to the other node's
  config (and vice versa).  After editing, restart both nodes.
- **LAN**: mDNS takes 5–10 seconds.  Both nodes must be running and in the
  same room (`swarm.rooms`).  Check the logs for
  `mDNS: discovered agent peer but swarm.peers is empty` — this confirms
  discovery works but the allowlist is the blocker.
- **Cross-network**: configure a relay with `swarm.relays`.

### "P2P error: relay connection failed"

1. Confirm the relay is running.
2. Check the multiaddr in `swarm.relays` matches what the relay printed on startup.
3. Check network connectivity: `ping relay.example.com`.

### "Cannot assign requested address (os error 99)"

`http.bind` contains an IP address that is not assigned to any local interface.
Either use `0.0.0.0:18790` to listen on all interfaces, or change the IP to
one that `ip addr` shows on this machine.  The startup log will now print a
clear message with the fix rather than just the raw OS error.

### Browser shows "Invalid HTTP response"

The server is running with TLS.  Use `https://` not `http://`:

```
https://localhost:18790/web
```

### Browser shows a certificate warning

- **With Tailscale** (`tls_mode: tailscale` or `auto` when Tailscale is
  running): the cert is issued by Let's Encrypt and should be trusted
  automatically.  Make sure you're connecting via the `*.ts.net` hostname, not
  the raw LAN IP.
- **With local CA**: run `sven node install-ca` once on this device, then
  restart the browser.
- **With self-signed**: click "Advanced → Proceed" once, or switch to
  `local-ca` mode.

### "TLS error: certificate not found" / stale cert

Delete the certs and restart; they will be regenerated:

```sh
rm ~/.config/sven/gateway/tls/gateway-cert.pem \
   ~/.config/sven/gateway/tls/gateway-key.pem
sven node start
```

The CA cert (`ca-cert.pem`) and CA key (`ca-key.pem`) are preserved so that
existing device trust is not invalidated.

### "WebAuthn error: rpid mismatch" / passkey registration fails

`web.rp_id` must exactly match the hostname or IP in the browser address bar.
If you access the node via `192.168.1.42`, set:

```yaml
web:
  rp_id: "192.168.1.42"
  rp_origin: "https://192.168.1.42:18790"
```

WebAuthn also requires HTTPS (or `localhost`).  A plain `http://` connection
will be rejected by the browser before sven is even involved.

### No log output from `sven node start`

The gateway logs to `stderr` at `info` level.  Try `sven -v node start` for
debug-level output.

---

For implementation details — wire protocols, internal architecture, the P2P
task flow, and the WebSocket API spec — see
[technical/gateway.md](technical/gateway.md).

For agent-to-agent collaboration — sessions (DMs), rooms (broadcast channels),
and the collaboration tools — see [09-collaboration.md](09-collaboration.md).
