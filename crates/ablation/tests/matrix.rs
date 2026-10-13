// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The variant matrix: the committed file is valid, an invalid one is refused
//! before anything runs, and every variant is realized by configuration alone.

use sven_ablation::matrix::{Disclosure, Endpoints, Matrix, Role};

const COMMITTED: &str = include_str!("../../../benchmarks/ablation/matrix.yaml");

fn committed() -> Matrix {
    Matrix::from_yaml(COMMITTED).expect("the committed matrix is valid")
}

fn endpoints() -> Endpoints {
    Endpoints {
        local_provider_url: "http://127.0.0.1:9".into(),
        brain_url: None,
        stock_model: "auto".into(),
        trained_model: None,
    }
}

fn with_replaced(from: &str, to: &str) -> String {
    assert!(COMMITTED.contains(from), "fixture text {from:?} not found");
    COMMITTED.replacen(from, to, 1)
}

#[test]
fn the_committed_matrix_defines_the_seven_variants() {
    let matrix = committed();
    let ids: Vec<&str> = matrix.variants.iter().map(|v| v.id.as_str()).collect();
    assert_eq!(ids, ["A", "B", "C", "D", "E", "F", "G"]);
    assert_eq!(matrix.baseline().id, "A");
    let roles: Vec<Role> = matrix.variants.iter().map(|v| v.role).collect();
    assert_eq!(
        roles,
        [
            Role::Baseline,
            Role::Architecture,
            Role::Heuristic,
            Role::StockModel,
            Role::TrainedModel,
            Role::NegativeControl,
            Role::Oracle
        ]
    );
}

#[test]
fn a_matrix_with_two_variants_of_one_id_is_refused() {
    let text = with_replaced("id: B", "id: A");
    let error = Matrix::from_yaml(&text).unwrap_err().to_string();
    assert!(error.contains("duplicate variant id"), "{error}");
}

#[test]
fn a_matrix_without_a_baseline_is_refused() {
    let mut matrix = committed();
    matrix.variants.remove(0);
    let text = serde_yaml::to_string(&matrix).unwrap();
    let error = Matrix::from_yaml(&text).unwrap_err().to_string();
    assert!(error.contains("baseline"), "{error}");
}

#[test]
fn a_role_used_twice_is_refused() {
    let text = with_replaced("role: architecture", "role: baseline");
    let error = Matrix::from_yaml(&text).unwrap_err().to_string();
    assert!(error.contains("more than one"), "{error}");
}

#[test]
fn a_role_that_contradicts_its_decisions_is_refused() {
    // The heuristic variant must use the heuristic provider.
    let text = with_replaced("kind: heuristic", "kind: oracle");
    let error = Matrix::from_yaml(&text).unwrap_err().to_string();
    assert!(error.contains("heuristic"), "{error}");
}

#[test]
fn an_unknown_field_is_refused_rather_than_ignored() {
    let text = with_replaced(
        "tool_disclosure: full",
        "tool_disclosure: full\n    tool_dislosure: x",
    );
    assert!(Matrix::from_yaml(&text).is_err());
}

#[test]
fn another_schema_version_is_refused() {
    let text = with_replaced("sven.ablation.matrix/1", "sven.ablation.matrix/2");
    let error = Matrix::from_yaml(&text).unwrap_err().to_string();
    assert!(error.contains("schema"), "{error}");
}

#[test]
fn the_baseline_is_the_legacy_configuration() {
    let matrix = committed();
    let plan = matrix.baseline().realize(&endpoints()).unwrap();
    assert_eq!(plan.tool_disclosure, Disclosure::Full);
    let config: serde_yaml::Value = serde_yaml::from_str(&plan.config_yaml).unwrap();
    assert_eq!(config["decision"]["enabled"].as_bool(), Some(false));
    assert_eq!(config["context"]["assembly"]["mode"].as_str(), Some("off"));
}

#[test]
fn the_architecture_variant_defers_tools_and_asks_no_model() {
    let matrix = committed();
    let b = matrix.variants.iter().find(|v| v.id == "B").unwrap();
    let plan = b.realize(&endpoints()).unwrap();
    assert_eq!(plan.tool_disclosure, Disclosure::Deferred);
    let config: serde_yaml::Value = serde_yaml::from_str(&plan.config_yaml).unwrap();
    assert_eq!(config["decision"]["enabled"].as_bool(), Some(false));
    assert_ne!(config["context"]["assembly"]["mode"].as_str(), Some("off"));
}

#[test]
fn injected_providers_are_reached_through_the_decision_section() {
    let matrix = committed();
    for (id, model) in [
        ("C", "ablation/heuristic"),
        ("F", "ablation/random"),
        ("G", "ablation/oracle"),
    ] {
        let v = matrix.variants.iter().find(|v| v.id == id).unwrap();
        let plan = v.realize(&endpoints()).unwrap();
        let config: serde_yaml::Value = serde_yaml::from_str(&plan.config_yaml).unwrap();
        assert_eq!(config["decision"]["enabled"].as_bool(), Some(true), "{id}");
        assert_eq!(config["decision"]["model"].as_str(), Some(model), "{id}");
        assert_eq!(
            config["decision"]["brain"]["url"].as_str(),
            Some("http://127.0.0.1:9"),
            "{id}"
        );
    }
}

#[test]
fn a_brain_variant_without_its_endpoint_is_unavailable_and_says_why() {
    let matrix = committed();
    let d = matrix.variants.iter().find(|v| v.id == "D").unwrap();
    let reason = d.realize(&endpoints()).unwrap_err().to_string();
    assert!(reason.contains("--brain-url"), "{reason}");

    let e = matrix.variants.iter().find(|v| v.id == "E").unwrap();
    let mut with_url = endpoints();
    with_url.brain_url = Some("http://127.0.0.1:8788".into());
    let reason = e.realize(&with_url).unwrap_err().to_string();
    assert!(reason.contains("--trained-model"), "{reason}");
}

#[test]
fn brain_variants_name_their_model_and_the_given_endpoint() {
    let matrix = committed();
    let mut ep = endpoints();
    ep.brain_url = Some("http://127.0.0.1:8788".into());
    ep.trained_model = Some("org/trained-1".into());
    for (id, model) in [("D", "auto"), ("E", "org/trained-1")] {
        let v = matrix.variants.iter().find(|v| v.id == id).unwrap();
        let config: serde_yaml::Value =
            serde_yaml::from_str(&v.realize(&ep).unwrap().config_yaml).unwrap();
        assert_eq!(config["decision"]["model"].as_str(), Some(model), "{id}");
        assert_eq!(
            config["decision"]["brain"]["url"].as_str(),
            Some("http://127.0.0.1:8788"),
            "{id}"
        );
    }
}
