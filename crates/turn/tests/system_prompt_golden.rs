// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the prompts of LLM
// agents reproducible for its clients. If your team needs expertise in
// attributing model behaviour to the exact prompt that caused it then you can
// procure our services by sending an email to info@swedishembedded.com.
//! Spec: the system prompt is byte-identical across refactors of how it is
//! composed.
//!
//! A model's behaviour depends on every byte of the system prompt, so
//! restructuring the composer must not change one. Each case renders a prompt
//! and compares its length and SHA-256 with the value captured from the
//! composer before it was split into named sections.

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

/// `(case, length in bytes, SHA-256)`, captured from the composer before it
/// was split into sections.
const GOLDEN: &[(&str, usize, &str)] = &[
    (
        "append-only",
        5634,
        "3499494a770886ee62ce40654683bf55cfc116184f636c22bed93edc9a5af592",
    ),
    (
        "blank-volatile",
        7377,
        "e190106597bdcc98a6615beb7241c611f56bef2a025bf89f85f234f0ae96af4a",
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
        7259,
        "c24c881b1fe2851b73f30c658bf095e51f14a4d1d6bfaf19abf40724cb927052",
    ),
    (
        "empty/agent",
        5622,
        "7cb3edc20d8949fd8918361f1194b31c775791b4e8dca1f858c162ac272ad150",
    ),
    (
        "empty/chat",
        5218,
        "ae3bfea117806ffed2dba4dc031ed070c7b7a4482d1930048e63301b319fda6f",
    ),
    (
        "empty/plan",
        5792,
        "e6d9f07e7bb8ba6298ee3212fcbee35ae7d7eb0d864025a63fc8bdf2ec80a053",
    ),
    (
        "empty/research",
        5325,
        "ee418d9497ebe70ebe78f4076b3cc098701e38a92072c9b44ad5170d5ffe6e88",
    ),
    (
        "empty/sdlc",
        5353,
        "28ce6e314c1dcc0dd9333823ebbf63aa0228e343814ac7f22d6db8606bcb2f10",
    ),
    (
        "full/agent",
        7414,
        "e3b37111c1db69d9f3419e31decce5a1213ec4637284a0e92b8209bbeca2c5b2",
    ),
    (
        "full/chat",
        7010,
        "dd0447f811aeb72c1ac3003f29a79e65c079d30e81067560e0c638f0b3c72cb4",
    ),
    (
        "full/plan",
        7584,
        "da0e9d93300d5a2c3b642fa38e85a99f4c52dfc2fcf08147b4542e25baffe062",
    ),
    (
        "full/research",
        7117,
        "ecd73550ab019fc88c6ca9452be6589c6acdc396d795140524570c6d99cb692a",
    ),
    (
        "full/sdlc",
        7145,
        "2f5ce943456b95539814fb651459e117f4fda2ebacdd1d829ca932d54e2a6282",
    ),
];
