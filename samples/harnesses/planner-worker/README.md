<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# planner-worker - three roles, three ways of being sampled

A v2 workflow with a planner, a worker and a supervisor.

What it shows:

- A **`model_only` task** is one model turn with no tools, in a conversation of
  its own. The planner answers in a JSON schema; a malformed answer ends the run
  (`invalid_answer`) instead of feeding prose downstream, and the worker's prompt
  reads the plan's field, `{{ tasks.plan.output.goal }}`.
- **Decoding settings per role**: the agent sets a default temperature and top_p;
  the planner replaces the temperature (0, with a seed) and turns thinking off;
  the supervisor turns it on with a token budget. Every request records the
  settings it was sent with, in the trajectory.
- A **gate on a model-only role**: the supervisor's review must pass
  `check_review.sh`; a review that leaks is refused by the exit status and the
  supervisor is asked again, shown why (`{{ failure.detail }}`).

As in the other samples, the cases fake `shell_gate`; a real run has the real
tool on every host.

Cases: `all-three-roles`, `a-leaking-review-is-asked-again`,
`a-plan-outside-its-schema-fails`.

## Run it

```sh
sven harness validate samples/harnesses/planner-worker
sven harness test     samples/harnesses/planner-worker
```
