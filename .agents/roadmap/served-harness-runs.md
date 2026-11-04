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

## Open

4. **Notices on the remote-control channel.**
   `ControlCommand::Notify { session_id, event }`. A notice enters the
   conversation as a `User`-role message framed as coming from the host, as
   `CONTINUE_INTERRUPTED` does (`crates/remote/src/sessions.rs`), never as a
   mid-conversation `System` message: the Anthropic driver keeps only the
   last system message as the system prompt
   (`crates/model-drivers/src/anthropic.rs`), so it would replace the
   agent's instructions. It is typed as a notice in session events and
   traces, so no surface or export takes it for the person; today the only
   way into a conversation is `Event::UserMessage`, so this is a kernel
   event of its own, handled by every machine that takes a message. A
   notice starts a turn only when the server says so.

Already in place: packages carry their `skills/` and `bin/` under the bundle
digest (`docs/reference/harnesses.md`, Imports); `file` inputs, `publish:`
with `HarnessRun::on_publish` checkpoints, and `retry: never`
(`docs/reference/harnesses.md`); the client side of MCP's file transfer
(`docs/reference/mcp.md`, Files); tools and gates served from another
process (`sven mcp serve --listen`, `ForwardedTool`); skills the signed-in
server offers (`docs/guides/skills.md`); the worker of a served run
(`sven harness work`, `sven_control::work`); files attached to an input on
the remote-control channel (`docs/guides/remote-control.md`). Every signed-in sven mounts its server's MCP endpoint
(`src/main.rs`, `connectors::add_signed_in`; the `sven rc` host through
`crates/remote/src/host/services.rs`).

## Checks

Each item is done when its own test proves it:

- 4: a notice is shown as a notice, starts no turn unless asked, and leaves
  the system prompt intact on every driver.
