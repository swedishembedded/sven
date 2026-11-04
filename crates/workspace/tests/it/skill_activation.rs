// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! Spec: a skill states when it applies in its `sven.activation` block, and a
//! file that states it wrongly is refused rather than guessed at.

use std::fs;
use std::path::Path;

use sven_workspace::{discover_skills_anchored, parse_skill_file, parse_skill_frontmatter};
use tempfile::TempDir;

fn write_skill(root: &Path, command: &str, frontmatter_extra: &str) {
    let dir = root.join(".sven").join("skills").join(command);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\ndescription: A skill.\n{frontmatter_extra}---\n\nBody."),
    )
    .unwrap();
}

#[test]
fn a_skill_without_activation_has_none_and_a_stable_id() {
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "sven/plan", "");
    let skills = discover_skills_anchored(Some(tmp.path()), None, false);
    let skill = skills.iter().find(|s| s.command == "sven/plan").unwrap();
    assert!(skill.activation().is_none());
    assert_eq!(skill.id(), "skill:sven/plan");
}

#[test]
fn a_skill_declares_activation_in_its_sven_block() {
    let raw = "---\ndescription: x\nsven:\n  activation:\n    paths: [\"src/auth/**\"]\n    modes: [plan]\n---\nB";
    let a = parse_skill_file(raw)
        .expect("parses")
        .sven_meta
        .unwrap()
        .activation
        .unwrap();
    assert_eq!(a.paths, vec!["src/auth/**".to_string()]);
    assert_eq!(a.modes, vec!["plan".to_string()]);
}

#[test]
fn an_unknown_activation_key_refuses_the_skill_and_names_the_key() {
    let raw = "---\ndescription: x\nsven:\n  activation:\n    pathz: [a]\n---\nB";
    let err = parse_skill_frontmatter(raw).err().expect("refused");
    assert!(err.contains("pathz"), "{err}");
    assert!(parse_skill_file(raw).is_none());
}

#[test]
fn an_activation_without_a_condition_refuses_the_skill() {
    let raw = "---\ndescription: x\nsven:\n  activation: {}\n---\nB";
    assert!(parse_skill_frontmatter(raw).is_err());
}

#[test]
fn a_refused_skill_is_not_discovered() {
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "bad", "sven:\n  activation:\n    pathz: [a]\n");
    let skills = discover_skills_anchored(Some(tmp.path()), None, false);
    assert!(skills.iter().all(|s| s.command != "bad"));
}
