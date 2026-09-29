# Semantic Memory

Sven's semantic memory stores facts, notes, decisions and any information worth
remembering across sessions. It uses SQLite with FTS5 full-text search
(BM25 ranking), making it fast and fully local - no external vector database needed.

## How It Works

```
User: "Remember that the firmware build needs arm-none-eabi-gcc 13 or newer."
Agent: semantic_memory.remember({ content: "...", entity: "firmware build", source: "user" })
       → Stored with ID 42

User: "What do we know about the firmware build?"
Agent: semantic_memory.recall({ query: "firmware build toolchain" })
       → Returns relevant memories scored by BM25 similarity
```

## Configuration

```yaml
tools:
  memory:
    backend: "sqlite"              # "json" (legacy) or "sqlite"
    db_path: "~/.config/sven/memory/memory.sqlite"  # default
```

The legacy JSON KV store is automatically migrated to SQLite on first run.

## semantic_memory tool

| Action | Description |
|--------|-------------|
| `remember` | Store a fact, note, or observation |
| `recall` | Semantic search for relevant memories |
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

- Default: `~/.config/sven/memory/memory.sqlite`
- Override: `tools.memory.db_path`

The SQLite file uses WAL mode for concurrent access and can be backed up with any standard file backup tool.
