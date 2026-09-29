# Knowledge Base

The knowledge base is a collection of Markdown specifications stored under
`.sven/knowledge/` in a project root.  Each file documents one subsystem -
its architecture, correctness invariants, and known failure modes - written
explicitly for AI consumption.

Unlike source code comments or README files, knowledge documents are designed
to be retrieved on demand by an AI agent.  They encode the information that
would otherwise have to be re-derived from source code every session.

---

## Core concept

> If you explained something twice across sessions, it should be in the
> knowledge base.

Knowledge documents serve three roles:

1. **Coordination** - they propagate design decisions consistently across many
   independent sessions without requiring the developer to re-explain them.
2. **Captured experience** - they encode lessons from debugging sessions so
   the next agent does not repeat the same trial-and-error.
3. **On-demand context** - they are retrieved only when relevant, keeping the
   session's token budget free for actual work.

---

## File format

Each knowledge document is a plain Markdown file with YAML frontmatter:

```markdown
---
subsystem: HSM Kernel
files:
  - crates/hsm/**
  - crates/kernel/**
updated: 2026-03-01
---

## Core Architecture

The kernel drives a hierarchical state machine: every LLM turn and every tool
call is a transition, so control flow is deterministic and replayable rather
than emergent from the model's output.

## Correctness Invariants

- A transition MUST NOT perform I/O.  Machines emit `Effect`s; only the
  executor layer touches the outside world.
- Every dispatched `Effect::CallTool` MUST produce a terminal event
  (`ToolSucceeded` or `ToolFailed`), even on timeout, or the kernel hangs.

## Known Failure Modes

| Symptom | Cause | Fix |
|---------|-------|-----|
| Session hangs after a tool call | Executor dropped without emitting a terminal event | Synthesize `ToolFailed` on every error path |
| Approval never resolves | Approval id minted instead of derived | Derive the id from the tool call |
| Runtime outlives its handle | Handle dropped rather than detached | Call `detach()` explicitly |

## Critical Patterns

Always derive an approval id from the call it guards:

```rust
let approval_id = approval_id_for(&tool_call);
```
```

### Frontmatter fields

| Field       | Required | Description                                                  |
|:------------|:---------|:-------------------------------------------------------------|
| `subsystem` | Yes      | Human-readable name shown in tool output and drift warnings  |
| `files`     | No       | Glob patterns for source files this doc covers               |
| `updated`   | No       | ISO date (YYYY-MM-DD) when the doc was last reviewed         |

`files:` and `updated:` enable drift detection.  Without them, the document
is still discoverable via `list_knowledge` and `search_knowledge`, but drift
warnings are never emitted for it.

---

## Discovery

Sven scans `<project-root>/.sven/knowledge/*.md` at session start.  No
registration is needed - any valid Markdown file with `subsystem:` frontmatter
is automatically included.

Files are sorted by subsystem name for deterministic output.  Files larger
than 128 KiB are skipped with a warning.

---

## Tools

### `list_knowledge`

Enumerates all knowledge documents with their subsystem name, covered file
patterns, last-updated date, and filename:

```
Found 3 knowledge document(s):

Subsystem                      Covers                                   Updated      File
----------------------------------------------------------------------------------------------------
Agent Loop & Compaction        crates/turn/**                      2026-03-01   sven-turn.md
HSM Kernel                     crates/hsm/**, crates/kernel/**     2026-03-01   sven-hsm.md
Tool System                    crates/tool-*/**, crates/tools-*/** 2026-03-01   sven-tools.md

Use `search_knowledge "<query>"` to find relevant content across all docs.
```

Call `list_knowledge` to get an overview before `search_knowledge` when you
are unsure which subsystem covers your topic.

### `search_knowledge`

Keyword search across all knowledge document bodies.  Returns matching
excerpts with context lines, sorted by match count:

```
search_knowledge("effect")

## Knowledge Search: `effect`
Found 3 match(es) in 1 of 2 document(s):

### HSM Kernel - `sven-hsm.md` (updated 2026-03-01)  [3 match(es)]

```
   5 │ A transition MUST NOT perform I/O.  Machines emit `Effect`s;
>  6 │ only the executor layer touches the outside world.
   7 │ Every dispatched effect produces a terminal event.
```
...
```

Use `search_knowledge` before editing a subsystem.  If the search returns no
results, the subsystem may not yet have a knowledge document - consider
creating one after your changes.

### `read_file`

To load a complete knowledge document, use `read_file` with the absolute path
(which `list_knowledge` displays in the file column):

```
read_file(".sven/knowledge/sven-hsm.md")
```

---

## Drift detection

When `updated:` is set in a document's frontmatter, Sven checks at session
start whether any file matching a `files:` glob has been committed since that
date.  If drift is detected, a warning appears in the system prompt:

```
## Knowledge Drift Detected

⚠ `.sven/knowledge/sven-hsm.md` covers `HSM Kernel` - last updated 2026-01-15.
  Files committed since then: crates/kernel/src/lib.rs
  Before editing these files, call `search_knowledge "HSM Kernel"` and update the doc after changes.
```

The check uses `git log --since=<updated>` with the `files:` patterns as
pathspecs.  Uncommitted (staged/dirty) changes are not included.

**Keeping documents current:** after a session that modifies a subsystem,
update the knowledge doc's `updated:` date and revise any sections that
changed.  This takes two to five minutes per session and prevents silent
failures in future sessions.

---

## Recommended document structure

Every knowledge document should have at minimum:

```markdown
---
subsystem: <name>
files:
  - <glob>
updated: <YYYY-MM-DD>
---

## Core Architecture
(one to three paragraphs on how the subsystem works)

## Correctness Invariants
(bullet list of MUST / MUST NOT rules)

## Known Failure Modes
| Symptom | Cause | Fix |

## Critical Patterns
(code snippets for the patterns that must be followed)
```

Documents are written for the AI, not for human readers.  Explicit file
paths, function names, and concrete code snippets are more useful than
high-level descriptions.

---

## Crate responsibilities

| Crate | Responsibility |
|-------|----------------|
| `sven-workspace` | `KnowledgeInfo`, `SharedKnowledge`; `discover_knowledge()`; `check_knowledge_drift()`; `format_drift_warnings()` |
| `sven-turn` | `build_knowledge_section()` - knowledge overview in system prompt; `PromptContext.knowledge` and `.knowledge_drift_note` fields |
| `sven-tools-ctx` | `ListKnowledgeTool`, `SearchKnowledgeTool`, and the compound `MemoryTool` that exposes them as its `list_knowledge`/`search_knowledge` actions |
| `sven-bootstrap` | Calls `discover_knowledge()` and `check_knowledge_drift()` in `RuntimeContext::auto_detect()`; registers the `memory` tool in `build_tool_registry()` |

---

## Integration with skills and agents

Knowledge documents complement skills and agent specs rather than replacing
them:

- **Skills / agents** embed the core invariants and most common failure modes
  directly in their body (always pre-loaded for that domain).
- **Knowledge documents** carry the full architecture narrative and extended
  failure tables (retrieved on demand, not loaded into every session).

Use the `knowledge:` frontmatter field in an agent spec to declare which
knowledge documents it depends on:

```markdown
---
name: hsm-specialist
description: HSM kernel expert. Use when modifying sven-hsm or sven-kernel.
knowledge:
  - sven-hsm.md
---

... system prompt body ...
```

When the agent spec is loaded, a hint pointing to these documents is appended
automatically.
