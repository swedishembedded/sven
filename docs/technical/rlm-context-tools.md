# RLM Context Tools — Technical Reference

This document describes the internal architecture of sven's memory-mapped
context tools, which implement the inference paradigm introduced in
[_Recursive Language Models_](https://arxiv.org/abs/2512.24601) (RLM, 2024).

---

## Motivation

A language model's context window is finite.  When an agent needs to reason
over a large file, a build log, or an entire directory tree, naïvely reading
the content consumes most or all of the available token budget — leaving little
room for tool calls, chain-of-thought, and the model's own response.  The RLM
paper's core insight is that **large content should live outside the context
window**; the model instead receives a symbolic handle and interacts with the
content through structured read, search, and recursive sub-query operations.

sven implements this pattern with Rust-native, zero-copy memory-mapped tools
whose descriptions encode the RLM workflow so that the model uses them
correctly without external orchestration.

---

## Architecture overview

```
┌─────────────────────────────────────────────────────────────────────┐
│  sven-tools crate                                                    │
│                                                                      │
│  ContextStore (Arc<Mutex<ContextStore>>, one per session)            │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │ HashMap<String, ContextHandle>                                │   │
│  │  ContextHandle.kind                                           │   │
│  │    SingleFile  { path, Mmap, line_index: Vec<usize> }        │   │
│  │    Directory   { root, Vec<FileEntry { path, Mmap, ... }> }  │   │
│  │    Results     { path, Mmap, line_index, entry_count }        │   │
│  └──────────────────────────────────────────────────────────────┘   │
│                                                                      │
│  context_open  ──► ContextStore::open_file / open_directory          │
│  context_read  ──► ContextStore::read_range                          │
│  context_grep  ──► ContextStore::grep                                │
│                                                                      │
│  SubQueryRunner trait                                                │
└─────────────────────────────────────────────────────────────────────┘
        │ Arc<dyn SubQueryRunner>
        ▼
┌─────────────────────────────────────────────────────────────────────┐
│  sven-bootstrap crate                                                │
│                                                                      │
│  ModelSubQueryRunner (impl SubQueryRunner)                           │
│    └─► ModelProvider::complete (stateless 2-msg completion)          │
│                                                                      │
│  context_query  ──► chunk → parallel sub-queries → Results handle   │
│  context_reduce ──► read_all → tree-reduce → plain text              │
└─────────────────────────────────────────────────────────────────────┘
```

**Dependency rule**: `sven-tools` never imports `sven-model`.  The
`SubQueryRunner` trait is the boundary that keeps the crates decoupled.  The
concrete `ModelSubQueryRunner` implementation lives in `sven-bootstrap`
alongside `TaskTool`, which follows the same pattern.

---

## ContextStore

**Source**: `crates/sven-tools/src/builtin/context/store.rs`

One instance is created per agent session in `build_tool_registry` and wrapped
in `Arc<Mutex<ContextStore>>`.  All five tools share the same pointer.

### Handle kinds

| Kind | When created | Backing storage |
|------|-------------|-----------------|
| `SingleFile` | `context_open` on a regular file | One `Mmap` + `Vec<usize>` line index |
| `Directory` | `context_open` on a directory | `Vec<FileEntry>`, each with its own `Mmap` + line index |
| `Results` | `context_query` writes a temp file; `register_results` mmaps it | One `Mmap` + `Vec<usize>` line index |

### Line index

For every mapped file sven builds a `Vec<usize>` of byte offsets — one entry
per line start.  Line 0 always starts at byte 0; subsequent entries are
recorded immediately after each `\n`.

```
content: "alpha\nbeta\ngamma\n"
         0     6     11
index:   [0,   6,    11]
```

Reading line range `[start, end]` (1-indexed, inclusive) becomes a single
seek-and-slice operation:

```rust
let byte_start = line_index[start - 1];
let byte_end   = line_index[end]; // or mmap.len() at EOF
&mmap[byte_start..byte_end]
```

No heap allocation, no syscall beyond the initial `mmap(2)`.

### Directory traversal

`open_directory` recursively walks the tree with `fs::read_dir`, skipping
`.git`, `target`, `node_modules`, `.cache`, `__pycache__`, `.venv`, and
`venv`.  Each regular text file is individually mmapped.  Binary files are
detected by a null-byte check and a >30% non-printable-byte heuristic (the
same heuristic used by `read_file`); they are skipped and counted in the
summary.

An optional include pattern is applied using a small wildcard matcher (supports
`*` and `?`) so that no external crate is required.

---

## context_open

**Source**: `crates/sven-tools/src/builtin/context/open.rs`

Accepts a `path` (file or directory), an optional `include_pattern`, and an
optional `recursive` flag (default `true`).

Returns only metadata to the model:

```
Context opened: handle=ctx_0001
Files: 1, Lines: 4823, Bytes: 198412

Single file: /data/my-project/src/main.c
Size: 198412 bytes
Lines: 4823

Use context_grep(handle="ctx_0001", ...) to locate relevant sections.
Use context_read(handle="ctx_0001", start_line=N, end_line=M) to inspect ranges.
Use context_query(handle="ctx_0001", prompt="...") for semantic analysis.
```

The `OutputCategory` is `Generic` so the standard `smart_truncate` applies (no
special head/tail treatment — the output is already bounded by design).

---

## context_read

**Source**: `crates/sven-tools/src/builtin/context/read.rs`

Accepts a handle ID, `start_line`, `end_line` (1-indexed, inclusive), and an
optional `file` path-substring for directory handles.

Delegates to `ContextStore::read_range`, which:

1. Validates the handle exists.
2. For `SingleFile` / `Results` handles: slices the mmap using the line index.
3. For `Directory` handles without a `file` hint: iterates files in order,
   tracking a global line offset, and concatenates matching ranges separated by
   `--- /path/to/file ---` headers.
4. For `Directory` handles with a `file` hint: filters to the first file whose
   path contains the hint string.

Output lines are formatted as `L{n}:content` matching the `read_file` contract
so that subsequent `edit_file` calls can reference them directly.

`OutputCategory` is `FileContent`, which means the standard 4000-token cap and
head-only truncation applies.

---

## context_grep

**Source**: `crates/sven-tools/src/builtin/context/grep.rs`

Accepts a handle ID, a Rust-regex `pattern`, an optional `file` hint, a
`context_lines` count (default 2), and a `limit` (default 50).

`ContextStore::grep` iterates the line index, converts each byte slice to
UTF-8 (lossily), and tests it against the compiled `regex::Regex`.  For
matches it collects `context_lines` surrounding lines using the same index
arithmetic.

For directory handles, files are iterated in sorted path order.  Each match
carries its source `PathBuf` so that file context appears in the output.

`OutputCategory` is `MatchList`, applying the standard ripgrep-style
truncation.

---

## SubQueryRunner trait

**Source**: `crates/sven-tools/src/builtin/context/query_runner.rs`

```rust
#[async_trait]
pub trait SubQueryRunner: Send + Sync {
    async fn query(&self, system: &str, prompt: &str) -> Result<String, String>;
}
```

A two-message stateless LLM call.  The model receives no conversation history,
no tools, and no session context — only the system message and the user
message.  This is the `llm_query()` function from the RLM paper.

### ModelSubQueryRunner

**Source**: `crates/sven-bootstrap/src/context_query.rs`

Wraps `Arc<dyn ModelProvider>`.  Each call:

1. Optionally truncates the prompt to `sub_query_max_chars` (default 120 000
   characters ≈ 30 000 tokens) with a notice.
2. Builds a two-message `CompletionRequest` with `stream: true` and an empty
   tools list.
3. Drives the provider's stream via `futures::StreamExt`, collecting
   `ResponseEvent::TextDelta` fragments.
4. Returns the full assembled text string.

---

## context_query

**Source**: `crates/sven-bootstrap/src/context_query.rs`

The **map** step.  Accepts a handle, a `prompt` template (supporting
`{chunk}`, `{chunk_index}`, `{total_chunks}` placeholders), optional explicit
`ranges`, a `chunk_lines` size, and a `max_parallel` concurrency limit.

### Execution flow

```
1. Acquire store lock → build chunk list
   a. If ranges provided: each range is one chunk (store.read_range).
   b. Else: call store.chunks(chunk_lines) to generate all chunks.

2. Release store lock.

3. For each batch of min(max_parallel, remaining) chunks:
   a. Spawn tokio task: ModelSubQueryRunner::query(SYSTEM, prompt_with_chunk).
   b. Collect JoinHandles.
   c. Await all handles; failures are recorded as error strings (not panics).

4. Sort results by chunk index.

5. Serialize results to a temp file:
   /tmp/sven_ctx_results_<hex>_<pid>.txt

6. Acquire store lock → ContextStore::register_results(tmp_path, entry_count)
   → new Results handle with its own Mmap + line index.

7. Return: handle ID, byte count, entry count, preview of chunk 0 result,
   and usage instructions.
```

The sub-query system prompt is hardcoded:

```
You are a focused analysis sub-agent. Answer the question or perform the
analysis described in the user prompt using only the provided content. Be
concise and structured. Do not ask for clarification.
```

Each user message is prefixed with `[Chunk N/M: label]` so the model knows its
position in the overall scan.

### Configuration

```yaml
tools:
  context:
    max_parallel: 4          # default concurrent sub-queries
    default_chunk_lines: 500 # lines per chunk when chunk_lines not specified
    sub_query_max_chars: 120000
```

---

## context_reduce

**Source**: `crates/sven-bootstrap/src/context_query.rs`

The **reduce** step.  Accepts a handle and a synthesis `prompt`.

Calls `ContextStore::read_all` to get the full content of the handle as a
string, then delegates to `tree_reduce`.

### Tree reduction algorithm

```
tree_reduce(runner, system, content, prompt, max_chars, chunk_lines, depth=0):
  if depth >= MAX_REDUCE_DEPTH (4):
    truncate content to max_chars + notice; send directly.
  if content.len() <= max_chars:
    send as single call: content + "\n\n" + prompt.
  else:
    split content into lines, chunk by chunk_lines.
    for each chunk:
      intermediate = runner.query(system, chunk + "\n\n" + reduce_prompt)
    combined = join intermediates with "---" separators
    return tree_reduce(runner, system, combined, prompt, ..., depth+1)
```

The `reduce_prompt` used for intermediate levels asks the sub-agent to
summarize while preserving specific values (line numbers, severity labels,
etc.).  This is critical for use cases like security audits where losing a
specific finding during intermediate reduction would silently drop signal.

The maximum recursion depth is 4 levels.  At each level the intermediate
results are typically an order of magnitude smaller than the input, so
convergence is fast in practice.

---

## Registration

**Source**: `crates/sven-bootstrap/src/registry.rs`

Both `Full` (interactive TUI) and `SubAgent` (spawned by `TaskTool`) profiles
receive all five tools.  The `ContextStore` and `SubQueryRunner` are
constructed fresh per registry call:

```rust
let context_store = Arc::new(Mutex::new(ContextStore::new()));
reg.register(ContextOpenTool::new(context_store.clone()));
reg.register(ContextReadTool::new(context_store.clone()));
reg.register(ContextGrepTool::new(context_store.clone()));
let (ctx_query, ctx_reduce) = build_context_query_tools(context_store, model, cfg);
reg.register(ctx_query);
reg.register(ctx_reduce);
```

Each session thus has its own isolated store.  Handles from one session are not
accessible to another.  Sub-agents spawned by `TaskTool` get their own store
and can open independent contexts.

---

## System prompt integration

**Source**: `crates/sven-core/src/prompts.rs`, function `build_guidelines_section`

A `### Large Content Analysis` section is injected into every system prompt
via the `guidelines::large_content()` static string.  It states when to
prefer context tools over `read_file` and describes the recommended workflow
(`context_open` → `context_grep` → `context_read` → `context_query` →
`context_reduce`).

This ensures the model selects the correct tool chain proactively, without
relying solely on the individual tool descriptions.

---

## File layout

```
crates/sven-tools/src/builtin/context/
  mod.rs             module declarations and re-exports
  store.rs           ContextStore, ContextHandle, ContextKind, build_line_index
  open.rs            ContextOpenTool
  read.rs            ContextReadTool
  grep.rs            ContextGrepTool
  query_runner.rs    SubQueryRunner trait

crates/sven-bootstrap/src/
  context_query.rs   ModelSubQueryRunner, ContextQueryTool, ContextReduceTool,
                     build_context_query_tools, tree_reduce
```

---

## Security and safety notes

- **No path traversal beyond the provided root**: `open_directory` canonicalises
  the root before walking.  Symlinks are followed by `fs::read_dir`, which is
  consistent with the rest of sven's filesystem tools.
- **Temp files**: `context_query` writes results to `/tmp/sven_ctx_results_*.txt`.
  These are named with nanosecond timestamp + PID.  They are not cleaned up at
  session end; users on shared machines should treat `/tmp` as world-readable.
- **Binary content**: files detected as binary are silently skipped in
  directory handles.  The summary reports a count of skipped files.
- **Mmap lifetime**: the `Mmap` struct is `!Clone`.  Handles are stored by
  value inside the `ContextStore`; the store is the sole owner.  Dropping
  the store unmaps all content.
