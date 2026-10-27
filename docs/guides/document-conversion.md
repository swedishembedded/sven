<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# Converting PDFs to Markdown

sven converts a PDF to a Markdown file without passing the pages through the
chat model. One engine serves two entry points:

- `sven document convert in.pdf out.md` on the command line, and
- the `document_convert` tool, so that "convert paper.pdf to paper.md using
  deepseek ocr v2" typed to an agent is one tool call. The tool result is a
  short record (status, output path, hash, page counts, warnings); the text goes
  from the OCR model into the file and never into the conversation.

Nothing needs to be installed besides sven: pages are rendered and their native
text is read by pure-Rust libraries.

## How a conversion runs

1. **Inspect.** The page count is read; an encrypted PDF is refused with a clear
   message.
2. **Classify each page.** A page whose own text layer is good keeps it (fast,
   parallel, no model). "Good" is judged by the text, not by its presence: a
   scanned archive's old OCR layer is full of non-words and is not trusted.
   Pages without usable text are rendered and sent to the OCR model.
   `--ocr always` sends every page, `--ocr never` uses native text only.
3. **Recognise.** Pages are rendered ahead of the model on all cores, and the
   model is kept busy: up to `--concurrency` requests are in flight per endpoint
   (the window starts there and is halved while the server pushes back with
   429/503, a full queue or a timeout, then grows again). Only about twice that
   many page images exist at any time, so memory does not grow with the
   document, and the report states the peak in flight and how long the backend
   sat idle while pages remained. Each request carries one page image
   and the prompt (default `Convert the document to markdown.`, deliberately
   without a grounding marker, which would switch a grounding-capable model to
   bounding-box output). Blank pages cost no request.
   **Columns and articles.** Whole-page OCR reads a newspaper badly: columns
   interleave and a model loses its place after the first column. With
   `--layout auto` (the default) a page that looks like columns is sent to the
   model for its blocks with their locations (DeepSeek-OCR's grounding output,
   `<|ref|>label<|/ref|><|det|>[[x1,y1,x2,y2]]<|/det|>`). When the blocks line
   up in three or more columns, or the whole-page reading was cut off or
   looped, the page is read block by block; otherwise (a page with figures) it
   is read whole. The blocks are put in reading order by a deterministic rule,
   with no chat model involved: wide blocks such as a masthead cut the page into
   bands; within a band the blocks are read column by column, each headline
   before the text under it and each article whole before the next. A block the
   model lists with two boxes (a paragraph that continues in the next column) is
   two regions. Runs of body blocks that follow each other in one column are
   merged into one crop, headlines and one-line blocks keep the text of the
   layout pass, and each crop is read again from the page at `--region-dpi`,
   which resolves small type the whole page cannot: a newspaper page costs
   about fifteen requests rather than one per paragraph. The crops are
   independent requests and use the same concurrency window. A crop that loops,
   runs to the token limit or is mostly non-words is a picture the model
   invented text for; the layout pass's own text stands in and the page is marked
   with a warning. A text repeated by two overlapping crops is kept once. A model
   that does not return locations reads the page whole, with a note.
   `--layout regions` reads every OCR page by blocks, `--layout off` never.
4. **Validate and retry.** An answer is checked for a cut-off (token limit),
   emptiness, degenerate repetition, leaked model control tokens, and against the page's own text layer: coverage
   of its words where the layer is good, and the numbers and codes (register
   values, bit patterns, amounts) wherever the layer has enough of them. A page that fails is
   rendered again at a higher resolution, up to `--max-attempts`. In `auto`,
   a page whose recognised numbers still disagree with its text layer keeps the
   text layer (a misread digit in a register value is worse than lost
   formatting) and says so; `--ocr always` keeps the recognised text. A page that
   never validates keeps its best text with a warning; a page that never
   produced text is marked failed in the output.
5. **Publish.** Pages are assembled in order (`--page-markers` adds
   `<!-- page: N -->`; `--dedupe-headers` removes running headers and footers)
   and written by temporary file and atomic rename. An error before that leaves
   any existing output untouched.

Native text is plain text: it keeps reading order but not headings or tables.
Use `--ocr always` when the structure matters.

## The manifest and resuming

Beside the output, `<out>.manifest.json` records for every page how it was
obtained (`native`, `ocr`, `blank`), its status (`ok`, `warning`, `failed`),
the model, attempts, resolution, SHA-256 of its text, timings, token counts and
warnings, together with the text itself. It is rewritten atomically as pages
finish, so it is valid after a crash or a cancelled conversion.

A page's identity is the hash of the source bytes, page number, method, render
resolution, model, prompt and token limit. A rerun reuses pages that are `ok`
with the same identity and a text that still matches its hash, and redoes the
rest. Changing the model, the prompt or the resolution therefore redoes the
pages; `--no-resume` ignores the manifest.

## Choosing the OCR model

The model is an ordinary sven model spec resolved by the usual provider layer;
it must accept images. brain and hosted services work the same way:

```yaml
providers:
  ocr-box:
    name: brain
    base_url: http://127.0.0.1:8800/v1
    api_key_file: ~/.config/sven/ocr.key

ocr:
  model: ocr-box/deepseek-ai/DeepSeek-OCR-2     # or openrouter/qwen/qwen2.5-vl-72b-instruct
  models:                                        # aliases for loose names
    fast: ocr-box/deepseek-ai/DeepSeek-OCR
```

`--model` (or the tool's `model` argument) overrides `ocr.model`, and accepts
a spec, an alias or a loose name such as `"deepseek ocr v2"`, matched
word by word against the aliases and against the OCR models the reachable
providers report serving. An ambiguous or unknown name is refused with the
list of choices. Without any of these the default is DeepSeek-OCR-2 on a
provider that serves it.

A model the catalog does not know as image-capable needs
`input_modalities: [text, image]` on its provider entry. Several endpoints that
serve the same model (one server per GPU) are listed as
`models: {pool: [a/m, b/m]}`; requests go to the endpoint with the most headroom, so work spreads in proportion to what each can take.

**Tiling.** brain reads a dense page in tiles; `driver_options: {max_tiles: N}`
on the provider entry (a request field brain forwards, `0` reads the page as one
view) trades accuracy for speed. Measured on three pages of a born-digital paper
with DeepSeek-OCR-2: the default (up to 6 tiles) gave a character error rate of
0.006 against the PDF's own text on a dense text page, `max_tiles: 0` 0.040 in
60% of the time, so the default stays; on a figure page where tiling made the
model loop to the token limit, `max_tiles: 0` ended after 193 tokens.

**No OCR model.** When none is configured or discovered, or the configured one
is unreachable or cannot take images, nothing falls back silently. Pages with a
good text layer still convert; every page that needs OCR fails with that cause
and is marked in the output, so the result is `partial` (CLI exit 2, the tool
record adds `no_ocr_model` with the reason). When no page could convert, or
`--ocr always` was asked for, nothing is converted: the CLI exits
with status 3 and says so; the tool returns `status: no_ocr_model` whose text
tells the chat model to report this to the user and to ask whether to try
command-line converters that may be installed (`pdftotext`, `mutool`, `pandoc`,
`ocrmypdf`, `tesseract`; the tool only checks which are on `PATH` and never runs
them).

## Command line

```
sven document convert in.pdf out.md [--model SPEC] [--pages 1-3,7] [--ocr auto|always|never]
    [--layout auto|off|regions] [--region-dpi N] [--layout-prompt TEXT] [--concurrency N] [--dpi N] [--max-tokens N] [--prompt TEXT] [--max-attempts N]
    [--page-markers] [--dedupe-headers] [--no-resume] [--json] [--quiet]
sven document score reference.txt out.md
```

Progress goes to stderr, one line per page. Exit status: 0 success, 2 when
some pages failed (the output is still written, failed pages marked), 3 when no
OCR model is available and no page could convert without one. `score` prints the character error rate of a text
against a reference after whitespace normalisation, for checking a conversion
against ground truth.

## The agent tool

`document_convert(source, destination, model?, pages?, ocr?)` runs in `agent`
mode and is gated as a file write. Both paths resolve through the session's
workspace scope. It is deferred: the model loads it with the `tools` tool.
The result looks like:

```json
{"status":"ok","output":"/work/paper.md","sha256":"…","model":"…",
 "pages":{"total":15,"native":15,"ocr":0,"blank":0,"reused":0,"warning":0,"failed":0},
 "warnings":[],"manifest":"/work/paper.md.manifest.json","seconds":4}
```

`status` is `ok`, `ok_with_warnings`, `partial` (some pages failed), `failed`
(none converted) or `no_ocr_model`. A failure carries `failures` (each distinct
cause with the pages it hit, as the model server reported it) and an
instruction to the chat model to report the cause to the user and stop, not to
retry or work around it; an error that ends the whole conversion (an unreadable
source) is worded the same way. Cancelling the tool stops the requests in flight and leaves the
manifest consistent.

### Converting one chapter

`document_locate(source, query?)` finds where something is without reading it.
It is read-only and runs in every mode. With a `query` it returns the outline
entries whose title matches, each with the pages it covers, and the pages whose
native text matches, each with a match count and a short snippet; without one
it lists the outline's top two levels. The query is literal by default;
`syntax: "regex"` takes a regular expression (`USART_(SR|BRR)`) and
`syntax: "fzf"` takes fzf's extended search (`^25 usart`, `baud 'register`),
ranked best first. `pages` narrows the search, and results come `limit` at a
time with a `next_offset` for the rest, so a small context window can page
through a large result. All parameters are in the
[tools reference](../reference/tools.md#document_locate).

From a `document_locate(source: "manual.pdf", query: "USART")` on the STM32F10x
reference manual (RM0008, 995 pages), shortened:

```json
{"query":"USART","outline_matches":[
  {"level":1,"title":"25 Universal synchronous asynchronous receiver transmitter (USART)","pages":"653-693"},
  {"level":2,"title":"25.3 USART functional description","pages":"654-680"}, …],
 "outline_match_count":…,"text_hits":[{"page":653,"count":…,"snippet":"…"}, …],
 "text_hit_pages":…,"searched_pages":995,"page_count":995,"next_offset":30}
```

So "extract the USART chapter of manual.pdf into usart.md" is one
`document_locate` and one `document_convert` with that `pages` range; no page
text passes through the chat model. Pages without a usable text layer are not
searched, and the record says how many there were.

Configuration keys are in the [configuration reference](../reference/configuration.md#ocr).
