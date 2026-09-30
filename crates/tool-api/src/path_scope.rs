// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The directory a tool resolves paths against, and is confined to.
//!
//! A [`PathScope`] is handed to a tool when it is constructed. Unconfined (the
//! default), a path means what it means to the process: relative paths
//! resolve against the working directory and nothing is refused. Confined to a
//! root, a relative path resolves against the root and any path that ends up
//! outside it is refused.
//!
//! # The confinement rule
//!
//! [`PathScope::resolve`] on a confined scope:
//!
//! 1. joins a relative path onto the root (an absolute path is taken as is);
//! 2. normalises it lexically: `.` is dropped and `..` removes the component
//!    before it, so `a/../../x` is `../x` relative to the root;
//! 3. follows symlinks through the longest prefix that exists, so a link
//!    inside the root that points outside it resolves to where it points;
//!    a path that does not exist yet is judged by its nearest existing
//!    ancestor, and a dangling symlink on the way is refused outright,
//!    because writing through it would create its target wherever that is;
//! 4. refuses the result unless it is the root or lies beneath it.
//!
//! The returned path is the resolved one, so a tool operates on exactly what
//! was checked rather than re-resolving the caller's string. The check is a
//! point-in-time one: a process that swaps a directory for a symlink between
//! the check and the use is not defended against. A root confines the tools
//! built with it; it is not a sandbox.

use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::tool::{ToolCall, ToolOutput};

/// Where a tool's paths resolve, and whether they may leave it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathScope {
    /// The canonical root, or `None` for the process working directory with
    /// no confinement.
    root: Option<Arc<PathBuf>>,
}

/// A path a [`PathScope`] refuses.
#[derive(Debug)]
pub enum PathScopeError {
    /// The path resolves outside the root.
    Outside {
        /// The path as the caller gave it.
        requested: PathBuf,
        /// The root it had to stay inside.
        root: PathBuf,
    },
    /// Some part of the path could not be resolved (a dangling symlink, a
    /// symlink loop, a directory that cannot be read), so where it leads is
    /// unknown.
    Unresolvable {
        /// The path as the caller gave it.
        requested: PathBuf,
        /// The root it had to stay inside.
        root: PathBuf,
        /// Why it could not be resolved.
        reason: String,
    },
}

impl fmt::Display for PathScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Outside { requested, root } => write!(
                f,
                "path '{}' is outside the project root {}",
                requested.display(),
                root.display()
            ),
            Self::Unresolvable {
                requested,
                root,
                reason,
            } => write!(
                f,
                "path '{}' cannot be resolved inside the project root {}: {reason}",
                requested.display(),
                root.display()
            ),
        }
    }
}

impl std::error::Error for PathScopeError {}

impl PathScope {
    /// Paths mean what they mean to the process; nothing is refused.
    #[must_use]
    pub fn unconfined() -> Self {
        Self::default()
    }

    /// Confines paths to `root`, which must be an existing directory.
    ///
    /// # Errors
    ///
    /// When `root` does not exist, cannot be resolved, or is not a directory.
    pub fn confined(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref();
        let canonical = std::fs::canonicalize(root).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("cannot resolve project root {}: {e}", root.display()),
            )
        })?;
        if !canonical.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("project root {} is not a directory", root.display()),
            ));
        }
        Ok(Self {
            root: Some(Arc::new(canonical)),
        })
    }

    /// The canonical root, or `None` when unconfined.
    #[must_use]
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref().map(PathBuf::as_path)
    }

    /// The path a tool should operate on for `requested`.
    ///
    /// Unconfined, `requested` is returned unchanged. Confined, see the
    /// module documentation for the rule.
    ///
    /// # Errors
    ///
    /// Only when confined: when `requested` resolves outside the root, or
    /// cannot be resolved at all.
    pub fn resolve(&self, requested: impl AsRef<Path>) -> Result<PathBuf, PathScopeError> {
        let requested = requested.as_ref();
        let Some(root) = self.root.as_deref() else {
            return Ok(requested.to_path_buf());
        };
        let unresolvable = |reason: String| PathScopeError::Unresolvable {
            requested: requested.to_path_buf(),
            root: root.clone(),
            reason,
        };

        let resolved =
            follow_existing_prefix(&normalise(&root.join(requested))).map_err(unresolvable)?;
        if resolved.starts_with(root) {
            Ok(resolved)
        } else {
            Err(PathScopeError::Outside {
                requested: requested.to_path_buf(),
                root: root.clone(),
            })
        }
    }

    /// [`Self::resolve`] for a tool answering `call`: a refusal becomes the
    /// error result the model reads.
    ///
    /// # Errors
    ///
    /// The error [`ToolOutput`] for `call` when [`Self::resolve`] refuses.
    pub fn resolve_for(
        &self,
        call: &ToolCall,
        requested: impl AsRef<Path>,
    ) -> Result<PathBuf, ToolOutput> {
        self.resolve(requested)
            .map_err(|e| ToolOutput::err(&call.id, e.to_string()))
    }
}

/// `path` with `.` dropped and each `..` removing the component before it.
/// A `..` above the filesystem root stays at the root, as the kernel treats it.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `path` with symlinks followed through its longest existing prefix; the
/// components that do not exist yet are appended unchanged.
///
/// `path` is absolute and already normalised, so the components after the
/// existing prefix contain no `..` for a symlink to reinterpret.
fn follow_existing_prefix(path: &Path) -> Result<PathBuf, String> {
    let mut existing = path;
    let mut missing: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        match std::fs::canonicalize(existing) {
            Ok(canonical) => {
                let mut resolved = canonical;
                resolved.extend(missing.iter().rev());
                return Ok(resolved);
            }
            Err(e) => {
                // Present but unresolvable: a dangling symlink, a loop, or a
                // permission problem. Its target is unknown, so it cannot be
                // judged - and it must not be skipped over as "missing".
                if std::fs::symlink_metadata(existing).is_ok() {
                    return Err(format!("{}: {e}", existing.display()));
                }
                let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
                    return Err(format!("{}: {e}", existing.display()));
                };
                missing.push(name);
                existing = parent;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(dir: &Path) -> PathScope {
        PathScope::confined(dir).expect("a temp dir is a valid root")
    }

    #[test]
    fn unconfined_returns_the_path_unchanged() {
        let s = PathScope::unconfined();
        assert_eq!(s.resolve("../x").unwrap(), PathBuf::from("../x"));
        assert_eq!(
            s.resolve("/etc/passwd").unwrap(),
            PathBuf::from("/etc/passwd")
        );
        assert_eq!(s.root(), None);
    }

    #[test]
    fn relative_and_absolute_paths_inside_the_root_resolve_under_it() {
        let tmp = tempfile::tempdir().unwrap();
        let s = scope(tmp.path());
        let root = s.root().unwrap().to_path_buf();
        std::fs::create_dir(root.join("src")).unwrap();

        assert_eq!(s.resolve("src/new.rs").unwrap(), root.join("src/new.rs"));
        assert_eq!(s.resolve(root.join("src")).unwrap(), root.join("src"));
        assert_eq!(s.resolve("src/../a/./b").unwrap(), root.join("a/b"));
        assert_eq!(s.resolve(".").unwrap(), root);
    }

    #[test]
    fn a_path_leaving_the_root_is_refused_naming_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let s = scope(tmp.path());
        for outside in ["../x", "a/../../x", "/etc/passwd"] {
            let err = s.resolve(outside).expect_err(outside).to_string();
            assert!(
                err.contains(&s.root().unwrap().display().to_string()),
                "{err}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_judged_by_where_they_lead() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
        std::os::unix::fs::symlink(root.join("inner"), root.join("in")).unwrap();
        std::fs::create_dir(root.join("inner")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("nowhere"), root.join("dangling")).unwrap();
        let s = scope(&root);

        // Existing and not-yet-existing paths through a link out are refused.
        assert!(s.resolve("out").is_err());
        assert!(s.resolve("out/new/file").is_err());
        // A link that stays inside is fine.
        assert!(s.resolve("in/new").is_ok());
        // A dangling link would create its target wherever it points.
        assert!(matches!(
            s.resolve("dangling"),
            Err(PathScopeError::Unresolvable { .. })
        ));
    }

    #[test]
    fn a_root_must_be_an_existing_directory() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(PathScope::confined(tmp.path().join("missing")).is_err());
        let file = tmp.path().join("f");
        std::fs::write(&file, "x").unwrap();
        assert!(PathScope::confined(&file).is_err());
    }
}
