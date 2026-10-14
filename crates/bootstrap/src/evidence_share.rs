// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping delegated agent work
// cheap and non-duplicative for its clients. If your team needs expertise in
// multi-agent context handoff then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The parent's chunk store, as a sub-agent sees it.
//!
//! A `task` sub-agent that was briefed with chunks expands them from the
//! parent's store. It opens that store read-only, as it was when the child
//! started: a snapshot it can read and never change. The parent names the
//! store with `sven acp serve --evidence-store DIR`; the model never does, and
//! the model's own arguments carry only chunk ids.
//!
//! `DIR` is accepted only in the shape [`ContextWiring`](crate::context_wiring)
//! creates, `<root>/.sven/context/<uuid>`: an absolute path without `..`
//! whose last component is the hyphenated UUID of a session's store. Anything
//! else is refused before anything is opened, so the flag cannot point a
//! child at an arbitrary directory. The project root the chunks' file paths
//! are relative to is the `<root>` of that shape.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use sven_chunks::{ChunkStore, FsProbe, SharedChunkStore, SourceProbe, StoreLimits};
use uuid::Uuid;

/// The directory name of a store: a hyphenated UUID, as the session
/// generated it.
pub(crate) fn is_store_id(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|u| u.hyphenated().to_string() == id)
}

/// The store directory `text` names, if it has the shape of a session's
/// store. See the module documentation.
///
/// # Errors
///
/// Says what is wrong with `text`.
pub fn evidence_store_path(text: &str) -> Result<PathBuf, String> {
    let (path, _root) = split(Path::new(text))?;
    Ok(path)
}

/// `dir` and the project root it belongs to.
fn split(dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    let refuse = |why: &str| {
        Err(format!(
            "'{}' is not a session chunk store: {why}",
            dir.display()
        ))
    };
    if !dir.is_absolute() {
        return refuse("the path must be absolute");
    }
    if dir
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return refuse("the path must not contain '.' or '..'");
    }
    let name = |p: &Path| p.file_name().and_then(|n| n.to_str()).map(str::to_string);
    let shaped = dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_store_id)
        && dir.parent().and_then(name).as_deref() == Some("context")
        && dir
            .parent()
            .and_then(Path::parent)
            .and_then(name)
            .as_deref()
            == Some(".sven");
    match (shaped, dir.ancestors().nth(3)) {
        (true, Some(root)) => Ok((dir.to_path_buf(), root.to_path_buf())),
        _ => refuse("expected <root>/.sven/context/<session id>"),
    }
}

/// A parent's store opened for reading, with the probe that tells whether the
/// files its chunks were read from have changed since.
#[derive(Clone)]
pub(crate) struct SharedEvidence {
    pub(crate) store: SharedChunkStore,
    pub(crate) probe: Arc<dyn SourceProbe + Send + Sync>,
}

impl SharedEvidence {
    /// Opens the store in `dir` read-only.
    pub(crate) fn open(dir: &Path) -> Result<Self, String> {
        let (dir, root) = split(dir)?;
        let store = ChunkStore::open_read_only(&dir, StoreLimits::default())
            .map_err(|e| format!("{e:#}"))?;
        let probe = FsProbe::new(&root).map_err(|e| e.to_string())?;
        Ok(Self {
            store: Arc::new(Mutex::new(store)),
            probe: Arc::new(probe),
        })
    }
}

#[cfg(test)]
mod tests {
    use sven_chunks::{ChunkDraft, ChunkKind, Lifetime, Provenance, Sensitivity};

    use super::*;

    const ID: &str = "0b9cdb5e-7d0e-4f0e-9d6e-0d3f6a1c2b4a";

    fn store_dir(root: &Path) -> PathBuf {
        root.join(".sven").join("context").join(ID)
    }

    #[test]
    fn only_the_shape_of_a_session_store_is_accepted() {
        let ok = format!("/work/.sven/context/{ID}");
        assert_eq!(evidence_store_path(&ok).unwrap(), PathBuf::from(&ok));
        for bad in [
            "relative/.sven/context/0b9cdb5e-7d0e-4f0e-9d6e-0d3f6a1c2b4a",
            "/work/.sven/context/../context/0b9cdb5e-7d0e-4f0e-9d6e-0d3f6a1c2b4a",
            "/work/.sven/context/not-a-uuid",
            "/work/.sven/other/0b9cdb5e-7d0e-4f0e-9d6e-0d3f6a1c2b4a",
            "/work/.git/context/0b9cdb5e-7d0e-4f0e-9d6e-0d3f6a1c2b4a",
            "/etc",
            "/",
            "",
        ] {
            assert!(evidence_store_path(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn a_session_store_is_opened_read_only_with_a_probe_at_its_project_root() {
        let root = tempfile::tempdir().unwrap();
        let dir = store_dir(root.path());
        let draft = ChunkDraft {
            kind: ChunkKind::File,
            provenance: Provenance {
                source_tool: "read_file".into(),
                args_digest: String::new(),
                path: Some("a.txt".into()),
                source_version: None,
                turn: 1,
            },
            priority: 0,
            sensitivity: Sensitivity::Internal,
            lifetime: Lifetime::Session,
            pinned: false,
            failure: false,
        };
        let id = ChunkStore::open(&dir, StoreLimits::default())
            .unwrap()
            .put("evidence", draft.clone())
            .unwrap()
            .chunk
            .id;

        let shared = SharedEvidence::open(&dir).unwrap();
        let mut store = shared.store.lock().unwrap();
        assert!(store.is_read_only());
        assert_eq!(store.read(&id).unwrap(), "evidence");
        assert!(store.put("more", draft).is_err());
        drop(store);
        std::fs::write(root.path().join("a.txt"), "x").unwrap();
        assert!(shared.probe.version("a.txt").unwrap().is_some());
    }

    #[test]
    fn a_store_that_does_not_exist_is_not_created() {
        let root = tempfile::tempdir().unwrap();
        let dir = store_dir(root.path());
        assert!(SharedEvidence::open(&dir).is_err());
        assert!(!dir.exists());
    }
}
