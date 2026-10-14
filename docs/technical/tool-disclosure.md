# Deferred tool disclosure

Every model request carries the definition of every tool the session offers:
name, description and parameter schema. The [token ledger](token-ledger.md)
measures what that costs: 4,253 estimated tokens for the fifteen tools of a
default `agent` run, 284 on average, charged again on every request of a
session whether or not the task ever touches those tools. MCP servers add
theirs on top.

Deferred disclosure sends the definitions of a few **core** tools and, for
every other tool, a one-line summary. The model asks for the definitions it
needs and gets them from its next step on. Nothing is lost: every tool is still
reachable, with the same schema, the same permission rules and the same
approval semantics as before.

## Modes

`tools.disclosure.mode` ([configuration](../05-configuration.md#toolsdisclosure))
or `--tool-disclosure` for one process:

| mode | a request carries |
|------|-------------------|
| `full` | the definition of every tool the mode offers - the listing sven always sent, unchanged |
| `deferred` | the core tools' definitions, the definitions of the tools the session has loaded, and the `tools` tool; the rest by summary |

The default is `deferred` for a program that reads the configuration file
and does not set the mode, and `full` for a `Config` built in code, so an
embedding application's own tools stay in every request unless it opts in.

Deferral takes effect only while the registry holds the `tools` tool. A
registry without it (an embedding application that built its own, a
`--no-tools` session) offers everything it has, because tools nobody can load
must not be withheld. A state of a machine that names its tools explicitly (an
SDLC phase, a harness agent step) is offered exactly those, in full: it has
already decided what it needs.

## What the model sees

**Core tools**, always in full, in name order: `edit_file`, `find_file`,
`grep`, `read_file`, `shell`, `todo`, `write_file`, and `tools`. They are the
tools that the baseline scenarios of the ledger call; anything that is not
needed by most tasks is deferred. `sven_tool_registry::CORE_TOOLS` is the list.
An MCP tool is never core.

**The index**, a `## Deferred Tools` section of the system prompt, written once
when the session starts:

```
## Deferred Tools

Your tool list carries full definitions for core tools only. Other tools exist;
load one before you use it. Call `tools` with action=load and the names: ...

Not loaded:
- ask_question: Ask the user multiple-choice questions and wait for the answers.
- attach_file: Attach an image or audio file to the conversation so the model ...
- memory: Save and recall persistent notes and project knowledge by key.
- task: Delegate a focused sub-task to a sub-agent and get its final answer.
...
```

A line is `name: summary`, the summary at most 100 characters. Above 40 unloaded
tools the section gives counts by category instead of a line each, so a session
with hundreds of MCP tools does not pay for hundreds of lines on every
request; `tools` `list` is the live view either way.

**The `tools` tool**, one definition of about 170 tokens:

| action | arguments | result |
|--------|-----------|--------|
| `search` | `query`, optional `category` | the unloaded tools that match, best first, each with its summary |
| `load` | `names` | the names that can be loaded are activated; the rest are reported, with the nearest tool for a misspelling |
| `list` | optional `offset`, `category` | every unloaded tool, 25 to a page, with the offset of the next page |
| `help` | `names` | the full definition of up to three tools, without loading them |

A search that finds nothing says how many tools are not loaded and points to
`list`, so a wrong shortlist is never a dead end. `category` is the label of the
capability a tool is gated under (`read`, `write`, `delete`, `shell`,
`network`, `git`, `verify`, `device`, `agent`) or `mcp`.

## Loading is activation, not proxying

There is no tool that calls another tool. `load` names tools; the executor
posts `Event::ToolsActivated` before the call's own `ToolSucceeded`; the kernel
adds the names to the session's context; and the next request carries their
definitions. The model then calls the tool directly, and that call is an
ordinary `Effect::CallTool` under the tool's own `kernel_capability` /
`call_capability`, classified by the permission policy of the machine's current
state, asked about under `--approval manual`, audited in the tool audit trail.
[ADR 0003](../adr/0003-agents-as-typed-objects.md) rejects designs that route
around that gate; this is not one. The `tools` tool itself is read-only
(`ToolCapability::ReadFile`), and loading grants nothing.

A call to a deferred tool that was not loaded is **not** executed. It goes
through the gate like any other call, under the capability of the tool it
names, and:

- if the policy forbids the capability, it fails with the gate's
  `permission denied`;
- otherwise the tool executor refuses it, without running the tool, with

  > tool 'web_fetch' is not loaded, so it was not run. Call `tools` with action=load and names=["web_fetch"], then call it again.

  which reaches the model as the call's result. A call in the same turn as the
  `load` that would have enabled it is refused too: the definition is in the
  list from the next step.

Under `--approval manual` the refused call is asked about first, as any
unloaded-or-not call to that tool would be; the person approves a call that
then fails. A model that only calls tools it was given definitions for never
produces one.

## Where the loaded set lives

`sven_hsm::Context::active_tools` - kernel state, not a machine's field and not
a tool's private state. The consequences follow from that:

- `Hsm::dispatch` applies `Event::ToolsActivated` to the context ahead of the
  machine, so every machine has it with no code of its own.
- A **snapshot** holds the context, so a resumed agent is offered the same
  definitions in the same order. A **replay** of the recorded events rebuilds
  it. A snapshot that predates the field resumes with nothing loaded.
- Each dispatch copies the set onto the model requests it emits
  (`TurnRequest::active_tools`), so no machine reads or writes it.
- `ToolRegistry::offer`, used by the turn executor and by the SDK when it
  records what a turn was offered, is the one function that turns the set and
  the registry into the list a request carries.

## Order and the provider's prompt cache

A provider reuses its cached prompt prefix while the tool list it saw last is
a prefix of the new one, and it renders tools ahead of the system prompt and
the conversation, so a change to the list rewrites the cache of everything
behind it. The list is therefore: the core tools in name order (a stable
prefix, with its own cache breakpoint), then the loaded tools in the order they
were loaded. The set only grows. Loading costs one rewrite of the system prompt
and history, and every request after it is a prefix hit again; it also means
`load` takes several names, so related tools are loaded in one step. Loaded
tools are flagged as the part of the list after the stable prefix, whatever
they are, which is how the provider places its second breakpoint.

## Sub-agents

A `task` sub-agent is a new session with its own context: it starts with the
core tools and nothing loaded, plus whatever its mode or definition names
explicitly. It does not inherit the parent's loaded tools - the parent's
history is not in the child's prefix either, so there is nothing to keep warm,
and a child that needs a tool loads it. It inherits the parent's contract
(budgets, deadline, permissions) as before. Because it runs as its own process,
it reads `tools.disclosure.mode` from the configuration file; the
`--tool-disclosure` flag applies to the process it was given to.

## Registry metadata

Every tool states a `summary` (`Tool::summary`, at most 100 characters); a test
fails for any registered tool whose summary is empty or longer. A tool written
before summaries existed falls back to the first sentence of its description,
and an MCP tool is summarised from its server's description the same way.

`ToolDescriptor` is what the index and `search` work with: name, summary, the
tool's `ToolCapability`, whether it is an MCP tool and, for search only, the
whole description and the descriptions of the schema's top-level parameters.
Nothing is declared beside the tool, so it cannot drift: the risk class is the
capability the kernel gates the tool under, and the category is that
capability's label.

## Search

`ToolRanker` ranks descriptors against a query; `Bm25Ranker` is the default.
It is a pure function: the same query over the same tools answers identically
in every run, whatever order the tools were registered in (ties are ordered by
name). It calls no model and never loads or runs a tool; it only orders
descriptors. `rank` is async so that a ranker backed by a model fits behind the
same trait: with `decision.features.tool_routing` a decision model breaks BM25's
ties (see [decisions](decisions.md#tool-search-re-ranking)), and in every other
case the result is BM25's, exactly. The nearest-name suggestion for a
misspelled `load` is always BM25.

What is indexed, per tool, with the weight of a term found there:

| field | weight | comes from |
|---|---:|---|
| name | 2.0 | `readFile`, `read_file` and `read-file` all give `read`, `file` |
| summary | 1.0 | the line the index shows |
| category | 1.0 | the capability label, or `mcp` |
| description | 0.4 | the whole description, not only its first sentence |
| parameter descriptions | 0.2 | the `description` of each top-level property of the tool's schema |

Terms are lowercased, split at word boundaries, stripped of plural and verb
endings (`searches`, `searching`), a doubled final consonant is reduced
(`cancelling` meets `cancel`) and common words are dropped. Scores are BM25 with
`k1` 5.0 and `b` 0.9 over the weighted term counts. Two further steps use the
catalog being searched, not a list of words:

- **Ending alignment.** A query term no tool uses is replaced by the same word
  with an `e` added or removed if the tools use that: `deleting` finds `delete`
  without merging two unrelated words that differ by an `e`.
- **Widening.** A query term that at least two tools hold is added to with the
  terms held by at least 90% of those tools (at most eight, each counting up to
  as much as a query term, in proportion to that share). A tool described as
  "also known as a package" beside `parcel` makes `package` mean `parcel` in
  that catalog and no other.

A tool asked for by its own name, whole or in quotes, goes above every other
match. The ranker builds its index from the candidates each time it is asked;
`Bm25Ranker::prepare` indexes once for many queries.

### Search quality

Search is the safety property of deferral: a tool the model cannot find is a
failed task. `crates/tool-registry/tests/search_quality.rs` measures it on
generated catalogs, with three query kinds: **exact** (the words of the tool's
name), **paraphrased** (synonyms of them) and **indirect** (the tool's domain
described without being named). The metrics are shortlist recall (the tool is
within the first 8 results, what `search` shows), top-1 and mean reciprocal
rank.

The corpus is seeded and deterministic: catalogs of 40 to 288 overlapping tools
(a tool is a domain, an object and a verb; ten verbs, twelve domains of four
objects per split), shuffled, named in four styles, and queries from six
template families per kind. It is split in two so that tuning never sees what is
reported:

- the **tuning** split is the only data a ranking parameter or a text
  normalisation may be fitted on (`tests/search_tuning.rs`, an opt-in release
  test: each parameter in turn takes the value of its grid that raises the
  tuning objective, recall@8 plus half the MRR averaged over the kinds);
- the **validation** split shares no catalog seed, no domain, object or verb,
  and no query template family with it, so its numbers say how search does on
  tools and phrasings it was not shaped by.

`cargo test -p sven-tool-registry --test search_quality` runs 6 catalogs and 180
queries per split in about a second and pins the held-out recall;
`-- --ignored` runs 24 catalogs and 480 queries per kind per split. The numbers
below are that sweep.

What the generator assumes is stated in `tests/quality/corpus.rs`: a tool's
name, summary and parameters use the plain words, while its longer description
mentions a synonym of the object, of the verb and a gloss of the domain only
some of the time (a probability drawn per catalog from 0 to 0.8). A query none
of whose words occurs in the catalog's text cannot be answered by any lexical
ranker, so the numbers are a function of that assumption and are comparable
between rankers, not predictions for a given catalog. "Evidence" below is the
subset of catalogs whose mean probability is at least 0.4.

| ranker | split | kind | recall@8 | top-1 | MRR |
|---|---|---|---:|---:|---:|
| BM25 over name, summary, category (before) | tuning | exact | 99.2% | 82.9% | 0.884 |
| | | paraphrased | 60.4% | 8.8% | 0.255 |
| | | indirect | 12.3% | 2.9% | 0.063 |
| | validation | exact | 99.8% | 84.4% | 0.895 |
| | | paraphrased | 63.5% | 7.3% | 0.239 |
| | | indirect | 11.5% | 2.7% | 0.057 |
| as described above | tuning | exact | 99.6% | 94.4% | 0.963 |
| | | paraphrased | 96.0% | 63.5% | 0.767 |
| | | indirect | 94.6% | 56.9% | 0.703 |
| | validation | exact | 100.0% | 100.0% | 1.000 |
| | | paraphrased | 95.2% | 62.7% | 0.750 |
| | | indirect | 90.2% | 54.8% | 0.682 |
| | validation, evidence | exact | 100.0% | 100.0% | 1.000 |
| | | paraphrased | 98.6% | 72.3% | 0.816 |
| | | indirect | 97.3% | 67.7% | 0.788 |

Each addition was kept or dropped on the tuning split; the validation split
shows whether it carried over. Validation recall@8, the fitted ranker with one
thing taken away (exact is at 100% throughout; its top-1 is given where it
moves):

| taken away | paraphrased | indirect | other |
|---|---:|---:|---|
| nothing | 95.2% | 90.2% | |
| the description beyond the summary | 75.6% | 50.8% | |
| widening | 86.5% | 69.0% | |
| parameter descriptions | 94.8% | 89.0% | |
| `k1` 5.0 for 0.6 | 94.0% | 88.1% | |
| ending alignment | 94.8% | 90.2% | exact top-1 85.4% for 100% |
| the doubled-consonant reduction | unchanged | unchanged | exact top-1 2.7 points lower (82.3% for 85.0%, an earlier fit) |

Tried and dropped, each added to the fitted ranker (validation paraphrased /
indirect recall, against 95.2% / 90.2%): parameter names 94.6% / 91.0%
and enumeration values 94.2% / 91.5% (no gain on either split: a parameter is
named for the object the tool is already named for); dropping a final `e` from
every term, which lifts the exact top-1 like the alignment but costs the tuning
split 0.7 and 2.1 points of paraphrased and indirect recall through words it
merges; ignoring query terms held by most tools (no change); the category
weight (no change either way, so the existing weight of 1 stays: a query that
names a category such as `network` should still find it). The tools' `modes`
are not indexed: the candidates are already limited to the session's current
mode, so every one of them carries the same modes and the field cannot tell
them apart.

The paraphrased and indirect targets of the work (99% and 95% within the first
8) are met only where the catalog's text carries evidence for the paraphrase
(98.6% and 97.3% on validation). Where a catalog says nothing that links a
synonym to a tool, no ranking that reads text can find the tool, and the
numbers above show the gap (95.2% and 90.2% over all catalogs).

## The model router

The router described in the [roadmap](../../.agents/roadmap/model-router.md)
adds capability without adding a tool per action. That is this mechanism: a
brain's actions become descriptors, deferred like any other tool, found with
`search` and loaded with `load`.

## Measurements

`make benchmark/ledger` (mock provider, empty home, default tools; estimated
tokens, `bytes_div_4`), before this change and with each mode after it:

**Per scenario** (summed over each scenario's requests). *before* is the tree
before this change; *full* and *deferred* are the same later tree, so the
difference between those two is this change alone (the later tree also carries
shorter tool descriptions that landed separately, which is why *full* is below
*before*). `deferred_tool` is new: one call to a tool outside the core set.

| scenario | requests | before: schemas | before: total | full: schemas | full: total | deferred: schemas | deferred: system (with index) | deferred: total | deferred vs full |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| minimal | 1 | 4,253 | 5,630 | 3,122 | 4,499 | 1,555 | 1,626 | 3,186 | -29.2% |
| tool_heavy | 7 | 29,771 | 85,318 | 21,854 | 77,394 | 10,885 | 11,382 | 68,203 | -11.9% |
| noisy_output | 2 | 8,506 | 13,951 | 6,244 | 11,689 | 3,110 | 3,254 | 9,063 | -22.5% |
| long_history | 1 | 4,253 | 8,933 | 3,122 | 7,802 | 1,555 | 1,627 | 6,489 | -16.8% |
| deferred_tool | 2 | - | - | 6,244 | 9,013 | 5,059 | 4,881 | 10,051 | +11.5% |

The index and the `tools` definition are what deferral adds to a request:
253 estimated tokens for the unloaded tools of a default session, 167
for `tools`. `deferred_tool` is the price of a load: the
extra request carries the whole prefix again, and after the load the tool's
definition rides along on every later request, so a session that needs one
rarely-used tool pays more than `full` for a short run (+11.5% here) and less
for a long one that does not (`tool_heavy`, which uses core tools only, is
11.9% cheaper).

**Per tool** (the `minimal` scenario, schema tokens):

| tool | before | full | deferred |
|---|---:|---:|---:|
| `task` | 591 | 458 | - |
| `grep` | 399 | 265 | 265 |
| `system` | 395 | 293 | - |
| `edit_file` | 385 | 199 | 199 |
| `semantic_memory` | 342 | 342 | - |
| `todo` | 324 | 229 | 229 |
| `find_file` | 296 | 216 | 216 |
| `shell` | 252 | 185 | 185 |
| `memory` | 247 | 197 | - |
| `read_file` | 219 | 158 | 158 |
| `attach_file` | 196 | 146 | - |
| `write_file` | 193 | 136 | 136 |
| `tools` | - | - | 167 |
| `web_fetch` | 140 | 97 | - |
| `skill` | 137 | 104 | - |
| `web_search` | 137 | 97 | - |

Ledger lines of a default `agent` session: 15 definitions in `full`; in
`deferred` 8 (the core tools and `tools`) plus a `Deferred Tools` system
section. The bats suite runs in both modes (`make tests/e2e/basic` and
`make test/e2e/basic/deferred`); the cases that fail do so identically in both.

