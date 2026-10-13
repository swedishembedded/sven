<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# 02-model-routing - one model turn, then a branch

The smallest useful use of a model: ask one question whose answer is
constrained by a JSON Schema, and let ordinary steps decide what to do with it.
The model classifies a ticket as `bug`, `feature` or `question`; a branch
sends bugs to the `bugs` queue and everything else to the `backlog`.

What it shows:

- `predict` with a `schema`: the answer must parse as JSON, and a malformed
  answer is a failed step.
- `retry` around it: two attempts, and the plan's own `fail` step
  (`unroutable`) when both are spent. `max_model_turns` has to cover every
  attempt.
- The model never decides control flow; it only fills a fact the branch reads.

## Run it

```sh
sven harness validate samples/harnesses/02-model-routing
sven harness inspect  samples/harnesses/02-model-routing
sven harness test     samples/harnesses/02-model-routing
sven harness run      samples/harnesses/02-model-routing --input '{"ticket": "crash on start"}' \
    --case samples/harnesses/02-model-routing/cases/bug.yaml
```

`sven harness test` runs every file under `cases/` on mock models and fixture
tools; nothing reaches a real model. The same cases run in the repository's
tests, together with the other harness samples:

```sh
cargo test -p sven-harness-testkit --test samples
```

The SDK-only runner runs the same cases from an application, with no test kit:

```sh
cargo run -p sample-harnesses-sdk-runner -- samples/harnesses/02-model-routing
```

## Expected

```text
PASS bug
PASS feature
PASS malformed-then-valid
PASS unroutable
4 passed, 0 failed
```

`bug` completes with `{"queue": "bugs", "label": "bug"}` after one model turn;
`malformed-then-valid` starts `ask` twice and still completes; `unroutable`
ends `failed` with code `unroutable`.
