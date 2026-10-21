<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# planner-worker - three roles, three ways of being sampled

A workflow with a planner, a worker and a supervisor.

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

## Run it

```sh
sven harness validate samples/harnesses/planner-worker
sven harness inspect  samples/harnesses/planner-worker
sven harness run samples/harnesses/planner-worker --input '{"feature": "..."}' --workspace DIR \
    --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
    --approvals auto --trace trace.jsonl
```

`DIR` is the repository the agent works in; the model is a server you already
run. The gates are the repository's own commands, so `DIR` must hold what they
run.
