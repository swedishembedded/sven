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
        1757,
        "fd6c2f3169611bdde3424ecb8cef350d98f1a191ec5078786822c1cbb50dc20c",
    ),
    (
        "blank-volatile",
        3500,
        "a9ed945640abcca5cd5bb43172904bb13d5d5529a11fa4f6b30c922db3ccae0a",
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
        3382,
        "7b930cf52dc48d79587210f59450f79a726a0c2c24b67b85a28e538433f94610",
    ),
    (
        "empty/agent",
        1745,
        "2b9a1039ae22160ff843e53d144f3b2727e2e7a0e74d05c21b8030c5f92f8778",
    ),
    (
        "empty/chat",
        1698,
        "45db14a89a7c33cf7ac78607190ddb65da6f09eb1cdf205cd0d26d9369f20692",
    ),
    (
        "empty/plan",
        2272,
        "9f583abfbcae88a1d7797ea96a2c64f66a86e11e6659f5d0dbf5df41fe2a7913",
    ),
    (
        "empty/research",
        1805,
        "8032616dedffe041dc057ea11bd2dc569982b0930cbe78ed1225f5a03ca80912",
    ),
    (
        "empty/sdlc",
        1833,
        "a5e7fe4de2dfa5f158537732a04ec21e57448afb28270dea2f3618b674ad6cc3",
    ),
    (
        "full/agent",
        3537,
        "efa57a0bf17d319bb02f9318583f310baab639ac181ce5c168e8ba8615303fd8",
    ),
    (
        "full/chat",
        3490,
        "a1b739190cf5fc046d6ac60fd508a4a1829fb8920b617ec9a10dcb57b3f214e2",
    ),
    (
        "full/plan",
        4064,
        "34fdc2c5de3a1570284b21e3e70daac422103ca0c4ece8209a2fe728853e0582",
    ),
    (
        "full/research",
        3597,
        "8028edf39d8dd862969af7b5f8252d1b8f74e8429c5c4e2d5bd026bfa1f508ab",
    ),
    (
        "full/sdlc",
        3625,
        "01b061e9ad9ddead2f1f3d88b9a4faf24fdb05281c12ea6be5832a65825be6d8",
    ),
];
