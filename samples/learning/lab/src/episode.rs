// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! One episode: a fresh workspace, a live world, an agent, and a verdict.
//!
//! The order here is the load-bearing part. The world is started before the
//! agent and stopped before the verifier runs, and the verifier reads the
//! filesystem the agent left behind rather than anything the agent said about
//! it. An agent's closing message is a claim; only the workspace is evidence.
//!
//! Each episode gets its own directory under a run root and is never reused.
//! That is not tidiness: an answer surviving into the next instance would
//! score as a second success, and the second instance would be measuring the
//! first one's leftovers.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::Family;

/// A materialised workspace with its world running beside it.
///
/// The hidden state is written to the world's stdin-side pipe and nowhere
/// else - see `tasks/README.md` for why a file, `argv` or the environment
/// would each defeat the point.
pub struct Episode {
    dir: PathBuf,
    workspace: PathBuf,
    socket: PathBuf,
    hidden_state: String,
    world: Option<Child>,
}

/// Why an episode could not be set up or run.
#[derive(Debug)]
pub enum EpisodeError {
    Io {
        what: String,
        source: std::io::Error,
    },
    World(String),
}

impl std::fmt::Display for EpisodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EpisodeError::Io { what, source } => write!(f, "{what}: {source}"),
            EpisodeError::World(message) => write!(f, "the world: {message}"),
        }
    }
}

impl std::error::Error for EpisodeError {}

fn io<T>(what: &str, r: std::io::Result<T>) -> Result<T, EpisodeError> {
    r.map_err(|source| EpisodeError::Io {
        what: what.to_string(),
        source,
    })
}

impl Episode {
    /// Materialise `family`'s workspace into `dir` and start its world with
    /// `hidden_state` live.
    pub fn start(family: &Family, dir: &Path, hidden_state: &str) -> Result<Episode, EpisodeError> {
        io(
            "creating the episode directory",
            std::fs::create_dir_all(dir),
        )?;
        let workspace = dir.join("workspace");
        copy_tree(&family.root().join("workspace"), &workspace)?;

        let socket = dir.join("service.sock");
        let world = Command::new("python3")
            .arg(family.root().join("world/service.py"))
            .arg(workspace.join("config"))
            .arg(&socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();
        let mut world = io("starting the world", world)?;

        // The hidden state travels down a pipe and is then closed, so it
        // exists only in the world's memory. Reading every byte on the machine
        // does not reveal it; asking the service does.
        {
            use std::io::Write;
            let stdin = world.stdin.as_mut().expect("stdin was piped");
            io(
                "handing the world its hidden state",
                writeln!(stdin, "{hidden_state}"),
            )?;
        }
        drop(world.stdin.take());

        let episode = Episode {
            dir: dir.to_path_buf(),
            workspace,
            socket,
            hidden_state: hidden_state.to_string(),
            world: Some(world),
        };
        episode.await_socket()?;
        Ok(episode)
    }

    fn await_socket(&self) -> Result<(), EpisodeError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            if self.socket.exists() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Err(EpisodeError::World(
            "did not create its socket within 15s".into(),
        ))
    }

    /// The directory the agent works in.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// The environment an agent needs to reach the world. Nothing here names
    /// the hidden state.
    pub fn agent_env(&self) -> Vec<(String, String)> {
        vec![
            (
                "UPLOAD_SERVICE_SOCKET".into(),
                self.socket.display().to_string(),
            ),
            // The built-in read tool resolves a missing absolute path by
            // dropping components, which can reach a different file than the
            // one asked for. Off, so a path that does not exist reads as a
            // path that does not exist.
            ("SVEN_NO_PATH_ASCENT".into(), "1".into()),
        ]
    }

    /// What was live for this episode. For the run record, written only after
    /// the episode ends.
    pub fn hidden_state(&self) -> &str {
        &self.hidden_state
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Stop the world. Called before verifying, so nothing the verifier reads
    /// can still be changing.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.world.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Episode {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Capture every deployment's effective configuration before the agent runs,
/// so "unchanged" is measured against what was actually there rather than
/// against what the template says.
pub fn baseline_effective(family: &Family, episode: &Episode) -> Result<String, EpisodeError> {
    let out = Command::new("python3")
        .arg("-c")
        .arg(
            "import json,sys; sys.path.insert(0, sys.argv[1]); \
             from verify import effective_all; \
             print(json.dumps(effective_all(__import__('pathlib').Path(sys.argv[2]), sys.argv[3])))",
        )
        .arg(family.root().join("world"))
        .arg(episode.workspace().join("config"))
        .arg(episode.hidden_state())
        .output();
    let out = io("capturing the baseline configuration", out)?;
    if !out.status.success() {
        return Err(EpisodeError::World(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run the family's verifier over the workspace the agent left behind.
///
/// Returns the predicate map. The verifier is a separate process reading the
/// filesystem: it is given no access to the agent, its messages, or its
/// trajectory, because a verifier that can see the agent's account of its work
/// is measuring the account.
pub fn run_verifier(
    family: &Family,
    episode: &Episode,
    baseline_json: &str,
) -> Result<std::collections::BTreeMap<String, bool>, EpisodeError> {
    let baseline_path = episode.dir().join("baseline.json");
    io(
        "writing the baseline",
        std::fs::write(&baseline_path, baseline_json),
    )?;

    let out = Command::new("python3")
        .arg(family.root().join("world/verify.py"))
        .arg("--config-dir")
        .arg(episode.workspace().join("config"))
        .arg("--active")
        .arg(episode.hidden_state())
        .arg("--baseline")
        .arg(&baseline_path)
        .output();
    let out = io("running the verifier", out)?;
    if !out.status.success() {
        return Err(EpisodeError::World(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ));
    }

    #[derive(serde::Deserialize)]
    struct Report {
        predicates: std::collections::BTreeMap<String, bool>,
    }
    let report: Report = serde_json::from_slice(&out.stdout)
        .map_err(|e| EpisodeError::World(format!("the verifier's report did not parse: {e}")))?;
    Ok(report.predicates)
}

/// Run the family's witness against a live episode.
///
/// Used by `audit` and as a control arm: a run where the witness fails is a
/// broken environment, not a hard task, and the two must never be confused.
pub fn run_witness(family: &Family, episode: &Episode) -> Result<u32, EpisodeError> {
    let mut command = Command::new("python3");
    command
        .arg(family.root().join("witness/solve.py"))
        .arg(episode.workspace());
    for (key, value) in episode.agent_env() {
        command.env(key, value);
    }
    let out = io("running the witness", command.output())?;
    if !out.status.success() {
        return Err(EpisodeError::World(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ));
    }
    #[derive(serde::Deserialize)]
    struct Record {
        tool_calls: u32,
    }
    let record: Record = serde_json::from_slice(&out.stdout)
        .map_err(|e| EpisodeError::World(format!("the witness's record did not parse: {e}")))?;
    Ok(record.tool_calls)
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), EpisodeError> {
    io(
        "creating a workspace directory",
        std::fs::create_dir_all(to),
    )?;
    for entry in io("reading the workspace template", std::fs::read_dir(from))? {
        let entry = io("reading a template entry", entry)?;
        let target = to.join(entry.file_name());
        let kind = io("inspecting a template entry", entry.file_type())?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            io(
                "copying a template file",
                std::fs::copy(entry.path(), &target),
            )
            .map(|_| ())?;
            // The workspace ships an executable CLI; a copy that lost the bit
            // would make every episode fail for a reason that has nothing to
            // do with the agent.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = io("reading a template mode", entry.metadata())?
                    .permissions()
                    .mode();
                io(
                    "restoring a template mode",
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode)),
                )?;
            }
        }
    }
    Ok(())
}
