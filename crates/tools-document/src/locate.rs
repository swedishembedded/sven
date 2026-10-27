// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `document_locate` tool: where something is in a PDF, without reading it.
//!
//! With a query it lists the outline entries (chapters, sections) whose title
//! matches, each with the page range it covers, and the pages whose text
//! contains the query; without one it lists the outline's top levels. Only
//! titles, page numbers and short snippets come back, so "convert the USART
//! chapter" costs one locate and one `document_convert` of those pages instead
//! of reading the manual through the chat model.
//!
//! Swedish Embedded AB implements agent tools that move bulk data around the
//! language model instead of through it. If your team needs expertise in
//! building cost-efficient document agents then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_document::{find_text, outline, outline_matches, HayroBackend, OutlineEntry, PdfBackend};
use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolDisplay, ToolOutput};
use sven_tool_api::PathScope;

/// Outline levels listed when no query is given.
const OVERVIEW_LEVELS: usize = 2;
/// Most outline entries or text hits returned.
const MAX_LISTED: usize = 40;

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
        "Find which pages of a PDF hold a chapter, section or term (outline + text search). Returns page numbers, not text."
    }

    fn description(&self) -> &str {
        "Find where something is in a PDF without reading it. With `query` (a chapter or section \
         title, or a term such as a register name) it returns the outline entries whose title \
         matches, each with its page range, and the pages whose text contains the query, with a \
         short snippet. Without `query` it returns the outline's top levels. Use it to pick the \
         `pages` for document_convert, e.g. to extract one chapter of a manual."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "source": {"type": "string", "description": "PDF file"},
                "query": {"type": "string", "description": "chapter/section title or term to find; omit for the outline"}
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
        let Some(source) = call.args.get("source").and_then(Value::as_str) else {
            return ToolOutput::err(&call.id, "Missing required parameter: source.");
        };
        let query = call
            .args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|q| !q.is_empty());
        let path = match self.scope.resolve_for(call, source) {
            Ok(p) => p,
            Err(refused) => return refused,
        };
        let data = match tokio::fs::read(&path).await {
            Ok(d) => d,
            Err(e) => {
                return ToolOutput::err(&call.id, format!("cannot read {}: {e}", path.display()))
            }
        };
        let query = query.map(str::to_owned);
        let located = tokio::task::spawn_blocking(move || locate(data, query.as_deref())).await;
        match located {
            Ok(Ok(record)) => ToolOutput::ok(&call.id, record.to_string()),
            Ok(Err(e)) => ToolOutput::err(&call.id, e),
            Err(e) => ToolOutput::err(&call.id, format!("locating failed: {e}")),
        }
    }
}

fn entry(e: &OutlineEntry) -> Value {
    json!({"level": e.level, "title": e.title, "pages": format!("{}-{}", e.first_page, e.last_page)})
}

/// The located record: outline matches with page ranges and text hits, or the
/// outline overview. Never page text beyond a snippet.
fn locate(data: Vec<u8>, query: Option<&str>) -> Result<Value, String> {
    let entries = outline(&data).map_err(|e| e.to_string())?;
    let Some(query) = query else {
        let top: Vec<Value> = entries
            .iter()
            .filter(|e| e.level <= OVERVIEW_LEVELS)
            .take(MAX_LISTED * 4)
            .map(entry)
            .collect();
        let note = if entries.is_empty() {
            "This PDF has no outline; pass `query` to search its text."
        } else {
            "Pass `query` to find a chapter or term; give a match's `pages` to document_convert."
        };
        return Ok(json!({"outline": top, "outline_entries": entries.len(), "note": note}));
    };
    let matches: Vec<Value> = outline_matches(&entries, query)
        .into_iter()
        .take(MAX_LISTED)
        .map(entry)
        .collect();
    let source = HayroBackend
        .open(Arc::new(data))
        .map_err(|e| e.to_string())?;
    let text = source.native_text();
    let (hits, total) = find_text(&text, query, MAX_LISTED);
    let pages_without_text = text.iter().filter(|t| t.is_none()).count();
    let mut record = json!({
        "query": query,
        "outline_matches": matches,
        "text_hits": hits.iter().map(|h| json!({"page": h.page, "snippet": h.snippet})).collect::<Vec<_>>(),
        "text_hit_pages": total,
        "page_count": text.len(),
    });
    if pages_without_text > 0 {
        // Say what the search could not see rather than imply the term is absent.
        record["warning"] = json!(format!(
            "{pages_without_text} page(s) have no usable text layer and were not searched (scanned pages need document_convert with OCR)"
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

    #[test]
    fn a_chapter_query_returns_its_page_range_and_text_hits() {
        let record = locate(manual(), Some("USART")).unwrap();
        assert_eq!(record["outline_matches"][0]["pages"], "2-3");
        assert_eq!(record["text_hit_pages"], 2);
        assert!(record.get("warning").is_none());
    }

    #[test]
    fn without_a_query_the_outline_overview_comes_back() {
        let record = locate(manual(), None).unwrap();
        assert_eq!(record["outline_entries"], 3);
        assert_eq!(record["outline"][2]["pages"], "4-4");
    }
}
