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
| The inspector is a read-only pager; servers are added only by editing the configuration file | `tui/src/ui/inspector.rs` |
| In a container the callback listener binds `0.0.0.0:5598` (for `docker -p`; `state` refuses a forged callback; `sven mcp auth` with a pasted address needs no forwarded port) | `callback.rs` `bind` |

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

**M3 Sign-in UX: mostly done 2026-11-01.** Done: `sven mcp list|auth|logout`
with a pasted return address for remote shells, the sign-in link shown in the
TUI (`McpEvent::SignInLink`), `/mcp logout|enable|disable`, a refused sign-in
asking for a new one, a changed tool list reaching the TUI session, headless
`[sven:mcp]` notices; checked by `tests/sign_in.rs`, `tests/conformance.rs`
and the callback tests. Left: `/mcp add` and `sven mcp add` writing a server
into the configuration file, an inspector you act from, and a bats e2e of
`sven mcp auth` against the local authorization server fixture.

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
