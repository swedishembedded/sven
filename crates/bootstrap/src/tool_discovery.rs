// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in deferred tool
// disclosure for agents then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The `tools` tool: how a model finds and loads the tools that its tool list
//! does not carry.
//!
//! Under deferred disclosure (see [`sven_tool_registry::Disclosure`]) a
//! request carries full definitions only for the core tools and the tools the
//! session has loaded. The rest are known by a one-line summary, in
//! [`index_section`] of the system prompt and through this tool:
//!
//! | action | does |
//! |--------|------|
//! | `search` | ranks the unloaded tools against a query ([`ToolRanker`]) |
//! | `load` | makes tools callable from the next step |
//! | `list` | pages through every unloaded tool, so a missed search is never a dead end |
//! | `help` | shows a tool's full definition without loading it |
//!
//! `load` activates; it does not call. The tool names it accepts are returned
//! from [`Tool::activated_tools`], the executor posts them to the kernel, and
//! the kernel records them in the session's context. The model then calls the
//! loaded tool directly, as a typed effect under that tool's own capability.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use sven_hsm::ToolCapability;
use sven_tool_api::{ApprovalPolicy, Tool, ToolCall, ToolOutput};
use sven_tool_registry::{
    present_matches, Bm25Ranker, Disclosure, ToolCatalog, ToolDescriptor, ToolRanker, ToolRegistry,
    DISCOVERY_TOOL,
};
use sven_vocab::AgentMode;

/// Matches returned by `search` unless the model asks for another number.
const DEFAULT_SEARCH_RESULTS: usize = 8;
/// The most matches one `search` returns.
const MAX_SEARCH_RESULTS: usize = 20;
/// Entries on one page of `list`.
const LIST_PAGE: usize = 25;
/// The most tools one `load` or `help` names.
const MAX_NAMES: usize = 16;
/// The most definitions one `help` prints, in full.
const MAX_HELP: usize = 3;
/// Above this many unloaded tools the system prompt carries counts by
/// category instead of one line each, so a session with hundreds of MCP tools
/// does not pay for hundreds of lines on every request.
pub const INLINE_INDEX_LIMIT: usize = 40;

/// The discovery tool. Registered, with deferred disclosure, in every
/// profile that can reach more tools than its core.
pub struct ToolDiscoveryTool {
    catalog: ToolCatalog,
    mode: Arc<Mutex<AgentMode>>,
    ranker: Arc<dyn ToolRanker>,
}

impl ToolDiscoveryTool {
    /// A discovery tool over `registry`'s tools, scoped to the session's
    /// current mode, ranking with [`Bm25Ranker`].
    #[must_use]
    pub fn new(registry: &ToolRegistry, mode: Arc<Mutex<AgentMode>>) -> Self {
        Self {
            catalog: registry.catalog(),
            mode,
            ranker: Arc::new(Bm25Ranker),
        }
    }

    /// Ranks with `ranker` instead of [`Bm25Ranker`].
    #[must_use]
    pub fn with_ranker(mut self, ranker: Arc<dyn ToolRanker>) -> Self {
        self.ranker = ranker;
        self
    }

    /// The unloaded tools of the current mode, optionally of one category.
    async fn candidates(&self, category: Option<&str>) -> Vec<ToolDescriptor> {
        let mode = *self.mode.lock().await;
        let mut tools = self.catalog.deferred_descriptors(mode);
        if let Some(category) = category {
            tools.retain(|t| t.category() == category);
        }
        tools
    }

    async fn search(&self, call: &ToolCall) -> ToolOutput {
        let Some(query) = str_arg(call, "query").filter(|q| !q.trim().is_empty()) else {
            return ToolOutput::err(
                &call.id,
                "search needs `query`: say in plain words what you need a tool for",
            );
        };
        let category = str_arg(call, "category");
        let limit = uint_arg(call, "limit")
            .unwrap_or(DEFAULT_SEARCH_RESULTS)
            .clamp(1, MAX_SEARCH_RESULTS);
        let pool = self.candidates(category).await;
        let ranked = self.ranker.rank(query, &pool).await;
        if ranked.is_empty() {
            return ToolOutput::ok(
                &call.id,
                format!(
                    "No tool matches {query:?} among {} that are not loaded. Try other words, or \
                     page through all of them with action=list.",
                    pool.len()
                ),
            );
        }
        ToolOutput::ok(&call.id, present_matches(query, &pool, &ranked, limit))
    }

    async fn list(&self, call: &ToolCall) -> ToolOutput {
        let pool = self.candidates(str_arg(call, "category")).await;
        let start = uint_arg(call, "offset").unwrap_or(0);
        if pool.is_empty() {
            return ToolOutput::ok(&call.id, "No tools are waiting to be loaded.");
        }
        if start >= pool.len() {
            return ToolOutput::err(
                &call.id,
                format!(
                    "offset {start} is past the end: there are {} tools to list",
                    pool.len()
                ),
            );
        }
        let end = (start + LIST_PAGE).min(pool.len());
        let page: Vec<&ToolDescriptor> = pool[start..end].iter().collect();
        let mut out = format!(
            "Tools not loaded, {}-{} of {}:\n",
            start + 1,
            end,
            pool.len()
        );
        push_lines(&mut out, &page);
        if end < pool.len() {
            out.push_str(&format!("More: action=list with offset={end}."));
        } else {
            out.push_str("That is all of them.");
        }
        ToolOutput::ok(&call.id, out)
    }

    async fn load(&self, call: &ToolCall) -> ToolOutput {
        let names = match names_arg(call) {
            Ok(names) => names,
            Err(why) => return ToolOutput::err(&call.id, why),
        };
        let mode = *self.mode.lock().await;
        let pool = self.catalog.deferred_descriptors(mode);
        let loadable: Vec<String> = self.activated_tools(&call.args);

        let mut unknown = Vec::new();
        let mut not_loadable = Vec::new();
        for name in &names {
            if loadable.contains(name) {
                continue;
            }
            match self.catalog.describe(name) {
                Some(_) => not_loadable.push(name.clone()),
                None => unknown.push(self.unknown(name, &pool)),
            }
        }

        if loadable.is_empty() {
            return ToolOutput::err(
                &call.id,
                format!(
                    "Nothing loaded. {}",
                    problems(&unknown, &not_loadable).unwrap_or_default()
                ),
            );
        }
        let mut out = format!(
            "Loaded: {}. Their definitions are in your tool list from your next step; call \
             them directly, like any other tool.",
            loadable.join(", ")
        );
        if let Some(problems) = problems(&unknown, &not_loadable) {
            out.push(' ');
            out.push_str(&problems);
        }
        ToolOutput::ok(&call.id, out)
    }

    /// `name` is not a tool: say so, and name the nearest one if there is one.
    fn unknown(&self, name: &str, pool: &[ToolDescriptor]) -> String {
        match Bm25Ranker.rank_now(name, pool).first() {
            Some(nearest) => format!("{name} (not a tool; did you mean {}?)", nearest.name),
            None => format!("{name} (not a tool)"),
        }
    }

    fn help(&self, call: &ToolCall) -> ToolOutput {
        let names = match names_arg(call) {
            Ok(names) => names,
            Err(why) => return ToolOutput::err(&call.id, why),
        };
        let mut out = String::new();
        for name in names.iter().take(MAX_HELP) {
            match self.catalog.definition(name) {
                Some(def) => out.push_str(&format!(
                    "{}\n{}\nparameters: {}\n\n",
                    def.name, def.description, def.parameters
                )),
                None => out.push_str(&format!("{name}: not a tool\n\n")),
            }
        }
        if names.len() > MAX_HELP {
            out.push_str(&format!(
                "Only the first {MAX_HELP} are shown; ask for the rest separately.\n"
            ));
        }
        out.push_str("This does not load a tool; use action=load for that.");
        ToolOutput::ok(&call.id, out)
    }
}

#[async_trait]
impl Tool for ToolDiscoveryTool {
    fn name(&self) -> &str {
        DISCOVERY_TOOL
    }

    fn description(&self) -> &str {
        "Find and load tools missing from your tool list. search: ranked by a query. load: \
         callable from your next step. list: all of them, paged. help: a definition, without \
         loading."
    }

    fn summary(&self) -> &str {
        "Search, list and load the tools that are not in your tool list yet."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["search", "load", "list", "help"]
                },
                "query": {
                    "type": "string",
                    "description": "[search] what you need"
                },
                "names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "[load|help] tool names"
                },
                "category": {
                    "type": "string",
                    "description": "[search|list] read, write, shell, network, agent, mcp, ..."
                },
                "offset": {
                    "type": "integer",
                    "description": "[list] first entry; default 0"
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    /// Browsing and loading change nothing outside the run, and loading grants
    /// nothing: the loaded tool is gated under its own capability when it is
    /// called. So this is read-only, in every mode, and never asked about.
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }

    fn activated_tools(&self, args: &Value) -> Vec<String> {
        if args.get("action").and_then(Value::as_str) != Some("load") {
            return Vec::new();
        }
        let mut loadable: Vec<String> = Vec::new();
        for name in args
            .get("names")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .take(MAX_NAMES)
        {
            if self.catalog.is_deferred(name) && !loadable.iter().any(|n| n == name) {
                loadable.push(name.to_string());
            }
        }
        loadable
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        match str_arg(call, "action") {
            Some("search") => self.search(call).await,
            Some("load") => self.load(call).await,
            Some("list") => self.list(call).await,
            Some("help") => self.help(call),
            Some(other) => ToolOutput::err(
                &call.id,
                format!("unknown action {other:?}; use search, load, list or help"),
            ),
            None => ToolOutput::err(&call.id, "missing `action`: search, load, list or help"),
        }
    }
}

fn str_arg<'a>(call: &'a ToolCall, key: &str) -> Option<&'a str> {
    call.args.get(key).and_then(Value::as_str)
}

fn uint_arg(call: &ToolCall, key: &str) -> Option<usize> {
    call.args
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

/// The `names` argument: a non-empty list of at most [`MAX_NAMES`] strings.
fn names_arg(call: &ToolCall) -> Result<Vec<String>, String> {
    let names: Vec<String> = call
        .args
        .get("names")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    match names.len() {
        0 => Err("`names` is needed: a list of tool names".to_string()),
        n if n > MAX_NAMES => Err(format!("at most {MAX_NAMES} names at a time, got {n}")),
        _ => Ok(names),
    }
}

fn push_lines(out: &mut String, tools: &[&ToolDescriptor]) {
    for tool in tools {
        out.push_str(&tool.listing_line());
        out.push('\n');
    }
}

fn problems(unknown: &[String], not_loadable: &[String]) -> Option<String> {
    let mut parts = Vec::new();
    if !unknown.is_empty() {
        parts.push(format!("Not loaded: {}.", unknown.join(", ")));
    }
    if !not_loadable.is_empty() {
        parts.push(format!(
            "Already in your tool list: {}.",
            not_loadable.join(", ")
        ));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Turns deferred disclosure on for `registry`: registers the discovery tool
/// and has requests carry only core and loaded tools from now on.
///
/// Call after the last tool is registered or removed it will list; the tool
/// reads the registry live, so later changes (MCP servers connecting) show up
/// in `search` and `list`.
///
/// `ranker` replaces [`Bm25Ranker`] for `search` when given.
pub fn enable(
    registry: &mut ToolRegistry,
    mode: Arc<Mutex<AgentMode>>,
    ranker: Option<Arc<dyn ToolRanker>>,
) {
    let mut tool = ToolDiscoveryTool::new(registry, mode);
    if let Some(ranker) = ranker {
        tool = tool.with_ranker(ranker);
    }
    registry.register(tool);
    registry.set_disclosure(Disclosure::Deferred);
}

/// The system-prompt section that tells the model how deferred tools work
/// and, while there are few enough, which ones exist.
///
/// Rendered once when the session starts; the discovery tool's `list` is the
/// live view when tools come and go (MCP servers connecting).
#[must_use]
pub fn index_section(registry: &ToolRegistry, mode: AgentMode) -> Option<String> {
    if registry.disclosure() != Disclosure::Deferred || registry.get(DISCOVERY_TOOL).is_none() {
        return None;
    }
    let tools = registry.catalog().deferred_descriptors(mode);
    if tools.is_empty() {
        return None;
    }
    let mut out = String::from(
        "## Deferred Tools\n\n\
         These tools exist but are not in your tool list. Load a tool before you call it: call \
         `tools` with action=load and names; from your next step it is in your tool list and you \
         call it directly. action=search (query) finds tools, action=list pages through all of \
         them, action=help shows a definition without loading it. A built-in tool does its job \
         with nothing to install: when one fits the task, prefer it over a skill or a script \
         that installs packages.",
    );
    if tools.iter().any(|t| t.name == "document_locate") {
        out.push_str(" For PDF files that is document_locate (which pages) and document_convert.");
    }
    out.push_str("\n\n");
    if tools.len() <= INLINE_INDEX_LIMIT {
        out.push_str("Not loaded:\n");
        for tool in &tools {
            out.push_str(&format!("- {}\n", tool.index_line()));
        }
    } else {
        out.push_str(&format!(
            "{} tools are not loaded, by category:",
            tools.len()
        ));
        let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
        for tool in &tools {
            *counts.entry(tool.category()).or_default() += 1;
        }
        for (category, count) in counts {
            out.push_str(&format!(" {category} {count};"));
        }
        out.push('\n');
    }
    Some(out)
}
