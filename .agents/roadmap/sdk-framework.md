# sdk-framework

**Status: planned.** What an application embedding sven through `sven-sdk`
still cannot do, found by building the brain-linked learning applications
(now Splinter, a separate repository) on the facade. Each item below is a
gap in the facade, not in that application.

The acceptance test: an application provides one model and a small toolset,
runs a bounded task, handles human input, suspends and resumes it, and gets
back a trustworthy outcome and trajectory - without inheriting sven's coding
environment and without reaching past `sven-sdk`. A standalone sample in
`samples/` demonstrates exactly that, with no brain dependency, so sven
proves agent execution, verification and trajectory capture on its own.

## Gaps

- **Tools are opt-out, not opt-in.** `EngineBuilder::tool()` adds to the
  built-in registry, and every turn assembles the default coding toolset and
  MCP wiring through `RuntimeBuilder`. The default should be an empty
  registry, with coding/research presets layered explicitly on top. A
  closed-book question ("answer from memory") needs a run with no tools at
  all, which the facade cannot express today.
- **Every `send()` rebuilds the runtime.** Session resources (buffers, MCP
  connections, default provider construction) are rebuilt per turn instead
  of living for the session.
- **Approval is all-or-nothing** (`Deny` / `AutoApprove`). The facade needs
  asynchronous approval and question answering.
- **No structured outcome.** A caller cannot tell completed, failed,
  cancelled, waiting for input and budget-exhausted apart without parsing
  text. Budgets (tokens, steps, tool calls, children) and cancellation with
  deadlines belong on the facade.
- **History is rebuilt from a lossy stream.** The SDK reconstructs the
  transcript from broadcast observations and skips `RecvError::Lagged`,
  which is acceptable for progress display and wrong for the authoritative
  record. Final history and trajectory must come from the session.
- **An SDK application gets no tracing.** The tracing subscriber is
  installed by the `sven` binary, not by the facade, so `RUST_LOG` has no
  effect in an embedding application, and diagnosing a kernel stall from one
  means decoding `.sven/audit.jsonl` by hand.
- **The agent loop stalled after two scripted rounds.** Driving the agent
  with a scripted model at the wire, the kernel recorded `TOOL <name>
  Started` and then nothing on the third round - no result, no further
  request - while the same loop reached eleven tool calls with a real
  provider. Not the tools (each hung command runs instantly via `sven tool
  call`) and not the scripted server (a unit test drives it through six
  requests). The difference is in what a real provider's stream carries and
  the scripted one omitted: a `usage` chunk, or arguments split across
  several deltas. Unverified since the kernel change that serves captures
  only after in-flight tool calls settle; re-test before assuming either way.
