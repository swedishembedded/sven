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
        scratch_dir: None,
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
        1697,
        "8cbfb87f0962126808d289296fdc51ebfa21946e5f7bd675f83aa1a47c28b713",
    ),
    (
        "blank-volatile",
        3132,
        "6d673db7a68e2266c08afebbfd39082d3f301ad2945c77bc91b6c709b80d9432",
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
        2833,
        "1c4c77f1a7a9dcd658763f6ab6d742c9f778d4329fe231de0a4b1238a42fe712",
    ),
    (
        "empty/agent",
        1685,
        "67ad50a474097c062e4f9792aaf57997bf813b4902b2a55478ea0254bdb9a6ee",
    ),
    (
        "empty/chat",
        1694,
        "2c7f32dd97deb952a25ad07d0356e398cf80263d96ee2a2028ef88adad2c14ba",
    ),
    (
        "empty/plan",
        2004,
        "8874d62934b4175ececaf74d5d5871985c86213a4d9050659300e8d020a36466",
    ),
    (
        "empty/research",
        1722,
        "e2bdc91b50ff9f0781fd609a392d0bfedc63d1623006d5b5db3229da7f7d609a",
    ),
    (
        "empty/sdlc",
        1799,
        "3cdbb77a564799999f55d4f47f6f05328433f854487d1ec877be4b73d908308e",
    ),
    (
        "full/agent",
        3169,
        "6a51063963e74972f060eec8274b9076df34f65b6964e35de6a5511c6c49c1a7",
    ),
    (
        "full/chat",
        3178,
        "2a97f021c20b33f013561f29ab8ad9c5ca3d33176ffdd43c7a1eae0d72d9b5a2",
    ),
    (
        "full/plan",
        3488,
        "4badc6a7d3c53b0cd4cc3c660d9c48a5596bb6169729a6ca77322e01cf45686d",
    ),
    (
        "full/research",
        3206,
        "de8a484c4a1009345511d16b661fe1e0dd971f9a1999499cf4bb9392ec3b3590",
    ),
    (
        "full/sdlc",
        3283,
        "e031dff7ae9364813cd5e678bccb01466eb3fcaaa9d5e5f7ee02d43caa2b8bfe",
    ),
];
