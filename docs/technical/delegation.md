# Delegation

The `task` tool starts a sub-agent: a separate `sven acp serve` process held
to the parent's authority (see the contract in
`crates/bootstrap/src/task_tool/mod.rs`). This page covers what that
sub-agent starts from and what it hands back. It does not change who may
delegate or what a child may do: a child never holds more than its parent,
and none of the mechanisms below overrides a permission, an approval or a
conflict rule.

## Briefs

A bare `task` call gives the child its `prompt` and nothing else. A call may
instead name the evidence the child starts from. All three fields are optional
and additive; without them the child's first request is exactly `prompt`.

| argument | meaning |
|----------|---------|
| `evidence` | chunk ids (`c-` and 16 hex digits, at most 32) from the parent's chunk store, usually the references a shortened result carries |
| `evidence_visibility` | `hidden`, `short` (default), `long` or `full`: how much of each chunk the child sees first |
| `deliverable` | the shape of the answer wanted |

The child's first request is then, in this order:

1. `## Subgoal`: the `prompt`.
2. `## Constraints`: every chunk of the parent's store that is pinned and is a
   user constraint or instruction, in full. They bind the child as the task
   does. A pinned chunk of any other kind is quoted as evidence instead.
3. `## Evidence`: each named chunk at the chosen visibility, in a fence
   labelled `untrusted`. A shortened form states the call that expands it. The
   quoted text is capped at 8,000 estimated tokens; a chunk past the cap is
   reduced to its one-line reference. A chunk labelled `secret` is named,
   never quoted. A form is raised to its floor as everywhere else (a failure
   is at least `long`, a pinned chunk is whole).
4. `## Deliverable`.

The parent's conversation is not part of it.

An id that is not a chunk id, or that the parent's store never recorded, is an
error before anything starts. `evidence` needs addressable context
(`context.assembly.mode` other than `off`); `deliverable` does not.

### Sharing the store

When chunks are named, the parent starts the child with `--evidence-store
<root>/.sven/context/<session id>`. The child opens that store read-only, as
it was when the child started, and its `context` tool expands the briefed ids
from it, with the same staleness report as the parent's own expansions. The
flag accepts only that shape (absolute, no `..`, a UUID as the last
component); the model only ever supplies chunk ids, never a path. The child
cannot record into, pin in or otherwise change the parent's store.

## Results

A child that was briefed is told to end its answer with one fenced `finding`
block, a JSON object:

| field | meaning |
|-------|---------|
| `summary` | what it found, in a few sentences (required, at most 2,000 characters) |
| `evidence` | ids of the briefed chunks the answer rests on (at most 20; an id the parent's store never recorded is dropped) |
| `files_touched` | files it changed, empty if it only read (at most 20 entries) |
| `open_questions` | what it could not settle (at most 20 entries) |

The parent is shown the summary, the evidence ids, the files touched and the
open questions, not the child's transcript or the prose before the block. The
whole answer is recorded in the parent's chunk store as a `subagent_finding`
chunk whose source is `task:<handle>`, and the result ends with the call that
expands it. A briefed child that produces no valid block is reported exactly as
an unbriefed one is: its answer as it is. The block is untrusted text: ids are
validated, every field is bounded and nothing in it is interpreted.

### What a child cost

Every finished child, briefed or not, is entered in the session's account:
the tokens its turn used as the provider reported them, the estimated tokens
(bytes / 4) of what the parent was handed, and of the child's whole transcript
(what it said, the calls it made and what they returned), which is what
forwarding the transcript would have put in the parent's context. A briefed
child's result ends with a `Cost:` line; `task` with `action: "ledger"` lists
every child and the total. The child's own requests are still charged to the
session through the existing token accounting; this account adds the per-child
view and the saving, which is an estimate of context size, not of money.
