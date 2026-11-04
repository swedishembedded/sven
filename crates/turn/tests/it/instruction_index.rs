// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! Spec: under deferred skill disclosure the system prompt carries a compact
//! index of the skills instead of their full metadata, and never hides a skill
//! the session needs.

use std::path::PathBuf;

use sven_turn::instructions::{InstructionDisclosure, ListingDisclosure};
use sven_turn::prompts::{system_prompt, PromptContext};
use sven_vocab::AgentMode;
use sven_workspace::{Activation, AgentInfo, KnowledgeInfo, SkillInfo, SvenSkillMeta};

fn skill(command: &str, description: &str, meta: Option<SvenSkillMeta>) -> SkillInfo {
    SkillInfo {
        command: command.to_string(),
        name: command.rsplit('/').next().unwrap().to_string(),
        description: description.to_string(),
        version: None,
        skill_md_path: PathBuf::from(format!("skills/{command}/SKILL.md")),
        skill_dir: PathBuf::from(format!("skills/{command}")),
        content: "body".to_string(),
        sven_meta: meta,
    }
}

const LONG: &str = "This skill should be used when the user asks to \"work on the billing \
service\", \"change billing behaviour\", or \"explain how billing is wired\". It covers the \
module layout and the commands that build it.";

fn prompt(skills: Vec<SkillInfo>, disclosure: ListingDisclosure) -> String {
    let ctx = PromptContext {
        skills: skills.into(),
        disclosure: InstructionDisclosure {
            skills: disclosure,
            ..InstructionDisclosure::default()
        },
        ..PromptContext::default()
    };
    system_prompt(AgentMode::Agent, None, ctx)
}

fn section<'a>(prompt: &'a str, heading: &str) -> &'a str {
    let start = prompt.find(heading).expect("a section");
    let rest = &prompt[start..];
    rest.find("\n\n## ").map_or(rest, |end| &rest[..end])
}

fn skills_section(prompt: &str) -> &str {
    section(prompt, "## Skills")
}

#[test]
fn the_index_lists_a_command_and_a_short_summary_not_the_whole_description() {
    let p = prompt(
        vec![skill("billing", LONG, None)],
        ListingDisclosure::Deferred,
    );
    let section = skills_section(&p);
    assert!(section.contains("- billing: "), "{section}");
    assert!(!section.contains("module layout"), "{section}");
    assert!(!section.contains("<description>"), "{section}");
    let line = section
        .lines()
        .find(|l| l.starts_with("- billing: "))
        .unwrap();
    let summary = line.trim_start_matches("- billing: ");
    assert!(summary.chars().count() <= 80, "{summary}");
    assert!(
        summary.starts_with("the user asks to"),
        "boilerplate is dropped: {summary}"
    );
}

#[test]
fn the_index_says_how_to_load_and_how_to_search() {
    let p = prompt(
        vec![skill("billing", LONG, None)],
        ListingDisclosure::Deferred,
    );
    let section = skills_section(&p);
    assert!(
        section.contains("\"load\"") && section.contains("\"search\""),
        "{section}"
    );
}

#[test]
fn the_index_is_much_smaller_than_the_full_listing() {
    let many: Vec<SkillInfo> = (0..40)
        .map(|i| skill(&format!("area{}/skill{i}", i % 10), LONG, None))
        .collect();
    let full = prompt(many.clone(), ListingDisclosure::Full);
    let deferred = prompt(many, ListingDisclosure::Deferred);
    let (deferred, full) = (skills_section(&deferred).len(), skills_section(&full).len());
    assert!(
        deferred * 2 < full,
        "deferred {deferred} vs full {full} characters"
    );
}

#[test]
fn above_the_limit_the_index_gives_counts_by_package() {
    let many: Vec<SkillInfo> = (0..60)
        .map(|i| skill(&format!("pkg{}/s{i}", i % 3), LONG, None))
        .collect();
    let p = prompt(many, ListingDisclosure::Deferred);
    let section = skills_section(&p);
    assert!(section.contains("- pkg0: 20"), "{section}");
    assert!(!section.contains("pkg0/s0"), "{section}");
}

#[test]
fn a_pinned_skill_keeps_its_full_entry_in_the_index() {
    let pinned = skill(
        "style",
        LONG,
        Some(SvenSkillMeta {
            always: true,
            ..SvenSkillMeta::default()
        }),
    );
    let p = prompt(
        vec![pinned, skill("other", LONG, None)],
        ListingDisclosure::Deferred,
    );
    let section = skills_section(&p);
    assert!(section.contains("<command>style</command>") && section.contains("module layout"));
    assert!(section.contains("- other: "));
}

#[test]
fn a_user_invocable_only_skill_is_not_indexed_and_a_conditional_one_is_left_to_its_announcement() {
    let hidden = skill(
        "secret",
        LONG,
        Some(SvenSkillMeta {
            user_invocable_only: true,
            ..SvenSkillMeta::default()
        }),
    );
    let conditional = skill(
        "scoped",
        LONG,
        Some(SvenSkillMeta {
            activation: Some(Activation {
                paths: vec!["src/**".into()],
                ..Activation::default()
            }),
            ..SvenSkillMeta::default()
        }),
    );
    let p = prompt(
        vec![hidden, conditional, skill("open", LONG, None)],
        ListingDisclosure::Deferred,
    );
    let section = skills_section(&p);
    assert!(
        !section.contains("secret") && !section.contains("scoped"),
        "{section}"
    );
    assert!(
        section.contains("- open: ") && section.contains("1 more skill"),
        "{section}"
    );
}

#[test]
fn full_disclosure_is_the_listing_it_always_was() {
    let p = prompt(vec![skill("billing", LONG, None)], ListingDisclosure::Full);
    assert!(p.contains("<available_skills>") && p.contains("<description>"));
}

#[test]
fn no_skills_means_no_section_in_either_mode() {
    assert!(!prompt(vec![], ListingDisclosure::Deferred).contains("## Skills"));
}

// ── Subagents and knowledge ──────────────────────────────────────────────────

fn agent(name: &str, readonly: bool, activation: Option<Activation>) -> AgentInfo {
    AgentInfo {
        name: name.to_string(),
        description: format!(
            "Use this subagent when the task calls for a {name}. It works on one bounded \
             question, reads before it writes, and returns a short report."
        ),
        model: Some("fast".to_string()),
        readonly,
        is_background: false,
        content: String::new(),
        agent_md_path: PathBuf::from(format!("agents/{name}.md")),
        knowledge: vec![],
        activation,
    }
}

fn doc(subsystem: &str, activation: Option<Activation>) -> KnowledgeInfo {
    KnowledgeInfo {
        subsystem: subsystem.to_string(),
        files: vec![
            format!("src/{subsystem}/**"),
            format!("tests/{subsystem}/**"),
        ],
        updated: Some("2026-01-01".to_string()),
        path: PathBuf::from(format!(".sven/knowledge/{subsystem}.md")),
        body: "body".to_string(),
        activation,
    }
}

fn catalog_prompt(
    agents: Vec<AgentInfo>,
    knowledge: Vec<KnowledgeInfo>,
    disclosure: InstructionDisclosure,
) -> String {
    let ctx = PromptContext {
        agents: agents.into(),
        knowledge: knowledge.into(),
        disclosure,
        ..PromptContext::default()
    };
    system_prompt(AgentMode::Agent, None, ctx)
}

fn deferred() -> InstructionDisclosure {
    InstructionDisclosure {
        skills: ListingDisclosure::Deferred,
        subagents: ListingDisclosure::Deferred,
        knowledge: ListingDisclosure::Deferred,
    }
}

#[test]
fn the_subagent_index_is_a_name_and_a_short_summary() {
    let p = catalog_prompt(vec![agent("verifier", true, None)], vec![], deferred());
    let s = section(&p, "## Subagents");
    let line = s.lines().find(|l| l.starts_with("- verifier")).expect(s);
    assert!(line.contains("(readonly)"), "{line}");
    assert!(
        !s.contains("<description>") && !s.contains("<model>"),
        "{s}"
    );
    assert!(
        s.contains("\"agents\""),
        "how to read the full descriptions: {s}"
    );
    assert!(line.chars().count() <= 120, "{line}");
}

#[test]
fn the_subagent_index_is_smaller_than_the_full_listing() {
    let many = |_: ()| {
        (0..12)
            .map(|i| agent(&format!("role{i}"), false, None))
            .collect::<Vec<_>>()
    };
    let full = catalog_prompt(many(()), vec![], InstructionDisclosure::default());
    let compact = catalog_prompt(many(()), vec![], deferred());
    let (full, compact) = (
        section(&full, "## Subagents").len(),
        section(&compact, "## Subagents").len(),
    );
    assert!(compact * 2 < full, "{compact} vs {full}");
}

#[test]
fn the_knowledge_index_names_each_subsystem_with_the_files_it_covers() {
    let p = catalog_prompt(vec![], vec![doc("auth", None)], deferred());
    let s = section(&p, "## Knowledge Base");
    assert!(s.contains("- auth (src/auth/**, tests/auth/**)"), "{s}");
    assert!(!s.contains("<doc>") && !s.contains("updated"), "{s}");
    assert!(s.contains("search_knowledge"), "{s}");
}

#[test]
fn above_the_limit_the_knowledge_index_gives_a_count() {
    let docs: Vec<KnowledgeInfo> = (0..60).map(|i| doc(&format!("area{i}"), None)).collect();
    let p = catalog_prompt(vec![], docs, deferred());
    let s = section(&p, "## Knowledge Base");
    assert!(s.contains("60 documents"), "{s}");
    assert!(!s.contains("area7"), "{s}");
    assert!(s.contains("list_knowledge"), "{s}");
}

#[test]
fn conditional_subagents_and_documents_stay_out_of_the_indexes() {
    let when = Some(Activation {
        paths: vec!["src/**".into()],
        ..Activation::default()
    });
    let p = catalog_prompt(
        vec![
            agent("open", false, None),
            agent("scoped", false, when.clone()),
        ],
        vec![doc("open", None), doc("scoped", when)],
        deferred(),
    );
    assert!(!p.contains("scoped"), "{p}");
    assert!(p.contains("1 more subagent") && p.contains("1 more knowledge document"));
}

#[test]
fn each_listing_has_its_own_mode() {
    let only_agents = InstructionDisclosure {
        subagents: ListingDisclosure::Deferred,
        ..InstructionDisclosure::default()
    };
    let p = catalog_prompt(
        vec![agent("verifier", false, None)],
        vec![doc("auth", None)],
        only_agents,
    );
    assert!(section(&p, "## Subagents").contains("- verifier"));
    assert!(section(&p, "## Knowledge Base").contains("<knowledge_base>"));
}
