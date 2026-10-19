<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# Working with Large Content

sven can analyse files and directory trees that are far too large to read
in full - build logs, entire codebases, generated output files, firmware
images - without filling the context window. This page explains when that
capability is available and how to get the most out of it.

---

## The problem with large files

Every model has a context window: a hard limit on how many tokens it can hold
at once. When a file exceeds that limit, an agent has to choose between
truncating the file (losing information) or refusing to analyse it.

sven's `context` tool takes a different approach. Large content is kept in
memory **outside** the model's context window and accessed on demand. The model
sees only a small summary - a _handle_ - and then retrieves exactly the parts it
needs.

---

## When the `context` tool is available

The `context` tool is part of sven's **full** tool set, which a session gets when
the project root contains GDB configuration (`.gdbinit`, `openocd.cfg`, a
`debugging/` directory and similar; see [Debugging](debugging.md#when-the-gdb-tool-is-offered)).
It works in `agent` mode, and not in `task` sub-agents. In other projects the
default coding tool set has no `context` tool, and large content is handled with
`read_file` (line ranges with `offset` and `limit`), `grep`, and `shell` output,
which keeps its first and last 100 lines when it exceeds about 20 KB. A `task`
sub-agent can also be given the large reading and return only its conclusion.

Under the default deferred tool disclosure the model loads `context` with the
`tools` tool the first time it needs it; you do not need to do anything. See
[Tools and approvals](../reference/tools.md#context).

---

## How it works (from the user's perspective)

When the model decides a file or directory is too large to read, it follows a
four-step pattern on its own:

1. **Open** - map the content into memory and get back a handle and a
   structural summary (size, line count, file types).
2. **Search** - run a regex search across the entire content to find the
   relevant sections, without loading anything into the model context.
3. **Inspect** - read the specific line ranges that were found.
4. **Analyse** - for questions that require reading many sections, dispatch
   sub-queries over chunks of the content in parallel (`query`), then combine
   the per-chunk answers into one (`reduce`).

You do not need to break up your questions or pre-process the input. Each
sub-query is a single model completion over one chunk plus the model's prompt:
it has no tools and no access to the conversation.

`query` and `reduce` send the content to the model provider of the session. For
that reason they are network calls: under `--approval manual` you are asked
before each one, while `open`, `read` and `grep` are read-only and are not.

---

## What you can do with this

### Analyse a large log file

```
Analyse /var/log/kern.log and find all USB disconnect errors from the last boot.
Summarise each unique error message, its frequency, and the first timestamp it appeared.
```

The model opens the log, searches for `usb.*disconnect` and related patterns,
reads the matching sections, and returns a structured summary, even if the log
is hundreds of megabytes.

### Review a large codebase for a specific concern

```
Review the entire src/ directory for any place where malloc() is called but the
return value is not checked.  For each finding, report the file, line number,
function name, and the unchecked call.
```

The tree is split into chunks, each chunk is analysed by a separate sub-query
(four at a time by default), and the findings are combined into a deduplicated
final report.

### Summarise a generated file

```
I have a build log at /tmp/build.log (2 MB). Find all compiler warnings, group them
by warning type, and tell me which file generates the most warnings overall.
```

### Deep-dive into an unfamiliar module

```
Open the drivers/spi/ directory and explain how the STM32 SPI DMA transfer is
initiated. Focus on the interrupt handlers and the DMA callback chain.
```

---

## Tips for getting the best results

**Be specific about what you're looking for.**
The analysis is most effective when you give sven a clear target. Instead of
_"analyse this log"_, try _"find all ERROR-level messages after timestamp
14:22:00 and group them by subsystem"_.

**You don't need to split large tasks manually.**
Chunking and parallelism are handled by the tool. Asking about a 50 000-line
file is no harder than asking about a 500-line file.

**Chain follow-up questions naturally.**
A handle stays open for the rest of the running session, so follow-up questions
can drill into sections found in earlier answers without re-opening the file.
Handles are not saved with the session: after a restart or a resume the content
is opened again.

> **Example session:**
> ```
> User:   Open src/ and find all functions that allocate memory.
> Sven:   [searches, finds 47 functions across 12 files, returns summary]
>
> User:   Which of those functions are called from interrupt context?
> Sven:   [searches for the 47 identifiers in ISR-related files, returns subset]
>
> User:   Show me the full source of the three most suspicious ones.
> Sven:   [reads the specific line ranges, returns formatted code]
> ```

**For very large directories, narrow with an include pattern.**
If you only care about C files, say so:

```
Open drivers/ but only include *.c files, and find all calls to k_mutex_lock
that are not paired with a k_mutex_unlock in the same function.
```

The pattern is a file-name wildcard (`*` and `?`). Directory handles skip
`.git`, `target`, `node_modules`, `.cache`, `__pycache__`, `.venv`, `venv`,
empty files, binary files and symbolic links.

---

## Configuration

The defaults work well for most use cases. To tune them, add a `tools.context`
section to your sven config file:

```yaml
tools:
  context:
    max_parallel: 4              # concurrent sub-queries (default: 4)
    default_chunk_lines: 500     # lines per chunk when not specified (default: 500)
    sub_query_max_chars: 120000  # character cap per sub-query prompt (default: 120000)
    sub_query_timeout_secs: 120  # per sub-query timeout, 0 for none (default: 120)
```

Increase `max_parallel` on fast hardware to reduce wall-clock time for large
directory scans. Reduce it if you hit provider rate limits.

Reduce `default_chunk_lines` for dense files (for example minified JavaScript)
where 500 lines would still be too large for a meaningful sub-query.

---

## Small files

The system prompt tells the model to reserve `context` for files over roughly
500 lines, build logs and large codebases; smaller files are read directly with
`read_file`, which is simpler and faster.

---

## See also

- [Configuration reference](../reference/configuration.md#toolscontext) - the `tools.context` options
- [Technical reference](../internals/rlm-context-tools.md) - implementation details
  for contributors and integrators
