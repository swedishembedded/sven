// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for attributing the behaviour of
// LLM agents to the exact prompt that produced it for its clients. If your
// team needs expertise in reproducible prompt engineering and prompt
// ablation then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Spec: the system prompt is composed of named sections whose byte spans
//! tile the text, so one section can be found, hashed and replaced by id.

use std::path::PathBuf;
use std::sync::Arc;

use sven_model::manifest::sha256_hex;
use sven_turn::prompts::{compose_system_prompt, system_prompt, PromptContext};
use sven_turn::AgentRuntimeContext;
use sven_vocab::AgentMode;
use sven_workspace::SkillInfo;

fn skill() -> SkillInfo {
    SkillInfo {
        command: "review".into(),
        name: "review".into(),
        description: "Review code.".into(),
        version: None,
        skill_md_path: PathBuf::from("/proj/skills/review/SKILL.md"),
        skill_dir: PathBuf::from("/proj/skills/review"),
        content: "body".into(),
        sven_meta: None,
    }
}

fn full<'a>(root: &'a std::path::Path) -> PromptContext<'a> {
    PromptContext {
        project_root: Some(root),
        git_context: Some("## Git\nbranch: main"),
        ci_context: Some("## CI\nrunner: ci-1"),
        append: Some("Extra instructions."),
        skills: Arc::from(vec![skill()]),
        ..PromptContext::default()
    }
}

fn ids(c: &sven_turn::prompts::ComposedPrompt) -> Vec<&str> {
    c.sections.sections.iter().map(|s| s.id.as_str()).collect()
}

#[test]
fn the_sections_tile_the_text_that_system_prompt_returns() {
    let root = PathBuf::from("/proj");
    for mode in [AgentMode::Research, AgentMode::Agent, AgentMode::Sdlc] {
        let composed = compose_system_prompt(mode, None, full(&root));
        assert_eq!(composed.text, system_prompt(mode, None, full(&root)));

        let mut at = 0;
        for s in &composed.sections.sections {
            assert_eq!(s.start, at, "{} starts where the last one ended", s.id);
            assert!(s.end > s.start, "{} is not empty", s.id);
            assert_eq!(s.sha256, sha256_hex(composed.text[s.range()].as_bytes()));
            at = s.end;
        }
        assert_eq!(at, composed.text.len());
        assert_eq!(
            composed.sections.sha256,
            sha256_hex(composed.text.as_bytes())
        );
    }
}

#[test]
fn the_default_prompt_names_its_sections_in_text_order() {
    let root = PathBuf::from("/proj");
    let composed = compose_system_prompt(AgentMode::Agent, None, full(&root));
    assert_eq!(
        ids(&composed),
        [
            "identity",
            "mode",
            "project",
            "git",
            "skills",
            "ci",
            "guidelines.working_method",
            "guidelines.tools",
            "append",
        ]
    );
}

#[test]
fn a_section_is_replaced_by_its_span_and_nothing_else_changes() {
    let root = PathBuf::from("/proj");
    let composed = compose_system_prompt(AgentMode::Agent, None, full(&root));
    let span = composed.sections.get("guidelines.tools").unwrap();
    let variant = format!(
        "{}\n\n### Tools\n- Use tools.{}",
        &composed.text[..span.start],
        &composed.text[span.end..]
    );
    let other = compose_system_prompt(AgentMode::Agent, None, full(&root));
    assert_eq!(other.text, composed.text, "composition is deterministic");
    assert!(variant.ends_with("- Use tools.\n\nExtra instructions."));
    assert_eq!(&variant[..span.start], &composed.text[..span.start]);
}

#[test]
fn an_override_is_one_section_and_an_append_another() {
    let only = compose_system_prompt(
        AgentMode::Agent,
        Some("Be brief.\n"),
        PromptContext::default(),
    );
    assert_eq!(only.text, "Be brief.\n");
    assert_eq!(ids(&only), ["custom"]);

    let both = compose_system_prompt(
        AgentMode::Agent,
        Some("Be brief.\n"),
        PromptContext {
            append: Some("More."),
            ..PromptContext::default()
        },
    );
    assert_eq!(both.text, "Be brief.\n\nMore.");
    assert_eq!(ids(&both), ["custom", "append"]);
    assert_eq!(both.sections.get("append").unwrap().range(), 9..16);
}

#[test]
fn the_runtime_context_returns_the_sections_of_the_message_it_builds() {
    let runtime = AgentRuntimeContext {
        append_system_prompt: Some("Extra.".into()),
        ..AgentRuntimeContext::default()
    };
    let composed = runtime.build_system_prompt(AgentMode::Agent).unwrap();
    let message = runtime.build_system_message(AgentMode::Agent).unwrap();
    assert_eq!(message.as_text(), Some(composed.text.as_str()));
    assert_eq!(composed.sections.sections.last().unwrap().id, "append");

    // `--no-system` with only appended text: that text is the whole prompt.
    let bare = AgentRuntimeContext {
        no_system: true,
        append_system_prompt: Some("Just this.".into()),
        ..AgentRuntimeContext::default()
    };
    let composed = bare.build_system_prompt(AgentMode::Agent).unwrap();
    assert_eq!(composed.text, "Just this.");
    assert_eq!(ids(&composed), ["append"]);
}
