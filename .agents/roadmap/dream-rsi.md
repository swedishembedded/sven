# dream-rsi

**Status: research done, nothing accepted for implementation. This document is
the reading of the paper, the surrounding field, and an inventory of what sven,
brain and whale already have that bears on it. No milestone here is committed
work.**

Primary source: Tong Zheng, Xidong Wu, Zheng Zhang et al., *Dream-RSI:
Recursive Self-Improvement through Evolving Worlds*, arXiv:2609.14858,
submitted 2026-09-14 (Google, Google DeepMind, University of Maryland,
University of Virginia). Project page `dream-rsi.com`; repo
`github.com/zhengkid/Dream-RSI` is **documentation only** - as of this reading
the code, the discovered programs and the reproduction scripts are all still
listed as "being prepared". Everything below about the mechanism comes from the
paper text and its two published prompts, not from running anything.

---

## 1. What it actually is

One sentence: **freeze the agent and the evaluator, make the search strategy an
ordinary program, and improve that program offline by re-running it against the
recorded results of past searches.**

The weights never change. The coding agent never changes. The evaluator never
changes. The only artifact that changes across rounds is a Python file
implementing `OptimalPolicy.solve(question, budget)` - which decides, at each
round, *which previously-reached point to continue from, how many to continue
in parallel, and when to stop*. That file is rewritten by an LLM
"policy-development agent" that is shown replay trajectories and scores.

The claim that makes it interesting: a completed search already contains every
outcome, so an alternative strategy can be scored by *reading* those outcomes
instead of re-running the agent. Off-policy evaluation at zero execution cost.

## 2. The mechanism, precisely

**Node.** A discovery tree is rooted at `r` (the initial workspace). Each
non-root node has exactly one *primary parent*. A node records: the parent's
workspace resumed and extended, the generated artifact, the evaluator's
diagnostics, and a scalar score `s_v` (larger is better, fixed scoring protocol
per task).

**Decision interface** (identical online and in replay). The policy observes a
tree `T` and picks a batch `C subset of A(T)` with `|C| <= W`, where
`A(T) = {r} union {leaves of T}` and `W` is the worker count. Selecting `r` opens a
new branch; selecting a leaf extends that branch. Batch size is the
parallelism decision; the empty batch is the stop decision.

**Online rollout** (round `t`, at most `K1` decision rounds). Policy `pi_t`
picks batches; each selected node is handed to the discovery agent, which
resumes that node's saved workspace and produces one child; the evaluator
scores it. Stochastic - the same parent can yield different children. The final
tree `T_t` is appended to the history `H_t = H_{t-1} union {T_t}`.

**Offline replay.** History frozen. `M` policy versions `pi_t^0 ... pi_t^{M-1}` are
produced in sequence, `pi_t^0 = pi_t`. Each is replayed against *every* recorded
tree `T_i`, `i = 1..t`. Replay starts from `{r}` and reveals only what the
policy selects. Revealing is **deterministic**: for a non-root `v`, it returns
`v`'s unique recorded child if one exists; for `r`, it returns the
earliest-created recorded child not yet revealed. Nothing outside `T_i` can be
generated. Terminates on empty batch, `K2` rounds, or full reveal.

**Replay objective** (equation 1):

```
V_i^m = max_{v revealed} s_v        (discovery quality)
      - beta1 * N_i^m               (execution cost: non-root nodes revealed)
      + beta2 * N_i^m / max(1, k*)  (parallelism bonus: nodes per decision round)
```

**Selection.** `V^m = (1/t) sum_i V_i^m`; `pi_{t+1} = argmax_m V^m`. Because
`pi_t^0 = pi_t` is in the candidate set, the selected policy is never worse than
the incumbent *on the replay set*. That last qualifier is doing a lot of work -
see section 5.

## 3. The finding that is not in the abstract

**The recorded "tree" is a forest of linear chains, not a tree.** Replay
returns *"`v`'s unique recorded child"* - each node has at most one recorded
continuation. The published policy-improvement prompt says it outright: *"The
environment is a frozen, irregular branch-by-attempt grid. A policy opens a root
or refines the next cell of an already-open branch."*

So the counterfactual freedom being explored is exactly four things:

1. which recorded branches to open,
2. how far down each recorded chain to walk,
3. how to group probes into parallel batches,
4. when to stop.

It is **not** "what if the agent had branched here instead". No such node was
ever recorded, so no such policy can be scored. This is the classic off-policy
coverage limit, and the paper's exactness claim is bought precisely by
refusing to leave the logged support. That is intellectually honest but it means
"world model" and "dreaming" are decoration: this is tabular replay over a
frozen grid, closer to a bandit-replay estimator than to Dreamer.

**Why this matters to us: it is far cheaper to build than the framing suggests,
and we can do strictly better than the paper** - see section 8.

## 4. Results, read honestly

Setup: Gemini-3.1-Pro and Gemini-3.7-Flash driven through the Gemini CLI. The
controlled baseline is *Recursive Fixed Exploration* - same agent, same
evaluator, same initial hand-written policy (parallel independent workspaces,
each refining its own candidate), held fixed across rounds. Both start
identical in round 1. Cost is counted in cumulative discovery-agent calls.

**Lasso regularization path** (6 held-out datasets, wall-clock ms, lower better):

| | compute | Gisette | RCV1 | DNA | Leukemia | Colon | Duke | Avg |
|---|---|---|---|---|---|---|---|---|
| Fixed, Pro | 550 | 1861.8 | 19550.1 | 41.5 | 26.1 | 14.5 | 28.4 | 3587.1 |
| Dream-RSI, Pro | 317 | 2841.0 | 14616.0 | 49.9 | 30.2 | 16.4 | 32.5 | 2931.0 |
| Fixed, Flash | 3200 | 1133.1 | 13873.0 | 29.8 | 24.1 | 15.7 | 24.4 | 2516.7 |
| Dream-RSI, Flash | 1879 | 1091.9 | 12923.4 | 31.4 | 21.0 | 12.2 | 23.6 | 2350.6 |

Two things the paper does not say:

- **The "Avg" column is an unweighted mean over datasets whose runtimes span
  four orders of magnitude**, so RCV1 (~10^4 ms) sets it and the biological
  datasets (~10^1 ms) contribute nothing.
- **With Gemini-3.1-Pro, Dream-RSI is worse on five of the six held-out
  datasets** and better only on RCV1. The favourable average is that one
  dataset. With Flash it is a clean result: better on five of six, at 1.7x
  less compute. So the method has one solid demonstration and one that
  survives only through the aggregation.

**The 162x is against the wrong baseline for our purposes.** It compares
Dream-RSI/Pro (317 calls) to SimpleTES (51,200 calls) - a different agent, a
different model (gpt-oss-120b), a different harness. Against its own controlled
baseline the number is **1.7x**. Take 1.7x as the honest effect size.

**Kernel engineering** (KernelBench: VGG16, LayerNorm, ConvDiv, ConvMax,
Gemini-3.1-Pro, score = 1/ms subject to correctness): comparable performance at
2.43x / 1.79x fewer generations on VGG16 / LayerNorm; 2.09x / 1.44x higher
performance at comparable budget on ConvDiv / ConvMax. This is the domain
closest to our own work and the most consistent result in the paper.

**Math optimization** (sum-difference, circle packing n in {26,32},
autocorrelation): matches or surpasses baselines within 1k generations.

**Behaviour analysis (section 5.2, ConvDiv, 9 rounds).** Round-best performance climbs
0.427 -> 1.898 while evaluated attempts go 110 -> 87 -> 80 -> 50 -> 92 -> 80 -> 91 ->
86. The learned policy *spends less while it is winning and spends more when it
plateaus.* That adaptive-budget behaviour is the most transferable idea in the
paper and does not depend on any of its machinery.

**Negative result worth keeping (section 5.1).** They also tried distilling history
into high-level directional insights injected into the prompt. **Prompt-level
semantic guidance consistently underperformed no guidance at all**, for both
Dream-RSI and the fixed baseline. Their reading: strong semantic priors about
where to search next over-constrain parallel long-horizon exploration. Evidence
is one figure on one task, so it is a caution and not a law - but it points
directly at how we use distilled "lessons" and memory (section 7).

## 5. The criticisms, and which are right

From the HN thread (id 49726955) and secondary analyses:

- **"Not RSI."** Correct. Nothing recursive improves anything about the
  intelligence doing the work; a search-allocation program is tuned around a
  frozen agent. *"If this is RSI then all RL is RSI."* The honest description is
  meta-level policy search with replay-based off-policy evaluation.
- **"Overfitting to already-discovered branches."** Correct and structural.
  Selection is plain `argmax_m V^m` over `M` versions on `t` replay worlds -
  uncontrolled adaptive multiple testing with no correction whatsoever. The
  guarantee `V^{m*} >= V^0` holds *on the replay set the versions were written
  against*. This is the single weakest joint in the paper, and it is the one
  we are best placed to fix (section 8).
- **"Doesn't transfer across tasks."** Unresolved. Every experiment keeps the
  policy within one task family.
- **Reproducibility.** Gemini-only, code unreleased. The prompts are published,
  which is the part that actually transfers.

## 6. Where it sits in the field

**Ancestors - evolutionary program discovery.** FunSearch -> AlphaEvolve
(arXiv:2506.13131) established LLM-as-mutation-operator over a population of
programs with a hard evaluator. Open reimplementations: OpenEvolve,
ShinkaEvolve (Sakana, ICLR 2026, sample-efficiency focused), CodeEvolve
(arXiv:2510.14150, reports matching AlphaEvolve on 5/9 of its suite and beating
OpenEvolve/ShinkaEvolve on 6/9 under matched settings). SimpleTES
(arXiv:2604.19341) is the direct baseline here: parallel independent
trajectories, iterative refinement, local selection, selective history reuse -
minimal by design, and expensive.

**Siblings - meta-level search optimisation.** EvoX optimises search strategies
rather than solutions; SwarmResearch orchestrates branches dynamically;
SkyDiscover provides adaptive discovery infrastructure. Dream-RSI's contribution
against these is specifically the *off-policy* evaluation of the meta-policy.

**The other family - agents that rewrite themselves.** Darwin Godel Machine
(arXiv:2505.22954, ICLR 2026) rewrites its own agent code, keeps an archive of
ancestors as stepping stones, and moves SWE-bench 20.0 -> 50.0% and Polyglot
14.2 -> 30.7%, with transfer across models and languages. ADAS has a meta-agent
invent agent designs in code. Godel Agent rewrites its own logic. Voyager grows
a skill library; Agent Workflow Memory induces reusable routines. Dream-RSI is
deliberately *narrower* than all of these - it changes one file with one
function - and that narrowness is why it can be gated and audited.

**The acceptance-gate line, which is the part the field is converging on.**
PACE (arXiv:2606.08106) makes exactly the argument in section 5: greedy "keep it if
the score went up" is uncontrolled adaptive multiple testing, *"the agent
p-hacks itself"*. On Qwen2.5 agents across three datasets greedy acceptance
committed **30-42% false edits**; a testing-by-betting e-process committed
essentially only the real one, matched held-out accuracy at lower variance and
~18% lower evaluation cost. **This is the literature agreeing with brain's
`crates/promote` design** - and it is the missing half of Dream-RSI.

**The named inspiration.** Dreamer / World Models (Ha & Schmidhuber; Hafner et
al.) learn an approximate latent dynamics model and train the policy inside it.
Dream-RSI's analogy is rhetorical: nothing is learned, nothing is approximated,
nothing generalises beyond the logged support. Useful to keep straight, because
the *actual* Dreamer idea - a learned model that extrapolates past the log - is
a separate and much larger thing that brain (`wm-core`, `diamond`,
`genieredux`) is independently equipped for.

## 7. What we already have

This is the part that decides whether any of it is worth doing. The inventory
is unusually good, because the pieces were built for other reasons.

**sven - the node primitive already exists.**
`VerifiedTaskMachine` (`crates/machines/src/machines/verified_task.rs`) is
precisely Dream-RSI's node contract and arguably stricter: it freezes a
`VerifierSpec` *before* the attempt, treats a model's final turn as "I believe I
am finished" and nothing more, and takes the verdict only from
`Event::VerificationComplete` produced by an executor that ran the verifier
against the real world, re-checking the spec hash before trusting it. Score,
correctness gate, and no self-grading - the three things a replay simulator's
node needs.

**sven - the recording already exists, and is typed.**
`crates/hsm/src/audit.rs` appends one `AuditRecord` per dispatch and `replay`
deterministically reconstructs state from input events, because dispatch is pure
and effects are returned rather than executed. `crates/atif` is a spec-complete
Agent Trajectory Interchange Format v1.7 implementation with NDJSON step
streaming. Google's harness scrapes `attempt_*/proposal.md` and
`eval/score.json` out of a filesystem; we have serde-typed events with a
validator.

**sven - the branching primitive already exists.**
`crates/hsm/src/snapshot.rs` + `Hsm::restore`: O(1) suspend/resume of a machine
from `{state label, Context}`, with `all_states()` making the label->state
mapping total and `RestoreError::UnknownState` refusing to silently restart.
`ErasedRuntime::capture` reads a live session's state without stopping it.

**sven - the parallelism primitive already exists.**
`docs/technical/parallel-submachines.md`: `Effect::InstantiateSubmachine`,
`ChildSpawner`, per-child `Runtime` with its own queue/context/conversation
store, and `InternalEvent::SubmachineCompleted` carrying a structured result for
append-only aggregation. That is `probe_batch(cells)` with `W` workers.

**sven - the SDK.** `crates/sdk` publishes `Engine` (process-lifetime, shares
the model client), `Agent` (per-task, cheap), `AgentState` (serialized, no live
handles), plus `EngineBuilder::machine` / `::tool` for adding capabilities
without editing sven. An exploration policy driving many agent steps is an
application of this surface, not a change to it.

**brain - the evaluator and the honest-measurement discipline.**
`crates/gradcheck` (finite-difference gradient checking, no PyTorch oracle),
`crates/bench` + `axes.rs` (capability axes, JSON artifacts with arch/size/
params/commit/seed), `crates/perf` (14 scenarios; warm-up never enters a
statistic, failed requests are never goodput and never leave the denominator,
unmeasured fields serialise as `null` never `0`, `compare` refuses to rank
across artifact units). A discovery loop is only as good as its scorer and this
scorer already refuses to produce a flattering number.

**brain - the acceptance gate, already built.**
`crates/promote`: the model-agnostic promote/reject decision - `Environment` /
`Verifier` reward seam, an exact one-sided paired sign test, a four-bar gate
over already-scored pairs. A deliberate leaf crate with no brain dependencies.
**This is PACE's argument, implemented, in our tree, today.**

**brain - the kernels that are the obvious first target.**
`crates/kernels` is a catalogue of hand-written WGSL kernels with a standing
rule that a new kernel must be a proper fast one. KernelBench is the domain
where Dream-RSI's numbers are most consistent, and `brain perf` + `gradcheck`
already supply correctness-gated `1/ms`.

**brain - the world-model crates**, if we ever want the *real* Dreamer idea
rather than replay: `wm-core`, `diamond`, `genieredux`, `wm-display`.

**whale - the scheduling and the existing loop.**
`.agents/roadmap/continuous-learning.md` (whale owns the cross-repo entry point;
sven and brain have their halves) already designs the *weight-level* loop:
detect a gap -> extract facts and self-checkable probes -> schedule a LoRA train
across the node network -> refuse promotion unless it clears a pre-registered
statistical bar and doesn't regress an anchor suite -> hot-swap. `W1a` has
landed.

**The relationship between the two loops is the useful observation:**
continuous-learning improves the *weights* and leaves the search fixed;
Dream-RSI improves the *search* and leaves the weights fixed. They are
orthogonal, they share an acceptance gate, and they share a trajectory format
(`crates/atif`, mirrored by hand into brain under brain's standing "brain never
depends on sven" invariant).

## 8. Where we could do better than the paper

Three of these are not incremental.

**(a) Real branching instead of chain-extension.** The paper's replay can only
walk recorded linear chains because its nodes are filesystem snapshots reached
by one recorded continuation each. `Hsm::snapshot`/`restore` gives us an exact,
cheap, *resumable* node. A recorded node can therefore be re-entered and
continued differently at any time, so our recorded structure can be a genuine
tree - and a replay that walks off the recorded support does not have to fail:
it can **execute** that one step and append it. A hybrid dream - replay where
the log covers, execute where it does not - removes the coverage limit that
defines the paper's method, at a cost that is metered rather than unbounded.

**(b) A real acceptance gate instead of `argmax`.** Replace `pi_{t+1} =
argmax_m V^m` with `promote`'s paired sign test over per-world scores (the
worlds are naturally paired: every policy version is replayed on the same `t`
trees). Keep the incumbent unless the challenger clears the bar. This directly
answers the field's sharpest criticism of the method, and the code exists.

**(c) A typed policy instead of a Python file.** Dream-RSI's policy is an
LLM-edited `OptimalPolicy.solve`. In sven the same object is a machine or an
SDK-level strategy over `Engine`/`Agent`, and the thing being improved can be a
*bounded, typed* set of decisions - open-branch / extend / batch-size / stop -
rather than arbitrary code. Narrower search space, no sandbox question, and it
stays inside the `Effect` gate rather than routing around it. (The
"code-as-action" cluster of ideas was already rejected in
`docs/adr/0003-agents-as-typed-objects.md` for exactly this reason; an
LLM-authored policy file is the same proposal wearing a hat.)

## 9. Honest gap list

Things we do **not** have, that any version of this needs:

- **No discovery tree as a first-class object.** We have per-session trajectories
  and parent-child submachine spawning; we have no persisted, scored,
  re-enterable tree of attempts across sessions, and no store for it.
- **No replay simulator and no exploration-policy seam.** Exploration today is
  whatever a machine's control flow does. There is no `A(T)`, no `probe_batch`,
  no swappable thing that decides where to continue.
- **No cross-session score protocol.** `VerifiedTaskMachine` produces a verdict;
  Dream-RSI's objective needs a comparable scalar per node plus a `fail_class`
  taxonomy (the published prompt leans hard on distinguishing *repairable
  implementation failure* from *hard-unrecoverable* - output/correctness
  mismatch, shared-memory limits, mask/layout/shape errors are normally
  repairable, and `n_valid == 0` is a signal not a closure).
- **No beta sweep / Pareto machinery.** The published evaluator sweeps a single
  `beta` knob and ranks the resulting curve by `pareto.auc - lambda*parallel_penalty`,
  where the penalty is mean `effective_sequential_rounds / total_probes` (~1
  serial, ~1/W for full batches).
- **The prompt-guidance negative result cuts against work in flight.** section 5.1 says
  injecting distilled directional insight *hurt*. Our memory/knowledge-base and
  lessons files do exactly that. One figure on one task is weak evidence and the
  settings differ (they mean "where to search next" in a parallel search, not
  "this API behaves like X") - but it should be measured before we lean harder
  on distilled guidance, not assumed.

## 10. Where it would pay off first

Ranked by (evidence it works) x (scorer we already trust):

1. **WGSL kernel discovery in brain.** Closest to the paper's most consistent
   result; `gradcheck` gives correctness, `perf` gives correctness-gated `1/ms`,
   and brain's `.agents/rules/kernels.md` gives the discovery agent a real
   prior. The evaluator is the expensive part of every discovery loop and here
   it is already built and already refuses to flatter.
2. **Adaptive exploration budget, standalone.** section 5.2's spend-less-while-winning,
   spend-more-on-plateau behaviour needs no replay simulator, no policy
   rewriting, and no new crate - it is a scheduling rule over an existing
   fan-out. Cheapest possible test of whether any of this helps us.
3. **sven's own CI / `VerifiedTaskMachine` fan-out**, where a task has a frozen
   verifier and multiple attempts are already the shape of the work.

Explicitly *not* first: anything open-ended without a hard scorer. The method's
scope is exactly "discovery tasks with a measurable evaluator"; it makes no
claim about reasoning, judgement or honesty, and neither should we.

## 11. What would make this a waste of time

- Building a replay simulator before there is a scored, persisted attempt tree
  worth replaying. The simulator is the cheap half; the node contract and the
  scorer are the expensive half, and we only have the scorer.
- Adopting `argmax` selection. Without a real gate this reliably commits false
  improvements (30-42% in PACE's measurements) and the system churns rather than
  improves.
- Treating the 162x as the expected effect. The controlled number is 1.7x, and
  on one of the two models the aggregate win is an artifact of averaging across
  datasets with incomparable scales.
- Letting "dreaming" imply extrapolation. Replay cannot leave the logged
  support. Either accept that limit or build (8a), but do not conflate them.
