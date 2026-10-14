// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping delegated agent work
// cheap and non-duplicative for its clients. If your team needs expertise in
// multi-agent context handoff then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The call's side of [`voi`](super::voi): what `delegation.policy` does with
//! the estimate.

use sven_chunks::tokens_of;
use sven_tool_api::{PathScope, ToolOutput};

use super::subgoals::parse_paths;
use super::voi::{assess, declared_work_tokens, Subtask, Weighed};
use super::TaskTool;
use crate::config::DelegationPolicy;

/// The call being weighed.
pub(super) struct Candidate<'a> {
    pub(super) id: &'a str,
    pub(super) args: &'a serde_json::Value,
    pub(super) scope: &'a PathScope,
    /// The child's first request.
    pub(super) brief: &'a str,
    /// The `prompt` argument, which a declined call is told to do itself.
    pub(super) prompt: &'a str,
    pub(super) writes: bool,
}

/// What `delegation.policy` says to do with a call.
pub(super) enum Verdict {
    /// Launch it. Under `advise`, with a note to add to the result when the
    /// estimate favoured doing the work inline.
    Launch { note: Option<String> },
    /// Do not launch; this is the answer.
    Decline(ToolOutput),
}

impl TaskTool {
    /// What to do about `call` under `delegation.policy`. `off` (the default)
    /// launches without estimating anything.
    pub(super) fn weigh(&self, call: &Candidate<'_>) -> Verdict {
        let policy = self.delegation.policy;
        if policy == DelegationPolicy::Off {
            return Verdict::Launch { note: None };
        }
        // A malformed `paths` was already refused when the call was admitted.
        let paths = parse_paths(call.args).unwrap_or_default();
        let parent_context = self.chunks.as_deref().map_or(
            self.delegation.assumed_parent_context_tokens,
            |session| {
                session.store().lock().map_or(
                    self.delegation.assumed_parent_context_tokens,
                    |store| {
                        store
                            .chunks()
                            .map(|c| u64::from(c.token_estimate))
                            .sum::<u64>()
                    },
                )
            },
        );
        let weighed = assess(
            &self.delegation,
            &self.ledger.observed(self.delegation.min_samples),
            &Subtask {
                brief_tokens: u64::from(tokens_of(call.brief)),
                inline_work_tokens: declared_work_tokens(&paths, call.scope),
                parent_context_tokens: parent_context,
                independent_subgoals: self.subgoals.in_flight().max(1),
                writes: call.writes,
            },
        );
        if weighed.worth_it() {
            return Verdict::Launch { note: None };
        }
        match policy {
            DelegationPolicy::Enforce => {
                Verdict::Decline(ToolOutput::ok(call.id, declined(&weighed, call.prompt)))
            }
            _ => Verdict::Launch {
                note: Some(format!(
                    "Delegation note: {}; doing this subtask inline would have been cheaper.",
                    weighed.explain()
                )),
            },
        }
    }
}

fn declined(weighed: &Weighed, prompt: &str) -> String {
    format!(
        "Not delegated: {}. Do this subtask yourself, in this conversation:\n\n{prompt}",
        weighed.explain()
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use sven_tools_fs::OutputBufferStore;
    use sven_vocab::AgentMode;
    use sven_workspace::SharedAgents;
    use tokio::sync::{mpsc, Mutex};

    use super::*;
    use crate::config::DelegationConfig;

    fn task(policy: DelegationPolicy) -> TaskTool {
        let (tx, _rx) = mpsc::channel(8);
        TaskTool::new(
            Arc::new(Mutex::new(OutputBufferStore::new())),
            tx,
            None,
            SharedAgents::empty(),
            Arc::new(Mutex::new(AgentMode::Agent)),
        )
        .with_delegation(DelegationConfig {
            policy,
            ..DelegationConfig::default()
        })
    }

    fn verdict(task: &TaskTool, dir: &std::path::Path, args: &serde_json::Value) -> Verdict {
        let scope = PathScope::confined(dir).unwrap();
        task.weigh(&Candidate {
            id: "t",
            args,
            scope: &scope,
            brief: "read a.txt",
            prompt: "read a.txt",
            writes: false,
        })
    }

    fn tiny_file() -> (tempfile::TempDir, serde_json::Value) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one line").unwrap();
        (dir, json!({"paths": ["a.txt"]}))
    }

    #[test]
    fn off_launches_without_estimating() {
        let (dir, args) = tiny_file();
        assert!(matches!(
            verdict(&task(DelegationPolicy::Off), dir.path(), &args),
            Verdict::Launch { note: None }
        ));
    }

    #[test]
    fn advise_launches_and_says_the_work_was_cheaper_inline() {
        let (dir, args) = tiny_file();
        let Verdict::Launch { note: Some(note) } =
            verdict(&task(DelegationPolicy::Advise), dir.path(), &args)
        else {
            panic!("advise annotates a trivial subtask")
        };
        assert!(note.contains("inline would have been cheaper"), "{note}");
    }

    #[test]
    fn enforce_declines_a_trivial_subtask_with_the_reason_and_the_instruction() {
        let (dir, args) = tiny_file();
        let Verdict::Decline(out) = verdict(&task(DelegationPolicy::Enforce), dir.path(), &args)
        else {
            panic!("enforce declines a trivial subtask")
        };
        assert!(!out.is_error);
        assert!(out.content.starts_with("Not delegated:"), "{}", out.content);
        assert!(
            out.content.contains("Do this subtask yourself"),
            "{}",
            out.content
        );
        assert!(out.content.contains("read a.txt"));
    }

    #[test]
    fn a_subtask_that_is_worth_it_launches_under_every_policy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big.txt"), "x".repeat(200_000)).unwrap();
        let args = json!({"paths": ["big.txt"]});
        for policy in [DelegationPolicy::Advise, DelegationPolicy::Enforce] {
            assert!(
                matches!(
                    verdict(&task(policy), dir.path(), &args),
                    Verdict::Launch { note: None }
                ),
                "{policy:?}"
            );
        }
        // Undeclared work is assumed large enough to delegate.
        assert!(matches!(
            verdict(&task(DelegationPolicy::Enforce), dir.path(), &json!({})),
            Verdict::Launch { note: None }
        ));
    }
}
