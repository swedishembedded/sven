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
use sven_model::Message;
use sven_turn::assembly::ContextPolicy;
use sven_turn::reconstruct::{reconstruct, ReconstructConfig};
use tracing::warn;
use uuid::Uuid;

use crate::config::{AssemblyConfig, Config};
use crate::evidence_share::is_store_id;

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
    /// The wiring `config` asks for, reopening the store `snapshot` recorded.
    /// The store's id is recorded among `facts`, so a snapshot carries it.
    pub(crate) fn for_session(
        config: &Config,
        project_root: Option<&Path>,
        snapshot: Option<&sven_hsm::Snapshot>,
        facts: &mut serde_json::Map<String, serde_json::Value>,
    ) -> Option<Self> {
        let wiring = Self::open(
            &config.context.assembly,
            project_root,
            resumed_store_id(snapshot).as_deref(),
        )?;
        facts.insert(
            CONTEXT_STORE_FACT.into(),
            serde_json::json!(wiring.store_id),
        );
        Some(wiring)
    }

    /// The chunk session the tools and executors share.
    pub(crate) fn session(&self) -> Arc<ChunkSession> {
        Arc::clone(&self.session)
    }

    /// The history a session is seeded with. A resumed session in
    /// `reconstruct` mode starts from the rebuilt history; if the evidence its
    /// recap refers to cannot be stored the transcript is replayed whole,
    /// because a reference that cannot be expanded loses the evidence.
    pub(crate) fn seed(
        wiring: &Option<Self>,
        config: &Config,
        initial: &[Message],
    ) -> Vec<Message> {
        let Some(wiring) = wiring
            .as_ref()
            .filter(|_| config.context.assembly.reconstructs())
        else {
            return initial.to_vec();
        };
        let rebuilt = reconstruct(initial, &ReconstructConfig::default());
        if rebuilt
            .evidence
            .iter()
            .all(|e| wiring.session.store_evidence(e))
        {
            rebuilt.messages
        } else {
            warn!("the history of the resumed session is replayed whole: its evidence could not be stored");
            initial.to_vec()
        }
    }

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
fn resumed_store_id(snapshot: Option<&sven_hsm::Snapshot>) -> Option<String> {
    snapshot?
        .context
        .facts
        .get(CONTEXT_STORE_FACT)?
        .as_str()
        .map(str::to_string)
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

    fn long_history() -> Vec<Message> {
        use sven_model::{FunctionCall, MessageContent, Role};
        let bulk = (1..=300)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        vec![
            Message::user("Constraint: do not touch vendor/."),
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "a".into(),
                    function: FunctionCall {
                        name: "read_file".into(),
                        arguments: r#"{"path":"src/lib.rs"}"#.into(),
                    },
                },
                reasoning: None,
            },
            Message::tool_result("a", bulk),
            Message::assistant("done reading"),
        ]
    }

    #[test]
    fn only_reconstruct_rebuilds_the_history_of_a_resumed_session() {
        let root = tempfile::tempdir().unwrap();
        let history = long_history();
        let mut config = Config::default();
        for (mode, rebuilt) in [
            (AssemblyMode::Off, false),
            (AssemblyMode::Demote, false),
            (AssemblyMode::Reconstruct, true),
        ] {
            config.context.assembly.mode = mode;
            let wiring = ContextWiring::open(&config.context.assembly, Some(root.path()), None);
            let seeded = ContextWiring::seed(&wiring, &config, &history);
            assert_eq!(
                seeded.len() != history.len() || seeded[1].as_text().is_some(),
                rebuilt,
                "{mode:?}"
            );
            if !rebuilt {
                assert_eq!(seeded.len(), history.len());
            }
        }
    }

    #[test]
    fn a_rebuilt_history_leaves_its_evidence_in_the_store() {
        let root = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.context.assembly.mode = AssemblyMode::Reconstruct;
        let wiring = ContextWiring::open(&config.context.assembly, Some(root.path()), None);
        let seeded = ContextWiring::seed(&wiring, &config, &long_history());
        assert!(seeded.iter().any(|m| m
            .as_text()
            .is_some_and(|t| t.starts_with(sven_turn::reconstruct::RECAP_MARKER))));
        assert_eq!(wiring.unwrap().session.store().lock().unwrap().len(), 1);
    }

    #[test]
    fn a_store_that_cannot_be_opened_leaves_the_session_running_without_assembly() {
        let root = tempfile::tempdir().unwrap();
        // `.sven` is a file, so no directory can be created under it.
        std::fs::write(root.path().join(".sven"), "x").unwrap();
        assert!(ContextWiring::open(&on(), Some(root.path()), None).is_none());
    }
}
