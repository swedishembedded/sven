# Training-grade trajectories - roadmap

What a trajectory must record so that an application that trains models can learn
from it without re-deriving anything, and so a run is reproducible. Update this
file in the same commit that finishes or changes an item. Nothing here makes an
agent depend on any trainer: sven records, applications consume.

## What exists (checked 2026-10-16)

- ATIF v1.7 steps carry message, reasoning, tool calls with arguments, observations,
  per-request token ledger, decision verdicts, validated `extra.sven` gate and memory
  records, reward, and embedded subagent trajectories with parent linkage.
- The schema already has `StepMetrics.prompt_token_ids`, `completion_token_ids` and
  `logprobs`, but no code fills them; `TraceStep.model_name` is never set; the served
  adapter appears only as a `ServedModelChanged` notice and is not written into a step.
- `CompletionRequest` has no temperature, top_p, seed or logprob request;
  `ResponseEvent` has no token ids or logprobs; the local-model driver sends the
  temperature from provider configuration only.
- A trajectory is rewritten as a whole file at every flush. The NDJSON step helper has
  no production caller. A trajectory with no reward means "outcome unknown"; there is no
  scanner for partial trajectories.
- No sandboxed shell: containment is the embedding application's job.

## Work, in order

1. Sampling and identity on the model seam (M): `CompletionRequest.sampling`
   {temperature, top_p, top_k, seed}, `want_logprobs`; `ResponseEvent::Tokens`
   {ids, logprobs} and `ResponseEvent::Served` {model, adapter, seed_used,
   fingerprint}; the OpenAI-compatible driver sends seed/top_p/logprobs when set and
   parses `choices[].logprobs`; mock providers emit deterministic ids and logprobs.
2. Session store fills the existing ATIF fields (M): token ids, logprobs,
   `TraceStep.model_name`, and a typed `extra.sven.sampling` record (seed, temperature,
   top_p, adapter id and digest, fingerprint) validated like the gate and memory records.
   Per-tool duration and time to first token.
3. Append-only trace writer (M): bounded queue, explicit loss policy, fsync policy,
   `flush_trace` appends instead of rewriting; a lost training-critical record marks the
   trajectory incomplete. Crash test: kill mid-run, the file parses and is flagged.
4. SDK accessors for the above (S); the `Agent::trajectory()` export carries per-step
   metrics and sampling parameters (today it omits them).
5. Per-step model switch recorded (S): a mid-session model change is visible per step.
6. File tools rooted in a given directory (M), so an embedding application can run an
   agent against a copy of a repository without a sandbox shell.
7. Housekeeping: the stale module doc in `executors/src/verify.rs`; the memory roadmap's
   ATIF item has landed.
Acceptance: `make test check`; an ATIF round trip with the new fields; a session
recorded with logprobs on a mock provider whose ids and logprobs reappear unchanged in
the trajectory.
