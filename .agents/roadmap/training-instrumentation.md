# Training-grade trajectories - roadmap

Scope: What a trajectory records so an application that trains models can learn from it and a run can be reproduced.

What a trajectory must record so that an application that trains models can learn from it
without re-deriving anything, and so a run is reproducible. Update this file in the same
commit that finishes or changes an item; finished items move to
[`docs/internals/trajectories.md`](../../docs/internals/trajectories.md).
Nothing here makes an agent depend on any trainer: sven records, applications consume.

## Done (see the doc)

Sampling and sampled-token capture on the model seam and from `RunOptions`; prompt and
completion token ids, log-probabilities, the seed the server used, adapter, base-checkpoint
digest, backend and engine per step (and per request when a step covers several);
whole-conversation record under compaction; append-only journal with an incomplete marker and
a kill test; children embedded in a harness run's trajectory; mock-stream reconstruction from a
trajectory; the round trip against a real server (`make test/e2e/capture`); a brain server's
`policy_trace` (asked for with `policy_trace: true`: prompt and generated ids, whole-vocabulary log-probs and their
temperature), the policy version, tokenizer and chat-template digests and the log-probability kind
(`policy`/`sampler`) a server reports, and a `truncated` mark on a response cut off at the
output-token ceiling; every run of an agent keeps its metrics, whatever ended the runs before;
which requested sampling controls took effect (`sampling_honoured`), with a control a provider
does not honour refused or, on explicit best effort, recorded as `sampling_unhonoured`.

## Still open

1. Per-tool duration and time to first token on the step (S).
2. A mid-session model change recorded per step, not only as a notice (S).
3. The `Agent` trajectory embeds no children: an SDLC task child's record is collected by the
   spawner (`ChildRecords`) but only a harness run exports it; an `Agent::trajectory()` over an
   `sdlc` run should embed them the same way (M).
4. A per-request cost on `extra.sven.requests` (today the step carries the summed cost).
5. (Done 2026-11-13.) The D-Bus provider asks for brain's `policy_trace` and reads it and the
   identity echo as the HTTP driver does.
6. File tools rooted in a given directory (M), so an embedding application can run an agent
   against a copy of a repository without a sandbox shell.
7. (Done 2026-11-17.) Step metrics lost across runs of one `Agent`. Cause: a result the machine
   answered (a refused, unapproved or unloaded call, and a call a stopped run left open) entered
   the kernel's thread without an observation, so `take_history` saw one message more than the
   run folded, judged events lost and took every request of every run off its step. Fixed in
   the turn executor (it observes those results), the SDK (a stopped run answers its open calls)
   and `take_history` (lost events replace only the run's own part of the record). See the doc,
   "Every run of an agent keeps its metrics".
8. Headless ATIF fidelity: a step records a tool's raw output, not the text the model was shown (output limits, truncation and
   framing differ). A trainer that rebuilds contexts from the trajectory therefore differs from what was sampled, and has to read the
   request log (`sven.request-log/1`) instead. Record the model-shown observation on the step. Acceptance: for a recorded run,
   every step observation equals the matching tool message of the next request.
9. Fork replay drift (`sdk/src/fork.rs`): a fork replays the recorded prefix and refuses when the world answers differently. On a
   real pass over 1275 trials (543 failures) 260 forks were refused: calls with garbled arguments (now replayed as the runtime
   answered them), a continuation whose prefix no longer matches, a mid-run system turn or a final reply, host-versus-sandbox
   paths, `ls -l` owner and time fields, shell durations, error text that differs between builds, and nondeterministic output.
   Next: replay with the recorded build and normalise the remaining nondeterministic fields. Acceptance: under 10% refused on a
   real pass of at least 500 failures.
10. A cost cap per run, so that arms can be compared at equal cost. The completion-token and turn caps landed 2026-11-21
    (`docs/reference/configuration.md#run-budget`).
11. Done 2026-11-18/19: headless sampling flags (`--temperature --top-p --top-k --seed --logprobs`) honoured or refused; the
    request log declared with its schema and counts; compaction notes no longer send an unbounded thinking budget (brain refused
    them with 400); fork comparison ignores owner, group and duration fields; a shell launcher and trimmed observations in replay.
12. (Done 2026-11-24.) Per-request identity: every entry of `extra.sven.requests` carries the identity its own response
    reported, and `final_metrics.extra.sven.policy_version_span` is `[oldest, newest]` over all requests. See the doc.
13. (Done 2026-11-24.) The sampler's top-K per generated position: `RunOptions::top_logprobs(K)` and `--top-logprobs K`
    ask a brain server's policy trace for it (`policy_trace: {"top_log_probs": K}` over HTTP; `policy_trace` plus
    `policy_trace_top_log_probs` over D-Bus) and record `generation_top_ids` / `generation_top_log_probs` per request;
    a reply without them fails the turn and another provider refuses. See the doc.
14. (Done 2026-11-24.) The HTTP and D-Bus providers record the same request metadata: one brain reply (ids, sampled and
    top log-probabilities, kind and temperature, identity, truncation) served over each is folded to the per-request
    record and compared field by field (`model-drivers/tests/it/provider_contract.rs`, with the `dbus` feature).
15. (Done 2026-11-25.) The run-level policy identity a brain server reports beside the serving version
    (`brain.adapter.lineage`: run, epoch, optimiser update, base and adapter digests) is recorded as `policy_lineage` per
    request in `extra.sven.requests[]` and on a single-request step, through the one identity parser both providers use;
    a malformed one is logged and left unknown, never half-filled. The provider contract test covers it over HTTP and
    D-Bus; `session-store/tests/it/request_identity.rs` holds the per-request and single-step records.
16. (Done 2026-11-25.) The sampler's support per generated position: `RunOptions::sampler_support(N)` and
    `--sampler-support N` (1..=256) ask a brain server's policy trace for it (`policy_trace: {"support": N}` over
    HTTP, beside `top_log_probs` when both are set; `policy_trace` plus `policy_trace_support` over D-Bus) and record
    `support_cap`, `generation_sampler_log_probs`, `generation_support_offsets` and `generation_support_ids` per request
    and per step (`sven_vocab::SamplerSupport`, one parser for both providers); a reply without them, or with a
    malformed or inconsistent one, fails the turn, and another provider refuses. The provider contract test covers it
    over HTTP and D-Bus. See the doc.
17. (Done 2026-11-27.) History as generated: `agent.history_as_generated: true` (sven config or
    `sven config set agent.history_as_generated true`; an embedding application sets `config.agent.history_as_generated`)
    sends every earlier answer back as the model wrote it, each call's argument text byte for byte and the reasoning
    untrimmed; default off. A server whose chat template rewrites earlier turns needs its own option too, passed through
    `model.driver_options` (a brain server: `{"chat_template_kwargs": {"history_as_generated": true}}`). With both, 50/59
    (4B), 41/59 (8B) and 60/60 (27B) recorded vim sessions are append-only, from 0, 0 and 58 (knowledge
    `history-re-serialised-from-parsed-arguments-is-not-what-the-model-sampled.md`). Open: the history rebuilt from
    session events between person turns (`reduce_history`) still re-serialises.
