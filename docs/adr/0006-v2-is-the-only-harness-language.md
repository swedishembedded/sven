<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

Swedish Embedded AB implements verifiable, declarative agent workflows for its
clients. If your team needs expertise in retiring a legacy configuration
language without losing the guarantees it carried then you can procure our
services by sending an email to info@swedishembedded.com.
-->

# ADR 0006: The workflow language is the only harness language

## Status

Accepted. Implemented by `crates/harness-spec` (the workflow modules are the
crate's main path; the `v2` directory is gone), `crates/harness-runtime`,
`crates/sdk`, `crates/harness-testkit` and the `sven harness` commands. It
removes the document language of [ADR 0004](0004-programmable-harnesses.md)
and keeps the language of [ADR 0005](0005-declarative-harness-v2.md).

## Context

ADR 0004 defined a harness as a map of steps with twelve primitives
(`api_version: "1"`). ADR 0005 added a workflow language (`api_version: "2"`)
that lowers onto that step language and left version 1 working byte for byte.
Two document languages for one machine cost something every time the project
moved: each feature was specified twice (or deliberately only for version 2),
the validator carried rules that applied to one language, the schema and the
docs described both, and the samples taught the language that authors should
no longer write. Version 2 had also overtaken the original in what it could
express safely: gates the agent cannot redefine, bounded repair, frozen
loops, conversations, variables, packages. What version 1 offered that version
2 could not was a short list, and most of it was never reachable from the
workflow language at all.

## Decision

1. A harness document is a workflow. `api_version: "2"` stays a **required**
   key rather than becoming optional: a document that says `"1"` (or nothing,
   or anything else) is refused with an error that names the removed version.
   An author with an old document learns what happened, and the next revision
   of the language stays unambiguous.
2. The step language stays, but only as the internal plan a workflow lowers
   to. It is a specification type the lowering writes, not something a file
   can contain; the validator and the compiler are unchanged in purpose, and
   the plan, its digest, checkpoints, recordings and traces keep working. A
   test-only cargo feature (`testing` on `sven-harness-spec`) lets machine
   tests write plans in the step language directly; production builds cannot
   parse it. Plans carry one stamp, `harness-plan/2`.
3. The v2 modules are the crate's main path: `steps*` (the step
   specification and its validation), `workflow*` (the workflow AST and its
   checks), `lower*` and `parse`. No `v2/` directory is left.
4. Everything only version 1 could reach is removed, with its tests and docs:
   the primitives `tool`, `retry`, `parallel` and `transform`; `delegate` to a
   registered machine mode; the WASM helper runtime (`sven-harness-transform`),
   helpers in bundles, `sven harness bundle` and `sven harness helper run`;
   the `children/` directory convention; the `RetryAttempt`, `FanOut` and
   `Joined` trace records. A bundle is now a main document plus its imports.
5. What those features were for has a workflow form: a bounded `repair` instead
   of `retry`, a `for_each` instead of fan-out, a command gate instead of a
   tool step, `imports:`/`use:` instead of `children/`.

## Consequences

- One language is documented, schema-checked (`harness-document.schema.json`)
  and taught by the samples. `custom-helper`, `parallel-research` and
  `branching` are deleted because their point was a removed feature; the other
  samples are rewritten as workflows.
- Plan digests changed (the stamp and the lowering of the formerly shared
  primitives), so checkpoints and recordings made before this change are
  refused, never reinterpreted. A pinned lock of a consumer keeps working until
  it bumps; the bump needs its harness documents rewritten.
- The wire and trace vocabulary lost `RetryScheduled`, `FanOut`, `Joined` and
  the `attempt` of `StepStarted`.
- There is no sandboxed arbitrary-code step any more. A computation a workflow
  needs runs as a command gate (trusted, confined to the workspace, judged by
  its exit status) or in the host.

History is unchanged: ADRs 0004 and 0005 describe the situation when they were
written.
