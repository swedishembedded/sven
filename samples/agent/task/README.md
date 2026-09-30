<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# sample-agent-task - a bounded agent task, checked independently

An agent drafts release notes from a change log and publishes them. The task
is small on purpose: the sample shows what an application built on `sven-sdk`
puts around an agent.

| What | How |
|---|---|
| Least privilege | the read-only preset plus three tools of the application's own: `read_changes`, `write_notes` (the only file it can write) and `publish` |
| Bounded runs | every run has a deadline, an output-token budget and the configured tool-round budget; Ctrl-C cancels the run in progress |
| A question that waits | the agent asks who the notes are for; the run ends `Waiting`, the agent is suspended to `agent-state.json`, and a fresh engine resumes it and answers |
| Human approval | `publish` runs a command, so the kernel asks before it runs; the application approves only a draft that passes the check below |
| Independent check | the notes must mention every change in `CHANGES`; the agent's own report is never taken as evidence |
| Trajectory | the whole run is written as ATIF to `trajectory.json` |

## Run it

```sh
make samples/agent/task/build
make samples/agent/task/run ARGS="--demo"
```

`--demo` runs on a scripted model (`demo.yaml`, sven's `mock` provider), so it
needs no API key and always does the same thing: it reads the change log,
writes a draft and asks its question in one turn, finishes after the answer,
and publishes when asked. With a real model the agent decides its calls
itself:

```sh
make samples/agent/task/run ARGS="--config ~/.config/sven/config.yaml --workspace ./release"
```

The workspace must contain `CHANGES`, one change per line starting with `- `.
Other options: `--answer TEXT` (default `users`) and `--deadline SECS`
(default 300).

## What to expect

```text
draft     Waiting (...)
          asked: Who reads these notes, users or developers? ["users", "developers"]
answered  Success (...)
          I drafted RELEASE_NOTES.md for the audience you chose.
publish   Success (...)
          The release notes are published.
check:    the notes cover every change
```

The process exits 1 when the check fails. A draft that misses a change is
never published: the approval is refused and the agent is told why.

## Limits

- The check is a keyword test (each change's first word must appear in the
  notes). It shows where an independent check sits, not how strict one should
  be.
- `publish` copies the notes with `cp`, so it needs a Unix-like system.
