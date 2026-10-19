# Decision experiments: the arms and the GPU run plan

<!--
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
SPDX-License-Identifier: Apache-2.0

Swedish Embedded AB implements solutions for proving, with controlled
experiments, that an optimisation of an LLM agent pays for itself. If your
team needs expertise in agent benchmarking then you can procure our
services by sending an email to info@swedishembedded.com.
-->

Do decisions help an agent finish tasks for less? Four arms answer it, each
adding one thing to the one before. They are runs of the
[ablation matrix](ablation-benchmarks.md); nothing else varies.

| arm | matrix variant | decisions | what it tells |
|-----|----------------|-----------|---------------|
| D0 | B `architecture` | off | the cost and success of the architecture alone: the reference |
| D1 | C `heuristic` | deterministic rules | what a model has to beat |
| D2 | D `stock-model` | a stock Brain decision model | whether any small model earns its inference |
| D3 | E `trained-model` | the trained Brain decision model | whether training earned more than D2 |

The random control (F) and the oracle (G) bracket the arms in scripted runs;
they are not part of the GPU plan.

## What is measured

Scripted runs measure the harness: tokens by source, evidence kept,
shortlists, restarts, security. They do not say whether a model succeeds. The
arms are therefore also run with a **real task model**, and each run is
verified by its final answer (it must cite every critical piece of evidence;
`answer` in the run record). Per arm, `sven-ablation compare` reports

- verified success rate;
- tokens, uncached input and wall time **per verified success**;
- the share of input served from the provider cache;
- decision calls, accepted, abstained and applied verdicts.

For a decision arm the per-decision-point quality comes from the traces in the
run records: `sven_decision::quality::point_quality` gives, per policy, the
abstention reasons, the stability of the answers, and, with labels (the task's
oracle labels, `oracle_labels` in `task.json`), accuracy on accepted
answers, coverage and calibration ([decisions](decisions.md)).

## Order of the runs

Each step has a gate; a failed gate stops the plan and the failure is the
result.

1. **D0 against D1**, real task model, 5 seeds on every suite. No decision
   model is involved, so this needs only the task model.

   ```
   benchmarks/ablation/compare-decisions --out OUT/d0-d1 --model brain/<task-model> --seeds 5
   ```

   Gate: D1 is not worse than D0 on verified success, and tokens per verified
   success are not higher. If D1 loses, heuristics do not pay and D2 and D3 are
   measured against D0 only.
2. **Decision model in isolation**, no task model. Run the scripted matrix with
   variants D and E and read the per-point quality (accuracy against the oracle
   labels, coverage, calibration) before spending GPU time on end-to-end runs.
   Gate: accuracy on accepted answers above the heuristic's (variant C) and the
   random control's (F), calibration error reported.
3. **D2 and D3 end to end**, real task model, the seeds of step 1 plus
   held-out seeds (`--first-seed` past the seeds exported for training), so the
   trained model is never judged on what it trained on.

   ```
   benchmarks/ablation/run --out OUT/d2-d3 --variants B,C,D,E --seeds 5 \
       --first-seed 100 --model brain/<task-model> \
       --brain-url http://127.0.0.1:8788 --stock-model auto --trained-model <id>
   sven-ablation compare --runs OUT/d2-d3 --out OUT/d2-d3 --reference C --candidate E
   ```

   Gate: D3 beats D1 and D0 on verified success or on tokens per verified
   success without losing the other, and beats the random control.

## Hardware

Two models are served: the task model and the decision model. Brain serves
both from one process on one card; the decision model is small, and the task
model sets the card's memory. Wrap each run in the shared GPU lock, keep VRAM
under the card's limit with both loaded, and record the card, the Brain commit
(`--brain-commit`) and the model checkpoints. GPU nondeterminism across cards is
real: compare arms on the same card, and repeat a surprising difference on
another before believing it.

Profile each run against the card's ceiling before reading its numbers. A run
that waits on one unbatched decision call per request is measuring the wait.

## What would make this inconclusive

- Fewer than the minimum seeds for an interval: the comparison prints counts,
  not significance.
- A verifier that passes answers that cite the markers but did not do the work:
  the answer check is a floor, not proof of the work. Read a sample of
  trajectories.
- A task model that succeeds everywhere (nothing to gain) or nowhere (nothing
  to lose); report the ceiling and floor before the arms.
