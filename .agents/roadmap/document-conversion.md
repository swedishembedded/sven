# document-conversion

Scope: Open work and measured numbers for PDF to Markdown conversion (`sven document`, `document_convert`).

The behaviour is described in `docs/guides/document-conversion.md`; this ledger
holds what is measured and what is not done.

## Measured (2026-10-26)

Client: sven debug build with the PDF dependencies optimised, on a shared
144-core node. Server: brain (current main) serving DeepSeek-OCR v1 on one
GH200, which decodes one request at a time today, so these are server-bound.

| Workload | Result |
|----------|--------|
| RM0090 (1,741 pages, born digital), `--ocr never` | 17 s, all cores idle but one (text extraction is one thread): about 6,100 pages/min; 72 MB peak at 20 pages |
| RM0090, `auto`, pages 100-119 | 17 native, 3 to OCR, 188 s (46 requests, the three OCR pages read by layout) |
| RM0090, `--ocr always`, pages 100-119 | 724 s for 20 pages (1.7 pages/min, 36 s/page for dense register pages on the serial server), 24 requests, peak 16 in flight, backend idle 0 ms, 36 MB client peak; one page hit the token limit |
| RM0090 text-layer quality | 225 of 1,741 pages (13%) judged unreliable (figure labels, bit-field diagrams) and routed to OCR in `auto` |
| DeepSeek-OCR paper, 22 pages, `--ocr always` | 476 s (21.6 s/page), 0 warnings, CER 0.19 against the PDF's own text (includes Markdown markup and figures) |
| Workers World 1960, 4 newspaper pages, `--layout regions` | 182 requests in 2,037 s (11 s per request on the serial server), peak 8 in flight, backend idle 0 ms; articles and columns come out separate |
| Same page, whole-page reading | the model loops after the first column and hits the token limit |
| Polish newspaper, 6 pages, `auto` | 75 requests in 740 s; the 3 multi-column pages read by layout |
| Wonder Stories 1934, pages 49-50 (good archive.org layer) | native vs OCR CER 0.024-0.026: `auto` keeps the layer |
| Wonder Stories 1934, cover and an advert (poor layer) | 20 and 2,395 native characters against 89 and 6,412 recognised: `auto` routes them to OCR |
| NACA 1940s typewritten, pages 4-5 | native vs OCR CER 0.11 and 0.21 (OCR writes equations as LaTeX) |
| getomni rows 1-4 (photos, tables, delivery note) | CER 0.15, 0.73, 0.24, 0.22 against the Markdown ground truth |
| RM0090 pages 100 and 106, OCR vs text layer | OCR recovers the bit-field tables as Markdown tables but misreads some digits (`D1011` for `01011`); the layer has the digits right and loses the structure. The numbers check compares the two and warns |
| Client cost | page render 0.9 s median per page at 150 dpi including PNG encoding |

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
- `auto` layout uses a cheap gutter count on the rendered page (2 or more
  columns); it also fires on pages with large figures, where the layout pass
  costs extra requests for no gain. A model-side signal would be better.
- Reading order is one deterministic rule (bands, columns, headline-owned
  articles); articles that wrap around a picture or a sidebar are not modelled.
- Handwriting (19th-century German, IAM) was not scored: the available OCR
  model is not a handwriting model.
