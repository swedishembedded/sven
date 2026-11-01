<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# mcp-client

Scope: sven as an MCP client: a current-protocol client for any remote or local MCP server, standalone with any model provider, with a guided sign-in and a tool policy that holds after the server's tool list changes.

Goal: a person running sven alone (OpenRouter, a local model, brain) adds
an MCP server by URL or command, signs in to it once from the TUI or with a
printed link in a headless run, and the agent sees every tool, prompt and
resource the server offers with full results. A connector gateway is to sven
just one more remote MCP server reached with the sign-in sven already has;
nothing in sven is specific to one gateway.

Grounded against sven ea973df2 on 2026-11-01 and the MCP 2026-07-28
specification (changelog, authorization and Streamable HTTP pages). Every
file reference below was read.

## Where the client is today

`crates/mcp-client` (5048 lines, hand-written) beside `crates/mcp` (sven as
an MCP server, built on `rmcp` 0.15). Two implementations of the same wire
types in one workspace.

| Finding | Evidence |
| --- | --- |
| Legacy-only: always `initialize`, version `2024-11-05`, no negotiation | `protocol.rs:149`, `client.rs:32-64`, `transport.rs:325`, `oauth.rs:318,369,419,963` |
| Lists stop at the first page; `nextCursor` parsed, never followed | `client.rs:89,145,190`; `protocol.rs:168,229,284` |
| Results keep text only; images and resources dropped; no audio, `resource_link` or `structuredContent` | `protocol.rs:~198,215` |
| SSE body read whole, then split; server requests on the stream ignored | `transport.rs:483` |
| stdio has no pending-request map: concurrent calls can eat each other's replies, then time out | `transport.rs:~143-235` |
| Server-initiated requests dropped, not answered; `roots.listChanged` advertised without a handler | `protocol.rs:~150` |
| Manager holds the servers read lock across the network call; reconnect and 401 handling wait a tool timeout | `manager.rs:579-606` |
| Every MCP tool is `NetworkAccess` with `Ask`; tool annotations not parsed | `bridge.rs:100,115-117` |
| `/mcp enable` and `/mcp disable` advertised, not implemented; inspector is a read-only pager | `commands/src/builtin/inspect.rs:~145-215`, `tui/src/ui/inspector.rs` |

## Security findings still open

Fixed on 2026-11-01: a refresh bypassing `tools.only` / `tools.disabled`
(registry `NamePolicy`), `--hosted` and `--container` starting configured MCP
servers, token text in the sign-in result, and the token store's unprotected
read-modify-write (`sven_config::write_private` plus a lock).

| Finding | Evidence | Closed by |
| --- | --- | --- |
| No `iss` check of the authorization response (RFC 9207); no `resource` parameter (RFC 8707) in authorize, code exchange or refresh | `oauth.rs:~1040-1075,595-603,1298-1355` | M2: the flow moves onto `rmcp`'s, which does both (`transport/auth.rs`, 3.8.0) |
| Credentials keyed by server name and URL, not by issuer (2026-07-28 requires issuer keying) | `oauth.rs` `CredentialsStore::key` | M2: `rmcp`'s `StoredCredentials` carries the issuer; sven's store keys by it |
| In a container the callback listener binds `0.0.0.0:5598` (needed for `docker -p` forwarding; `state` stops a forged callback) | `oauth.rs:65,713-725` | M3: paste-the-redirect sign-in needs no listener reachable from outside |

## Decisions

| Topic | Choice |
| --- | --- |
| Protocol implementation | The client moves onto `rmcp` 3.8 (Apache-2.0), protocol and OAuth; `crates/mcp` upgrades from 0.15 with it. The hand-written protocol, transports, SSE parser and OAuth flow are deleted, not kept as a fallback |
| Lifecycle | HTTP: `ClientLifecycleMode::Auto` (modern first, `initialize` on a legacy answer). stdio: `Auto`, and when the server closes the pipe on `server/discover` (an `rmcp` 0.15 server does), start the process again with `Initialize` |
| What stays sven's | Manager (health, reconnect, events), the bridge to `Tool`, the credential store, the sign-in UX and the tool policy. `rmcp` types stay inside `crates/mcp-client` |
| Tool policy | An immutable policy value held by the registry and checked in `execute` as well as when building the list, so no later insertion path can bypass it |
| Tool annotations | Untrusted hints: they may only tighten a tool's default (`destructiveHint` makes it always ask), never loosen it |
| Hosted mode | `--hosted` connects to no MCP server except the ones its launcher names explicitly; stdio servers never |
| Headless sign-in | Never opens a browser or blocks; the run ends the call with a structured `authentication_required` result carrying the URL |

## Milestones

Each closes with `make check && make test` and the number named.

**M0 Security fixes.** Four done (see above); the rest close with M2 and M3,
each with a regression test: a callback with a wrong `iss` persists no token;
`resource` is sent in all three requests.

**M1 rmcp spike: done 2026-11-01.** A scratch client on `rmcp` 3.8.0
against an `rmcp` 0.15 server over a pipe: `Auto` fails because the legacy
server closes on `server/discover`; respawn with `Initialize` connects (2 ms,
negotiates `2025-03-26`); `list_all_tools` follows three pages; four
concurrent 300 ms calls on one connection finish in 303 ms. From the 3.8.0
source: `iss` checked when present or required, `resource` sent in all three
OAuth requests, CIMD supported, issuer kept with stored credentials,
pluggable `CredentialStore` with a refresh guard, MRTR in `call_tool`,
`ttlMs` cache hints, audio, `resource_link` and `structuredContent` types.
Cost: it requires `reqwest` 0.13 (the workspace is on 0.12), so the
workspace moves to 0.13 first.

**M2 Client on rmcp.** Workspace `reqwest` 0.13, then `crates/mcp` on `rmcp`
3.8, then port the manager and bridge; delete the hand-written
protocol layer. Pagination followed to the end (bounded page count and
total size); all content types carried to the model as typed parts
(`structuredContent` kept, images and audio through the existing media path,
large resources as artifacts, not inlined); `ttlMs` respected for lists; MRTR
`input_required` answered through the session's question path. Number: a
conformance suite in `crates/mcp-client/tests` against both eras and both
transports, including 3-page lists, every content type, two concurrent calls
on one stdio server, and a server that drops the connection mid-call.

**M3 Sign-in UX.** `/mcp` becomes a real manager: add a server (URL or
command), sign in, sign out, enable, disable, inspect tools and their policy.
A 401 on a stored token offers sign-in again instead of failing the server.
SSH and containers get a printed link and a pasted code, not a listening
port. Client ID Metadata Document as the default registration, DCR as the
fallback. Number: an e2e bats suite against a local OAuth-protected MCP
server fixture (sign in, expire, refresh, revoke, re-sign-in).

**M4 Tool policy by action.** Per-server and per-tool policy in config
(`allow`, `ask`, `deny`) with annotations tightening the default. Number:
policy tests across modes, including `--hosted` and auto-approve modes.

**M5 Gateway as a server.** A remote-control sign-in also configures that
service's MCP endpoint as a server, authenticated with the same credential,
listed apart in `/mcp` as connectors; its `authentication_required` results
render as an "open this link" action; its tool list refreshes on
`list_changed` (which is why M0's policy fix comes first). Nothing here names
or special-cases a particular service. Tasks extension
(`io.modelcontextprotocol/tasks`) follows once a gateway or server needs it.

## Not planned

Running MCP servers on behalf of other people; sven stays a client of what
its user configured. MCP Apps rendering until a surface needs it.
