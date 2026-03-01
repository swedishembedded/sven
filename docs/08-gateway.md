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

To have two agents collaborate, simply start the gateway on a second machine:

```sh
# machine B
sven node start
```

Both agents discover each other via mDNS within a few seconds — no pairing,
no configuration needed on a local network.  Each agent automatically gets
`list_peers` and `delegate_task` tools pointing at the other.

To give each agent a distinct identity, set their names in the config:

```yaml
# machine A — .gateway.yaml
p2p:
  agent:
    name: "backend-agent"
    description: "Rust and PostgreSQL specialist"
    capabilities: ["rust", "postgres"]

# machine B — .gateway.yaml
p2p:
  agent:
    name: "frontend-agent"
    description: "React and TypeScript specialist"
    capabilities: ["typescript", "react"]
```

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

## Security defaults

Everything is secure out of the box.  These defaults are hardcoded and cannot
be weakened by accident:

| What | Default |
|------|---------|
| HTTP TLS | On — ECDSA P-256, 90-day auto-generated cert |
| TLS version | TLS 1.3 only |
| P2P encryption | Noise protocol (Ed25519), always on |
| P2P authorisation | Deny-all — every peer must be explicitly paired |
| HTTP binding | `127.0.0.1` — loopback only |
| Rate limiting | 5 failures/min locks out the source for 60 s |
| Bearer token storage | SHA-256 hash only — plaintext never written to disk |
| Secret file permissions | `0o600` on Unix |
| Task timeout | 15 minutes per inbound delegated task |

To expose the gateway beyond loopback, set `http.bind` explicitly in your
config and make sure the machine is behind a firewall.

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

p2p:
  keypair_path: "~/.config/sven/gateway/keypair"
```

### Full example

```yaml
http:
  bind: "127.0.0.1:18790"
  insecure_dev_mode: false  # only set true for local development
  tls_cert_dir: "~/.config/sven/gateway/tls"
  token_file: "~/.config/sven/gateway/token.yaml"

p2p:
  listen: "/ip4/0.0.0.0/tcp/0"
  keypair_path: "~/.config/sven/gateway/keypair"
  authorized_peers_file: "~/.config/sven/gateway/authorized_peers.yaml"
  mdns: true

  # Identity this agent shows to other agents
  agent:
    name: "backend-agent"
    description: "Rust and PostgreSQL specialist"
    capabilities: ["rust", "postgres", "api-design"]

  # Rooms group agents together for discovery
  rooms: ["default", "team-alpha"]

  # Relay for connecting agents across networks (optional)
  relays:
    - "/ip4/relay.example.com/tcp/9000/p2p/12D3KooW..."

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
| `bind` | `127.0.0.1:18790` | Address and port to listen on |
| `insecure_dev_mode` | `false` | Disable TLS for local development only |
| `tls_cert_dir` | `~/.config/sven/gateway/tls` | Where to store the auto-generated certificate |
| `token_file` | `~/.config/sven/gateway/token.yaml` | Hashed bearer token storage |
| `max_body_bytes` | `4194304` | Max request body size (4 MiB) |

#### `p2p`

| Key | Default | Description |
|-----|---------|-------------|
| `listen` | `/ip4/0.0.0.0/tcp/0` | libp2p listen address (OS picks the port) |
| `keypair_path` | — (ephemeral) | Persist the operator keypair across restarts |
| `authorized_peers_file` | `~/.config/sven/gateway/authorized_peers.yaml` | Operator allowlist |
| `mdns` | `true` | mDNS for automatic LAN discovery |
| `agent.name` | system hostname | Name shown to peer agents |
| `agent.description` | `"General-purpose sven agent"` | Free-form description |
| `agent.capabilities` | `[]` | Tags other agents use to choose this agent |
| `rooms` | `["default"]` | Discovery namespaces; peers in the same room find each other |
| `agent_keypair_path` | `~/.config/sven/gateway/agent-keypair` | Persist the agent routing keypair |
| `relays` | `[]` | Relay multiaddrs for cross-network connectivity |

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
p2p:
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

### Peers not appearing after `list_peers`

- **LAN**: mDNS takes 5–10 seconds.  Both gateways must be running and in the
  same room (`p2p.rooms`).
- **Cross-network**: configure a relay with `p2p.relays`.

### "P2P error: relay connection failed"

1. Confirm the relay is running.
2. Check the multiaddr in `p2p.relays` matches what the relay printed on startup.
3. Check network connectivity: `ping relay.example.com`.

### "TLS error: certificate not found"

Delete the stale cert and restart:

```sh
rm ~/.config/sven/gateway/tls/*.pem
sven node start
```

### No log output from `sven node start`

The gateway logs to `stderr` at `info` level.  Try `sven -v gateway start` for
debug-level output.

---

For implementation details — wire protocols, internal architecture, the P2P
task flow, and the WebSocket API spec — see
[technical/gateway.md](technical/gateway.md).
