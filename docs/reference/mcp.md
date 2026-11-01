<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# MCP - Model Context Protocol

Sven speaks the [Model Context Protocol](https://modelcontextprotocol.io) in
both directions:

- as a **client** (`mcp_servers` in the configuration): sven connects to
  external MCP servers and offers their tools and prompts to the agent;
- as a **server** (`sven mcp serve`): an MCP host such as Cursor or Claude
  Desktop launches sven and calls a subset of its built-in tools.

Both are part of the `network` cargo feature, which is on by default. For an
editor that drives sven as a whole agent, use [ACP](acp.md) instead.

## Using MCP servers (client)

Each entry of `mcp_servers` in `config.yaml` is one server. The entry's key is
its name and the prefix of its tools. The key reference is in
[configuration](configuration.md#mcp_servers); this page explains behaviour.

```yaml
mcp_servers:
  github:
    transport:
      type: stdio
      command: npx
      args: ["-y", "@modelcontextprotocol/server-github"]
    env:
      GITHUB_TOKEN: "${GITHUB_TOKEN}"

  docs:
    transport:
      type: http
      url: "https://mcp.example.com/mcp"
      headers:
        Authorization: "Bearer ${DOCS_TOKEN}"
    timeout_secs: 60
```

| Key | Default | Meaning |
|-----|---------|---------|
| `transport.type` | | `stdio` or `http` (required) |
| `transport.command`, `transport.args` | `[]` | stdio: the executable and its arguments, started as a child process speaking JSON-RPC over stdin/stdout |
| `transport.url`, `transport.headers` | `{}` | http: the endpoint and extra request headers (for a static bearer token) |
| `enabled` | `true` | a disabled server is not connected and offers no tools |
| `env` | `{}` | environment variables for a stdio server |
| `oauth` | none | OAuth 2.0 settings for an http server (below) |
| `timeout_secs` | `30` | longest a single call may take |

`${VAR}` and `${VAR:-default}` in any value of the configuration file are
replaced from the environment when the file is read, so secrets need not be
written into it.

### Transports

- **stdio** starts `command` with `args` and the variables in `env` (and
  `PATH`, nothing else of sven's environment) and talks line-delimited
  JSON-RPC on its standard streams.
- **http** implements the Streamable HTTP transport, with or without a
  server session.

sven speaks the current protocol revision (2026-07-28: `server/discover` and
per-request metadata) and the earlier ones (`initialize`), whichever the
server does. Over http the first request finds out; a stdio server of an
earlier revision that closes on the unfamiliar first request is started again
and spoken to with `initialize`. Requests to one server run side by side, and
each ends at `timeout_secs` however the server goes quiet.

### Tools and prompts

On connection sven lists the server's tools and prompts, every page of them,
and lists them again when the server says they changed.

- A tool is registered as `<server>-<tool>`, with any character outside
  `[a-zA-Z0-9_-]` in either part replaced by `_`; a description is cut at 500
  characters. For the permission model an MCP tool is a network call
  (`NetworkAccess`): the session mode's policy decides whether it may run at
  all, and its default policy is to ask, so under manual approval, or through
  an ACP client, each call is put to a person.
- A tool's result reaches the agent whole: text, images and audio as
  themselves, an embedded resource as its text, a resource link as its URI,
  and a structured result as JSON when the server sent nothing else.
- A prompt is a slash command named `/<server>/<prompt>` in the TUI, with the
  prompt's arguments completed as `<name>`.

### Managing servers

| Where | Command | Effect |
|-------|---------|--------|
| TUI | `/mcp` | the servers, their status and tools |
| TUI | `/mcp auth <name>`, `/mcp logout <name>` | sign in, forget the sign-in |
| TUI | `/mcp enable <name>`, `/mcp disable <name>` | connect it and offer its tools, or withdraw them, for this session (the configuration file is unchanged) |
| shell | `sven mcp list` | each configured server, whether it connects, its tool count, or what it needs |
| shell | `sven mcp auth <name>`, `sven mcp logout <name>` | sign in, forget the sign-in |

A server's tool list that changes while a TUI session runs reaches the agent at
once, held to `tools.only` and `tools.disabled`.

A connected server that drops is reconnected with exponential backoff; after
ten consecutive failures its status becomes `failed` and sven stops trying.
The other statuses are `initializing`, `connecting`, `connected`, `disabled`,
`reconnecting` and `needs-auth`.

### OAuth

An http server that requires OAuth 2.0 is configured with an `oauth` block;
`oauth: {}` is enough for most servers.

| Key | Meaning |
|-----|---------|
| `scopes` | scopes to request; empty means discover them (the `WWW-Authenticate` challenge, then the server's protected-resource metadata, then none) |
| `client_id`, `client_secret` | a pre-registered client; without them sven registers one (Dynamic Client Registration), or uses the public client id `sven-mcp-client` when the server offers no registration |
| `client_metadata_url` | an HTTPS Client ID Metadata Document describing the client, used as the client id by authorization servers that accept such documents |
| `redirect_uri` | where the browser returns; by default `sven://sven.mcp/callback` natively and `http://127.0.0.1:5598/callback` in a container |
| `callback_port` | port of the local callback listener (default 5598) |

The flow follows the MCP authorization rules: authorization code with PKCE,
the token bound to the server it is for (the `resource` parameter, RFC 8707),
and the issuer of the browser's answer checked against the authorization
server's (RFC 9207), so an answer from another authorization server signs in
nothing. Tokens are kept in `~/.config/sven/mcp-oauth.json`, readable by its
owner only, shared by every project that names the server the same and uses
the same URL, and refreshed when they expire; a sign-in is forgotten when the
server's authorization server changes. sven processes running at once update
the file under a lock, and refresh one at a time. In the TUI a server that
answers 401 is signed in to at once: the browser opens and the sign-in link is
shown as well. `sven mcp auth <name>` signs in from any terminal; over SSH or
from a container, paste the address the browser ends on (see
[troubleshooting](../guides/troubleshooting.md#signing-in-from-a-remote-shell-or-a-container)).
A headless run never opens a browser or waits: the server shows as
`needs-auth` and the run prints a `[sven:mcp]` line saying how to sign in. A
server that refuses its sign-in is signed out and asks for a new one. A static
bearer token needs no `oauth` block: put it in `transport.headers`. Setup of
the `sven://` and `cursor://` redirect handlers is in
[troubleshooting](../guides/troubleshooting.md#mcp-oauth-authentication).

### Managing servers

- In the TUI, `/mcp` opens the server inspector and `/mcp auth <name>` starts
  the OAuth flow of one server.
- The model can add and remove servers through the `system` tool
  (`add_mcp_server`, `remove_mcp_server`), which edit the nearest
  `.sven/config.yaml`; research mode and sub-agents are not offered these
  actions. See [tools](tools.md).
- An engine built with the SDK and no built-in tools (`Toolset::none()`, the
  default) connects no MCP servers.

## Serving sven over MCP (server)

```sh
sven mcp serve
```

The server speaks line-delimited JSON-RPC on stdin/stdout until stdin reaches
EOF, to hosts of the current MCP revision (2026-07-28, `server/discover`) and
of earlier ones (`initialize`). Tool results carry text, image and audio
blocks. It binds no port and has no authentication of its own: security is
inherited from the host process that launched it. The same server is built as
the standalone binary `sven-mcp`.

Host configuration (Cursor, Claude Desktop, opencode):

```json
{ "mcpServers": { "sven": { "command": "sven", "args": ["mcp", "serve"] } } }
```

It advertises the tools capability only, no prompts or resources, and serves
these built-in tools by default: `edit_file`, `find_file`, `grep`,
`read_file`, `read_lints`, `shell`, `web_fetch`, `web_search` and
`write_file`. They are the same tools, under the same names, as the agent's.

| Flag | Meaning |
|------|---------|
| `--tools a,b,...` | serve only these tools; `default` or no flag serves the nine above; a name outside that set (including a stale `all`) is an error. The set is deliberately smaller than the agent's tools: those that need a live session (`ask_question`, `todo`, `switch_mode`) or change agent state are not offered to MCP clients |
| `--brave-api-key KEY` | key for `web_search` (also read from `BRAVE_API_KEY`) |

Every served tool runs when the host calls it; the host applies its own
permission UI. The file tools, and the directory the shell starts in, are
confined to the directory the server was started in. The shell refuses a
command that matches `tools.deny_patterns` and honours `tools.timeout_secs`.
The configuration file is read as for any other sven process.
