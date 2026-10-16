# Semantic Memory

Sven's semantic memory stores facts, notes, decisions and any information worth
remembering across sessions. It uses SQLite with FTS5 full-text search
(BM25 ranking), making it fast and fully local - no external vector database
needed. Search is lexical: it matches words, not meaning. The store can hold an
embedding per index generation, but nothing computes or ranks by one yet.

## How It Works

```
User: "Remember that the firmware build needs arm-none-eabi-gcc 13 or newer."
Agent: semantic_memory.remember({ content: "...", entity: "firmware build", source: "user" })
       → Stored with ID n-3f9a1c0e5b7d2a46

User: "What do we know about the firmware build?"
Agent: semantic_memory.recall({ query: "firmware build toolchain" })
       → Returns memories whose words match, ranked by BM25
```

## semantic_memory tool

| Action | Description |
|--------|-------------|
| `remember` | Store a fact, note, or observation |
| `recall` | Full-text search for matching memories |
| `forget` | Delete a specific memory by ID |
| `list` | List stored memories, newest first (with optional tag filter) |
| `get` | Retrieve a specific memory by ID |

IDs are opaque text. The tool hands one out when it remembers and expects it
back unchanged in `forget` and `get`.

### remember

```json
{
  "action": "remember",
  "content": "Alice Johnson, VP Sales at Acme Corp. Prefers afternoon calls. Direct: +1-206-555-1234.",
  "entity": "Alice Johnson",
  "source": "email",
  "tags": ["contact", "acme-corp", "sales"]
}
```

### recall

```json
{
  "action": "recall",
  "query": "Alice Acme Corp phone number",
  "limit": 5
}
```

### forget

```json
{ "action": "forget", "id": "n-3f9a1c0e5b7d2a46" }
```

### list

```json
{ "action": "list", "tag_filter": "contact" }
```

`tag_filter` matches a tag, or the `entity` a memory was stored with, as a
whole string; `%`, `_` and `*` mean nothing special in it or in a query.

### What the model is told about the text it gets back

A memory is written by the agent, and the agent may have written it after
reading a hostile page. Recall and `list` therefore show it as quoted material
(`UNTRUSTED agent_written content, quoted verbatim ...`), never as a statement
the agent is making. Only text that a person stated, put into the store by a
host, comes back unquoted.

### Reporting

A recall is reported to the run's trajectory: the step that made the call
carries `extra.sven.memory` with the query, the space searched, the index
generation (`lexical` while no embeddings are used) and every returned unit
with its revision, anchor, score and rank. See
[SDK](technical/sdk.md#trajectories).

## Storage Location

`~/.config/sven/memory/memory.sqlite`. It is not configurable; no
`tools.memory` key selects a backend or a path. The location is an input of the
session, not something it looks up: runtime detection
(`RuntimeContext::auto_detect*`, which every CLI, TUI and ACP session goes
through) supplies the user's home as `memory_home`, and a session built without
one - `RuntimeContext::empty()`, or any session of the embedded SDK - opens no
store and is not offered the tool, rather than writing into the current
directory. A test gives the session a temporary directory as `memory_home` and
never touches the real home. An SDK host that wants memory adds
`SemanticMemoryTool` with `Engine::builder().tool(..)`.

The SQLite file uses WAL mode for concurrent access and can be backed up with any standard file backup tool.
The tool reads and writes one space of it, `personal`.

### Notes from the previous store

Versions before the memory contract kept notes in a flat `memories` table of
the same file. The first start of this version moves them into the `personal`
space in one transaction and then drops the table, so an interrupted start
leaves the old rows where they were and a repeated start moves nothing. Notes
an agent wrote come across as untrusted material, as above. A note that was
confined to one session goes to a space of its own that the tool does not
read. Embeddings were never computed and are not carried over. A row the
contract cannot hold (more tags than a source may carry) stays in the old
table and is logged. A sven that predates the move recreates an empty
`memories` table and writes there; the next start of this version moves
those notes too.

## The memory contract

Two crates define what a memory store is, apart from the tool above:

- `sven-memory-api` is the contract: scoped sources and units under opaque string
  ids, an `AccessContext` that the host builds and memory never widens, an
  append-only annotation log with expected-revision checks, embedding
  specs and index generations, and the `MemoryStore` trait. It has no storage.
  Its `conformance` feature carries a suite that every implementation must pass;
  a host that stores memory in its own database runs the same suite with
  `memory_conformance_tests!`.
- `sven-memory-sqlite` is the SQLite implementation. The caller's spaces are
  conditions of the query, so a result limit counts only what the caller may
  see; stored text is matched as words, never as a pattern; a damaged row fails
  the query instead of disappearing from it; the schema is versioned in
  `user_version` and migrated on open; and without a home directory it refuses
  to open rather than writing into the current directory.

The `semantic_memory` tool (crate `sven-memory`) runs on `MemoryStore`: it
holds an `AccessContext` and one space, and knows nothing of SQLite. The CLI
opens `sven-memory-sqlite` for it.
