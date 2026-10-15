// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The gates a run puts to its host: a question for a person, or an approval
//! of one tool call.
//!
//! Swedish Embedded AB implements human-in-the-loop agent runtimes for its
//! clients. If your team needs expertise in approval workflows for agents
//! then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::Arc;

use sven_bootstrap::session_handles::KernelChannels;
use sven_hsm::{ApprovalId, GatedCall, ToolCapability};
use tokio::sync::{mpsc, oneshot};

use crate::run::RunId;

/// One gate of a run, handed to the host that has chosen to answer it.
///
/// A gate carries its own reply channel, so the host owns answering: it may
/// reply now, or hold the channel while it asks someone and reply minutes
/// later. A gate dropped without a reply is answered for the host and never
/// leaves the run waiting: a question gets
/// [`NO_USER_ANSWER`](crate::tool::NO_USER_ANSWER), and an approval is denied
/// with the reason `handler_dropped`, which the model reads.
///
/// A host that cannot answer on the spot parks approvals instead; see
/// [`EngineBuilder::park_approvals`](crate::EngineBuilder::park_approvals).
pub enum HumanGate {
    /// A free-text question for a person.
    Question {
        /// What to show the person.
        prompt: String,
        /// Their answer.
        reply_tx: oneshot::Sender<String>,
    },
    /// Whether one tool call may run.
    #[non_exhaustive]
    Approval {
        /// The run that raised the gate. Events of that run carry the same
        /// id, so a host serving several sessions routes the gate by it.
        run_id: RunId,
        /// Identifies this approval, and the call it is for. It is the id a
        /// parked approval would carry, so a host keeps one key for both.
        approval_id: ApprovalId,
        /// What capability is being asked for.
        capability: ToolCapability,
        /// What will happen if this is approved.
        prompt: String,
        /// The tool call it gates, with its arguments, when it gates one.
        call: Option<GatedCall>,
        /// `true` to approve.
        reply_tx: oneshot::Sender<bool>,
    },
}

/// Where a host receives its gates: a callback, because the host already has
/// somewhere to put them (its own run state, its own UI), and a callback never
/// obliges it to keep a task alive just to move values between queues.
pub(crate) type GateHandler = Arc<dyn Fn(HumanGate) + Send + Sync>;

/// Hands every question and approval of run `run_id` to `handler`, until the
/// session's channels close.
pub(crate) async fn forward(mut channels: KernelChannels, run_id: RunId, handler: GateHandler) {
    loop {
        tokio::select! {
            question = channels.question_rx.recv() => match question {
                Some(q) => handler(HumanGate::Question {
                    prompt: q.prompt,
                    reply_tx: q.reply_tx,
                }),
                None => break,
            },
            approval = channels.approval_rx.recv() => match approval {
                Some(a) => handler(HumanGate::Approval {
                    run_id,
                    approval_id: a.approval_id,
                    capability: a.capability,
                    prompt: a.description,
                    call: a.call,
                    reply_tx: a.reply_tx,
                }),
                None => break,
            },
        }
    }
}

/// A question channel for the `ask_question` tool whose questions go to
/// `handler` as [`HumanGate::Question`]s: the questions' prompts, each with
/// its options, and the host's reply returned as the answer. The channel
/// closes when the session drops the tool.
pub(crate) fn ask_through(handler: GateHandler) -> mpsc::Sender<sven_bootstrap::QuestionRequest> {
    let (tx, mut rx) = mpsc::channel::<sven_bootstrap::QuestionRequest>(4);
    tokio::spawn(async move {
        while let Some(request) = rx.recv().await {
            let prompt = request
                .questions
                .iter()
                .map(|q| match q.options.as_slice() {
                    [] => q.prompt.clone(),
                    options => format!("{} ({})", q.prompt, options.join(" / ")),
                })
                .collect::<Vec<_>>()
                .join("\n");
            handler(HumanGate::Question {
                prompt,
                reply_tx: request.answer_tx,
            });
        }
    });
    tx
}
