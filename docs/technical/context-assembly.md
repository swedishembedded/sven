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
| `short` | command, test and other output: the first 3 and last 5 lines, every line matching error, fail, panic, assert, exception, traceback, fatal or warning (up to 30) with its line number, the match counts and the exit status of a shell call. A file: path, size and an outline of its declarations or headings (up to 40). A diff: the file list with added and removed counts and every hunk header |
| `long` | the same with a wider window: 40 first and last lines and up to 200 matches; a file adds its first 30 and last 10 lines; a diff adds the first 8 lines of each hunk |
| `full` | the raw text, unchanged |

The extractors are fixed functions of the text. No model is called, so the
same chunk always renders identically, which keeps a provider's prompt cache
valid for a request prefix that did not change.

Every form below `full` states the chunk id, the lines, bytes and tokens of
the whole, the number of lines omitted, and the call that returns the raw
text: `context_expand {"chunk":"c-..."}`.

Two invariants hold for every kind and are tested:

- a pinned chunk is only ever `full`;
- a chunk that reports a failure is never below `long`.

`render` raises a request below the floor to the floor, so a caller cannot
break them. A tool result is recognised as a failure by `indicates_failure`:
an executor error, a non-zero shell exit (exit 1 included), a failing test
summary or a compiler error line.
