<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# Instruction disclosure

Skills, subagents and knowledge documents are written for the model, and the
system prompt tells it which exist. That listing grows with the project, not
with sven, and it is paid again by every request of every session whether or not
the task touches any of them (see
[the instruction catalog](token-ledger.md#the-instruction-catalog) in the token
ledger).

Two mechanisms keep a request from paying for instructions it does not need,
without ever withholding one it does:

- **Activation**: an instruction may state when it applies. Until then it is
  not in the system prompt; once it applies, a short announcement rides with
  every request.
- **Progressive disclosure** ([below](#progressive-disclosure-of-skills)): the
  system prompt lists a compact index of the skills, the subagents and the
  knowledge documents, and the tool that serves each catalog returns the rest.

Neither grants anything. Disclosure decides what the model is *told about*; a
skill still loads by its exact command, a subagent is still delegated to
through `task`, and every tool call is gated exactly as before. The text of an
instruction file is untrusted evidence and cannot widen a permission.

## Identities

Every instruction has a stable id, used in the kernel's active set and in
snapshots:

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
| `always` | in every session: the instruction is listed from the start, like one with no block |

The conditions are alternatives: any one that holds activates the instruction.
Matching is case-insensitive for modes and states. Globs are relative to the
project root: `*` matches within one path segment, `?` one character, and a
`**` segment any number of segments. A path outside the project root never
matches.

A file with no `activation` block applies always: the system prompt lists it.
An unknown key under `activation`, or an `activation` that names no condition,
refuses the file: it is not loaded, and the log names the file and the reason.

### What a session sees

- The system prompt lists the instructions that apply always. Instructions with
  a conditional activation block are left out, so the prompt, and with it the
  provider's cached prefix, does not depend on what a session touches. Where
  something was left out, the section says how many more will be announced.
- Each request carries an `## Active Instructions` block in the dynamic suffix
  of the system message (the part after the cached prefix), listing every
  activated instruction by command or name and a summary of at most 80
  characters.
- A skill whose activation is conditional can be loaded at any time with
  `skill` `action: "load"`, and `action: "list"` shows every skill, activated or
  not.

### Where activation is recorded

`sven_hsm::Context::active_instructions` is kernel state, like the loaded tools:

- The executors decide. The tool executor posts `Event::InstructionsActivated`
  after a successful call whose `path`, `paths`, `file`, `files`, `file_path`,
  `old_path`, `new_path` or `workdir` argument matches a `paths` glob. The turn
  executor posts it when the latest user message names a matching file (a word
  with a `/` or a file extension), when the session's mode matches `modes`, or
  when the state stamped on the request (`TurnRequest::state`) matches `states`.
- `Hsm::dispatch` applies the event to the context ahead of the machine and
  copies the set onto every model request (`TurnRequest::active_instructions`),
  so no machine, executor or tool keeps it.
- The set only grows, and an id keeps its place. A snapshot holds it and a replay
  of the recorded events rebuilds it. The announcement is rendered from the set
  on every request, so rebuilding the context never loses an activated
  instruction.

### Pinned instructions

These are never deferred, hidden or dropped by activation or by any
disclosure mode:

- The project instructions file (`AGENTS.md`, `.sven/context.md`): the
  `## Project Instructions` section names its path whatever else is
  configured, and is the same few tokens however long the file is.
- A skill with `sven.always: true`, even if it also has an `activation` block.
- A skill the user requests explicitly (a slash command), which is not subject
  to any listing.

## Progressive disclosure of skills

`skills.disclosure` ([configuration](../reference/configuration.md#skills-subagents-and-knowledge))
or `--skill-disclosure` for one process (the subagent and knowledge listings
have [their own](#subagents-and-knowledge-documents)). The default is `deferred`
for a program that reads the configuration file and does not set the mode, and
`full` for a `Config` built in code, as for
[tool disclosure](tool-disclosure.md):

| mode | the system prompt lists |
|------|-------------------------|
| `full` | every skill's command, name and whole description in an `<available_skills>` block |
| `deferred` | one line per skill, `- <command>: <summary>`, the summary the first line of the description cut to 80 characters, with an opening such as "This skill should be used when" or "Use when" dropped |

Above 50 skills the index gives a count per package instead of a line per skill
(`- auth: 4`), so a project with hundreds of skills does not pay for hundreds
of lines on every request; `skill` `list` (with a `regex`) and `skill` `search`
are the live views either way.

### One place for details: the `skill` tool

There is no second discovery tool. The `skill` tool's actions:

| action | arguments | result |
|--------|-----------|--------|
| `load` | `name` | the whole `SKILL.md` body, its base directory, bundled files and sub-skills |
| `list` | optional `regex` | every skill the model may discover, one line each |
| `search` | `query` | up to 8 skills, best first, each with its command and a summary of at most 160 characters |

`search` ranks with the same `ToolRanker` as the `tools` tool: BM25 over the
stemmed terms of the command (weight 2.0) and the skill's whole description
(weight 1.0), which is where its trigger phrases live. It is a pure function of
the query and the skills. With `decision.features.skill_selection` a decision
model breaks BM25's ties exactly as it does for tools (see
[decisions](decisions.md#skill-search-re-ranking)); in every other case the
result is BM25's.

### What is never deferred or hidden

- A skill with `sven.always: true` keeps its full `<skill>` entry in the index.
- A skill that is active (see [Activation](#activation)) is announced with every
  request in the `## Active Instructions` block, whatever the mode.
- Every skill loads by its exact command in either mode, activated or not,
  listed or not: `load` never consults the index or the active set. A skill
  marked `sven.user_invocable_only` is left out of the model's index, `list` and
  `search`, as it is from the full listing, and loads when its exact command is
  named.
- The project instructions file and explicitly requested skills, as under
  [Pinned instructions](#pinned-instructions).

## Subagents and knowledge documents

The same switch, per listing: `subagents.disclosure` and `knowledge.disclosure`
(`--subagent-disclosure`, `--knowledge-disclosure`), with the same defaults.

- **Subagents.** `deferred` lists `- <name> (readonly): <summary>`, the summary
  at most 80 characters, and says to delegate with `task` and the name as
  `mode`. `task` with `action: "agents"` returns every subagent with its whole
  description, its model and whether it is read-only. The `mode` enum of the
  `task` tool already names every subagent, so delegating never depends on the
  index.
- **Knowledge documents.** `deferred` lists `- <subsystem> (<files it covers>)`
  and drops the `updated` date, which only the drift check uses. The
  `search_knowledge` and `list_knowledge` actions of the `memory` tool return
  the documents themselves.

Both give a count instead of lines above 50 entries. A subagent or document
with a conditional `sven.activation` is left out of both modes and announced
when it applies.

## Choosing a mode

`deferred` is the default of a program that reads the configuration file. What a
required instruction rests on does not depend on the index: the project
instructions section, a pinned skill's full entry, the announcement of an active
instruction, loading a skill by its exact command and the `mode` enum of the
`task` tool are all independent of it.

What a deferred index cannot guarantee is that a model that sees an 80-character
summary picks the right skill as reliably as one that sees the whole
description. A project whose descriptions put the decisive trigger phrase late
sets `skills.disclosure: full`, or writes the decisive words first: the index
shows the first line of the description.

## Measured

`make benchmark/ledger INSTRUCTIONS=full|deferred` replays the ledger scenarios
against a catalog fixture of skills, subagents and knowledge documents (see the
[token ledger](token-ledger.md#the-instruction-catalog)) and records the
estimated tokens of each system section (`Skills`, `Subagents`,
`Knowledge Base`, `Project Instructions`) per request. The basic end-to-end suite
runs with every listing deferred under
`make test/e2e/basic/instructions-deferred`.
