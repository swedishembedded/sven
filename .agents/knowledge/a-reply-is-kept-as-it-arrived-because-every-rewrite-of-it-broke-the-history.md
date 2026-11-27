<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# A reply is kept as it arrived, because every rewrite of it broke the history

Swedish Embedded AB implements agent runtimes whose recorded sessions can be trained on as they ran for its clients.
If your team needs expertise in agent trajectories as training data then you can procure our services by sending an
email to info@swedishembedded.com.

**Lesson:** sven never changes a model's reply. Each rewrite it used to make (arguments re-serialised from their
parsed value with sorted keys, JSON repair, `{}` for arguments that did not parse, a call ended early at its first
parsing brace, trimmed reasoning, a think block or `<invoke>` markup moved out of the text, an `[aborted]` assistant
message standing in for a cut reply) made the next request differ from what the model had generated. A session was
then never append-only, so it could not be trained as one record, and the server's prefix cache missed. An opt-in
option to stop some of the rewrites (this note's first version, `agent.history_as_generated`) was replaced by the one
standard behaviour: the reply is an immutable `SampledReply` (text, reasoning, each call's argument text, the
server's `sampled` record as the exact JSON it sent, the finish), filed and sent back unchanged; parsed arguments only
run tools.

## Rules now

- A call whose arguments are not a JSON object runs nothing; its result names the parse failure; the history keeps
  the text. A call the output limit cut is answered as cut. An empty argument text is a call with no arguments.
- A reply cut off mid-stream (a loop stopped by the watchdog, a deadline, a cancel) is not filed; a user-role
  `[aborted: <reason>]` note is, and a loop retry appends its own note. The request's record keeps what was sampled.
- A reply's reasoning and its `sampled` record go back to every server; a provider that refuses either says so.
- A request that rewrites what the thread sent (system prompt, tools, or a history that is not the previous one
  extended, as after a compaction) emits `ContextRewritten{segment, cause}`; a conditional instruction is announced
  once as an appended note instead of in the system prompt, and the dynamic system suffix is gone.
- A deferred tool that a session loads changes the tools a request offers: that is a segment boundary with cause
  `tools`, the standard (a trainer merges each segment on its own); the tool list is not frozen across it.
- Every driver sends the reply back verbatim: OpenAI-compatible HTTP and D-Bus alike, and a conversation file's
  `## Tool` envelope keeps the argument text.
- A trajectory keeps each call's argument text (`extra.raw_arguments`) and the record (`extra.sven.sampled`), so a
  session resumed from it sends the same history byte for byte.

## Evidence (2026-11-27/28)

- 178 recorded vim sessions (59 Qwen3-4B, 59 Qwen3-8B, 60 Qwen3.8-27B-FP8): arguments came back as generated in
  383/452 (4B), 370/462 (8B) and 3/406 (27B) calls before. The deleted JSON repair rescued 10 of 455 calls (4B, in 9
  of 60 sessions), 1 of 483 (8B) and none of 422 (27B); those calls are now answered with the parse failure and the
  model resends them. The sample holds no verdicts, so the pass-rate effect is not measured.
- End to end against a CPU brain (Qwen3-0.6B, `tests/e2e/basic/28_brain_replay.bats`): a two-call tool session
  reported `brain.history.append_only` on both calls, and the second call's prompt was served 3104 of 3196 ids from
  the prefix cache (the first prompt was 3039 ids).
- `crates/turn/tests/it/reply_round_trip.rs`: 60 generated replies streamed in arbitrary chunks come back in the next
  request byte for byte (text, reasoning, ids, names, argument text, `sampled`).
