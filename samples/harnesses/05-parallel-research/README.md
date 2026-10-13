<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# 05-parallel-research - fan out, join in order

`parallel` starts two branches at once (`max_parallel: 2`) and continues at its
join when both have returned. Branch `history` is one model turn; branch
`sources` calls a slow search tool and then summarizes its hits. The join
reads `steps.split.value`: the branches' return values **in declared order**,
whichever finished first.

What it shows:

- `parallel` needs `SpawnChild`, and each branch runs as its own child run
  with its own tool calls and verification.
- Branch order is not defined, so the case's model is `rules` (stateless,
  matched on the prompt) rather than `sequence`.
- Cancelling the run stops branches that are still working: in
  `cancelled-with-children-active` the search tool never returns, the case
  cancels after 100 ms, and the run ends `cancelled` with nothing left running
  (the test kit fails a case that leaks a fixture call).

## Run it

```sh
sven harness validate samples/harnesses/05-parallel-research
sven harness inspect  samples/harnesses/05-parallel-research
sven harness test     samples/harnesses/05-parallel-research
```

`sven harness test` runs every file under `cases/` on mock models and fixture
tools; nothing reaches a real model. The same cases run in the repository's
tests, together with the other harness samples:

```sh
cargo test -p sven-harness-testkit --test samples
```

## Expected

`joins-in-declared-order` completes with
`{"history": "Tide tables date from antiquity.", "sources": "Two almanacs agree."}`.
`cancelled-with-children-active` ends `cancelled`.
