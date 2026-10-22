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
trajectory; the round trip against a real server (`make test/e2e/capture`).

## Still open

1. Per-tool duration and time to first token on the step (S).
2. A mid-session model change recorded per step, not only as a notice (S).
3. The `Agent` trajectory embeds no children: an SDLC task child's record is collected by the
   spawner (`ChildRecords`) but only a harness run exports it; an `Agent::trajectory()` over an
   `sdlc` run should embed them the same way (M).
4. A per-request cost on `extra.sven.requests` (today the step carries the summed cost).
5. D-Bus provider: prompt ids and the server's identity echo (the HTTP driver has them).
6. File tools rooted in a given directory (M), so an embedding application can run an agent
   against a copy of a repository without a sandbox shell.
