# document-to-capability

**Status: experiment design, not implemented.** The claim under test is the
one that matters and the one nothing here has yet demonstrated: *exposure to
a document the agent did not have makes it able to do something it provably
could not do before, and that ability survives the document going away.*

Companion to [closing-the-loop.md](closing-the-loop.md) (what the field
offers) and [dream-rsi.md](dream-rsi.md) (one paper read closely). brain's
`.agents/roadmap/gauntlet.md` designs the weights-only half of this; this
file is the sven-side half brain deliberately left open.

---

## Why not a real algorithm-design task

The obvious version - "have it discover a faster Lasso solver, then feed it a
paper and see if it does better" - cannot be made conclusive, because you
cannot separate *learned from our document* from *already in the base model,
just needed a nudge*. The RLVR-limit result makes this concrete: a base
model's pass@k at large k is the ceiling, and it is often far above its
pass@1. Any real algorithm has some presence in pretraining.

So the knowledge has to be **authored by us and absent from every corpus**.
That is the one property that makes the experiment interpretable, and it is
worth giving up realism for.

## The task: a toy ISA with an undocumented instruction

A small cycle-accurate VM - call it `tvm` - with ~16 documented opcodes, an
in-process simulator, and a task family:

> Implement `f` for `tvm`. Correctness is checked against a reference
> implementation. Your score is the cycle count.

Task family members: dot product, 1-D convolution, prefix sum, argmax, 4x4
matmul. Each has a known best cycle count using only documented opcodes.

**The secret:** the simulator implements one further instruction - say `VFMA`,
a 4-wide fused multiply-accumulate costing 1 cycle where the documented path
costs 12 - which appears in no `isa.md`, and which **traps unless its operands
satisfy an arbitrary alignment rule**. Two arbitrary constants (the cost, the
alignment) that cannot be derived, only read.

**The document:** `isa-errata.md`, an engineering note describing `VFMA`, its
encoding, the alignment rule, and one worked example. Plus a **decoy** of the
same length and shape describing a different invented instruction that the
simulator does not implement.

Why this shape:

- **It tests use, not recall.** Reciting "VFMA costs 1 cycle" is not the same
  as writing code that uses it. Those can dissociate, and separating them is
  the scientific content of the experiment (see the 2x2 below).
- **The score is continuous.** Cycle count shows partial learning; pass/fail
  does not.
- **Guessability is measurable**, not assumed - see arm A0.
- **It fits the existing shapes.** brain's gauntlet already designs `AlienAPI`
  ("a synthetic tool/object API's argument rules and ordering constraints")
  and requires every environment to be seed-generated with an exact in-process
  oracle. This is that, with the rule *documented somewhere the agent must
  find* rather than *inducible from examples* - which is the realistic case
  and the one that exercises the document pipeline end to end.

## The cheap version: no emulator, no new verifier

The toy ISA above is the *good* experiment. It is not the *first* one, because
building a cycle-accurate simulator is most of the work and none of the claim.

The constraint that decides the cheap version: `VerifierSpec` is
**declarative-only by design** (`crates/vocab/src/verify.rs`) - there is no
`Command` or `UnitTests` shape, so there is no arbitrary-code-execution surface
in the vocabulary at all. "Run `cargo test` and check the exit code" is not
expressible today and should not be added just for an experiment.

What is expressible is `VerifierSpec::FileHash { path, sha256 }`, and it turns
out to be exactly the right tool:

> Write `out.bin` containing the SVF encoding of this payload.

The verifier is the SHA-256 of the correct bytes, which we precompute with a
reference implementation. The agent **cannot forge it and cannot approximate
it** - it either produces the right bytes or it does not. No emulator, no test
runner, no new verifier variant, no execution surface.

**SVF** is an invented byte format. The spec document the agent gets describes
the frame layout. The errata document - the knowledge under test - carries the
parts that can only be read, never derived:

- an arbitrary 4-byte magic header,
- a CRC-8 polynomial and init value,
- one gotcha: the length field covers the payload but excludes the checksum.

That is roughly 16-32 bits of arbitrary content. Nothing in any pretraining
corpus contains it, and no amount of reasoning recovers it.

### Graded, despite the all-or-nothing hash

A hash gives no partial credit, which is what makes it unguessable and also
what would flatten the result. Grading is recovered by **separating the secrets
across task types**, so the score says *which* piece of knowledge landed:

| Task type | Needs | Instances |
|---|---|---|
| `header` | magic bytes only | 6 |
| `framed` | magic + length rule | 6 |
| `checked` | magic + length + CRC | 8 |

Score is instances-hash-matched out of 20, and the staircase is diagnostic:
a model that gets 6/20 learned the header and nothing else.

### What it costs to build

A reference implementation (~60 lines), two markdown documents (spec, errata),
a decoy errata of matching length and shape, and a fixture generator emitting
`{payload, expected_sha256}` pairs plus one task seed per instance. Call it
300 lines and two documents.

**Tier 0**, if even that is too much before the plumbing is trusted: one secret
constant, one task type, five instances. Half a day. It proves the pipeline
moves knowledge - `/learn` -> `knowledge-extract` -> `FactBatch` -> train ->
`promote` -> `--watch-adapters` -> `sven-ci` verified task -> `VERDICT_FACT` ->
reward stamp -> `ingest_dir` - and it proves nothing about capability
acquisition, because one constant is close to pure recall. Its job is to find
the plumbing bugs cheaply, before the real experiment is worth running.

### One property this task does not have

`FileHash` gives the agent **no gradient**: a near-miss and a wild guess score
identically. That is correct for an acceptance test, and it is what makes arm
A0 meaningful. It also means this task is unusable as an RL environment later -
there is no hill to climb. Training data here comes from the document, not from
rollouts, so that costs nothing now; it is the reason the toy ISA (scored by
cycle count) is still the right second experiment.

## The arms

Nothing below is optional. Each one exists because without it a specific
wrong conclusion is available.

| Arm | Setup | Required result | Catches |
|---|---|---|---|
| **A0** | base model, no document, large budget (>=256 attempts) | **must fail** | the task is guessable / derivable |
| **A1** | base model, document **in context** | **must succeed** | the document is insufficient or the task is too hard - if A1 fails nothing downstream is interpretable |
| **A2** | trained adapter, no document, clean context | *the measurement* | - |
| **A3** | trained on the **decoy** document | **must not improve** | "any training helps"; a gate that is a coin |
| **A4** | anchor suite, before vs after | **must not regress** | paying for the new skill with an old one |
| **A5** | fresh-adapter plasticity control | per brain's `run_study` | the model has stopped being able to learn at all |

A0 is also the quantitative claim: 0 successes in 256 attempts bounds the
discovery probability at roughly 1.2% (rule of three), and that number goes in
the write-up rather than the word "unguessable".

## The four-configuration ablation, which sven can now actually run

brain's gauntlet notes that it can only honestly fill two of the four standard
cells, because brain has no external learned state to retain or wipe, and says
the other two belong to whoever wires this into a real agent harness.

**sven now has that state.** `semantic_memory` is real and default-on
(`sven-bootstrap`'s `default = ["gdb", "memory"]`, constructed in
`runtime_builder.rs`), and `assimilate_fact` is gated behind real
`HumanApproved` events for `ToolCapability::AssimilateKnowledge` via a shared
`KnowledgeApprovals` handle - so provenance is not something the model can
forge.

| Config | Weights | Memory store | Reads |
|---|---|---|---|
| `BASE` | old | wiped | floor |
| `WEIGHTS` | new | wiped | **weight contribution** |
| `CONTROL` | old | retained | **retrieval contribution** |
| `FULL` | new | retained | the deployed system |

`WEIGHTS - BASE` is what went into the parameters. `CONTROL - BASE` is what a
lookup would have given you for free. **If `CONTROL - BASE` accounts for the
whole effect, nothing was learned - the agent just wrote itself a note**, and
that is the single most likely way this experiment produces a false positive.
Run all four or claim nothing.

## Recall versus use: the 2x2 that is the actual result

Report probe accuracy (does it know the fact) and task cycle count (can it
deploy the fact) **separately**, never as one number:

| | task improves | task flat |
|---|---|---|
| **probes improve** | capability acquired - the result | a parrot: the fact is in there, the model cannot use it |
| **probes flat** | audit for leakage before believing it | no learning |

The parrot cell is the likely default if the extracted training data is only
question-answer pairs. Teaching *use* probably needs the document-derived data
to include the knowledge **deployed in worked snippets**, not just asked about.
That is a design decision to make up front, and a second arm worth running:
QA-only versus QA-plus-worked-usage.

## Hygiene: the ways this experiment lies to you

Each of these is an assertion in the harness, not a habit:

- **Context bytes.** At eval, assert the secret's name, its constants and the
  document path appear nowhere in the assembled prompt. sven can *prove* this
  from the trajectory rather than assert it, because the kernel records what
  was in context.
- **Filesystem.** The document is absent from the eval workspace, not merely
  unmentioned.
- **No session carryover.** Fresh `AgentState`, no `.sven/` history, memory
  store wiped for the `BASE`/`WEIGHTS` cells.
- **Silent verifier.** The simulator's error messages must never name the
  secret instruction, and must be identical whether or not it was attempted.
- **Silent scaffolding.** Task names, test names, fixture filenames and commit
  messages leak; check them.

## "Continuously" is a retention matrix, not a before/after

One secret and one before/after answers "did it learn once". The user-facing
claim is about learning *continuously as it is exposed to new information*,
which is a curriculum: secret 1 in cycle 1, secret 2 in cycle 2, each with its
own document, its own frozen probe set, its own task family.

The artifact that answers it is the **retention matrix** - does secret 1 still
work after cycle 5 - plus BWT and the plasticity curve. `rl::continual::
Curriculum` and `run_study` already emit all three, with a pre-registered
PASS/FAIL block. That machinery is built; it has never been pointed at a
document-sourced skill.

Apply brain's three holdout tiers per secret: **instance** (unseen input sizes,
the floor), **rule** (a freshly invented secret of the same shape - tests the
*pipeline*, not the learned fact), **generator** (a second, independently
written task generator - catches training against one generator's quirks,
which is exactly what many cycles of a self-improvement loop would do).

## What exists, and what this needs

Already built:

- `promote::document` - `FactProbe`, `FactBatch`, `train_probe_split`,
  `DocumentEnv`, `DocumentVerifier`, `fact_verdicts`, `document_gate_config`:
  the frozen `{fact, probe_question, expected_answer}` contract with a
  disjoint train/probe split.
- `rl::document::Curriculum`, `rl::continual::run_study` - retention matrix,
  BWT, plasticity control, pre-registered gate.
- `crates/promote` - the paired sign test.
- sven `/learn` + the `knowledge-extract` persona - document to structured data.
- `VerifiedTaskMachine` - frozen verifier, hash-checked verdict, no self-grading.
- `brain serve --watch-adapters DIR` - hot-swap into a live server.

Missing, in dependency order:

1. **The `tvm` environment**: simulator, task family, reference implementations,
   cycle-count scorer, the secret instruction, the document, the decoy.
2. **A task-performance environment**, as opposed to a probe-recall one.
   `DocumentEnv` scores answers to probe questions; this experiment's real
   reward is *cycle count on a held-out task*. That is the actual new piece.
3. **The production driver** - the chain `ingest_dir -> fit_weighted -> gate ->
   publish` exists only as the `stage_adapter` test helper.
4. **The hygiene assertions** above.

## Honest limits of what a positive result would prove

- **It lands at L2-L3 on brain's own ladder** - agent gathers data, humans
  initiate; or agent identifies the weakness and trains itself. It is not an
  RSI claim, which that ladder puts at L6. Say L3 and mean it.
- **A rank-8 LoRA on a small model may not have the capacity** to acquire a
  procedural rule as opposed to a fact. brain's gauntlet already warns that its
  gates run on small full-parameter models rather than tiny LoRAs, for
  calibration reasons. If A1 succeeds and A2 fails, capacity is the first
  suspect, not the pipeline.
- **One invented secret is one data point.** The rule-holdout tier is what
  turns it into a claim about the *process*.

Calibrate first and cheaply: A0 and A1 need no training at all, and if either
comes out wrong the task is wrong. Do not build anything else until both pass.
