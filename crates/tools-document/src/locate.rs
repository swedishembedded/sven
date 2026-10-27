// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `document_locate` tool: search a PDF without reading it.
//!
//! A PDF is binary: `grep` cannot see its text and `read_file` would pour
//! whole pages into the context. This tool searches the outline (chapters and
//! sections, each with the pages it covers) and the native text layer, with a
//! literal, regex or fzf query, and returns only titles, page numbers, match
//! counts and short snippets, paged by `limit` and `offset` so a small context
//! can walk a large result and a large one can take it at once. Without a
//! query it lists the outline. "Convert the USART chapter" is then one locate
//! and one `document_convert` of those pages.
//!
//! Swedish Embedded AB implements agent tools that move bulk data around the
//! language model instead of through it. If your team needs expertise in
//! building cost-efficient document agents then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use sven_document::{
    assess_native_text, find_text, outline, outline_matches, parse_selection, HayroBackend,
    OutlineEntry, PdfBackend, Query, Syntax, TextQuality,
};
use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolDisplay, ToolOutput};
use sven_tool_api::PathScope;

/// Outline levels the overview lists unless `depth` says otherwise.
const OVERVIEW_DEPTH: usize = 2;
/// Entries per list unless `limit` says otherwise: a page of results that
/// fits a small context.
const DEFAULT_LIMIT: usize = 30;
/// The largest `limit`.
const MAX_LIMIT: usize = 1000;
/// Characters of snippet on each side of a match unless `context` says
/// otherwise, and the most it may say.
const DEFAULT_CONTEXT: usize = 60;
const MAX_CONTEXT: usize = 1000;

const PARAMETERS: [&str; 8] = [
    "source", "query", "syntax", "pages", "depth", "limit", "offset", "context",
];

pub struct DocumentLocateTool {
    scope: PathScope,
}

impl DocumentLocateTool {
    pub fn new(scope: PathScope) -> Self {
        Self { scope }
    }
}

#[async_trait]
impl Tool for DocumentLocateTool {
    fn name(&self) -> &str {
        "document_locate"
    }

    fn summary(&self) -> &str {
        "Search a PDF's outline and text (literal, regex, fzf): the pages holding a chapter or term"
    }

    fn description(&self) -> &str {
        "Search a PDF's outline and text layer without reading it (plain-text files need no \
         special tool). Returns the outline entries whose title matches, each with its page \
         range, and the pages whose text matches, each with a match count and a snippet; \
         without `query`, the outline. Results are paged: pass `next_offset` back as `offset`. \
         A match's `pages` is the selection to convert that part to Markdown."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "source": {"type": "string", "description": "PDF file"},
                "query": {"type": "string", "description": "what to find; omit to list the outline"},
                "syntax": {"type": "string", "enum": Syntax::NAMES, "description": "literal (default); regex; fzf: terms on one line, abc fuzzy, 'abc exact, ^abc prefix, abc$ suffix, !abc not, a | b either"},
                "pages": {"type": "string", "description": "only these pages, e.g. 600-700"},
                "depth": {"type": "integer", "minimum": 1, "description": "outline levels listed"},
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "description": "entries per list"},
                "offset": {"type": "integer", "minimum": 0},
                "context": {"type": "integer", "minimum": 0, "maximum": MAX_CONTEXT, "description": "snippet characters each side"}
            },
            "required": ["source"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let request = match Request::parse(&call.args) {
            Ok(r) => r,
            Err(e) => return ToolOutput::err(&call.id, e),
        };
        let path = match self.scope.resolve_for(call, &request.source) {
            Ok(p) => p,
            Err(refused) => return refused,
        };
        let data = match tokio::fs::read(&path).await {
            Ok(d) => d,
            Err(e) => {
                return ToolOutput::err(&call.id, format!("cannot read {}: {e}", path.display()))
            }
        };
        match tokio::task::spawn_blocking(move || locate(data, &request)).await {
            Ok(Ok(record)) => ToolOutput::ok(&call.id, record.to_string()),
            Ok(Err(e)) => ToolOutput::err(&call.id, e),
            Err(e) => ToolOutput::err(&call.id, format!("locating failed: {e}")),
        }
    }
}

/// The checked arguments of one call.
#[derive(Debug)]
struct Request {
    source: String,
    query: Option<Query>,
    query_text: String,
    pages: Option<String>,
    depth: Option<usize>,
    limit: usize,
    offset: usize,
    context: usize,
}

impl Request {
    /// Every argument checked: an unknown name or a value of the wrong type
    /// or range is refused, never ignored or clamped.
    fn parse(args: &Value) -> Result<Self, String> {
        let empty = Map::new();
        let args = match args {
            Value::Object(m) => m,
            Value::Null => &empty,
            _ => return Err("the arguments must be an object".into()),
        };
        if let Some(unknown) = args.keys().find(|k| !PARAMETERS.contains(&k.as_str())) {
            return Err(format!(
                "unknown parameter `{unknown}`; document_locate takes {}",
                PARAMETERS.join(", ")
            ));
        }
        let text = |key: &str| -> Result<Option<&str>, String> {
            match args.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.as_str())),
                Some(other) => Err(format!("`{key}` must be a string, not {other}")),
            }
        };
        let number = |key: &str, min: usize, max: usize| -> Result<Option<usize>, String> {
            match args.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => match v.as_u64().and_then(|n| usize::try_from(n).ok()) {
                    Some(n) if (min..=max).contains(&n) => Ok(Some(n)),
                    _ => Err(format!(
                        "`{key}` must be an integer from {min} to {max}, not {v}"
                    )),
                },
            }
        };
        let source = text("source")?.ok_or("missing required parameter `source`")?;
        let syntax = match text("syntax")? {
            None => Syntax::default(),
            Some(name) => Syntax::from_name(name).ok_or_else(|| {
                format!(
                    "`syntax` is one of {}, not {name:?}",
                    Syntax::NAMES.join(", ")
                )
            })?,
        };
        let query_text = text("query")?.unwrap_or("").to_string();
        let query = if query_text.trim().is_empty() {
            if args.contains_key("syntax") {
                return Err("`syntax` says how to read `query`, and there is no query".into());
            }
            None
        } else {
            Some(Query::new(&query_text, syntax).map_err(|e| e.to_string())?)
        };
        Ok(Self {
            source: source.to_string(),
            query,
            query_text,
            pages: text("pages")?.map(str::to_string),
            depth: number("depth", 1, usize::MAX)?,
            limit: number("limit", 1, MAX_LIMIT)?.unwrap_or(DEFAULT_LIMIT),
            offset: number("offset", 0, usize::MAX)?.unwrap_or(0),
            context: number("context", 0, MAX_CONTEXT)?.unwrap_or(DEFAULT_CONTEXT),
        })
    }

    /// One page of `items`, and the offset of the next page if there is one.
    fn page<T>(&self, items: Vec<T>) -> (Vec<T>, Option<usize>) {
        let total = items.len();
        let shown: Vec<T> = items
            .into_iter()
            .skip(self.offset)
            .take(self.limit)
            .collect();
        let next = self.offset + self.limit;
        (shown, (next < total).then_some(next))
    }
}

fn entry(e: &OutlineEntry) -> Value {
    json!({"level": e.level, "title": e.title, "pages": format!("{}-{}", e.first_page, e.last_page)})
}

/// `1,2,3,7` as `1-3,7`.
fn page_ranges(pages: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < pages.len() {
        let mut j = i;
        while j + 1 < pages.len() && pages[j + 1] == pages[j] + 1 {
            j += 1;
        }
        out.push(if i == j {
            pages[i].to_string()
        } else {
            format!("{}-{}", pages[i], pages[j])
        });
        i = j + 1;
    }
    out.join(",")
}

/// The located record: outline matches with page ranges and text hits, or the
/// outline overview. Never page text beyond a snippet.
fn locate(data: Vec<u8>, request: &Request) -> Result<Value, String> {
    let entries = outline(&data).map_err(|e| e.to_string())?;
    let source = HayroBackend
        .open(Arc::new(data))
        .map_err(|e| e.to_string())?;
    let page_count = source.page_count();
    let selected = parse_selection(request.pages.as_deref().unwrap_or(""), page_count)
        .map_err(|e| e.to_string())?;
    let in_selection = |e: &OutlineEntry| {
        selected
            .iter()
            .any(|&p| (e.first_page..=e.last_page).contains(&(p + 1)))
    };
    let depth = |e: &OutlineEntry| request.depth.is_none_or(|d| e.level <= d);

    let Some(query) = &request.query else {
        let depth = request.depth.unwrap_or(OVERVIEW_DEPTH);
        let listed: Vec<&OutlineEntry> = entries
            .iter()
            .filter(|e| e.level <= depth && in_selection(e))
            .collect();
        let total = listed.len();
        let (shown, next) = request.page(listed);
        let mut record = json!({
            "outline": shown.into_iter().map(entry).collect::<Vec<_>>(),
            "outline_entries": total,
            "page_count": page_count,
        });
        if let Some(next) = next {
            record["next_offset"] = json!(next);
        }
        record["note"] = json!(if entries.is_empty() {
            "This PDF has no outline; pass `query` to search its text."
        } else {
            "Pass `query` to search; give a match's `pages` to document_convert."
        });
        return Ok(record);
    };

    let matches: Vec<Value> = outline_matches(&entries, query)
        .into_iter()
        .filter(|(e, _)| depth(e) && in_selection(e))
        .map(|(e, _)| entry(e))
        .collect();
    let match_count = matches.len();
    let text = source.native_text();
    let hits: Vec<Value> = find_text(&text, query, &selected, request.context)
        .into_iter()
        .map(|h| json!({"page": h.page, "count": h.count, "snippet": h.snippet}))
        .collect();
    let hit_pages = hits.len();
    let (matches, next_match) = request.page(matches);
    let (hits, next_hit) = request.page(hits);
    let mut record = json!({
        "query": request.query_text,
        "syntax": query.syntax().name(),
        "outline_matches": matches,
        "outline_match_count": match_count,
        "text_hits": hits,
        "text_hit_pages": hit_pages,
        "searched_pages": selected.len(),
        "page_count": page_count,
    });
    if let Some(next) = next_match.max(next_hit) {
        record["next_offset"] = json!(next);
    }
    let unsearchable: Vec<usize> = selected
        .iter()
        .filter(|&&i| {
            text[i]
                .as_deref()
                .is_none_or(|t| assess_native_text(t) == TextQuality::Missing)
        })
        .map(|i| i + 1)
        .collect();
    if !unsearchable.is_empty() {
        // Say what the search could not see rather than imply the term is absent.
        let (pages, have, its, it) = if unsearchable.len() == 1 {
            ("page", "has", "its", "it")
        } else {
            ("pages", "have", "their", "them")
        };
        record["warning"] = json!(format!(
            "{pages} {} {have} no usable text layer, so {its} text was not searched; document_convert with an OCR model reads {it}",
            page_ranges(&unsearchable)
        ));
    }
    Ok(record)
}

impl ToolDisplay for DocumentLocateTool {
    fn display_name(&self) -> &str {
        "Locate"
    }
    fn category(&self) -> &str {
        "file"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        let source = args.get("source").and_then(Value::as_str).unwrap_or("?");
        match args.get("query").and_then(Value::as_str) {
            Some(q) => format!("{q:?} in {source}"),
            None => format!("outline of {source}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_document::testing::{build_pdf_with_outline, FixturePage};

    fn manual() -> Vec<u8> {
        let page = |t: &'static str| FixturePage::Text(vec![t]);
        build_pdf_with_outline(
            &[
                page("Contents"),
                page("USART overview"),
                page("USART_SR status register"),
                page("SPI"),
            ],
            &[
                (
                    1,
                    "1 Universal synchronous asynchronous receiver transmitter (USART)",
                    1,
                ),
                (2, "1.1 USART registers", 2),
                (1, "2 Serial peripheral interface (SPI)", 3),
            ],
        )
    }

    fn run(args: Value) -> Result<Value, String> {
        let request = Request::parse(&args)?;
        locate(manual(), &request)
    }

    #[test]
    fn a_chapter_query_returns_its_page_range_and_text_hits() {
        let record = run(json!({"source": "m.pdf", "query": "USART"})).unwrap();
        assert_eq!(record["outline_matches"][0]["pages"], "2-3");
        assert_eq!(record["text_hit_pages"], 2);
        assert_eq!(record["text_hits"][1]["count"], 1);
        assert!(record.get("next_offset").is_none());
    }

    #[test]
    fn without_a_query_the_outline_overview_comes_back() {
        let record = run(json!({"source": "m.pdf"})).unwrap();
        assert_eq!(record["outline_entries"], 3);
        assert_eq!(record["outline"][2]["pages"], "4-4");
        let top = run(json!({"source": "m.pdf", "depth": 1})).unwrap();
        assert_eq!(top["outline_entries"], 2);
    }

    #[test]
    fn regex_and_fzf_queries_and_a_page_range_narrow_the_search() {
        let regex =
            run(json!({"source": "m.pdf", "query": "usart_s[rt]", "syntax": "regex"})).unwrap();
        assert_eq!(regex["text_hits"][0]["page"], 3);
        let fzf = run(json!({"source": "m.pdf", "query": "^1.1 regs", "syntax": "fzf"})).unwrap();
        assert_eq!(fzf["outline_match_count"], 1);
        let narrowed = run(json!({"source": "m.pdf", "query": "usart", "pages": "3-4"})).unwrap();
        assert_eq!(narrowed["text_hit_pages"], 1);
        assert_eq!(narrowed["searched_pages"], 2);
    }

    #[test]
    fn results_come_a_page_at_a_time() {
        let first = run(json!({"source": "m.pdf", "query": "usart", "limit": 1})).unwrap();
        assert_eq!(first["text_hits"].as_array().unwrap().len(), 1);
        assert_eq!(first["next_offset"], 1);
        let second =
            run(json!({"source": "m.pdf", "query": "usart", "limit": 1, "offset": 1})).unwrap();
        assert_eq!(second["text_hits"][0]["page"], 3);
        assert!(second.get("next_offset").is_none());
    }

    #[test]
    fn a_misnamed_or_out_of_range_argument_is_refused() {
        for args in [
            json!({"source": "m.pdf", "regex": "usart"}),
            json!({"source": "m.pdf", "query": "usart", "syntax": "glob"}),
            json!({"source": "m.pdf", "query": "usart", "limit": 0}),
            json!({"source": "m.pdf", "query": "usart", "limit": "5"}),
            json!({"source": "m.pdf", "syntax": "regex"}),
            json!({"source": "m.pdf", "query": "(", "syntax": "regex"}),
        ] {
            assert!(Request::parse(&args).is_err(), "{args}");
        }
    }

    #[test]
    fn the_definition_stays_small() {
        let t = DocumentLocateTool::new(PathScope::unconfined());
        assert!(t.summary().chars().count() <= sven_tool_api::SUMMARY_MAX_CHARS);
        assert!(t.parameters_schema().to_string().len() < 1200);
    }

    #[test]
    fn page_lists_print_as_ranges() {
        assert_eq!(page_ranges(&[1, 2, 3, 7, 9, 10]), "1-3,7,9-10");
    }
}
