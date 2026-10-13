<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# 07-custom-transform - a sandboxed helper in a bundle

Data reshaping does not belong in a model turn or a tool, and a harness has no
arithmetic of its own. `transform` runs a *helper*: a WebAssembly module that
imports nothing, touches only its own memory, and runs under a fuel and a
memory cap. The helper's bytes are part of the program, so they travel with
the document in a bundle and are pinned by one digest.

## Files

- `document.yaml` - the harness document. Its `prep` step is
  `transform: { helper: reshape, ... }`.
- `bundle.yaml` - **generated**: `{ document: <document.yaml>, helpers: { reshape: ... } }`
  with the module base64-encoded. This is the file the CLI and the tests load
  (a sample directory holds exactly one of `harness.yaml` and `bundle.yaml`).
- `cases/` - two cases that route an order through the helper.

## The helper

ABI `helper-run/1`: the module exports `run(input_ptr: i32, input_len: i32) -> i64`
and one memory. The host writes the input as JSON at `input_ptr`; the helper
returns `(out_ptr << 32) | out_len`, the range of its JSON output. A fresh
instance serves every call, so a helper cannot keep state between calls.

The module in this sample is the *identity*: it copies its input to its output
slot byte for byte. That is deliberate. It keeps the module small enough to be
authored without a toolchain and shows the whole plumbing - the step's input
is rendered from run data, crosses the sandbox boundary as JSON, and comes
back as `facts.shaped`, which the branch then reads. A real helper
would compute something (a total, a threshold, a reshaped record) between the
same two boundaries.

## Authoring without a toolchain

The module is hand-encoded by the WASM test encoder in `sven-harness-transform`.
`crates/harness-runtime/tests/sample_bundle.rs` builds it, composes the bundle
from `document.yaml`, and requires `bundle.yaml` to be byte-equal. After editing
`document.yaml`, regenerate:

```sh
SVEN_BLESS_GOLDEN=1 cargo test -p sven-harness-runtime --features transform --test sample_bundle
```

## Run it

```sh
sven harness validate samples/harnesses/07-custom-transform
sven harness inspect  samples/harnesses/07-custom-transform
sven harness test     samples/harnesses/07-custom-transform
```

`sven harness test` runs every file under `cases/` on mock models and fixture
tools; nothing reaches a real model. The same cases run in the repository's
tests, together with the other harness samples:

```sh
cargo test -p sven-harness-testkit --test samples
```

## Expected

`reshapes-and-routes` completes with `{"queue": "express", "customer": "acme"}`;
`second-route` with `{"queue": "standard", "customer": "globex"}`.
