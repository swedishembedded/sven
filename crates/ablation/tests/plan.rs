// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! What a variant does to the sven command line and configuration.

use std::path::Path;

use sven_ablation::matrix::{Endpoints, Matrix};
use sven_ablation::plan::{compose_config, invocation, merge, ModelChoice, RunLayout};
use sven_ablation::suites::generate;
use sven_ablation::task::Suite;

const MATRIX: &str = include_str!("../../../benchmarks/ablation/matrix.yaml");

fn endpoints() -> Endpoints {
    Endpoints {
        local_provider_url: "http://127.0.0.1:9".into(),
        brain_url: None,
        stock_model: "auto".into(),
        trained_model: None,
    }
}

fn layout() -> RunLayout {
    RunLayout {
        dir: "/runs/x".into(),
    }
}

#[test]
fn a_scripted_segment_names_the_mock_model_and_its_script() {
    let task = generate(Suite::NoisyOutput, 1).task;
    let matrix = Matrix::from_yaml(MATRIX).unwrap();
    let realization = matrix.baseline().realize(&endpoints()).unwrap();
    let inv = invocation(&task, &realization, &layout(), &ModelChoice::Scripted, 0);
    let args = inv.args.join(" ");
    assert!(args.contains("--model mock"));
    assert!(args.contains("--tool-disclosure full"));
    assert!(args.contains("--token-ledger"));
    assert!(args.contains("--approval auto"));
    assert!(
        !args.contains("--load-trace"),
        "the first segment starts cold"
    );
    assert!(inv
        .env
        .iter()
        .any(|(k, v)| k == "SVEN_MOCK_RESPONSES" && v.ends_with("segment-0/mock.yaml")));
    assert!(
        inv.env
            .iter()
            .any(|(k, v)| k == "HOME" && v == "/runs/x/home"),
        "an empty home: nothing of the user's enters a request"
    );
    assert_eq!(inv.cwd, Path::new("/runs/x/repo"));
}

#[test]
fn a_restart_loads_the_trajectory_of_the_segment_before() {
    let task = generate(Suite::RestartSurvival, 1).task;
    let matrix = Matrix::from_yaml(MATRIX).unwrap();
    let b = matrix.variants.iter().find(|v| v.id == "B").unwrap();
    let realization = b.realize(&endpoints()).unwrap();
    let inv = invocation(&task, &realization, &layout(), &ModelChoice::Scripted, 2);
    let args = inv.args.join(" ");
    assert!(args.contains("--tool-disclosure deferred"));
    assert!(args.contains("--load-trace /runs/x/segment-1/trajectory.json"));
    assert!(args.contains("--output-trace /runs/x/segment-2/trajectory.json"));
}

#[test]
fn a_real_model_is_named_explicitly_and_gets_no_script() {
    let task = generate(Suite::NoisyOutput, 1).task;
    let matrix = Matrix::from_yaml(MATRIX).unwrap();
    let realization = matrix.baseline().realize(&endpoints()).unwrap();
    let inv = invocation(
        &task,
        &realization,
        &layout(),
        &ModelChoice::Real("acme/model-1".into()),
        0,
    );
    assert!(inv.args.join(" ").contains("--model acme/model-1"));
    assert!(inv.env.iter().all(|(k, _)| k != "SVEN_MOCK_RESPONSES"));
}

#[test]
fn the_security_task_carries_its_deny_patterns_and_disabled_tools() {
    let task = generate(Suite::Security, 1).task;
    let matrix = Matrix::from_yaml(MATRIX).unwrap();
    let realization = matrix.baseline().realize(&endpoints()).unwrap();
    let inv = invocation(&task, &realization, &layout(), &ModelChoice::Scripted, 0);
    assert!(inv.args.join(" ").contains("--mode agent"));
    let config = compose_config(
        &task,
        &realization,
        Path::new("/bin/bench"),
        Path::new("/runs/x/catalog.json"),
    )
    .unwrap();
    let value: serde_yaml::Value = serde_yaml::from_str(&config).unwrap();
    assert!(value["tools"]["deny_patterns"].as_sequence().unwrap().len() >= 3);
    assert_eq!(value["tools"]["disabled"].as_sequence().unwrap().len(), 2);
    assert_eq!(
        value["decision"]["enabled"].as_bool(),
        Some(false),
        "the variant's keys are merged in"
    );
}

#[test]
fn a_task_with_a_catalog_gets_the_stub_server_in_its_configuration() {
    let task = generate(Suite::ToolSelection, 1).task;
    let matrix = Matrix::from_yaml(MATRIX).unwrap();
    let realization = matrix.baseline().realize(&endpoints()).unwrap();
    let config = compose_config(
        &task,
        &realization,
        Path::new("/bin/bench"),
        Path::new("/runs/x/catalog.json"),
    )
    .unwrap();
    let value: serde_yaml::Value = serde_yaml::from_str(&config).unwrap();
    let server = &value["mcp_servers"]["catalog"];
    assert_eq!(server["transport"]["type"].as_str(), Some("stdio"));
    assert_eq!(server["transport"]["command"].as_str(), Some("/bin/bench"));
    let args: Vec<&str> = server["transport"]["args"]
        .as_sequence()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(args, ["mcp-stub", "--catalog", "/runs/x/catalog.json"]);

    let plain = generate(Suite::NoisyOutput, 1).task;
    let config = compose_config(
        &plain,
        &realization,
        Path::new("/bin/bench"),
        Path::new("/c"),
    )
    .unwrap();
    assert!(!config.contains("mcp_servers"));
}

#[test]
fn merging_replaces_scalars_and_combines_maps() {
    let mut a: serde_yaml::Value = serde_yaml::from_str("x: {a: 1, b: 2}\ny: 1").unwrap();
    merge(
        &mut a,
        serde_yaml::from_str("x: {b: 3, c: 4}\nz: 5").unwrap(),
    );
    let expected: serde_yaml::Value =
        serde_yaml::from_str("x: {a: 1, b: 3, c: 4}\ny: 1\nz: 5").unwrap();
    assert_eq!(a, expected);
}
