// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Suite: an interrupted long task with cold restarts.
//!
//! One task runs in three sven processes. The first states the ground rules
//! and reads a first report; each restart loads the saved trajectory of the
//! one before and carries on with another report and unrelated reading. The
//! last asks for the rules and the findings verified so far. Half of the
//! findings are phrased with words a failure extractor keeps (`fails`,
//! `error`) and half without.
//!
//! Measured: whether every constraint and every verified finding is still in
//! the request at the final question, after two restarts (survival), and the
//! tokens summed over all three processes (cumulative, never the last
//! turn's). Not measured: whether a model would obey the rules.

use std::collections::BTreeMap;

use serde_json::json;

use super::corpus::{big_files, TurnBuilder};
use crate::rng::Rng;
use crate::task::{Evidence, EvidenceRole, Generated, Suite, Task};

pub const MEASURES: &str = "Harness side only, model behaviour scripted: whether every constraint \
and every verified finding is still in the request at the final question after two cold restarts \
(survival), precision against unverified rumours, and the tokens summed over all processes of the \
task. Not measured: whether a model obeys the constraints or reasons from the findings.";

const SEGMENTS: usize = 3;

pub fn generate(seed: u64) -> Generated {
    let mut rng = Rng::new(seed, "restart_survival");
    let mut task = Task::new(Suite::RestartSurvival, seed, MEASURES);
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    // One report per process. Each holds one verified finding among rumours.
    let subjects = ["alpha", "beta", "gamma"];
    for (n, subject) in subjects.iter().enumerate() {
        let finding = format!("VERIFIED-{}", rng.hex(6));
        let phrasing = if rng.below(2) == 0 {
            format!("{finding}: module {subject} fails when the offset is negative")
        } else {
            format!("{finding}: module {subject} rejects offsets below zero")
        };
        let mut report = String::new();
        for line in 0..rng.between(120, 200) {
            report.push_str(&format!("check {line:03} of module {subject}: ok\n"));
        }
        for _ in 0..rng.between(2, 5) {
            let rumour = format!("DIST-{}", rng.hex(6));
            report.push_str(&format!(
                "UNVERIFIED {rumour}: someone says module {subject} may be flaky\n"
            ));
            task.evidence.push(Evidence {
                marker: rumour,
                role: EvidenceRole::Distractor,
                kind: "finding".into(),
                source: format!("reports/check-{}.txt", n + 1),
            });
        }
        report.push_str(&phrasing);
        report.push('\n');
        files.insert(format!("reports/check-{}.txt", n + 1), report.into_bytes());
        task.evidence.push(Evidence {
            marker: finding,
            role: EvidenceRole::Critical,
            kind: "finding".into(),
            source: format!("reports/check-{}.txt", n + 1),
        });
    }

    let rules = [
        format!("CONSTRAINT-{}", rng.hex(6)),
        format!("CONSTRAINT-{}", rng.hex(6)),
        format!("CONSTRAINT-{}", rng.hex(6)),
    ];
    for rule in &rules {
        task.evidence.push(Evidence {
            marker: rule.clone(),
            role: EvidenceRole::Critical,
            kind: "constraint".into(),
            source: "user".into(),
        });
    }

    let filler = big_files(&mut rng, &mut files, "bulk", 2 * SEGMENTS);
    for (n, subject) in subjects.iter().enumerate() {
        let prefix = format!("rs{n}");
        let mut turns = TurnBuilder::new(&task.id, &prefix);
        let report = format!("reports/check-{}.txt", n + 1);
        let intro = match n {
            0 => format!(
                "Ground rules for the whole task. {} never edit anything under vendor/. {} keep \
                 two-space indentation. Start with the {subject} report.",
                rules[0], rules[1]
            ),
            1 => format!(
                "We were interrupted and restarted. New rule: {} every change needs a test. \
                 Continue with the {subject} report.",
                rules[2]
            ),
            _ => format!("Restarted again. Continue with the {subject} report."),
        };
        turns.push(
            &intro,
            vec![("shell", json!({"shell_command": format!("cat {report}")}))],
            "Read the report.",
        );
        turns.push(
            "Look at the surrounding modules.",
            filler[n * 2..n * 2 + 2]
                .iter()
                .map(|p| ("read_file", json!({"path": p})))
                .collect(),
            "Looked at the surrounding modules.",
        );
        if n == SEGMENTS - 1 {
            turns.push(
                "List the ground rules and every finding that was verified, from the start.",
                Vec::new(),
                "Answered from the evidence in context.",
            );
        }
        task.segments.push(turns.finish());
    }
    Generated { task, files }
}
