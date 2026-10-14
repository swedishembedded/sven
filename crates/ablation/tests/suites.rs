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
    assert_eq!(spec.canaries.len(), 2);
    // The rules reach the model by being read, at the start.
    let first = &g.task.segments[0].turns[0].calls[0];
    assert_eq!(
        (first.tool.as_str(), first.args["path"].as_str()),
        ("read_file", Some("AGENTS.md"))
    );
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
    // The ledger is protected by disabling the tools that write files.
    let disabled: Vec<&str> = config["tools"]["disabled"]
        .as_sequence()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(disabled.contains(&"write_file") && disabled.contains(&"edit_file"));
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

#[test]
fn delegation_hands_five_subgoals_to_sub_agents_two_of_which_overlap() {
    for seed in 0..6 {
        let g = generate(Suite::Delegation, seed);
        let segment = &g.task.segments[0];
        assert_eq!(segment.children.len(), 5);
        // The parent delegates all five in one round, then combines.
        assert_eq!(segment.turns[0].calls.len(), 5);
        assert!(segment.turns[0].calls.iter().all(|c| c.tool == "task"));
        assert!(segment.turns[1].calls.is_empty());
        // Each sub-agent reads existing files and answers with its own result.
        let mut reads: Vec<Vec<&str>> = Vec::new();
        for child in &segment.children {
            let paths: Vec<&str> = child
                .calls
                .iter()
                .map(|c| c.args["path"].as_str().unwrap())
                .collect();
            assert!(paths.iter().all(|p| g.files.contains_key(*p)));
            reads.push(paths);
        }
        let all: Vec<&str> = reads.iter().flatten().copied().collect();
        let distinct: BTreeSet<&str> = all.iter().copied().collect();
        assert_eq!(
            all.len() - distinct.len(),
            2,
            "the two overlapping subgoals share two files"
        );
        let results: Vec<_> = g.task.critical().collect();
        assert_eq!(results.len(), 5);
        for (r, child) in results.iter().zip(&segment.children) {
            assert!(child.reply.contains(&r.marker));
            // The prompt the parent sends is the instruction the child's script matches.
            assert!(segment.turns[0]
                .calls
                .iter()
                .any(|c| c.args["prompt"] == child.user.as_str()));
        }
    }
}

// ── recovery ─────────────────────────────────────────────────────────────────

#[test]
fn the_suites_where_demotion_applies_script_a_recovery_from_the_question_itself() {
    for suite in [Suite::RelevantFile, Suite::NoisyOutput] {
        for seed in 1..4 {
            let task = generate(suite, seed).task;
            let last = task.segments[0].turns.last().unwrap();
            let recover = last.recover.as_ref().unwrap_or_else(|| panic!("{suite:?}"));
            assert!(!recover.focus.is_empty());
            let question = last.user.to_lowercase();
            for term in &recover.focus {
                assert!(
                    question.contains(&term.to_lowercase()),
                    "{suite:?}: focus {term:?} is not in the question"
                );
                for e in &task.evidence {
                    assert!(
                        !term.contains(&e.marker),
                        "the focus must not be an evidence label"
                    );
                }
            }
            assert!(
                task.segments[0].turns[..task.segments[0].turns.len() - 1]
                    .iter()
                    .all(|t| t.recover.is_none()),
                "only the final question recovers"
            );
        }
    }
}

#[test]
fn a_script_recovers_only_when_asked_to() {
    use sven_ablation::mockscript::render_recovering_mock;
    let task = generate(Suite::RelevantFile, 2).task;
    let log = std::path::Path::new("/nonexistent/requests.jsonl");
    let plain = render_mock(&task.segments[0], log);
    let recovering = render_recovering_mock(&task.segments[0], log);
    assert!(!plain.contains("recover"));
    assert!(recovering.contains("recover"));
    sven_model_mock::YamlMockProvider::load(&plain).unwrap();
    sven_model_mock::YamlMockProvider::load(&recovering).unwrap();
}

#[test]
fn the_recovery_pattern_matches_the_reference_a_demoted_result_carries() {
    use sven_ablation::mockscript::EXPAND_REFERENCE;
    use sven_chunks::{render, ChunkId, ChunkKind, RenderRequest, Visibility};
    let raw = (1..=80).map(|n| format!("line {n}\n")).collect::<String>();
    let id = ChunkId::for_content(raw.as_bytes());
    let shown = render(
        &RenderRequest {
            id: id.clone(),
            kind: ChunkKind::File,
            source_tool: "read_file",
            path: Some("src/a.rs"),
            failure: false,
            pinned: false,
            raw: &raw,
            focus: None,
            note: None,
        },
        Visibility::Short,
    );
    let re = Regex::new(EXPAND_REFERENCE).unwrap();
    let found: Vec<String> = re
        .captures_iter(&shown.text)
        .map(|c| c[1].to_string())
        .collect();
    assert!(!found.is_empty() && found.iter().all(|f| *f == id.to_string()));
}

// ── harder fixtures ──────────────────────────────────────────────────────────

#[test]
fn noisy_output_look_alikes_are_half_leveled_and_half_bare() {
    let (mut leveled, mut bare) = (0, 0);
    for seed in 1..7 {
        let g = generate(Suite::NoisyOutput, seed);
        let log = text(&g, "ci/build.log");
        for line in log.lines().filter(|l| l.contains("(DIST-")) {
            if ["WARN ", "INFO ", "DEBUG "]
                .iter()
                .any(|p| line.starts_with(p))
            {
                leveled += 1;
            } else {
                bare += 1;
            }
        }
    }
    assert!(leveled > 20 && bare > 20, "{leveled} leveled, {bare} bare");
}

#[test]
fn relevant_file_puts_the_defect_in_the_named_function_on_even_seeds_and_in_a_helper_on_odd_ones() {
    let (mut inline, mut delegated) = (0, 0);
    for seed in 1..9 {
        let g = generate(Suite::RelevantFile, seed);
        let symbol = g.task.segments[0].turns[0].calls[0].args["pattern"]
            .as_str()
            .unwrap()
            .to_string();
        let critical: Vec<_> = g.task.critical().collect();
        let source = text(&g, &critical[0].source);
        let cause = source
            .lines()
            .position(|l| l.contains(&critical[0].marker))
            .unwrap();
        let lines: Vec<&str> = source.lines().collect();
        let declaration = lines[..=cause]
            .iter()
            .rev()
            .find(|l| l.contains("fn "))
            .unwrap();
        assert_eq!(occurrences(&g, &critical[0].marker), 1);
        assert_eq!(
            declaration.contains(&format!("fn {symbol}(")),
            seed % 2 == 0,
            "seed {seed}"
        );
        if declaration.contains(&format!("fn {symbol}(")) {
            inline += 1;
        } else {
            delegated += 1;
            assert!(
                source.contains(&format!("{symbol}(entries")),
                "the named function is still defined"
            );
        }
    }
    assert!(
        inline > 0 && delegated > 0,
        "{inline} inline, {delegated} delegated"
    );
}
