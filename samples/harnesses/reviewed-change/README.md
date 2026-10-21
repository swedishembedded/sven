<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# reviewed-change - a workflow that uses another workflow

A package: `harness.yaml` makes a change and then `use`s the reviewer in
`lib/review.yaml`, an ordinary workflow of its own. `imports: {review:
./lib/review.yaml}` names the file, a path relative to the importing document
that has to stay inside the package; `use: review` with `with: {subject: ...}`
runs it as a child.

What it shows:

- The call is **checked at compile time** against the reviewer's interface:
  `with` must pass exactly the inputs the reviewer takes, `tasks.reviewed.output`
  can only name outputs the reviewer returns, and the capabilities the reviewer
  holds (here: a shell for its command gate) must be held by the caller.
- The reviewer is **pinned**: the bundle digest covers the compiled plan of
  every imported file, so a checkpoint taken with one reviewer is refused with
  another.
- The reviewer **spends under the caller's limits**: its model turns count
  against the caller's `model_turns: 6`, so the tree never spends more than the
  root declared.
- A reviewer that **fails fails the caller** (`child_failed`): its own command
  gate decides whether it approved, not what it said.
- Imports are files of the package. An absolute path, a URL, a `..` that leaves
  the package, a symlink that leads out of it, a cycle and two documents with
  one name are all refused before anything runs.

## Run it

```sh
sven harness validate samples/harnesses/reviewed-change
sven harness inspect  samples/harnesses/reviewed-change
sven harness run samples/harnesses/reviewed-change --input '{"feature": "..."}' --workspace DIR \
    --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
    --approvals auto --trace trace.jsonl
```

`DIR` is the repository the agent works in; the model is a server you already
run. The gates are the repository's own commands, so `DIR` must hold what they
run.
