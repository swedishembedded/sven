// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping delegated agent work
// cheap and non-duplicative for its clients. If your team needs expertise in
// multi-agent context handoff then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The structured result of a briefed sub-agent.
//!
//! A child that was briefed (see [`super::brief`]) is told to end its answer
//! with one fenced `finding` block: a summary, the briefed chunks the answer
//! rests on, the files it changed and what it could not settle. The parent is
//! shown that, not the child's whole answer; the whole answer is recorded as a
//! chunk of the parent's store, with the child's handle as its source, and can
//! be expanded. A child that does not produce the block is reported exactly as
//! an unbriefed one is.
//!
//! Parsing and rendering are pure. The child's text is untrusted: ids are
//! validated, every field is bounded, and nothing in it is interpreted.

use std::fmt::Write as _;

use serde::Deserialize;
use sven_chunks::{
    content_digest, ChunkDraft, ChunkId, ChunkKind, Lifetime, Provenance, Sensitivity,
    EXPAND_ACTION, EXPAND_TOOL_NAME,
};
use sven_executors::ChunkSession;

/// Longest summary, in characters.
const MAX_SUMMARY_CHARS: usize = 2_000;
/// Most entries in each list.
const MAX_LIST: usize = 20;
/// Longest list entry, in characters.
const MAX_ENTRY_CHARS: usize = 300;

/// What the child is told to end with. Appended to every brief.
pub(super) const REPORTING_INSTRUCTIONS: &str = "\n## Reporting\n\
End your final answer with one fenced block labelled `finding` holding a JSON object: \
`summary` (what you found, in a few sentences), `evidence` (ids of the evidence chunks above \
that your answer rests on), `files_touched` (files you changed; empty if you only read) and \
`open_questions` (what you could not settle). Anything you write before the block is kept \
for the parent to expand, not shown to it.\n\
```finding\n{\"summary\": \"...\", \"evidence\": [], \"files_touched\": [], \"open_questions\": []}\n```\n";

/// A child's finding, bounded and validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Finding {
    pub(super) summary: String,
    pub(super) evidence: Vec<ChunkId>,
    pub(super) files_touched: Vec<String>,
    pub(super) open_questions: Vec<String>,
}

#[derive(Deserialize)]
struct Raw {
    summary: String,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    files_touched: Vec<String>,
    #[serde(default)]
    open_questions: Vec<String>,
}

fn clip(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        let cut: String = text.chars().take(limit).collect();
        format!("{cut}…")
    }
}

fn entries(list: Vec<String>) -> Vec<String> {
    list.iter()
        .map(|e| clip(e, MAX_ENTRY_CHARS))
        .filter(|e| !e.is_empty())
        .take(MAX_LIST)
        .collect()
}

/// The finding in `final_text`: the last `finding` fenced block that holds a
/// JSON object with a non-empty summary. `None` when there is none.
pub(super) fn parse(final_text: &str) -> Option<Finding> {
    const OPEN: &str = "```finding";
    let mut found = None;
    let mut rest = final_text;
    while let Some(at) = rest.find(OPEN) {
        let after = &rest[at + OPEN.len()..];
        let Some(body_start) = after.find('\n') else {
            break;
        };
        let body = &after[body_start + 1..];
        let Some(end) = body.find("\n```") else {
            break;
        };
        if let Ok(raw) = serde_json::from_str::<Raw>(&body[..end]) {
            if !raw.summary.trim().is_empty() {
                found = Some(raw);
            }
        }
        rest = &body[end + 4..];
    }
    let raw = found?;
    let mut evidence: Vec<ChunkId> = Vec::new();
    for text in raw.evidence.iter().take(MAX_LIST * 2) {
        if let Ok(id) = ChunkId::parse(text) {
            if !evidence.contains(&id) {
                evidence.push(id);
            }
        }
    }
    evidence.truncate(MAX_LIST);
    Some(Finding {
        summary: clip(&raw.summary, MAX_SUMMARY_CHARS),
        evidence,
        files_touched: entries(raw.files_touched),
        open_questions: entries(raw.open_questions),
    })
}

/// What the parent is shown for `finding`; `full_answer` is the chunk that
/// holds the child's whole answer, when it was recorded.
pub(super) fn render(finding: &Finding, full_answer: Option<&ChunkId>) -> String {
    let mut out = format!("--- Finding ---\nSummary: {}\n", finding.summary);
    if !finding.evidence.is_empty() {
        let ids: Vec<String> = finding.evidence.iter().map(ToString::to_string).collect();
        let _ = writeln!(out, "Evidence: {}", ids.join(", "));
    }
    if finding.files_touched.is_empty() {
        out.push_str("Files touched: none\n");
    } else {
        let _ = writeln!(out, "Files touched: {}", finding.files_touched.join(", "));
    }
    if !finding.open_questions.is_empty() {
        out.push_str("Open questions:\n");
        for question in &finding.open_questions {
            let _ = writeln!(out, "- {question}");
        }
    }
    if let Some(id) = full_answer {
        let _ = writeln!(
            out,
            "Whole answer: {EXPAND_TOOL_NAME} {{\"action\":\"{EXPAND_ACTION}\",\"chunk\":\"{id}\"}}"
        );
    }
    out
}

/// Records the child's whole answer in the parent's store, with the child
/// that produced it as its source. `None` when the store refuses it: the
/// parent is then shown the finding without a reference to expand.
pub(super) fn record_answer(
    session: &ChunkSession,
    handle_id: &str,
    answer: &str,
) -> Option<ChunkId> {
    let draft = ChunkDraft {
        kind: ChunkKind::SubagentFinding,
        provenance: Provenance {
            source_tool: format!("task:{handle_id}"),
            args_digest: content_digest(handle_id.as_bytes()),
            path: None,
            source_version: None,
            turn: 0,
        },
        priority: 1,
        sensitivity: Sensitivity::Internal,
        lifetime: Lifetime::Session,
        pinned: false,
        failure: false,
    };
    let mut store = session.store().lock().ok()?;
    match store.put(answer, draft) {
        Ok(outcome) => Some(outcome.chunk.id),
        Err(e) => {
            tracing::warn!(error = %e, "the sub-agent's answer was not recorded as a chunk");
            None
        }
    }
}

/// Keeps the evidence ids of `finding` that the parent's store holds: an id
/// the child invented is dropped, not shown as a reference.
pub(super) fn keep_known(finding: &mut Finding, session: &ChunkSession) {
    if let Ok(store) = session.store().lock() {
        finding.evidence.retain(|id| store.get(id).is_some());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use sven_chunks::{ChunkStore, FsProbe, StoreLimits};

    use super::*;

    fn block(json: &str) -> String {
        format!("I looked.\n```finding\n{json}\n```\n")
    }

    #[test]
    fn the_last_valid_finding_block_is_the_finding() {
        let text = format!(
            "{}{}{}",
            block(r#"{"summary": "first"}"#),
            block(r#"{"summary": 7}"#),
            block(
                r#"{"summary": "the caller is run()", "evidence": ["c-0123456789abcdef", "../x", "c-0123456789abcdef"],
                   "files_touched": ["src/a.rs"], "open_questions": ["is it async?", ""]}"#
            )
        );
        let f = parse(&text).unwrap();
        assert_eq!(f.summary, "the caller is run()");
        assert_eq!(f.evidence, [ChunkId::parse("c-0123456789abcdef").unwrap()]);
        assert_eq!(f.files_touched, ["src/a.rs"]);
        assert_eq!(f.open_questions, ["is it async?"]);
    }

    #[test]
    fn text_without_a_usable_block_has_no_finding() {
        for text in [
            "plain answer",
            "```finding\nnot json\n```",
            "```finding\n{\"summary\": \"  \"}\n```",
            "```finding\n{\"summary\": \"unterminated\"}",
            "```json\n{\"summary\": \"wrong label\"}\n```",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }

    #[test]
    fn every_field_is_bounded() {
        let many: Vec<String> = (0..100).map(|n| format!("q{n}")).collect();
        let json = serde_json::json!({
            "summary": "s".repeat(10_000),
            "open_questions": many,
            "files_touched": ["f".repeat(1_000)],
        });
        let f = parse(&block(&json.to_string())).unwrap();
        assert!(f.summary.chars().count() <= MAX_SUMMARY_CHARS + 1);
        assert_eq!(f.open_questions.len(), MAX_LIST);
        assert!(f.files_touched[0].chars().count() <= MAX_ENTRY_CHARS + 1);
    }

    #[test]
    fn the_parent_sees_the_summary_and_references_not_the_answer() {
        let finding = Finding {
            summary: "the caller is run()".into(),
            evidence: vec![ChunkId::parse("c-0123456789abcdef").unwrap()],
            files_touched: vec![],
            open_questions: vec!["is it async?".into()],
        };
        let id = ChunkId::parse("c-fedcba9876543210").unwrap();
        let shown = render(&finding, Some(&id));
        assert!(shown.contains("Summary: the caller is run()"));
        assert!(shown.contains("Evidence: c-0123456789abcdef"));
        assert!(shown.contains("Files touched: none"));
        assert!(shown.contains("- is it async?"));
        assert!(shown.contains("\"chunk\":\"c-fedcba9876543210\""));
        assert!(!render(&finding, None).contains("Whole answer"));
    }

    fn session() -> (ChunkSession, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = ChunkStore::open(&dir.path().join("s"), StoreLimits::default()).unwrap();
        let probe = FsProbe::new(dir.path()).unwrap();
        (
            ChunkSession::new(Arc::new(Mutex::new(store)), Arc::new(probe)),
            dir,
        )
    }

    #[test]
    fn the_whole_answer_is_recorded_with_the_child_as_its_source() {
        let (s, _dir) = session();
        let id = record_answer(&s, "buf_0007", "long prose\n```finding\n{}\n```").unwrap();
        let store = s.store().lock().unwrap();
        let chunk = store.get(&id).unwrap();
        assert_eq!(chunk.kind, ChunkKind::SubagentFinding);
        assert_eq!(chunk.provenance.source_tool, "task:buf_0007");
    }

    #[test]
    fn evidence_the_parent_never_recorded_is_dropped() {
        let (s, _dir) = session();
        let known = record_answer(&s, "h", "x").unwrap();
        let mut f = Finding {
            summary: "s".into(),
            evidence: vec![known.clone(), ChunkId::parse("c-ffffffffffffffff").unwrap()],
            files_touched: vec![],
            open_questions: vec![],
        };
        keep_known(&mut f, &s);
        assert_eq!(f.evidence, [known]);
    }
}
