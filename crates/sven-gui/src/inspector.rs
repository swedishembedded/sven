// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Inspector data: fetches skills, tools, peers, context stats, etc. for the
//! inspector overlay.

use slint::{ModelRc, SharedString, VecModel};

use crate::InspectorItem;

/// Kind of inspector tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InspectorKind {
    Skills = 0,
    Subagents = 1,
    Peers = 2,
    Context = 3,
    Tools = 4,
    Mcp = 5,
}

impl InspectorKind {
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Self::Subagents,
            2 => Self::Peers,
            3 => Self::Context,
            4 => Self::Tools,
            5 => Self::Mcp,
            _ => Self::Skills,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Skills => "Skills",
            Self::Subagents => "Subagents",
            Self::Peers => "Peers",
            Self::Context => "Context",
            Self::Tools => "Tools",
            Self::Mcp => "MCP",
        }
    }
}

/// Build placeholder items for the inspector when the agent has not yet
/// supplied data for the selected tab.
pub fn placeholder_items(kind: InspectorKind) -> ModelRc<InspectorItem> {
    let items = vec![InspectorItem {
        title: SharedString::from(format!("Loading {} …", kind.title())),
        subtitle: SharedString::new(),
        tag: SharedString::new(),
    }];
    ModelRc::new(VecModel::from(items))
}

/// Build `InspectorItem`s from a list of `(title, subtitle, tag)` tuples.
pub fn items_from_list(list: &[(String, String, String)]) -> ModelRc<InspectorItem> {
    if list.is_empty() {
        let empty = vec![InspectorItem {
            title: SharedString::from("(none)"),
            subtitle: SharedString::new(),
            tag: SharedString::new(),
        }];
        return ModelRc::new(VecModel::from(empty));
    }
    let items: Vec<InspectorItem> = list
        .iter()
        .map(|(t, s, tag)| InspectorItem {
            title: SharedString::from(t.as_str()),
            subtitle: SharedString::from(s.as_str()),
            tag: SharedString::from(tag.as_str()),
        })
        .collect();
    ModelRc::new(VecModel::from(items))
}
