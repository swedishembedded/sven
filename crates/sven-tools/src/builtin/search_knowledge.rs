// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! `search_knowledge` — keyword search across project knowledge documents.

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_config::AgentMode;
use sven_runtime::SharedKnowledge;

use crate::policy::ApprovalPolicy;
use crate::tool::{OutputCategory, Tool, ToolCall, ToolOutput};

/// Number of context lines shown before and after each match.
const CONTEXT_LINES: usize = 3;
/// Maximum number of matches shown per document.
const MAX_MATCHES_PER_DOC: usize = 5;
/// Maximum number of documents shown in results.
const MAX_DOCS_IN_RESULTS: usize = 8;

/// Search project knowledge documents with keyword/substring matching.
///
/// Searches the body of every `.sven/knowledge/*.md` file (skipping YAML
/// frontmatter) for the given query string.  Results are sorted by match
/// count descending so the most relevant document appears first.
pub struct SearchKnowledgeTool {
    pub knowledge: SharedKnowledge,
}

#[async_trait]
impl Tool for SearchKnowledgeTool {
    fn name(&self) -> &str {
        "search_knowledge"
    }

    fn description(&self) -> &str {
        "Search project knowledge documents with a keyword query.\n\
         Returns: matching excerpts (with context lines) sorted by relevance.\n\
         Use before modifying a subsystem to retrieve architecture notes,\n\
         correctness invariants, and known failure-mode tables.\n\
         Use `list_knowledge` to see all available documents."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keyword or phrase to search for (case-insensitive substring match)"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Research, AgentMode::Plan, AgentMode::Agent]
    }

    fn output_category(&self) -> OutputCategory {
        OutputCategory::MatchList
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let query = match call.args.get("query").and_then(|v| v.as_str()) {
            Some(q) if !q.trim().is_empty() => q.trim().to_string(),
            _ => return ToolOutput::err(&call.id, "missing or empty 'query'"),
        };

        let docs = self.knowledge.get();

        if docs.is_empty() {
            return ToolOutput::ok(
                &call.id,
                "No knowledge documents found in `.sven/knowledge/`. \
                 Use `list_knowledge` for details on creating them.",
            );
        }

        let query_lower = query.to_lowercase();

        // Search each doc and collect (match_count, formatted_result).
        let mut results: Vec<(usize, String)> = docs
            .iter()
            .filter_map(|doc| {
                // Count actual matching lines (not excerpts, which merge close matches).
                let match_count = doc
                    .body
                    .lines()
                    .filter(|l| l.to_lowercase().contains(&query_lower))
                    .count();
                if match_count == 0 {
                    return None;
                }
                let excerpts =
                    extract_excerpts(&doc.body, &query_lower, CONTEXT_LINES, MAX_MATCHES_PER_DOC);
                if excerpts.is_empty() {
                    return None;
                }
                let filename = doc
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown.md");
                let updated_hint = doc
                    .updated
                    .as_deref()
                    .map(|d| format!(" (updated {d})"))
                    .unwrap_or_default();

                let mut block = vec![format!(
                    "### {} — `{}`{}  [{} match(es)]",
                    doc.subsystem, filename, updated_hint, match_count
                )];
                for excerpt in &excerpts {
                    block.push(String::new());
                    block.push(excerpt.clone());
                }

                Some((match_count, block.join("\n")))
            })
            .collect();

        if results.is_empty() {
            return ToolOutput::ok(
                &call.id,
                format!(
                    "No matches for `{query}` in {} knowledge document(s).\n\
                     Try `list_knowledge` to see available subsystems.",
                    docs.len()
                ),
            );
        }

        // Sort by descending match count so most relevant doc comes first.
        results.sort_by(|a, b| b.0.cmp(&a.0));
        results.truncate(MAX_DOCS_IN_RESULTS);

        let total_docs_searched = docs.len();
        let total_matches: usize = results.iter().map(|(c, _)| c).sum();
        let header = format!(
            "## Knowledge Search: `{query}`\n\
             Found {total_matches} match(es) in {} of {total_docs_searched} document(s):\n",
            results.len()
        );

        let mut lines = vec![header];
        lines.extend(results.into_iter().map(|(_, block)| block));
        lines.push(String::new());
        lines.push(
            "Use `read_file <path>` to load the full document, or `list_knowledge` to see all docs."
                .to_string(),
        );

        ToolOutput::ok(&call.id, lines.join("\n"))
    }
}

/// Extract matching excerpts from `body` for the given `query_lower`.
///
/// Returns up to `max_matches` excerpts, each consisting of `context_lines`
/// lines before and after the matching line.
fn extract_excerpts(
    body: &str,
    query_lower: &str,
    context_lines: usize,
    max_matches: usize,
) -> Vec<String> {
    let lines: Vec<&str> = body.lines().collect();
    let mut excerpts = Vec::new();

    // Track which lines are already covered to avoid overlapping excerpts.
    let mut covered_up_to: usize = 0;

    for (i, line) in lines.iter().enumerate() {
        if line.to_lowercase().contains(query_lower) {
            let start = i.saturating_sub(context_lines);
            let end = (i + context_lines + 1).min(lines.len());

            // Skip if this match overlaps with the previous excerpt.
            if start < covered_up_to && !excerpts.is_empty() {
                // Extend the previous excerpt rather than creating a new one.
                // (Simply skip; the context already covered this match.)
                continue;
            }
            covered_up_to = end;

            let excerpt_lines: Vec<String> = lines[start..end]
                .iter()
                .enumerate()
                .map(|(j, l)| {
                    let line_num = start + j + 1;
                    let marker = if start + j == i { ">" } else { " " };
                    format!("{marker} {line_num:4} │ {l}")
                })
                .collect();

            excerpts.push(format!("```\n{}\n```", excerpt_lines.join("\n")));

            if excerpts.len() >= max_matches {
                break;
            }
        }
    }

    excerpts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Tool, ToolCall};
    use serde_json::json;
    use sven_runtime::KnowledgeInfo;

    fn call(query: &str) -> ToolCall {
        ToolCall {
            id: "sk1".into(),
            name: "search_knowledge".into(),
            args: json!({ "query": query }),
        }
    }

    fn make_doc(subsystem: &str, body: &str) -> KnowledgeInfo {
        KnowledgeInfo {
            subsystem: subsystem.to_string(),
            files: vec![],
            updated: None,
            path: std::path::PathBuf::from(format!(
                ".sven/knowledge/{}.md",
                subsystem.to_lowercase().replace(' ', "-")
            )),
            body: body.to_string(),
        }
    }

    #[tokio::test]
    async fn missing_query_is_error() {
        let t = SearchKnowledgeTool {
            knowledge: SharedKnowledge::empty(),
        };
        let out = t
            .execute(&ToolCall {
                id: "x".into(),
                name: "search_knowledge".into(),
                args: json!({}),
            })
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("missing"));
    }

    #[tokio::test]
    async fn empty_knowledge_base() {
        let t = SearchKnowledgeTool {
            knowledge: SharedKnowledge::empty(),
        };
        let out = t.execute(&call("relay")).await;
        assert!(!out.is_error);
        assert!(out.content.contains("No knowledge documents found"));
    }

    #[tokio::test]
    async fn no_match_returns_informative_message() {
        let t = SearchKnowledgeTool {
            knowledge: SharedKnowledge::new(vec![make_doc(
                "P2P",
                "## Architecture\n\nThe node uses mDNS for discovery.",
            )]),
        };
        let out = t.execute(&call("raft")).await;
        assert!(!out.is_error);
        assert!(out.content.contains("No matches for"));
        assert!(out.content.contains("list_knowledge"));
    }

    #[tokio::test]
    async fn finds_match_with_context() {
        let body = "line 1\nline 2\nThe relay handles routing\nline 4\nline 5";
        let t = SearchKnowledgeTool {
            knowledge: SharedKnowledge::new(vec![make_doc("P2P", body)]),
        };
        let out = t.execute(&call("relay")).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("P2P"));
        assert!(out.content.contains("relay"));
        assert!(out.content.contains("1 match"));
    }

    #[tokio::test]
    async fn case_insensitive_search() {
        let body = "The RELAY_TIMEOUT constant controls reconnect backoff.";
        let t = SearchKnowledgeTool {
            knowledge: SharedKnowledge::new(vec![make_doc("P2P", body)]),
        };
        let out = t.execute(&call("relay_timeout")).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("P2P"));
    }

    #[tokio::test]
    async fn results_sorted_by_match_count() {
        let doc_few = make_doc("Alpha", "only one mention of target here");
        let doc_many = make_doc(
            "Beta",
            "target here\ntarget there\ntarget everywhere\ntarget again",
        );
        let t = SearchKnowledgeTool {
            knowledge: SharedKnowledge::new(vec![doc_few, doc_many]),
        };
        let out = t.execute(&call("target")).await;
        assert!(!out.is_error, "{}", out.content);
        // Beta (more matches) should appear before Alpha.
        let beta_pos = out.content.find("Beta").unwrap();
        let alpha_pos = out.content.find("Alpha").unwrap();
        assert!(
            beta_pos < alpha_pos,
            "Beta (more matches) should rank first"
        );
    }

    #[test]
    fn extract_excerpts_returns_context_window() {
        let body = "a\nb\nc\nmatch line\nd\ne\nf";
        let excerpts = extract_excerpts(body, "match", 2, 5);
        assert_eq!(excerpts.len(), 1);
        let text = &excerpts[0];
        assert!(
            text.contains(">"),
            "matching line should be marked with '>'"
        );
        assert!(text.contains("match line"));
        // context lines before and after
        assert!(text.contains("b"));
        assert!(text.contains("d"));
    }

    #[test]
    fn extract_excerpts_no_match_returns_empty() {
        let body = "nothing relevant here\nor here";
        let result = extract_excerpts(body, "missing", 3, 5);
        assert!(result.is_empty());
    }
}
