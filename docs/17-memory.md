# Semantic Memory

Sven's semantic memory stores facts, notes, decisions and any information worth
remembering across sessions. It uses SQLite with FTS5 full-text search
(BM25 ranking), making it fast and fully local - no external vector database
needed. Search is lexical: the store holds an optional embedding per record,
but nothing computes or ranks by one yet.

## How It Works

```
User: "Remember that the firmware build needs arm-none-eabi-gcc 13 or newer."
Agent: semantic_memory.remember({ content: "...", entity: "firmware build", source: "user" })
       → Stored with ID 42

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
| `list` | List all stored memories (with optional tag filter) |
| `get` | Retrieve a specific memory by ID |

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
{ "action": "forget", "id": 42 }
```

### list

```json
{ "action": "list", "tag_filter": "contact" }
```

## Storage Location

`~/.config/sven/memory/memory.sqlite`. It is not configurable; no
`tools.memory` key selects a backend or a path.

The SQLite file uses WAL mode for concurrent access and can be backed up with any standard file backup tool.

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

The `semantic_memory` tool still runs on the older `sven-memory` store.
