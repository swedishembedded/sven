// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The ACP `Client` half that a subagent session presents to its child:
//! notification forwarding and the permission decision for each of the
//! child's tool calls, taken from the child's run contract.
//!
//! Swedish Embedded AB implements solutions for gating the tool calls of
//! untrusted agent subprocesses for its clients. If your team needs expertise
//! in capability-based permission enforcement for agents, you can procure our
//! services by sending an email to info@swedishembedded.com.
use std::cell::Cell;
use std::rc::Rc;

use agent_client_protocol::{
    Client, PermissionOption, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, Result as AcpResult,
    SelectedPermissionOutcome, SessionNotification,
};
use async_trait::async_trait;
use serde_json::Value;
use tracing::debug;

use sven_hsm::{capability_for_tool_name, ChildRunContract, ToolCapability};
use sven_tool_api::tool::ToolCall;

use super::ChildApprover;

/// Minimal ACP `Client` for subagent task execution.
///
/// Forwards `session_notification` updates through a channel and answers the
/// child's permission requests from the child's contract (see
/// [`answer_permission`]).
pub(super) struct AcpTaskClient {
    pub(super) notification_tx: futures::channel::mpsc::UnboundedSender<SessionNotification>,
    pub(super) contract: ChildRunContract,
    pub(super) approver: Option<ChildApprover>,
    /// Permission requests being answered right now. The inactivity timeout
    /// waits while any is: a person deciding is not a silent child.
    pub(super) pending: Rc<Cell<usize>>,
}

/// Counts one pending permission request for as long as it lives.
struct Pending(Rc<Cell<usize>>);

impl Pending {
    fn start(count: &Rc<Cell<usize>>) -> Self {
        count.set(count.get() + 1);
        Self(Rc::clone(count))
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

#[async_trait(?Send)]
impl Client for AcpTaskClient {
    async fn request_permission(
        &self,
        args: RequestPermissionRequest,
    ) -> AcpResult<RequestPermissionResponse> {
        let _pending = Pending::start(&self.pending);
        let outcome = answer_permission(&self.contract, self.approver.as_ref(), &args).await;
        Ok(RequestPermissionResponse::new(outcome))
    }

    async fn session_notification(&self, args: SessionNotification) -> AcpResult<()> {
        let _ = self.notification_tx.unbounded_send(args);
        Ok(())
    }
}

/// The answer to one of the child's permission requests.
///
/// The request names the capability the child classed the call under
/// ([`CAPABILITY_META_KEY`](super::CAPABILITY_META_KEY)). One that does not
/// is held to what its tool's name says (`capability_for_tool_name`), but
/// never to a read-only capability: a tool's name is the child's to choose,
/// and an MCP server may call anything `read_file`. A call outside the contract
/// is refused without asking anyone. Within it, a parent whose host asks
/// about every call ([`ChildApprover::Host`]) puts it to the host; a call the
/// contract allows without approval is allowed; any other - one that is not
/// read-only, under a parent's manual approval - goes to `approver` (the
/// parent's own gate), and is refused when there is none.
pub(super) async fn answer_permission(
    contract: &ChildRunContract,
    approver: Option<&ChildApprover>,
    request: &RequestPermissionRequest,
) -> RequestPermissionOutcome {
    let fields = &request.tool_call.fields;
    let name = fields.title.clone().unwrap_or_default();
    let capability = request
        .tool_call
        .meta
        .as_ref()
        .and_then(|meta| meta.get(super::CAPABILITY_META_KEY))
        .and_then(|cap| serde_json::from_value::<ToolCapability>(cap.clone()).ok())
        .unwrap_or_else(|| match capability_for_tool_name(&name) {
            named if named.is_read_only() => ToolCapability::NetworkAccess,
            named => named,
        });
    let host_asks = approver.is_some_and(ChildApprover::asks_every_call);
    let allowed = if !contract.policy.allows_in_every_state(capability) {
        debug!(
            ?capability,
            "task: refusing a sub-agent call outside its contract"
        );
        false
    } else if !host_asks && contract.allows_without_asking(capability) {
        true
    } else if let Some(approver) = approver {
        let call = ToolCall {
            id: request.tool_call.tool_call_id.to_string(),
            name,
            args: fields.raw_input.clone().unwrap_or(Value::Null),
        };
        approver
            .requester()
            .request_permission(&call, capability)
            .await
    } else {
        debug!(?capability, "task: nobody to ask about a sub-agent call");
        false
    };
    let wanted: &[PermissionOptionKind] = if allowed {
        &[PermissionOptionKind::AllowOnce]
    } else {
        &[
            PermissionOptionKind::RejectOnce,
            PermissionOptionKind::RejectAlways,
        ]
    };
    match pick_option(&request.options, wanted) {
        Some(option) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
            option.option_id.clone(),
        )),
        None => RequestPermissionOutcome::Cancelled,
    }
}

/// The first option of the first kind in `kinds` that `options` offers.
fn pick_option<'a>(
    options: &'a [PermissionOption],
    kinds: &[PermissionOptionKind],
) -> Option<&'a PermissionOption> {
    kinds
        .iter()
        .find_map(|kind| options.iter().find(|o| o.kind == *kind))
}
