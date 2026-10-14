// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Suite: many similar tools.
//!
//! A catalog of 40 to 144 tools whose descriptions overlap: the same verb in
//! several domains, the same object under several verbs. The catalog order is
//! shuffled by the seed. Eight queries, three using the words of a tool's
//! name, three paraphrasing it and two naming its domain only indirectly,
//! each have exactly one tool that answers them. The scripted model searches
//! the catalog once per query and finally calls the first query's tool.
//!
//! Measured: shortlist recall (is the right tool among the results the model
//! is shown), top-1 rate, reciprocal rank, and the tokens the catalog costs by
//! source. Where every tool is offered in full the shortlist is the whole
//! catalog and top-1 is not measured. Not measured: whether a model picks the
//! right tool from a shortlist.

use serde_json::json;

use super::corpus::TurnBuilder;
use crate::providers::OracleLabel;
use crate::rng::Rng;
use crate::task::{CatalogTool, Generated, QueryStyle, SearchProbe, Suite, Task};

pub const MEASURES: &str = "Harness side only, model behaviour scripted: whether the tool that \
answers each query is among the search results shown to the model (shortlist recall), whether it \
is first (top-1), reciprocal rank, and the tokens the catalog costs by source. Under full \
disclosure the shortlist is the whole catalog and top-1 is not measured. Not measured: whether a \
model chooses correctly from a shortlist.";

/// The name the MCP server is configured under; tools are shown as
/// `<server>-<tool>`.
pub const SERVER: &str = "catalog";

/// The name a tool is shown under.
#[must_use]
pub fn shown_name(tool: &str) -> String {
    format!("{SERVER}-{tool}")
}

struct Domain {
    name: &'static str,
    /// An indirect way to refer to the domain.
    around: &'static str,
    objects: [(&'static str, &'static str); 3],
}

const DOMAINS: [Domain; 8] = [
    Domain {
        name: "billing",
        around: "money owed by customers",
        objects: [
            ("invoice", "bill"),
            ("payment", "remittance"),
            ("subscription", "recurring plan"),
        ],
    },
    Domain {
        name: "inventory",
        around: "goods held in the warehouse",
        objects: [
            ("item", "product"),
            ("shipment", "delivery"),
            ("supplier", "vendor"),
        ],
    },
    Domain {
        name: "support",
        around: "helping customers with problems",
        objects: [
            ("ticket", "support request"),
            ("agent", "responder"),
            ("macro", "canned reply"),
        ],
    },
    Domain {
        name: "calendar",
        around: "scheduling people's time",
        objects: [
            ("event", "appointment"),
            ("room", "meeting space"),
            ("reminder", "nudge"),
        ],
    },
    Domain {
        name: "hosting",
        around: "running services on servers",
        objects: [
            ("deployment", "rollout"),
            ("domain", "web address"),
            ("certificate", "tls credential"),
        ],
    },
    Domain {
        name: "analytics",
        around: "measuring what users do",
        objects: [
            ("report", "summary"),
            ("dashboard", "overview"),
            ("funnel", "conversion path"),
        ],
    },
    Domain {
        name: "payroll",
        around: "paying employees",
        objects: [
            ("salary", "wage"),
            ("timesheet", "hours record"),
            ("bonus", "extra pay"),
        ],
    },
    Domain {
        name: "library",
        around: "lending books",
        objects: [
            ("loan", "borrowing"),
            ("catalogue", "book index"),
            ("member", "reader"),
        ],
    },
];

/// Verb, a paraphrase of it, and a clause for a description.
const VERBS: [(&str, &str, &str); 6] = [
    ("create", "make a new", "Creates a new"),
    ("list", "show all", "Lists the"),
    ("cancel", "call off", "Cancels an existing"),
    ("export", "download a copy of", "Exports a copy of a"),
    ("archive", "put away", "Archives a"),
    ("update", "change", "Updates the fields of a"),
];

pub fn generate(seed: u64) -> Generated {
    let mut rng = Rng::new(seed, "tool_selection");
    let mut task = Task::new(Suite::ToolSelection, seed, MEASURES);

    // Every (domain, object, verb) combination, in a seeded order; the catalog
    // is the first `size` of them.
    let mut combos: Vec<(usize, usize, usize)> = Vec::new();
    for (d, domain) in DOMAINS.iter().enumerate() {
        for o in 0..domain.objects.len() {
            for v in 0..VERBS.len() {
                combos.push((d, o, v));
            }
        }
    }
    rng.shuffle(&mut combos);
    let size = rng.between(40, 144);
    combos.truncate(size);

    let describe = |rng: &mut Rng, (d, o, v): (usize, usize, usize)| {
        let domain = &DOMAINS[d];
        let (object, synonym) = domain.objects[o];
        let (_, _, clause) = VERBS[v];
        let also = if rng.below(2) == 0 {
            format!(" Also known as working with a {synonym}.")
        } else {
            String::new()
        };
        format!(
            "{clause} {object} in the {} system. Takes the identifier of the record and returns \
             the updated record.{also}",
            domain.name
        )
    };
    task.catalog = combos
        .iter()
        .map(|&combo| {
            let (d, o, v) = combo;
            CatalogTool {
                name: format!(
                    "{}_{}_{}",
                    DOMAINS[d].name, DOMAINS[d].objects[o].0, VERBS[v].0
                ),
                description: describe(&mut rng, combo),
            }
        })
        .collect();

    // Eight probes on distinct tools of the catalog.
    let mut targets = combos.clone();
    rng.shuffle(&mut targets);
    let styles = [
        QueryStyle::Exact,
        QueryStyle::Exact,
        QueryStyle::Exact,
        QueryStyle::Paraphrase,
        QueryStyle::Paraphrase,
        QueryStyle::Paraphrase,
        QueryStyle::Indirect,
        QueryStyle::Indirect,
    ];
    for (&(d, o, v), style) in targets.iter().zip(styles) {
        let domain = &DOMAINS[d];
        let (object, object_synonym) = domain.objects[o];
        let (verb, verb_synonym, _) = VERBS[v];
        let query = match style {
            QueryStyle::Exact => format!("{verb} {object} {}", domain.name),
            QueryStyle::Paraphrase => {
                format!(
                    "I need to {verb_synonym} {object_synonym} in {}",
                    domain.name
                )
            }
            QueryStyle::Indirect => {
                format!("{verb_synonym} {object_synonym} for {}", domain.around)
            }
        };
        let target = shown_name(&format!("{}_{object}_{verb}", domain.name));
        task.oracle_labels.push(OracleLabel {
            decision_type: "tool_routing".into(),
            state_contains: query.clone(),
            correct_option: target.clone(),
        });
        task.probes.push(SearchProbe {
            query,
            style,
            target,
        });
    }

    let mut turns = TurnBuilder::new(&task.id, "ts");
    for probe in &task.probes {
        turns.push(
            &format!("Find the tool for this: {}", probe.query),
            vec![(
                "tools",
                json!({"action": "search", "query": probe.query, "limit": 8}),
            )],
            "Searched the catalog.",
        );
    }
    let first = task.probes[0].target.clone();
    turns.push(
        "Now use the first tool you found.",
        vec![(first.as_str(), json!({"id": "r-1"}))],
        "Called the tool.",
    );
    task.segments.push(turns.finish());
    Generated {
        task,
        files: [("README.md".to_string(), b"Catalog task.\n".to_vec())].into(),
    }
}
