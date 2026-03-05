# Working with Large Content

sven can analyse files and directory trees that are far too large to read
in full — build logs, entire codebases, generated output files, firmware
images — without ever running out of context window.  This section explains
how to get the most out of that capability.

---

## The problem with large files

Every AI model has a context window: a hard limit on how many tokens it can
hold at once.  When a file exceeds that limit, a naïve agent has to choose
between truncating the file (losing information) or refusing to analyse it
at all.

sven takes a different approach.  Large content is kept in memory **outside**
the model's context window and accessed on demand through a set of structured
tools.  The model sees only a small summary — a _handle_ — and then retrieves
exactly the parts it needs.

---

## How it works (from the user's perspective)

When sven encounters a large file or directory, it follows a four-step pattern
automatically:

1. **Open** — map the content into memory and get back a handle and a
   structural summary (size, line count, file types).
2. **Search** — run a regex search across the entire content to find the
   relevant sections, without loading anything into the model context.
3. **Inspect** — read the specific line ranges that were found.
4. **Analyse** — for questions that require reading many sections in parallel,
   dispatch sub-agent queries over chunks of the content simultaneously.

The model manages this loop itself.  You do not need to break up your
questions or pre-process the input.

---

## What you can do with this

### Analyse a large log file

```
Analyse /var/log/kern.log and find all USB disconnect errors from the last boot.
Summarise each unique error message, its frequency, and the first timestamp it appeared.
```

sven will open the log, search for `usb.*disconnect` and related patterns,
read the matching sections, and return a structured summary — even if the log
is hundreds of megabytes.

### Review a large codebase for a specific concern

```
Review the entire src/ directory for any place where malloc() is called but the
return value is not checked.  For each finding, report the file, line number,
function name, and the unchecked call.
```

sven chunks the source tree, sends each chunk to a parallel sub-agent, collects
all findings, and then synthesises a deduplicated final report.

### Compare behaviour across a large generated file

```
I have a build log at /tmp/build.log (2 MB). Find all compiler warnings, group them
by warning type, and tell me which file generates the most warnings overall.
```

### Deep-dive into an unfamiliar module

```
Open the drivers/spi/ directory and explain how the STM32 SPI DMA transfer is
initiated. Focus on the interrupt handlers and the DMA callback chain.
```

sven will index all source files in the directory, grep for DMA-related
symbols, read the relevant code, and produce a coherent explanation — without
running out of context.

---

## Tips for getting the best results

**Be specific about what you're looking for.**
The analysis is most effective when you give sven a clear target.  Instead of
_"analyse this log"_, try _"find all ERROR-level messages after timestamp
14:22:00 and group them by subsystem"_.

**You don't need to split large tasks manually.**
sven handles chunking and parallelism internally.  Asking about a 50 000-line
file is no harder than asking about a 500-line file.

**Chain follow-up questions naturally.**
Once a handle is open it persists for the entire conversation.  You can ask
follow-up questions that drill into sections found in earlier answers without
re-opening the file.

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

---

## Configuration

The default settings work well for most use cases.  If you need to tune them,
add a `tools.context` section to your sven config file:

```yaml
tools:
  context:
    max_parallel: 4          # concurrent sub-agent queries (default: 4)
    default_chunk_lines: 500 # lines per chunk when not specified (default: 500)
    sub_query_max_chars: 120000  # character cap per sub-query prompt (default: 120000)
```

Increase `max_parallel` on fast hardware to reduce wall-clock time for large
directory scans.  Reduce it if you hit provider rate limits.

Reduce `default_chunk_lines` for dense files (e.g. minified JavaScript) where
500 lines would still be too large for a meaningful sub-query.

---

## When sven falls back to read_file

For files under roughly 500 lines, sven uses `read_file` directly — it is
simpler and faster.  The large-content tools activate automatically when the
file or task exceeds that threshold.  You never need to choose between them
explicitly.

---

## See also

- [Configuration reference](05-configuration.md) — full list of `tools.context` options
- [Technical reference](technical/rlm-context-tools.md) — implementation details
  for contributors and integrators
