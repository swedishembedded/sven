<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# suspend-resume - suspend, resume, replay

A run that is stopped by a bound is not lost. Here a report is read by a
command gate (`cat report.txt`); if the run is stopped while the gate is
running (a deadline, a token budget, SIGINT or SIGTERM), it is suspended, and
the checkpoint it leaves resumes under the same program and finishes.

What it shows:

- A deadline (`--timeout`), a token budget (`--max-output-tokens`) or a signal
  ends a run `suspended` (exit 3), with a checkpoint (`--checkpoint-out`),
  rather than failed. The harness is pinned by digest: a checkpoint from
  another program is refused.
- The interrupted gate runs again on resume, and costs neither a repair
  attempt nor a second start of the step.
- Replay: a recording (`--record`) is replayed offline - nothing asks a model
  or runs a tool - and must end in the same verdict and trace.

## Run it

```sh
sven harness validate samples/harnesses/suspend-resume
sven harness run    samples/harnesses/suspend-resume --workspace DIR \
    --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 --approvals auto \
    --timeout 1 --record run.json --trace trace.jsonl --checkpoint-out checkpoint.json
# exit 3 if the deadline stopped it first: suspended, with a checkpoint
sven harness resume samples/harnesses/suspend-resume --checkpoint checkpoint.json \
    --workspace DIR --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
    --approvals auto                                     # exit 0: completed
sven harness replay samples/harnesses/suspend-resume --recording run.json \
    --expect-trace trace.jsonl                           # offline; exit 0 when it matches
```

`DIR` holds a `report.txt`. `sven harness status samples/harnesses/suspend-resume
--checkpoint checkpoint.json` says where a suspended run rests.
