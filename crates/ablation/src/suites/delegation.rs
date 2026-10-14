// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Suite: delegation against duplication.
//!
//! The scripted parent hands five subgoals to sub-agents at once. Three are
//! independent: each reads its own two large files. Two overlap: each reads
//! the same two large files and one of its own. Every sub-agent answers with a
//! marked result, and the parent is finally asked to combine them.
//!
//! Measured: whether every sub-agent's result is in the request the parent
//! reads at the end, the tokens of the whole task including every sub-agent
//! (a sub-agent's requests are tokens too), and the reads that more than one
//! agent made of the same file (duplication). Delegation is scripted, so the
//! suite cannot show that a variant delegates better; it shows what delegating
//! costs the harness and how much of that cost is repeated work. Not measured:
//! whether a model would split the work this way.

use std::collections::BTreeMap;

use serde_json::json;

use super::corpus::{big_files, TurnBuilder};
use crate::rng::Rng;
use crate::task::{Evidence, EvidenceRole, Generated, ScriptedCall, Suite, Task, Turn};

pub const MEASURES: &str = "Harness side only, delegation scripted: whether every sub-agent's \
result is in the parent's request at the end, the tokens of the whole task including every \
sub-agent, and the reads that more than one agent made of the same file (duplication). Not \
measured: whether a model would split the work this way or combine the results well.";

const INDEPENDENT: usize = 3;
const OVERLAPPING: usize = 2;

pub fn generate(seed: u64) -> Generated {
    let mut rng = Rng::new(seed, "delegation");
    let mut task = Task::new(Suite::Delegation, seed, MEASURES);
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    let own = big_files(&mut rng, &mut files, "own", INDEPENDENT * 2 + OVERLAPPING);
    let shared = big_files(&mut rng, &mut files, "shared", 2);

    let mut reads: Vec<Vec<String>> = Vec::new();
    for pair in own[..INDEPENDENT * 2].chunks(2) {
        reads.push(pair.to_vec());
    }
    for extra in &own[INDEPENDENT * 2..] {
        let mut set = shared.clone();
        set.push(extra.clone());
        reads.push(set);
    }

    let mut children: Vec<Turn> = Vec::new();
    for (n, set) in reads.iter().enumerate() {
        let marker = format!("[ablation {} child {}]", task.id, n + 1);
        let result = format!("RESULT-{}", rng.hex(6));
        task.evidence.push(Evidence {
            marker: result.clone(),
            role: EvidenceRole::Critical,
            kind: "result".into(),
            source: format!("subgoal {}", n + 1),
        });
        children.push(Turn {
            user: format!("{marker} Read {} and report what is in it.", set.join(", ")),
            marker,
            calls: set
                .iter()
                .enumerate()
                .map(|(i, p)| ScriptedCall {
                    id: format!("dg-c{}-{i}", n + 1),
                    tool: "read_file".into(),
                    args: json!({"path": p}),
                })
                .collect(),
            reply: format!("{result}: subgoal {} done.", n + 1),
        });
    }

    let mut turns = TurnBuilder::new(&task.id, "dg");
    turns.push(
        "Split the work: hand each subgoal to a sub-agent.",
        children
            .iter()
            .enumerate()
            .map(|(n, c)| {
                (
                    "task",
                    json!({
                        "action": "spawn",
                        "prompt": c.user,
                        "description": format!("subgoal {}", n + 1),
                        "mode": "research",
                    }),
                )
            })
            .collect(),
        "Handed out the subgoals.",
    );
    turns.push(
        "Combine what the sub-agents found into one summary.",
        Vec::new(),
        "Summarised from the results in context.",
    );
    let mut segment = turns.finish();
    segment.children = children;
    task.segments.push(segment);
    Generated { task, files }
}
