// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements unattended agent runs that leave a complete
// record even when they are killed, for its clients. If your team needs
// expertise in agent pipelines you can procure our services by sending an email
// to info@swedishembedded.com.

//! Spec: the conversations of a harness run are one JSON-lines file, read back
//! with the last line of each index winning; the directory layout older
//! versions wrote is read too.

use sven_sdk::read_conversations;

fn trajectory(model: &str) -> serde_json::Value {
    let mut agent = sven_sdk::atif::AgentProfile::new("sven", "test");
    agent.model_name = Some(model.to_string());
    serde_json::to_value(sven_sdk::atif::Trajectory::new("ATIF-v1.7", agent)).unwrap()
}

fn line(index: usize, name: &str, model: &str) -> String {
    serde_json::json!({"index": index, "name": name, "trajectory": trajectory(model)}).to_string()
}

#[test]
fn the_last_line_of_an_index_wins_and_conversations_come_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("conversations.jsonl");
    // Conversation 1 finished first; 0 was appended again after it grew.
    let text = [
        line(0, "a#0", "early"),
        line(1, "a#1", "only"),
        line(0, "a#0", "late"),
    ]
    .join("\n")
        + "\n";
    std::fs::write(&file, text).unwrap();
    let read = read_conversations(&file).unwrap();
    assert_eq!(
        read.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["a#0", "a#1"]
    );
    assert_eq!(read[0].trajectory.agent.model_name.as_deref(), Some("late"));
}

#[test]
fn a_line_a_kill_cut_short_is_ignored_but_damage_before_the_end_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("c.jsonl");
    let whole = line(0, "a#0", "m");
    std::fs::write(&file, format!("{whole}\n{{\"index\": 1, \"na")).unwrap();
    assert_eq!(read_conversations(&file).unwrap().len(), 1);

    std::fs::write(&file, format!("not json\n{whole}\n")).unwrap();
    assert!(read_conversations(&file).is_err());
}

#[test]
fn the_directory_older_versions_wrote_is_read_too() {
    let dir = tempfile::tempdir().unwrap();
    for (file, model) in [("0001-b-1.json", "second"), ("0000-a-0.json", "first")] {
        std::fs::write(dir.path().join(file), trajectory(model).to_string()).unwrap();
    }
    std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
    let read = read_conversations(dir.path()).unwrap();
    assert_eq!(
        read.iter()
            .map(|c| (c.index, c.name.as_str()))
            .collect::<Vec<_>>(),
        [(0, "a-0"), (1, "b-1")]
    );
}
