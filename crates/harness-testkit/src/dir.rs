// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A harness directory and its jail.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::{Component, Path, PathBuf};

use crate::error::TestkitError;

/// The files of one harness: its main document, children and cases.
#[derive(Debug, Clone)]
pub struct HarnessDir {
    /// The directory every fixture path is confined to (a file's parent).
    pub root: PathBuf,
    /// `harness.yaml` or `bundle.yaml`, or the file that was opened.
    pub main: PathBuf,
    /// `children/*.yaml`, sorted.
    pub children: Vec<PathBuf>,
    /// `cases/*.yaml`, sorted.
    pub cases: Vec<PathBuf>,
}

impl HarnessDir {
    /// Opens a harness: a file is a main document alone; a directory holds
    /// exactly one of `harness.yaml` or `bundle.yaml`, optional
    /// `children/*.yaml` and `cases/*.yaml`.
    ///
    /// # Errors
    ///
    /// [`TestkitError::Io`] for an unreadable path, [`TestkitError::Layout`]
    /// for a directory without exactly one main document.
    pub fn open(path: &Path) -> Result<Self, TestkitError> {
        let meta = std::fs::metadata(path).map_err(|source| io_error(path, source))?;
        if meta.is_file() {
            let root = path
                .parent()
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
            return Ok(Self {
                root,
                main: path.to_path_buf(),
                children: Vec::new(),
                cases: Vec::new(),
            });
        }
        let found: Vec<PathBuf> = MAIN_NAMES
            .iter()
            .map(|name| path.join(name))
            .filter(|candidate| candidate.is_file())
            .collect();
        let [main] = found.as_slice() else {
            return Err(TestkitError::Layout(format!(
                "{} must hold exactly one of {} (found {})",
                path.display(),
                MAIN_NAMES.join(" or "),
                found.len()
            )));
        };
        Ok(Self {
            root: path.to_path_buf(),
            main: main.clone(),
            children: yaml_files(&path.join("children"))?,
            cases: yaml_files(&path.join("cases"))?,
        })
    }

    /// Replaces the cases with those of another directory (`compare` runs
    /// one case set over two variants).
    ///
    /// # Errors
    ///
    /// [`TestkitError::Io`] when `dir` cannot be listed.
    pub fn with_cases_from(mut self, dir: &Path) -> Result<Self, TestkitError> {
        self.cases = yaml_files(dir)?;
        Ok(self)
    }

    /// Resolves a manifest-relative path to an existing file inside the
    /// directory.
    ///
    /// # Errors
    ///
    /// [`TestkitError::Jail`] for an absolute path, a `..` component, or a
    /// path whose canonical form (symlinks followed) is outside the root.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, TestkitError> {
        let path = jail_join(&self.root, rel)?;
        let root = canonical(&self.root)?;
        let real = canonical(&path)?;
        if !real.starts_with(&root) {
            return Err(outside(rel));
        }
        Ok(real)
    }
}

const MAIN_NAMES: [&str; 2] = ["harness.yaml", "bundle.yaml"];

fn io_error(path: &Path, source: std::io::Error) -> TestkitError {
    TestkitError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn canonical(path: &Path) -> Result<PathBuf, TestkitError> {
    path.canonicalize().map_err(|source| io_error(path, source))
}

fn outside(rel: &str) -> TestkitError {
    TestkitError::Jail(format!(
        "`{rel}` is outside the directory it is confined to"
    ))
}

/// The `*.yaml` files directly inside `dir`, sorted; none when the
/// directory does not exist.
pub(crate) fn yaml_files(dir: &Path) -> Result<Vec<PathBuf>, TestkitError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(io_error(dir, source)),
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "yaml"))
        .collect();
    files.sort();
    Ok(files)
}

/// Joins `rel` under `root` for a file that may not exist yet, refusing
/// anything that would land outside `root`.
pub(crate) fn jail_join(root: &Path, rel: &str) -> Result<PathBuf, TestkitError> {
    let relative = Path::new(rel);
    if relative.is_absolute() || rel.is_empty() {
        return Err(TestkitError::Jail(format!(
            "`{rel}` must be a relative path inside the directory"
        )));
    }
    for component in relative.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => return Err(outside(rel)),
        }
    }
    let joined = root.join(relative);
    // A symlink inside the root can still lead out: the deepest existing
    // ancestor, with links followed, must remain under the canonical root.
    let mut ancestor = joined.as_path();
    while !ancestor.exists() {
        match ancestor.parent() {
            Some(parent) => ancestor = parent,
            None => return Err(outside(rel)),
        }
    }
    if !canonical(ancestor)?.starts_with(canonical(root)?) {
        return Err(outside(rel));
    }
    Ok(joined)
}
