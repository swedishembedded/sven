// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Opens the chunk store of a session that has addressable context on.
//!
//! The store lives beside the project's other session state, under
//! `.sven/context/<id>/`. The id is a session fact, so a snapshot restores it
//! and a resumed session finds the evidence its transcript refers to.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sven_chunks::{ChunkStore, FsProbe};
use sven_executors::{ChunkSession, ContextAssembler};
use sven_turn::assembly::ContextPolicy;
use tracing::warn;
use uuid::Uuid;

use crate::config::AssemblyConfig;

/// The session fact that names the chunk store directory.
pub(crate) const CONTEXT_STORE_FACT: &str = "context_store";

/// What a session with addressable context on is built with.
pub(crate) struct ContextWiring {
    /// Shared by the tool executor, which records results, and the tools that
    /// expand them.
    pub session: Arc<ChunkSession>,
    /// Builds the context view of each request.
    pub assembler: ContextAssembler,
    /// The store's directory name, recorded as a session fact.
    pub store_id: String,
}

impl ContextWiring {
    /// Open (or create) the store of the session, or `None` when assembly is
    /// off. A store that cannot be opened is logged and the session runs
    /// without assembly: an optimization never stops a session from starting.
    pub(crate) fn open(
        config: &AssemblyConfig,
        project_root: Option<&Path>,
        resumed: Option<&str>,
    ) -> Option<Self> {
        if !config.enabled() {
            return None;
        }
        let root = project_root.unwrap_or(Path::new("."));
        let store_id = resumed
            .filter(|id| is_store_id(id))
            .map_or_else(|| Uuid::new_v4().to_string(), str::to_string);
        let dir: PathBuf = root.join(".sven").join("context").join(&store_id);
        let opened = ChunkStore::open(&dir, config.store_limits())
            .map_err(|e| format!("{e:#}"))
            .and_then(|store| {
                FsProbe::new(root)
                    .map(|probe| (store, probe))
                    .map_err(|e| e.to_string())
            });
        match opened {
            Ok((store, probe)) => {
                let session = Arc::new(ChunkSession::new(
                    Arc::new(Mutex::new(store)),
                    Arc::new(probe),
                ));
                Some(Self {
                    assembler: ContextAssembler::new(
                        ContextPolicy::new(config.policy()),
                        Arc::clone(&session),
                    ),
                    session,
                    store_id,
                })
            }
            Err(e) => {
                warn!(error = %e, dir = %dir.display(), "addressable context is off: the chunk store could not be opened");
                None
            }
        }
    }
}

/// The store id a snapshot recorded, if it recorded one.
pub(crate) fn resumed_store_id(snapshot: Option<&sven_hsm::Snapshot>) -> Option<String> {
    snapshot?
        .context
        .facts
        .get(CONTEXT_STORE_FACT)?
        .as_str()
        .map(str::to_string)
}

/// Whether `id` is a name this module could have generated. A snapshot is
/// input: its id becomes a directory name, so anything but a UUID is ignored.
fn is_store_id(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|u| u.hyphenated().to_string() == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AssemblyMode;

    fn on() -> AssemblyConfig {
        AssemblyConfig {
            mode: AssemblyMode::Demote,
            ..AssemblyConfig::default()
        }
    }

    #[test]
    fn an_assembly_that_is_off_opens_nothing() {
        let root = tempfile::tempdir().unwrap();
        assert!(ContextWiring::open(&AssemblyConfig::default(), Some(root.path()), None).is_none());
        assert!(!root.path().join(".sven").exists());
    }

    #[test]
    fn the_store_is_created_under_the_project_and_named_by_its_id() {
        let root = tempfile::tempdir().unwrap();
        let wiring = ContextWiring::open(&on(), Some(root.path()), None).unwrap();
        assert!(root
            .path()
            .join(".sven/context")
            .join(&wiring.store_id)
            .join("payloads")
            .is_dir());
    }

    #[test]
    fn a_resumed_session_reopens_the_store_it_recorded() {
        let root = tempfile::tempdir().unwrap();
        let first = ContextWiring::open(&on(), Some(root.path()), None).unwrap();
        let again = ContextWiring::open(&on(), Some(root.path()), Some(&first.store_id)).unwrap();
        assert_eq!(first.store_id, again.store_id);
    }

    #[test]
    fn a_recorded_id_that_is_not_one_of_ours_is_never_used_as_a_path() {
        let root = tempfile::tempdir().unwrap();
        for bad in ["../../etc", "/abs", "", "a/b", "0123"] {
            let wiring = ContextWiring::open(&on(), Some(root.path()), Some(bad)).unwrap();
            assert_ne!(wiring.store_id, bad);
            assert!(Uuid::parse_str(&wiring.store_id).is_ok());
        }
    }

    #[test]
    fn a_store_that_cannot_be_opened_leaves_the_session_running_without_assembly() {
        let root = tempfile::tempdir().unwrap();
        // `.sven` is a file, so no directory can be created under it.
        std::fs::write(root.path().join(".sven"), "x").unwrap();
        assert!(ContextWiring::open(&on(), Some(root.path()), None).is_none());
    }
}
