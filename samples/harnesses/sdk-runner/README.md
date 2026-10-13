<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# sample-harnesses-sdk-runner - the cases, through the SDK alone

An application does not need the test kit to run a harness. This crate depends
on `sven-sdk` and nothing else from this workspace: it loads a harness
directory's `harness.yaml` on an `Engine` with an injected model, starts a run
per case, executes it, and compares the verdict with the case's `expect`.

The model is a 60-line `ModelProvider` that serves the case's `model.sequence`
one answer per call (`src/model.rs`). That is all the "mock" a text-only case
needs.

It is minimal on purpose. A case that needs tools, `rules`, a `live` model, a
workspace, bounds or a resume phase is refused with a message pointing to the
full runner:

```text
case `happy` uses `tools`, which this runner (public SDK only) does not carry; run it with `sven harness test <dir>`
```

## Run it

```sh
cargo run -p sample-harnesses-sdk-runner -- samples/harnesses/01-decision
cargo run -p sample-harnesses-sdk-runner -- samples/harnesses/02-model-routing
cargo test -p sample-harnesses-sdk-runner
```

## Expected

```text
PASS bug
PASS feature
PASS malformed-then-valid
PASS unroutable
4 passed, 0 failed
```

The exit code is 0 when every case passes, 1 when one fails, 2 when a case or
harness cannot be run.
