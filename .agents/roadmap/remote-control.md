# remote-control

Scope: `sven auth login` and `sven rc` (`sven-remote`), the HTTP binding in
`sven_control::remote`, and running sven's agent core in a browser.

**Status:** a host signs in to a server, takes commands over a server-sent
stream, runs one session actor per conversation and uploads every session
event; proven against a real server end to end.

## Left

1. **Durable sessions.** Done: each session's own conversation (tool calls
   included) is kept after every finished tool call and turn, and reopened
   from there after a restart (`sven_remote::sessions`). A turn stopped
   mid-run keeps its work; a cut-off call is reported as interrupted, never
   re-run; the server's retry continues the turn. A model call in flight is
   simply asked again: it has no effect to repeat.
2. **Approvals through the server.** Done: a tool that asks first asks
   the server (`ToolNeedsApproval`) and waits for `ApproveTool`/`DenyTool`,
   denied after ten minutes of silence (`sven_remote::approvals`).
3. **Terminal input.** A line typed at `sven rc` is the conversation's next
   message (`ControlEvent::UserInput`, recorded by the server before its
   answer). `sven rc attach` is a terminal on a conversation from
   anywhere: it reads the shared document live and writes through the
   server (`{control_url}/conversations/{id}/messages`), so every client
   writes through the same authorized path. Left: the full-screen TUI as
   an attach client (today attach is line-based).
4. **Browser sessions.** A whole agent turn runs as wasm with the same
   observable sequence as natively, and `sven-wasm` exposes a session to
   JavaScript (`make build/wasm`, `make test/wasm`). A page gives its
   agent tools (declared in the options, run by one JavaScript handler,
   allowed exactly their declared effects; `crates/wasm/tests/session.mjs`
   proves the round trip). Left: running `test/wasm` in a real browser
   worker once a WebDriver is available in CI.
5. **Shared conversation document.** `sven-session-sync` is the CRDT
   (yrs) document every replica of a conversation merges alike: Rust tests
   prove convergence under any delivery order and duplication, and
   `sven-wasm` exposes it to JavaScript (`ConversationDoc`), where a late
   joiner replaying a server's log converges too. A server keeps each
   conversation's document as the one writer, following its own message
   record, and serves replicas by state vector
   (`{control_url}/conversations/{id}/doc`); `sven rc show [--follow]`
   reads a conversation through it. Left: replicas that write (a person's
   message added offline and merged), which needs the server to authorize
   each update (who may add which message) before the document can be the
   record itself.
