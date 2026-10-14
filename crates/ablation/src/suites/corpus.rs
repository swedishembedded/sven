// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Deterministic source-like text for the generated repositories.
//!
//! Identifiers are drawn from a fixed vocabulary and carry a hex suffix, so
//! generated text never contains an identifier a task defines for itself.

use crate::rng::Rng;

pub const MODULES: [&str; 12] = [
    "auth",
    "billing",
    "cache",
    "catalog",
    "export",
    "gateway",
    "inventory",
    "jobs",
    "metrics",
    "queue",
    "search",
    "storage",
];

const NOUNS: [&str; 24] = [
    "ledger", "invoice", "session", "quota", "cursor", "shard", "lease", "batch", "policy",
    "index", "ticket", "bucket", "window", "snapshot", "channel", "profile", "segment", "manifest",
    "journal", "replica", "schedule", "webhook", "account", "token",
];

const VERBS: [&str; 20] = [
    "settle", "rotate", "flush", "compact", "resolve", "dispatch", "validate", "evict", "merge",
    "archive", "refresh", "sweep", "probe", "replay", "throttle", "audit", "seal", "rebuild",
    "drain", "verify",
];

/// A function name that is unique to the call site.
pub fn function_name(rng: &mut Rng) -> String {
    format!("{}_{}_{}", rng.pick(&VERBS), rng.pick(&NOUNS), rng.hex(4))
}

/// A file name stem like `ledger_sweep`.
pub fn file_stem(rng: &mut Rng) -> String {
    format!("{}_{}", rng.pick(&NOUNS), rng.pick(&VERBS))
}

/// `lines` lines of plausible Rust, none of which names anything but fresh
/// identifiers.
pub fn source_lines(rng: &mut Rng, module: &str, lines: usize) -> String {
    let mut out = format!("//! The {module} module.\n\nuse std::collections::BTreeMap;\n\n");
    let mut written = 4;
    while written < lines {
        let name = function_name(rng);
        let noun = rng.pick(&NOUNS);
        let body = [
            format!("pub fn {name}(input: &BTreeMap<String, u64>, limit: u64) -> u64 {{"),
            format!("    let mut total = 0_u64; // running {noun} total"),
            "    for (key, value) in input {".to_string(),
            format!("        if key.len() as u64 > limit {{ continue; }} // skip long {noun} keys"),
            format!(
                "        total = total.saturating_add(*value ^ {});",
                rng.below(255)
            ),
            "    }".to_string(),
            "    total".to_string(),
            "}".to_string(),
            String::new(),
        ];
        for line in body {
            out.push_str(&line);
            out.push('\n');
            written += 1;
        }
    }
    out
}

/// A line of log output.
pub fn log_line(rng: &mut Rng, second: usize) -> String {
    let level = ["INFO", "DEBUG", "INFO", "TRACE"][rng.below(4)];
    format!(
        "2026-03-14T09:{:02}:{:02}Z {level} {}::{} took {}ms ({} items)",
        second / 3600 % 60,
        second / 60 % 60,
        rng.pick(&MODULES),
        function_name(rng),
        rng.between(1, 900),
        rng.between(1, 400)
    )
}

/// Adds `count` large unrelated source files to `files` under `src/` and
/// returns their paths. They are what a model reads that has nothing to do
/// with the task, and what pushes older results out of a token budget.
pub fn big_files(
    rng: &mut Rng,
    files: &mut std::collections::BTreeMap<String, Vec<u8>>,
    tag: &str,
    count: usize,
) -> Vec<String> {
    (0..count)
        .map(|n| {
            let module = *rng.pick(&MODULES);
            let path = format!("src/{module}/{tag}_{n:02}_{}.rs", rng.hex(3));
            let lines = rng.between(150, 200);
            files.insert(path.clone(), source_lines(rng, module, lines).into_bytes());
            path
        })
        .collect()
}

/// Builds the turns of a scripted task. Every turn gets a marker unique to
/// the task, which the user text carries.
pub struct TurnBuilder<'a> {
    task_id: &'a str,
    prefix: &'a str,
    turns: Vec<crate::task::Turn>,
}

impl<'a> TurnBuilder<'a> {
    #[must_use]
    pub fn new(task_id: &'a str, prefix: &'a str) -> Self {
        Self {
            task_id,
            prefix,
            turns: Vec::new(),
        }
    }

    /// Adds a turn: the user text, one round of `(tool, args)` calls, and the
    /// scripted reply.
    pub fn push(&mut self, user: &str, calls: Vec<(&str, serde_json::Value)>, reply: &str) {
        let n = self.turns.len() + 1;
        let marker = format!("[ablation {} t{n:02}]", self.task_id);
        self.turns.push(crate::task::Turn {
            user: format!("{marker} {user}"),
            marker,
            calls: calls
                .into_iter()
                .enumerate()
                .map(|(i, (tool, args))| crate::task::ScriptedCall {
                    id: format!("{}-{n:02}-{i}", self.prefix),
                    tool: tool.into(),
                    args,
                })
                .collect(),
            reply: reply.into(),
            recover: None,
        });
    }

    /// Lets the scripted model fetch what a shortened result left out before
    /// it answers the last turn, following results that mention `focus`.
    /// Every term must be a word of that turn's question.
    ///
    /// # Panics
    ///
    /// No turn yet, or a term the question does not contain.
    pub fn recover_on(&mut self, focus: &[&str]) {
        let turn = self.turns.last_mut().expect("a turn to recover on");
        let question = turn.user.to_lowercase();
        for term in focus {
            assert!(
                question.contains(&term.to_lowercase()),
                "recovery focus {term:?} is not in the question"
            );
        }
        turn.recover = Some(crate::task::Recovery {
            focus: focus.iter().map(|t| (*t).to_string()).collect(),
        });
    }

    #[must_use]
    pub fn finish(self) -> crate::task::Segment {
        crate::task::Segment {
            turns: self.turns,
            children: Vec::new(),
        }
    }
}
