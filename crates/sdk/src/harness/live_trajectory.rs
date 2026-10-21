// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The trajectory a gate can read while the run goes.
//!
//! A gate that reads `{{ run.trajectory_file }}` judges what the agent did:
//! which files it read before editing, which commands it ran, how many calls it
//! made. The file is the run's ATIF trajectory so far, written by the host
//! just before the gate's command starts, in a directory of its own under the
//! host's temporary directory (not the workspace, which the agent can write).
//! After the run the file holds the final trajectory.
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

/// Where run `run_id`'s trajectory file is: a directory of its own under the
/// host's temporary directory.
pub(super) fn trajectory_path(run_id: &str) -> PathBuf {
    std::env::temp_dir()
        .join(format!("sven-harness-{run_id}"))
        .join("trajectory.json")
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
    /// The trajectory so far, as JSON text. The trace comes from the latest
    /// state the machine showed; the conversations from the live stores.
    fn render(&self) -> Result<String, String> {
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
        let evidence = Evidence::capture(
            &self.engine,
            &self.plan,
            &self.handle,
            &so_far,
            (self.bundle_digest.as_deref(), &state.steps),
            self.children.finished(),
        );
        serde_json::to_string_pretty(&build_trajectory(&evidence, &self.plan, &self.run_id))
            .map_err(|e| e.to_string())
    }

    /// Writes the trajectory so far to `path`, replacing it whole.
    pub(super) fn write(&self, path: &Path) -> Result<(), String> {
        write_atomically(path, &self.render()?)
    }
}

/// Writes `text` to `path` through a private file renamed into place, so a
/// reader never sees half of it.
pub(super) fn write_atomically(path: &Path, text: &str) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, text).map_err(|e| format!("{}: {e}", partial.display()))?;
    std::fs::rename(&partial, path).map_err(|e| format!("{}: {e}", path.display()))
}
