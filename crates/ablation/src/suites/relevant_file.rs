// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Suite: the relevant file among hundreds, with misleading matches.
//!
//! A repository of 200 to 399 source files. One defines the function a task
//! is about and holds the two lines that explain its defect; several others
//! only mention that function in comments, notes and tests. The scripted model
//! searches for the function, reads the misleading files, reads the relevant
//! file, reads unrelated files, and is finally asked for the root cause.
//!
//! Measured: whether the two defect lines are still in what the model reads
//! at the final question (critical-evidence recall), how much of what is shown
//! of the labelled files is the relevant one (precision), and the tokens by
//! source over the whole task. Not measured: whether a model would find the
//! defect.

use std::collections::BTreeMap;

use serde_json::json;

use super::corpus::{big_files, file_stem, function_name, source_lines, TurnBuilder, MODULES};
use crate::rng::Rng;
use crate::task::{Evidence, EvidenceRole, Generated, Suite, Task};

pub const MEASURES: &str = "Harness side only, model behaviour scripted: whether the two lines that \
explain the defect are still in the request at the final question (critical-evidence recall), \
the share of visible labelled evidence that is critical (precision), expansions of demoted \
context, and the tokens by source of the whole task. Not measured: whether a model finds the defect.";

pub fn generate(seed: u64) -> Generated {
    let mut rng = Rng::new(seed, "relevant_file");
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    let total = rng.between(200, 399);
    for _ in 0..total {
        let module = *rng.pick(&MODULES);
        let path = format!("src/{module}/{}_{}.rs", file_stem(&mut rng), rng.hex(3));
        let lines = rng.between(20, 45);
        let text = source_lines(&mut rng, module, lines);
        files.insert(path, text.into_bytes());
    }

    let symbol = function_name(&mut rng);
    let mut task = Task::new(Suite::RelevantFile, seed, MEASURES);

    // The relevant file: the definition and the two lines that explain the defect.
    let relevant = format!("src/billing/{}_{}.rs", file_stem(&mut rng), rng.hex(3));
    let cause = format!("EVID-REL-{}", rng.hex(6));
    let effect = format!("EVID-REL-{}", rng.hex(6));
    let mut text = source_lines(&mut rng, "billing", 40);
    text.push_str(&format!(
        "pub fn {symbol}(entries: &[u64], window_end: u64) -> u64 {{\n\
         \x20   let cutoff = window_end - 1; // {cause} off-by-one: the final entry is dropped\n\
         \x20   entries.iter().filter(|e| **e < cutoff).sum() // {effect} callers see a short total\n\
         }}\n"
    ));
    files.insert(relevant.clone(), text.into_bytes());
    for (marker, source) in [(&cause, &relevant), (&effect, &relevant)] {
        task.evidence.push(Evidence {
            marker: marker.clone(),
            role: EvidenceRole::Critical,
            kind: "file_line".into(),
            source: source.clone(),
        });
    }

    // Large files: the misleading ones, and unrelated ones read as filler.
    let misleading_count = rng.between(8, 14);
    let mut misleading: Vec<String> = Vec::new();
    for n in 0..misleading_count {
        let module = *rng.pick(&MODULES);
        let path = format!("src/{module}/notes_{n:02}_{}.rs", rng.hex(3));
        let marker = format!("DIST-{}", rng.hex(6));
        let lines = rng.between(120, 170);
        let mut text = source_lines(&mut rng, module, lines);
        text.push_str(&format!(
            "// {marker} see {symbol}: deprecated, the replacement lives elsewhere\n\
             // TODO({symbol}) revisit after the next release\n"
        ));
        files.insert(path.clone(), text.into_bytes());
        task.evidence.push(Evidence {
            marker,
            role: EvidenceRole::Distractor,
            kind: "file_line".into(),
            source: path.clone(),
        });
        misleading.push(path);
    }
    let filler_rounds = rng.between(2, 4);
    let filler = big_files(&mut rng, &mut files, "bulk", filler_rounds * 2);

    let mut turns = TurnBuilder::new(&task.id, "rf");
    turns.push(
        &format!("Find where `{symbol}` is used."),
        vec![("grep", json!({"pattern": symbol, "path": "src"}))],
        "Searched for the function.",
    );
    turns.push(
        "Read the files that mention it.",
        misleading
            .iter()
            .map(|p| ("read_file", json!({"path": p})))
            .collect(),
        "Read the files that mention it.",
    );
    turns.push(
        "Read the file that defines it.",
        vec![("read_file", json!({"path": relevant}))],
        "Read the definition.",
    );
    for pair in filler.chunks(2) {
        turns.push(
            "Check the neighbouring modules too.",
            pair.iter()
                .map(|p| ("read_file", json!({"path": p})))
                .collect(),
            "Checked the neighbouring modules.",
        );
    }
    turns.push(
        &format!("What is the root cause of the wrong total in `{symbol}`? Cite the lines."),
        Vec::new(),
        "Answered from the evidence in context.",
    );
    turns.recover_on(&[&symbol]);
    task.segments.push(turns.finish());
    Generated { task, files }
}
