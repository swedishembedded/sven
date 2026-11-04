// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! Spec: which skills, subagents and knowledge documents a session is told
//! about, and when.
//!
//! An instruction without an activation block is listed from the start, as
//! before. One with an activation block is left out of the system prompt and
//! announced in the per-turn block once its condition holds: a file it covers
//! was touched or named, the machine reached a state it waits for, or the
//! session runs in a mode it names.

use std::path::{Path, PathBuf};

use serde_json::json;
use sven_turn::instructions::{Activator, InstructionCatalog};
use sven_turn::prompts::{system_prompt, PromptContext};
use sven_vocab::AgentMode;
use sven_workspace::{
    Activation, AgentInfo, KnowledgeInfo, SharedAgents, SharedKnowledge, SharedSkills, SkillInfo,
    SvenSkillMeta,
};

const ROOT: &str = "/proj";

fn skill(command: &str, description: &str, activation: Option<Activation>) -> SkillInfo {
    SkillInfo {
        command: command.to_string(),
        name: command.rsplit('/').next().unwrap().to_string(),
        description: description.to_string(),
        version: None,
        skill_md_path: PathBuf::from(format!("skills/{command}/SKILL.md")),
        skill_dir: PathBuf::from(format!("skills/{command}")),
        content: "body".to_string(),
        sven_meta: activation.map(|a| SvenSkillMeta {
            activation: Some(a),
            ..SvenSkillMeta::default()
        }),
    }
}

fn agent(name: &str, activation: Option<Activation>) -> AgentInfo {
    AgentInfo {
        name: name.to_string(),
        description: format!("{name} agent"),
        model: None,
        readonly: false,
        is_background: false,
        content: String::new(),
        agent_md_path: PathBuf::from(format!("agents/{name}.md")),
        knowledge: vec![],
        activation,
    }
}

fn doc(stem: &str, activation: Option<Activation>) -> KnowledgeInfo {
    KnowledgeInfo {
        subsystem: format!("{stem} subsystem"),
        files: vec![],
        updated: None,
        path: PathBuf::from(format!(".sven/knowledge/{stem}.md")),
        body: "body".to_string(),
        activation,
    }
}

fn on_paths(globs: &[&str]) -> Option<Activation> {
    Some(Activation {
        paths: globs.iter().map(|g| (*g).to_string()).collect(),
        ..Activation::default()
    })
}

fn catalog() -> InstructionCatalog {
    InstructionCatalog::new(
        SharedSkills::new(vec![
            skill("always-on", "Always listed.", None),
            skill(
                "auth",
                "Work on authentication.",
                on_paths(&["src/auth/**"]),
            ),
        ]),
        SharedAgents::new(vec![
            agent("verifier", None),
            agent(
                "executor-helper",
                Some(Activation {
                    states: vec!["Executing".to_string()],
                    ..Activation::default()
                }),
            ),
        ]),
        SharedKnowledge::new(vec![
            doc("overview", None),
            doc(
                "planning",
                Some(Activation {
                    modes: vec!["plan".to_string()],
                    ..Activation::default()
                }),
            ),
        ]),
    )
}

fn activator(mode: &str) -> Activator {
    Activator::new(catalog(), Some(PathBuf::from(ROOT)), mode.to_string())
}

#[test]
fn a_touched_file_activates_the_instructions_that_cover_it() {
    let a = activator("agent");
    let paths = a.paths_of_call(&json!({"path": "/proj/src/auth/login.rs"}));
    assert_eq!(paths, ["src/auth/login.rs"]);
    assert_eq!(a.activated_by_paths(&paths), ["skill:auth"]);
    let other = a.paths_of_call(&json!({"path": "/proj/src/billing/x.rs"}));
    assert!(a.activated_by_paths(&other).is_empty());
}

#[test]
fn a_path_outside_the_project_activates_nothing() {
    let a = activator("agent");
    assert!(a.paths_of_call(&json!({"path": "/etc/hosts"})).is_empty());
}

#[test]
fn a_task_that_names_a_file_activates_what_covers_it() {
    let a = activator("agent");
    let plan = a.plan_turn(
        &[],
        "Idle",
        "please fix the bug in src/auth/login.rs, thanks",
    );
    assert_eq!(plan.activate, ["skill:auth"]);
    let block = plan.block.expect("an active instruction is announced");
    assert!(block.contains("auth"), "{block}");
}

#[test]
fn a_state_and_a_mode_activate_what_waits_for_them() {
    let a = activator("plan");
    let plan = a.plan_turn(&[], "Executing", "hello");
    assert_eq!(
        plan.activate,
        ["agent:executor-helper", "knowledge:planning"]
    );
    let a = activator("agent");
    assert!(a.plan_turn(&[], "Idle", "hello").activate.is_empty());
}

#[test]
fn an_instruction_that_is_already_active_stays_announced_without_being_activated_again() {
    let a = activator("agent");
    let active = vec!["skill:auth".to_string()];
    let plan = a.plan_turn(&active, "Idle", "unrelated task");
    assert!(plan.activate.is_empty());
    assert!(plan.block.expect("still announced").contains("auth"));
}

#[test]
fn the_block_never_repeats_what_the_system_prompt_lists() {
    let a = activator("agent");
    let active = vec!["skill:auth".to_string()];
    let block = a.plan_turn(&active, "Idle", "x").block.unwrap();
    assert!(!block.contains("always-on"), "{block}");
    assert!(a.plan_turn(&[], "Idle", "x").block.is_none());
}

#[test]
fn an_id_the_catalog_no_longer_has_is_ignored() {
    let a = activator("agent");
    let active = vec!["skill:gone".to_string()];
    assert!(a.plan_turn(&active, "Idle", "x").block.is_none());
}

fn prompt(c: &InstructionCatalog) -> String {
    let ctx = PromptContext {
        skills: c.skills.get(),
        agents: c.agents.get(),
        knowledge: c.knowledge.get(),
        project_root: Some(Path::new(ROOT)),
        ..PromptContext::default()
    };
    system_prompt(AgentMode::Agent, None, ctx)
}

#[test]
fn the_system_prompt_leaves_conditional_instructions_to_the_per_turn_block() {
    let p = prompt(&catalog());
    assert!(p.contains("always-on") && p.contains("verifier") && p.contains("overview"));
    assert!(!p.contains("Work on authentication."), "{p}");
    assert!(!p.contains("executor-helper") && !p.contains("planning subsystem"));
    assert!(
        p.contains("1 more skill"),
        "the model is told further skills exist: {p}"
    );
}

#[test]
fn a_catalog_without_activation_blocks_renders_exactly_as_before() {
    let plain = InstructionCatalog::new(
        SharedSkills::new(vec![skill("always-on", "Always listed.", None)]),
        SharedAgents::new(vec![agent("verifier", None)]),
        SharedKnowledge::new(vec![doc("overview", None)]),
    );
    let p = prompt(&plain);
    assert!(
        !p.contains("more skill") && !p.contains("more agent") && !p.contains("more knowledge")
    );
}

#[test]
fn a_pinned_skill_is_listed_whatever_its_activation() {
    let pinned = SkillInfo {
        sven_meta: Some(SvenSkillMeta {
            always: true,
            activation: on_paths(&["src/never/**"]),
            ..SvenSkillMeta::default()
        }),
        ..skill("pinned", "Pinned skill.", None)
    };
    let c = InstructionCatalog::new(
        SharedSkills::new(vec![pinned]),
        SharedAgents::empty(),
        SharedKnowledge::empty(),
    );
    assert!(prompt(&c).contains("Pinned skill."));
    let a = Activator::new(c, Some(PathBuf::from(ROOT)), "agent".into());
    assert!(a
        .activated_by_paths(&["src/never/x.rs".to_string()])
        .is_empty());
}
