// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the prompts of LLM
// agents reproducible for its clients. If your team needs expertise in
// attributing model behaviour to the exact prompt that caused it then you can
// procure our services by sending an email to info@swedishembedded.com.
//! Spec: the system prompt changes only on purpose.
//!
//! A model's behaviour depends on every byte of the system prompt, so a
//! refactor of how it is composed must not change one, and a change of its
//! text must show up here. Each case renders a prompt and compares its length
//! and SHA-256 with the pinned value.

use std::path::PathBuf;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use sven_turn::instructions::{InstructionDisclosure, ListingDisclosure};
use sven_turn::prompts::{system_prompt, PromptContext};
use sven_vocab::AgentMode;
use sven_workspace::{Activation, AgentInfo, KnowledgeInfo, SkillInfo, SvenSkillMeta};

fn skill(command: &str, activation: Option<Activation>) -> SkillInfo {
    SkillInfo {
        command: command.to_string(),
        name: command.to_string(),
        description: format!("Use the {command} skill when asked."),
        version: None,
        skill_md_path: PathBuf::from(format!("/proj/skills/{command}/SKILL.md")),
        skill_dir: PathBuf::from(format!("/proj/skills/{command}")),
        content: "body".to_string(),
        sven_meta: activation.map(|a| SvenSkillMeta {
            activation: Some(a),
            ..SvenSkillMeta::default()
        }),
    }
}

fn agent(name: &str) -> AgentInfo {
    AgentInfo {
        name: name.to_string(),
        description: format!("{name} agent"),
        model: None,
        readonly: false,
        is_background: false,
        content: String::new(),
        agent_md_path: PathBuf::from(format!("/proj/agents/{name}.md")),
        knowledge: vec![],
        activation: None,
    }
}

fn doc(stem: &str) -> KnowledgeInfo {
    KnowledgeInfo {
        subsystem: format!("{stem} subsystem"),
        files: vec!["src/**".to_string()],
        updated: Some("2026-01-01".to_string()),
        path: PathBuf::from(format!("/proj/.sven/knowledge/{stem}.md")),
        body: "body".to_string(),
        activation: None,
    }
}

fn path_activation() -> Option<Activation> {
    Some(Activation {
        paths: vec!["src/auth/**".to_string()],
        ..Activation::default()
    })
}

/// Every optional block set, with an activation-gated skill so the "announced
/// when relevant" note is rendered too.
fn full<'a>(root: &'a std::path::Path, context_file: &'a std::path::Path) -> PromptContext<'a> {
    PromptContext {
        project_root: Some(root),
        git_context: Some("## Git\nbranch: main"),
        project_context_file: Some(context_file),
        ci_context: Some("## CI\nrunner: ci-1"),
        append: Some("Extra instructions."),
        skills: Arc::from(vec![
            skill("review", None),
            skill("auth", path_activation()),
        ]),
        agents: Arc::from(vec![agent("verifier")]),
        knowledge: Arc::from(vec![doc("overview")]),
        knowledge_drift_note: Some("## Knowledge Drift\noverview is stale"),
        tool_index: Some("## Deferred Tools\n\nNot loaded:\n- grep\n\n"),
        disclosure: InstructionDisclosure::default(),
    }
}

fn digest(text: &str) -> (usize, String) {
    let sum = Sha256::digest(text.as_bytes());
    (text.len(), sum.iter().map(|b| format!("{b:02x}")).collect())
}

/// The working directory is the one machine-dependent byte run of a prompt
/// without a project root.
fn without_cwd(text: String) -> String {
    let cwd = std::env::current_dir().unwrap();
    text.replace(&cwd.display().to_string(), "<cwd>")
}

fn cases() -> Vec<(String, String)> {
    let root = PathBuf::from("/proj");
    let file = PathBuf::from("/proj/AGENTS.md");
    let mut out = Vec::new();
    for mode in [
        AgentMode::Research,
        AgentMode::Plan,
        AgentMode::Agent,
        AgentMode::Chat,
        AgentMode::Sdlc,
    ] {
        out.push((
            format!("empty/{mode}"),
            without_cwd(system_prompt(mode, None, PromptContext::default())),
        ));
        out.push((
            format!("full/{mode}"),
            system_prompt(mode, None, full(&root, &file)),
        ));
    }
    let deferred = PromptContext {
        disclosure: InstructionDisclosure {
            skills: ListingDisclosure::Deferred,
            subagents: ListingDisclosure::Deferred,
            knowledge: ListingDisclosure::Deferred,
        },
        ..full(&root, &file)
    };
    out.push((
        "deferred".into(),
        system_prompt(AgentMode::Agent, None, deferred),
    ));
    // A block that is present but empty still takes its separator.
    let blank = PromptContext {
        git_context: Some(""),
        ci_context: Some(""),
        ..full(&root, &file)
    };
    out.push((
        "blank-volatile".into(),
        system_prompt(AgentMode::Agent, None, blank),
    ));
    let append_only = PromptContext {
        append: Some("Only this."),
        ..PromptContext::default()
    };
    out.push((
        "append-only".into(),
        without_cwd(system_prompt(AgentMode::Agent, None, append_only)),
    ));
    let no_append = PromptContext {
        append: None,
        ..full(&root, &file)
    };
    out.push((
        "custom".into(),
        system_prompt(AgentMode::Agent, Some("Be brief.\n"), no_append),
    ));
    out.push((
        "custom+append".into(),
        system_prompt(AgentMode::Agent, Some("Be brief.\n\n"), full(&root, &file)),
    ));
    out
}

#[test]
fn rendered_prompts_are_byte_identical_to_the_captured_composer() {
    let mut mismatches = Vec::new();
    for (name, text) in cases() {
        let (len, sha) = digest(&text);
        println!("{name:<24} {len:>6} {sha}");
        match GOLDEN.iter().find(|(n, ..)| *n == name) {
            Some((_, want_len, want_sha)) if *want_len == len && *want_sha == sha => {}
            other => mismatches.push(format!("{name}: got {len} {sha}, want {other:?}")),
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// `(case, length in bytes, SHA-256)` of the current prompt. A change here is a
/// deliberate change of what every model reads.
const GOLDEN: &[(&str, usize, &str)] = &[
    (
        "append-only",
        1664,
        "0a089fd7019ef8c16c5700c0c66b1e3731fc090be7c667ed5121ef6e13069f72",
    ),
    (
        "blank-volatile",
        3407,
        "86c4dd4d9ca5d0b751f030f52afc8e87de6563c0583a16020fa7192f57728d9a",
    ),
    (
        "custom+append",
        30,
        "8eedc9a6aff004f8cdb78719369861ffb6fd2accb77a221c52f9d533cecf143a",
    ),
    (
        "custom",
        10,
        "96fb1c7f068c5ce63e2b45fc4aea602d48d5302be6ca033f3e1f0c7148558a49",
    ),
    (
        "deferred",
        3289,
        "5fda72023a607b0bfd48057ab007351f659051e143ac5efcd956ed6e6369c4ba",
    ),
    (
        "empty/agent",
        1652,
        "e59d68529726d2e4f465e356197553dd2b826fe5b536e278d6722436a90fe2f0",
    ),
    (
        "empty/chat",
        1605,
        "a7a2e8524c7ace1f4e6a8cd537a7901666186bee6f39397c22de314328c3646d",
    ),
    (
        "empty/plan",
        2179,
        "6e8c7fb00580015bda2d2a366129afeceefcb3303115e1928aa032061ea6f7f6",
    ),
    (
        "empty/research",
        1712,
        "23676f7009055ac9ea89723777d750688e24fa39d6f1625f3b76b04e4671ba1c",
    ),
    (
        "empty/sdlc",
        1740,
        "ac63c3faa49447558133509696d96090b05d2bbd95dca0a0aed13103661ad62c",
    ),
    (
        "full/agent",
        3444,
        "2d97883f7f402c06507dc7e3b54905bc4790a21bbe2ee2bb7ef05fec77625fb8",
    ),
    (
        "full/chat",
        3397,
        "13fd2cfce19a4ca459a3e86a5cd60837cd65ec060ecbaa88d9702639c8b6865a",
    ),
    (
        "full/plan",
        3971,
        "72697074589584c7f3d1c4f354df2e1973230d0af0aceaa83fc5ce33b34f789b",
    ),
    (
        "full/research",
        3504,
        "f75a6a722fd3d2dc1062a026eb414f7ab0541aa660fd2138948dfc844f09695e",
    ),
    (
        "full/sdlc",
        3532,
        "2481e3816bde4151beb693e54b0a4ff9a598990a44a9eb1b7e549ec4e70b9399",
    ),
];
