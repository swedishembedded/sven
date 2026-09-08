// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Persist and restore subagent ("task" tool) sessions as ATIF
//! `subagent_trajectories` embedded in the parent's trajectory file - the
//! same on-disk shape [`sven_ci::runner::event::finalize_subagent_child`]
//! already produces for headless runs, so a TUI session's subagent
//! transcripts survive process exit instead of dying in memory (a subagent
//! `SessionEntry` used to be created with `session_path: None` and never
//! saved unless the user happened to switch into it).

use sven_session_store::SessionId;

use crate::{
    app::{
        chat_state::ChatState,
        session_manager::{SessionEntry, SessionManager},
    },
    chat::segment::ChatSegment,
};

impl SessionEntry {
    /// Build this subagent entry's embedded-child trajectory, or `None` if
    /// there is nothing to persist yet (no `SubagentEvent` has arrived and
    /// there is no spawn prompt to fall back on).
    ///
    /// Unlike [`Self::to_trajectory`] (built for the active/root session,
    /// which is never itself embedded), this also stamps `trajectory_id`:
    /// `atif::validate::validate_embedded_subagents` requires every entry
    /// under `Trajectory.subagent_trajectories` to carry one, unique among
    /// siblings. This entry's own [`SessionId`], stable across saves (see
    /// [`restore_subagent_children`]), already satisfies both.
    pub fn to_child_trajectory(
        &self,
        model: Option<String>,
        mode: Option<String>,
    ) -> Option<atif::Trajectory> {
        let chat = if let Some(mut chat) = self.stored_chat.clone() {
            // `to_trajectory` has no `ChatSegment::Error` arm and silently
            // drops it; record the failure as a plain assistant message
            // instead, mirroring the CI runner's own failure marker
            // (`sven_ci::runner::event::finalize_subagent_child`).
            for seg in &mut chat.segments {
                if let ChatSegment::Error(reason) = seg {
                    *seg = ChatSegment::Message(sven_model::Message::assistant(format!(
                        "(subagent failed: {reason})"
                    )));
                }
            }
            chat
        } else if let Some(prompt) = &self.initial_prompt {
            // No `SubagentEvent` has arrived yet (e.g. still starting, or it
            // failed before its first event) - persist the spawn prompt
            // rather than silently dropping the subagent.
            let mut chat = ChatState::new();
            chat.segments = vec![ChatSegment::Message(sven_model::Message::user(prompt))];
            chat
        } else {
            return None;
        };

        let mut trajectory = self.to_trajectory(&chat, model, mode);
        trajectory.trajectory_id = Some(self.id.as_str().to_string());
        Some(trajectory)
    }
}

/// Build the ATIF [`atif::Trajectory`] for the currently active session -
/// either via its registered [`SessionEntry::to_trajectory`], or (rare: the
/// active entry isn't found) reconstructed directly from `chat`'s segments -
/// then embeds any subagent children (see [`embed_children`]).
///
/// Moved out of `chat_ops` (a rendering-focused module) since it is entirely
/// about trajectory construction, which belongs alongside the subagent
/// embedding it now also does.
pub fn build_active_trajectory(
    sessions: &SessionManager,
    chat: &ChatState,
    chat_title: &str,
    model: Option<String>,
    mode: Option<String>,
) -> atif::Trajectory {
    let active_id = sessions.active_id.clone();
    let trajectory = if let Some(entry) = sessions.get(&active_id) {
        entry.to_trajectory(chat, model.clone(), mode.clone())
    } else {
        // Fallback for the rare case where the active entry isn't found.
        let records: Vec<sven_session_store::ConversationRecord> = chat
            .segments
            .iter()
            .filter_map(|seg| match seg {
                ChatSegment::Message(m) => {
                    Some(sven_session_store::ConversationRecord::Message(m.clone()))
                }
                ChatSegment::Thinking { content } => {
                    Some(sven_session_store::ConversationRecord::Thinking {
                        content: content.clone(),
                    })
                }
                ChatSegment::ContextCompacted {
                    tokens_before,
                    tokens_after,
                    strategy,
                    turn,
                } => Some(sven_session_store::ConversationRecord::ContextCompacted {
                    tokens_before: *tokens_before,
                    tokens_after: *tokens_after,
                    strategy: Some(strategy.to_string()),
                    turn: Some(*turn),
                }),
                _ => None,
            })
            .collect();
        let steps = sven_session_store::conversation_records_to_steps(&records);
        let mut agent = sven_session_store::default_agent_profile();
        if let Some(m) = &model {
            agent = agent.with_model(m.clone());
        }
        let mut trajectory = atif::Trajectory::new(sven_session_store::ATIF_SCHEMA_VERSION, agent);
        trajectory.session_id = Some(active_id.as_str().to_string());
        trajectory.steps = steps;
        let meta = sven_session_store::SvenSessionMeta {
            title: chat_title.to_string(),
            status: sven_session_store::ChatStatus::Active,
            mode: mode.clone(),
            parent_session_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        meta.apply_to_trajectory(&mut trajectory);
        trajectory
    };
    embed_children(sessions, &active_id, trajectory, model, mode)
}

/// Fill in `trajectory.subagent_trajectories` from `active_id`'s children in
/// `sessions`, then return it. A no-op (returns `trajectory` unchanged) when
/// there are no children or none of them has anything to persist yet.
pub fn embed_children(
    sessions: &SessionManager,
    active_id: &SessionId,
    mut trajectory: atif::Trajectory,
    model: Option<String>,
    mode: Option<String>,
) -> atif::Trajectory {
    if let Some(child_ids) = sessions.children.get(active_id) {
        let children: Vec<atif::Trajectory> = child_ids
            .iter()
            .filter_map(|id| sessions.get(id))
            .filter_map(|entry| entry.to_child_trajectory(model.clone(), mode.clone()))
            .collect();
        if !children.is_empty() {
            trajectory.subagent_trajectories = Some(children);
        }
    }
    trajectory
}

/// Restore subagent children embedded in a loaded parent trajectory into
/// `mgr`, under `parent_id`.
///
/// Skips any child already present (e.g. re-opening a session already
/// resumed once in this process) and any without a `trajectory_id`
/// (required by `atif::validate` for every embedded child, so its absence
/// means the entry didn't come from [`SessionEntry::to_child_trajectory`]).
///
/// `buffer_handle` is deliberately left `None`: `OutputBufferStore` restarts
/// its id counter from `buf_0001` every process run, so reusing a persisted
/// handle here could route a genuinely live subagent's event stream into
/// this restored (dead) entry instead - see
/// [`SessionManager::find_by_buffer_handle`].
pub fn restore_subagent_children(
    mgr: &mut SessionManager,
    parent_id: &SessionId,
    subagent_trajectories: &[atif::Trajectory],
) {
    for child_trajectory in subagent_trajectories {
        let Some(trajectory_id) = child_trajectory.trajectory_id.clone() else {
            continue;
        };
        let child_id = SessionId::from_string(trajectory_id);
        if mgr.entries.contains_key(&child_id) {
            continue;
        }

        let meta = sven_session_store::SvenSessionMeta::from_trajectory(child_trajectory);
        let title = meta
            .as_ref()
            .map(|m| m.title.clone())
            .unwrap_or_else(|| "Subagent task".to_string());
        let segments: Vec<ChatSegment> =
            sven_session_store::steps_to_conversation_records(&child_trajectory.steps)
                .into_iter()
                .filter_map(crate::app::construct::conversation_record_to_chat_segment)
                .collect();
        let mut chat = ChatState::new();
        chat.segments = segments;

        let mut entry = SessionEntry::new_subagent(title, parent_id.clone(), None, String::new());
        entry.id = child_id;
        // The prompt is already the chat's first segment (reconstructed from
        // the trajectory); `initial_prompt` is only a fallback synthesised
        // when `stored_chat` is still `None`, which no longer applies here.
        entry.initial_prompt = None;
        entry.status = meta
            .as_ref()
            .map(|m| m.status)
            .unwrap_or(sven_session_store::ChatStatus::Completed);
        entry.copied_context_steps = sven_session_store::copied_context_steps(child_trajectory);
        entry.stored_chat = Some(chat);

        mgr.add_child_session(parent_id.clone(), entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model::{Message, Role};

    /// One `(is_user, text)` pair per message - `true` for a user turn,
    /// `false` for an assistant turn.
    fn chat_with_messages(msgs: &[(bool, &str)]) -> ChatState {
        let mut chat = ChatState::new();
        chat.segments = msgs
            .iter()
            .map(|(is_user, text)| {
                ChatSegment::Message(if *is_user {
                    Message::user(*text)
                } else {
                    Message::assistant(*text)
                })
            })
            .collect();
        chat
    }
    const USER: bool = true;
    const ASSISTANT: bool = false;

    #[test]
    fn to_child_trajectory_stamps_trajectory_id_and_validates() {
        let (mut mgr, root) = SessionManager::new();
        let root_id = root.id.clone();
        mgr.register(root);

        let mut child = SessionEntry::new_subagent(
            "Fix the bug",
            root_id.clone(),
            Some("buf_0001".to_string()),
            "Please fix the bug".to_string(),
        );
        let child_id = child.id.clone();
        child.stored_chat = Some(chat_with_messages(&[
            (USER, "Please fix the bug"),
            (ASSISTANT, "Fixed it."),
        ]));

        let child_trajectory = child
            .to_child_trajectory(None, None)
            .expect("stored_chat present");
        assert_eq!(
            child_trajectory.trajectory_id.as_deref(),
            Some(child_id.as_str()),
            "embedded child must carry the ATIF-required trajectory_id"
        );

        let mut parent_trajectory =
            mgr.get(&root_id)
                .unwrap()
                .to_trajectory(&ChatState::new(), None, None);
        parent_trajectory.subagent_trajectories = Some(vec![child_trajectory]);
        assert!(
            atif::validate::validate_trajectory(&parent_trajectory).is_ok(),
            "a trajectory with one embedded child must validate: {:?}",
            atif::validate::validate_trajectory(&parent_trajectory)
        );
    }

    #[test]
    fn to_child_trajectory_gives_two_children_distinct_ids() {
        let (mut mgr, root) = SessionManager::new();
        let root_id = root.id.clone();
        mgr.register(root);

        let mut child_a =
            SessionEntry::new_subagent("Task A", root_id.clone(), None, "do A".to_string());
        child_a.stored_chat = Some(chat_with_messages(&[(USER, "do A")]));
        let mut child_b =
            SessionEntry::new_subagent("Task B", root_id.clone(), None, "do B".to_string());
        child_b.stored_chat = Some(chat_with_messages(&[(USER, "do B")]));

        let traj_a = child_a.to_child_trajectory(None, None).unwrap();
        let traj_b = child_b.to_child_trajectory(None, None).unwrap();
        assert_ne!(traj_a.trajectory_id, traj_b.trajectory_id);

        let mut parent_trajectory =
            mgr.get(&root_id)
                .unwrap()
                .to_trajectory(&ChatState::new(), None, None);
        parent_trajectory.subagent_trajectories = Some(vec![traj_a, traj_b]);
        assert!(atif::validate::validate_trajectory(&parent_trajectory).is_ok());
    }

    #[test]
    fn to_child_trajectory_falls_back_to_the_spawn_prompt_when_never_updated() {
        let (_, root) = SessionManager::new();
        let child = SessionEntry::new_subagent(
            "Still starting",
            root.id.clone(),
            None,
            "investigate the flaky test".to_string(),
        );
        assert!(child.stored_chat.is_none());

        let trajectory = child
            .to_child_trajectory(None, None)
            .expect("spawn prompt must be enough to persist something");
        let records = sven_session_store::steps_to_conversation_records(&trajectory.steps);
        let segments: Vec<ChatSegment> = records
            .into_iter()
            .filter_map(crate::app::construct::conversation_record_to_chat_segment)
            .collect();
        assert!(segments.iter().any(|s| matches!(
            s,
            ChatSegment::Message(m) if m.role == Role::User
                && m.as_text() == Some("investigate the flaky test")
        )));
    }

    #[test]
    fn to_child_trajectory_records_failure_instead_of_dropping_it() {
        let (_, root) = SessionManager::new();
        let mut child =
            SessionEntry::new_subagent("Doomed task", root.id.clone(), None, "do X".to_string());
        let mut chat = chat_with_messages(&[(USER, "do X")]);
        chat.segments
            .push(ChatSegment::Error("network timeout".to_string()));
        child.stored_chat = Some(chat);

        let trajectory = child.to_child_trajectory(None, None).unwrap();
        let records = sven_session_store::steps_to_conversation_records(&trajectory.steps);
        let segments: Vec<ChatSegment> = records
            .into_iter()
            .filter_map(crate::app::construct::conversation_record_to_chat_segment)
            .collect();
        assert!(
            segments.iter().any(|s| matches!(
                s,
                ChatSegment::Message(m) if m.as_text().is_some_and(|t| t.contains("network timeout"))
            )),
            "failure reason must survive as a message, not be silently dropped: {segments:?}"
        );
    }

    #[test]
    fn restore_subagent_children_reconstructs_transcript_and_metadata() {
        let (mut mgr, root) = SessionManager::new();
        let root_id = root.id.clone();
        mgr.register(root);

        let mut child =
            SessionEntry::new_subagent("Fix the bug", root_id.clone(), None, "fix it".to_string());
        child.stored_chat = Some(chat_with_messages(&[(USER, "fix it"), (ASSISTANT, "done")]));
        let child_trajectory = child.to_child_trajectory(None, None).unwrap();

        restore_subagent_children(&mut mgr, &root_id, std::slice::from_ref(&child_trajectory));

        let restored_id = SessionId::from_string(child_trajectory.trajectory_id.clone().unwrap());
        let restored = mgr.get(&restored_id).expect("child must be restored");
        assert_eq!(restored.parent_id.as_ref(), Some(&root_id));
        assert!(
            restored.buffer_handle.is_none(),
            "a restored child must never claim a buffer handle"
        );
        let segments = &restored.stored_chat.as_ref().unwrap().segments;
        assert!(segments
            .iter()
            .any(|s| matches!(s, ChatSegment::Message(m) if m.as_text() == Some("fix it"))));
        assert!(segments
            .iter()
            .any(|s| matches!(s, ChatSegment::Message(m) if m.as_text() == Some("done"))));
    }

    #[test]
    fn restore_subagent_children_is_idempotent() {
        let (mut mgr, root) = SessionManager::new();
        let root_id = root.id.clone();
        mgr.register(root);
        let mut child = SessionEntry::new_subagent("Task", root_id.clone(), None, "go".to_string());
        child.stored_chat = Some(chat_with_messages(&[(USER, "go")]));
        let child_trajectory = child.to_child_trajectory(None, None).unwrap();

        restore_subagent_children(&mut mgr, &root_id, std::slice::from_ref(&child_trajectory));
        restore_subagent_children(&mut mgr, &root_id, std::slice::from_ref(&child_trajectory));

        assert_eq!(
            mgr.children.get(&root_id).map(|v| v.len()),
            Some(1),
            "restoring the same embedded child twice must not duplicate it"
        );
    }
}
