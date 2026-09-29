// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The answer a machine gets when an effect it emitted is not carried out.
//!
//! One mapping, used by the kernel when the permission gate refuses a
//! non-tool batch and by every executor that has nowhere to send an effect,
//! so that "refused" and "undeliverable" read the same to a machine.

use sven_hsm::{Effect, Event};

/// The event that tells the emitting machine `effect` was not carried out,
/// or `None` for an effect nothing waits on (`PersistAudit`, `EmitInternal`,
/// `CancelTimeout`).
///
/// An effect whose result a machine already knows how to receive as a
/// failure gets that event, so no machine needs a second failure path:
/// `CallLlm` answers with [`Event::LlmFailed`], `CallTool` with
/// [`Event::ToolFailed`], and an approval request that was never put to
/// anyone with [`Event::HumanRejected`] - not approved is not approved.
/// Everything else answers with [`Event::EffectFailed`].
#[must_use]
pub fn failure_event(effect: &Effect, error: &str) -> Option<Event> {
    let error = error.to_string();
    match effect {
        Effect::CallLlm { .. } => Some(Event::LlmFailed { error }),
        Effect::CallTool { call_id, .. } => Some(Event::ToolFailed {
            call_id: *call_id,
            error,
        }),
        Effect::RequestHumanApproval { approval_id, .. } => Some(Event::HumanRejected {
            approval_id: *approval_id,
        }),
        Effect::PersistAudit | Effect::EmitInternal { .. } | Effect::CancelTimeout { .. } => None,
        other => Some(Event::EffectFailed {
            kind: other.kind(),
            error,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::EffectKind;

    #[test]
    fn a_fire_and_forget_effect_needs_no_answer() {
        assert_eq!(failure_event(&Effect::PersistAudit, "x"), None);
    }

    #[test]
    fn an_awaited_effect_is_answered_with_its_kind() {
        let got = failure_event(&Effect::AskUser { prompt: "?".into() }, "refused");
        assert_eq!(
            got,
            Some(Event::EffectFailed {
                kind: EffectKind::AskUser,
                error: "refused".into(),
            })
        );
    }
}
