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
sven harness run      samples/harnesses/06-checkpoint-replay --tool-fixture blocked.yaml --timeout 1 \
    --record run.json --checkpoint-out checkpoint.json     # exit 3: suspended
sven harness resume   samples/harnesses/06-checkpoint-replay --checkpoint checkpoint.json \
    --tool-fixture answered.yaml                           # exit 0: completed
sven harness replay   samples/harnesses/06-checkpoint-replay --recording run.json
```

`sven harness test` runs every file under `cases/` on mock models and fixture
tools; nothing reaches a real model. The same cases run in the repository's
tests, together with the other harness samples:

```sh
cargo test -p sven-harness-testkit --test samples
```

A tool fixture file is the `tools:` block of a case. `blocked.yaml` is a read
that never answers, `answered.yaml` one that does:

```yaml
# blocked.yaml                       # answered.yaml
read_report:                         read_report:
  capability: ReadFile                 capability: ReadFile
  responses: [ { block: true } ]       responses: [ { ok: "all green" } ]
```

`--timeout 1` suspends the first run after a second, `--checkpoint-out` keeps
its checkpoint, and `resume` continues it. Add `--trace trace.jsonl` to `run`
and `--expect-trace trace.jsonl` to `replay` to check that the replay
reproduces the run's trace.

## Expected

`suspended-then-resumed` passes: phase one is `suspended` after one `call`
start, phase two is `completed` with `{"report": "all green"}`.
