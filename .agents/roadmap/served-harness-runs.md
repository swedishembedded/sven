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

3. **`sven harness work`.** The worker side of a run protocol defined in
   `sven_control` the way `remote.rs` defines remote control: sign in from a
   login file, fetch the run descriptor, the package and the input files,
   run, and at each publishing boundary upload the files and the checkpoint
   and commit them in one request; heartbeat; obey pause and cancel. Every
   request carries the run's generation, and a superseded worker stops on
   `409`, as a superseded `sven rc` does.
4. **Notices and attachments on the remote-control channel.**
   `ControlCommand::Notify { session_id, event }` and attachments (file
   values) on `NewSession`/`SendInput`. A notice enters the conversation as a
   `User`-role message framed as coming from the host, as
   `CONTINUE_INTERRUPTED` does (`crates/remote/src/sessions.rs`), never as a
   mid-conversation `System` message: the Anthropic driver keeps only the
   last system message as the system prompt
   (`crates/model-drivers/src/anthropic.rs`), so it would replace the
   agent's instructions. It is typed as a notice in session events and
   traces, so no surface or export takes it for the person. A notice starts
   a turn only when the server says so.
6. **Skills from the signed-in server.** The `skill` tool
   (`crates/tools-agent/src/skill.rs`) lists and loads skills the signed-in
   server offers, beside local ones. sven's MCP client ignores a server's
   `instructions` and prompts today (`crates/mcp-client`).
7. **`sven mcp serve` reachable from another pod.** Streamable HTTP with a
   bearer token read from a file (rmcp's `transport-streamable-http-server`
   is already a dependency of `crates/mcp`), and `shell_gate` servable on
   request. A host then keeps the model and the plan in one process and runs
   tools, and gates, elsewhere by registering forwarding tools with
   `EngineBuilder::tool` (a registered tool wins a name collision with a
   built-in one, `crates/sdk/src/engine.rs`), so content a run reads can
   never reach the plan, the gate scripts or the network.

Already in place: packages carry their `skills/` and `bin/` under the bundle
digest (`docs/reference/harnesses.md`, Imports); `file` inputs, `publish:`
with `HarnessRun::on_publish` checkpoints, and `retry: never`
(`docs/reference/harnesses.md`); the client side of MCP's file transfer
(`docs/reference/mcp.md`, Files). Every signed-in sven mounts its server's MCP endpoint
(`src/main.rs`, `connectors::add_signed_in`; the `sven rc` host through
`crates/remote/src/host/services.rs`).

## Checks

Each item is done when its own test proves it:

- 3: against a fake server, a stale generation's commit is refused and the
  worker stops.
- 4: a notice is shown as a notice, starts no turn unless asked, and leaves
  the system prompt intact on every driver.
- 7: a harness run whose tools live in another `sven mcp serve` passes its
  gates; the agent cannot read the gate scripts.
