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

- **stdio** starts `command` with `args` and `env` and talks line-delimited
  JSON-RPC on its standard streams.
- **http** implements the Streamable HTTP transport: each request is a POST
  that accepts a JSON or `text/event-stream` reply, and a server-issued
  `mcp-session-id` is sent back on later requests.

### Tools and prompts

On connection sven lists the server's tools and prompts.

- A tool is registered as `<server>-<tool>`, with any character outside
  `[a-zA-Z0-9_-]` in either part replaced by `_`; a description is cut at 500
  characters. For the permission model an MCP tool is a network call
  (`NetworkAccess`): the session mode's policy decides whether it may run at
  all, and its default policy is to ask, so under manual approval, or through
  an ACP client, each call is put to a person.
- A prompt is a slash command named `/<server>/<prompt>` in the TUI, with the
  prompt's arguments completed as `<name>`.

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
| `client_id`, `client_secret` | a pre-registered client; without them sven uses its public client `sven-mcp-client` |
| `redirect_uri` | where the browser returns; by default `sven://sven.mcp/callback` natively and `http://127.0.0.1:5598/callback` in a container |
| `callback_port` | port of the local callback listener (default 5598) |

The flow is authorization code with PKCE (RFC 7636). Tokens are cached in
`~/.config/sven/mcp-credentials.json`, shared by every project that uses the
same server URL, and refreshed before they expire. A server that answers 401
shows as `needs-auth`; `/mcp auth <name>` in the TUI starts the flow. A static
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
EOF. It binds no port and has no authentication of its own: security is
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
| `--tools a,b,...` | serve only these tools; `all` or no flag serves the default set; a name outside the default set is ignored |
| `--brave-api-key KEY` | key for `web_search` (also read from `BRAVE_API_KEY`) |

Every served tool runs when the host calls it; the host applies its own
permission UI. The file tools, and the directory the shell starts in, are
confined to the directory the server was started in. The shell refuses a
command that matches `tools.deny_patterns` and honours `tools.timeout_secs`.
The configuration file is read as for any other sven process.
