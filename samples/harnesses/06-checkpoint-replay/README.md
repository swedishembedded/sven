<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# 06-checkpoint-replay - suspend, resume, replay

A run that is stopped by a bound is not lost. Here a report read blocks, the
case's 300 ms deadline suspends the run, and the checkpoint it leaves resumes
under the same program and finishes.

What it shows:

- A deadline (`bounds.deadline_ms`) or token budget ends a run `suspended`,
  with a checkpoint, rather than failed.
- `resume:` in a case is a second phase on the first phase's checkpoint, with
  fresh fixtures. The harness is pinned by digest: a checkpoint from another
  program is refused.
- The resumed read is a second start of the `call` step (`steps: { call: 2 }`).
- Replay: every phase is recorded and replayed offline - nothing asks a model
  or runs a tool - and must end in the same verdict and trace.

## Run it

```sh
sven harness validate samples/harnesses/06-checkpoint-replay
sven harness inspect  samples/harnesses/06-checkpoint-replay
sven harness test     samples/harnesses/06-checkpoint-replay
sven harness run      samples/harnesses/06-checkpoint-replay --tool-fixture <tools.yaml> --timeout 1 \
    --record run.json --checkpoint-out checkpoint.json
sven harness resume   samples/harnesses/06-checkpoint-replay --checkpoint checkpoint.json --tool-fixture <tools.yaml>
sven harness replay   samples/harnesses/06-checkpoint-replay --recording run.json
```

`sven harness test` runs every file under `cases/` on mock models and fixture
tools; nothing reaches a real model. The same cases run in the repository's
tests, together with the other harness samples:

```sh
cargo test -p sven-harness-testkit --test samples
```

## Expected

`suspended-then-resumed` passes: phase one is `suspended` after one `call`
start, phase two is `completed` with `{"report": "all green"}`.
