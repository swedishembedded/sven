<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# scoped-agent - least privilege across an imported workflow

`triage` answers a question from the notes. It does not read them itself: it
imports the workflow `reader` (`lib/reader.yaml`) and runs it as a task
(`use: reader`), and the authority narrows at each step.

- `triage` declares `SpawnChild` and `ReadFile`.
- `reader` declares `ReadFile` only. A child never holds more than its own
  document declares, whatever its parent holds.
- `reader`'s agent lists `tools: [read_notes]`. The list is an enforced
  allowlist: a call to any other tool is refused before it runs, and the
  model is told so.

## Run it

```sh
sven harness validate samples/harnesses/scoped-agent
sven harness run samples/harnesses/scoped-agent --input '{"question": "When is the deploy window?"}' --workspace DIR \
    --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
    --approvals auto --trace trace.jsonl
```

`DIR` is the directory the agent works in; the model is a server you already
run. The trace and the final report say what the run did.
