<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# Deferred tool disclosure

Every model request carries the definition of every tool the session offers:
name, description and parameter schema. The [token ledger](token-ledger.md)
measures what that costs; it is charged again on every request of a session
whether or not the task ever touches those tools. MCP servers add theirs on top.

Deferred disclosure sends the definitions of a few **core** tools and, for
every other tool, a one-line summary. The model asks for the definitions it
needs and gets them from its next step on. Nothing is lost: every tool is still
reachable, with the same schema, the same permission rules and the same
approval semantics as before.

## Modes

`tools.disclosure.mode` ([configuration](../reference/configuration.md#toolsdisclosure))
or `--tool-disclosure` for one process:

| mode | a request carries |
|------|-------------------|
| `full` | the definition of every tool the mode offers |
| `deferred` | the core tools' definitions, the definitions of the tools the session has loaded, and the `tools` tool; the rest by summary |

The default is `deferred` for a program that reads the configuration file and
does not set the mode, and `full` for a `Config` built in code, so an embedding
application's own tools stay in every request unless it opts in.

Deferral takes effect only while the registry holds the `tools` tool. A
registry without it (an embedding application that built its own, a
`--no-tools` session) offers everything it has, because tools nobody can load
must not be withheld. A state of a machine that names its tools explicitly (an
SDLC phase, a harness agent step) is offered exactly those, in full: it has
already decided what it needs.

## What the model sees

**Core tools**, always in full, in name order: `edit_file`, `find_file`,
`grep`, `read_file`, `shell`, `todo`, `write_file`, and `tools`.
`sven_tool_registry::CORE_TOOLS` is the list. An MCP tool is never core,
whatever its name.

**The index**, a `## Deferred Tools` section of the system prompt, written once
when the session starts, for the session's starting mode:

```
## Deferred Tools

These tools exist but are not in your tool list. Load a tool before you call it:
call `tools` with action=load and names; from your next step it is in your tool
list and you call it directly. action=search (query) finds tools, action=list
pages through all of them, action=help shows a definition without loading it.

Not loaded:
- ask_question: Ask the user multiple-choice questions and wait for the answers.
- memory: Save and recall persistent notes and project knowledge by key.
- task: Delegate a focused sub-task to a sub-agent and get its final answer.
...
```

A line is `name: summary`, the summary at most 100 characters. Above 40 unloaded
tools the section gives counts by category instead (`read 3; network 2; ...`),
so a session with hundreds of MCP tools does not pay for hundreds of lines on
every request; `tools` `list` is the live view either way.

**The `tools` tool.**

| action | arguments | result |
|--------|-----------|--------|
| `search` | `query`, optional `category` | the 8 best unloaded matches, each with its summary and category, then the names (only) of the next 8 |
| `load` | `names` (at most 16) | the names that can be loaded are activated; the rest are reported, with the nearest tool for a name that is not one |
| `list` | optional `offset`, `category` | every unloaded tool, 25 to a page, with the offset of the next page |
| `help` | `names` | the full definition of the first three, without loading them |

`search`, `list` and `load` consider the tools of the session's current mode that
are not core. A search that finds nothing says how many tools are not loaded and
points to `list`, so a wrong shortlist is never a dead end. `category` is the
label of the capability a tool is gated under (`read`, `write`, `delete`,
`shell`, `network`, `git`, `verify`, `device`, `agent`) or `mcp`.

## Loading is activation, not proxying

There is no tool that calls another tool. `load` names tools; the executor
posts `Event::ToolsActivated` before the call's own `ToolSucceeded`; the kernel
adds the names to the session's context; and the next request carries their
definitions. The model then calls the tool directly, and that call is an
ordinary `Effect::CallTool` under the tool's own `kernel_capability` /
`call_capability`, classified by the permission policy of the machine's current
state, asked about under `--approval manual`, and audited in the tool audit
trail. [ADR 0003](../adr/0003-agents-as-typed-objects.md) rejects designs that
route around that gate; this is not one. The `tools` tool itself is read-only
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

Under `--approval manual` the refused call is asked about first, as any call to
that tool would be; the person approves a call that then fails. A model that
only calls tools it was given definitions for never produces one.

## Where the loaded set lives

`sven_hsm::Context::active_tools` is kernel state, not a machine's field and not
a tool's private state:

- `Hsm::dispatch` applies `Event::ToolsActivated` to the context ahead of the
  machine, so every machine has it with no code of its own.
- A **snapshot** holds the context, so a resumed agent is offered the same
  definitions in the same order. A **replay** of the recorded events rebuilds
  it.
- Each dispatch copies the set onto the model requests it emits
  (`TurnRequest::active_tools`), so no machine reads or writes it.
- `ToolRegistry::offer`, used by the turn executor and by the SDK when it
  records what a turn was offered, is the one function that turns the set and
  the registry into the list a request carries. A request that names tools
  (`TurnRequest::tools`) gets exactly those; otherwise the mode's tools are
  offered, and under deferral only the core and loaded ones.

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

A `task` sub-agent is a new process with its own session: it starts with the
core tools and nothing loaded. It does not inherit the parent's loaded tools -
the parent's history is not in the child's prefix either, so there is nothing
to keep warm, and a child that needs a tool loads it. It reads
`tools.disclosure.mode` from the configuration file; the `--tool-disclosure`
flag applies to the process it was given to.

## Registry metadata

Every tool states a `summary` (`Tool::summary`, at most 100 characters); a test
fails for any registered tool whose summary is empty, longer, or spans lines. A
tool without one falls back to the first sentence of its description, and an
MCP tool is summarised from its server's description the same way.

`ToolDescriptor` is what the index and `search` work with: name, summary, the
tool's `ToolCapability`, whether it is an MCP tool and, for search only, the
description (first 2,000 characters) and the descriptions of the schema's
top-level parameters. Nothing is declared beside the tool, so it cannot drift:
the risk class is the capability the kernel gates the tool under, and the
category is that capability's label.

## Search

`ToolRanker` ranks descriptors against a query; `Bm25Ranker` is the default. It
is a pure function: the same query over the same tools answers identically in
every run, whatever order the tools were registered in (ties are ordered by
name). It calls no model and never loads or runs a tool; it only orders
descriptors. `rank` is async so that a ranker backed by a model fits behind the
same trait: with `decision.features.tool_routing` a decision model breaks BM25's
ties (see [decisions](decisions.md#tool-search-re-ranking)), and in every other
case the result is BM25's, exactly. The nearest-name suggestion for a
misspelled `load` is always BM25.

What is indexed, per tool, with the weight of a term found there
(`FieldWeights`):

| field | weight | comes from |
|---|---:|---|
| name | 2.0 | `readFile`, `read_file` and `read-file` all give `read`, `file` |
| summary | 1.0 | the line the index shows |
| category | 1.0 | the capability label, or `mcp` |
| description | 0.4 | the description |
| parameter descriptions | 0.2 | the `description` of each top-level property of the tool's schema |

Terms are lowercased, split at word boundaries and at lower-to-upper case
changes, stripped of plural and verb endings (`searches`, `searching`), a doubled
final consonant is reduced (`cancelling` meets `cancel`) and common words are
dropped. Scores are BM25 with `k1` 5.0 and `b` 0.9 over the weighted term
counts. Two further steps use the catalog being searched, not a list of words:

- **Ending alignment.** A query term no tool uses is replaced by the same word
  with an `e` added or removed if the tools use that: `deleting` finds `delete`
  without merging two unrelated words that differ by an `e`.
- **Widening.** A query term held by at least two tools, and by at most half of
  all of them, is added to with the terms held by at least 90% of the tools that
  hold it (at most eight, each counting up to as much as a query term, in
  proportion to that share). A tool described as "also known as a package"
  beside `parcel` makes `package` mean `parcel` in that catalog and no other.

A tool asked for by its own name, whole or in quotes or backticks, goes above
every other match. A name that is nearly the query, spelled with a letter wrong,
gets a bonus in proportion to the Dice coefficient of the character trigrams of
the two (at least 0.6, four times the best BM25 score at 1.0), so a misspelled
name finds its tool first. The ranker builds its index from the candidates each
time it is asked; `Bm25Ranker::prepare` indexes once for many queries.

### What a search shows

`search` returns the 8 best matches with their summaries, `Load the ones you
need with action=load and their names.`, and after it one line, `Also related
(names only): a, b, ...`, with the next 8 names, so that a near miss is one
`load` away and not another search. A name that is not a tool gets the nearest
tool's name from the same ranker.

### Search quality

Search is the safety property of deferral: a tool the model cannot find is a
failed task. `crates/tool-registry/tests/search_quality.rs` measures it on
generated catalogs of 40 to 288 overlapping tools, with four query kinds:
**exact** (the words of the tool's name), **paraphrased** (synonyms of them),
**indirect** (the tool's domain described without being named) and
**misspelled** (the tool's name with a letter wrong). The metrics are shortlist
recall (the tool is within the first 8 results, what `search` shows), top-1 and
mean reciprocal rank.

The corpus is seeded and deterministic and split in two so that tuning never
sees what is reported: the **tuning** split is the only data a ranking
parameter or text normalisation may be fitted on (`tests/search_tuning.rs`, an
opt-in release test); the **validation** split shares no catalog seed, no
domain, object or verb, and no query template family with it, so its numbers say
how search does on tools and phrasings it was not shaped by. What the generator
assumes is stated in `tests/quality/corpus.rs`; a query none of whose words
occurs in the catalog's text cannot be answered by any lexical ranker, so the
numbers compare rankers and do not predict a given catalog.

```
cargo test -p sven-tool-registry --test search_quality                       # pins the held-out recall floor
cargo test -p sven-tool-registry --test search_quality -- --ignored --nocapture   # larger sweep, result sizes, ambiguity
cargo test --release -p sven-tool-registry --test search_tuning -- --ignored --nocapture
```

The `tool_selection` suite of the
[ablation benchmark](../contributing/ablation-benchmarks.md) runs the same
searches through the scripted model against a catalog of overlapping MCP tools,
and reads what `search` shows.

## Measurements

`make benchmark/ledger DISCLOSURE=full|deferred` replays fixed scenarios against
the mock provider and writes the token ledger of each: in `deferred` the
`schema:` lines of the non-core tools disappear, the `tools` tool appears and
the system prompt gains the `Deferred Tools` section. A load costs one extra
request that carries the whole prefix again, and the loaded definition rides
along on every later request, so a session that needs one rarely-used tool pays
more than `full` for a short run, and a long one that uses core tools only pays
less. The end-to-end suite runs in both modes (`make test/e2e/basic` and
`make test/e2e/basic/deferred`).
