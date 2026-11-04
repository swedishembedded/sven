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

Each entry of `mcp_servers` in `config.yaml` is one server; `sven mcp add`
and `/mcp add` write one for you (see [Managing servers](#managing-servers)). The entry's key is
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
| `tools` | `{}` | what sven may do with each tool without asking: `allow`, `ask` or `deny`, by tool name, `"*"` for every tool not named (below) |

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
  an ACP client, each call is put to a person. A server's `tools` setting
  changes that per tool: `allow` runs it without asking, `deny` never offers
  or runs it. A tool named there takes its own setting, any other the `"*"`
  one, and a tool in neither asks. A tool the server marks destructive asks
  under `"*": allow` unless it is named itself; the server's hints never
  loosen a setting:

  ```yaml
  mcp_servers:
    github:
      transport: {type: http, url: "https://mcp.example.com/mcp"}
      tools:
        "*": allow          # read and change issues without asking
        delete_repository: deny
  ```

- A tool's result reaches the agent whole: text, images and audio as
  themselves, an embedded resource as its text, a resource link as its URI,
  and a structured result as JSON when the server sent nothing else.
- A prompt is a slash command named `/<server>/<prompt>` in the TUI, with the
  prompt's arguments completed as `<name>`.

### Files

sven speaks MCP's file transfer extension (SEP-2631, now the experimental
extension `experimental-ext-file-uploads`; sven follows the PR revision of
2026-06-16) and declares it as the client extension
`io.modelcontextprotocol/files`:

- A tool argument marked `x-mcp-file` (a URI string, or a list of them)
  takes a file. When the model writes a local path there, sven asks the
  server `files/authorizeUpload` (name, media type, size, SHA-256 in
  base64url), uploads the bytes to the URL it answers (a single HTTPS
  request; a multipart form is refused), and calls the tool with the file
  URI the server minted. A path outside the directory sven works in, or a
  symlink out of it, is refused before anything is sent. A value that
  already names a file (`scheme://...`, `data:`) is passed as it is.
- A file URI is fetched with `files/authorizeDownload` and kept only when it
  matches the digest the server gave; its name is kept to one path segment.
- The model never sees an upload or download URL.

A host can add fields to the `_meta` of every tool call and file request to
a server (the session they are for); `sven rc` sets `sven/session` to the
session's id.

### Connected accounts

Signed in to a server that offers connected accounts (`sven auth login`),
every session also gets the server `connectors`: the accounts you connected
there, reached with this machine's key. See
[remote control](../guides/remote-control.md#connected-accounts).

### Managing servers

| Where | Command | Effect |
|-------|---------|--------|
| TUI | `/mcp` | the servers, their status and tools |
| TUI | `/mcp add [--local] <name> <url>`, `/mcp add <name> <command> [args...]` | says where the server goes, adds it there and sees it through to a connection (see [what `add` does](#what-add-does)) |
| TUI | `/mcp remove <name>` | remove it from your configuration file and disconnect it |
| TUI | `/mcp auth <name>`, `/mcp logout <name>` | sign in, forget the sign-in |
| TUI | `/mcp enable <name>`, `/mcp disable <name>` | connect it and offer its tools, or withdraw them, for this session (the configuration file is unchanged) |
| shell | `sven mcp add <name> <url>` (`--header`, `--oauth`, `--client-id`, `--local`), `sven mcp add <name> [--env NAME=VALUE] -- <command> [args...]` | the same, from a shell (`--scope user`, the default, `project` or `workspace`, picks the file of a local server) |
| shell | `sven mcp remove <name>` | remove it from that file |
| shell | `sven mcp list` | the servers on this machine, then your account's `connectors` with the services connected there; each with whether it connects, its tool count, or what it needs |
| shell | `sven mcp auth <name>`, `sven mcp logout <name>` | sign in, forget the sign-in |

#### What `add` does

`sven mcp add` and `/mcp add` first say where the server is going, in one
line, then do exactly one of these:

- **To your account.** Signed in to sven (`sven auth login`) and given one web
  address with no header, `--env` or client of your own: "Adding NAME to your
  sven account at ORIGIN (usable from all your sessions; use --local to keep
  it on this machine only) ...". The server is added to your account as a
  connector of your own, which is the default because one connection serves
  every session and computer and your sign-in to the service stays with your
  account, never on this machine (it works for a service that lets nobody
  register, GitHub, once your sven administrator set up a sign-in app for
  it). sven prints the sign-in link on a line of its own, opens it in a
  browser when it can, and waits: the shell until the server's tools reach
  the `connectors` server ("Waiting for you to finish signing in in the
  browser... (Ctrl+C to stop; the server stays added to your account)", at
  most ten minutes), the TUI in the background, with a toast when connected.
- **To a file on this machine.** Not signed in, or `--local`, or the request
  carries a header, `--env` or an OAuth client: "Adding NAME to FILE on this
  machine ...". A URL is a Streamable HTTP server, anything else a stdio
  command. When the server asks for an OAuth sign-in, sven starts it at once:
  it prints the link on a line of its own, opens the browser when it can and,
  in the shell, accepts the address your browser ends on pasted on stdin
  (for a browser on another machine); then it reconnects and says
  "Connected NAME: N tools." The TUI shows the link in the pager (`y` copies
  it) and toasts "NAME is connected (N tools)".
- **Neither**, when your sign-in record is from an older release that cannot
  add servers to an account: sven says so and stops (non-zero exit), rather
  than writing a file you did not ask for. Run `sven auth login` again, or use
  `--local`.

The sign-in link is always shown, never left to the browser opening: on a
headless machine opening "succeeds" with nothing on screen.

#### Which server wins

Servers configured on this machine and the account's `connectors` are offered
together; signing in never hides or disables a local server, and `--local`
works while signed in. When the same service is on both, the higher tier wins
and the model never sees its tools twice. The tiers, highest first:

1. the project configuration (the current directory),
2. the user configuration (your home),
3. the system configuration,
4. the signed-in account's connectors (`connectors`).

The first three merge key by key as the [configuration](configuration.md)
files do, so a server of the same name in the project replaces the user's.
The account's server is the lowest tier: each tool it offers carries `_meta`
`"com.swedishembedded/connector": {"name": ..., "url": ...}`, and a tool
whose connector has the name of an enabled local server, or the URL of an
enabled local HTTP server (compared by scheme, host and path, ignoring case
and a trailing slash), is dropped.

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
| `--listen ADDR` | serve over Streamable HTTP at `http://ADDR/mcp` instead of stdio; needs `--token-file` |
| `--token-file PATH` | the bearer token an HTTP client must present: at least 32 characters, in a file readable by its owner alone; any other request is answered 401 |

`shell_gate` (the command a harness gate runs, whose exit status decides it)
is served only when `--tools` names it. Over HTTP the server is for a host
that keeps its plan in one place and runs tools in another, a sandbox or a
verifier on a private network: `sven_mcp_client::ForwardedTool` offers a
served tool under its own name and the capability its local namesake has,
so a harness registered with them (`EngineBuilder::tool`) runs its agent's
tools in one server and its gates in another, and the agent cannot read
what the gates run.

Every served tool runs when the host calls it; the host applies its own
permission UI. The file tools, and the directory the shell starts in, are
confined to the directory the server was started in. The shell refuses a
command that matches `tools.deny_patterns` and honours `tools.timeout_secs`.
The configuration file is read as for any other sven process.
