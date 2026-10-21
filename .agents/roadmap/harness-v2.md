# harness v2

**Status: the language, the runtime, the CLI, the samples and the docs are
done (ADR 0005, `docs/reference/harnesses.md`, `docs/guides/harnesses.md`).
One item of the "done when" list is not fully met and keeps this file alive.**

## Left to do

- [ ] `embedded-hardening` has not yet completed end to end against a live
  model. The full pipeline is proven offline (three mock cases through
  `sven harness test`, and `tests/embedded_hardening_fixture.rs`, which turns
  every real gate green with the reference fix), and one live run on the local
  27B got through bootstrap, plan and two of three modules, each accepted only by
  the pinned gates, before the model server answered a repair turn with empty
  replies and the run failed with `side_effect_failed` (37 minutes; evidence in
  the lane run directory). Run it again on a healthier server, or make an empty
  model reply in a repair turn retry instead of ending the run, and when a live
  run reaches `completed` delete this file.

## Decisions taken

- Repair is unrolled, not a `retry` (a verdict is not a step failure; ADR 0005).
- Invalidation of a completed item by a changed *gate* is not a feature: a
  changed gate is a changed plan and a checkpoint from the old one is refused.
  Changed *files* and a changed *backlog* are (`covers`, `reconcile: refresh`).
