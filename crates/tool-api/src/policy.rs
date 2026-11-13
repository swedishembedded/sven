// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The approval-decision vocabulary every [`crate::tool::Tool`] impl names in
//! its `default_policy()`, plus the trait bridging a gated call out to an
//! external approval mechanism.
//!
//! The config-driven policy *engine* that decides which [`ApprovalPolicy`] a
//! given command gets (`ToolPolicy`) lives one tier up, in
//! `sven-tool-registry` -- it needs a compiled
//! pattern set, which is registry-shaped state, not part of the
//! `Tool` trait's interface. `ApprovalPolicy` itself has to
//! live here rather than there: `Tool::default_policy(&self) -> ApprovalPolicy`
//! is part of the trait signature, so the type it names cannot sit at a higher
//! tier than the trait without creating an illegal upward edge from every
//! crate that implements `Tool`.

/// Per-tool approval policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Always run without asking
    Auto,
    /// Ask user before each invocation
    Ask,
    /// Never run; return an error
    Deny,
}

/// A person's answer to a call put to them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permission {
    Allowed,
    /// Refused; `reason`, when given, is for the agent: why, and what to do
    /// instead.
    Denied {
        reason: Option<String>,
    },
}

/// Async callback invoked before executing a tool that requires approval.
///
/// Implementors bridge from the tool-execution pipeline to an external approval
/// mechanism.  When Sven runs as an ACP server the implementation calls
/// `AgentSideConnection::request_permission` so the IDE can allow or deny the
/// call.  When no requester is wired up, a tool whose policy is `Ask` runs.
#[async_trait::async_trait]
pub trait PermissionRequester: Send + Sync {
    /// Return `true` to allow the tool call, `false` to deny it.
    /// `capability` is what the call does
    /// ([`Tool::call_capability`](crate::Tool::call_capability)), so the
    /// answer never rests on the tool's name.
    async fn request_permission(
        &self,
        call: &crate::ToolCall,
        capability: crate::ToolCapability,
    ) -> bool;

    /// The answer with what the person said about it: a denial may carry a
    /// reason the agent is told (what to do instead). By default the yes or
    /// no of [`Self::request_permission`], with no reason.
    async fn decide(
        &self,
        call: &crate::ToolCall,
        capability: crate::ToolCapability,
    ) -> Permission {
        if self.request_permission(call, capability).await {
            Permission::Allowed
        } else {
            Permission::Denied { reason: None }
        }
    }

    /// `true` while every call must be put to this requester, a tool's
    /// [`ApprovalPolicy::Auto`] included: the turn running was started by
    /// someone other than the session's owner, and nothing of the owner's
    /// may run on it unasked.
    fn asks_every_call(&self) -> bool {
        false
    }
}
