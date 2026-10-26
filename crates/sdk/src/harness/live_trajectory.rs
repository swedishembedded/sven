// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The trajectory a gate can read while the run goes.
//!
//! A gate that reads `{{ run.trajectory_file }}` judges what the agent did:
//! which files it read before editing, which commands it ran, how many calls it
//! made. The file is the run's ATIF trajectory so far, written by the host
//! just before the gate's command starts, in the run's scratchpad (not the
//! workspace, which the agent can write). After the run the file is removed,
//! unless the host asked to keep it
//! ([`super::run::HarnessRun::keep_trajectory_file`]); then it holds the final
//! trajectory.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in checks that judge what an
//! agent did and not only what it left behind then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use sven_bootstrap::child_records::ChildRecords;
use sven_bootstrap::RuntimeHandle;
use sven_harness_runtime::HarnessRunState;
use sven_harness_spec::HarnessPlan;
use sven_hsm::Snapshot;
use sven_tools_exec::TrajectoryHook;

use super::run::HarnessOutcome;
use super::trajectory::{build_trajectory, Evidence};
use crate::engine::Engine;
use crate::run::{RunConclusion, Usage};

/// Removes the run's trajectory file when dropped, unless the host asked to
/// keep it: the file is for the gates of the run, and a host that runs
/// thousands of harnesses must not collect one for each. Dropping covers every
/// way the run can end: a verdict, a bound, a cancellation, an error and a
/// panic.
pub(super) struct Cleanup {
    file: Option<PathBuf>,
}

impl Cleanup {
    /// The cleanup of the trajectory `file`; nothing is removed when `keep`.
    pub(super) fn of(file: PathBuf, keep: bool) -> Self {
        Self {
            file: (!keep).then_some(file),
        }
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(file) = &self.file {
            let _ = std::fs::remove_file(file);
            let _ = std::fs::remove_file(partial(file));
        }
    }
}

/// The private file a trajectory is written to before it is renamed into place.
fn partial(file: &Path) -> PathBuf {
    file.with_extension("json.partial")
}

/// The slot the gate tool's hook reads, filled once the run's session exists.
pub(super) type Slot = Arc<OnceLock<Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>>>;

/// The hook a gate tool is given: it writes through whatever the slot holds.
pub(super) fn hook(slot: &Slot) -> TrajectoryHook {
    let slot = Arc::clone(slot);
    Arc::new(move |path| match slot.get() {
        Some(write) => write(path),
        None => Err("the run has not started".to_string()),
    })
}

/// Everything the writer reads about the run, captured when its session starts.
pub(super) struct Source {
    pub engine: Engine,
    pub plan: Arc<HarnessPlan>,
    pub run_id: String,
    pub bundle_digest: Option<String>,
    pub handle: RuntimeHandle,
    pub children: ChildRecords,
    pub tap: tokio::sync::watch::Receiver<Option<Arc<Snapshot>>>,
}

impl Source {
    /// What the run has done so far: the trace from the latest state the
    /// machine showed, the conversations from the live stores.
    fn evidence(&self) -> Evidence {
        let state = self
            .tap
            .borrow()
            .as_ref()
            .map(|snapshot| HarnessRunState::load(&snapshot.context))
            .unwrap_or_default();
        let so_far = HarnessOutcome {
            conclusion: RunConclusion::Success,
            result: None,
            usage: Usage::default(),
            trace: state.trace.clone(),
            steps: state.counts.steps,
        };
        Evidence::capture(
            &self.engine,
            &self.plan,
            &self.handle,
            &so_far,
            (self.bundle_digest.as_deref(), &state.steps),
            self.children.finished(),
        )
    }

    /// The trajectory so far, as JSON text.
    fn render(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&build_trajectory(
            &self.evidence(),
            &self.plan,
            &self.run_id,
        ))
        .map_err(|e| e.to_string())
    }

    /// Writes the trajectory so far to `path`, replacing it whole.
    pub(super) fn write(&self, path: &Path) -> Result<(), String> {
        write_atomically(path, &self.render()?)
    }
}

/// A conversation of the run, handed to the host when an item finishes.
#[derive(Debug, Clone)]
pub struct ConversationUpdate {
    /// Where the conversation sits in the order the conversations began;
    /// stable for the whole run.
    pub index: usize,
    /// The conversation's name (`<task>#<item index>` inside a `for_each`).
    pub name: String,
    /// The conversation so far, as a trajectory of its own.
    pub trajectory: atif::Trajectory,
}

/// Receives the conversations that changed since the host was last told.
pub(super) type ConversationSink = Box<dyn Fn(Vec<ConversationUpdate>) + Send + Sync>;

impl super::HarnessRun {
    /// Hands `sink` the conversations that changed each time a `for_each`
    /// item finishes (completed or failed), while the run goes: a run that is
    /// stopped keeps the transcripts of the items it finished. Each call
    /// carries only the conversations that gained messages since the last
    /// one, with their position in the order the conversations began.
    #[must_use]
    pub fn on_item_conversations(
        mut self,
        sink: impl Fn(Vec<ConversationUpdate>) + Send + Sync + 'static,
    ) -> Self {
        self.item_conversations = Some(Box::new(sink));
        self
    }
}

/// Tells the host of the changed conversations when `finished_loops` says an
/// item has just finished.
pub(super) fn hand_over_finished(handoff: Option<&mut Handoff>, finished_loops: &[String]) {
    if let (Some(handoff), false) = (handoff, finished_loops.is_empty()) {
        handoff.hand_over();
    }
}

/// Hands the host the conversations that changed each time an item finishes.
pub(super) struct Handoff {
    sink: ConversationSink,
    source: Arc<Source>,
    /// What the host has seen of each conversation, by name.
    delivered: std::collections::HashMap<String, (usize, usize)>,
}

impl Handoff {
    pub(super) fn new(sink: ConversationSink, source: Arc<Source>) -> Self {
        Self {
            sink,
            source,
            delivered: std::collections::HashMap::new(),
        }
    }

    /// Tells the host of every conversation that changed since it was last
    /// told.
    pub(super) fn hand_over(&mut self) {
        let updates: Vec<ConversationUpdate> = self
            .source
            .evidence()
            .changed_conversations(&self.source.run_id, &mut self.delivered)
            .into_iter()
            .map(|(index, name, trajectory)| ConversationUpdate {
                index,
                name,
                trajectory,
            })
            .collect();
        if !updates.is_empty() {
            (self.sink)(updates);
        }
    }
}

/// Writes `text` to `path` through a private file renamed into place, so a
/// reader never sees half of it.
pub(super) fn write_atomically(path: &Path, text: &str) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let partial = partial(path);
    std::fs::write(&partial, text).map_err(|e| format!("{}: {e}", partial.display()))?;
    std::fs::rename(&partial, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes the final `trajectory` to `file`, for a host that keeps it; a
/// failure is a warning, the run's verdict stands.
pub(super) fn write_final(file: &Path, trajectory: Option<atif::Trajectory>) {
    let Some(trajectory) = trajectory else {
        return;
    };
    let written = serde_json::to_string_pretty(&trajectory)
        .map_err(|e| e.to_string())
        .and_then(|text| write_atomically(file, &text));
    if let Err(e) = written {
        eprintln!("warning: cannot write the final trajectory: {e}");
    }
}
