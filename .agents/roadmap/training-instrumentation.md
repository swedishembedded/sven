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
output-token ceiling.

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
7. Step metrics are lost across runs of one `Agent` (M, found 2026-11-17). When a run's history does
   not line up with the kernel's thread (`take_history` returns `events_intact = false`; observed
   after a run stopped by the caller's cancel), `unplace_requests` takes every earlier request off
   its step too, so the token ids, log-probabilities and usage of every earlier run vanish from
   `Agent::trajectory()` although the server returned them. Measured with a logging proxy in front of
   a brain server: 42 of 42 replies carried `policy_trace`, yet 13 of the 42 steps of an embedding
   application's 8 episodes had no metrics; in each, the untraced steps were exactly those before the
   last run that ended intact. Fix: keep earlier requests placed while the thread still begins with
   the record they were placed on (no compaction), and give a repro with a cancelled first run.
