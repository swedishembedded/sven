// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping delegated agent work
// cheap and non-duplicative for its clients. If your team needs expertise in
// multi-agent context handoff then you can procure our services by sending an
// email to info@swedishembedded.com.
//! What a finished sub-agent hands back to its parent, and what that cost.
//!
//! [`Reporting::conclude`] turns the child's answer into the text the parent
//! is shown (the structured finding of a briefed child, otherwise the answer
//! as it is) and adds the child to the session's [`ChildLedger`]: the tokens
//! the child used, the size of what the parent receives, and the size of the
//! child's whole transcript, which is what forwarding it would have cost the
//! parent. The ledger is the parent's per-child account; the child's own
//! requests are still charged through `SubagentUpdate::TokensUsed`.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use sven_chunks::tokens_of;
use sven_executors::ChunkSession;
use sven_tool_api::events::SubagentUpdate;

use super::finding;

/// The bytes of a child's transcript an update carries: the text it said, the
/// calls it made and what they returned. Thinking is not forwarded to a
/// parent that would be handed the transcript, so it is not counted.
pub(super) fn transcript_bytes(update: &SubagentUpdate) -> usize {
    match update {
        SubagentUpdate::TextDelta(text) => text.len(),
        SubagentUpdate::ToolCallStarted { name, args, .. } => name.len() + args.to_string().len(),
        SubagentUpdate::ToolCallFinished { output, .. } => output.len(),
        _ => 0,
    }
}

/// What one finished child cost, in estimated tokens (bytes / 4) unless it
/// says provider-reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChildCost {
    pub(super) handle: String,
    pub(super) description: String,
    /// Provider-reported input and output tokens of the child's turn.
    pub(super) used: Option<(u64, u64)>,
    /// The parent's share: tokens of the text it was handed.
    pub(super) handed_back: u64,
    /// Tokens of the child's whole transcript, which forwarding it would have
    /// put in the parent's context.
    pub(super) transcript: u64,
}

impl ChildCost {
    /// Tokens the parent did not take on by receiving a result instead of the
    /// transcript.
    pub(super) fn saved(&self) -> u64 {
        self.transcript.saturating_sub(self.handed_back)
    }

    fn line(&self) -> String {
        let used = self.used.map_or_else(
            || "tokens not reported".to_string(),
            |(i, o)| format!("{i} input / {o} output tokens"),
        );
        format!(
            "child used {used}; handed back ~{} tokens; its transcript is ~{} (saved ~{})",
            self.handed_back,
            self.transcript,
            self.saved()
        )
    }
}

/// The session's account of its children, in the order they finished.
#[derive(Debug, Default)]
pub(super) struct ChildLedger {
    children: Mutex<Vec<ChildCost>>,
    /// Calls answered from a twin instead of launching a child.
    avoided: AtomicU32,
}

impl ChildLedger {
    /// A call was answered from a twin: no child was launched.
    pub(super) fn note_avoided(&self) {
        self.avoided.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record(&self, cost: ChildCost) {
        if let Ok(mut all) = self.children.lock() {
            all.push(cost);
        }
    }

    /// One line per child, the totals and the launches avoided.
    pub(super) fn render(&self) -> String {
        let all = self.children.lock().map(|a| a.clone()).unwrap_or_default();
        let mut out = String::new();
        if all.is_empty() {
            out.push_str("no sub-agent has finished in this session");
        } else {
            for c in &all {
                let _ = writeln!(out, "{} ({}): {}", c.handle, c.description, c.line());
            }
            let (given, transcript) = all
                .iter()
                .fold((0, 0), |(g, t), c| (g + c.handed_back, t + c.transcript));
            let _ = write!(
                out,
                "total: {} sub-agents; handed back ~{given} tokens of ~{transcript} (saved ~{})",
                all.len(),
                transcript.saturating_sub(given)
            );
        }
        let avoided = self.avoided.load(Ordering::Relaxed);
        if avoided > 0 {
            let _ = write!(
                out,
                "\nlaunches avoided as repeats of a read-only subgoal: {avoided}"
            );
        }
        out
    }
}

/// How a finished child's answer is handed back.
pub(super) struct Reporting {
    /// The child was briefed, so it was asked for a finding.
    pub(super) structured: bool,
    /// Where the child's whole answer is recorded.
    pub(super) chunks: Option<Arc<ChunkSession>>,
    pub(super) ledger: Arc<ChildLedger>,
}

/// The child being concluded.
pub(super) struct Child<'a> {
    pub(super) handle: &'a str,
    pub(super) description: &'a str,
    pub(super) used: Option<(u64, u64)>,
    pub(super) transcript_bytes: usize,
}

impl Reporting {
    /// The text the parent is shown for a child that finished with `answer`,
    /// and the cost line to append when the child was briefed. The child is
    /// entered in the ledger either way.
    pub(super) fn conclude(&self, child: &Child<'_>, answer: &str) -> (String, Option<String>) {
        let parsed = self.structured.then(|| finding::parse(answer)).flatten();
        let shown = match parsed {
            Some(mut found) => {
                let recorded = self
                    .chunks
                    .as_deref()
                    .and_then(|s| finding::record_answer(s, child.handle, answer));
                if let Some(s) = self.chunks.as_deref() {
                    finding::keep_known(&mut found, s);
                }
                finding::render(&found, recorded.as_ref())
            }
            None => answer.to_string(),
        };
        let cost = ChildCost {
            handle: child.handle.to_string(),
            description: child.description.to_string(),
            used: child.used,
            handed_back: u64::from(tokens_of(&shown)),
            transcript: u64::try_from(child.transcript_bytes.div_ceil(4)).unwrap_or(u64::MAX),
        };
        let line = self.structured.then(|| format!("Cost: {}", cost.line()));
        self.ledger.record(cost);
        (shown, line)
    }
}

#[cfg(test)]
mod tests {
    use sven_chunks::{ChunkStore, FsProbe, StoreLimits};

    use super::*;

    fn session() -> (Arc<ChunkSession>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = ChunkStore::open(&dir.path().join("s"), StoreLimits::default()).unwrap();
        let probe = FsProbe::new(dir.path()).unwrap();
        (
            Arc::new(ChunkSession::new(
                Arc::new(Mutex::new(store)),
                Arc::new(probe),
            )),
            dir,
        )
    }

    fn child(transcript_bytes: usize) -> Child<'static> {
        Child {
            handle: "buf_0001",
            description: "find callers",
            used: Some((1200, 80)),
            transcript_bytes,
        }
    }

    const ANSWER: &str =
        "long prose that the parent does not need\n```finding\n{\"summary\": \"run() calls it\"}\n```\n";

    #[test]
    fn a_briefed_childs_answer_becomes_its_finding_and_a_reference() {
        let (chunks, _dir) = session();
        let reporting = Reporting {
            structured: true,
            chunks: Some(chunks.clone()),
            ledger: Arc::default(),
        };
        let (shown, cost) = reporting.conclude(&child(40_000), ANSWER);
        assert!(shown.contains("Summary: run() calls it"), "{shown}");
        assert!(!shown.contains("long prose"), "{shown}");
        assert!(shown.contains("Whole answer: context"), "{shown}");
        assert_eq!(chunks.store().lock().unwrap().len(), 1);
        let cost = cost.unwrap();
        assert!(cost.contains("1200 input / 80 output"), "{cost}");
        assert!(cost.contains("transcript is ~10000"), "{cost}");
    }

    #[test]
    fn an_unbriefed_childs_answer_is_handed_back_as_it_is() {
        let reporting = Reporting {
            structured: false,
            chunks: None,
            ledger: Arc::default(),
        };
        let (shown, cost) = reporting.conclude(&child(100), ANSWER);
        assert_eq!(shown, ANSWER);
        assert_eq!(cost, None);
        assert!(reporting
            .ledger
            .render()
            .contains("buf_0001 (find callers)"));
    }

    #[test]
    fn a_briefed_child_that_gave_no_finding_is_reported_whole() {
        let (chunks, _dir) = session();
        let reporting = Reporting {
            structured: true,
            chunks: Some(chunks.clone()),
            ledger: Arc::default(),
        };
        let (shown, _) = reporting.conclude(&child(10), "just prose");
        assert_eq!(shown, "just prose");
        assert!(chunks.store().lock().unwrap().is_empty());
    }

    #[test]
    fn the_ledger_totals_what_the_parent_was_spared() {
        let ledger = ChildLedger::default();
        assert!(ledger.render().contains("no sub-agent"));
        for (h, handed, transcript) in [("a", 100, 1000), ("b", 50, 30)] {
            ledger.record(ChildCost {
                handle: h.into(),
                description: String::new(),
                used: None,
                handed_back: handed,
                transcript,
            });
        }
        let text = ledger.render();
        assert!(
            text.contains("total: 2 sub-agents; handed back ~150 tokens of ~1030 (saved ~880)"),
            "{text}"
        );
        assert!(text.contains("tokens not reported"));
    }

    #[test]
    fn avoided_launches_are_counted_in_the_account() {
        let ledger = ChildLedger::default();
        ledger.note_avoided();
        ledger.record(ChildCost {
            handle: "a".into(),
            description: String::new(),
            used: None,
            handed_back: 1,
            transcript: 2,
        });
        assert!(ledger
            .render()
            .contains("launches avoided as repeats of a read-only subgoal: 1"));
    }

    #[test]
    fn a_transcript_counts_what_was_said_called_and_returned() {
        assert_eq!(
            transcript_bytes(&SubagentUpdate::TextDelta("abcd".into())),
            4
        );
        assert_eq!(
            transcript_bytes(&SubagentUpdate::ToolCallFinished {
                id: "1".into(),
                name: "read_file".into(),
                output: "x".repeat(100),
                is_error: false
            }),
            100
        );
        assert_eq!(
            transcript_bytes(&SubagentUpdate::ThinkingDelta("hmm".into())),
            0
        );
    }
}
