# closing-the-loop

**Status: literature map, nothing accepted for implementation.** This is the
answer to "what does the field already have for the gaps we actually have",
where the gaps are the ones found by reading our own code rather than our own
roadmaps - see the audit note at the bottom, because two roadmap files are
stale about what blocks what.

Companion to [dream-rsi.md](dream-rsi.md), which reads one paper in this space
in depth and concludes we should take very little from it. This file is the
wider sweep.

---

## The gaps, restated

1. **Exploration.** Nothing in sven decides where an attempt should start, or
   recognises that a partial path is heading somewhere good. Machines have
   control flow; they have no search.
2. **Credit assignment.** `rl::atif` broadcasts one session-level reward across
   every token. That is the crudest possible attribution and it is the weakest
   link in a loop whose every other link is careful.
3. **Label-free accumulation.** `Regime::Grpo` measured ACC 0.271 against an
   untrained base of 0.354. The regime that works (`Regime::Sft`) is
   teacher-forced on known-correct completions, so it needs an oracle.
4. **Forgetting across cycles.** Instrumented (retention matrix, BWT,
   plasticity control) but the adapter-accumulation strategy is unsettled.
5. **No real-model run.** Every verification used tiny synthetic fixtures, on
   purpose. There is no task supply with real verifiers to run against.

---

## 1. Exploration

### Which search strategy - and the honest answer

**Heuresis: Search Strategies for Autonomous AI Research Agents Across
Quality, Diversity and Novelty** (Antoniades et al., arXiv:2606.25198,
2026-07-01) is the empirical study this question deserves: six strategies -
Greedy, MAP-Elites, Go-Explore, Islands, Curiosity, Omni (LLM-"interestingness"
gated archive) - across three domains (NanoGPT pretraining, on-policy RL in
MinAtar, model unlearning), **3,222 scored runs**, measuring quality, embedding
diversity, and web-search-graded novelty separately.

What it found, and it is not encouraging for archive-based exploration:

- **No universal winner.** Greedy took quality on NanoGPT and unlearning;
  MAP-Elites and Islands took on-policy RL.
- **MAP-Elites and Go-Explore lead diversity everywhere - and it does not
  convert.** High pairwise embedding distance did not produce better solutions
  on the tasks where Greedy won.
- **Novelty is essentially absent.** *"No idea across our scored runs is rated
  as 'Original'."* Exactly one top-10 idea scored novelty <= 2.
- Their hypothesis for when each works: **recombination** (MAP-Elites, Islands)
  for parameter-like mutations, **sequential** (Greedy, Curiosity) for
  code-level edits, **gating** (Omni) for narrow-literature domains.

**Read for us:** our target is code-level edits, which is the regime their own
hypothesis assigns to *sequential* search - i.e. close to what we already do.
Before building an archive, run the cheap comparison. This paper is also the
strongest available evidence against the premise that a search strategy is
where the leverage is.

### Knowing a partial path leads somewhere

This is the half Dream-RSI does not address at all, and it is the half we
actually want.

- **LATS** (arXiv:2310.04406, ICML) - MCTS where the LM is policy, state
  evaluator and reflection generator: UCT-select a leaf, expand with sampled
  actions, evaluate, simulate to terminal, backpropagate, reflect on failure.
- **SWE-Search** (arXiv:2410.20285) - the same shape on SWE-bench Lite, and the
  useful number: **the value function converged on the correct solution 73% of
  the time across five models, and a discriminator raised correct-solution
  selection to 84%.** That is a measured answer to "can a model tell which
  branch is winning" - not great, well above chance.
- **DARS** (arXiv:2503.14269) - dynamic action re-sampling, adaptive tree
  traversal, for coding agents specifically.
- **SWE-TRACE** (arXiv:2604.14820) - rubric process reward models plus
  heuristic test-time scaling for long-horizon SWE agents.
- **R2E-Gym's hybrid verifiers** (below) - combining execution-based and
  execution-free verifiers, which is directly what our `VerifierSpec` (exact,
  slow) plus a learned judge (cheap, approximate) would be.

### When to stop, and how much to spend

**Bayesian Control for Coding Agents** (Papamarkou, Smirnov, Mazanov,
Vazhentsev, Nakov, Baldwin, Shelmanov; arXiv:2606.24453, 2026-06-24). Models
code generation as a POMDP, maintains a Bayesian belief over solution quality,
and uses **Wald sequential analysis** to decide at each step whether to stop
with the current best or keep searching - trading verification cost against
expected improvement. Beats fixed-budget baselines on MBPP+, SWE-bench and
LiveCodeBench; demonstrated compatible with SWE-Agent and OpenHands.

To apply it you need a reliable verification mechanism, a quantifiable quality
metric, and cost parameters. **We have all three.**

**This is the principled version of the one idea worth taking from Dream-RSI**
(its section 5.2 adaptive-budget behaviour), and it is derived rather than
discovered by an LLM rewriting a Python file, gated by a decision rule rather
than by argmax over a nine-world replay set. Prefer it.

---

## 2. Credit assignment - the highest-value adoption

**VICT: Verifier-Instrumented Credit Tracing for Long-Horizon LLM Agent
Reinforcement Learning** (arXiv:2608.28128). Instead of broadcasting the
terminal reward uniformly, it:

1. decomposes the verifier into executable or evidence-backed **atoms**;
2. builds a **proof graph** linking actions to atoms through observable
   evidence changes, using fixed witness predicates (writes, reveals, commits,
   violations);
3. runs a dependency-closed core search for the atoms that actually determine the
   outcome;
4. redistributes group-normalised advantage **only along verified
   action-to-atom edges**, keeping the terminal reward as the anchor;
5. **abstains** when evidence is incomplete or dependencies ambiguous.

*"Shifts credit assignment from rollout-side inference to verifier-side
tracing."*

Results (Qwen2.5-7B): ALFWorld + WebShop **93.7% average / 83.6% strict**
against GRPO's 77.6% / 66.1% - roughly +16 and +17 points. Edges out
fine-grained baselines (HCAPO, SALT, GiGPO); substantially beats outcome-only
(GRPO, RLOO). +5.3 / +5.1 over Fission-GRPO on tau-Bench retail/airline.
Ablations attribute the gain to the dependency cores, the proof edges and the
abstention - not to dense atoms or temporal proximity.

**What it requires is an unusually exact description of what we already have:**
programmatic verifiers with reconstructible internal logic (`VerifierSpec`,
frozen before the attempt and hash-checked before its verdict is trusted);
observable state/evidence changes in trajectory logs (every `Effect` and
`SessionEvent`, serde-typed and append-only); deterministic dependency rules;
and an abstention path (`SessionOutcome::Unknown`, which we already treat as
skip-never-default).

Most systems would have to build a witness log to adopt VICT. Ours is the
kernel's existing audit trail. If one thing on this page is worth doing, it is
this.

Orientation: **From Reasoning to Agentic: Credit Assignment in RL for LLMs**
(arXiv:2604.09459) surveys 47 methods 2024-early 2026 in a granularity x
methodology taxonomy. **Verifiable Process Rewards** (arXiv:2605.10325) covers
the case where intermediate actions are checkable by symbolic oracles.

---

## 3. Why the label-free regime did not accumulate

**Does RL Really Incentivize Reasoning Capacity in LLMs Beyond the Base Model?**
(arXiv:2504.13837, NeurIPS 2025; "Limit of RLVR"). Across model families, RL
algorithms, and math/coding/visual benchmarks: **RLVR-trained models beat their
base at small k, and the base beats them at large pass@k.** Six popular RLVR
algorithms perform similarly and all remain far from the base model's ceiling.
The conclusion is that RLVR sharpens a distribution the base already contains
rather than adding reasoning patterns - **and that distillation does introduce
genuinely new patterns.**

This is a candidate explanation for brain's own GRPO result (ACC 0.271 below a
base of 0.354) that does not require the run to have been misconfigured, and it
says the fix is not to tune GRPO harder. brain already has `DistillTopK` (P14,
done) - that is the lever this literature endorses.

**On chasing label-free self-improvement anyway:** don't, on this evidence.
TTRL improves and then collapses after roughly 50 steps; self-consistency
rewards produce template collapse, where the model learns to emit a fixed
answer regardless of input, because the feedback is self-reinforcing.
Mitigations exist and are worth knowing if we ever need them - **RESTRAIN**
(arXiv:2510.02172; self-penalisation, discounting low-self-consistency
prompts), **Co-Reward** (arXiv:2508.00410, ICLR 2026), and RLER (reported
stable to 1M unlabeled samples) - but the framing paper for the failure mode
itself, *Self-Improvement Can Self-Regress* (arXiv:2606.21090), is a v1
single-author preprint and should be cited for vocabulary, not evidence.

**We are not in the label-free case and should stop acting as if the
interesting version of this problem is.** We have a real verifier. Verifier-
grounded training with good credit assignment (section 2) plus distillation is
the path the evidence supports.

---

## 4. Forgetting across cycles

**Merge before Forget: A Single LoRA Continual Learning via Continual Merging**
(SLAO; arXiv:2512.23017, ICLR 2026) - orthogonally initialise each new LoRA and
sequentially merge it into one fixed-size adapter: constant memory, no linear
parameter growth, no frozen stack of past adapters to interfere.

Relevant when adapter chaining becomes the shape of the loop. brain's existing
50/50 rehearsal mixing and its retention-matrix/BWT instrumentation already
cover the measurement side; this is about what to do with the *k*-th adapter.

---

## 5. Running with a real model: the bottleneck is tasks, not weights

- **R2E-Gym** (arXiv:2504.07164, COLM 2025) - 8.1K procedurally curated
  executable SWE tasks. The recipe, **SWEGEN**, curates executable environments
  by **test generation and back-translation directly from commits**,
  deliberately removing the dependence on human-written issues or unit tests.
  51% on SWE-bench Verified with open weights; hybrid execution-based +
  execution-free verifiers.
- **SWE-Gym** - 2,438 executable tasks from real repositories.
- **SWE-smith** - 50K instances synthesised from 128 repositories.
- **DeepSWE** - the scale marker: 4,500 R2E-Gym tasks, **64 H100s, six days**,
  SOTA among open-weight models. Useful mainly for calibrating what full-blown
  agentic RL costs versus our LoRA-scale ambitions.

**The idea worth extracting:** SWEGEN's recipe applies to our own repositories
directly. sven, brain and whale have thousands of real commits, and each has a
real, fast, already-trusted verifier - `make check`, `make test`,
`cargo run -p xtask -- arch`, `gradcheck`, the bats suite. Back-translating a
task from a commit and generating tests against it is how we mint a verified
task supply without inventing environments or paying for human labels. It also
sidesteps benchmark contamination, because these commits are ours.

That, rather than a benchmark download, is the concrete answer to "we need to
run this with a real model".

---

## What the evidence says NOT to do

- Do not expect search diversity to convert into quality on code-level edits.
  Heuresis measured the opposite, and its own hypothesis puts our regime in
  Greedy's column.
- Do not tune GRPO expecting new capability. Limit-of-RLVR says the ceiling is
  the base model's own distribution; distillation is the lever that moves it.
- Do not build label-free self-rewarding. It collapses, reproducibly, and we
  have a verifier that makes the whole question unnecessary.
- Do not adopt argmax selection anywhere in the loop. `crates/promote` exists;
  PACE (arXiv:2606.08106) measured 30-42% false commits for greedy acceptance.

## Suggested order, if any of this is taken up

1. **VICT-style credit tracing** - unique fit to our substrate, largest
   measured effect, and it makes every later training run worth more.
2. **Task supply from our own commit history**, SWEGEN-style, verified by the
   `make` targets we already trust.
3. **Bayesian stopping** in place of a learned exploration budget.
4. **Verifier-guided search** (SWE-Search / LATS shape) only once 1-3 show
   signal, and only with the value function measured, not assumed - 73% is the
   number to beat.
5. **Distillation over more GRPO.**

## Audit note: two roadmaps are stale

Found while checking what actually blocks what, and worth correcting before
anyone plans against them:

- brain's `.agents/roadmap/self-improve.md` marks P6 blocked on sven because
  "sven's trajectories carry no reward signal". **They do** -
  `crates/session-store/src/reward.rs` stamps `final_metrics.extra.reward`,
  wired into `sven-ci`, and brain's own `rl::atif::ingest_dir` already reads
  that exact path and skips anything unstamped. Both halves of the wire
  contract are implemented and they agree.
- brain's `.agents/roadmap/continuous-learning.md` says "planned, not started",
  but `rl::document` exists and `brain serve --watch-adapters DIR` is wired
  into `run_cli.rs` and tested.
- The real remaining gap is narrower than either file implies: the chain
  `ingest_dir -> fit_weighted -> adapter` exists only as a **test helper**
  (`stage_adapter`, inside `mod tests` in `crates/cli/src/continuous_train.rs`),
  nothing runs it on a schedule, and `promote` is not in that path.
