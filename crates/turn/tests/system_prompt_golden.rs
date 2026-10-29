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
        1680,
        "beeb02886d7bccfa12d429ef0dcccbaff12e1aa6fbcc3bc65d185e07192364e0",
    ),
    (
        "blank-volatile",
        3423,
        "0c2401b8cfefb1539e63890b30d8d2878a08fcabfcacc5a817eb8f2935e4ba77",
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
        3305,
        "9226528f6b7ad7722dd134503106372714689e580963d2b87ae6a8de21869258",
    ),
    (
        "empty/agent",
        1668,
        "e585831cd66b1f27bac5cf05983c9deafff52b47f317f890f2998a539e794bf3",
    ),
    (
        "empty/chat",
        1621,
        "cc375ed67040a1890a3fe1c4d601a8fb146584146226e177a3a42fec47317ea8",
    ),
    (
        "empty/plan",
        2195,
        "ca70c56999aeac7fa8e4e6ef7df05d4923605563e2207a162007acf568b14104",
    ),
    (
        "empty/research",
        1728,
        "a04d3fec8fedb7b13e994faad0e7d93a84a3ed1b6ffa68c64ba1b60d279e818b",
    ),
    (
        "empty/sdlc",
        1756,
        "c40cfa63980048a2c28b69dc8a3d975564924d02800a7c49b3cf55cdd551aa74",
    ),
    (
        "full/agent",
        3460,
        "c5cfb95dc3052494d6f6c524523f209a4341d609b85133ada85279e2a7dadf46",
    ),
    (
        "full/chat",
        3413,
        "f5d43e93579c3de3dc045a668b634bf6535741de3c7144b30d3657c1991d5eba",
    ),
    (
        "full/plan",
        3987,
        "38680a951ea6d991bfaee1b09d2c7b877b9b75578a457c14f767e71786630f1e",
    ),
    (
        "full/research",
        3520,
        "9bbe700dc98bb8bd0798ec19ac8f45f010acd13ec47d0a59d9f087e355b1866a",
    ),
    (
        "full/sdlc",
        3548,
        "178fa6baadcc73b15b6af3ab7bbfd00404f1d6a54490736631c7792004fa1c45",
    ),
];
