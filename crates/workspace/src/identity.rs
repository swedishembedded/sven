// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for repository-scoped agent memory
// for its clients. If your team needs expertise in keeping what an agent
// learned about a codebase attached to the right codebase then you can procure
// our services by sending an email to info@swedishembedded.com.
//! The identity of a workspace: which repository, and which checkout of it.
//!
//! Memory and knowledge about a codebase belong to the repository, not to the
//! directory it happens to be checked out in: a linked worktree of the same
//! repository shares them, a different repository does not, and two worktrees
//! can still be told apart. Everything here takes the directory it is asked
//! about; nothing reads the current directory.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Which repository a workspace belongs to and which checkout of it it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceIdentity {
    /// Names the repository. The same for every worktree of one repository and
    /// for clones that share the `origin` remote; for a repository without one,
    /// the location of its git directory.
    pub repository_id: String,
    /// Names the checkout: the canonical path of the worktree's root.
    pub worktree_id: String,
    /// Whether this is a linked worktree rather than the main checkout.
    pub linked_worktree: bool,
    /// The root of the worktree.
    pub root: PathBuf,
}

/// The identity of the workspace holding `dir`, or `None` when `dir` is not in
/// a git working tree (or cannot be read).
#[must_use]
pub fn workspace_identity(dir: &Path) -> Option<WorkspaceIdentity> {
    let dir = dir.canonicalize().ok()?;
    let (root, dot_git) = dir.ancestors().find_map(|d| {
        d.join(".git")
            .exists()
            .then(|| (d.to_path_buf(), d.join(".git")))
    })?;
    let (git_dir, linked) = if dot_git.is_dir() {
        (dot_git, false)
    } else {
        // A linked worktree's `.git` is a file naming its private git directory.
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let private = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        let private = if private.is_absolute() {
            private
        } else {
            root.join(private)
        };
        (private.canonicalize().ok()?, true)
    };
    // The private directory of a linked worktree points at the common one.
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => git_dir.join(text.trim()).canonicalize().ok()?,
        Err(_) => git_dir.clone(),
    };
    let basis = match origin_url(&common) {
        Some(url) => format!("remote:{url}"),
        None => format!("path:{}", common.display()),
    };
    Some(WorkspaceIdentity {
        repository_id: format!("repo-{}", short_digest(&basis)),
        worktree_id: format!("wt-{}", short_digest(&root.display().to_string())),
        linked_worktree: linked,
        root,
    })
}

fn short_digest(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// The URL of the `origin` remote in the common git directory's config.
fn origin_url(common: &Path) -> Option<String> {
    let config = std::fs::read_to_string(common.join("config")).ok()?;
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
        } else if in_origin {
            if let Some(url) = line.strip_prefix("url") {
                return Some(url.trim_start().strip_prefix('=')?.trim().to_owned());
            }
        }
    }
    None
}
