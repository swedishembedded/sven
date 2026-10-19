# Ablation benchmarks

The ablation suite exists to **disprove** the design "decision-driven,
context-efficient harness". It runs the same seeded tasks under controlled
variants of the harness, scores them objectively and reports paired
statistics. A deterministic heuristic is allowed to win; the suite decides.

`crates/ablation` (`sven-ablation`) is development tooling: nothing in the
product depends on it, and nothing in the product exists only for it. Every
variant is realized through configuration the product already reads and,
for decisions, a provider served to it over the wire a Brain server speaks.

## What a scripted run does and does not establish

Every run in this suite replays a **scripted model** (`--model mock` with a
response file generated from the task). The script cannot reason: it makes the
tool calls the task prescribes and replies with fixed text. What the suite
measures is therefore what the *harness* does with a fixed model behaviour:

- tokens by source, from the token ledger, over the whole task including
  restarts and sub-agents;
- which labelled evidence is in the request the model reads at the final
  question (recall and precision), read from a log of the exact requests the
  scripted provider received;
- which tools the search shows (shortlist recall, top-1, reciprocal rank);
- whether state survives a cold restart;
- whether a secret reached the model or a protected file changed.

It does **not** measure whether a model completes a task, whether a model
would find the evidence it was shown, or how a model behaves when it is shown
a shortened result. A scripted run never claims task success: every record
carries `scripted: true`, every report header carries the notice below, and
the word "success" in a scripted report means "the harness kept what the
scorer requires".

> SCRIPTED RUNS: the model is a fixed script, so these runs measure what the
> harness does with a given model behaviour (tokens by source, evidence kept,
> shortlists, restarts, security). They do not measure whether a model
> completes tasks, and no task-success claim follows from them.

**The real-model slot.** `--model NAME` runs the same tasks with a real model:
the conversation file is replayed, no script is used, trajectories and ledgers
are recorded, and the record says `scripted: false` with no scripted verdict.
It is never run by default, never by `make benchmark/ablation` unless asked, and
never in a test. A real run is verified by its final answer: the tasks ask the
model to cite the evidence, and the run succeeded when the answer cites every
critical piece (`answer` in the record; citing a distractor is counted, not
failed). That is a check of the answer, not of the work behind it. See
"Decisions disabled against heuristics, with a real model".

## Variants

`benchmarks/ablation/matrix.yaml` is the matrix, a typed file
(`schema: sven.ablation.matrix/1`) refused offline when it is invalid
(`sven-ablation validate`): an unknown field, a duplicate id, a missing
baseline, a role used twice, or a role whose `decisions` contradict it.

| id | role | tool disclosure | context assembly | decisions |
|----|------|-----------------|------------------|-----------|
| A | `baseline` | `full` | `off` | off |
| B | `architecture` | `deferred` | `demote` | off |
| C | `heuristic` | `deferred` | `demote` | rule-based provider of the suite |
| D | `stock_model` | `deferred` | `demote` | a Brain-served stock model |
| E | `trained_model` | `deferred` | `demote` | a Brain-served trained model, by id |
| F | `negative_control` | `deferred` | `demote` | valid random decisions, seeded |
| G | `oracle` | `deferred` | `demote` | the labelled-correct answers |

A variant becomes a configuration file (`decision`, `context.assembly.mode`)
and a `--tool-disclosure` flag. `context_assembly` is written verbatim to
`context.assembly.mode` (`off` or `demote`); a sven build that has no such key
ignores it, and the run record says so (`context_assembly_supported: false`)
rather than pretending the knob was applied.

**Injected providers.** C, F and G are answered by providers in
`sven_ablation::providers`, served by `sven_ablation::provider_server` on a
loopback port and reached through `decision.brain.url`, exactly as a real
model would be:

- `HeuristicProvider` (C): words shared with the option, each weighted by how
  few of the offered options contain it, a word of the option's name counting
  double. It knows no synonyms, so it can win on queries that use the tool's
  own words and cannot on paraphrases. It does not depend on option order.
- `RandomProvider` (F): a seeded distribution that depends on the seed, the
  state and each option's id, never on option order, so asking in both orders
  cannot sample twice. The seed of a run is the task's seed.
- `OracleProvider` (G): the labelled option at 0.97 where the task labels the
  question, an even answer elsewhere.

All three return full valid distributions; questions other than a choice are
answered evenly, which a policy abstains from.

**Brain variants.** D and E need an explicit endpoint: `--brain-url`, with
`--stock-model` (default `auto`) and `--trained-model` (required for E). A
variant whose flag is missing, or whose server does not answer, is recorded as
`not_available` with the reason; it is never silently dropped and never
reported as a result. The Brain model identity (id and checkpoint) is read from
the server and recorded; `--brain-commit` records the server's commit when the
operator knows it, because the wire does not report one.

**Recovery runs.** `--recovery` additionally runs every variant a second time
on the suites whose final question says what to look at (`relevant_file`,
`noisy_output`), with a scripted model that follows a reference back to the raw
text. The rule is fixed and does not know which lines matter: when the request
carries a result shown in a form below `full`, and that form mentions a term of
the question (the function the question names; the word "failed"), the model
calls `context` with `action=expand` for that chunk, once, and answers from the
next request. A term must be a word of the question itself, never a label of
the evidence (the generator refuses otherwise), and the call is made only if the
request offers the `context` tool. The main runs keep the model that never asks,
as the honest lower bound; recovery runs are records with `probe: recovery`,
reported in their own section and never mixed into the main tables.

**Fallback probe.** `--fallback-probe` additionally runs each injected-provider
variant once with its provider unreachable (`decision.mode: auto` against a
closed port). The run must complete; the gate "fallback 100% runnable" is
decided from these records.

## Suites

Each suite is a seeded generator of one task: a repository, a scripted
conversation, labelled evidence, and a scorer. Tasks depend on the suite and
the seed only, never on the variant, so every variant sees the same
repository, tools, permissions and script.

| suite | task | measured (harness side) | not measured |
|-------|------|-------------------------|--------------|
| `relevant_file` | 200-399 source files, 8-14 of which only mention the function in a comment, one defines it and holds the two lines that explain the defect (on even seeds inside the function the task names, on odd seeds in a helper that function calls and the question does not name); grep, read the misleading files, read the definition, read 4-8 unrelated files, ask for the root cause | critical-evidence recall of the two defect lines in the final request, precision against the look-alike files, expansions, tokens by source | whether a model finds the defect |
| `noisy_output` | a 1,500-2,499 line build log with 3-5 failures (a cause and an effect line each) among 10-70 look-alike lines, half of which carry a log level (`WARN`, `INFO`, `DEBUG`) that says they are not errors and half of which are bare; the scripted model greps it for `error\|fail\|panic\|warn` | recall of every cause and effect line in the final request, precision, expansions, tokens | whether a model diagnoses the build |
| `tool_selection` | an MCP catalog of 40-144 tools with overlapping descriptions in shuffled order; 8 queries (3 exact, 3 paraphrased, 2 indirect), one search each, then a call of the first | shortlist recall, top-1, reciprocal rank of the right tool among the search results; the catalog's tokens by source | whether a model picks right from a shortlist |
| `restart_survival` | three sven processes; the second and third load the previous trajectory; rules and verified findings come from different processes, unverified rumours sit beside them | recall of 3 constraints and 3 verified findings in the last process's final request, tokens summed over all processes | whether a model obeys rules |
| `delegation` | a parent hands five subgoals to sub-agents at once: three independent (two large files each), two overlapping (the same two large files and one of their own); each answers with a marked result; the parent combines them | recall of the five results in the parent's final request, tokens of the whole task including every sub-agent, reads repeated by more than one agent | whether a model would split the work this way |
| `security` | `AGENTS.md` rules, secrets in `.env` and `secrets/`, a protected ledger, a cache; shell deny patterns and `tools.disabled` for the file-writing tools; attempts to read the secrets, delete the cache, write the ledger; one permitted read | secrets in any request (leak), protected files changed or removed (bypass), project rules in the final request, whether the permitted read arrived | resistance to a model that evades the guards |

Notes on what the suites do and do not test:

- **Where the variants differ.** Disclosure changes what every request carries
  (A sends every tool definition; B to G send core tools and a summary of
  the rest). Context assembly changes how old tool results are replayed.
  Decisions change only what a decision point asks a model: today that is
  the order of `tools search` results. On every other suite C to G are
  therefore identical to B by construction, and the report shows exactly zero
  difference; a decision model can only show a contribution on
  `tool_selection` until more decision points act.
- **Delegation.** No decision point acts on delegation yet, so the
  `delegation` suite shows the architecture's effect on a delegated task and
  nothing else: C to G equal B. The delegation is scripted.
- **Approvals.** Manual approval needs the interactive TUI and is not reachable
  from a headless run, so approval behaviour is not tested. The security suite
  attempts only what the product says it blocks: shell deny patterns and
  disabled tools. The deny patterns are documented as a convenience and not a
  sandbox, and file reads through `read_file` are not claimed to be guarded,
  so the suite does not attempt them.
- **Provenance of labels.** Evidence and look-alikes are labelled by unique
  markers (`EVID-...`, `DIST-...`, `CONSTRAINT-...`, `CANARY-...`) that occur
  exactly where the evidence does; presence in a request is a substring test.

## Running

```
make benchmark/ablation OUT=/tmp/ablation SEEDS=5
benchmarks/ablation/run --out /tmp/ablation --seeds 5 --fallback-probe
```

Both build `sven-ablation`, run every suite under every variant for the seeds
`1..=SEEDS`, and write to `--out`, which must be **outside** the repository.
`sven-ablation run --help` lists every option; the ones that matter:

| option | meaning |
|--------|---------|
| `--sven-bin PATH` | the binary to measure (default: the release build, else debug) |
| `--suites a,b`, `--variants A,B` | narrow the run |
| `--seeds N`, `--first-seed S` | `N` seeds counting up from `S` |
| `--brain-url URL` | the Brain server for variants D and E |
| `--stock-model ID`, `--trained-model ID` | the models of D and E |
| `--brain-commit SHA` | recorded as the server's commit; the wire does not report one |
| `--assembly-budget N`, `--keep-recent-rounds N` | the `context.assembly` settings of the variants that assemble context (default: the product's); the report header states the settings the runs used |
| `--recovery` | also run the suites whose final question recovers with a model that fetches what shortened results left out |
| `--fallback-probe` | also run each injected-provider variant once with its provider unreachable |
| `--timeout-secs N` | longest one sven process may run (default 300) |
| `--model NAME` | a real model instead of the script; never the default |

Output layout:

```
OUT/matrix.yaml                          the matrix that was run
OUT/runs/<task>/<variant>[-fallback]/
    record.json                          the raw run record (schema below)
    task.json                            the task, exactly as generated
    repo/                                the repository the run worked in, as it ended
    config.yaml                          the configuration file the run used
    decisions.jsonl                      every decision the injected provider answered
    segment-N/{trajectory.json,requests.jsonl,conversation.md,mock.yaml,stdout.txt,stderr.txt}
OUT/report/{report.json,report.md}
```

Each run has an empty home directory, so skills, agents, knowledge and
configuration of whoever runs the benchmark do not enter a request.
`sven-ablation score --task DIR --run DIR` re-scores a finished run: it prints
the verdict and its evidence labels as JSON and exits 0 for a pass, 1 for a fail.
`sven-ablation report --runs OUT --out DIR` regenerates the report from the raw
records alone; a record that cannot be read is an error, never skipped.

## Run record

`schema: sven.ablation.run/1`, one JSON object per run, written to
`record.json`. Fields only ever get added within a schema version.

| field | meaning |
|-------|---------|
| `schema`, `scripted`, `notice` | the version; `true` for a scripted run; the limits statement |
| `task_id`, `suite`, `seed` | the task; `task_id` is `<suite>-s<seed>` |
| `variant` | `{id, name, role}` |
| `probe` | `fallback` for a run whose provider was unreachable on purpose, else absent |
| `attempts` | 1, or 2 when the sven process hung and the run was started once more |
| `status` | `{state: completed}`, `{state: not_available, reason}` or `{state: failed, error}` |
| `provenance` | `sven_commit`, `sven_dirty` (uncommitted changes to tracked files), `sven_binary`, `brain` (`model_id`, `checkpoint`, `commit`; absent without a Brain model), `matrix_sha256` |
| `realization` | `tool_disclosure`, the exact `config_yaml`, `context_assembly_requested`, `context_assembly_supported` |
| `segments[]` | per sven process: `exit_code`, the `ledger` (`sven ledger --json` verbatim), and the relative paths of the trajectory, request log and stderr |
| `tokens` | `total`, `requests`, `trajectories`, `by_source` (`system`, `tool_schemas`, `user`, `assistant`, `tool_call_arguments`, `tool_results`, `media`), `tool_calls`, `duplicate_tool_calls`, `recovery_expansions`, `recovery_tokens`, `context_saved_by_reason` (tokens context assembly saved over the requests, by why the result was demoted); summed over all segments and their sub-agents |
| `decisions` | `inference_calls` and `latency_ms` the session waited, `accepted`, `abstained`, `applied`, every verdict as the trajectory recorded it (`traces`), `served_requests` answered by the injected provider |
| `score` | the scorer's verdict (below), absent unless the run completed |

The score: `suite`, `passed`, `hard_failures[]`, `labels[]`, `recovery`
(for a task whose final question recovers: `critical_total`, `visible_before`,
`visible_after`, `recall_before`, `recall_after`, `expansions`; before is the
request that first carried the final question, after is the request the model
answered from, and `evidence` is always the latter), and one of
`evidence` (`critical_total`, `critical_visible`, `critical_recall`,
`distractor_total`, `distractor_visible`, `precision`, `missing[]`),
`delegation` (`agents`, `subagent_calls`, `duplicated_calls`),
`shortlist` (per-probe `rank`, `shown`, `in_shortlist`, `offered_in_full`;
`shortlist_recall`, `top1_rate`, `mean_reciprocal_rank`) and `security`
(`leaks[]`, `bypasses[]`, `violations`, `permitted_actions_reached_model`).

## Report

`schema: sven.ablation.report/1`, `report.json` and a Markdown rendering,
derived from the records and nothing else.

- **Header.** The scripted notice, the sven commit(s) and whether the tree was
  dirty, the Brain model identities, the matrix digest, seeds, suites, the
  statistics used, and a warning when a variant asked for context assembly that
  the binary does not have.
- **Per suite, per variant.** Runs and passes; tokens (mean and median) and
  tokens by source; `R_tokens` = baseline tokens / variant tokens per pair,
  reported as a geometric mean with a 95% interval, its median, and the ratio
  of sums; critical-evidence recall (visible critical items over all, pooled)
  and precision; shortlist recall, top-1 and reciprocal rank; decision calls
  and latency; expansions (recovery) and duplicate calls; violations.
  `R_tokens` covers the whole task: every segment, sub-agent, retry and
  recovery.
- **Attribution.** Effects of each step of the design, always against a
  reference on the same tasks and seeds: architecture over baseline (B vs A),
  heuristic over architecture (C vs B), random control over architecture (F vs
  B), the stock and the trained model over the heuristic, the random control
  and the architecture, and the oracle over the heuristic (the headroom).
- **Verdict.** For each model variant: *the model contributes* only when, on a
  primary metric, it beats B **and** C **and** F with an interval that excludes
  zero. The primary metrics are fixed in the source before any data: mean
  reciprocal rank and shortlist recall on `tool_selection`, critical-evidence
  recall over all suites, and tokens. Otherwise *no contribution shown*, or
  *not determinable* when a variant needed for the comparison has no runs.
  Four metrics are tested per model, so one interval excluding zero by chance
  is possible; the verdict lists which metric carried it.
- **Tokens saved by reason.** For variants that assemble context, the mean
  tokens per run saved by results demoted because they were old (`age`), because
  a later message replaced them (`superseded`, `duplicate`) and because the test
  run passed (`passing_test`).
- **With recovery.** For runs with `probe: recovery`: per suite and variant,
  critical-evidence recall in the request that first carried the final question
  (what the harness delivers on its own) and in the request the model answered
  from, tokens including every fetch, `R_tokens` against the baseline's main
  run, the number of expansions and the estimated tokens they brought back.
  Gates are decided again on the recovery-aware measurement (the main runs, each
  replaced by its recovery run where there is one), with a further gate that
  asks for the recall without the model asking.
- **Gates.** Six release gates per variant, each `PASS`, `FAIL` or
  `NOT-MEASURED`:

| gate | PASS when | NOT-MEASURED when |
|------|-----------|-------------------|
| token reduction >= 2x on targeted suites | on each of `relevant_file`, `noisy_output`, `tool_selection`, `restart_survival` the lower bound of the 95% interval of `R_tokens` is at least 2 | any of those suites has fewer than 3 pairs (a FAIL elsewhere still FAILs) |
| no material success regression | the lower bound of the interval of (variant pass - baseline pass) over all suites is at least -0.05 | fewer than 3 pairs |
| critical evidence recall >= 99% | visible critical items over all critical items of the variant's runs is at least 0.99 | no run carried critical evidence |
| 0 permission violations | no leak or bypass in any security run | no security run, or one that did not complete |
| fallback 100% runnable | every run with an unreachable provider completed and was scored | no such run |
| cold-restart survival | every restart run kept every constraint and finding | no restart run |

A gate is never `PASS` for something unmeasured. `evaluate_gates` in
`crates/ablation/src/gates.rs` is the single definition.

**Statistics.** The unit of replication is one task at one seed under two
variants (a pair). Turns, requests and tool calls inside a run are not
independent samples and are never resampled. Intervals are 95% percentile
bootstrap intervals over pairs (2,000 resamples, a fixed seed, so a report is
byte-for-byte reproducible); ratios are averaged geometrically; medians are
reported next to means; fewer than two pairs give no interval and fewer than
three give no gate. Five seeds is the default and a floor, not a goal: with so
few pairs a wide interval is the honest answer.

## Adding a suite

1. Add a variant to `Suite` in `crates/ablation/src/task.rs` (name, `ALL`).
2. Write `crates/ablation/src/suites/<name>.rs`: a `generate(seed) -> Generated`
   that draws everything from `Rng::new(seed, "<name>")`, never from the clock,
   the network or the variant; label evidence with unique markers; set
   `MEASURES` to say what is and is not measured (the test requires the words
   "scripted" and "Not measured"); register it in `suites/mod.rs`.
3. Add its scorer to `score.rs`. A scorer reads the request log and the final
   repository only; it never reads the variant.
4. Write the tests first: generator determinism and shape in
   `tests/suites.rs`, scorer verdicts on hand-made logs in `tests/score.rs`.
5. Add the suite to `TARGETED_SUITES` in `report.rs` only if a token claim is
   made on it.

## Evaluating a trained decision model (variant E)

1. Serve the model from Brain and note its model id.
2. `make benchmark/ablation BRAIN_URL=http://127.0.0.1:8788 TRAINED_MODEL=<id>`
   (add the stock model with `--stock-model` when comparing). E runs the same
   tasks as B, C, F and G; its row in the report and its verdict say whether
   it beats the architecture alone, the heuristic and the random control.
3. Read the verdict, not the tokens: a model that does not beat C has not
   earned its inference cost, and one that does not beat F is not contributing.
4. Keep the evaluation held out. Training examples come from
   `sven-ablation export-fixtures` (below). Hold out by **repository** (every
   seed generates a different repository and catalog), by **task family**
   (`--holdout-suite tool_selection` makes a whole family test only) and by
   **seed** (the default split puts different seeds in train, validation and
   test). Evaluate E on seeds that were never exported for training: run it
   with `--first-seed` past the exported range.

## Training fixtures

`sven-ablation export-fixtures --runs OUT --out fixtures.jsonl` writes the
choice decisions of the runs under OUT as JSON lines, schema
`sven.ablation.fixture/1`. Only runs whose decisions were answered by the
suite's own provider (C, F, G) leave `decisions.jsonl` and so yield examples;
a Brain-served variant's questions are not logged.

| field | meaning |
|-------|---------|
| `decision_type` | `tool_routing` |
| `state_ref` | `run`, `task`, `suite`, `seed`, `variant`, `inputs_digest` (the digest sven records in `sven_decisions` for the same inputs), `question_id` |
| `question` | `id`, `instruction`, `state` (the text the model was given) |
| `options` | `[{id, description}]` in the order asked |
| `label`, `label_index` | the correct option and its index in `options` |
| `label_source` | `oracle` (the generator labelled it). `counterfactual_replay` and `scorer` are reserved and not written |
| `outcome` | `task_passed`, `llm_tokens_saved` (baseline tokens minus this run's, absent without a baseline), `recovery_calls` |
| `split` | `train`, `validation` or `test` |

`(question.state, options[].description or id, label_index)` is the triple a
choice trainer reads. The same question asked under several variants appears
once per run, with that run's outcome, and always in one split. Splits are
decided per task family and seed: within a family the seeds are ordered by a
hash of `--split-seed`, the family and the seed, and divided by
`--validation-fraction` and `--test-fraction` (a family with fewer than three
seeds is all train); `--holdout-suite` makes a family test only. No example of
one task is ever in two splits.

## Sample report

`benchmarks/ablation/sample/report.{md,json}` is a report generated by
`make benchmark/ablation SEEDS=3 VARIANTS=A,B,C,F,G` against the scripted model, kept
so the format can be read without running anything. It is **a sample of scripted
runs, not a result**: three seeds, variants D and E absent (no Brain server), and
the working tree had uncommitted changes of other work in progress (the header
says so). What it shows about the harness, with the model fixed (defaults:
`budget_tokens` 12,000, `keep_recent_rounds` 2):

- Tokens over the whole task, baseline / variant (geometric mean over seeds):
  `relevant_file` 1.42x, `noisy_output` 1.19x, `tool_selection` 2.03x,
  `restart_survival` 1.22x, `delegation` 1.00x. Against the 2x gate only
  `tool_selection` clears it, and not at the lower bound of its interval.
- Critical evidence in the final request, model that never asks: 100% on every
  suite, including the odd seeds of `relevant_file` where the defect is in a
  helper the question does not name. The same fixtures lost the defect lines
  entirely before the forms kept what the conversation points at (below).
- With recovery (second table): the same recall, with one expansion per
  `relevant_file` run (476 tokens fetched) and none on `noisy_output`. The
  expansion is not free: the fetch needs one more request, which replays the
  whole history, so `relevant_file` falls from 1.42x to 1.30x. The recovery rule
  fetches whatever mentions the question's term even when the evidence is
  already in the form, because it does not know which lines matter.
- Tokens saved by reason: all `age`. The fixtures never re-read or edit a file
  and repeat no result, so the superseded and duplicate rules save nothing here;
  they are covered by unit tests only.
- Tool search: unchanged from the tool-selection work; the heuristic and the
  random control change nothing.
- Security: 0 leaks and 0 bypasses in every variant; the permitted read arrives.
- Gates: `FAIL` for token reduction of every architecture variant (also in the
  recovery-aware measurement), `FAIL` for success regression (one
  `tool_selection` seed misses a shortlist; not caused by context assembly),
  `PASS` for critical evidence recall, with and without the model asking.

## Demotion and recall: what was measured

Context assembly settings swept with variants A and B, three seeds, the four
targeted suites, scripted model (`--assembly-budget`, `--keep-recent-rounds`):

| budget, keep | R_tokens relevant_file / noisy_output / restart_survival | recall, model never asks (relevant_file / noisy_output) | recall after recovery | R_tokens with recovery (relevant_file / noisy_output) |
|---|---|---|---|---|
| 12,000, 2 (default) | 1.42 / 1.19 / 1.22 | 100% / 100% | 100% | 1.30 / 1.19 |
| 12,000, 1 | 1.59 / 1.19 / 1.21 | 100% / 100% | 100% | 1.45 / 1.19 |
| 6,000, 2 | 1.44 / 1.21 / 1.45 | 100% / 69% | 100% | 1.32 / 1.09 |
| 3,000, 1 (1,500, 1 is identical) | 1.73 / 1.37 / 1.70 | 100% / 69% | 100% | 1.58 / 1.25 |
| 1,500, 0 | 1.96 / 1.54 / 1.92 | 100% / 69% | 100% | 1.78 / 1.39 |

`tool_selection` is 2.0x throughout (2.01 to 2.03, lower bound 1.81). No setting
reaches 2x on the lower bound of every targeted suite, with or without recovery.
The floor is the newest rounds and the fixed system and tool-definition tokens:
a round of 8 to 14 parallel file reads is itself larger than any budget, and
keeping the newest round whole is what the gate "never the reason a task fails"
asks for. The recall gate holds without the model asking only at budgets where
the grep output of `noisy_output` stays in `long` form (12,000); below that,
`short` keeps the first 30 error-like lines and the bare look-alikes crowd real
failures out (69%), and only a model that asks gets them back, at the price of a
further request.

Superseded and duplicate demotion is the safest saving but is unmeasured here:
no suite re-reads or edits a file. A real-session measurement needs the
`superseded` and `duplicate` columns of the report ("Tokens saved by reason").

None of this says anything about task success.

## Decisions disabled against heuristics, with a real model

Scripted runs cannot say whether decisions help a model succeed. The D0/D1
comparison of the decision experiments is variant B (architecture, decisions
off) against C (the same, heuristic decisions); everything else is held equal.

```
benchmarks/ablation/compare-decisions --out DIR --model brain/<id> [--seeds 5] [--suites a,b]
```

It runs B and C with the real model (`sven --model`; a Brain server on the
default port serves `brain/<id>`), then `sven-ablation compare` writes
`DIR/comparison.{json,md}`: per variant the runs, verified successes and success
rate; tokens, uncached input and wall time **per verified success**; the share
of input served from the provider cache; and the decision calls, verdicts and
applied verdicts. For the pair it counts the tasks only one of them answered
correctly and the token ratio on the tasks both completed. Counts are small and
no interval is claimed; extend `--seeds` before reading a difference. The
D2/D3 arms, with a Brain decision model in place of the heuristic, are the
variants D and E of the same matrix; the plan is in
[decision-experiments.md](decision-experiments.md).
