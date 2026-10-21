<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# module-sweep - a long job that can be interrupted

A workflow with three tasks: a planning task that leaves `backlog.json`, a
task that runs once per module in it, and a task that repeats until coverage
reaches a threshold.

What it shows:

- **`for_each`** reads its collection once, when the loop begins, and freezes it
  in the run state with each item's id, value and digest. The agent can rewrite
  `backlog.json` afterwards and nothing changes
  (`backlog_mutation_does_not_skip_items`). Each module is verified by the test
  command before the next begins; an item that fails after its repair stops the
  sweep there and the failure names it.
- **Progress is data.** The run state records every item's status, so a
  checkpoint resumes at the module it was on: finished modules are not run again,
  the backlog is not read again, and the interrupted test run is run again
  (`interrupted-sweep-resumes-at-the-module`).
- **`until`** repeats a task while a metric is unmet. The figure is measured by a
  fresh run of the coverage command after every pass, and a bound ends a loop that
  never gets there with `until_exhausted` instead of an endless run.

A real run needs the host's tools: `shell_gate` and `read_json_snapshot` are
provided by every `sven harness run`.

## Run it

```sh
sven harness validate samples/harnesses/module-sweep
sven harness inspect  samples/harnesses/module-sweep
sven harness run samples/harnesses/module-sweep --workspace DIR \
    --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
    --approvals auto --trace trace.jsonl
```

`DIR` is the repository the agent works in; the model is a server you already
run. The gates are the repository's own commands, so `DIR` must hold what they
run.
