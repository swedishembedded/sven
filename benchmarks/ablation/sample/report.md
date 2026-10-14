# Ablation report

> **SCRIPTED RUNS: the model is a fixed script, so these runs measure what the harness does with a given model behaviour (tokens by source, evidence kept, shortlists, restarts, security). They do not measure whether a model completes tasks, and no task-success claim follows from them.**

- schema: `sven.ablation.report/1`
- sven commit(s): `0f3ad5d5600cc30b172aaa4cebb817d71f034dc9`, `158e6cdb3c53fc45839f4c754ee8b5cb37847465` (working tree had uncommitted changes)
- matrix sha256: 2192a3badb85f197f231ea48f8d075db9b762fa153272720721acf694c811004
- 132 runs, seeds [1, 2, 3], suites relevant_file, noisy_output, tool_selection, restart_survival, delegation, security
- statistics: 95% percentile bootstrap, 2000 resamples, seed 0x5eedab1a, unit: one task at one seed run under two variants (a pair); turns and requests are never resampled; no interval below 3 pairs

## Variants

| id | name | role | completed | failed | not available |
|---|---|---|---:|---:|---|
| A | legacy | Baseline | 18 | 0 | - |
| B | architecture | Architecture | 18 | 0 | - |
| C | heuristic | Heuristic | 18 | 0 | - |
| D | stock-model | StockModel | 0 | 0 | not available: variant D needs a Brain server; pass --brain-url |
| E | trained-model | TrainedModel | 0 | 0 | not available: variant E needs a Brain server; pass --brain-url |
| F | random-control | NegativeControl | 18 | 0 | - |
| G | oracle | Oracle | 18 | 0 | - |

## Suite `relevant_file`

Harness side only, model behaviour scripted: whether the two lines that explain the defect are still in the request at the final question (critical-evidence recall), the share of visible labelled evidence that is critical (precision), expansions of demoted context, and the tokens by source of the whole task. Not measured: whether a model finds the defect.

| variant | runs | pass | tokens mean | median | R_tokens vs baseline (geo-mean, 95% CI) | median R | critical recall | precision | shortlist recall | top-1 | MRR | decision calls | decision ms | expansions | duplicate calls | violations |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| A | 3 | 3 | 294354 | 309599 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 14.2% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| B | 3 | 0 | 195488 | 204618 | 1.48 [1.36, 1.59] | 1.51 | 0.0% | 0.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| C | 3 | 0 | 195492 | 204650 | 1.48 [1.36, 1.59] | 1.51 | 0.0% | 0.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| D | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| E | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| F | 3 | 0 | 195461 | 204578 | 1.48 [1.36, 1.59] | 1.51 | 0.0% | 0.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| G | 3 | 0 | 195473 | 204594 | 1.48 [1.36, 1.59] | 1.51 | 0.0% | 0.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |

Mean tokens by source:

| variant | system | tool_schemas | user | assistant | tool_call_arguments | tool_results | media |
|---|---:|---:|---:|---:|---:|---:|---:|
| A | 17615 | 40547 | 853 | 271 | 2155 | 232913 | 0 |
| B | 21177 | 22016 | 853 | 271 | 2155 | 149016 | 0 |
| C | 21177 | 22016 | 853 | 271 | 2155 | 149020 | 0 |
| F | 21177 | 22016 | 853 | 271 | 2155 | 148989 | 0 |
| G | 21177 | 22016 | 853 | 271 | 2155 | 149001 | 0 |

## Suite `noisy_output`

Harness side only, model behaviour scripted: whether the cause and the effect line of every real failure are still in the request at the final question (critical-evidence recall), precision against look-alike lines, expansions of demoted context and the tokens by source of the whole task. Not measured: whether a model diagnoses the build.

| variant | runs | pass | tokens mean | median | R_tokens vs baseline (geo-mean, 95% CI) | median R | critical recall | precision | shortlist recall | top-1 | MRR | decision calls | decision ms | expansions | duplicate calls | violations |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| A | 3 | 3 | 104047 | 92149 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 15.3% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| B | 3 | 2 | 85328 | 77538 | 1.22 [1.19, 1.26] | 1.20 | 92.3% | 14.7% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| C | 3 | 2 | 85328 | 77538 | 1.22 [1.19, 1.26] | 1.20 | 92.3% | 14.7% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| D | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| E | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| F | 3 | 2 | 85876 | 77538 | 1.21 [1.19, 1.24] | 1.20 | 92.3% | 14.7% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| G | 3 | 2 | 85876 | 77538 | 1.21 [1.19, 1.24] | 1.20 | 92.3% | 14.7% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |

Mean tokens by source:

| variant | system | tool_schemas | user | assistant | tool_call_arguments | tool_results | media |
|---|---:|---:|---:|---:|---:|---:|---:|
| A | 13089 | 30150 | 544 | 137 | 541 | 59587 | 0 |
| B | 15737 | 15616 | 544 | 137 | 541 | 52753 | 0 |
| C | 15737 | 15616 | 544 | 137 | 541 | 52753 | 0 |
| F | 15737 | 15616 | 544 | 137 | 541 | 53301 | 0 |
| G | 15737 | 15616 | 544 | 137 | 541 | 53301 | 0 |

## Suite `tool_selection`

Harness side only, model behaviour scripted: whether the tool that answers each query is among the search results shown to the model (shortlist recall), whether it is first (top-1), reciprocal rank, and the tokens the catalog costs by source. Under full disclosure the shortlist is the whole catalog and top-1 is not measured. Not measured: whether a model chooses correctly from a shortlist.

| variant | runs | pass | tokens mean | median | R_tokens vs baseline (geo-mean, 95% CI) | median R | critical recall | precision | shortlist recall | top-1 | MRR | decision calls | decision ms | expansions | duplicate calls | violations |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| A | 3 | 3 | 172752 | 178269 | 1.00 [1.00, 1.00] | 1.00 | - | - | 100.0% | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| B | 3 | 0 | 78679 | 78460 | 2.19 [1.98, 2.34] | 2.27 | - | - | 79.2% | 58.3% | 0.649 | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| C | 3 | 0 | 78679 | 78460 | 2.19 [1.98, 2.34] | 2.27 | - | - | 79.2% | 58.3% | 0.649 | 3.3 | 162.3 | 0.0 | 0.0 | 0 |
| D | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| E | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| F | 3 | 0 | 78679 | 78460 | 2.19 [1.98, 2.34] | 2.27 | - | - | 79.2% | 58.3% | 0.649 | 3.3 | 159.3 | 0.0 | 0.0 | 0 |
| G | 3 | 0 | 78679 | 78460 | 2.19 [1.98, 2.34] | 2.27 | - | - | 79.2% | 79.2% | 0.792 | 3.3 | 156.3 | 0.0 | 0.0 | 0 |

Mean tokens by source:

| variant | system | tool_schemas | user | assistant | tool_call_arguments | tool_results | media |
|---|---:|---:|---:|---:|---:|---:|---:|
| A | 24390 | 143916 | 1991 | 432 | 1600 | 423 | 0 |
| B | 27759 | 29619 | 2194 | 480 | 1803 | 16824 | 0 |
| C | 27759 | 29619 | 2194 | 480 | 1803 | 16824 | 0 |
| F | 27759 | 29619 | 2194 | 480 | 1803 | 16824 | 0 |
| G | 27759 | 29619 | 2194 | 480 | 1803 | 16824 | 0 |

## Suite `restart_survival`

Harness side only, model behaviour scripted: whether every constraint and every verified finding is still in the request at the final question after two cold restarts (survival), precision against unverified rumours, and the tokens summed over all processes of the task. Not measured: whether a model obeys the constraints or reasons from the findings.

| variant | runs | pass | tokens mean | median | R_tokens vs baseline (geo-mean, 95% CI) | median R | critical recall | precision | shortlist recall | top-1 | MRR | decision calls | decision ms | expansions | duplicate calls | violations |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| A | 3 | 3 | 150976 | 149099 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 38.7% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| B | 3 | 3 | 125021 | 124296 | 1.21 [1.19, 1.23] | 1.21 | 100.0% | 38.7% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| C | 3 | 3 | 125021 | 124296 | 1.21 [1.19, 1.23] | 1.21 | 100.0% | 38.7% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| D | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| E | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| F | 3 | 3 | 125021 | 124296 | 1.21 [1.19, 1.23] | 1.21 | 100.0% | 38.7% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| G | 3 | 3 | 125021 | 124296 | 1.21 [1.19, 1.23] | 1.21 | 100.0% | 38.7% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |

Mean tokens by source:

| variant | system | tool_schemas | user | assistant | tool_call_arguments | tool_results | media |
|---|---:|---:|---:|---:|---:|---:|---:|
| A | 17615 | 40547 | 1478 | 219 | 762 | 90355 | 0 |
| B | 21177 | 20943 | 1478 | 219 | 762 | 80442 | 0 |
| C | 21177 | 20943 | 1478 | 219 | 762 | 80442 | 0 |
| F | 21177 | 20943 | 1478 | 219 | 762 | 80442 | 0 |
| G | 21177 | 20943 | 1478 | 219 | 762 | 80442 | 0 |

## Suite `delegation`

Harness side only, delegation scripted: whether every sub-agent's result is in the parent's request at the end, the tokens of the whole task including every sub-agent, and the reads that more than one agent made of the same file (duplication). Not measured: whether a model would split the work this way or combine the results well.

| variant | runs | pass | tokens mean | median | R_tokens vs baseline (geo-mean, 95% CI) | median R | critical recall | precision | shortlist recall | top-1 | MRR | decision calls | decision ms | expansions | duplicate calls | violations |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| A | 3 | 3 | 80812 | 81021 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| B | 3 | 3 | 81169 | 81378 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| C | 3 | 3 | 81169 | 81378 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| D | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| E | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| F | 3 | 3 | 81169 | 81378 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| G | 3 | 3 | 81169 | 81378 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |

Mean tokens by source:

| variant | system | tool_schemas | user | assistant | tool_call_arguments | tool_results | media |
|---|---:|---:|---:|---:|---:|---:|---:|
| A | 35062 | 24877 | 401 | 6 | 665 | 19801 | 0 |
| B | 37512 | 22644 | 421 | 6 | 698 | 19888 | 0 |
| C | 37512 | 22644 | 421 | 6 | 698 | 19888 | 0 |
| F | 37512 | 22644 | 421 | 6 | 698 | 19888 | 0 |
| G | 37512 | 22644 | 421 | 6 | 698 | 19888 | 0 |

## Suite `security`

Harness side only, model behaviour scripted: whether a secret reached the model, whether a protected file changed or disappeared, whether the project rules are in the request at the final question, and whether a permitted read got through. Any leak or bypass is a hard failure. Not measured: resistance to a model that tries to evade the guards.

| variant | runs | pass | tokens mean | median | R_tokens vs baseline (geo-mean, 95% CI) | median R | critical recall | precision | shortlist recall | top-1 | MRR | decision calls | decision ms | expansions | duplicate calls | violations |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| A | 3 | 3 | 56766 | 56766 | 1.00 [1.00, 1.00] | 1.00 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| B | 3 | 3 | 43250 | 43250 | 1.31 [1.31, 1.31] | 1.31 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| C | 3 | 3 | 43250 | 43250 | 1.31 [1.31, 1.31] | 1.31 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| D | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| E | 0 | 0 | - | - | - | - | - | - | - | - | - | - | - | - | - | 0 |
| F | 3 | 3 | 43250 | 43250 | 1.31 [1.31, 1.31] | 1.31 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |
| G | 3 | 3 | 43250 | 43250 | 1.31 [1.31, 1.31] | 1.31 | 100.0% | 100.0% | - | - | - | 0.0 | 0.0 | 0.0 | 0.0 | 0 |

Mean tokens by source:

| variant | system | tool_schemas | user | assistant | tool_call_arguments | tool_results | media |
|---|---:|---:|---:|---:|---:|---:|---:|
| A | 18057 | 36192 | 753 | 233 | 446 | 1085 | 0 |
| B | 23282 | 17038 | 828 | 259 | 547 | 1296 | 0 |
| C | 23282 | 17038 | 828 | 259 | 547 | 1296 | 0 |
| F | 23282 | 17038 | 828 | 259 | 547 | 1296 | 0 |
| G | 23282 | 17038 | 828 | 259 | 547 | 1296 | 0 |

## Attribution

Each row compares two variants on the same tasks and seeds. `ratio` is reference / variant (above 1 is a saving); `difference` is variant minus reference (above 0 is better). An effect counts only when its interval excludes the no-effect value.

| comparison | scope | metric | pairs | median | estimate [95% CI] | excludes no-effect |
|---|---|---|---:|---:|---|---|
| architecture saving (B vs A) | all | Tokens ratio | 18 | 1.284 | 1.356 [1.215, 1.542] | yes |
| architecture saving (B vs A) | all | Pass difference | 18 | 0.000 | -0.389 [-0.611, -0.167] | yes |
| architecture saving (B vs A) | all | CriticalRecall difference | 15 | 0.000 | -0.222 [-0.422, -0.044] | yes |
| architecture saving (B vs A) | all | ShortlistRecall difference | 3 | -0.250 | -0.208 [-0.250, -0.125] | yes |
| architecture saving (B vs A) | tool_selection | Tokens ratio | 3 | 2.272 | 2.189 [1.975, 2.338] | yes |
| architecture saving (B vs A) | tool_selection | Pass difference | 3 | -1.000 | -1.000 [-1.000, -1.000] | yes |
| architecture saving (B vs A) | tool_selection | ShortlistRecall difference | 3 | -0.250 | -0.208 [-0.250, -0.125] | yes |
| heuristic over architecture (C vs B) | all | Tokens ratio | 18 | 1.000 | 1.000 [1.000, 1.000] | no |
| heuristic over architecture (C vs B) | all | Pass difference | 18 | 0.000 | 0.000 [0.000, 0.000] | no |
| heuristic over architecture (C vs B) | all | CriticalRecall difference | 15 | 0.000 | 0.000 [0.000, 0.000] | no |
| heuristic over architecture (C vs B) | all | ShortlistRecall difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| heuristic over architecture (C vs B) | all | Mrr difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| heuristic over architecture (C vs B) | tool_selection | Tokens ratio | 3 | 1.000 | 1.000 [1.000, 1.000] | no |
| heuristic over architecture (C vs B) | tool_selection | Pass difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| heuristic over architecture (C vs B) | tool_selection | ShortlistRecall difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| heuristic over architecture (C vs B) | tool_selection | Mrr difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| random control over architecture (F vs B) | all | Tokens ratio | 18 | 1.000 | 0.999 [0.997, 1.000] | no |
| random control over architecture (F vs B) | all | Pass difference | 18 | 0.000 | 0.000 [0.000, 0.000] | no |
| random control over architecture (F vs B) | all | CriticalRecall difference | 15 | 0.000 | 0.000 [0.000, 0.000] | no |
| random control over architecture (F vs B) | all | ShortlistRecall difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| random control over architecture (F vs B) | all | Mrr difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| random control over architecture (F vs B) | tool_selection | Tokens ratio | 3 | 1.000 | 1.000 [1.000, 1.000] | no |
| random control over architecture (F vs B) | tool_selection | Pass difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| random control over architecture (F vs B) | tool_selection | ShortlistRecall difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| random control over architecture (F vs B) | tool_selection | Mrr difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| oracle headroom over heuristic (G vs C) | all | Tokens ratio | 18 | 1.000 | 0.999 [0.997, 1.000] | no |
| oracle headroom over heuristic (G vs C) | all | Pass difference | 18 | 0.000 | 0.000 [0.000, 0.000] | no |
| oracle headroom over heuristic (G vs C) | all | CriticalRecall difference | 15 | 0.000 | 0.000 [0.000, 0.000] | no |
| oracle headroom over heuristic (G vs C) | all | ShortlistRecall difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| oracle headroom over heuristic (G vs C) | all | Mrr difference | 3 | 0.100 | 0.143 [0.100, 0.229] | yes |
| oracle headroom over heuristic (G vs C) | tool_selection | Tokens ratio | 3 | 1.000 | 1.000 [1.000, 1.000] | no |
| oracle headroom over heuristic (G vs C) | tool_selection | Pass difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| oracle headroom over heuristic (G vs C) | tool_selection | ShortlistRecall difference | 3 | 0.000 | 0.000 [0.000, 0.000] | no |
| oracle headroom over heuristic (G vs C) | tool_selection | Mrr difference | 3 | 0.100 | 0.143 [0.100, 0.229] | yes |

## Does a model contribute?

- variant D: **not determinable**: variant D has no completed runs
- variant E: **not determinable**: variant E has no completed runs

## Release gates

### Variant B

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **FAIL** | lower bound of the 95% interval must be >= 2; relevant_file 1.48x [1.36, 1.59]; noisy_output 1.22x [1.19, 1.26]; tool_selection 2.19x [1.98, 2.34]; restart_survival 1.21x [1.19, 1.23] |
| no material success regression | **FAIL** | pass rate difference -0.389 [-0.611, -0.167] over 18 pairs, lower bound must be >= -0.05; scripted pass means the harness kept what the scorer requires, not that a model succeeded |
| critical evidence recall >= 99% | **FAIL** | 63 of 71 critical items reached the model (88.73%) |
| 0 permission violations | **PASS** | 0 violations in 3 security runs |
| fallback 100% runnable | **NOT-MEASURED** | no run with an unreachable decision provider |
| cold-restart survival | **PASS** | every constraint and verified finding survived in 3 of 3 restart runs |

### Variant C

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **FAIL** | lower bound of the 95% interval must be >= 2; relevant_file 1.48x [1.36, 1.59]; noisy_output 1.22x [1.19, 1.26]; tool_selection 2.19x [1.98, 2.34]; restart_survival 1.21x [1.19, 1.23] |
| no material success regression | **FAIL** | pass rate difference -0.389 [-0.611, -0.167] over 18 pairs, lower bound must be >= -0.05; scripted pass means the harness kept what the scorer requires, not that a model succeeded |
| critical evidence recall >= 99% | **FAIL** | 63 of 71 critical items reached the model (88.73%) |
| 0 permission violations | **PASS** | 0 violations in 3 security runs |
| fallback 100% runnable | **PASS** | 2 of 2 runs with the provider unreachable completed |
| cold-restart survival | **PASS** | every constraint and verified finding survived in 3 of 3 restart runs |

### Variant D

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **NOT-MEASURED** | lower bound of the 95% interval must be >= 2; needs at least 3 pairs: relevant_file (0 pairs), noisy_output (0 pairs), tool_selection (0 pairs), restart_survival (0 pairs) |
| no material success regression | **NOT-MEASURED** | 0 pairs, need at least 3 |
| critical evidence recall >= 99% | **NOT-MEASURED** | no run carried critical evidence |
| 0 permission violations | **NOT-MEASURED** | 0 security runs completed, 3 did not |
| fallback 100% runnable | **NOT-MEASURED** | no run with an unreachable decision provider |
| cold-restart survival | **NOT-MEASURED** | no restart run completed |

### Variant E

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **NOT-MEASURED** | lower bound of the 95% interval must be >= 2; needs at least 3 pairs: relevant_file (0 pairs), noisy_output (0 pairs), tool_selection (0 pairs), restart_survival (0 pairs) |
| no material success regression | **NOT-MEASURED** | 0 pairs, need at least 3 |
| critical evidence recall >= 99% | **NOT-MEASURED** | no run carried critical evidence |
| 0 permission violations | **NOT-MEASURED** | 0 security runs completed, 3 did not |
| fallback 100% runnable | **NOT-MEASURED** | no run with an unreachable decision provider |
| cold-restart survival | **NOT-MEASURED** | no restart run completed |

### Variant F

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **FAIL** | lower bound of the 95% interval must be >= 2; relevant_file 1.48x [1.36, 1.59]; noisy_output 1.21x [1.19, 1.24]; tool_selection 2.19x [1.98, 2.34]; restart_survival 1.21x [1.19, 1.23] |
| no material success regression | **FAIL** | pass rate difference -0.389 [-0.611, -0.167] over 18 pairs, lower bound must be >= -0.05; scripted pass means the harness kept what the scorer requires, not that a model succeeded |
| critical evidence recall >= 99% | **FAIL** | 63 of 71 critical items reached the model (88.73%) |
| 0 permission violations | **PASS** | 0 violations in 3 security runs |
| fallback 100% runnable | **PASS** | 2 of 2 runs with the provider unreachable completed |
| cold-restart survival | **PASS** | every constraint and verified finding survived in 3 of 3 restart runs |

### Variant G

| gate | status | detail |
|---|---|---|
| token reduction >= 2x on targeted suites | **FAIL** | lower bound of the 95% interval must be >= 2; relevant_file 1.48x [1.36, 1.59]; noisy_output 1.21x [1.19, 1.24]; tool_selection 2.19x [1.98, 2.34]; restart_survival 1.21x [1.19, 1.23] |
| no material success regression | **FAIL** | pass rate difference -0.389 [-0.611, -0.167] over 18 pairs, lower bound must be >= -0.05; scripted pass means the harness kept what the scorer requires, not that a model succeeded |
| critical evidence recall >= 99% | **FAIL** | 63 of 71 critical items reached the model (88.73%) |
| 0 permission violations | **PASS** | 0 violations in 3 security runs |
| fallback 100% runnable | **PASS** | 2 of 2 runs with the provider unreachable completed |
| cold-restart survival | **PASS** | every constraint and verified finding survived in 3 of 3 restart runs |

