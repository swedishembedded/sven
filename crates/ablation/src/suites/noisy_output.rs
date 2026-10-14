// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Suite: large noisy command output with a few causally important failures.
//!
//! A build log of 1,500 to 2,499 lines holds a handful of failures, each a
//! cause and its effect, among many lines that use the same words (a warning
//! about a retried error, a summary of zero failures). The scripted model
//! greps the log for every such word, reads unrelated files, and is finally
//! asked which tests failed and why.
//!
//! Measured: whether every cause and effect line is still in the request at
//! the final question (critical-evidence recall), precision against the
//! look-alike lines, expansions of demoted context and tokens by source. The
//! more look-alike lines, the closer a fixed cap on kept matches comes to
//! cutting a real failure. Not measured: whether a model diagnoses the build.

use std::collections::BTreeMap;

use serde_json::json;

use super::corpus::{big_files, function_name, log_line, TurnBuilder, MODULES};
use crate::rng::Rng;
use crate::task::{Evidence, EvidenceRole, Generated, Suite, Task};

pub const MEASURES: &str =
    "Harness side only, model behaviour scripted: whether the cause and the \
effect line of every real failure are still in the request at the final question \
(critical-evidence recall), precision against look-alike lines, expansions of demoted context \
and the tokens by source of the whole task. Not measured: whether a model diagnoses the build.";

pub fn generate(seed: u64) -> Generated {
    let mut rng = Rng::new(seed, "noisy_output");
    let mut task = Task::new(Suite::NoisyOutput, seed, MEASURES);
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    let length = rng.between(1500, 2499);
    let failures = rng.between(3, 5);
    let lookalikes = rng.between(10, 70);

    // Line numbers, distinct, where an event goes. A failure takes two lines.
    let mut slots: Vec<usize> = (0..length / 2).map(|n| n * 2).collect();
    rng.shuffle(&mut slots);
    let mut events: BTreeMap<usize, (String, Option<(EvidenceRole, String)>)> = BTreeMap::new();

    for _ in 0..failures {
        let at = slots.pop().expect("the log is longer than its events");
        let test = format!("tests::{}_keeps_invariant", function_name(&mut rng));
        let cause = format!("EVID-FAIL-{}", rng.hex(6));
        let effect = format!("EVID-FAIL-{}", rng.hex(6));
        events.insert(
            at,
            (
                format!(
                    "ERROR {}::{} invariant violated: bucket {} over capacity ({cause})",
                    rng.pick(&MODULES),
                    function_name(&mut rng),
                    rng.below(64)
                ),
                Some((EvidenceRole::Critical, cause)),
            ),
        );
        events.insert(
            at + 1,
            (
                format!(
                    "FAILED {test} - panicked at src/lib.rs:{} ({effect})",
                    rng.between(10, 900)
                ),
                Some((EvidenceRole::Critical, effect)),
            ),
        );
    }
    for _ in 0..lookalikes {
        let at = slots.pop().expect("the log is longer than its events");
        let marker = format!("DIST-{}", rng.hex(6));
        let line = match rng.below(4) {
            0 => format!("WARN retry succeeded after transient error on attempt 2 ({marker})"),
            1 => format!(
                "INFO summary: 0 failed, {} passed ({marker})",
                rng.between(100, 900)
            ),
            2 => format!(
                "DEBUG ignoring expected failure in flaky_net_test_{} ({marker})",
                rng.below(9)
            ),
            _ => format!("WARN deprecated call, error code table will change ({marker})"),
        };
        events.insert(at, (line, Some((EvidenceRole::Distractor, marker))));
    }

    let mut log = String::new();
    for n in 0..length {
        match events.remove(&n) {
            Some((line, evidence)) => {
                log.push_str(&line);
                if let Some((role, marker)) = evidence {
                    task.evidence.push(Evidence {
                        marker,
                        role,
                        kind: "log_line".into(),
                        source: "ci/build.log".into(),
                    });
                }
            }
            None => log.push_str(&log_line(&mut rng, n)),
        }
        log.push('\n');
    }
    files.insert("ci/build.log".into(), log.into_bytes());

    let filler_rounds = rng.between(2, 4);
    let filler = big_files(&mut rng, &mut files, "bulk", filler_rounds * 2);

    let mut turns = TurnBuilder::new(&task.id, "no");
    turns.push(
        "Look through the build log for anything that went wrong.",
        vec![(
            "shell",
            json!({"shell_command": "grep -n -i -E 'error|fail|panic|warn' ci/build.log"}),
        )],
        "Searched the build log.",
    );
    for pair in filler.chunks(2) {
        turns.push(
            "Check the surrounding modules too.",
            pair.iter()
                .map(|p| ("read_file", json!({"path": p})))
                .collect(),
            "Checked the surrounding modules.",
        );
    }
    turns.push(
        "Which tests failed, and what caused each failure? Cite the log lines.",
        Vec::new(),
        "Answered from the evidence in context.",
    );
    task.segments.push(turns.finish());
    Generated { task, files }
}
