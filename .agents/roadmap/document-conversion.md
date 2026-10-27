# document-conversion

Scope: Open work and measured numbers for PDF to Markdown conversion (`sven document`, `document_convert`).

The behaviour is described in `docs/guides/document-conversion.md`; this ledger
holds what is measured and what is not done.

## Measured

First round (2026-10-26): sven debug build with the PDF dependencies
optimised, on a shared 144-core node; brain served DeepSeek-OCR v1 on one
GH200, one request at a time, so those numbers were server-bound.

| Workload | Result |
|----------|--------|
| RM0090 (1,741 pages, born digital), `--ocr never` | 17 s, about 6,100 pages/min, text extraction on one thread; 72 MB peak at 20 pages |
| RM0090 text-layer quality | 225 of 1,741 pages (13%) judged unreliable (figure labels, bit-field diagrams) and routed to OCR in `auto` |
| DeepSeek-OCR paper, 22 pages, `--ocr always`, v1 serial | 476 s, 0 warnings, CER 0.19 against the PDF's own text (includes Markdown markup and figures) |
| Wonder Stories 1934, pages 49-50 (good archive.org layer) | native vs OCR CER 0.024-0.026: `auto` keeps the layer |
| Wonder Stories 1934, cover and an advert (poor layer) | 20 and 2,395 native characters against 89 and 6,412 recognised: `auto` routes them to OCR |
| NACA 1940s typewritten, pages 4-5 | native vs OCR CER 0.11 and 0.21 (OCR writes equations as LaTeX) |
| getomni rows 1-4 (photos, tables, delivery note) | CER 0.15, 0.73, 0.24, 0.22 against the Markdown ground truth |
| RM0090 pages 100 and 106, v1 OCR vs text layer | OCR recovers the bit-field tables as Markdown tables but misread digits (`D1011` for `01011`); the layer has the digits right and loses the structure |
| Page render | 0.9 s median per page at 150 dpi including PNG encoding |

Second round (2026-10-27): brain main from 02:29 serving DeepSeek-OCR v1 and v2
on gpu0 (shared with a 41 GB peer process), DeepSeek-OCR-2 by the loose name
"deepseek ocr v2", concurrency 16.

| Workload | Before | After |
|----------|--------|-------|
| 1960 newspaper page 1, v2, layout | 57-60 requests, 330 s (serial brain), 89 s on the faster brain | 16 requests, 61 s |
| Same, 4 pages, `auto` | about 60 requests per page | 67 requests (16, 17, 21, 13 per page), 91-93 s wall, peak 16 in flight, backend idle 1 s, 0 warnings, 425 MB client peak |
| RM0090 pages 100-119, `auto` | 17 native, 3 OCR, 46 requests, 188 s | 17 native, 3 OCR, 8 requests, 74 s |
| RM0090 pages 100-119, `--ocr always` | 107 requests (layout fired on every table page), 222 s | 23 requests, 149 s = 8.0 pages/min (register tables are 3,000+ tokens a page, about 400 tokens/s here; the 190-205 pages/min of the server benchmark is for 1,000-token pages on an idle card) |
| `max_tiles` on three pages of the DeepSeek paper (v2) | default (6 tiles): CER 0.158, 0.006 and a loop to the token limit on a figure page | `max_tiles: 0`: 0.158, 0.040, 193 tokens; 60% of the time. The default stays; `0` is the workaround for a page that loops |

Agent level (27B `unsloth/Qwen3.8-27B-GGUF` on gpu0 as the chat model): with an OCR
endpoint that answers every page with HTTP 400, the 27B called `document_convert`
once, reported the server's cause ("too large for that device's budget") and
asked how to proceed, running no other tool; before the instruction it had tried
`pip install pypdf`. The 27B and the OCR server did not fit on the node at
once (a 41 GB and a 56 GB peer process hold the rest of the two cards), so the
literal "convert ... using deepseek ocr v2" was not re-run end to end here; its
parts were run separately (the 27B finding the tool and calling it in the
earlier round, the newspaper and RM0090 numbers above through the CLI).

What cut the requests: the model's multi-box blocks (a paragraph that continues
in the next column) are separate regions instead of one box spanning both
columns, which had defeated every merge; runs of body blocks in one column are
one crop; headlines and one-line blocks keep the layout pass's text; a page
whose blocks do not line up in three disjoint columns (indents and table cells
are not columns) is read whole; and the cheap pre-check also needs the dense ink
of newspaper print (0.13-0.23 measured against 0.03-0.08 for register and
figure pages).

The OpenRouter path was checked against a stub on the OpenRouter driver (no key
is configured here): the request is a `chat/completions` call whose user
message is `[{type: image_url, image_url: {url: data:image/png;base64,...}},
{type: text, text: ...}]`, with the OpenRouter headers.

## Open

- DeepSeek-OCR-2 is not served yet; the default model selection prefers it and
  falls back to any OCR model a provider reports.
- The text extractor runs on one thread; very large born-digital documents
  would gain from splitting it by page range.
- Born-digital pages that carry tables are taken as plain text in `auto`; there
  is no table-structure detection on the text layer, only the quality check.
- `auto` layout is decided by a gutter count and ink density on the rendered
  page, then confirmed by the columns the model finds; a figure page that
  passes the pre-check costs one extra grounding request. A faint scan (the
  Polish newspaper's multi-column page has ink 0.053) misses the pre-check and
  is caught only when its whole-page reading fails.
- Reading order is one deterministic rule (bands, columns, headline-owned
  articles); articles that wrap around a picture or a sidebar are not modelled.
- Handwriting (19th-century German, IAM) was not scored: the available OCR
  model is not a handwriting model.
