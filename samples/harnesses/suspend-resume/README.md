<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# suspend-resume - suspend, resume, replay

A run that is stopped by a bound is not lost. Here a report is read by a
command gate (`cat report.txt`) that blocks, the case's 300 ms deadline
suspends the run, and the checkpoint it leaves resumes under the same program
and finishes.

What it shows:

- A deadline (`bounds.deadline_ms`) or token budget ends a run `suspended`,
  with a checkpoint, rather than failed.
- `resume:` in a case is a second phase on the first phase's checkpoint, with
  fresh fixtures. The harness is pinned by digest: a checkpoint from another
  program is refused.
- The interrupted gate runs again on resume, and costs neither a repair
  attempt nor a second start of the step (`steps: { fetch__v0_read_a0: 1 }`).
- Replay: every phase is recorded and replayed offline - nothing asks a model
  or runs a tool - and must end in the same verdict and trace.

## Run it

```sh
sven harness validate samples/harnesses/suspend-resume
sven harness inspect  samples/harnesses/suspend-resume
sven harness test     samples/harnesses/suspend-resume
sven harness run      samples/harnesses/suspend-resume --tool-fixture blocked.yaml --timeout 1 \
    --record run.json --checkpoint-out checkpoint.json     # exit 3: suspended
sven harness resume   samples/harnesses/suspend-resume --checkpoint checkpoint.json \
    --tool-fixture answered.yaml                           # exit 0: completed
sven harness replay   samples/harnesses/suspend-resume --recording run.json
```

`sven harness test` runs every file under `cases/` on mock models and fixture
tools; nothing reaches a real model. The same cases run in the repository's
tests, together with the other harness samples:

```sh
cargo test -p sven-harness-testkit --test samples
```

A tool fixture file is the `tools:` block of a case. `blocked.yaml` is a gate
that never answers, `answered.yaml` one that does:

```yaml
# blocked.yaml
shell_gate:
  capability: ExecuteShell
  responses: [ { block: true } ]
```

```yaml
# answered.yaml
shell_gate:
  capability: ExecuteShell
  responses:
    - ok: { exit_code: 0, timed_out: false, error: null, stdout: "all green", stderr: "" }
```

`--timeout 1` suspends the first run after a second, `--checkpoint-out` keeps
its checkpoint, and `resume` continues it. Add `--trace trace.jsonl` to `run`
and `--expect-trace trace.jsonl` to `replay` to check that the replay
reproduces the run's trace.

## Expected

`suspended-then-resumed` passes: phase one is `suspended` after one `fetch`
gate start, phase two is `completed` with `{"report": "all green"}`.
