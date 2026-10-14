// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Who answers a sub-agent's permission request that its contract does not
//! settle: the host that asks about every call, or the session's own approval
//! gate.

use std::sync::Arc;

use sven_tool_api::PermissionRequester;
use sven_vocab::ApprovalMode;
use tokio::sync::mpsc;

/// Who answers a sub-agent's permission request that is not allowed outright.
#[derive(Clone)]
pub enum ChildApprover {
    /// The host's requester (an IDE over ACP). The host is asked about every
    /// call a tool's policy marks `Ask`, in the parent as in the child, so no
    /// child request is allowed without it.
    Host(Arc<dyn PermissionRequester>),
    /// The session's own approval gate (`GateApprover`), asked under manual
    /// approval about what the session's policy does not allow without
    /// asking.
    Gate(Arc<dyn PermissionRequester>),
}

impl ChildApprover {
    /// Who answers a sub-agent's requests in a session with `host`'s
    /// requester (an IDE over ACP), under `approval`: the host when there is
    /// one, the session's own approval gate (`approvals`) under manual
    /// approval, and nobody under auto - there the contract settles every
    /// request.
    pub(crate) fn for_session(
        host: Option<Arc<dyn PermissionRequester>>,
        approval: ApprovalMode,
        approvals: &mpsc::Sender<sven_executors::ApprovalRequest>,
        tools: &crate::config::ToolsConfig,
    ) -> Option<Self> {
        match (host, approval) {
            (Some(host), _) => Some(Self::Host(host)),
            (None, ApprovalMode::Manual) => Some(Self::Gate(Arc::new(
                crate::session_handles::GateApprover::new(approvals.clone())
                    .with_preapproval(crate::mode_policy::preapproval(tools)),
            ))),
            (None, ApprovalMode::Auto) => None,
        }
    }

    /// The requester that is asked.
    pub(crate) fn requester(&self) -> &dyn PermissionRequester {
        match self {
            Self::Host(r) | Self::Gate(r) => r.as_ref(),
        }
    }

    /// `true` when the parent itself asks about every `Ask` call.
    pub(crate) fn asks_every_call(&self) -> bool {
        matches!(self, Self::Host(_))
    }
}
