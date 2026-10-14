// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The tools and the context view a turn request carries.
//!
//! A demoted result is a reference, so a request that carries one must also
//! offer the tool that expands it. The two are decided together: when the view
//! demotes anything the expansion tool is loaded into the session, and when the
//! request cannot offer it (a state that names its own tools, `--no-tools`) the
//! view is dropped and the history is sent as it is.

use sven_chunks::EXPAND_TOOL_NAME;
use sven_model::Message;
use sven_tool_registry::{OfferedTools, ToolOffer};
use sven_vocab::{ContextView, TurnRequest};

use super::TurnExecutor;

/// The messages to send and the record of how they were assembled.
pub(super) type View = (Vec<Message>, ContextView);

/// What a request carries: its tools, and its context when that is not the
/// history itself.
pub(super) struct Planned {
    pub offered: OfferedTools,
    pub view: Option<View>,
    /// Tools to load into the session before the request goes out.
    pub activate: Vec<String>,
}

impl TurnExecutor {
    /// The tools and the context view of `req`, decided together.
    pub(super) fn plan_request(&mut self, req: &TurnRequest, history: &[Message]) -> Planned {
        let mut view = self.assemble_view(history);
        let demotes = view.as_ref().is_some_and(|(_, r)| r.demoted > 0);
        let offer = |tools: &Self, active: &[String]| {
            if tools.no_tools {
                OfferedTools::default()
            } else {
                tools.tools.offer(&ToolOffer {
                    names: &req.tools,
                    all_tools_mode: &req.all_tools_mode,
                    active,
                })
            }
        };

        let mut activate = Vec::new();
        let mut active = req.active_tools.clone();
        if demotes && !active.iter().any(|n| n == EXPAND_TOOL_NAME) {
            active.push(EXPAND_TOOL_NAME.to_string());
            activate.push(EXPAND_TOOL_NAME.to_string());
        }
        let mut offered = offer(self, &active);
        if demotes && !offers_expansion(&offered) {
            view = None;
            activate.clear();
            offered = offer(self, &req.active_tools);
        }
        Planned {
            offered,
            view,
            activate,
        }
    }

    /// The view of `history` once it changed (after a compaction). Dropped
    /// when `expandable` is false.
    pub(super) fn reassemble_view(
        &mut self,
        history: &[Message],
        expandable: bool,
    ) -> Option<View> {
        let view = self.assemble_view(history)?;
        (expandable || view.1.demoted == 0).then_some(view)
    }

    /// Note that `view` went out with a request.
    pub(super) fn mark_view_sent(&mut self, view: &ContextView) {
        if let Some(assembler) = self.context.as_mut() {
            assembler.mark_sent(view);
        }
    }

    fn assemble_view(&mut self, history: &[Message]) -> Option<View> {
        let assembler = self.context.as_mut()?;
        Some(assembler.assemble(history.to_vec()))
    }
}

/// Whether `offered` carries the expansion tool's definition.
pub(super) fn offers_expansion(offered: &OfferedTools) -> bool {
    offered.schemas.iter().any(|s| s.name == EXPAND_TOOL_NAME)
}
