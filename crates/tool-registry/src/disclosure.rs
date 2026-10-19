// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in deferred tool
// disclosure for agents then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Deferred tool disclosure: which tool definitions a model request carries.
//!
//! With [`Disclosure::Full`] a request carries the definition of every tool
//! the mode offers. With [`Disclosure::Deferred`] it carries the definitions
//! of the [`CORE_TOOLS`] and of the tools the session has loaded; every other
//! tool is known to the model by its one-line [`ToolDescriptor`] only, until
//! the model loads it through the [`DISCOVERY_TOOL`].
//!
//! Loading is activation, not proxying. A loaded tool's definition joins the
//! list of the requests that follow and the model calls the tool directly, so
//! the call is an ordinary typed effect under the tool's own capability,
//! permission policy and approval semantics. Nothing here executes a tool.
//!
//! The set of loaded tools is session state held by the kernel
//! (`sven_hsm::Context::active_tools`); this module only turns it, together
//! with the registry, into the list a request carries.
//!
//! # Order and the provider's prompt cache
//!
//! A provider reuses its cached prompt prefix only while the tool list it saw
//! last time is a prefix of the new one, and it renders tools ahead of the
//! system prompt and the conversation, so a change in the list rewrites the
//! cache of everything behind it. The list is therefore the core tools in
//! name order, then the loaded tools in the order they were loaded. The set
//! never shrinks and never reorders: loading costs one rewrite, and
//! everything after it is a prefix hit again.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock, Weak};

use serde::{Deserialize, Serialize};
use sven_hsm::ToolCallId;
use sven_tool_api::{Tool, ToolCall};
use sven_vocab::{AgentMode, ToolSchema};

use crate::descriptor::ToolDescriptor;
use crate::ToolRegistry;

/// The name of the tool that searches and loads the tools not yet offered.
pub const DISCOVERY_TOOL: &str = "tools";

/// The tools every task needs, always offered in full under
/// [`Disclosure::Deferred`]: the file, search and shell tools of the coding
/// loop, the todo list, and the discovery tool that reaches everything else.
/// Chosen by measurement: these are the tools that appear in the tool calls of
/// the baseline scenarios (see `docs/internals/token-ledger.md`).
///
/// An MCP tool is never core, whatever its name.
pub const CORE_TOOLS: &[&str] = &[
    "edit_file",
    "find_file",
    "grep",
    "read_file",
    "shell",
    DISCOVERY_TOOL,
    "todo",
    "write_file",
];

/// How much of each tool's definition a model request carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Disclosure {
    /// Every tool the mode offers, in full.
    #[default]
    Full,
    /// The core tools and the loaded tools in full; the rest by summary.
    Deferred,
}

impl Disclosure {
    /// The mode named `name` (`full` or `deferred`), if it is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "full" => Some(Self::Full),
            "deferred" => Some(Self::Deferred),
            _ => None,
        }
    }
}

/// What one model request asks the registry to offer.
#[derive(Clone, Copy, Debug)]
pub struct ToolOffer<'a> {
    /// Tools the requesting state names. When any, exactly these are offered,
    /// in full: a state that names its tools has decided what it needs.
    pub names: &'a [String],
    /// Otherwise, the [`AgentMode`] (by name) whose tools are offered. Empty
    /// offers nothing; an unknown name is `agent`.
    pub all_tools_mode: &'a str,
    /// The tools the session has loaded, in load order.
    pub active: &'a [String],
}

/// The tools a request offers and the ones it kept back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OfferedTools {
    /// The definitions to send, in the order to send them.
    pub schemas: Vec<ToolSchema>,
    /// Tools the mode has but the request does not offer because they are
    /// deferred and not loaded; sorted by name. A call to one of these is
    /// answered with an instruction to load it.
    pub withheld: Vec<String>,
}

/// The definition of `tool` as a model request carries it.
pub(crate) fn schema_of(tool: &dyn Tool, is_mcp: bool) -> ToolSchema {
    ToolSchema {
        name: tool.name().to_string(),
        description: tool.description().to_string(),
        parameters: tool.parameters_schema(),
        is_mcp,
    }
}

/// `true` for a tool that is always offered in full under deferral.
fn is_core(tool: &dyn Tool) -> bool {
    !tool.is_mcp() && CORE_TOOLS.contains(&tool.name())
}

/// The registry's tools by name.
pub(crate) type ToolTable = RwLock<HashMap<String, Arc<dyn Tool>>>;

/// The most call ids [`ToolRegistry::hold_back`] remembers; the oldest is
/// forgotten first. A call is taken back within one dispatch, so the bound is
/// only ever reached by calls that never ran (refused or rejected).
const HELD_BACK_CAPACITY: usize = 256;

/// Calls made to tools that were not loaded. Process-local and transient: it
/// carries one decision from the executor that saw the model propose the call
/// to the executor that is about to run it, and is not session state.
#[derive(Default)]
pub(crate) struct HeldBack {
    calls: HashSet<ToolCallId>,
    order: VecDeque<ToolCallId>,
}

impl ToolRegistry {
    /// How requests disclose tool definitions.
    #[must_use]
    pub fn disclosure(&self) -> Disclosure {
        self.disclosure
    }

    /// Sets how requests disclose tool definitions. Deferral takes effect
    /// only while the registry holds the [`DISCOVERY_TOOL`]: tools nobody can
    /// load must not be withheld.
    pub fn set_disclosure(&mut self, disclosure: Disclosure) {
        self.disclosure = disclosure;
    }

    /// The tools, and the definitions, one model request carries.
    ///
    /// Named tools win; else the mode's tools, which under deferral are the
    /// core tools in name order followed by the loaded tools in load order.
    /// A loaded tool is flagged `is_mcp` whatever it is: the flag tells the
    /// provider where the stable part of the list ends, and everything after
    /// the core tools is not stable.
    #[must_use]
    pub fn offer(&self, offer: &ToolOffer<'_>) -> OfferedTools {
        if !offer.names.is_empty() {
            return OfferedTools {
                schemas: self.schemas_for_names(offer.names),
                withheld: Vec::new(),
            };
        }
        if offer.all_tools_mode.is_empty() {
            return OfferedTools::default();
        }
        let mode = AgentMode::from_name(offer.all_tools_mode).unwrap_or(AgentMode::Agent);
        if self.disclosure == Disclosure::Full || self.get(DISCOVERY_TOOL).is_none() {
            return OfferedTools {
                schemas: self.schemas_for_mode(mode),
                withheld: Vec::new(),
            };
        }

        let mut in_mode = tools_in_mode(&self.tools, mode);
        in_mode.sort_by(|a, b| a.name().cmp(b.name()));
        let (core, deferred): (Vec<_>, Vec<_>) = in_mode.iter().partition(|t| is_core(t.as_ref()));

        let mut schemas: Vec<ToolSchema> =
            core.iter().map(|t| schema_of(t.as_ref(), false)).collect();
        let mut loaded = HashSet::new();
        for name in offer.active {
            if let Some(tool) = deferred.iter().find(|t| t.name() == name) {
                if loaded.insert(name.as_str()) {
                    schemas.push(schema_of(tool.as_ref(), true));
                }
            }
        }
        let withheld = deferred
            .iter()
            .map(|t| t.name().to_string())
            .filter(|name| !loaded.contains(name.as_str()))
            .collect();
        OfferedTools { schemas, withheld }
    }

    /// The definitions a model turn is offered: [`Self::offer`] without the
    /// tools it withheld.
    ///
    /// The one place the precedence lives (a machine's named tools, else the
    /// mode's tools, else none), so what a turn sends and what a record of the
    /// turn says it was offered cannot disagree.
    #[must_use]
    pub fn schemas_offered(
        &self,
        names: &[String],
        all_tools_mode: &str,
        active: &[String],
    ) -> Vec<ToolSchema> {
        self.offer(&ToolOffer {
            names,
            all_tools_mode,
            active,
        })
        .schemas
    }

    /// A handle for browsing this registry's tools that does not keep it
    /// alive; see [`ToolCatalog`].
    #[must_use]
    pub fn catalog(&self) -> ToolCatalog {
        ToolCatalog {
            tools: Arc::downgrade(&self.tools),
        }
    }

    /// The tools a successful `call` asks to have offered from now on, in the
    /// order it asked ([`Tool::activated_tools`] of the tool it names).
    #[must_use]
    pub fn activations(&self, call: &ToolCall) -> Vec<String> {
        self.get(&call.name)
            .map(|tool| tool.activated_tools(&call.args))
            .unwrap_or_default()
    }

    /// Records that the model called `call` without having loaded its tool.
    pub fn hold_back(&self, call: ToolCallId) {
        let Ok(mut held) = self.held_back.lock() else {
            return;
        };
        if held.calls.insert(call) {
            held.order.push_back(call);
        }
        while held.order.len() > HELD_BACK_CAPACITY {
            if let Some(oldest) = held.order.pop_front() {
                held.calls.remove(&oldest);
            }
        }
    }

    /// Whether `call` was held back, forgetting it: the answer is `true` once.
    pub fn take_held_back(&self, call: ToolCallId) -> bool {
        let Ok(mut held) = self.held_back.lock() else {
            return false;
        };
        let was_held = held.calls.remove(&call);
        if was_held {
            held.order.retain(|id| *id != call);
        }
        was_held
    }
}

fn tools_in_mode(tools: &ToolTable, mode: AgentMode) -> Vec<Arc<dyn Tool>> {
    tools.read().map_or_else(
        |_| Vec::new(),
        |tools| {
            tools
                .values()
                .filter(|t| t.modes().contains(&mode))
                .cloned()
                .collect()
        },
    )
}

/// A read-only view of a registry's tools, for a tool that lives inside the
/// registry and so cannot own it.
///
/// Holds the registry's tool table weakly: the discovery tool is itself in
/// that table, and a strong handle would keep the table, and so the tool
/// holding the handle, alive forever. Once the registry is gone every query
/// answers as if no tool were registered.
#[derive(Clone)]
pub struct ToolCatalog {
    tools: Weak<ToolTable>,
}

impl ToolCatalog {
    fn table(&self) -> Option<Arc<ToolTable>> {
        self.tools.upgrade()
    }

    /// The descriptors of the tools `mode` has that a request does not carry
    /// unless they are loaded: every non-core tool, sorted by name.
    #[must_use]
    pub fn deferred_descriptors(&self, mode: AgentMode) -> Vec<ToolDescriptor> {
        let Some(table) = self.table() else {
            return Vec::new();
        };
        let mut descriptors: Vec<_> = tools_in_mode(&table, mode)
            .iter()
            .filter(|t| !is_core(t.as_ref()))
            .map(|t| ToolDescriptor::of(t.as_ref()))
            .collect();
        descriptors.sort_by(|a, b| a.name.cmp(&b.name));
        descriptors
    }

    /// The registered tool `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.table()?.read().ok()?.get(name).cloned()
    }

    /// The descriptor of the registered tool `name`.
    #[must_use]
    pub fn describe(&self, name: &str) -> Option<ToolDescriptor> {
        self.get(name).map(|t| ToolDescriptor::of(t.as_ref()))
    }

    /// `true` when `name` is registered and deferred (not core).
    #[must_use]
    pub fn is_deferred(&self, name: &str) -> bool {
        self.get(name).is_some_and(|t| !is_core(t.as_ref()))
    }

    /// The full definition of the registered tool `name`.
    #[must_use]
    pub fn definition(&self, name: &str) -> Option<ToolSchema> {
        self.get(name).map(|t| schema_of(t.as_ref(), t.is_mcp()))
    }
}
