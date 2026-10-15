<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# sample-host-sessions - one engine, several supervised sessions

A deploy desk serves two agent sessions at a time. Each session asks to
deploy one service; the deploy tool is the application's own and needs a
person's approval. The sample shows what a host that serves many sessions
builds on `sven-sdk`, in two acts.

| What | How |
|---|---|
| One engine, several sessions | `Engine::agent` makes an agent per session; they share the engine's model client |
| A gate handler per session | `Agent::with_human_gates` answers a session's gates; an agent without one gets the engine's handler, which here refuses |
| Gates that name their run | `HumanGate::Approval` carries the `RunId` and `ApprovalId`; the events of the same run carry the same `RunId` |
| Parking | `EngineBuilder::park_approvals`: the run ends `Waiting` with the approval in `outcome.approval` and returns at once |
| Restart | each session is suspended to `<session>.json`; a fresh engine resumes it |
| Deciding later | `AgentState::pending_approval` finds what waits in the stored state alone; `Agent::decide` approves or denies it, the denial with a reason the model reads |
| A numbered stream | every event has a `seq` that carries on across the restart; a subscriber that falls behind is told with `EventsDropped` |

## Run it

```sh
make samples/host/sessions/build
make samples/host/sessions/run ARGS="--demo"
```

`--demo` runs on a scripted model (`demo.yaml`, sven's `mock` provider), so it
needs no API key and always does the same thing. With a real model the agent
decides its calls itself:

```sh
make samples/host/sessions/run ARGS="--config ~/.config/sven/config.yaml --workspace ./desk"
```

A real model must be willing to call `deploy` for the four services the
sessions ask about (`ledger`, `inventory`, `search`, `billing`).

## What to expect

```text
live: one engine, a gate handler per session
  ledger     gate for ledger     run <uuid>  approved
  inventory  gate for inventory  run <uuid>  refused
parked: suspended, restarted, decided from the stored state
  search     search     approved; the session ended Success
  billing    billing    denied (billing is frozen until the audit ends); the session ended Success
event numbering carried on across the restart: true
deployed: ledger, search
check:    the desk did what its policy says
```

The process exits 1 when the check fails: a gate that reached another
session's handler, a service deployed without approval, or numbering that
started over after the restart.

## Limits

- The two acts use two engines because parking is an engine setting. A host
  that wants both asks live for some tools and parks for others by running two
  engines, or decides in its handler to reply later.
- The deploy tool only leaves a marker file. A service name that is a path is
  refused.
- A call made by a sub-agent (`task` tool) is never parked: the sub-agent does
  not outlive the run, so its approvals go to the handler.
