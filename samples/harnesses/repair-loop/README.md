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

## Run it

```sh
sven harness validate samples/harnesses/repair-loop
sven harness run samples/harnesses/repair-loop --workspace DIR \
    --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
    --approvals auto --trace trace.jsonl
```

`DIR` is the directory the agent works in; the model is a server you already
run. The trace and the final report say what the run did.

The harness takes no inputs.
