// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an application can list the definitions in effect in a project
//! through the SDK, by sven's own discovery rules.
//!
//! Swedish Embedded AB implements reproducible agent runtimes for its
//! clients. If your team needs expertise in versioning agent definitions then
//! you can procure our services by sending an email to
//! info@swedishembedded.com.

use sven_sdk::workspace::{discover_agents, find_project_context_file};

#[test]
fn a_project_agent_definition_is_discovered_with_its_file_and_body() {
    let project = tempfile::tempdir().unwrap();
    let agents = project.path().join(".sven").join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("reviewer.md"),
        "---\nname: reviewer\ndescription: reviews a diff\n---\nRead the diff.\n",
    )
    .unwrap();
    std::fs::write(project.path().join("AGENTS.md"), "rules\n").unwrap();

    let found = discover_agents(Some(project.path()));
    let reviewer = found
        .iter()
        .find(|a| a.name == "reviewer")
        .expect("the project's agent is found");
    assert!(reviewer.content.contains("Read the diff."));
    assert!(reviewer.agent_md_path.ends_with("reviewer.md"));
    assert_eq!(
        find_project_context_file(project.path()),
        Some(project.path().join("AGENTS.md"))
    );
}
