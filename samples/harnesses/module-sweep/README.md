<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# module-sweep - a long job that can be interrupted

A v2 workflow with three tasks: a planning task that leaves `backlog.json`, a
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

As in [verified-task](../verified-task/), the cases fake the `shell_gate` tool
(and `read_json_snapshot` for the backlog); a real run needs a host that
provides them.

Cases: `sweeps-every-module`, `a-failing-module-stops-the-sweep`,
`coverage-takes-two-passes`, `coverage-never-arrives`,
`interrupted-sweep-resumes-at-the-module`.

## Run it

```sh
sven harness validate samples/harnesses/module-sweep
sven harness test     samples/harnesses/module-sweep
```
