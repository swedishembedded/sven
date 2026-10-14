// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! Spec: under deferred skill disclosure the model finds a skill the index
//! does not make obvious with `skill` `action: "search"`, ranked like tool
//! search, and every skill stays loadable by its exact command.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use sven_tool_api::tool::{Tool, ToolCall};
use sven_tool_registry::{RankedTool, ToolDescriptor, ToolRanker};
use sven_tools_agent::SkillTool;
use sven_workspace::{SharedSkills, SkillInfo, SvenSkillMeta};

fn skill(command: &str, description: &str) -> SkillInfo {
    SkillInfo {
        command: command.to_string(),
        name: command.rsplit('/').next().unwrap().to_string(),
        description: description.to_string(),
        version: None,
        skill_md_path: PathBuf::from(format!("skills/{command}/SKILL.md")),
        skill_dir: PathBuf::from(format!("skills/{command}")),
        content: "body".to_string(),
        sven_meta: None,
    }
}

fn call(args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "c".into(),
        name: "skill".into(),
        args,
    }
}

fn catalog() -> Vec<SkillInfo> {
    vec![
        skill(
            "billing",
            "Work on invoices, payments and the billing service.",
        ),
        skill(
            "auth",
            "Work on login, sessions and the authentication service.",
        ),
        skill(
            "auth/review",
            "Review a change to the authentication service.",
        ),
    ]
}

fn tool(skills: Vec<SkillInfo>) -> SkillTool {
    SkillTool::new(SharedSkills::new(skills))
}

#[tokio::test]
async fn search_ranks_skills_by_their_description() {
    let out = tool(catalog())
        .execute(&call(
            json!({"action": "search", "query": "invoices and payments"}),
        ))
        .await;
    assert!(!out.is_error, "{}", out.content);
    let first = out
        .content
        .lines()
        .find(|l| l.contains("command="))
        .unwrap();
    assert!(first.contains("command=\"billing\""), "{}", out.content);
}

#[tokio::test]
async fn search_never_returns_a_skill_the_user_alone_may_invoke() {
    let mut hidden = skill("secret", "Deploy invoices to production.");
    hidden.sven_meta = Some(SvenSkillMeta {
        user_invocable_only: true,
        ..SvenSkillMeta::default()
    });
    let mut skills = catalog();
    skills.push(hidden);
    let out = tool(skills)
        .execute(&call(
            json!({"action": "search", "query": "deploy invoices"}),
        ))
        .await;
    assert!(!out.content.contains("secret"), "{}", out.content);
}

#[tokio::test]
async fn search_without_a_query_is_an_error_that_names_the_parameter() {
    let out = tool(catalog())
        .execute(&call(json!({"action": "search"})))
        .await;
    assert!(
        out.is_error && out.content.contains("query"),
        "{}",
        out.content
    );
}

#[tokio::test]
async fn a_search_that_finds_nothing_points_to_list() {
    let out = tool(catalog())
        .execute(&call(json!({"action": "search", "query": "zzzz"})))
        .await;
    assert!(!out.is_error);
    assert!(out.content.contains("list"), "{}", out.content);
}

#[tokio::test]
async fn search_returns_at_most_eight_skills() {
    let many: Vec<SkillInfo> = (0..20)
        .map(|i| skill(&format!("pkg/s{i:02}"), "Change a document."))
        .collect();
    let out = tool(many)
        .execute(&call(
            json!({"action": "search", "query": "change document"}),
        ))
        .await;
    assert_eq!(
        out.content.matches("command=").count(),
        8,
        "{}",
        out.content
    );
}

/// Puts the candidates in reverse name order, whatever the query.
struct Reversed;

#[async_trait]
impl ToolRanker for Reversed {
    async fn rank(&self, _query: &str, candidates: &[ToolDescriptor]) -> Vec<RankedTool> {
        let mut names: Vec<&str> = candidates.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable_by(|a, b| b.cmp(a));
        names
            .into_iter()
            .enumerate()
            .map(|(i, n)| RankedTool {
                name: n.to_string(),
                score: 100.0 - i as f64,
            })
            .collect()
    }
}

#[tokio::test]
async fn an_injected_ranker_decides_the_order() {
    let out = tool(catalog())
        .with_ranker(Arc::new(Reversed))
        .execute(&call(json!({"action": "search", "query": "anything"})))
        .await;
    let commands: Vec<&str> = out
        .content
        .lines()
        .filter_map(|l| l.split("command=\"").nth(1))
        .filter_map(|l| l.split('"').next())
        .collect();
    assert_eq!(commands, ["billing", "auth/review", "auth"]);
}

#[tokio::test]
async fn every_skill_still_loads_by_its_exact_command() {
    let out = tool(catalog())
        .execute(&call(json!({"action": "load", "name": "billing"})))
        .await;
    // The skill file is not on disk in this fixture; the lookup, not the read,
    // is what search must not disturb.
    assert!(!out.content.contains("not found"), "{}", out.content);
}
