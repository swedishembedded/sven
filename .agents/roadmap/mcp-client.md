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

## Where the client is

`crates/mcp-client` runs on `rmcp` 3.8 (2550 lines, from 5048 hand-written),
as does `crates/mcp`. Done and described in `docs/reference/mcp.md`: both
protocol eras over both transports, full pagination, concurrent requests, a
timeout on every request, every content type to the agent, no lock held
across a call, OAuth with `iss` and `resource` checked by
`tests/sign_in.rs`, sign-ins bound to their issuer. The conformance suite is
`tests/conformance.rs`.

Still as before:

| Finding | Evidence |
| --- | --- |
| Every MCP tool is `NetworkAccess` with `Ask`; tool annotations not used | `bridge.rs` |
| `/mcp enable` and `/mcp disable` advertised, not implemented; inspector is a read-only pager | `commands/src/builtin/inspect.rs:~145-215`, `tui/src/ui/inspector.rs` |
| A refreshed tool list reaches no live session in the shipped frontends: the TUI's refresh channel has no subscriber | `tui/src/app/run.rs:72-73,385-387` |
| A refused stored sign-in fails the server instead of offering a new sign-in | `manager.rs` `needs_sign_in` |
| The sign-in link is only opened in a browser or printed to stderr; no frontend shows it | `callback.rs` `open_browser` |
| In a container the callback listener binds `0.0.0.0:5598` (for `docker -p`; `state` refuses a forged callback) | `callback.rs` `bind` |

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

**M0-M2: done 2026-11-01.** Security fixes, the `rmcp` spike, and the client
on `rmcp` (see above). Left from M2: MRTR `input_required` answered through
the session's question path (today sven advertises neither elicitation nor
sampling, so a server cannot ask),
`ttlMs` used to skip refetching a fresh list, and large resources kept as
artifacts instead of inlined.

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
