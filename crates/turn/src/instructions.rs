// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! Which skills, subagents and knowledge documents a session is told about.
//!
//! An instruction with no `activation` block is *unconditional*: the system
//! prompt lists it from the start, exactly as it always did. One with an
//! activation block is *conditional*: the system prompt leaves it out (so the
//! cached prefix never depends on what a session touches) and the per-turn
//! block announces it once its condition holds. What has held is recorded in
//! the kernel as `Context::active_instructions`; this module only decides
//! which ids to add and how to word the announcement.
//!
//! Activation decides what the model is *told*, never what it may do: a
//! conditional skill still loads by exact command, and nothing here widens a
//! permission. A pinned instruction (`sven.always: true`) is listed whatever
//! its activation says.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sven_workspace::{
    is_active, ActivationFacts, AgentInfo, KnowledgeInfo, SharedAgents, SharedKnowledge,
    SharedSkills, SkillInfo,
};

/// How much of each skill's metadata the system prompt carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillDisclosure {
    /// Every skill's name and description, as the listing always was.
    #[default]
    Full,
    /// A compact index (command and a short summary); the description and the
    /// body load through the `skill` tool.
    Deferred,
}

/// The longest summary of an instruction in a listing, in characters.
pub const SUMMARY_CHARS: usize = 80;

/// Argument names whose string values are file paths a call touches.
const PATH_ARGS: &[&str] = &[
    "path",
    "paths",
    "file",
    "files",
    "file_path",
    "old_path",
    "new_path",
    "workdir",
];

/// The live skills, subagents and knowledge documents of a session.
#[derive(Clone, Default)]
pub struct InstructionCatalog {
    /// Discovered skills.
    pub skills: SharedSkills,
    /// Discovered subagents.
    pub agents: SharedAgents,
    /// Discovered knowledge documents.
    pub knowledge: SharedKnowledge,
}

impl InstructionCatalog {
    /// A catalog over the session's shared collections.
    #[must_use]
    pub fn new(skills: SharedSkills, agents: SharedAgents, knowledge: SharedKnowledge) -> Self {
        Self {
            skills,
            agents,
            knowledge,
        }
    }
}

/// `true` when the system prompt lists the skill from the start.
#[must_use]
pub fn skill_is_unconditional(skill: &SkillInfo) -> bool {
    skill.sven_meta.as_ref().is_some_and(|m| m.always)
        || skill.activation().is_none_or(|a| !a.is_conditional())
}

/// `true` when the system prompt lists the subagent from the start.
#[must_use]
pub fn agent_is_unconditional(agent: &AgentInfo) -> bool {
    agent
        .activation
        .as_ref()
        .is_none_or(|a| !a.is_conditional())
}

/// `true` when the system prompt lists the document from the start.
#[must_use]
pub fn knowledge_is_unconditional(doc: &KnowledgeInfo) -> bool {
    doc.activation.as_ref().is_none_or(|a| !a.is_conditional())
}

/// What one turn needs to know about instructions.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TurnInstructions {
    /// Ids newly active this turn, to be recorded in the kernel.
    pub activate: Vec<String>,
    /// The announcement of every active conditional instruction; `None` when
    /// there is none.
    pub block: Option<String>,
}

/// Decides activation from a session's mode, its machine state, the files its
/// tools touch and the files its task names.
pub struct Activator {
    catalog: InstructionCatalog,
    project_root: Option<PathBuf>,
    mode: String,
}

impl Activator {
    /// An activator for a session in `mode`, rooted at `project_root`.
    #[must_use]
    pub fn new(catalog: InstructionCatalog, project_root: Option<PathBuf>, mode: String) -> Self {
        Self {
            catalog,
            project_root,
            mode,
        }
    }

    /// The project-relative paths a tool call's arguments name. A path outside
    /// the project is not a project file and is dropped.
    #[must_use]
    pub fn paths_of_call(&self, args: &Value) -> Vec<String> {
        let mut found = Vec::new();
        if let Value::Object(map) = args {
            for key in PATH_ARGS {
                collect_strings(map.get(*key), &mut |s| found.extend(self.relative(s)));
            }
        }
        found
    }

    /// The ids of conditional instructions whose paths cover one of `paths`.
    #[must_use]
    pub fn activated_by_paths(&self, paths: &[String]) -> Vec<String> {
        self.activated(&ActivationFacts {
            paths,
            ..ActivationFacts::default()
        })
    }

    /// What a turn emitted in `state` for `task` adds to the active set
    /// `active`, and the announcement to send with it.
    #[must_use]
    pub fn plan_turn(&self, active: &[String], state: &str, task: &str) -> TurnInstructions {
        let named: Vec<String> = task_paths(task)
            .into_iter()
            .filter_map(|p| self.relative(&p))
            .collect();
        let facts = ActivationFacts {
            mode: Some(&self.mode),
            state: Some(state).filter(|s| !s.is_empty()),
            paths: &named,
        };
        let activate: Vec<String> = self
            .activated(&facts)
            .into_iter()
            .filter(|id| !active.contains(id))
            .collect();
        let mut all: Vec<&String> = active.iter().collect();
        all.extend(activate.iter());
        TurnInstructions {
            block: self.announce(&all),
            activate,
        }
    }

    /// Ids of every conditional instruction whose activation holds for `facts`.
    fn activated(&self, facts: &ActivationFacts<'_>) -> Vec<String> {
        let mut ids = Vec::new();
        for s in self.catalog.skills.get().iter() {
            if !skill_is_unconditional(s) && is_active(s.activation(), facts) {
                ids.push(s.id());
            }
        }
        for a in self.catalog.agents.get().iter() {
            if !agent_is_unconditional(a) && is_active(a.activation.as_ref(), facts) {
                ids.push(a.id());
            }
        }
        for k in self.catalog.knowledge.get().iter() {
            if !knowledge_is_unconditional(k) && is_active(k.activation.as_ref(), facts) {
                ids.push(k.id());
            }
        }
        ids
    }

    /// The announcement of the conditional instructions among `active`, in
    /// catalog order. An id the catalog no longer has is skipped.
    fn announce(&self, active: &[&String]) -> Option<String> {
        let wanted = |id: String| active.iter().any(|a| **a == id);
        let mut lines = Vec::new();
        for s in self.catalog.skills.get().iter() {
            if !skill_is_unconditional(s) && wanted(s.id()) {
                lines.push(format!(
                    "- skill `{}`: {}",
                    s.command,
                    summary(&s.description, SUMMARY_CHARS)
                ));
            }
        }
        for a in self.catalog.agents.get().iter() {
            if !agent_is_unconditional(a) && wanted(a.id()) {
                lines.push(format!(
                    "- subagent `{}`: {}",
                    a.name,
                    summary(&a.description, SUMMARY_CHARS)
                ));
            }
        }
        for k in self.catalog.knowledge.get().iter() {
            if !knowledge_is_unconditional(k) && wanted(k.id()) {
                lines.push(format!("- knowledge `{}`", k.subsystem));
            }
        }
        if lines.is_empty() {
            return None;
        }
        Some(format!(
            "## Active Instructions\n\n\
             These apply to what this session is working on. Load a skill with the \
             `skill` tool (`action: \"load\"`, `name`: its command), delegate with \
             `task`, read a document with `search_knowledge`.\n\n{}",
            lines.join("\n")
        ))
    }

    /// `path` relative to the project root, or `None` outside it.
    fn relative(&self, path: &str) -> Option<String> {
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            let root = self.project_root.as_deref()?;
            let rel = candidate.strip_prefix(root).ok()?;
            return Some(rel.to_string_lossy().into_owned()).filter(|s| !s.is_empty());
        }
        if candidate.components().any(|c| c.as_os_str() == "..") {
            return None;
        }
        Some(path.trim_start_matches("./").to_string()).filter(|s| !s.is_empty())
    }
}

/// The first line of `text`, whitespace collapsed, at most `max` characters
/// (the last replaced by `...` when it was cut).
#[must_use]
pub fn summary(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        return line;
    }
    let cut: String = line.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", cut.trim_end())
}

fn collect_strings(value: Option<&Value>, out: &mut impl FnMut(&str)) {
    match value {
        Some(Value::String(s)) => out(s),
        Some(Value::Array(items)) => items.iter().for_each(|v| collect_strings(Some(v), out)),
        _ => {}
    }
}

/// The path-like words of a task: words with a `/` or a file extension, with
/// surrounding quotes and trailing punctuation removed.
fn task_paths(task: &str) -> Vec<String> {
    task.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| {
                matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '<' | '>')
            })
            .trim_end_matches(['.', ',', ':', ';', '!', '?'])
        })
        .filter(|w| {
            w.contains('/')
                || w.rsplit_once('.')
                    .is_some_and(|(stem, ext)| !stem.is_empty() && (1..=5).contains(&ext.len()))
        })
        .map(str::to_string)
        .collect()
}
