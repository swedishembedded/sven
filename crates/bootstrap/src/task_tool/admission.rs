// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping delegated agent work
// cheap and non-duplicative for its clients. If your team needs expertise in
// multi-agent context handoff then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Whether a `task` call launches a child or is answered from a twin: the
//! call's side of [`subgoals`](super::subgoals).

use serde_json::Value;
use sven_chunks::ChunkId;
use sven_tool_api::{PathScope, ToolOutput};
use tracing::debug;

use super::subgoals::{
    joined_text, parse_paths, reused_text, Access, Admission, Leader, Subgoal, SubgoalSpec,
};
use super::TaskTool;

/// The call whose subgoal is being admitted.
pub(super) struct Call<'a> {
    pub(super) id: &'a str,
    pub(super) args: &'a Value,
    /// The directory tree the child is held inside; declared paths are read
    /// relative to it.
    pub(super) scope: &'a PathScope,
    /// The mode as asked, and the ACP mode it resolves to.
    pub(super) mode: &'a str,
    pub(super) acp_mode: &'a str,
    /// The `prompt` argument, before any brief was built from it.
    pub(super) prompt: &'a str,
    pub(super) deliverable: Option<&'a str>,
    pub(super) evidence: &'a [ChunkId],
}

impl TaskTool {
    /// The leader of a child to launch for `call`, or the answer to return
    /// instead: an error for a malformed `paths`, or the result of a twin.
    ///
    /// A read-only subgoal that repeats one already handed out is answered
    /// by reference; a writing one always launches.
    pub(super) async fn admit(&self, call: &Call<'_>) -> Result<Leader, ToolOutput> {
        let paths = parse_paths(call.args).map_err(|e| ToolOutput::err(call.id, e))?;
        let subgoal = Subgoal::new(&SubgoalSpec {
            mode: call.mode,
            prompt: call.prompt,
            deliverable: call.deliverable,
            paths: &paths,
            evidence: call.evidence,
            access: if call.acp_mode == "agent" {
                Access::Write
            } else {
                Access::ReadOnly
            },
        });
        loop {
            let advisor = self.near_match.as_deref();
            let admission = self
                .subgoals
                .admit(subgoal.clone(), call.scope, advisor)
                .await;
            if !matches!(admission, Admission::Launch(_)) {
                debug!(subgoal = %subgoal.digest(), "task: a repeat of a read-only subgoal");
            }
            match admission {
                Admission::Launch(leader) => return Ok(leader),
                Admission::Reuse(done) => {
                    self.ledger.note_avoided();
                    return Err(ToolOutput::ok(call.id, reused_text(&done)));
                }
                Admission::Join(mut finished) => {
                    if let Ok(done) = finished.wait_for(Option::is_some).await {
                        if let Some(done) = done.clone() {
                            self.ledger.note_avoided();
                            return Err(ToolOutput::ok(call.id, joined_text(&done)));
                        }
                    }
                    // The twin failed or was cancelled: ask again.
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;
    use sven_tool_api::tool::{Tool, ToolCall};
    use sven_tools_fs::OutputBufferStore;
    use sven_vocab::AgentMode;
    use sven_workspace::SharedAgents;
    use tokio::sync::{mpsc, Mutex};

    use super::super::subgoals::NearMatch;
    use super::*;

    /// An advisor that would say "same" to everything: nothing may reach it
    /// for a writing subgoal.
    struct AlwaysSame;

    #[async_trait]
    impl NearMatch for AlwaysSame {
        async fn same_goal(&self, _: &str, _: &str) -> Option<bool> {
            panic!("a writing subgoal was put to the advisor");
        }
    }

    fn task(parent: AgentMode) -> TaskTool {
        let (tx, _rx) = mpsc::channel(8);
        TaskTool::new(
            Arc::new(Mutex::new(OutputBufferStore::new())),
            tx,
            None,
            SharedAgents::empty(),
            Arc::new(Mutex::new(parent)),
        )
        .with_near_match(Some(Arc::new(AlwaysSame)))
    }

    fn admit_call<'a>(
        args: &'a Value,
        scope: &'a PathScope,
        acp_mode: &'a str,
        prompt: &'a str,
    ) -> Call<'a> {
        Call {
            id: "t",
            args,
            scope,
            mode: acp_mode,
            acp_mode,
            prompt,
            deliverable: None,
            evidence: &[],
        }
    }

    #[tokio::test]
    async fn two_writing_subgoals_with_overlapping_scope_are_both_launched() {
        let task = task(AgentMode::Agent);
        let scope = PathScope::confined(std::env::current_dir().unwrap()).unwrap();
        let args = json!({"paths": ["src/parser.rs"]});
        let first = task
            .admit(&admit_call(&args, &scope, "agent", "fix the parser"))
            .await;
        let second = task
            .admit(&admit_call(&args, &scope, "agent", "fix the parser"))
            .await;
        let third = task
            .admit(&admit_call(&args, &scope, "agent", "fix the parser now"))
            .await;
        assert!(first.is_ok() && second.is_ok() && third.is_ok());
    }

    #[tokio::test]
    async fn a_reader_is_not_answered_from_a_writer_nor_a_writer_from_a_reader() {
        let task = task(AgentMode::Agent);
        let scope = PathScope::confined(std::env::current_dir().unwrap()).unwrap();
        let args = json!({"paths": ["src/parser.rs"]});
        let writer = task
            .admit(&admit_call(&args, &scope, "agent", "review the parser"))
            .await;
        let reader = task
            .admit(&admit_call(&args, &scope, "research", "review the parser"))
            .await;
        let writer_again = task
            .admit(&admit_call(&args, &scope, "agent", "review the parser"))
            .await;
        assert!(writer.is_ok() && reader.is_ok() && writer_again.is_ok());
    }

    /// The permission rule that keeps a child within its parent still
    /// applies: deduplication never makes a writing child possible for a
    /// read-only parent.
    #[tokio::test]
    async fn a_read_only_parent_still_cannot_start_a_writer() {
        for parent in [AgentMode::Research, AgentMode::Plan] {
            let out = task(parent)
                .execute(&ToolCall {
                    id: "t".into(),
                    name: "task".into(),
                    args: json!({"prompt": "fix the parser", "mode": "agent", "paths": ["src/p.rs"]}),
                })
                .await;
            assert!(
                out.is_error && out.content.contains("cannot delegate"),
                "{}",
                out.content
            );
        }
    }
}
