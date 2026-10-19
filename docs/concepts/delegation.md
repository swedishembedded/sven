# Delegation

The `task` tool starts a sub-agent: a separate `sven acp serve` process held
to the parent's authority (see the contract in
`crates/bootstrap/src/task_tool/mod.rs`). This page covers what that
sub-agent starts from and what it hands back. It does not change who may
delegate or what a child may do: a child never holds more than its parent,
and none of the mechanisms below overrides a permission, an approval or a
conflict rule.

## The `task` tool

`action` is one of `spawn` (the default), `status`, `read`, `grep` and `agents`;
sessions with addressable context also offer `ledger`. The tool asks for
approval by default. `spawn` takes `prompt`
(required), `description` (a short label), `mode` (`research`, `plan`, `agent`
or the name of a discovered persona), `workdir` and `model`. `status`, `read`
and `grep` inspect the buffer of a finished or running child by `handle`;
`agents` lists the personas.

## Briefs

A bare `task` call gives the child its `prompt` and nothing else. A call may
instead name the evidence the child starts from. The fields below, and `paths`
(files or globs, at most 64, used by deduplication and the cost policy), are
optional and additive; without them the child's first request is exactly
`prompt`. All of them, and the `ledger` action, are in the tool's schema only
in sessions with addressable context (`context.assembly.mode` other than
`off`).

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
error before anything starts. `evidence` needs addressable context.

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

## A child on another model endpoint

A sub-agent runs on the parent's model unless the `task` call (or a persona)
names another: `model` takes `provider/name` or a named `providers:` entry,
and is passed to the child as `--model`. The child is a separate process that
reads the same configuration, so an entry's endpoint and key apply to it. No
new protocol is involved: the child speaks ACP to the parent as always, and
reaches its model through the driver the entry names.

A DeepSeek-compatible (or brain) endpoint is such an entry:

```yaml
providers:
  compat:
    name: deepseek                       # the driver that speaks the endpoint's protocol
    base_url: https://api.deepseek.com/v1   # or a brain server: http://127.0.0.1:8788/v1
    api_key: ...                         # or api_key_env
    models:
      deepseek-chat: {}
```

and `task` with `model: compat/deepseek-chat` runs the child there. The default
base URL of the `deepseek` driver is DeepSeek's own, so an entry that only
names the driver and a key reaches it. `tests/e2e/basic/23_delegated_model.bats`
holds the offline specification (the child answers from the entry the `model`
argument names, not from the parent's provider);
`make test/e2e/delegation/local-model ENDPOINT=<url> MODEL=<name> [KEY_FILE=<file>]`
runs a real delegated coding task against a server you already run: a scripted
parent delegates the repair of a failing test to a child on that endpoint, and
the repository's own tests judge the result.

## Deduplication

Before it launches a child, a `task` call registers a subgoal in the session's
registry (`task_tool/subgoals.rs`, one per `TaskTool`, so per session): the
normalized goal (lower case, single spaces, punctuation at word edges
dropped) and deliverable, the mode, the declared scope (`paths`: files or
globs, and the `evidence` ids) and whether the child may write (an `agent`
child does; `research` and `plan` children and read-only personas do not).

| relation | when | what happens |
|----------|------|--------------|
| same | both read-only, same mode, same normalized goal and deliverable, and the new scope contained in the registered one | the call is not launched |
| near | as above, but the wording differs while at least half of the words are shared | the decision model is asked (below); anything but a confident "same" launches |
| distinct | anything else, and any pair with a writer | launches |

Containment is conservative: every new path must equal a registered path or
match a registered glob, every evidence id must be registered, and a subgoal
that declares no scope is contained only in one that declares none. An unknown
scope is never assumed to lie inside a known one. Two writing subgoals are
never merged, whatever they say and whatever the model would answer.

A call whose twin is running waits for it and is told which handle holds the
result. A call whose twin finished is given that result only when every
declared path is a literal file whose size and modification time are what they
were when the twin was launched; with a glob, or no declared files, only a
running twin is joined. A twin that fails or is cancelled is never joined: the
waiting call launches its own child. `task` with `action: "ledger"` counts the
launches avoided.

### The decision model

`decision.features.delegation` lets a decision model break the tie of a near
match (`delegation_decision.rs`, label `delegation.same_goal`). The policy is
declared in that file: a `same`/`different` choice asked in both option orders,
confidence 0.9, margin 0.7, a false positive costing high. It only advises
toward launching less, only inside scopes the deterministic rules already
required to be contained; an abstention, a disagreement between the orders, a
failure or `decision.policy.mode: shadow` launches normally.

## Read-only and writing children

A read-only child (`research`, `plan`, a read-only persona) may share a
retrieval snapshot with its parent: the briefed chunks and the parent's store
as it was when the child started, opened read-only. A snapshot is evidence of
what a file contained when it was read, so it is never presented as current
when the file moved: a brief marks a chunk whose file changed (`is STALE`) or
whose version was never recorded, an expansion in the child says the same in
its header, and a finished result is reused for a repeated subgoal only while
the declared files are unchanged.

A writing child (`agent` mode, a persona that is not read-only) keeps every
rule it had. It runs in the parent's tree under the parent's contract, a
read-only parent cannot start one, its permission requests go to the parent's
approval gate, and teammates of an agent team keep their own task claims. Deduplication does not touch it: a writing subgoal is never
answered from another call, never put to the decision model, and never
served to a read-only twin or the reverse, whatever its scope overlaps.

## Cost policy

`delegation.policy` (`off`, `advise`, `enforce`; default `off`) decides whether
a subtask is worth a sub-agent. With `off` nothing is estimated and every call
behaves as it always has. The estimate (`task_tool/voi.rs`, a pure function)
is in tokens and delegates only when `expected_benefit > coordination_cost`:

```text
coordination_cost = child_overhead + brief + report (+ write_surcharge)
expected_benefit  = inline_work * (1 + parent_context / context_window) * (1 + replay_rounds)
                    + (independent_subgoals - 1) * parallel_credit
```

- `brief` is the tokens of the child's first request.
- `inline_work` is the sum of the sizes (bytes / 4) of the files the call
  declares in `paths` when all are literal and readable; otherwise
  `unknown_work_tokens`, which is chosen large enough to delegate.
- `child_overhead` and `report` are the averages of the session's finished
  children (the input tokens their turns used, and the size of what they
  handed back) once `min_samples` have; the configured values before.
- `parent_context` is the tokens of the chunks the session recorded when
  addressable context is on, `assumed_parent_context_tokens` otherwise. It is
  a proxy: the tool cannot see the conversation.
- `independent_subgoals` counts the subgoals in flight, this one included.

`advise` launches the child and, when the estimate favoured doing the work
inline, ends the result with a `Delegation note`. `enforce` does not launch:
the result (not an error) says what a sub-agent would cost and save, and tells
the model to do the subtask itself, with the prompt it gave. Every number is a
configuration key ([`delegation`](../reference/configuration.md#delegation)). The policy only
ever declines to delegate; it cannot start a child, widen one, or override a
permission, an approval or the concurrency rules, and the deduplication above
is decided first.

## Measurement

`make benchmark/ablation SUITES=delegation SEEDS=3 VARIANTS=A,B` runs the
delegation suite with a scripted model. Its five sub-agents are scripted with a
bare `prompt`, so none of the mechanisms above is exercised by it; it measures
only the cost of offering the extra fields in the `task` schema, which are
offered only in sessions with addressable context. No claim that briefs or
deduplication save tokens, and no model-quality claim, follows from it.
