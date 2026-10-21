<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# ticket-routing - schema-constrained model turns, then a condition

The smallest useful use of a model: ask one question whose answer is
constrained by a JSON Schema, and let the workflow decide what to do with it.
The model classifies a ticket as `bug`, `feature` or `question`; only a bug
earns a second model turn, a one-line summary for the bugs queue.

What it shows:

- A `model_only` task with a `schema`: the answer must parse as JSON of that
  shape, and a malformed answer fails the run with `invalid_answer`.
- `when:` on the second task reads a field of the first answer
  (`tasks.classify.output.label`); a skipped task leaves an empty output.
- `execution.limits.model_turns` has to cover every turn the workflow can take.
- The model never decides control flow; it only fills values the conditions read.

## Run it

```sh
sven harness validate samples/harnesses/ticket-routing
sven harness run samples/harnesses/ticket-routing --input '{"ticket": "crash on start"}' --workspace DIR \
    --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
    --approvals auto --trace trace.jsonl
```

`DIR` is the directory the agent works in; the model is a server you already
run. The trace and the final report say what the run did.
