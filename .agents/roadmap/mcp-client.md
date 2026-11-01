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

## Security findings (fixed first, M0)

| Finding | Evidence |
| --- | --- |
| A tool-list refresh re-adds tools that `tools.only` / `tools.disabled` removed at startup; nothing checks the allow list at execution. No shipped caller triggers a refresh today (the TUI's refresh channel has no subscriber), so it is latent until one does | `tool-registry/src/registry.rs:159-166`; startup filter `bootstrap/src/registry.rs:764-795`; callers `bootstrap/src/kernel_bridge.rs:362-369`, `frontend/src/agent.rs:305-313` |
| `--hosted` sets `tools.only` but keeps `mcp_servers`: configured servers are still spawned or connected, only their tools are hidden | `remote/src/host/hosted.rs:103-111` |
| Sign-in success message carries the first 8 characters of the access token | `mcp-client/src/manager.rs:788-791` |
| Tokens and registered clients written with `std::fs::write`: umask permissions, no atomic replace, lost updates between processes | `mcp-client/src/oauth.rs:165-192` |
| In a container the callback listener binds `0.0.0.0:5598` | `mcp-client/src/oauth.rs:65,713-725` |
| No `iss` check of the authorization response (RFC 9207); no `resource` parameter (RFC 8707) in authorize, code exchange or refresh | `oauth.rs:~1040-1075,595-603,1298-1355` |
| Credentials keyed by server name and URL, not by issuer (2026-07-28 requires issuer keying); docs say URL only | `oauth.rs:~224`; `docs/reference/mcp.md:97-98`, `docs/reference/configuration.md:1410-1412` |

## Decisions

| Topic | Choice |
| --- | --- |
| Protocol implementation | The client moves onto `rmcp` 3.x (README: implements 2026-07-28, compatible with 2025-11-25 and earlier, `Auto` lifecycle probes `server/discover` and falls back to `initialize`). `crates/mcp` upgrades with it. The hand-written protocol, transports and SSE parser are deleted, not kept as a fallback. Confirmed by the M1 spike before anything is ported |
| What stays sven's | Manager (health, reconnect, events), the bridge to `Tool`, the credential store, the sign-in UX and the tool policy. `rmcp` types stay inside `crates/mcp-client` |
| Tool policy | An immutable policy value held by the registry and checked in `execute` as well as when building the list, so no later insertion path can bypass it |
| Tool annotations | Untrusted hints: they may only tighten a tool's default (`destructiveHint` makes it always ask), never loosen it |
| Hosted mode | `--hosted` connects to no MCP server except the ones its launcher names explicitly; stdio servers never |
| Headless sign-in | Never opens a browser or blocks; the run ends the call with a structured `authentication_required` result carrying the URL |

## Milestones

Each closes with `make check && make test` and the number named.

**M0 Security fixes.** The seven findings above, one commit each, each with a
regression test: refresh after `list_changed` keeps an excluded tool
uncallable (asserted at `execute`, not only in schemas); `--hosted` with a
configured server starts no process and opens no connection; the sign-in
message holds no token material; the credential files are 0600 and replaced
atomically under a lock; the callback binds loopback unless a forwarded port
is configured explicitly; a callback with a wrong `iss` persists no token;
`resource` is sent in all three requests. Docs corrected in the same commits.

**M1 rmcp spike.** One branch that connects `rmcp` 3.x's client to a
2026-07-28 server and to a legacy server (`crates/mcp`'s own server, and one
stdio reference server), over stdio and Streamable HTTP. Answers, with code:
does its OAuth client do PKCE, RFC 9207, RFC 8707, Client ID Metadata
Documents and issuer-keyed storage, or does sven keep its own flow; does it
multiplex concurrent requests; licence and transitive dependencies
(`cargo deny`). Outcome recorded here before M2 starts.

**M2 Client on rmcp.** Port the manager and bridge; delete the hand-written
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
