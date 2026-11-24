// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent trajectory capture for its clients. If
// your team needs expertise in turning agent runs into training data then you
// can procure our services by sending an email to info@swedishembedded.com.

//! Spec: a step that covers several model requests keeps, for each request, the
//! identity its own response reported, so an online trainer can tell which policy
//! version produced which tokens; the trajectory's final metrics carry the
//! policy-version span over every request of the run.

use atif::StepMetrics;
use serde_json::{json, Value};
use sven_session_store::step_request::{record_response, totals};
use sven_vocab::{
    AdapterRef, LogprobKind, ReportedUsage, RequestRecord, Sampled, SamplingControl,
    SamplingControls, ServedIdentity,
};

/// A request answered by policy `version`, as a brain server reports it.
fn served_by(version: &str, token: i64) -> RequestRecord {
    let mut record = RequestRecord::default();
    record.add_usage(
        ReportedUsage {
            input: 10,
            output: 1,
            ..ReportedUsage::default()
        },
        None,
    );
    record.sampled = Sampled {
        model: Some("tiny".into()),
        seed: Some(5),
        prompt_token_ids: vec![1, 2],
        token_ids: vec![token],
        logprobs: vec![-0.5],
        identity: ServedIdentity {
            adapter: Some(AdapterRef {
                id: "lab".into(),
                digest: format!("sha256:v{version}"),
            }),
            base_digest: Some("sha256:base".into()),
            policy_version: Some(version.into()),
            logprob_kind: Some(LogprobKind::Policy),
            logprob_temperature: Some(0.8),
            ..ServedIdentity::default()
        },
        sampling_honoured: SamplingControls::of(&[SamplingControl::Seed]),
        truncated: version == "4",
        ..Sampled::default()
    };
    record
}

fn step_of(requests: &[RequestRecord]) -> StepMetrics {
    let (mut metrics, mut model) = (None, None);
    for request in requests {
        record_response(&mut metrics, &mut model, request);
    }
    metrics.expect("a recorded step")
}

#[test]
fn each_request_of_a_step_records_the_identity_its_own_response_reported() {
    let step = step_of(&[served_by("3", 7), served_by("4", 8)]);
    let sven = &step.extra.as_ref().expect("extra")["sven"];
    let requests = sven["requests"]
        .as_array()
        .expect("requests of a two-request step");
    assert_eq!(requests.len(), 2);
    for (entry, (version, token)) in requests.iter().zip([("3", 7), ("4", 8)]) {
        assert_eq!(entry["policy_version"], json!(version));
        assert_eq!(
            entry["adapter"],
            json!({"id": "lab", "digest": format!("sha256:v{version}")})
        );
        assert_eq!(entry["base_digest"], "sha256:base");
        assert_eq!(entry["logprob_kind"], "policy");
        assert_eq!(entry["logprob_temperature"], 0.8);
        assert_eq!(entry["sampling_honoured"], json!(["seed"]));
        assert_eq!(entry["seed"], 5);
        assert_eq!(entry["completion_token_ids"], json!([token]));
    }
    assert_eq!(
        requests[0].get("truncated"),
        None,
        "a finished response says nothing"
    );
    assert_eq!(
        requests[1]["truncated"], true,
        "a cut-off response says so on its own entry"
    );
    // The step's own fields keep their meaning: the first request that reported a fact fixes it.
    assert_eq!(sven["policy_version"], "3");
    assert_eq!(step.completion_token_ids, Some(vec![7, 8]));
}

#[test]
fn the_final_metrics_carry_the_policy_version_span_over_every_request() {
    let requests = [served_by("4", 1), served_by("3", 2), served_by("4", 3)];
    let finals = totals(requests.iter()).expect("totals");
    assert_eq!(
        finals.extra.as_ref().expect("extra")["sven"]["policy_version_span"],
        json!([3, 4])
    );
    let single = totals([served_by("7", 1)].iter()).expect("totals");
    assert_eq!(
        single.extra.as_ref().unwrap()["sven"]["policy_version_span"],
        json!([7, 7])
    );
}

#[test]
fn a_run_with_a_version_that_cannot_be_ordered_records_no_span() {
    let finals = totals([served_by("3", 1), served_by("rc-2", 2)].iter()).expect("totals");
    assert_eq!(
        finals.extra, None,
        "a span is never guessed from versions that are not numbers"
    );
    let none = totals([RequestRecord::default()].iter()).expect("totals");
    assert_eq!(none.extra.as_ref().map(Value::is_null), None);
}

#[test]
fn the_samplers_top_list_is_on_the_step_and_on_each_request_beside_its_logprobs() {
    let with_top = |version: &str, token: i64, other: i64| {
        let mut record = served_by(version, token);
        record.sampled.top_token_ids = vec![vec![token, other]];
        record.sampled.top_logprobs = vec![vec![-0.5, -1.5]];
        record
    };
    let single = step_of(&[with_top("3", 7, 9)]);
    let sven = &single.extra.as_ref().expect("extra")["sven"];
    assert_eq!(sven["generation_top_ids"], json!([[7, 9]]));
    assert_eq!(sven["generation_top_log_probs"], json!([[-0.5, -1.5]]));

    let step = step_of(&[with_top("3", 7, 9), with_top("4", 8, 6)]);
    let sven = &step.extra.as_ref().expect("extra")["sven"];
    // The step's lists run parallel to its concatenated logprobs; requests[] is per request.
    assert_eq!(sven["generation_top_ids"], json!([[7, 9], [8, 6]]));
    let requests = sven["requests"].as_array().expect("requests");
    assert_eq!(requests[0]["generation_top_ids"], json!([[7, 9]]));
    assert_eq!(requests[1]["generation_top_ids"], json!([[8, 6]]));
    assert_eq!(
        requests[1]["generation_top_log_probs"],
        json!([[-0.5, -1.5]])
    );
    assert_eq!(requests[1]["logprobs"], json!([-0.5]));
    // A request that was not asked records none, instead of an empty list.
    let plain = step_of(&[served_by("3", 7)]);
    assert!(plain.extra.as_ref().unwrap()["sven"]
        .get("generation_top_ids")
        .is_none());
}
