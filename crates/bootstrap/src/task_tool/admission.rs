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
