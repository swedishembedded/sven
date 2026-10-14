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

use sven_turn::instructions::SkillDisclosure;
use sven_turn::prompts::{system_prompt, PromptContext};
use sven_vocab::AgentMode;
use sven_workspace::{Activation, SkillInfo, SvenSkillMeta};

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

fn prompt(skills: Vec<SkillInfo>, disclosure: SkillDisclosure) -> String {
    let ctx = PromptContext {
        skills: skills.into(),
        skill_disclosure: disclosure,
        ..PromptContext::default()
    };
    system_prompt(AgentMode::Agent, None, ctx)
}

fn skills_section(prompt: &str) -> &str {
    let start = prompt.find("## Skills").expect("a skills section");
    let rest = &prompt[start..];
    rest.find("\n\n## ").map_or(rest, |end| &rest[..end])
}

#[test]
fn the_index_lists_a_command_and_a_short_summary_not_the_whole_description() {
    let p = prompt(
        vec![skill("billing", LONG, None)],
        SkillDisclosure::Deferred,
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
        SkillDisclosure::Deferred,
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
    let full = prompt(many.clone(), SkillDisclosure::Full);
    let deferred = prompt(many, SkillDisclosure::Deferred);
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
    let p = prompt(many, SkillDisclosure::Deferred);
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
        SkillDisclosure::Deferred,
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
        SkillDisclosure::Deferred,
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
    let p = prompt(vec![skill("billing", LONG, None)], SkillDisclosure::Full);
    assert!(p.contains("<available_skills>") && p.contains("<description>"));
}

#[test]
fn no_skills_means_no_section_in_either_mode() {
    assert!(!prompt(vec![], SkillDisclosure::Deferred).contains("## Skills"));
}
