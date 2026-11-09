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
//! the check and the use is not defended against by [`PathScope::resolve`]. A
//! root confines the tools built with it; it is not a sandbox.
//!
//! # Race-free opening
//!
//! Where another process writes the tree while tools run (the agent's
//! confined commands in a served harness run), [`PathScope::race_free`] holds
//! the root open as a directory and [`PathScope::open`] opens every file
//! with `openat2(root, relative, RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`:
//! the kernel resolves the path and refuses (`EXDEV`) anything that leaves
//! the root, so there is no window between a check and a use. The tool then
//! works on the returned descriptor only. Relative symlinks that stay inside
//! the root keep working; absolute ones are refused by the kernel. Missing
//! parent directories are created one component at a time with `mkdirat`
//! and the same resolution rules. The deny rules are applied to where the
//! opened descriptor really is (`/proc/self/fd`), and a file is only
//! truncated after that check. A tool that must hand a *path* to a walker, a
//! library or another process cannot be made race-free this way. Without
//! `race_free` (ordinary sven) `open` is [`PathScope::resolve`] and an
//! ordinary open.

use std::fmt;
use std::fs::File;
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
    /// Canonical locations inside the root that are refused all the same
    /// (see [`PathScope::denying`]).
    denied: Arc<Vec<PathBuf>>,
    /// Names refused as any component beneath the root (see
    /// [`PathScope::denying_names`]).
    denied_names: Arc<Vec<std::ffi::OsString>>,
    /// The root held open for race-free lookups (see [`PathScope::race_free`]).
    anchor: Anchor,
}

/// The open root directory of a race-free scope; absent otherwise.
#[derive(Clone, Debug, Default)]
struct Anchor(#[cfg(target_os = "linux")] Option<Arc<crate::beneath::Beneath>>);

impl PartialEq for Anchor {
    fn eq(&self, other: &Self) -> bool {
        #[cfg(target_os = "linux")]
        return match (&self.0, &other.0) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        #[cfg(not(target_os = "linux"))]
        return {
            let _ = other;
            true
        };
    }
}

impl Eq for Anchor {}

/// How [`PathScope::open`] opens a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    /// Read an existing file.
    Read,
    /// Read and rewrite an existing file in place; never creates.
    Modify,
    /// Create the file (and the directories above it) if missing, then
    /// empty it.
    Truncate,
    /// Create the file (and the directories above it) if missing, then
    /// append to it.
    Append,
}

/// Why [`PathScope::open`] failed.
#[derive(Debug)]
pub enum OpenError {
    /// The scope refuses the path (outside the root, denied, unresolvable).
    Refused(PathScopeError),
    /// The path is acceptable but the file could not be opened.
    Io(io::Error),
}

impl OpenError {
    /// The error result the model reads: a refusal as the scope words it, any
    /// other failure as `"{what}: {error}"`.
    #[must_use]
    pub fn into_output(self, call: &ToolCall, what: &str) -> ToolOutput {
        match self {
            Self::Refused(refused) => ToolOutput::err(&call.id, refused.to_string()),
            Self::Io(e) => ToolOutput::err(&call.id, format!("{what}: {e}")),
        }
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(e) => e.fmt(f),
            Self::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for OpenError {}

impl From<PathScopeError> for OpenError {
    fn from(refused: PathScopeError) -> Self {
        Self::Refused(refused)
    }
}

impl From<io::Error> for OpenError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
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
    /// The path lies in a location the scope was told to keep tools out of.
    Denied {
        /// The path as the caller gave it.
        requested: PathBuf,
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
            Self::Denied { requested } => write!(
                f,
                "path '{}' is a protected location tools never read or write",
                requested.display()
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
            denied: Arc::default(),
            denied_names: Arc::default(),
            anchor: Anchor::default(),
        })
    }

    /// This scope with its root held open, so that [`Self::open`] resolves
    /// every path inside the kernel (`openat2` with `RESOLVE_BENEATH`) and
    /// not in this process: a concurrent process cannot swap a directory for
    /// a symlink between the check and the use. Meaningful only on a confined
    /// scope; an unconfined one is returned unchanged.
    ///
    /// # Errors
    ///
    /// When the root cannot be opened, or the platform has no `openat2`
    /// (anything but Linux): a scope that was asked to be race-free and cannot
    /// be must not silently fall back.
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    pub fn race_free(mut self) -> io::Result<Self> {
        let Some(root) = self.root.as_deref() else {
            return Ok(self);
        };
        #[cfg(target_os = "linux")]
        {
            self.anchor = Anchor(Some(Arc::new(crate::beneath::Beneath::new(root)?)));
            Ok(self)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = root;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "race-free path resolution needs Linux openat2",
            ))
        }
    }

    /// [`Self::race_free`] where the platform can, else this scope as it was:
    /// for readers of a tree another process may write, which on a platform
    /// without `openat2` have no confinement to protect anyway.
    #[must_use]
    pub fn race_free_where_supported(self) -> Self {
        self.clone().race_free().unwrap_or(self)
    }

    /// Whether [`Self::open`] resolves inside the kernel on this scope.
    #[must_use]
    pub fn is_race_free(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.anchor.0.is_some();
        #[cfg(not(target_os = "linux"))]
        return false;
    }

    /// This scope, also refusing everything at or beneath `location` (a
    /// file or directory, followed through symlinks as far as it exists):
    /// credentials that happen to lie inside the root. Meaningful only on a
    /// confined scope; an unconfined one resolves nothing and refuses nothing.
    #[must_use]
    pub fn denying(mut self, location: impl AsRef<Path>) -> Self {
        let location = location.as_ref();
        let absolute = if location.is_absolute() {
            location.to_path_buf()
        } else {
            return self;
        };
        if let Ok(resolved) = follow_existing_prefix(&normalise(&absolute)) {
            Arc::make_mut(&mut self.denied).push(resolved);
        }
        self
    }

    /// This scope, also refusing every path beneath the root that has one of
    /// `names` as a component, at any depth: what another program runs on its
    /// own when it later works in some directory of the root (its
    /// configuration, git's hooks). Judged on the resolved path, so a link to
    /// such a location is refused too. Meaningful only on a confined scope.
    #[must_use]
    pub fn denying_names<'a>(mut self, names: impl IntoIterator<Item = &'a str>) -> Self {
        Arc::make_mut(&mut self.denied_names).extend(names.into_iter().map(Into::into));
        self
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
        self.judge(requested, root, resolved)
    }

    /// Applies the deny rules and the containment rule to `resolved`.
    fn judge(
        &self,
        requested: &Path,
        root: &Path,
        resolved: PathBuf,
    ) -> Result<PathBuf, PathScopeError> {
        let named = resolved.strip_prefix(root).is_ok_and(|inside| {
            inside.components().any(|part| {
                self.denied_names
                    .iter()
                    .any(|name| part.as_os_str() == name)
            })
        });
        if named || self.denied.iter().any(|d| resolved.starts_with(d)) {
            Err(PathScopeError::Denied {
                requested: requested.to_path_buf(),
            })
        } else if resolved.starts_with(root) {
            Ok(resolved)
        } else {
            Err(PathScopeError::Outside {
                requested: requested.to_path_buf(),
                root: root.to_path_buf(),
            })
        }
    }

    /// Opens the file `requested` names in `mode`.
    ///
    /// On a [race-free](Self::race_free) scope the kernel resolves the path
    /// beneath the open root, so what is opened is beneath the root whatever
    /// another process does to the tree meanwhile, and the deny rules are then
    /// applied to where the opened descriptor really is. Otherwise this is
    /// [`Self::resolve`] followed by an ordinary open, with the window that
    /// the module documentation describes.
    ///
    /// # Errors
    ///
    /// [`OpenError::Refused`] when the scope refuses the path;
    /// [`OpenError::Io`] when the open itself fails. Nothing is assumed about
    /// what was opened (a directory opens for reading); the caller reads
    /// the descriptor's own metadata.
    pub fn open(&self, requested: impl AsRef<Path>, mode: OpenMode) -> Result<File, OpenError> {
        let requested = requested.as_ref();
        let resolved = self.resolve(requested)?;
        #[cfg(target_os = "linux")]
        if let (Some(anchor), Some(root)) = (self.anchor.0.as_deref(), self.root.as_deref()) {
            return self.open_beneath(anchor, root, requested, &resolved, mode);
        }
        if matches!(mode, OpenMode::Truncate | OpenMode::Append) {
            if let Some(parent) = resolved.parent() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut options = std::fs::OpenOptions::new();
        match mode {
            OpenMode::Read => options.read(true),
            OpenMode::Modify => options.read(true).write(true),
            OpenMode::Truncate => options.write(true).create(true).truncate(true),
            OpenMode::Append => options.append(true).create(true),
        };
        Ok(options.open(resolved)?)
    }

    #[cfg(target_os = "linux")]
    fn open_beneath(
        &self,
        anchor: &crate::beneath::Beneath,
        root: &Path,
        requested: &Path,
        resolved: &Path,
        mode: OpenMode,
    ) -> Result<File, OpenError> {
        let rel = resolved.strip_prefix(root).unwrap_or(Path::new("."));
        let rel = if rel.as_os_str().is_empty() {
            Path::new(".")
        } else {
            rel
        };
        let flags = libc::O_NONBLOCK
            | libc::O_NOCTTY
            | match mode {
                OpenMode::Read => libc::O_RDONLY,
                OpenMode::Modify => libc::O_RDWR,
                OpenMode::Truncate => libc::O_WRONLY | libc::O_CREAT,
                OpenMode::Append => libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
            };
        if matches!(mode, OpenMode::Truncate | OpenMode::Append) {
            if let Some(parent) = rel.parent() {
                anchor
                    .create_dir_all(parent)
                    .map_err(|e| left(e, requested, root))?;
            }
        }
        let file = anchor
            .open(rel, flags, 0o666)
            .map_err(|e| left(e, requested, root))?;
        // The deny rules judge the object actually opened. Truncation waits
        // for this: a denied file must not be emptied before it is refused.
        let actual = crate::beneath::path_of(&file).map_err(|e| PathScopeError::Unresolvable {
            requested: requested.to_path_buf(),
            root: root.to_path_buf(),
            reason: format!("the opened file's location is unknown: {e}"),
        })?;
        self.judge(requested, root, actual)?;
        if mode == OpenMode::Truncate {
            file.set_len(0)?;
        }
        Ok(file)
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

/// An `openat2` failure as a refusal when the kernel kept the lookup from
/// leaving the root (`EXDEV`) or from following a magic link (`ELOOP`).
#[cfg(target_os = "linux")]
fn left(e: io::Error, requested: &Path, root: &Path) -> OpenError {
    match e.raw_os_error() {
        Some(libc::EXDEV) => OpenError::Refused(PathScopeError::Outside {
            requested: requested.to_path_buf(),
            root: root.to_path_buf(),
        }),
        Some(libc::ELOOP) => OpenError::Refused(PathScopeError::Unresolvable {
            requested: requested.to_path_buf(),
            root: root.to_path_buf(),
            reason: e.to_string(),
        }),
        _ => OpenError::Io(e),
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

    #[cfg(unix)]
    #[test]
    fn a_denied_location_inside_the_root_is_refused_however_it_is_reached() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(root.join("secrets")).unwrap();
        std::fs::write(root.join("secrets/key"), "k").unwrap();
        std::os::unix::fs::symlink(root.join("secrets"), root.join("alias")).unwrap();
        let s = scope(&root).denying(root.join("secrets"));
        let canonical = s.root().unwrap().to_path_buf();

        for path in ["secrets", "secrets/key", "alias/key", "x/../secrets/key"] {
            assert!(
                matches!(s.resolve(path), Err(PathScopeError::Denied { .. })),
                "{path}"
            );
        }
        assert_eq!(s.resolve("other").unwrap(), canonical.join("other"));
    }

    /// A name denied anywhere under the root: what a later program runs from
    /// a directory (its configuration, git's hooks) is refused at any depth
    /// and through any link, while a file that only resembles it is not.
    #[cfg(unix)]
    #[test]
    fn a_denied_name_is_refused_at_any_depth_however_it_is_reached() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(root.join("sub/.git/hooks")).unwrap();
        std::os::unix::fs::symlink(root.join("sub/.git"), root.join("g")).unwrap();
        let s = scope(&root).denying_names([".git", ".sven.yaml"]);
        let canonical = s.root().unwrap().to_path_buf();

        for path in [
            ".git/hooks/pre-commit",
            "sub/.git/config",
            "g/hooks/pre-commit",
            ".sven.yaml",
            "a/b/.sven.yaml",
        ] {
            assert!(
                matches!(s.resolve(path), Err(PathScopeError::Denied { .. })),
                "{path}"
            );
        }
        for path in [".gitignore", "sven.yaml.md", "notes/.git-tips"] {
            assert_eq!(s.resolve(path).unwrap(), canonical.join(path), "{path}");
        }
    }

    /// A race-free scope opens what the plain one does: it reads and writes
    /// beneath the root (creating the directories above a new file), follows a
    /// link that stays inside, and refuses a link out and a denied file, the
    /// latter without emptying it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_race_free_scope_opens_beneath_the_root_and_refuses_the_rest() {
        use std::io::{Read, Write};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(root.join("secrets")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("secrets/key"), "k").unwrap();
        std::fs::write(outside.join("f"), "o").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
        std::os::unix::fs::symlink("a/b.txt", root.join("alias")).unwrap();
        let s = scope(&root)
            .denying(root.join("secrets"))
            .race_free()
            .unwrap();
        assert!(s.is_race_free());

        let mut text = String::new();
        s.open("a/b.txt", OpenMode::Truncate)
            .unwrap()
            .write_all(b"one")
            .unwrap();
        s.open("a/b.txt", OpenMode::Append)
            .unwrap()
            .write_all(b"+two")
            .unwrap();
        s.open("alias", OpenMode::Read)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "one+two");

        for path in ["out/f", "../outside/f", "/etc/passwd"] {
            assert!(
                matches!(s.open(path, OpenMode::Read), Err(OpenError::Refused(_))),
                "{path}"
            );
        }
        assert!(matches!(
            s.open("secrets/key", OpenMode::Truncate),
            Err(OpenError::Refused(PathScopeError::Denied { .. }))
        ));
        assert_eq!(
            std::fs::read_to_string(root.join("secrets/key")).unwrap(),
            "k"
        );
        assert!(matches!(
            s.open("missing", OpenMode::Modify),
            Err(OpenError::Io(e)) if e.kind() == io::ErrorKind::NotFound
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
