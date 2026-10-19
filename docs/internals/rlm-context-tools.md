<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# RLM Context Tools - Technical Reference

This document describes the internal architecture of sven's memory-mapped
context tools, which implement the inference paradigm introduced in
[_Recursive Language Models_](https://arxiv.org/abs/2512.24601) (RLM, 2025).
The user-facing description is [Working with large content](../guides/large-content.md).

---

## Motivation

A language model's context window is finite. When an agent needs to reason
over a large file, a build log, or an entire directory tree, reading the
content consumes most or all of the available token budget, leaving little
room for tool calls, reasoning and the model's own response. The RLM paper's
core insight is that **large content should live outside the context window**;
the model instead receives a symbolic handle and interacts with the content
through structured read, search, and recursive sub-query operations.

sven implements this pattern with memory-mapped files and a single compound
tool, `context`, whose actions are `open`, `read`, `grep`, `query` and `reduce`
(plus `expand`, see [Expanding demoted results](#expanding-demoted-results)).
The tool's description encodes the RLM workflow so that the model uses it
correctly without external orchestration.

---

## Architecture overview

```
┌─────────────────────────────────────────────────────────────────────┐
│  sven-tools-ctx crate                                                │
│                                                                      │
│  ContextStore (Arc<Mutex<ContextStore>>, one per session)            │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │ HashMap<String, ContextHandle>                                │   │
│  │  ContextHandle.kind                                           │   │
│  │    SingleFile  { path, Mmap, line_index: Vec<usize> }        │   │
│  │    Directory   { root, Vec<FileEntry { path, Mmap, ... }> }  │   │
│  │    Results     { data: Vec<u8>, line_index, entry_count }    │   │
│  └──────────────────────────────────────────────────────────────┘   │
│                                                                      │
│  ContextOpenTool  ──► ContextStore::open_file / open_directory       │
│  ContextReadTool  ──► ContextStore::read_range                       │
│  ContextGrepTool  ──► ContextStore::grep                             │
│                                                                      │
│  SubQueryRunner trait                                                │
└─────────────────────────────────────────────────────────────────────┘
        │ Arc<dyn SubQueryRunner>
        ▼
┌─────────────────────────────────────────────────────────────────────┐
│  sven-bootstrap crate                                                │
│                                                                      │
│  ModelSubQueryRunner (impl SubQueryRunner)                           │
│    └─► sven_turn::complete_text over the session's ModelProvider     │
│                                                                      │
│  ContextQueryTool  ──► chunk → parallel sub-queries → Results handle │
│  ContextReduceTool ──► read_all → tree-reduce → plain text           │
│  ContextTool       ──► the one registered `context` tool, which      │
│                        dispatches `action` to the five above         │
└─────────────────────────────────────────────────────────────────────┘
```

**Dependency rule**: `sven-tools-ctx` never imports `sven-model`. The
`SubQueryRunner` trait is the boundary that keeps the crates decoupled. The
concrete `ModelSubQueryRunner` lives in `sven-bootstrap` alongside `TaskTool`,
which follows the same pattern.

The five operation tools (`context_open`, `context_read`, `context_grep`,
`context_query`, `context_reduce`) are not registered individually. The model
sees one `context` definition; each action delegates to the operation tool of
that name, so their argument handling and messages are the ones described
below.

---

## ContextStore

**Source**: `crates/tools-ctx/src/context/store.rs`

One instance is created per agent session when the full tool set is built and
wrapped in `Arc<Mutex<ContextStore>>`. All actions share the same pointer.

### Handle kinds

| Kind | When created | Backing storage | Handle id |
|------|-------------|-----------------|-----------|
| `SingleFile` | `open` on a regular file | One `Mmap` + `Vec<usize>` line index | `ctx_0001`, `ctx_0002`, ... |
| `Directory` | `open` on a directory | `Vec<FileEntry>`, each with its own `Mmap` + line index | `ctx_NNNN` |
| `Results` | `query` collects sub-query results | `Vec<u8>` in memory + line index | `res_NNNN` |

Ids are a per-store counter in four lowercase hex digits, prefixed `ctx_` for
opened content and `res_` for query results.

### Line index

For every mapped file sven builds a `Vec<usize>` of byte offsets, one entry per
line start. Line 0 always starts at byte 0; subsequent entries are recorded
immediately after each `\n` (a trailing newline does not start an empty line).

```
content: "alpha\nbeta\ngamma\n"
         0     6     11
index:   [0,   6,    11]
```

Reading line range `[start, end]` (1-indexed, inclusive) becomes a slice per
line between consecutive index entries; nothing outside the range is copied.

### Directory traversal

`open_directory` walks the tree with `fs::read_dir`, skipping `.git`, `target`,
`node_modules`, `.cache`, `__pycache__`, `.venv` and `venv`. Only regular files
are mapped: symbolic links are not followed, to files or to directories. Empty
files are skipped. A file is treated as binary and skipped, and counted in the
summary, when its first 4096 bytes contain a null byte or more than 30% bytes
below 0x20 other than tab, line feed, carriage return and the other whitespace
controls. Files are sorted by path.

An optional `include_pattern` is matched against the **file name** with a small
wildcard matcher (`*` and `?`), and `recursive` (default `true`) controls
whether subdirectories are walked.

---

## Actions

All actions are classified under the kernel capability of their kind: `open`,
`read`, `grep` and `expand` are reads; `query` and `reduce` send content to the
model provider and are network calls. The `context` tool is offered in `agent`
mode (with expansion also in `plan` and `research`).

### open

**Source**: `crates/tools-ctx/src/context/open.rs`

Accepts a `path` (file or directory), an optional `include_pattern` and an
optional `recursive` flag (default `true`). The two optional arguments are
accepted by the implementation but are not part of the `context` tool's
published schema. The path resolves through the session's path scope, so a
confined session cannot open a path outside its root.

Returns only metadata to the model:

```
Context opened: handle=ctx_0001
Files: 1, Lines: 4823, Bytes: 198412

Single file: <project-root>/src/main.c
Size: 198412 bytes
Lines: 4823

Use context_grep(handle="ctx_0001", ...) to locate relevant sections.
Use context_read(handle="ctx_0001", start_line=N, end_line=M) to inspect ranges.
Use context_query(handle="ctx_0001", prompt="...") for semantic analysis.
```

A directory handle's summary lists the file count (and how many were skipped as
binary), the total size, a per-extension count and up to ten directories.

### read

**Source**: `crates/tools-ctx/src/context/read.rs`

Accepts a handle id, `start_line`, `end_line` (1-indexed, inclusive) and an
optional `file` path substring for directory handles. For a `Directory` handle
without `file`, the files are treated as concatenated and lines are numbered
globally; ranges are joined with `--- /path/to/file ---` headers. With `file`,
the first file whose path contains the substring is read with its own line
numbers.

Output lines are formatted as `L{n}:content`, so a following `edit_file` can
reference them. The output category is `FileContent`.

### grep

**Source**: `crates/tools-ctx/src/context/grep.rs`

Accepts a handle id, a Rust-regex `pattern`, an optional `file` hint (directory
handles), `context_lines` (default 2) and `limit` (default 50). Each line is
tested as lossily decoded UTF-8; matches carry their source path (`<results>`
for a results handle) and the surrounding lines. Directory files are searched in
sorted path order until the limit is reached. The output category is
`MatchList`.

### SubQueryRunner trait

**Source**: `crates/tools-ctx/src/context/query_runner.rs`

```rust
#[async_trait]
pub trait SubQueryRunner: Send + Sync {
    async fn query(&self, system: &str, prompt: &str) -> Result<String, String>;
}
```

A two-message stateless LLM call. The model receives no conversation history,
no tools, and no session context, only the system message and the user message.
This is the `llm_query()` function from the RLM paper.

`ModelSubQueryRunner` (`crates/bootstrap/src/context_query.rs`) wraps the
session's `ModelProvider`. Each call:

1. Truncates a prompt longer than `sub_query_max_chars` (default 120,000
   characters) and appends a note of how many bytes were omitted.
2. Sends a streaming completion with the two messages and no tools, through
   `sven_turn::complete_text`, which applies the agent's stall limits (first
   byte, first token, stream idle; `agent.turn_limits()`).
3. Bounds the whole call by `sub_query_timeout_secs` (default 120; 0 disables
   the total timeout but not the stall limits). A timed-out or failed
   sub-query returns an error string.

### query

**Source**: `crates/bootstrap/src/context_query.rs`

The **map** step. Accepts a handle, a `prompt` (supporting `{chunk}`,
`{chunk_index}` and `{total_chunks}` placeholders), optional explicit `ranges`,
`chunk_lines` and `max_parallel`.

The chunk's text reaches the sub-agent only through the `{chunk}` placeholder in
`prompt`; the runner prefixes the message with `[Chunk N/M: label]` but appends
nothing else, so a prompt without `{chunk}` sends no content.

```
1. Lock the store; fail if the handle is unknown.
   a. If ranges were given: each range is one chunk (read_range).
   b. Otherwise: split the handle into chunk_lines-line chunks
      (default tools.context.default_chunk_lines).

2. Release the store lock.

3. For each batch of max_parallel chunks (default tools.context.max_parallel):
   a. Spawn one task per chunk: ModelSubQueryRunner::query(SYSTEM, prompt).
   b. Await the batch; a failed sub-query becomes the text
      "[chunk N sub-query failed: <error>]" instead of failing the call.

4. Sort results by chunk index.

5. Format as "=== chunk N of M ===\n<text>\n" entries and register them in
   memory as a Results handle (res_NNNN).

6. Return the handle id, byte count, entry count, the first line of chunk 1's
   result, and usage instructions.
```

Batches run one after another, so a batch is as slow as its slowest sub-query.
The sub-query system prompt is fixed:

```
You are a focused analysis sub-agent. Answer the question or perform the
analysis described in the user prompt using only the provided content. Be
concise and structured. Do not ask for clarification.
```

Progress is reported through the tool-event channel as `context_query: chunk
N/M`.

### reduce

**Source**: `crates/bootstrap/src/context_query.rs`

The **reduce** step. Accepts a handle and a synthesis `prompt`. It reads the
whole handle (`read_all`) and runs `tree_reduce` with the reduce system prompt
(a synthesis sub-agent that aggregates, deduplicates and summarizes).

```
tree_reduce(content, prompt, depth=0):
  if depth >= 4 (MAX_REDUCE_DEPTH):
    truncate content to sub_query_max_chars with a notice; send content + prompt.
  if len(content) <= sub_query_max_chars:
    send content + "\n\n" + prompt as one call.
  else:
    split content into default_chunk_lines-line chunks
    for each chunk (sequentially):
      intermediate = query(chunk + intermediate-step prompt)
    combined = join intermediates with "---" separators
    return tree_reduce(combined, prompt, depth+1)
```

The intermediate prompt asks the sub-agent to summarize and extract key
findings, preserving details, line numbers and specific values, as a step
toward the user's prompt. The maximum depth is 4 levels.

---

## Expanding demoted results

With addressable context on (`context.assembly.mode` other than `off`, see
[Context assembly](context-assembly.md)), older tool results can be sent in a
shorter form with a chunk id (`c-` and 16 hex digits). The `context` tool then
carries a sixth action, `expand`, which returns the raw text of a chunk,
optionally limited by `first_line` and `last_line`, and says whether the file the
chunk came from has changed since (a stale chunk is what the file contained
then). Expansion is also available in `plan` and `research`.

Where the full `context` tool is not registered (the coding, research and
sub-agent tool sets), a session with addressable context, or briefed with a
delegating session's evidence, gets a `context` tool that offers `expand`
alone (`ChunkExpandTool`).

---

## Registration

**Source**: `crates/bootstrap/src/registry.rs`

The `context` tool belongs to the **full** tool set only, which a session gets
when the project root holds GDB configuration (see
[Tools and approvals](../reference/tools.md)). The coding, research and sub-agent
tool sets do not have it: sub-agents spawned by `TaskTool` get neither the
parent's store nor the large-content actions.

```rust
let context_store = Arc::new(Mutex::new(ContextStore::new()));
let context = ContextTool::new(context_store, model, &cfg.tools.context, limits, Some(tool_event_tx))
    .with_scope(path_scope);
reg.register(context);   // with_expansion(expander) when addressable context is on
```

Each session thus has its own isolated store. Handles from one session are not
accessible to another.

Configuration is the `tools.context` section (`max_parallel` 4,
`default_chunk_lines` 500, `sub_query_max_chars` 120000, `sub_query_timeout_secs`
120); see the [configuration reference](../reference/configuration.md#toolscontext).

---

## System prompt integration

**Source**: `crates/turn/src/prompts.rs`, function `build_guidelines_section`

A `### Large Content Analysis` paragraph is part of the Guidelines section of
every system prompt (`guidelines::large_content()`). It states when to prefer
the `context` tool over `read_file` and describes the workflow
(`context(action=open)` → `grep` → `read` → `query` → `reduce`).

---

## File layout

```
crates/tools-ctx/src/context/
  mod.rs             module declarations and re-exports
  store.rs           ContextStore, ContextHandle, ContextKind, build_line_index
  open.rs            ContextOpenTool
  read.rs            ContextReadTool
  grep.rs            ContextGrepTool
  expand.rs          ChunkExpander, ChunkExpandTool
  query_runner.rs    SubQueryRunner trait

crates/bootstrap/src/
  context_tool.rs    ContextTool, the compound `context` tool
  context_query.rs   ContextConfig, ModelSubQueryRunner, ContextQueryTool,
                     ContextReduceTool, build_context_query_tools, tree_reduce
```

---

## Security and safety notes

- **Path scope**: `open` resolves its path through the session's path scope and
  refuses anything outside a confined root. A directory walk does not follow
  symbolic links, so a link inside the project cannot lead it out. Content that
  was opened is read through its handle, not by path.
- **Where content goes**: `query` and `reduce` send chunks of the opened content
  to the session's own model provider, and to no other. Query results live in
  memory only; sven writes no temporary files for them.
- **Binary content**: files detected as binary are silently skipped in
  directory handles. The summary reports a count of skipped files. A single
  file opened directly is mapped as it is.
- **Mmap lifetime**: the mapped files are read-only views owned by the store.
  Dropping the store unmaps all content. A file that is truncated or rewritten
  while it is open can change what later reads return.
