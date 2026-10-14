# Ablation report

> **SCRIPTED RUNS: the model is a fixed script, so these runs measure what the harness does with a given model behaviour (tokens by source, evidence kept, shortlists, restarts, security). They do not measure whether a model completes tasks, and no task-success claim follows from them.**

- schema: `sven.ablation.report/1`
- sven commit(s): `3eb4ef9110faad152554b0454b6a349c9b53783e` (working tree had uncommitted changes)
- matrix sha256: 2192a3badb85f197f231ea48f8d075db9b762fa153272720721acf694c811004
- 126 runs, seeds [1, 2, 3], suites relevant_file, noisy_output, tool_selection, restart_survival, delegation, security
- context assembly settings: budget_tokens default, keep_recent_rounds default
- statistics: 95% percentile bootstrap, 2000 resamples, seed 0x5eedab1a, unit: one task at one seed run under two variants (a pair); turns and requests are never resampled; no interval below 3 pairs

## Variants

| id | name | role | completed | failed | not available |
|---|---|---|---:|---:|---|
| A | legacy | Baseline | 18 | 0 | - |
| B | architecture | Architecture | 18 | 0 | - |
| C | heuristic | Heuristic | 18 | 0 | - |
| D | stock-model | StockModel | 0 | 0 | - |
| E | trained-model | TrainedModel | 0 | 0 | - |
| F | random-control | NegativeControl | 18 | 0 | - |
| G | oracle | Oracle | 18 | 0 | - |

## Suite `relevant_file`

Harness side only, model behaviour scripted: whether the two lines that explain the defect are still in the request at the final question (critical-evidence recall), the share of visible labelled evidence that is critical (precision), expansions of demoted context, and the tokens by source of the whole task. Not measured: whether a model finds the defect.

| variant | runs | pass | tokens mean | median | R_tokens vs baseline (geo-mean, 95% CI) | median R | critical recall | precision | shortlist recall | top-1 | MRR | decision calls | decision ms | expansions | duplicate calls | violations |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| A | 3 | 3 | 248553 | 267999 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 16.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| B | 3 | 3 | 172756 | 182722 | 1.42 [1.31, 1.49] | 1.47 | 100.0% | 16.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| C | 3 | 3 | 172757 | 182728 | 1.42 [1.31, 1.49] | 1.47 | 100.0% | 16.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| D | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| E | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| F | 3 | 3 | 172757 | 182726 | 1.42 [1.31, 1.49] | 1.47 | 100.0% | 16.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| G | 3 | 3 | 172757 | 182726 | 1.42 [1.31, 1.49] | 1.47 | 100.0% | 16.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |

Mean tokens by source:

| variant | system | tool_schemas | user | assistant | tool_call_arguments | tool_results | media |
|---|---:|---:|---:|---:|---:|---:|---:|
| A | 16724 | 39602 | 769 | 237 | 1816 | 189405 | 0 |
| B | 20128 | 20368 | 769 | 237 | 1816 | 129438 | 0 |
| C | 20128 | 20368 | 769 | 237 | 1816 | 129439 | 0 |
| F | 20128 | 20368 | 769 | 237 | 1816 | 129439 | 0 |
| G | 20128 | 20368 | 769 | 237 | 1816 | 129439 | 0 |

## Suite `noisy_output`

Harness side only, model behaviour scripted: whether the cause and the effect line of every real failure are still in the request at the final question (critical-evidence recall), precision against look-alike lines, expansions of demoted context and the tokens by source of the whole task. Not measured: whether a model diagnoses the build.

| variant | runs | pass | tokens mean | median | R_tokens vs baseline (geo-mean, 95% CI) | median R | critical recall | precision | shortlist recall | top-1 | MRR | decision calls | decision ms | expansions | duplicate calls | violations |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| A | 3 | 3 | 81066 | 89683 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 15.3% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| B | 3 | 3 | 67886 | 75268 | 1.19 [1.18, 1.20] | 1.19 | 100.0% | 15.3% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| C | 3 | 3 | 67886 | 75268 | 1.19 [1.18, 1.20] | 1.19 | 100.0% | 15.3% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| D | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| E | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| F | 3 | 3 | 67886 | 75270 | 1.19 [1.18, 1.20] | 1.19 | 100.0% | 15.3% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| G | 3 | 3 | 67886 | 75268 | 1.19 [1.18, 1.20] | 1.19 | 100.0% | 15.3% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |

Mean tokens by source:

| variant | system | tool_schemas | user | assistant | tool_call_arguments | tool_results | media |
|---|---:|---:|---:|---:|---:|---:|---:|
| A | 11300 | 26766 | 424 | 97 | 399 | 42081 | 0 |
| B | 13600 | 13240 | 424 | 97 | 399 | 40127 | 0 |
| C | 13600 | 13240 | 424 | 97 | 399 | 40127 | 0 |
| F | 13600 | 13240 | 424 | 97 | 399 | 40127 | 0 |
| G | 13600 | 13240 | 424 | 97 | 399 | 40127 | 0 |

## Suite `tool_selection`

Harness side only, model behaviour scripted: whether the tool that answers each query is among the search results shown to the model (shortlist recall), whether it is first (top-1), reciprocal rank, and the tokens the catalog costs by source. Under full disclosure the shortlist is the whole catalog and top-1 is not measured. Not measured: whether a model chooses correctly from a shortlist.

| variant | runs | pass | tokens mean | median | R_tokens vs baseline (geo-mean, 95% CI) | median R | critical recall | precision | shortlist recall | top-1 | MRR | decision calls | decision ms | expansions | duplicate calls | violations |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| A | 3 | 3 | 175056 | 181239 | 1.00 [1.00, 1.00] | 1.00 | - | - | 100.0% | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| B | 3 | 2 | 86014 | 85923 | 2.03 [1.81, 2.19] | 2.10 | - | - | 95.8% | 66.7% | 0.795 | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| C | 3 | 2 | 86014 | 85923 | 2.03 [1.81, 2.19] | 2.10 | - | - | 95.8% | 66.7% | 0.795 | 4.7 | 218.0 | 0.0 | 0.0 | 0 |
| D | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| E | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| F | 3 | 2 | 86014 | 85923 | 2.03 [1.81, 2.19] | 2.10 | - | - | 95.8% | 66.7% | 0.795 | 4.3 | 195.7 | 0.0 | 0.0 | 0 |
| G | 3 | 2 | 86014 | 85923 | 2.03 [1.81, 2.19] | 2.10 | - | - | 95.8% | 79.2% | 0.875 | 4.7 | 212.0 | 0.0 | 0.0 | 0 |

Mean tokens by source:

| variant | system | tool_schemas | user | assistant | tool_call_arguments | tool_results | media |
|---|---:|---:|---:|---:|---:|---:|---:|
| A | 24408 | 146202 | 1991 | 432 | 1600 | 423 | 0 |
| B | 27778 | 29619 | 2194 | 480 | 1803 | 24140 | 0 |
| C | 27778 | 29619 | 2194 | 480 | 1803 | 24140 | 0 |
| F | 27778 | 29619 | 2194 | 480 | 1803 | 24140 | 0 |
| G | 27778 | 29619 | 2194 | 480 | 1803 | 24140 | 0 |

## Suite `restart_survival`

Harness side only, model behaviour scripted: whether every constraint and every verified finding is still in the request at the final question after two cold restarts (survival), precision against unverified rumours, and the tokens summed over all processes of the task. Not measured: whether a model obeys the constraints or reasons from the findings.

| variant | runs | pass | tokens mean | median | R_tokens vs baseline (geo-mean, 95% CI) | median R | critical recall | precision | shortlist recall | top-1 | MRR | decision calls | decision ms | expansions | duplicate calls | violations |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| A | 3 | 3 | 152653 | 149814 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 38.7% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| B | 3 | 3 | 125073 | 124348 | 1.22 [1.20, 1.24] | 1.21 | 100.0% | 38.7% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| C | 3 | 3 | 125073 | 124348 | 1.22 [1.20, 1.24] | 1.21 | 100.0% | 38.7% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| D | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| E | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| F | 3 | 3 | 125073 | 124348 | 1.22 [1.20, 1.24] | 1.21 | 100.0% | 38.7% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| G | 3 | 3 | 125073 | 124348 | 1.22 [1.20, 1.24] | 1.21 | 100.0% | 38.7% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |

Mean tokens by source:

| variant | system | tool_schemas | user | assistant | tool_call_arguments | tool_results | media |
|---|---:|---:|---:|---:|---:|---:|---:|
| A | 17641 | 42198 | 1478 | 219 | 762 | 90355 | 0 |
| B | 21229 | 20943 | 1478 | 219 | 762 | 80442 | 0 |
| C | 21229 | 20943 | 1478 | 219 | 762 | 80442 | 0 |
| F | 21229 | 20943 | 1478 | 219 | 762 | 80442 | 0 |
| G | 21229 | 20943 | 1478 | 219 | 762 | 80442 | 0 |

## Suite `delegation`

Harness side only, delegation scripted: whether every sub-agent's result is in the parent's request at the end, the tokens of the whole task including every sub-agent, and the reads that more than one agent made of the same file (duplication). Not measured: whether a model would split the work this way or combine the results well.

| variant | runs | pass | tokens mean | median | R_tokens vs baseline (geo-mean, 95% CI) | median R | critical recall | precision | shortlist recall | top-1 | MRR | decision calls | decision ms | expansions | duplicate calls | violations |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| A | 3 | 3 | 81256 | 81243 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| B | 3 | 3 | 81441 | 81502 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| C | 3 | 3 | 81441 | 81502 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| D | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| E | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| F | 3 | 3 | 81441 | 81502 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| G | 3 | 3 | 81441 | 81502 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |

Mean tokens by source:

| variant | system | tool_schemas | user | assistant | tool_call_arguments | tool_results | media |
|---|---:|---:|---:|---:|---:|---:|---:|
| A | 35125 | 25258 | 401 | 6 | 665 | 19801 | 0 |
| B | 37584 | 22844 | 421 | 6 | 698 | 19888 | 0 |
| C | 37584 | 22844 | 421 | 6 | 698 | 19888 | 0 |
| F | 37584 | 22844 | 421 | 6 | 698 | 19888 | 0 |
| G | 37584 | 22844 | 421 | 6 | 698 | 19888 | 0 |

## Suite `security`

Harness side only, model behaviour scripted: whether a secret reached the model, whether a protected file changed or disappeared, whether the project rules are in the request at the final question, and whether a permitted read got through. Any leak or bypass is a hard failure. Not measured: resistance to a model that tries to evade the guards.

| variant | runs | pass | tokens mean | median | R_tokens vs baseline (geo-mean, 95% CI) | median R | critical recall | precision | shortlist recall | top-1 | MRR | decision calls | decision ms | expansions | duplicate calls | violations |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| A | 3 | 3 | 58469 | 58950 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| B | 3 | 3 | 43329 | 43329 | 1.35 [1.33, 1.36] | 1.36 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| C | 3 | 3 | 43329 | 43329 | 1.35 [1.33, 1.36] | 1.36 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| D | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| E | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| F | 3 | 3 | 43329 | 43329 | 1.35 [1.33, 1.36] | 1.36 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| G | 3 | 3 | 43329 | 43329 | 1.35 [1.33, 1.36] | 1.36 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |

Mean tokens by source:

| variant | system | tool_schemas | user | assistant | tool_call_arguments | tool_results | media |
|---|---:|---:|---:|---:|---:|---:|---:|
| A | 18109 | 37843 | 753 | 233 | 446 | 1085 | 0 |
| B | 23366 | 17038 | 828 | 259 | 547 | 1291 | 0 |
| C | 23366 | 17038 | 828 | 259 | 547 | 1291 | 0 |
| F | 23366 | 17038 | 828 | 259 | 547 | 1291 | 0 |
| G | 23366 | 17038 | 828 | 259 | 547 | 1291 | 0 |

## Attribution

Each row compares two variants on the same tasks and seeds. `ratio` is reference / variant (above 1 is a saving); `difference` is variant minus reference (above 0 is better). An effect counts only when its interval excludes the no-effect value.

| comparison | scope | metric | pairs | median | estimate [95% CI] | excludes no-effect |
|---|---|---|---:|---:|---|---|
| architecture saving (B vs A) | all | Tokens ratio | 18 | 1.279 | 1.335 [1.207, 1.496] | yes |
| architecture saving (B vs A) | all | Pass difference | 18 | 0.000 | -0.056 [-0.167, 0.000] | no |
| architecture saving (B vs A) | all | CriticalRecall difference | 15 | 0.000 | 0.000 [0.000, 0.000] | no |
| architecture saving (B vs A) | all | ShortlistRecall difference | 3 | 0.000 | -0.042 [-0.125, 0.000] | no |
| architecture saving (B vs A) | tool_selection | Tokens ratio | 3 | 2.101 | 2.028 [1.812, 2.193] | yes |
| architecture saving (B vs A) | tool_selection | Pass difference | 3 | 0.000 | -0.333 [-1.000, 0.000] | no |
| architecture saving (B vs A) | tool_selection | ShortlistRecall difference | 3 | 0.000 | -0.042 [-0.125, 0.000] | no |
| heuristic over architecture (C vs B) | all | Tokens ratio | 18 | 1.000 | 1.000 [1.000, 1.000] | no |
| heuristic over architecture (C vs B) | all | Pass difference | 18 | 0.000 | 0.000 [0.000, 0.000] | no |
| heuristic over architecture (C vs B) | all | CriticalRecall difference | 15 | 0.000 | 0.000 [0.000, 0.000] | no |
| heuristic over architecture (C vs B) | all | ShortlistRecall difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| heuristic over architecture (C vs B) | all | Mrr difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| heuristic over architecture (C vs B) | tool_selection | Tokens ratio | 3 | 1.000 | 1.000 [1.000, 1.000] | no |
| heuristic over architecture (C vs B) | tool_selection | Pass difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| heuristic over architecture (C vs B) | tool_selection | ShortlistRecall difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| heuristic over architecture (C vs B) | tool_selection | Mrr difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| random control over architecture (F vs B) | all | Tokens ratio | 18 | 1.000 | 1.000 [1.000, 1.000] | no |
| random control over architecture (F vs B) | all | Pass difference | 18 | 0.000 | 0.000 [0.000, 0.000] | no |
| random control over architecture (F vs B) | all | CriticalRecall difference | 15 | 0.000 | 0.000 [0.000, 0.000] | no |
| random control over architecture (F vs B) | all | ShortlistRecall difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| random control over architecture (F vs B) | all | Mrr difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| random control over architecture (F vs B) | tool_selection | Tokens ratio | 3 | 1.000 | 1.000 [1.000, 1.000] | no |
| random control over architecture (F vs B) | tool_selection | Pass difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| random control over architecture (F vs B) | tool_selection | ShortlistRecall difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| random control over architecture (F vs B) | tool_selection | Mrr difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| oracle headroom over heuristic (G vs C) | all | Tokens ratio | 18 | 1.000 | 1.000 [1.000, 1.000] | no |
| oracle headroom over heuristic (G vs C) | all | Pass difference | 18 | 0.000 | 0.000 [0.000, 0.000] | no |
| oracle headroom over heuristic (G vs C) | all | CriticalRecall difference | 15 | 0.000 | 0.000 [0.000, 0.000] | no |
| oracle headroom over heuristic (G vs C) | all | ShortlistRecall difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| oracle headroom over heuristic (G vs C) | all | Mrr difference | 3 | 0.083 | 0.080 [0.062, 0.094] | yes |
| oracle headroom over heuristic (G vs C) | tool_selection | Tokens ratio | 3 | 1.000 | 1.000 [1.000, 1.000] | no |
| oracle headroom over heuristic (G vs C) | tool_selection | Pass difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| oracle headroom over heuristic (G vs C) | tool_selection | ShortlistRecall difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| oracle headroom over heuristic (G vs C) | tool_selection | Mrr difference | 3 | 0.083 | 0.080 [0.062, 0.094] | yes |

## Does a model contribute?

- variant D: **not determinable**: variant D has no completed runs
- variant E: **not determinable**: variant E has no completed runs

## Release gates

### Variant B

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **FAIL** | lower bound of the 95% interval must be >= 2; relevant_file 1.42x [1.31, 1.49]; noisy_output 1.19x [1.18, 1.20]; tool_selection 2.03x [1.81, 2.19]; restart_survival 1.22x [1.20, 1.24] |
| no material success regression | **FAIL** | pass rate difference -0.056 [-0.167, +0.000] over 18 pairs, lower bound must be >= -0.05; scripted pass means the harness kept what the scorer requires, not that a model succeeded |
| critical evidence recall >= 99% | **PASS** | 71 of 71 critical items reached the model (100.00%) |
| 0 permission violations | **PASS** | 0 violations in 3 security runs |
| fallback 100% runnable | **NOT-MEASURED** | no run with an unreachable decision provider |
| cold-restart survival | **PASS** | every constraint and verified finding survived in 3 of 3 restart runs |

### Variant C

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **FAIL** | lower bound of the 95% interval must be >= 2; relevant_file 1.42x [1.31, 1.49]; noisy_output 1.19x [1.18, 1.20]; tool_selection 2.03x [1.81, 2.19]; restart_survival 1.22x [1.20, 1.24] |
| no material success regression | **FAIL** | pass rate difference -0.056 [-0.167, +0.000] over 18 pairs, lower bound must be >= -0.05; scripted pass means the harness kept what the scorer requires, not that a model succeeded |
| critical evidence recall >= 99% | **PASS** | 71 of 71 critical items reached the model (100.00%) |
| 0 permission violations | **PASS** | 0 violations in 3 security runs |
| fallback 100% runnable | **PASS** | 2 of 2 runs with the provider unreachable completed |
| cold-restart survival | **PASS** | every constraint and verified finding survived in 3 of 3 restart runs |

### Variant D

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **NOT-MEASURED** | lower bound of the 95% interval must be >= 2; needs at least 3 pairs: relevant_file (0 pairs), noisy_output (0 pairs), tool_selection (0 pairs), restart_survival (0 pairs) |
| no material success regression | **NOT-MEASURED** | 0 pairs, need at least 3 |
| critical evidence recall >= 99% | **NOT-MEASURED** | no run carried critical evidence |
| 0 permission violations | **NOT-MEASURED** | 0 security runs completed, 0 did not |
| fallback 100% runnable | **NOT-MEASURED** | no run with an unreachable decision provider |
| cold-restart survival | **NOT-MEASURED** | no restart run completed |

### Variant E

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **NOT-MEASURED** | lower bound of the 95% interval must be >= 2; needs at least 3 pairs: relevant_file (0 pairs), noisy_output (0 pairs), tool_selection (0 pairs), restart_survival (0 pairs) |
| no material success regression | **NOT-MEASURED** | 0 pairs, need at least 3 |
| critical evidence recall >= 99% | **NOT-MEASURED** | no run carried critical evidence |
| 0 permission violations | **NOT-MEASURED** | 0 security runs completed, 0 did not |
| fallback 100% runnable | **NOT-MEASURED** | no run with an unreachable decision provider |
| cold-restart survival | **NOT-MEASURED** | no restart run completed |

### Variant F

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **FAIL** | lower bound of the 95% interval must be >= 2; relevant_file 1.42x [1.31, 1.49]; noisy_output 1.19x [1.18, 1.20]; tool_selection 2.03x [1.81, 2.19]; restart_survival 1.22x [1.20, 1.24] |
| no material success regression | **FAIL** | pass rate difference -0.056 [-0.167, +0.000] over 18 pairs, lower bound must be >= -0.05; scripted pass means the harness kept what the scorer requires, not that a model succeeded |
| critical evidence recall >= 99% | **PASS** | 71 of 71 critical items reached the model (100.00%) |
| 0 permission violations | **PASS** | 0 violations in 3 security runs |
| fallback 100% runnable | **PASS** | 2 of 2 runs with the provider unreachable completed |
| cold-restart survival | **PASS** | every constraint and verified finding survived in 3 of 3 restart runs |

### Variant G

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **FAIL** | lower bound of the 95% interval must be >= 2; relevant_file 1.42x [1.31, 1.49]; noisy_output 1.19x [1.18, 1.20]; tool_selection 2.03x [1.81, 2.19]; restart_survival 1.22x [1.20, 1.24] |
| no material success regression | **FAIL** | pass rate difference -0.056 [-0.167, +0.000] over 18 pairs, lower bound must be >= -0.05; scripted pass means the harness kept what the scorer requires, not that a model succeeded |
| critical evidence recall >= 99% | **PASS** | 71 of 71 critical items reached the model (100.00%) |
| 0 permission violations | **PASS** | 0 violations in 3 security runs |
| fallback 100% runnable | **PASS** | 2 of 2 runs with the provider unreachable completed |
| cold-restart survival | **PASS** | every constraint and verified finding survived in 3 of 3 restart runs |

## Tokens saved by reason

Mean estimated tokens per run that context assembly saved, summed over the requests of the task, by why the result was demoted: `age` (the only copy of its evidence, oldest first), `superseded` and `duplicate` (a later message replaced it), `passing_test`.

| suite | variant | age | superseded | duplicate | passing_test |
|---|---|---:|---:|---:|---:|
| relevant_file | B | 59967 | - | - | - |
| relevant_file | C | 59966 | - | - | - |
| relevant_file | F | 59966 | - | - | - |
| relevant_file | G | 59966 | - | - | - |
| noisy_output | B | 1954 | - | - | - |
| noisy_output | C | 1954 | - | - | - |
| noisy_output | F | 1953 | - | - | - |
| noisy_output | G | 1954 | - | - | - |
| restart_survival | B | 9913 | - | - | - |
| restart_survival | C | 9913 | - | - | - |
| restart_survival | F | 9913 | - | - | - |
| restart_survival | G | 9913 | - | - | - |

## With recovery

The tasks whose final question names what to look at, run a second time with a scripted model that follows a reference back to the raw text when the shortened result mentions a term of the question (a fixed rule that does not know which lines matter). The main tables above are the model that never asks. `recall before` is the critical evidence in the request that first carried the final question, which is all the harness delivers on its own; `recall after` is the request the model answered from. Tokens include every fetch. Not measured: whether a real model would ask.

### Suite `relevant_file`

| variant | runs | pass | recall before | recall after | tokens mean | R_tokens vs baseline (geo-mean, 95% CI) | median R | expansions | tokens fetched |
|---|---:|---:|---:|---:|---:|---|---:|---:|---:|
| A | 3 | 3 | 100.0% | 100.0% | 248578 | 1.00 [1.00, 1.00] | 1.00 | 0.0 | 0 |
| B | 3 | 3 | 100.0% | 100.0% | 188131 | 1.30 [1.19, 1.38] | 1.35 | 1.0 | 476 |
| C | 3 | 3 | 100.0% | 100.0% | 188133 | 1.30 [1.19, 1.38] | 1.35 | 1.0 | 476 |
| F | 3 | 3 | 100.0% | 100.0% | 188131 | 1.30 [1.19, 1.38] | 1.35 | 1.0 | 476 |
| G | 3 | 3 | 100.0% | 100.0% | 188133 | 1.30 [1.19, 1.38] | 1.35 | 1.0 | 476 |

### Suite `noisy_output`

| variant | runs | pass | recall before | recall after | tokens mean | R_tokens vs baseline (geo-mean, 95% CI) | median R | expansions | tokens fetched |
|---|---:|---:|---:|---:|---:|---|---:|---:|---:|
| A | 3 | 3 | 100.0% | 100.0% | 81083 | 1.00 [1.00, 1.00] | 1.00 | 0.0 | 0 |
| B | 3 | 3 | 100.0% | 100.0% | 67903 | 1.19 [1.18, 1.20] | 1.19 | 0.0 | 0 |
| C | 3 | 3 | 100.0% | 100.0% | 67903 | 1.19 [1.18, 1.20] | 1.19 | 0.0 | 0 |
| F | 3 | 3 | 100.0% | 100.0% | 67903 | 1.19 [1.18, 1.20] | 1.19 | 0.0 | 0 |
| G | 3 | 3 | 100.0% | 100.0% | 67903 | 1.19 [1.18, 1.20] | 1.19 | 0.0 | 0 |

### Release gates on the recovery-aware measurement

The main runs, each replaced by its recovery run where there is one.

#### Variant B

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **FAIL** | lower bound of the 95% interval must be >= 2; relevant_file 1.30x [1.19, 1.38]; noisy_output 1.19x [1.18, 1.20]; tool_selection 2.03x [1.81, 2.19]; restart_survival 1.22x [1.20, 1.24] |
| no material success regression | **FAIL** | pass rate difference -0.056 [-0.167, +0.000] over 18 pairs, lower bound must be >= -0.05; scripted pass means the harness kept what the scorer requires, not that a model succeeded |
| critical evidence recall >= 99% | **PASS** | 71 of 71 critical items reached the model (100.00%) |
| critical evidence recall >= 99% without the model asking | **PASS** | 71 of 71 critical items were in the request when the question was first put (100.00%) |

#### Variant C

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **FAIL** | lower bound of the 95% interval must be >= 2; relevant_file 1.30x [1.19, 1.38]; noisy_output 1.19x [1.18, 1.20]; tool_selection 2.03x [1.81, 2.19]; restart_survival 1.22x [1.20, 1.24] |
| no material success regression | **FAIL** | pass rate difference -0.056 [-0.167, +0.000] over 18 pairs, lower bound must be >= -0.05; scripted pass means the harness kept what the scorer requires, not that a model succeeded |
| critical evidence recall >= 99% | **PASS** | 71 of 71 critical items reached the model (100.00%) |
| critical evidence recall >= 99% without the model asking | **PASS** | 71 of 71 critical items were in the request when the question was first put (100.00%) |

#### Variant D

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **NOT-MEASURED** | lower bound of the 95% interval must be >= 2; needs at least 3 pairs: relevant_file (0 pairs), noisy_output (0 pairs), tool_selection (0 pairs), restart_survival (0 pairs) |
| no material success regression | **NOT-MEASURED** | 0 pairs, need at least 3 |
| critical evidence recall >= 99% | **NOT-MEASURED** | no run carried critical evidence |
| critical evidence recall >= 99% without the model asking | **NOT-MEASURED** | no run carried critical evidence |

#### Variant E

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **NOT-MEASURED** | lower bound of the 95% interval must be >= 2; needs at least 3 pairs: relevant_file (0 pairs), noisy_output (0 pairs), tool_selection (0 pairs), restart_survival (0 pairs) |
| no material success regression | **NOT-MEASURED** | 0 pairs, need at least 3 |
| critical evidence recall >= 99% | **NOT-MEASURED** | no run carried critical evidence |
| critical evidence recall >= 99% without the model asking | **NOT-MEASURED** | no run carried critical evidence |

#### Variant F

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **FAIL** | lower bound of the 95% interval must be >= 2; relevant_file 1.30x [1.19, 1.38]; noisy_output 1.19x [1.18, 1.20]; tool_selection 2.03x [1.81, 2.19]; restart_survival 1.22x [1.20, 1.24] |
| no material success regression | **FAIL** | pass rate difference -0.056 [-0.167, +0.000] over 18 pairs, lower bound must be >= -0.05; scripted pass means the harness kept what the scorer requires, not that a model succeeded |
| critical evidence recall >= 99% | **PASS** | 71 of 71 critical items reached the model (100.00%) |
| critical evidence recall >= 99% without the model asking | **PASS** | 71 of 71 critical items were in the request when the question was first put (100.00%) |

#### Variant G

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **FAIL** | lower bound of the 95% interval must be >= 2; relevant_file 1.30x [1.19, 1.38]; noisy_output 1.19x [1.18, 1.20]; tool_selection 2.03x [1.81, 2.19]; restart_survival 1.22x [1.20, 1.24] |
| no material success regression | **FAIL** | pass rate difference -0.056 [-0.167, +0.000] over 18 pairs, lower bound must be >= -0.05; scripted pass means the harness kept what the scorer requires, not that a model succeeded |
| critical evidence recall >= 99% | **PASS** | 71 of 71 critical items reached the model (100.00%) |
| critical evidence recall >= 99% without the model asking | **PASS** | 71 of 71 critical items were in the request when the question was first put (100.00%) |

