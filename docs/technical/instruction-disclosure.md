# Instruction disclosure

Skills, subagents and knowledge documents are written for the model, and the
system prompt tells it which exist. That listing grows with the project, not
with sven: the [token ledger](token-ledger.md#the-instruction-catalog) measures
40 skills, 12 subagents and 30 knowledge documents at 5,883 estimated tokens,
paid again by every request of every session whether or not the task touches
any of them.

**Activation** keeps a request from paying for instructions it does not need,
without ever withholding one it does: an instruction may state when it
applies. Until then it is not in the system prompt; once it applies, a short
announcement rides with every request.

Activation grants nothing. Disclosure decides what the model is *told about*; a
skill still loads by its exact command, a subagent is still delegated to
through `task`, and every tool call is gated exactly as before. The text of an
instruction file is untrusted evidence and cannot widen a permission.

## Identities

Every instruction has a stable id, used in listings, in the kernel's active
set and in snapshots:

| kind | id |
|------|----|
| skill | `skill:<command>`, e.g. `skill:sven/plan` |
| subagent | `agent:<name>` |
| knowledge document | `knowledge:<file name without .md>` |

## Activation

A skill, subagent or knowledge file may carry an `activation` block in its
`sven:` frontmatter:

```yaml
---
description: Work on the authentication service.
sven:
  activation:
    paths: ["src/auth/**", "tests/auth/**"]
    modes: [plan]
    states: [Executing]
    always: false
---
```

| key | the instruction applies when |
|-----|------------------------------|
| `paths` | a tool call named a file matching one of these globs, or the task names one |
| `modes` | the session runs in one of these agent modes (`agent`, `plan`, ...) |
| `states` | the machine is in one of these states when a request is made |
| `always` | in every session |

The conditions are alternatives: any one that holds activates the instruction.
Matching is case-insensitive for modes and states. Globs are relative to the
project root: `*` matches within one path segment, `?` one character, and a
`**` segment any number of segments. A path outside the project root never
matches.

A file with no `activation` block applies always, which is how every
instruction behaved before: the system prompt lists it and its text is
byte-identical to what it was. An unknown key under `activation`, or an
`activation` that names no condition, refuses the file: it is not loaded, and
the log names the file and the key.

### What a session sees

- The system prompt lists the instructions that apply always. Instructions
  with an activation block are left out, so the prompt, and with it the
  provider's cached prefix, does not depend on what a session touches. Where
  something was left out, the section says how many more will be announced.
- Each request carries an `## Active Instructions` block in the uncached part of
  the system message, listing every activated instruction by command or name and
  a summary of at most 80 characters.
- A skill whose activation is conditional can be loaded at any time with
  `skill` `action: "load"`, and `action: "list"` shows every skill, activated or
  not.

### Where activation is recorded

`sven_hsm::Context::active_instructions`, kernel state like the loaded tools:

- The executors decide. The tool executor posts `Event::InstructionsActivated`
  after a successful call whose `path`, `paths`, `file`, `files`, `file_path`,
  `old_path`, `new_path` or `workdir` argument matches a `paths` glob. The turn
  executor posts it when the latest user message names a matching file, when
  the session's mode matches `modes`, or when the state stamped on the request
  (`TurnRequest::state`) matches `states`.
- `Hsm::dispatch` applies the event to the context ahead of the machine and
  copies the set onto every model request (`TurnRequest::active_instructions`),
  so no machine, executor or tool keeps it.
- The set only grows, and an id keeps its place. A snapshot holds it, a replay
  of the recorded events rebuilds it, and a snapshot that predates the field
  resumes with none active. The announcement is rendered from the set on every
  request, so rebuilding the context never loses an activated instruction.

### Pinned instructions

These are never deferred, hidden or dropped by activation or by any
disclosure mode:

- The project instructions file (`AGENTS.md`, `.sven/context.md`): the
  `## Project Instructions` section names its path whatever else is
  configured, and is the same few tokens however long the file is.
- A skill with `sven.always: true`, even if it also has an `activation` block.
- A skill the user requests explicitly (a slash command), whose text is placed
  in the conversation and is not subject to any listing.
