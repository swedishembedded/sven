<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# An interactive session sent other requests than a headless run of the same task

Measured 2026-11-21 with the mock model, the same task in `sven --headless` and in the TUI (driven through a
pseudo-terminal), both with `--record-requests`:

- The system prompt and the tool schemas were the same except for the project's git state and `ask_question`, which
  the TUI offers (a person is there to answer it).
- The TUI dropped `--append-system-prompt` and `--run-timeout` without a word and refused the sampling flags, so its
  user turns had no `Time: U of T min used` line and its requests no sampling controls.
- The TUI's first request was a title request (`openrouter/free` first, then the configured model): an extra entry in
  the request log and the gateway's usage that no headless run has.
- A positional PROMPT always made the run headless: there was no way to start the TUI on a task.

`sven --interactive PROMPT` (06b2ecc5) closes the first three for a session started on a task; the remaining
difference is the trace's fidelity (roadmap `generic-learning.md`, GL-S4). Check the request logs of both modes
side by side before treating a person-guided session as the same observation as a headless one.
