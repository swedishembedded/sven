// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The private directory a harness run owns.
//!
//! The run's gates, its audit log and its trajectory file live here, outside
//! the workspace the agent edits, so nothing the run needs to keep leaves a
//! mark in the repository and nothing the agent can write decides what a
//! check reads. Where it is: the host's choice, else the one the checkpoint
//! recorded (a resumed run), else the one the document names, else a fresh
//! directory of the run's own under the system temporary directory. It may
//! not lie inside the workspace, and it may not contain it.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in keeping the files of an
//! automated run out of the repository it works on then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::{Component, Path, PathBuf};

use sven_harness_runtime::RunValues;

use crate::error::CallError;

/// The file the run's trajectory is written to, in the scratchpad.
const TRAJECTORY_FILE: &str = "trajectory.json";

/// The file the run's audit log is written to, in the scratchpad.
const AUDIT_FILE: &str = "audit.jsonl";

/// Where the choices for a run's scratchpad come from, strongest first.
pub(super) struct Choices<'a> {
    /// The directory the host named for this run.
    pub host: Option<&'a Path>,
    /// The directory the checkpoint recorded.
    pub recorded: Option<&'a str>,
    /// The directory the document names, rendered over the inputs.
    pub document: Option<String>,
    /// The run's id, naming the default directory.
    pub run_id: &'a str,
    /// The workspace the scratchpad must stay clear of.
    pub workspace: Option<&'a Path>,
}

/// A run's scratchpad: created, and checked clear of the workspace.
pub(super) struct Scratchpad {
    dir: PathBuf,
}

impl Scratchpad {
    /// Settles where the scratchpad is and creates it (mode 0700).
    pub(super) fn resolve(choices: &Choices<'_>) -> Result<Self, CallError> {
        let refuse = |why: String| CallError::Precondition(format!("scratchpad: {why}"));
        let chosen = match (choices.host, choices.recorded, &choices.document) {
            (Some(dir), _, _) => dir.to_path_buf(),
            (None, Some(dir), _) => PathBuf::from(dir),
            (None, None, Some(dir)) => PathBuf::from(dir),
            (None, None, None) => {
                std::env::temp_dir().join(format!("sven-harness-{}", choices.run_id))
            }
        };
        if !chosen.is_absolute() {
            return Err(refuse(format!(
                "{} must be an absolute path",
                chosen.display()
            )));
        }
        if chosen.components().any(|c| c == Component::ParentDir) {
            return Err(refuse(format!("{} may not contain `..`", chosen.display())));
        }
        let dir = through_symlinks(&chosen);
        if let Some(workspace) = choices.workspace.and_then(|w| w.canonicalize().ok()) {
            if dir.starts_with(&workspace) {
                return Err(refuse(format!(
                    "{} is inside the workspace {}, where the agent can write",
                    dir.display(),
                    workspace.display()
                )));
            }
            if workspace.starts_with(&dir) {
                return Err(refuse(format!(
                    "{} contains the workspace {}",
                    dir.display(),
                    workspace.display()
                )));
            }
        }
        create_private(&dir)
            .map_err(|e| refuse(format!("cannot create {}: {e}", dir.display())))?;
        Ok(Self { dir })
    }

    /// The file the run's audit log goes to.
    pub(super) fn audit_log(&self) -> PathBuf {
        self.dir.join(AUDIT_FILE)
    }

    /// The file the run's trajectory is written to.
    pub(super) fn trajectory_file(&self) -> PathBuf {
        self.dir.join(TRAJECTORY_FILE)
    }

    /// What the host provides the run, for a plan that does or does not read
    /// its trajectory.
    pub(super) fn run_values(&self, reads_trajectory: bool) -> RunValues {
        RunValues {
            scratchpad: Some(self.dir.to_string_lossy().into_owned()),
            trajectory_file: reads_trajectory
                .then(|| self.trajectory_file().to_string_lossy().into_owned()),
        }
    }
}

/// `path` with the symbolic links of the part that exists resolved, and the
/// rest (not yet created) appended as written. Comparing against the
/// workspace by this path cannot be dodged with a link into it.
fn through_symlinks(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(real) = existing.canonicalize() {
            return missing.iter().rev().fold(real, |dir, part| dir.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_os_string());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Creates `dir` (and its missing parents) readable by its owner only.
fn create_private(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    if dir.is_dir() {
        Ok(())
    } else {
        Err(std::io::Error::other("it exists and is not a directory"))
    }
}
