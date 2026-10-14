// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping delegated agent work
// cheap and non-duplicative for its clients. If your team needs expertise in
// multi-agent context handoff then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Deduplication of delegated subgoals.
//!
//! Before a `task` call launches a sub-agent it registers a [`Subgoal`]: the
//! normalized goal, the declared scope (files or globs, evidence chunk ids)
//! and whether the child may write. A read-only subgoal that repeats one
//! already in flight, or one that finished over files that have not changed,
//! is answered by reference instead of launching again.
//!
//! Detection is deterministic and conservative ([`relate`]):
//!
//! - **Same** (deduplicated automatically): both read-only, in the same mode,
//!   the same normalized goal and deliverable, and the new scope fully
//!   contained in the registered one. A subgoal with no declared scope is
//!   contained only in one with none either: an unknown scope is never assumed
//!   to be inside a known one.
//! - **Near**: as Same, but the wording differs while sharing most of its
//!   words. Only this case may ask a [`NearMatch`] advisor, and an advisor that
//!   abstains, fails or says no means the subgoal launches.
//! - **Distinct**: everything else, always, and every writing subgoal: nothing
//!   probabilistic can suppress a write, and two writers are never merged.
//!
//! A finished result is reused only when every declared path is a literal file
//! whose size and modification time are what they were when the original
//! child was launched; a subgoal declaring globs or no files is deduplicated
//! only while its twin is in flight. A twin that fails or is cancelled is
//! never reused: the waiting call launches its own child.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use sven_chunks::{content_digest, ChunkId};
use sven_tool_api::PathScope;
use sven_workspace::glob_match;
use tokio::sync::watch;

/// Most declared paths one subgoal carries.
pub(super) const MAX_PATHS: usize = 64;
/// Longest declared path, in bytes.
const MAX_PATH_BYTES: usize = 256;
/// The least share of words (Jaccard) two goals must have to be a near match.
const NEAR_SHARE: f64 = 0.5;

/// Whether a child may change anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Access {
    ReadOnly,
    Write,
}

/// What a `task` call asks of a sub-agent, in the form duplicates are found by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Subgoal {
    mode: String,
    goal: String,
    paths: BTreeSet<String>,
    evidence: BTreeSet<String>,
    access: Access,
}

/// The parts of a call a [`Subgoal`] is made from.
pub(super) struct SubgoalSpec<'a> {
    pub(super) mode: &'a str,
    pub(super) prompt: &'a str,
    pub(super) deliverable: Option<&'a str>,
    pub(super) paths: &'a [String],
    pub(super) evidence: &'a [ChunkId],
    pub(super) access: Access,
}

/// The goal in canonical form: lower case, words separated by single spaces,
/// punctuation at the edges of a word dropped.
pub(super) fn normalize(text: &str) -> String {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| c.is_ascii_punctuation() && !matches!(c, '/' | '_' | '.'))
                .trim_end_matches('.')
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The declared paths of a call's `paths` argument, validated.
pub(super) fn parse_paths(args: &serde_json::Value) -> Result<Vec<String>, String> {
    let items = match args.get("paths") {
        None | Some(serde_json::Value::Null) => return Ok(Vec::new()),
        Some(serde_json::Value::Array(items)) => items,
        Some(_) => return Err("'paths' must be an array of strings".into()),
    };
    if items.len() > MAX_PATHS {
        return Err(format!(
            "'paths' names {} entries; at most {MAX_PATHS}",
            items.len()
        ));
    }
    items
        .iter()
        .enumerate()
        .map(|(n, item)| match item.as_str() {
            Some(p) if !p.trim().is_empty() && p.len() <= MAX_PATH_BYTES => {
                Ok(p.trim().to_string())
            }
            _ => Err(format!(
                "paths[{n}] must be a non-empty string of at most {MAX_PATH_BYTES} bytes"
            )),
        })
        .collect()
}

impl Subgoal {
    pub(super) fn new(spec: &SubgoalSpec<'_>) -> Self {
        let mut goal = normalize(spec.prompt);
        if let Some(deliverable) = spec.deliverable {
            goal.push_str(" => ");
            goal.push_str(&normalize(deliverable));
        }
        Self {
            mode: spec.mode.to_string(),
            goal,
            paths: spec
                .paths
                .iter()
                .map(|p| p.trim_start_matches("./").to_string())
                .collect(),
            evidence: spec.evidence.iter().map(ToString::to_string).collect(),
            access: spec.access,
        }
    }

    /// The canonical goal text, for display in traces and for the advisor.
    pub(super) fn goal(&self) -> &str {
        &self.goal
    }

    /// A digest of the goal, mode and scope, stable across wording that
    /// normalizes the same.
    pub(super) fn digest(&self) -> String {
        content_digest(
            format!(
                "{}\n{}\n{:?}\n{:?}",
                self.mode, self.goal, self.paths, self.evidence
            )
            .as_bytes(),
        )
    }

    fn scope_within(&self, other: &Self) -> bool {
        let covered = |p: &String| {
            other
                .paths
                .iter()
                .any(|e| e == p || (is_glob(e) && glob_match(e, p)))
        };
        let paths = if self.paths.is_empty() {
            other.paths.is_empty()
        } else {
            self.paths.iter().all(covered)
        };
        let evidence = if self.evidence.is_empty() {
            other.evidence.is_empty()
        } else {
            self.evidence.is_subset(&other.evidence)
        };
        paths && evidence
    }
}

fn is_glob(path: &str) -> bool {
    path.contains(['*', '?'])
}

/// How a new subgoal relates to a registered one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Relation {
    Same,
    Near,
    Distinct,
}

/// How `new` relates to `registered`. See the module documentation.
pub(super) fn relate(new: &Subgoal, registered: &Subgoal) -> Relation {
    if new.access == Access::Write
        || registered.access == Access::Write
        || new.mode != registered.mode
        || !new.scope_within(registered)
    {
        return Relation::Distinct;
    }
    if new.goal == registered.goal {
        Relation::Same
    } else if word_share(&new.goal, &registered.goal) >= NEAR_SHARE {
        Relation::Near
    } else {
        Relation::Distinct
    }
}

fn word_share(a: &str, b: &str) -> f64 {
    let a: BTreeSet<&str> = a.split(' ').collect();
    let b: BTreeSet<&str> = b.split(' ').collect();
    let union = a.union(&b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(&b).count() as f64 / union as f64
}

/// Says whether two near-identical read-only goals ask for the same thing.
/// `None` is an abstention (or a failure): the subgoal launches.
#[async_trait]
pub(crate) trait NearMatch: Send + Sync {
    async fn same_goal(&self, new: &str, registered: &str) -> Option<bool>;
}

/// What a call that joined a running twin is told: where the result is.
pub(super) fn joined_text(done: &Outcome) -> String {
    format!(
        "Not launched again: the same read-only subgoal was handed to sub-agent {} in this \
         session and has finished. Its result is that call's result; read it with \
         action=read, handle={}.",
        done.handle, done.handle
    )
}

/// What a call answered from a finished twin is told: its result.
pub(super) fn reused_text(done: &Outcome) -> String {
    format!(
        "Not launched again: the same read-only subgoal finished earlier as sub-agent {} and \
         the files it declared are unchanged since. Its result:\n\n{}",
        done.handle, done.text
    )
}

// ── Finished results ─────────────────────────────────────────────────────────

/// The state of the declared files when a child was launched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Fingerprint(Vec<(String, Option<(u64, SystemTime)>)>);

impl Fingerprint {
    /// The state now of the literal files `subgoal` declares, or `None` when
    /// it declares none or declares a glob: nothing then says the files are
    /// unchanged.
    pub(super) fn of(subgoal: &Subgoal, scope: &PathScope) -> Option<Self> {
        if subgoal.paths.is_empty() || subgoal.paths.iter().any(|p| is_glob(p)) {
            return None;
        }
        let state = |p: &String| {
            let version = scope
                .resolve(p)
                .ok()
                .and_then(|path| std::fs::metadata(path).ok())
                .and_then(|m| Some((m.len(), m.modified().ok()?)));
            (p.clone(), version)
        };
        Some(Self(subgoal.paths.iter().map(state).collect()))
    }
}

/// A finished child's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Outcome {
    pub(super) handle: String,
    pub(super) text: String,
}

enum State {
    InFlight(watch::Receiver<Option<Outcome>>),
    Done {
        outcome: Outcome,
        fingerprint: Option<Fingerprint>,
    },
}

struct Entry {
    id: u64,
    subgoal: Subgoal,
    state: State,
}

/// What a call should do about its subgoal.
pub(super) enum Admission {
    /// Nothing equivalent exists: launch, and report through the leader.
    Launch(Leader),
    /// An equivalent child is running: wait for it.
    Join(watch::Receiver<Option<Outcome>>),
    /// An equivalent child finished over files that are unchanged.
    Reuse(Outcome),
}

/// The session's registry of subgoals.
#[derive(Default)]
pub(super) struct SubgoalRegistry {
    entries: Mutex<Vec<Entry>>,
}

/// The call that launched a child. Dropping it without [`Leader::finish`]
/// withdraws the subgoal, and calls waiting on it launch their own.
pub(super) struct Leader {
    registry: Arc<SubgoalRegistry>,
    id: u64,
    tx: watch::Sender<Option<Outcome>>,
    fingerprint: Option<Fingerprint>,
    finished: bool,
}

impl Leader {
    /// The child ended with `outcome`; `None` when it failed or was cancelled.
    pub(super) fn finish(mut self, outcome: Option<Outcome>) {
        self.finished = true;
        match outcome {
            Some(outcome) => {
                self.registry
                    .settle(self.id, &outcome, self.fingerprint.take());
                let _ = self.tx.send(Some(outcome));
            }
            None => self.registry.withdraw(self.id),
        }
    }
}

impl Drop for Leader {
    fn drop(&mut self) {
        if !self.finished {
            self.registry.withdraw(self.id);
        }
    }
}

impl SubgoalRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Subgoals handed to a child that has not finished.
    pub(super) fn in_flight(&self) -> u64 {
        let running = self
            .lock()
            .iter()
            .filter(|e| matches!(e.state, State::InFlight(_)))
            .count();
        u64::try_from(running).unwrap_or(u64::MAX)
    }

    fn withdraw(&self, id: u64) {
        self.lock().retain(|e| e.id != id);
    }

    fn settle(&self, id: u64, outcome: &Outcome, fingerprint: Option<Fingerprint>) {
        if let Some(entry) = self.lock().iter_mut().find(|e| e.id == id) {
            entry.state = State::Done {
                outcome: outcome.clone(),
                fingerprint,
            };
        }
    }

    /// Decides what the call for `subgoal` does. `scope` resolves declared
    /// paths; `advisor` is asked only about a near match.
    pub(super) async fn admit(
        self: &Arc<Self>,
        subgoal: Subgoal,
        scope: &PathScope,
        advisor: Option<&dyn NearMatch>,
    ) -> Admission {
        let near = {
            let mut entries = self.lock();
            if let Some(found) = find_same(&entries, &subgoal, scope) {
                return found;
            }
            let near = advisor.and_then(|_| best_near(&entries, &subgoal));
            if near.is_none() {
                return self.register(&mut entries, subgoal, scope);
            }
            near
        };
        let verdict = match (advisor, &near) {
            (Some(advisor), Some((_, goal))) => advisor.same_goal(subgoal.goal(), goal).await,
            _ => None,
        };
        let mut entries = self.lock();
        // Another identical call may have registered while the advisor was asked.
        if let Some(found) = find_same(&entries, &subgoal, scope) {
            return found;
        }
        if verdict == Some(true) {
            if let Some((id, _)) = near {
                if let Some(found) = entries
                    .iter()
                    .find(|e| e.id == id)
                    .and_then(|e| reuse(e, scope))
                {
                    return found;
                }
            }
        }
        self.register(&mut entries, subgoal, scope)
    }

    fn register(
        self: &Arc<Self>,
        entries: &mut Vec<Entry>,
        subgoal: Subgoal,
        scope: &PathScope,
    ) -> Admission {
        let id = entries.iter().map(|e| e.id).max().map_or(0, |m| m + 1);
        let (tx, rx) = watch::channel(None);
        let fingerprint = Fingerprint::of(&subgoal, scope);
        entries.push(Entry {
            id,
            subgoal,
            state: State::InFlight(rx),
        });
        Admission::Launch(Leader {
            registry: Arc::clone(self),
            id,
            tx,
            fingerprint,
            finished: false,
        })
    }
}

fn find_same(entries: &[Entry], new: &Subgoal, scope: &PathScope) -> Option<Admission> {
    entries
        .iter()
        .filter(|e| relate(new, &e.subgoal) == Relation::Same)
        .find_map(|e| reuse(e, scope))
}

/// The admission that answers a call from `entry`, if it can.
fn reuse(entry: &Entry, scope: &PathScope) -> Option<Admission> {
    match &entry.state {
        State::InFlight(rx) => Some(Admission::Join(rx.clone())),
        State::Done {
            outcome,
            fingerprint: Some(recorded),
        } if Fingerprint::of(&entry.subgoal, scope).as_ref() == Some(recorded) => {
            Some(Admission::Reuse(outcome.clone()))
        }
        State::Done { .. } => None,
    }
}

/// The registered subgoal that is the best near match for `new`, with its goal.
fn best_near(entries: &[Entry], new: &Subgoal) -> Option<(u64, String)> {
    entries
        .iter()
        .filter(|e| relate(new, &e.subgoal) == Relation::Near)
        .max_by(|a, b| {
            word_share(&new.goal, &a.subgoal.goal)
                .total_cmp(&word_share(&new.goal, &b.subgoal.goal))
        })
        .map(|e| (e.id, e.subgoal.goal.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(prompt: &str, paths: &[&str], access: Access) -> Subgoal {
        let paths: Vec<String> = paths.iter().map(ToString::to_string).collect();
        Subgoal::new(&SubgoalSpec {
            mode: "research",
            prompt,
            deliverable: None,
            paths: &paths,
            evidence: &[],
            access,
        })
    }

    fn read(prompt: &str, paths: &[&str]) -> Subgoal {
        goal(prompt, paths, Access::ReadOnly)
    }

    #[test]
    fn wording_that_normalizes_the_same_is_the_same_goal() {
        assert_eq!(
            normalize("  Find the CALLERS of  run()!\n"),
            normalize("find the callers of run()")
        );
        assert_ne!(normalize("find callers"), normalize("find callees"));
        assert_eq!(
            read("Find callers.", &["./src/a.rs"]).digest(),
            read("find  callers", &["src/a.rs"]).digest()
        );
    }

    #[test]
    fn an_exact_match_over_contained_scope_is_the_same() {
        let wide = read("find callers", &["src/**/*.rs", "Cargo.toml"]);
        assert_eq!(
            relate(&read("find callers", &["src/a/b.rs"]), &wide),
            Relation::Same
        );
        assert_eq!(relate(&wide, &wide), Relation::Same);
        assert_eq!(
            relate(&read("find callers", &[]), &read("find callers", &[])),
            Relation::Same
        );
    }

    #[test]
    fn a_wider_or_unknown_scope_is_never_inside_a_narrower_one() {
        let narrow = read("find callers", &["src/a.rs"]);
        assert_eq!(
            relate(&read("find callers", &["src/a.rs", "src/b.rs"]), &narrow),
            Relation::Distinct
        );
        assert_eq!(
            relate(&read("find callers", &[]), &narrow),
            Relation::Distinct
        );
        assert_eq!(
            relate(&narrow, &read("find callers", &[])),
            Relation::Distinct
        );
        assert_eq!(
            relate(&read("find callers", &["src/c.rs"]), &narrow),
            Relation::Distinct
        );
    }

    #[test]
    fn evidence_must_be_contained_too() {
        let id = |n: u8| ChunkId::parse(&format!("c-{n:016x}")).unwrap();
        let with = |ids: &[ChunkId]| {
            Subgoal::new(&SubgoalSpec {
                mode: "research",
                prompt: "p",
                deliverable: None,
                paths: &[],
                evidence: ids,
                access: Access::ReadOnly,
            })
        };
        assert_eq!(
            relate(&with(&[id(1)]), &with(&[id(1), id(2)])),
            Relation::Same
        );
        assert_eq!(relate(&with(&[id(3)]), &with(&[id(1)])), Relation::Distinct);
        assert_eq!(relate(&with(&[]), &with(&[id(1)])), Relation::Distinct);
    }

    #[test]
    fn a_goal_that_shares_most_words_is_a_near_match_and_one_that_does_not_is_distinct() {
        let a = read("find every caller of the run function", &["src/a.rs"]);
        let b = read("find all callers of the run function", &["src/a.rs"]);
        assert_eq!(relate(&b, &a), Relation::Near);
        let c = read("summarize the history of the project", &["src/a.rs"]);
        assert_eq!(relate(&c, &a), Relation::Distinct);
    }

    #[test]
    fn a_different_mode_or_deliverable_is_not_the_same_goal() {
        let a = read("find callers", &[]);
        let mut plan = a.clone();
        plan.mode = "plan".into();
        assert_eq!(relate(&a, &plan), Relation::Distinct);
        let spec = |d| {
            Subgoal::new(&SubgoalSpec {
                mode: "research",
                prompt: "find callers",
                deliverable: Some(d),
                paths: &[],
                evidence: &[],
                access: Access::ReadOnly,
            })
        };
        assert_ne!(relate(&spec("a list"), &spec("a table")), Relation::Same);
    }

    #[test]
    fn a_writing_subgoal_is_never_the_same_or_near_to_anything() {
        let write = goal("fix the parser", &["src/p.rs"], Access::Write);
        for other in [
            goal("fix the parser", &["src/p.rs"], Access::Write),
            goal("fix the parser", &["src/p.rs"], Access::ReadOnly),
            goal("fix the parser now", &["src/p.rs"], Access::Write),
        ] {
            assert_eq!(relate(&write, &other), Relation::Distinct);
            assert_eq!(relate(&other, &write), Relation::Distinct);
        }
    }

    #[test]
    fn declared_paths_are_validated() {
        use serde_json::json;
        assert_eq!(parse_paths(&json!({})).unwrap(), Vec::<String>::new());
        assert_eq!(
            parse_paths(&json!({"paths": ["a", " b "]})).unwrap(),
            ["a", "b"]
        );
        for bad in [
            json!({"paths": "a"}),
            json!({"paths": [1]}),
            json!({"paths": [""]}),
        ] {
            assert!(parse_paths(&bad).is_err(), "{bad}");
        }
        let many: Vec<String> = (0..=MAX_PATHS).map(|n| n.to_string()).collect();
        assert!(parse_paths(&json!({"paths": many})).is_err());
    }

    // ── The registry ─────────────────────────────────────────────────────────

    fn scope(dir: &std::path::Path) -> PathScope {
        PathScope::confined(dir).unwrap()
    }

    fn outcome(handle: &str) -> Outcome {
        Outcome {
            handle: handle.into(),
            text: format!("result of {handle}"),
        }
    }

    struct Says(Option<bool>);

    #[async_trait]
    impl NearMatch for Says {
        async fn same_goal(&self, _: &str, _: &str) -> Option<bool> {
            self.0
        }
    }

    fn launch(a: Admission) -> Leader {
        match a {
            Admission::Launch(l) => l,
            _ => panic!("expected a launch"),
        }
    }

    #[tokio::test]
    async fn an_identical_read_only_subgoal_in_flight_joins_and_gets_the_result() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(SubgoalRegistry::default());
        let leader = launch(
            reg.admit(read("find callers", &[]), &scope(dir.path()), None)
                .await,
        );
        let Admission::Join(mut rx) = reg
            .admit(read("Find callers.", &[]), &scope(dir.path()), None)
            .await
        else {
            panic!("the twin must join");
        };
        leader.finish(Some(outcome("buf_1")));
        let done = rx.wait_for(Option::is_some).await.unwrap().clone().unwrap();
        assert_eq!(done.handle, "buf_1");
    }

    #[tokio::test]
    async fn a_twin_of_a_failed_or_cancelled_child_launches_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(SubgoalRegistry::default());
        let leader = launch(
            reg.admit(read("find callers", &[]), &scope(dir.path()), None)
                .await,
        );
        let Admission::Join(mut rx) = reg
            .admit(read("find callers", &[]), &scope(dir.path()), None)
            .await
        else {
            panic!("the twin must join");
        };
        drop(leader);
        assert!(
            rx.wait_for(Option::is_some).await.is_err(),
            "the waiter is released with no result"
        );
        assert!(matches!(
            reg.admit(read("find callers", &[]), &scope(dir.path()), None)
                .await,
            Admission::Launch(_)
        ));
    }

    #[tokio::test]
    async fn a_finished_result_is_reused_only_while_the_declared_files_are_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.rs");
        std::fs::write(&file, "fn run() {}").unwrap();
        let s = scope(dir.path());
        let reg = Arc::new(SubgoalRegistry::default());
        let leader = launch(reg.admit(read("find callers", &["a.rs"]), &s, None).await);
        leader.finish(Some(outcome("buf_1")));

        match reg.admit(read("find callers", &["a.rs"]), &s, None).await {
            Admission::Reuse(o) => assert_eq!(o.handle, "buf_1"),
            _ => panic!("unchanged files: the result is reused"),
        }
        std::fs::write(&file, "fn run() { changed(); }").unwrap();
        assert!(
            matches!(
                reg.admit(read("find callers", &["a.rs"]), &s, None).await,
                Admission::Launch(_)
            ),
            "a changed file: the result is stale and the subgoal launches"
        );
    }

    #[tokio::test]
    async fn a_finished_result_without_literal_files_is_never_reused() {
        let dir = tempfile::tempdir().unwrap();
        let s = scope(dir.path());
        let reg = Arc::new(SubgoalRegistry::default());
        for paths in [&[][..], &["src/*.rs"][..]] {
            let leader = launch(reg.admit(read("find callers", paths), &s, None).await);
            leader.finish(Some(outcome("buf_1")));
            assert!(
                matches!(
                    reg.admit(read("find callers", paths), &s, None).await,
                    Admission::Launch(_)
                ),
                "{paths:?}"
            );
        }
    }

    #[tokio::test]
    async fn two_writing_subgoals_with_the_same_scope_both_launch() {
        let dir = tempfile::tempdir().unwrap();
        let s = scope(dir.path());
        let reg = Arc::new(SubgoalRegistry::default());
        let write = || goal("fix the parser", &["src/p.rs"], Access::Write);
        let first = launch(reg.admit(write(), &s, None).await);
        let advisor = Says(Some(true));
        let second = launch(reg.admit(write(), &s, Some(&advisor)).await);
        drop((first, second));
    }

    #[tokio::test]
    async fn only_a_near_match_asks_the_advisor_and_only_a_yes_joins() {
        let dir = tempfile::tempdir().unwrap();
        let s = scope(dir.path());
        let reg = Arc::new(SubgoalRegistry::default());
        let _leader = launch(
            reg.admit(
                read("find every caller of the run function", &["a.rs"]),
                &s,
                None,
            )
            .await,
        );
        let near = || read("find all callers of the run function", &["a.rs"]);

        for verdict in [None, Some(false)] {
            let advisor = Says(verdict);
            assert!(
                matches!(
                    reg.admit(near(), &s, Some(&advisor)).await,
                    Admission::Launch(_)
                ),
                "{verdict:?}"
            );
        }
        assert!(
            matches!(reg.admit(near(), &s, None).await, Admission::Launch(_)),
            "no advisor: launch"
        );
        let yes = Says(Some(true));
        assert!(matches!(
            reg.admit(near(), &s, Some(&yes)).await,
            Admission::Join(_)
        ));
        // A distinct goal never reaches the advisor, whatever it would say.
        assert!(matches!(
            reg.admit(read("summarize the history", &["a.rs"]), &s, Some(&yes))
                .await,
            Admission::Launch(_)
        ));
    }
}
