<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# repair-loop - do not take the tool's word for it

One task asks the agent to apply a patch with `write_patch`. Whatever the agent
says, an independent file check (`verify: [{artifact: fixed.txt}]`) decides
whether the patch happened; a failed check earns one repair turn
(`repair: {attempts: 1}`), and a second failure fails the run.

What it shows:

- A check is evidence the model and the tool cannot give: a tool that answers
  "patched" and writes nothing is not believed.
- Repair is bounded and explicit in the plan: `patch__work`, the check
  `patch__v0_fixed_txt_a0`, `patch__v0_fixed_txt_a0_repair`, the check again
  and a `_fail` step.
- The failure is the run's own: `gate_failed`, with a message naming the task
  and the file.

Cases: `repaired-on-second-attempt` (the first patch fails, the repair turn's
call writes the file inside the case workspace); `never-repaired` (both patches
fail); `claims-success-without-writing` (the tool answers "patched" but writes
nothing - the agent is satisfied, the check is not).

## Run it

```sh
sven harness validate samples/harnesses/repair-loop
sven harness inspect  samples/harnesses/repair-loop
sven harness test     samples/harnesses/repair-loop
```

`sven harness test` runs every file under `cases/` on mock models and fixture
tools; nothing reaches a real model. The same cases run in the repository's
tests, together with the other harness samples:

```sh
cargo test -p sven-harness-testkit --test samples
```

## Expected

`repaired-on-second-attempt` completes with `{"result": "patched"}`, the repair
turn having run once. The other two cases end `failed` with code `gate_failed`.
