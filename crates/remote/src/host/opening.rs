// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements solutions for running agent sessions on a
// person's own machine for a server for its clients. If your team needs
// expertise in remotely driven agents then you can procure our services by
// sending an email to info@swedishembedded.com.

//! How a hosted session is opened: the model it talks to through the server
//! and the live controller built on the conversation kept so far.

use std::sync::Arc;

use sven_frontend::{SessionController, SessionOptions, SessionSpec};
use sven_model_drivers::ModelConfig;
use sven_vocab::goal::GoalCommand;

use super::conversation_files::ConversationUpload;
use super::hosted::session_runtime;
use super::{attachments, SessionActor};
use crate::approvals::{RemoteApprovals, APPROVAL_TIMEOUT};
use sven_tool_api::files::ConversationFiles;

impl SessionActor {
    /// Where this conversation's tools, attachments and files live
    /// ([`super::hosted::conversation_dir`]).
    pub(super) fn conversation_dir(&self) -> anyhow::Result<std::path::PathBuf> {
        super::hosted::conversation_dir(
            &self.context.working_dir,
            self.context.shared_workdir,
            self.id,
        )
    }

    /// The model this conversation talks to, through the server.
    pub(super) fn model_config(&self) -> anyhow::Result<ModelConfig> {
        #[cfg(test)]
        if let Some(model) = &self.scripted_model {
            return Ok(model.clone());
        }
        let name = self
            .model
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no model was chosen for this conversation"))?;
        Ok(ModelConfig {
            provider: "openai".into(),
            name,
            base_url: Some(self.context.openai_base_url.clone()),
            api_key: Some(self.context.token.clone()),
            // Each call names this conversation, so the server records what
            // it cost under it.
            session: Some(self.id.to_string()),
            ..ModelConfig::default()
        })
    }

    /// Opens the live session on the conversation kept so far.
    pub(super) async fn open_controller(
        &mut self,
        model: ModelConfig,
    ) -> anyhow::Result<SessionController> {
        let dir = self.conversation_dir()?;
        let mut runtime = session_runtime(
            &self.context.config,
            &dir,
            self.context.hosted,
            self.context.wide_access,
            self.context.shared_workdir,
        )?;
        // What its tools write, such as a converted document, can become a
        // file of this conversation, and of no other.
        runtime.conversation_files = Some(ConversationFiles(Arc::new(ConversationUpload {
            client: self.context.client.clone(),
            conversation: self.id,
            working_dir: dir.clone(),
        })));
        let mut options = SessionOptions::new(
            attachments::for_session(&self.context.config, self.id, &dir),
            runtime,
            self.events_tx.clone(),
        );
        // A tool that asks first asks the person at the server.
        options.permission_requester = Some(Arc::new(RemoteApprovals {
            session_id: self.id,
            pending: self.approvals.clone(),
            ask: self.context.asks.clone(),
            timeout: APPROVAL_TIMEOUT,
            turn: self.turn.clone(),
            approved: Arc::default(),
        }));
        // The first answer can already use the servers' tools, and a server
        // that lists them later (or changes them) still reaches the session:
        // nothing here shows MCP events, so nothing else would pick them up.
        options.wait_for_mcp_tools_ms = Some(self.context.mcp_tools_wait_ms);
        let spec = SessionSpec::for_mode(self.mode, model);
        let history = std::mem::take(&mut self.history);
        let (controller, _notices) = SessionController::open(options, spec, history).await?;
        controller.follow_mcp_tools();
        // A goal kept from before the session was let go (a restart, a
        // replaced conversation) comes back; an active one comes back paused.
        if let Some(goal) = self.goal.clone() {
            let restore = GoalCommand::Restore {
                goal: Box::new(goal),
            };
            if let Err(e) = controller.goal(restore, false).await {
                tracing::debug!("session {}: its goal is not restored: {e}", self.id);
            }
        }
        Ok(controller)
    }
}
