# Semantic Memory

Sven's semantic memory stores facts, notes, contact details, and any information
worth remembering across sessions. It uses SQLite with FTS5 full-text search
(BM25 ranking), making it fast and fully local — no external vector database needed.

## How It Works

```
User: "Remember that Alice from Acme Corp prefers afternoon calls and dislikes email."
Agent: semantic_memory.remember({ content: "...", entity: "Alice", source: "user" })
       → Stored with ID 42

User: "What do I know about Alice?"
Agent: semantic_memory.recall({ query: "Alice Acme preferences" })
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

## Second Brain Pattern

The simplest way to build a personal knowledge base:

1. **Ingest via messaging**: Configure a Telegram channel. Text anything to remember — the agent saves it with `remember`.
2. **Retrieve on demand**: Ask the agent to recall information; it runs a semantic search.
3. **Automatic extraction**: During email/calendar triage, the agent extracts and saves contact details, action items, and preferences.

### Example HEARTBEAT.md

```markdown
# Heartbeat Instructions

After each email session:
- Extract any new contact details mentioned and save with semantic_memory remember.
- Save any commitments or action items mentioned in emails.
- Update existing contact notes if preferences or details changed.
```

## CRM Integration

The memory store is ideal for a lightweight CRM:

```
semantic_memory.remember({
  content: "Meeting with Bob Smith (bob@techcorp.com) on 2026-04-15. Discussed Q2 partnership. Bob wants a proposal by April 30. Follow up needed.",
  entity: "Bob Smith",
  source: "calendar",
  tags: ["contact", "crm", "action-item"]
})
```

Then before a call:

```
semantic_memory.recall({ query: "Bob Smith TechCorp relationship history" })
```

## Storage Location

- Default: `~/.config/sven/memory/memory.sqlite`
- Override: `tools.memory.db_path`

The SQLite file uses WAL mode for concurrent access and can be backed up with any standard file backup tool.
