<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# verified-task - the test run decides, not the agent

A workflow: one task, one gate and a success condition,
in forty lines. The compiler lowers it to ordinary harness steps (`sven harness
inspect` lists them), so everything else in sven - checkpoints, replay, traces,
`harness test` - works on it unchanged.

What it shows:

- A **gate** is a command run without a shell whose exit status alone decides
  (`gate_command_exit_code_is_authoritative`). A program that prints "all
  tests passed" and exits 1 has failed; a timeout and a program that cannot be
  started have failed too, with a failure that says which.
- **Repair** is bounded and visible: `repair: { attempts: 1 }` earns one repair
  turn that is shown the failed check's output, then the check runs again. When
  the attempts are spent the run fails with `gate_failed`; it never advances.
- A **success condition** is re-evaluated after the last task from scratch. The
  coverage threshold reads a number that the coverage command wrote during
  this very check, and the agent has no way to declare it met
  (`coverage_gate_cannot_be_bypassed`).

In a run the gates use the real `shell_gate` tool (workspace-confined, killed
with its process group at the deadline).

## Run it

```sh
sven harness validate samples/harnesses/verified-task
sven harness inspect  samples/harnesses/verified-task
sven harness run samples/harnesses/verified-task --input '{"feature": "..."}' --workspace DIR \
    --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
    --approvals auto --trace trace.jsonl
```

`DIR` is the repository the agent works in; the model is a server you already
run. The gates are the repository's own commands, so `DIR` must hold what they
run.
