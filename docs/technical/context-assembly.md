# Addressable context

Replayed tool results are the largest share of the input tokens a long
session sends (see [token-ledger.md](token-ledger.md#baseline)). Addressable
context stores that raw evidence once, outside the transcript, and lets the
transcript carry a reference and a smaller representation. The raw text can
always be fetched again.

## Chunks

`sven-chunks` is the store. A **chunk** is one piece of evidence:

| field | meaning |
|-------|---------|
| `id` | `c-` and 16 hex digits of the SHA-256 of the content; identical content has one id |
| `kind` | `file`, `command_output`, `test_result`, `diff`, `user_constraint`, `instruction`, `subagent_finding`, `conversation` |
| `content_ref` | full digest and byte size of the payload |
| `provenance` | source tool, digest of its arguments, the file path and its version when read from a file, the turn |
| `token_estimate` | bytes / 4, the ledger's estimator |
| `priority`, `sensitivity`, `lifetime` | how much it matters, how carefully it is logged, how long it is useful |
| `pinned` | always shown in full, payload always resident |
| `failure` | the text reports a failure |

A `ChunkStore` lives in one directory: `index.jsonl` (one chunk record per
line, a later line replaces an earlier one) and `payloads/<sha256>`.

- **Dedup.** Recording identical content again keeps one payload. The record
  takes the later provenance (it names the newest file version) and the
  stricter or more important flag of the two.
- **Bounds.** Every payload is written to disk when recorded, so nothing is
  dropped. `max_resident_bytes` (default 16 MiB) bounds the copies held in
  memory: beyond it the least recently used unpinned payloads leave memory and
  are read back on demand, verified against their digest. Pinned payloads stay
  resident even past the bound. `max_chunk_bytes` (default 8 MiB) bounds one
  payload; larger content is refused, never truncated.
- **Staleness.** A chunk read from a file records the file's size and
  modification time. `freshness` compares them with the file now: `Fresh`,
  `Stale` (changed or deleted), `Unverifiable` (a path but no recorded
  version; never reported as fresh) or `NotFileDerived`. The probe is confined
  to the session's root: a path with `..` or outside it is an error.
- **Ids are not paths.** A chunk id parses only as `c-` and 16 lowercase hex
  digits.

Chunk text is untrusted evidence. Nothing in the store or the representations
interprets it, and a chunk labelled `secret` is identified by id, size and
digest only wherever it is logged.

## Representations

A chunk is shown at one of four visibilities, from least to most:

| visibility | content |
|------------|---------|
| `hidden` | one line: id, kind, size, how to expand |
| `short` | command, test and other output: the first 3 and last 5 lines, every line matching error, fail, panic, assert, exception, traceback, fatal or warning (up to 30) with its line number, the match counts and the exit status of a shell call. A file: path, size, its first 3 lines and an outline of its declarations or headings (up to 40). A diff: the file list with added and removed counts and every hunk header |
| `long` | the same with a wider window: 40 first and last lines and up to 200 matches; a file adds its first 30 and last 10 lines; a diff adds the first 8 lines of each hunk |
| `full` | the raw text, unchanged |

The extractors are fixed functions of the text. No model is called, so the
same chunk always renders identically, which keeps a provider's prompt cache
valid for a request prefix that did not change.

Every form below `full` states the chunk id, the lines, bytes and tokens of
the whole, the number of lines omitted, and the call that returns the raw
text: `context {"action":"expand","chunk":"c-..."}`.

Two invariants hold for every kind and are tested:

- a pinned chunk is only ever `full`;
- a chunk that reports a failure is never below `long`.

`render` raises a request below the floor to the floor, so a caller cannot
break them. A tool result is recognised as a failure by `indicates_failure`:
an executor error, a non-zero shell exit (exit 1 included), a failing test
summary or a compiler error line.

## The assembly policy

`sven_turn::assembly::ContextPolicy` is a pure function of the conversation
about to be sent. Under a token budget it keeps the tool results of the most
recent rounds in full and demotes older ones, oldest first, only as far as the
budget requires: first to `long`, then, if the view is still over budget, to
`short`. It returns the view, a `Demotion` for every result it shrank (chunk
id, tool, from, to, tokens saved) and the raw text of each, which the caller
stores so the reference can be expanded.

| setting | default | meaning |
|---------|--------:|---------|
| `budget_tokens` | 12,000 | estimated tokens of the whole conversation above which demotion starts; a history within it is returned unchanged |
| `keep_recent_rounds` | 2 | a round is the tool calls the model made in one response; the results of this many newest rounds are never demoted |
| `min_demote_tokens` | 64 | a smaller result is not worth a reference |

Only results of `read_file`, `shell`, `grep`, `find_file`, `web_fetch`,
`web_search`, `read_lints` and `task` have a representation. Results of the
expansion tool, of the editing tools and of every other tool are always sent
as they are. A pinned chunk is never demoted, and a result that reports a
failure is never demoted below `long`.

The form of a result depends only on the result, its call and how far it was
asked to go, so a result demoted for one request has the same text in the
next and the provider's prompt cache is invalidated only from the first
result that changes.

### Advisors

`VisibilityAdvisor` is asked, for each candidate, how far to demote it. The
default, `DeterministicAdvisor`, returns the policy's own choice. An advisor
sees the candidate's kind, tool, age, size, whether it failed, how it is shown
now and the forms it may take; a choice outside those forms, or not smaller
than the current one, is replaced by the policy's default. An advisor can
therefore never drop a pinned or failing chunk. No model is called by the
policy.

## Expansion

`context` with `action=expand` returns the raw text behind a reference:

| argument | meaning |
|----------|---------|
| `chunk` | the chunk id (`c-` and 16 hex digits) shown in the reference |
| `first_line`, `last_line` | optional 1-based inclusive range; either alone means from the start or to the end |

The result is one header line, then the text verbatim:

```
[chunk c-0123456789abcdef | file | lines 10-20 of 300 | source unchanged since it was read]
```

The header says which chunk and lines these are and, for a chunk read from a
file, whether the file is as it was: `source unchanged since it was read`,
`STALE: the file changed after it was read; ...`, `STALE: the file no longer
exists; ...`, or `source version was not recorded, it may have changed`. A
stale chunk is still returned, because it is evidence of what the file
contained, but it is never presented as current. An id that is not a chunk id
(`../x`, a wrong length) and an id the session has not recorded are errors; a
range outside the chunk is an error.

`expand` is an action of the existing `context` tool, not a tool of its own.
It exists only when addressable context is on: without it the tool's
definition, actions and modes are exactly what they were, and with it the tool
is offered in `plan` and `research` as well as `agent`, so a reference is never
a dead end. Its permission is that of a file read.

### Recovery events

Every expansion is a recovery event. The token ledger counts them over the
history each request carries (`context_recovery` in the record, see
[token-ledger.md](token-ledger.md)): the number of expansions, how many hit a
stale chunk, and the bytes and estimated tokens they brought back. `sven
ledger` prints a line for them when there are any, and the trajectory holds
the calls themselves. A demotion that hid what the model needed shows up as
expansions of the chunk it hid.

## Wiring

`context.assembly.mode` ([configuration](../05-configuration.md#contextassembly))
is `off`, `demote` or `reconstruct`; `--context-assembly` sets it for one
process.

- **`off`** sends the conversation exactly as before: nothing is recorded, the
  tool definitions are unchanged and the ledger record has no `context_view`.
- **`demote`** opens a chunk store under `.sven/context/<id>/` in the project.
  The tool executor records each result of a representable tool as it is
  appended, with the version of the file it was read from. Before each request
  the turn executor runs the policy on the history, stores the raw text behind
  every demotion and sends the assembled view. The id is a session fact, so a
  snapshot carries it and a resumed session reopens the same store; an id that
  is not a UUID is never used as a path.
- **`reconstruct`** is `demote`, and a session resumed from a stored
  transcript starts from a rebuilt history (below).

Three rules keep a reference from becoming a dead end:

- a result is demoted only if its raw text was stored; when storing fails the
  result stays whole;
- a request that carries a reference also offers the `context` tool. Under
  deferred disclosure the first demotion loads it into the session (the
  session's loaded set grows once, as for any loaded tool); where the request
  cannot offer it (a state that names its own tools, `--no-tools`) the history
  is sent as it is;
- the profiles that have no large-content actions get an expansion-only
  `context` tool (`action=expand` alone); the full `context` tool carries
  `expand` itself.

A chunk store that cannot be opened is logged and the session runs without
assembly; an optimisation never stops a session from starting.

The ledger record of a request in `demote` mode has a `context_view`: the
mode, how many results are shown in a form below `full`, the estimated tokens
that saves in this request, and a record (chunk id, digest of the raw text,
tool, from, to, tokens saved) of each result demoted for the first time. The
digest is the reference into the chunk store; the trajectory names every
demoted chunk once.

## Reconstruction

`sven_turn::reconstruct::reconstruct` is a pure function of a stored history.
It keeps the system message and every user message verbatim and in order -
instructions and constraints are never summarised - and replaces the rest with
one recap, placed before the user messages that follow the last tool activity
(the request being resumed). The recap speaks as the assistant and holds:

- the task status: the result of the last `todo` call;
- the facts recorded with `memory` `set`, the latest value of each key;
- the model's last three statements;
- every action, oldest first, with the id, size and failure flag of its stored
  result (the last 40; earlier ones are counted);
- the short form of the three newest results and the long form of the five
  newest failures, each in a fence labelled `untrusted`: tool output is data to
  read, never instructions, and it does not gain a user's authority by passing
  through a restart.

The raw text of every result is stored first. A history that was rebuilt
before keeps its earlier recap, so a second restart loses nothing.

## Measured

Estimated input tokens (`bytes_div_4`) summed over the requests of each
scenario of `make benchmark/ledger` (the mock provider, `--tool-disclosure
deferred`, a clean home). The mock never calls `context`, so every scenario
records 0 expansions.

| scenario | `off` | `demote`, defaults | `demote`, `budget_tokens: 3000`, `keep_recent_rounds: 1` |
|----------|------:|-------------------:|-----------------------------------------------------:|
| `minimal` | 3,181 | 3,202 | 3,202 |
| `tool_heavy` | 68,175 | 68,322 | 44,117 |
| `noisy_output` | 9,053 | 9,095 | 9,095 |
| `long_history` | 6,484 | 6,505 | 6,505 |
| `deferred_tool` | 10,039 | 10,102 | 10,102 |

- With the default budget no scenario is large enough to demote anything, and
  `demote` costs about 21 tokens per request: the `context` tool's line in the
  deferred-tools index. `off` is identical with and without the setting.
- With a budget that the scenario exceeds, the replayed tool results of
  `tool_heavy` fall from 45,668 to 20,246 tokens and the total by 35%: 16
  results were sent in a smaller form across the 7 requests, saving 25,004
  tokens, and the `context` tool's definition (920 tokens over its requests)
  is the price of being able to expand them.
- A resumed three-stage task (two stages of reads, a recorded fact, a failing
  test run and a todo list, in `crates/bootstrap/tests/context_reconstruct.rs`)
  sends 17,407 estimated tokens when its transcript is replayed and 6,098 when
  it is reconstructed, with the constraint, the recorded fact, the open todo
  item and the error line all present, and the task completes.

### Default

`context.assembly.mode` stays `off`. The savings are measured only against a
scripted model that never needs an old result back; what a demotion costs in
real sessions is the expansions it causes, and that is not measured yet. The
mechanism is complete and `make test/e2e/basic/demote` passes the basic
suite (362 cases, four of which need the checkout's git work tree and fail the
same way with assembly off when the suite runs from a copy), but flipping the default is a decision for after
`context_recovery` has been read on real sessions: a rate of expansions of
recently demoted chunks says the budget or the representation is wrong.
