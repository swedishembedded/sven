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
| The inspector is a read-only pager; acting on a server is a `/mcp` subcommand | `tui/src/ui/inspector.rs` |
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
and the callback tests; `sven mcp add|remove` and `/mcp add|remove` (one
`config_edit` module), checked end to end by `tests/mcp_cli.rs` (sven adds
itself as a stdio server). Left: an inspector you act from.

**M5 Gateway as a server: done 2026-11-02.** A server's discovery document
may name `mcp_url`; the sign-in keeps it (same-origin checked), and every
session-starting mode adds it as the `connectors` server with the machine's
key (`sven_bootstrap::connectors`; `sven rc` and hosted hosts in
`use_server_services`, whose allow list admits `connectors-*`). A configured
`mcp_servers.connectors` replaces it. Checked: the hosted host test in
`remote/src/host.rs`, the prefix test in the registry, the bootstrap test,
and by hand against a TypeScript SDK v2 server (negotiated 2026-07-28).
When the server names `mcp_token_url`, the server is reached with short-lived
tokens traded for the key (`exchange.rs`; conformance case), never the key.
Tasks extension (`io.modelcontextprotocol/tasks`) follows once a gateway or
server needs it.

**M4 Tool policy by action: done 2026-11-02.** `mcp_servers.<name>.tools`
(`allow`, `ask`, `deny`, by name or `*`; a destructive hint only tightens),
applied in `McpManager::tools` and in manual approval's preapproval for the
session's own registry. Child sessions still ask for an allowed MCP tool
(their preapproval has no registry): conservative, left as is.

**M6 Live tool lists (agreed 2026-11-08): done 2026-11-08.** The client follows a server's
tool-list changes for as long as a session runs: for a server advertising
`tools.listChanged`, a `subscriptions/listen` stream (2026-07-28) reopened
with backoff and followed by a re-list on every reopen, so a missed change is
recovered; for one that does not, a slow periodic re-list. A changed list
reaches the registry through the existing `tool_generation` path; under
deferred disclosure the definitions sent to the model change only when the
model loads a tool. Hosted sessions always run deferred. Done when: a
conformance case where a server adds a tool after the session started and
the session lists it without a restart, one where the stream drops and the
change is still seen, and the deferred request's tool definitions are
byte-identical across the change. whale's side: `connectors.md` C8.
Checked by `tests/conformance.rs` (a tool added mid-session, a change made
while the stream was down, a listing that changed nothing announcing
nothing), `tool-registry/tests/it/disclosure.rs` (definitions byte-identical
across a connector added) and the hosted host test.

## Not planned

Running MCP servers on behalf of other people; sven stays a client of what
its user configured. MCP Apps rendering until a surface needs it.
