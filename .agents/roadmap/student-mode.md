# student-mode

**Status: planned, not started, and deliberately not yet committed to git.**
The in-flight `continuous-learning` implementation is committing
sequentially into this same repo's working tree right now (`S1` through
`S7`). Committing this file concurrently would race those commits, so it
exists on disk only until that workflow finishes — see "Sequencing" below.

## Goal

A first-class sven mode, `StudentMachine`, whose job is the opposite stance
from every existing mode: not a confident agent executing a task, but an
adversarial interviewer whose only goal is to extract as much verified
understanding of a task as possible — pressing for specifics, surfacing
contradictions, never accepting the first vague answer — and, once it has
enough, exporting what it learned as a training-ready dataset. This is a
second entry point into the same knowledge-capture pipeline
`continuous-learning.md` is already building (`assimilate_fact`,
`FactSource`, the fact/probe/answer triple shape) — not a parallel system.

## Design — reuse what already exists; the new surface area is small

- **Machine shape**: reuses `loop_core` exactly like every other machine —
  no bespoke phase-transition graph. What makes it `StudentMachine` and not
  a copy of `ReactiveAgentMachine` is three things: a distinct system
  prompt/persona, a distinct `permission_policy()`, and one new gated tool.
  Rejecting `SdlcMachine`'s 7-phase shape here deliberately: an interview
  doesn't have Intake→Planning→Execution's real ordering constraints, so
  copying that machinery would be exactly the kind of premature ceremony
  worth avoiding.
- **Persona**: the system prompt is adversarial by construction — its job
  is to find what it doesn't know, not to reassure the user it understood.
  This is a prompt-engineering property, not new HSM logic, and it will
  need real iteration/eval once built — say so plainly rather than treating
  "adversarial" as a solved design problem.
- **Permission policy**: liberal on `ask_question`/`web_search`/`web_fetch`/
  read tools; **no write or exec tools at all** — a student doesn't need to
  edit code or run shell commands to build a dataset about a task, and
  denying those tools outright is cheaper and safer than gating them.
- **Knowledge capture reuses `S4` verbatim, nothing new to build here.**
  Every confirmed answer in the interview is a `FactSource::UserStated`
  fact, admitted into the same ledger via the same `assimilate_fact` gate
  the continuous-learning loop already requires. Student mode is a second
  *producer* into infrastructure being built anyway, not a second knowledge
  store.
- **The `export_dataset` gate**: usable only once a minimum coverage bar is
  met — N distinct confirmed facts, each phrased with a verifiable probe
  (`{fact, probe_question, expected_answer}`, the exact same triple shape
  brain's `B1`/`B2` already define for document-learning). Same instinct as
  brain's statistical gate, scaled down from "does this training run clear
  a bar" to "does this interview clear a bar" — and it means Student mode
  produces the SAME dataset format the rest of the pipeline already
  consumes. No second format to maintain.
- **Spawnability — "even the basic react agent could start a subagent in
  student mode" is not a new concept, it's a generalization of one that
  already exists.** `Effect::InstantiateSubmachine` already lets a parent
  machine spawn a child kernel; today only `SdlcChildSpawner` uses it, to
  spawn `TaskMachine` children for parallel subtask execution. The real new
  plumbing is making that spawn path available to any parent machine
  (including the default `ReactiveAgentMachine`) and parameterizing it by
  which child mode to spawn, rather than hardcoding `TaskMachine`. Treat
  this as a distinct, second step after the standalone mode works — don't
  build both at once.
- **Model-agnostic by construction, so "even a remote API model" is free.**
  sven already routes every mode through the same 34 provider drivers
  (`sven-model-drivers`) — Student mode needs no brain/whale dependency to
  run at all. This is what makes the end-to-end proof below cheap.

## Simplest possible end-to-end proof

`sven --mode student "task: <description>"` as a **standalone** top-level
session — no brain, no whale, any already-configured model provider. It
interviews the user; once the coverage gate is satisfied, it writes a real
dataset file in the same triple format `B1`/`B2` consume. That alone proves
the whole concept (adversarial interview → gated export → training-ready
output) without touching the cross-repo training/serving loop at all. The
child-spawn generalization is worth building only after this standalone
path is proven — it is a distribution mechanism for something already
working, not a prerequisite for proving the concept.

## Sequencing

Do not start implementation until the in-flight `continuous-learning`
workflow's sven milestones (`S1`→`S7`) have landed — same repo, same
working tree, and that workflow assumes it is the only thing committing
there. Once it finishes, `StudentMachine`'s knowledge-capture half is
already available for free (`S4`'s `assimilate_fact`/`FactSource`); what's
left to build is genuinely just the new machine, its persona/policy, and
the `export_dataset` gate.
