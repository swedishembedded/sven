// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The task generators: deterministic per seed, identical for every variant,
//! and shaped as each suite's definition says.

use std::collections::BTreeSet;

use regex::Regex;
use sven_ablation::mockscript::{render_conversation, render_mock};
use sven_ablation::suites::{generate, tool_selection};
use sven_ablation::task::{read_task, EvidenceRole, Generated, QueryStyle, Suite};

fn text(g: &Generated, path: &str) -> String {
    String::from_utf8(g.files[path].clone()).unwrap()
}

fn occurrences(g: &Generated, needle: &str) -> usize {
    g.files
        .values()
        .map(|b| String::from_utf8_lossy(b).matches(needle).count())
        .sum()
}

#[test]
fn every_suite_is_deterministic_per_seed_and_varies_between_seeds() {
    for suite in Suite::ALL {
        let a = generate(suite, 11);
        assert_eq!(a, generate(suite, 11), "{suite:?} must repeat for a seed");
        assert_ne!(a, generate(suite, 12), "{suite:?} must vary with the seed");
        assert_eq!(a.task.id, format!("{}-s11", suite.name()));
    }
}

#[test]
fn every_suite_states_what_it_does_not_measure() {
    for suite in Suite::ALL {
        let task = generate(suite, 1).task;
        assert!(task.measures.contains("Not measured"), "{suite:?}");
        assert!(task.measures.contains("scripted"), "{suite:?}");
    }
}

#[test]
fn a_task_is_written_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let g = generate(Suite::NoisyOutput, 3);
    g.write(dir.path()).unwrap();
    assert_eq!(read_task(dir.path()).unwrap(), g.task);
    assert!(dir.path().join("repo/ci/build.log").exists());
}

#[test]
fn a_task_of_another_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    generate(Suite::NoisyOutput, 3).write(dir.path()).unwrap();
    let path = dir.path().join("task.json");
    let json = std::fs::read_to_string(&path)
        .unwrap()
        .replace("sven.ablation.task/1", "sven.ablation.task/9");
    std::fs::write(&path, json).unwrap();
    assert!(read_task(dir.path()).unwrap_err().contains("schema"));
}

#[test]
fn the_scripted_model_replays_every_turn_of_every_suite() {
    use futures::StreamExt;
    use sven_model::{CompletionRequest, Message, ModelProvider, ResponseEvent};
    use sven_model_mock::YamlMockProvider;

    for suite in Suite::ALL {
        let task = generate(suite, 5).task;
        let log = tempfile::tempdir().unwrap();
        let yaml = render_mock(&task.segments[0], &log.path().join("requests.jsonl"));
        let provider = YamlMockProvider::load(&yaml).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for turn in &task.segments[0].turns {
            let request = CompletionRequest {
                messages: vec![Message::user(turn.user.as_str())],
                stream: true,
                ..Default::default()
            };
            let events: Vec<ResponseEvent> = runtime.block_on(async {
                let mut stream = provider.complete(request).await.unwrap();
                let mut out = Vec::new();
                while let Some(e) = stream.next().await {
                    out.push(e.unwrap());
                }
                out
            });
            let called: Vec<&str> = events
                .iter()
                .filter_map(|e| match e {
                    ResponseEvent::ToolCall { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect();
            let wanted: Vec<&str> = turn.calls.iter().map(|c| c.tool.as_str()).collect();
            assert_eq!(called, wanted, "{suite:?} turn {}", turn.marker);
        }
        let conversation = render_conversation(&task, 0);
        for turn in &task.segments[0].turns {
            assert!(conversation.contains(&turn.marker));
        }
    }
}

#[test]
fn relevant_file_hides_the_defect_among_hundreds_of_files_with_misleading_matches() {
    for seed in 0..6 {
        let g = generate(Suite::RelevantFile, seed);
        assert!(
            (200..420).contains(&g.files.len()),
            "{} files",
            g.files.len()
        );

        let critical: Vec<_> = g.task.critical().collect();
        assert_eq!(critical.len(), 2);
        for e in &critical {
            assert_eq!(occurrences(&g, &e.marker), 1, "{} appears once", e.marker);
            assert!(text(&g, &e.source).contains(&e.marker));
        }
        let distractors: Vec<_> = g.task.distractors().collect();
        assert!(distractors.len() >= 8);
        assert!(distractors
            .iter()
            .all(|e| e.role == EvidenceRole::Distractor));

        // The searched symbol is in every misleading file and in the relevant one.
        let grep = &g.task.segments[0].turns[0].calls[0];
        let symbol = grep.args["pattern"].as_str().unwrap();
        let mentioning = g
            .files
            .values()
            .filter(|b| String::from_utf8_lossy(b).contains(symbol))
            .count();
        assert_eq!(mentioning, distractors.len() + 1);

        // Every scripted read exists, and together they exceed the default
        // assembly budget so older results must be demoted.
        let mut read_bytes = 0;
        for turn in &g.task.segments[0].turns {
            for call in turn.calls.iter().filter(|c| c.tool == "read_file") {
                let path = call.args["path"].as_str().unwrap();
                read_bytes += g.files.get(path).unwrap_or_else(|| panic!("{path}")).len();
            }
        }
        assert!(read_bytes / 4 > 12_000, "{read_bytes} bytes read");
        assert!(g.task.segments[0].turns.last().unwrap().calls.is_empty());
    }
}

#[test]
fn noisy_output_buries_causes_and_effects_among_lookalikes() {
    let words = Regex::new(r"(?i)error|fail|panic|warn").unwrap();
    for seed in 0..6 {
        let g = generate(Suite::NoisyOutput, seed);
        let log = text(&g, "ci/build.log");
        let lines = log.lines().count();
        assert!((1500..2500).contains(&lines), "{lines} lines");

        let critical = g.task.critical().count();
        assert!(
            critical.is_multiple_of(2) && (6..=10).contains(&critical),
            "{critical}"
        );
        let lookalikes = g.task.distractors().count();
        assert!((10..=70).contains(&lookalikes));
        for e in &g.task.evidence {
            assert_eq!(occurrences(&g, &e.marker), 1, "{}", e.marker);
        }

        // The scripted grep selects every labelled line: nothing is hidden
        // from the model by the command itself, only by what is done with its
        // output afterwards.
        let command = g.task.segments[0].turns[0].calls[0].args["shell_command"]
            .as_str()
            .unwrap();
        assert!(command.contains("error|fail|panic|warn"));
        for e in &g.task.evidence {
            let line = log.lines().find(|l| l.contains(&e.marker)).unwrap();
            assert!(words.is_match(line), "{line}");
        }
    }
}

#[test]
fn tool_selection_offers_many_overlapping_tools_and_eight_distinct_probes() {
    for seed in 0..6 {
        let task = generate(Suite::ToolSelection, seed).task;
        assert!(
            (40..=144).contains(&task.catalog.len()),
            "{}",
            task.catalog.len()
        );
        let names: BTreeSet<_> = task.catalog.iter().map(|t| &t.name).collect();
        assert_eq!(names.len(), task.catalog.len(), "names are unique");

        assert_eq!(task.probes.len(), 8);
        let targets: BTreeSet<_> = task.probes.iter().map(|p| &p.target).collect();
        assert_eq!(targets.len(), 8);
        for probe in &task.probes {
            let bare = probe.target.strip_prefix("catalog-").unwrap();
            assert!(task.catalog.iter().any(|t| t.name == bare), "{bare}");
            assert_eq!(probe.target, tool_selection::shown_name(bare));
            let words: Vec<&str> = bare.split('_').collect();
            match probe.style {
                QueryStyle::Exact => assert!(words.iter().all(|w| probe.query.contains(w))),
                QueryStyle::Paraphrase | QueryStyle::Indirect => {
                    assert!(
                        !probe.query.contains(words[2]),
                        "{} for {bare}",
                        probe.query
                    );
                }
            }
        }
        // Each probe provokes a labelled decision for the oracle.
        assert_eq!(task.oracle_labels.len(), 8);
        // The domain name is shared by many tools, so a query cannot be
        // answered by it alone.
        let domain = task.probes[0]
            .target
            .trim_start_matches("catalog-")
            .split('_')
            .next()
            .unwrap()
            .to_string();
        let same_domain = task
            .catalog
            .iter()
            .filter(|t| t.name.starts_with(&domain))
            .count();
        assert!(same_domain >= 3);
    }
}

#[test]
fn tasks_do_not_depend_on_anything_but_suite_and_seed() {
    // The variant is not an input of generation: the signature takes none.
    let a = generate(Suite::ToolSelection, 21);
    let b = generate(Suite::ToolSelection, 21);
    assert_eq!(a.task.catalog, b.task.catalog);
    assert_eq!(a.task.segments, b.task.segments);
}

#[test]
fn restart_survival_spreads_rules_and_findings_over_three_processes() {
    for seed in 0..6 {
        let g = generate(Suite::RestartSurvival, seed);
        assert_eq!(g.task.segments.len(), 3, "two cold restarts");
        // Three constraints and three verified findings are critical; the
        // unverified rumours are not.
        let critical: Vec<_> = g.task.critical().collect();
        assert_eq!(critical.len(), 6);
        assert!(g.task.distractors().count() >= 6);
        for e in &g.task.evidence {
            if e.kind == "constraint" {
                // A constraint is stated by the user in the segment that gives it.
                let said = g
                    .task
                    .segments
                    .iter()
                    .any(|s| s.turns.iter().any(|t| t.user.contains(&e.marker)));
                assert!(said, "{} is never said", e.marker);
            } else {
                assert_eq!(occurrences(&g, &e.marker), 1, "{}", e.marker);
            }
        }
        // Only the last segment is asked to recall everything.
        let last = g.task.segments.last().unwrap().turns.last().unwrap();
        assert!(last.calls.is_empty());
        assert!(g.task.segments[0]
            .turns
            .iter()
            .all(|t| !t.user.contains("List the ground rules")));
    }
}

#[test]
fn security_attempts_only_what_the_product_says_it_blocks() {
    let g = generate(Suite::Security, 2);
    let spec = g.task.security.as_ref().unwrap();
    assert_eq!(g.task.mode, "research");
    assert_eq!(spec.canaries.len(), 2);
    for canary in &spec.canaries {
        assert!(text(&g, &canary.path).contains(&canary.marker));
        assert_eq!(occurrences(&g, &canary.marker), 1);
    }
    // Every forbidden shell command matches a configured deny pattern.
    let config: serde_yaml::Value = serde_yaml::from_str(&g.task.base_config_yaml).unwrap();
    let patterns: Vec<&str> = config["tools"]["deny_patterns"]
        .as_sequence()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    let denied = ["cat .env", "cat secrets/prod.key", "rm -rf build_cache"];
    for command in denied {
        let hit = patterns.iter().any(|p| {
            let re = format!("^{}$", regex::escape(p).replace("\\*", ".*"));
            Regex::new(&re).unwrap().is_match(command)
        });
        assert!(hit, "{command} is not covered by {patterns:?}");
    }
    // The permitted read is not covered by any pattern.
    assert!(!patterns.iter().any(|p| {
        let re = format!("^{}$", regex::escape(p).replace("\\*", ".*"));
        Regex::new(&re).unwrap().is_match("cat docs/public.md")
    }));
    assert!(spec
        .protected_files
        .iter()
        .all(|(p, _)| g.files.contains_key(p)));
}
