<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# 01-decision - pure branching

A harness with no model, no tool and no capability: the run is a walk over
`assign` and `branch` steps, so the same input always takes the same path and
a case can state the exact steps it took.

The harness picks a discount tier from a plan and a vip flag:

| plan | vip | tier |
|---|---|---|
| enterprise | yes | gold (20) |
| pro, or any plan when vip | | silver (10) |
| anything else | | standard (0) |

Conditions are paths (`inputs.vip`), `eq`/`ne` against a literal, and `all`,
`any` and `not` over those. There are no ordering comparisons: a threshold on
a number belongs in a helper (see [07-custom-transform](../07-custom-transform/)).

## Run it

```sh
sven harness validate samples/harnesses/01-decision
sven harness inspect  samples/harnesses/01-decision
sven harness test     samples/harnesses/01-decision
sven harness run      samples/harnesses/01-decision --input '{"plan": "pro", "vip": false}'
```

`sven harness test` runs every file under `cases/` on mock models and fixture
tools; nothing reaches a real model. The same cases run in the repository's
tests, together with the other harness samples:

```sh
cargo test -p sven-harness-testkit --test samples
```

## Expected

The `run` above ends `completed` with output
`{"tier": "silver", "discount": 10}` after four steps (`top`, `middle`,
`silver`, `finish`). `test` reports four cases, all passed.
