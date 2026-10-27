# remote-control

Scope: `sven auth login` and `sven rc` (`sven-remote`), the HTTP binding in
`sven_control::remote`, and running sven's agent core in a browser.

**Status:** a host signs in to a server, takes commands over a server-sent
stream, runs one session actor per conversation and uploads every session
event; proven against a real server end to end.

## Left

0. **Hosted mode** (decided 2026-10-27). A server runs `sven rc --hosted`,
   one per account, to answer conversations that have no machine of the
   person's: the same host and session actors, with a fixed safe tool set
   (web search, finding skills; no shell, no file tools) and a scratch
   working directory. Browsers and attached terminals only view and send
   input. Needed here: the `--hosted` tool allowlist and its tests.

1. **Durable sessions.** Done: each session's own conversation (tool calls
   included) is kept after every finished tool call and turn, and reopened
   from there after a restart (`sven_remote::sessions`). A turn stopped
   mid-run keeps its work; a cut-off call is reported as interrupted, never
   re-run; the server's retry continues the turn. A model call in flight is
   simply asked again: it has no effect to repeat.
2. **Approvals through the server.** Done: a tool that asks first asks
   the server (`ToolNeedsApproval`) and waits for `ApproveTool`/`DenyTool`,
   denied after ten minutes of silence (`sven_remote::approvals`).
3. **Attached sessions.** `sven rc` owns each session's executor and takes
   no input of its own (watch-only). `sven rc attach` is a terminal on a conversation from
   anywhere: it reads the shared document live and writes through the
   server (`{control_url}/conversations/{id}/messages`), so every client
   writes through the same authorized path. `--tui` shows it in the
   full-screen UI through an injected agent task
   (`sven_remote::attach::remote_agent_task`, `AppOptions::agent_task`)
   that turns the document's changes into the session events the UI
   already renders, asks about a tool call waiting for permission in the
   UI's own question modal (decided through
   `{control_url}/conversations/{id}/approvals`, withdrawn when decided
   elsewhere), and shows the conversation's model (the document's
   `meta.model`).
4. **Browser sessions.** A whole agent turn runs as wasm with the same
   observable sequence as natively, and `sven-wasm` exposes a session to
   JavaScript (`make build/wasm`, `make test/wasm`). A page gives its
   agent tools (declared in the options, run by one JavaScript handler,
   allowed exactly their declared effects; `crates/wasm/tests/session.mjs`
   proves the round trip). This is also how an agent served from a site
   gets its tools: they run in the browser, where the agent runs, and
   reach the site's backend only through its API as the signed-in person;
   there is no server-side agent. A consuming application runs the package
   and its page tools in a real browser worker in its own end-to-end
   suite, and `make test/wasm/browser` proves the same in a real headless
   Chrome from this repository (a module worker, a page tool, a fake model).
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
