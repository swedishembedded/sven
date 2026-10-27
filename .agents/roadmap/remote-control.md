# remote-control

Scope: `sven auth login` and `sven rc` (`sven-remote`), the HTTP binding in
`sven_control::remote`, and running sven's agent core in a browser.

**Status:** a host signs in to a server, takes commands over a server-sent
stream, runs one session actor per conversation and uploads every session
event; proven against a real server end to end.

## Left

1. **Durable sessions.** A session lives in memory; after `sven rc`
   restarts it is reopened from the flat text history `NewSession` carries,
   losing tool calls. Checkpoint each session (`AgentState`) beside the
   login and resume it.
2. **Approvals through the server.** Sessions run under auto approval. Send
   `ToolNeedsApproval` up and take `ApproveTool`/`DenyTool` down, with a
   per-session timeout.
3. **Attach from a terminal.** `sven rc` prints a line per event; a TUI
   attached to one of its sessions (sharing the same session, not a copy)
   needs the host to fan events out locally as well as to the server.
4. **Browser sessions.** A whole agent turn runs as wasm with the same
   observable sequence as natively, and `sven-wasm` exposes a session to
   JavaScript (`make build/wasm`, `make test/wasm`). Left: tools a page can
   register (the `Tool` trait is still `Send`), wiring the package into an
   application's Web Worker, and running `test/wasm` in a real browser
   worker once a WebDriver is available in CI.
5. **Shared conversation document.** A CRDT document of the conversation
   (one implementation for Rust and the browser) so a terminal and a
   browser can both write to it; execution order stays the host's.
