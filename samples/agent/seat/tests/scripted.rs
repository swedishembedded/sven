// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A scripted learner against a scripted simulated seat runs in milliseconds:
//! the record says simulated and carries the checks, and probes taken before
//! and after the conversation do not count as conversation.

use sample_agent_seat::{run, Probe, SeatChoice};
use sven_sdk::config::Config;
use sven_sdk::seat::{Ending, Provenance};

fn scripted(dir: &std::path::Path, name: &str, yaml: &str) -> Config {
    let file = dir.join(name);
    std::fs::write(&file, yaml).unwrap();
    let mut config = Config::default();
    config.model.provider = "mock".to_string();
    config.model.name = name.to_string();
    config.model.mock_responses_file = Some(file.display().to_string());
    config
}

#[tokio::test]
async fn a_scripted_learner_and_seat_leave_a_checked_simulated_record() {
    let dir = tempfile::tempdir().unwrap();
    let learner = scripted(
        dir.path(),
        "learner.yaml",
        "responses:\n  - {match_type: default, reply: \"How much was the deposit?\"}\n",
    );
    let seat = scripted(
        dir.path(),
        "seat.yaml",
        "responses:\n  - {match_type: contains, pattern: \"Quiz\", reply: \"I think it is 14000 SEK\"}\n  - {match_type: default, reply: \"It was 14000 SEK. [END]\"}\n",
    );
    let spec = dir.path().join("spec.yaml");
    std::fs::write(
        &spec,
        "id: t\npersona: a tenant\ngoals: [x]\nopening: My landlord kept my deposit.\nhidden_facts:\n  - {id: amount, value: 14000 SEK, disclose_when: [how much]}\nanswer_key: [deposit act section 12]\nseed: 3\n",
    )
    .unwrap();
    let probes = [Probe {
        id: "q".into(),
        text: "Quiz: what was the amount?".into(),
    }];

    let out = run(
        learner,
        None,
        SeatChoice::Simulated {
            spec: &spec,
            config: Box::new(seat),
        },
        &probes,
        4,
    )
    .await
    .expect("runs");

    assert_eq!(out.record.ending, Ending::SeatDone);
    assert!(matches!(
        out.record.provenance,
        Provenance::Simulated { seed: 3, .. }
    ));
    let checks = out.record.checks.expect("measured");
    assert_eq!(
        (checks.replies, checks.clean_replies),
        (1, 1),
        "the probes are not replies"
    );
    assert_eq!(out.probes_before[0].answer, "I think it is 14000 SEK");
    assert_eq!(out.probes_after, out.probes_before);
}
