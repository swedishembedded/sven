<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# 04-repair-loop - do not take the tool's word for it

A patch tool is retried while it fails (up to three attempts). Once it claims
success, an independent `verify` step checks that `fixed.txt` really exists,
and a branch on the verdict decides whether the run repaired anything.

What it shows:

- `retry` repeats a body whose *side effect fails*; a tool that reports an
  error is attempted again, up to `attempts` executions of the body in all.
- `verify` is evidence the model and the tool cannot give: the verdict is
  `passed` or `failed: <reason>`, stored under `steps.check.value`.
- The failure path is explicit: `unrepaired` is the plan's own `fail`.

Cases: `repaired-on-second-attempt` (the fixture fails once, then writes the
file inside the case workspace); `never-repaired` (three failures, the
verification never runs); `claims-success-without-writing` (the tool answers
"patched" but writes nothing - the retry is satisfied, the verification is
not).

## Run it

```sh
sven harness validate samples/harnesses/04-repair-loop
sven harness inspect  samples/harnesses/04-repair-loop
sven harness test     samples/harnesses/04-repair-loop
```

`sven harness test` runs every file under `cases/` on mock models and fixture
tools; nothing reaches a real model. The same cases run in the repository's
tests, together with the other harness samples:

```sh
cargo test -p sven-harness-testkit --test samples
```

## Expected

`repaired-on-second-attempt` completes with
`{"status": "repaired", "check": "passed"}`, `patch` having started twice.
The other two cases end `failed` with code `unrepaired`.
