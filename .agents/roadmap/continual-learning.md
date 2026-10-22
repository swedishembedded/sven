# experience-memory

Scope: Recording which memory units and skills were in play for a step and its outcome, so an embedding application can learn from them.

**Status: planned. Nothing below has been started. Extends `memory.md`; do the items there
first where an item here says "after".**

## Goal

Let an embedding application learn from what an agent did, without changing any model
weights: outcomes are recorded against the memory units and skills that were in play, so
that the application can tell which units help, which are stale, and which are harmful.
Sven owns the mechanism (store, tool, skills, recording). The embedding application owns
the policy (what qualifies, when a unit is activated or retired, what is trained).

Today sven stores memories (SQLite, FTS5 BM25, optional vectors), renders untrusted records
as quoted material on recall, loads skills from `SKILL.md` trees, and writes ATIF
trajectories. It does not record which memory unit or skill was retrieved for a step, what the
outcome was, or whether the unit was withheld, so nobody can attribute success to a unit.

## Left to do

- [ ] CL-S1 (M) Unit evidence and lifecycle in the memory API (after the scoped
  `MemoryStore` and the annotation log of `memory.md`).
  - Each unit carries append-only evidence revisions: outcome (pass, fail, abstain), the
    strength of the verdict that decided it (judged, consistency, formal, executable), the
    originating trajectory id, an environment fingerprint (an opaque host-supplied digest),
    a state (candidate, provisional, active, stale, retired), and counters: used, used and
    succeeded, withheld, withheld and succeeded.
  - State changes are made by the host through the store seam with an expected revision;
    sven never promotes or retires on its own. A superseded unit stays in the log.
  - Records whose resolved provenance is untrusted can be stored but a state change to
    active is refused with a typed reason.
  - Acceptance: store conformance suite cases for revision conflicts, refusal of an active
    state for an untrusted unit, append-only history, and supersession keeping both units.
    `make test check`.
- [ ] CL-S2 (S) Record retrieval and withholding in the trajectory (extends the retrieval
  record already planned in `memory.md`).
  - A retrieval step carries unit ids, revisions, scores, whether each result was put in the
    context, and for each eligible unit whether it was withheld by a host-supplied sampling
    decision. The withholding decision itself is made by the host through a hook so
    experiments stay reproducible from a seed the host records.
  - Acceptance: a trajectory round trip test (write, validate, read) and a test that a
    withheld unit is absent from the context but present in the record. `make test`.
- [ ] CL-S3 (M) Skills with origin and a lifecycle (after CL-S1).
  - `skill propose` writes a candidate `SKILL.md` package with its origin (trajectory id,
    verdict, strength) in the frontmatter or a sidecar, in a candidate area that discovery
    does not load by default. The host moves a candidate to the active tree or retires it;
    discovery precedence is unchanged.
  - Acceptance: discovery tests that a candidate is not loaded, an active skill is, a retired
    skill is not, and that the origin survives a rewrite. `make test check`.
- [ ] CL-S4 (S) Opaque session token on the model-provider request.
  - A host can pass an opaque token with a request so that a model provider that keeps
    per-session state (a recurrent-state cache, for instance) can find it again. Sven
    treats the token as opaque and does not log it outside the trajectory's extra record.
  - Acceptance: provider-mock test that the token reaches the request and is absent from
    prompts. `make test`.

## Invariants

- Sven records, hosts decide. No unit changes state without a host call carrying the
  expected revision.
- The agent's own words never count as evidence for a state change; only a recorded verdict
  does, and the strength of that verdict is stored beside it.
- Untrusted retrieved content stays quoted on recall (existing behaviour) and can never
  become an active unit.
- Nothing here names a model provider's internals; the provider is described only by the
  request fields it receives.
