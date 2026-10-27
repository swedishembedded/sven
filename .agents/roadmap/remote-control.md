# remote-control

Scope: `sven auth login` and `sven rc` (`sven-remote`), the HTTP binding in
`sven_control::remote`, and running sven's agent core in a browser.

**Status:** a host signs in to a server, takes commands over a server-sent
stream, runs one session actor per conversation and uploads every session
event; proven against a real server end to end.

## Left

1. **Durable sessions.** Each session's own conversation (tool calls
   included) is kept after every finished turn and reopened from there after
   a restart (`sven_remote::sessions`). Left: keep the kernel snapshot too
   (`AgentState`), so a turn interrupted by the restart resumes mid-run
   instead of from its last finished turn.
2. **Approvals through the server.** Sessions run under auto approval. Send
   `ToolNeedsApproval` up and take `ApproveTool`/`DenyTool` down, with a
   per-session timeout.
3. **Terminal input.** A line typed at `sven rc` is the conversation's next
   message (`ControlEvent::UserInput`, recorded by the server before its
   answer). Left: a full TUI attached to an rc session (sharing it, not a
   copy), and a multi-writer conversation document (item 5).
4. **Browser sessions.** A whole agent turn runs as wasm with the same
   observable sequence as natively, and `sven-wasm` exposes a session to
   JavaScript (`make build/wasm`, `make test/wasm`). Left: tools a page can
   register (the `Tool` trait is still `Send`), wiring the package into an
   application's Web Worker, and running `test/wasm` in a real browser
   worker once a WebDriver is available in CI.
5. **Shared conversation document.** `sven-session-sync` is the CRDT
   (yrs) document every replica of a conversation merges alike: Rust tests
   prove convergence under any delivery order and duplication, and
   `sven-wasm` exposes it to JavaScript (`ConversationDoc`), where a late
   joiner replaying a server's log converges too. Left: make it the
   conversation's record in a server and in the host (today the server's
   message table is), and authorize updates (who may add which message).
