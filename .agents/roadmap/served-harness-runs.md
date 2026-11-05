<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# served-harness-runs

Scope: Running harness packages for a server's users as durable, resumable, file-in/file-out runs, with files and notices flowing between a signed-in sven and its server.

A server that sven signs in to (`sven auth login`, `crates/remote`) may run
harness packages on its own workers for the people it serves: inputs are
files, every published step is checkpointed with its files, a worker can die
at any point and the run continues on another, and the person's own sven is
told when something needs them. sven stays the engine and the client; the
server owns runs, files, money and authorization. Nothing here names a
server; every change is generic and usable by any server that implements the
protocols.

Already in place: packages carry their `skills/` and `bin/` under the bundle
digest (`docs/reference/harnesses.md`, Imports); `file` inputs, `publish:`
with `HarnessRun::on_publish` checkpoints, and `retry: never`
(`docs/reference/harnesses.md`); the client side of MCP's file transfer
(`docs/reference/mcp.md`, Files); tools and gates served from another
process (`sven mcp serve --listen`, `ForwardedTool`); skills the signed-in
server offers (`docs/guides/skills.md`); the worker of a served run
(`sven harness work`, `sven_control::work`); files attached to an input on
the remote-control channel and notices a server tells a session, which join the conversation as the host's word and start a turn only when asked (`docs/guides/remote-control.md`). Every signed-in sven mounts its server's MCP endpoint
(`src/main.rs`, `connectors::add_signed_in`; the `sven rc` host through
`crates/remote/src/host/services.rs`).
