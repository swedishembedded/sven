<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# 03-tool-agent - least privilege across a delegation

`triage` answers a question from the notes. It does not read them itself: it
delegates to the child harness `reader` (`children/reader.yaml`), and the
authority narrows at each step.

- `triage` declares `SpawnChild` and `ReadFile`.
- `reader` declares `ReadFile` only. A child never holds more than its own
  document declares, whatever its parent holds.
- `reader`'s agent step lists `tools: [read_notes]`. The list is an enforced
  allowlist: a call to any other tool is refused before it runs, and the
  model is told so.

Cases: `happy` (the agent reads, then answers); `proposes-a-tool-outside-its-
allowlist` (the model asks for `write_notes`; nothing is written, and the case
checks both that the fixture was never invoked and that no `WriteFile`
capability was exercised); `tool-error-is-reported` (a failing tool goes back
to the model, not to the run's verdict).

A fixture tool must be honest: its declared `capability` has to equal the one
its name implies (`read_*` is `ReadFile`, `write_*` is `WriteFile`), or the
case errors.

## Run it

```sh
sven harness validate samples/harnesses/03-tool-agent
sven harness inspect  samples/harnesses/03-tool-agent
sven harness test     samples/harnesses/03-tool-agent
```

`sven harness test` runs every file under `cases/` on mock models and fixture
tools; nothing reaches a real model. The same cases run in the repository's
tests, together with the other harness samples:

```sh
cargo test -p sven-harness-testkit --test samples
```

This sample uses tools, so the SDK-only runner refuses it and points to
`sven harness test`.

## Expected

Three cases, all passed. In `happy` the run completes with
`{"answer": "The deploy window is Friday."}` and the fixture log shows exactly
one `read_notes` call.
