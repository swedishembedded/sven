# Ablation benchmarks

The ablation suite exists to **disprove** the design "decision-driven,
context-efficient harness". It runs the same seeded tasks under controlled
variants of the harness, scores them objectively and reports paired
statistics. A deterministic heuristic is allowed to win; the suite decides.

`crates/ablation` (`sven-ablation`) is development tooling: nothing in the
product depends on it, and nothing in the product exists only for it. Every
variant is realized through configuration the product already reads and,
for decisions, a provider served to it.

## Variants

`benchmarks/ablation/matrix.yaml` is the matrix, a typed file
(`schema: sven.ablation.matrix/1`) refused offline when it is invalid: an
unknown field, a duplicate id, a missing baseline, a role used twice, or a
role whose `decisions` contradict it.

| id | role | tool disclosure | context assembly | decisions |
|----|------|-----------------|------------------|-----------|
| A | `baseline` | `full` | `off` | off |
| B | `architecture` | `deferred` | `on` | off |
| C | `heuristic` | `deferred` | `on` | rule-based provider of the suite |
| D | `stock_model` | `deferred` | `on` | a Brain-served stock model |
| E | `trained_model` | `deferred` | `on` | a Brain-served trained model, by id |
| F | `negative_control` | `deferred` | `on` | valid random decisions, seeded |
| G | `oracle` | `deferred` | `on` | the labelled-correct answers |

A variant becomes a configuration file (`decision`, `context.assembly.mode`)
and a `--tool-disclosure` flag. `context_assembly` is written verbatim to
`context.assembly.mode`; a sven build that has no such key ignores it, and the
run record says so rather than pretending the knob was applied.

**Injected providers.** C, F and G are answered by providers in
`sven_ablation::providers`: `HeuristicProvider` (words shared with the option,
weighted by how distinctive they are among the offered options, a name word
counting double), `RandomProvider` (a seeded distribution that depends on the
seed, the state and each option's id and never on option order, so asking in
both orders cannot sample twice) and `OracleProvider` (the labelled option at
0.97, an even answer where it holds no label). All three return full valid
distributions and answer questions other than a choice evenly, which a policy
abstains from.

**Brain variants.** D and E need an explicit endpoint: `--brain-url`, with
`--stock-model` (default `auto`) and `--trained-model` (required for E). A
variant whose flag is missing, or whose server does not answer, is recorded as
`not available` with the reason; it is never silently dropped and never
reported as a result.
