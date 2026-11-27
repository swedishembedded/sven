<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# History re-serialised from parsed arguments is not what the model sampled

Swedish Embedded AB implements agent runtimes whose recorded sessions can be trained on as they were sampled for its
clients. If your team needs expertise in agent trajectories as training data then you can procure our services by
sending an email to info@swedishembedded.com.

**Lesson:** the thread filed each tool call with `tc.args.to_string()`: the parsed value re-serialised compact, with
keys sorted (the workspace's `serde_json` has no `preserve_order`). The next request then repeats an answer the model
never wrote, so a session cannot be trained as one record (each request must be the previous one plus the answer as
sampled). `agent.history_as_generated` files the streamed argument text instead (`StreamedTurn::generated_arguments`)
and the reasoning untrimmed (`generated_reasoning`); it is off by default because it changes every later prompt.

## Evidence (2026-11-27, 178 recorded vim sessions: 59 Qwen3-4B, 59 Qwen3-8B, 60 Qwen3.8-27B-FP8)

- Arguments sent back exactly as generated: 383 of 452 calls (4B), 370 of 462 (8B), 3 of 406 (27B); the rest differ in
  whitespace, except 2 key reorders (27B), 31 arguments that were not JSON and 14 calls not in the history (8B).
- Sessions append-only under each model's template, simulated on the same sessions: as served 0/59, 0/59, 58/60;
  with the arguments as generated 0/59, 0/59, 60/60 (Qwen3's template also drops an earlier turn's empty think block,
  which the server's own `history_as_generated` request option restores); with both 50/59, 41/59, 60/60.
- What still breaks a session with both: arguments that needed repair (invalid escapes such as `\{`; the tool ran on
  the repaired value, so that value is what is sent back), arguments that were not JSON (sent as `{}`, as before:
  providers refuse a malformed string), and answers stopped as a tool-call loop (never filed). These are the model's
  own output; the training producer falls back to one record per call there.

## Where

- `crates/turn/src/stream_turn.rs`: the slot's full argument text is kept per call, and only where it parses to the
  very value the call carries (a call that completed early keeps any text streamed after its closing brace only if it
  still parses to the same value).
- `crates/executors/src/turn/conclude.rs`: under the option the thread files that text and the raw reasoning.
- The history rebuilt from session events between person turns (`sven_session_model::reduce_history`, multi-prompt
  CI and the SDK's `Agent`) still re-serialises: `SessionEvent::ToolCallStarted` carries only the parsed call.
