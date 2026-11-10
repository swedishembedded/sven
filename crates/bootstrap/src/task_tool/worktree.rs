// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running several coding agents
// on one repository without them overwriting each other's work for its
// clients. If your team needs expertise in parallel agent workflows then you
// can procure our services by sending an email to info@swedishembedded.com.
//! Isolated working trees for writing sub-agents (`isolation: "worktree"`).
//!
//! Every git operation of the feature lives here. A child gets its own
//! `git worktree` on a fresh branch, started from a *snapshot* of the
//! parent's working state, and what it changed is merged back into the
//! parent's tree when it finishes.
//!
//! # Guarantees
//!
//! - **The parent's index and tree are read, never written, until the merge
//!   lands.** The snapshot is built in a temporary index
//!   (`GIT_INDEX_FILE`): `read-tree HEAD`, `add -A` (so untracked files are
//!   included, ignored ones are not), `write-tree`, `commit-tree`. The
//!   parent's own index file is not touched.
//! - **Integration is all or nothing.** The merge is computed by
//!   `git merge-tree --write-tree`, which writes only objects. A conflict
//!   leaves the parent's tree exactly as it was; a clean result is applied
//!   as one `git apply`, which itself refuses to apply half a patch. The
//!   parent's index is not updated either: the changes arrive as ordinary
//!   uncommitted edits.
//! - **Integrations are serialized.** One lock covers snapshotting the
//!   parent's current state through applying the result, so each child
//!   merges on top of the previous one. The lock is process-wide: two sven
//!   processes integrating into one repository at the same instant are not
//!   coordinated.
//! - **Nothing is left behind.** The worktree directory is removed after
//!   integration, on a failed or cancelled child, and when the guard is
//!   dropped. A branch survives only when its work was not integrated; the
//!   branch then holds the child's complete final state, uncommitted and
//!   untracked files included.
//!
//! Requires git 2.40 or newer (`merge-tree --write-tree`).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Context, Result};

/// Held while a child's change is merged into the parent's tree.
static INTEGRATION: Mutex<()> = Mutex::new(());

/// A child's private working tree, cleaned up when dropped.
#[derive(Debug)]
pub(super) struct Worktree {
    /// Top level of the parent's repository.
    repo: PathBuf,
    /// Scratch directory holding the worktree and temporary indexes.
    base: PathBuf,
    /// The worktree itself (`base/tree`).
    dir: PathBuf,
    /// The directory the parent asked for, relative to the repository's top.
    rel: PathBuf,
    branch: String,
    snapshot_commit: String,
    snapshot_tree: String,
    /// Set once `integrate` or `keep` has settled the worktree.
    settled: bool,
}

/// How a finished child's change reached the parent.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Integration {
    /// Merged into the parent's tree; the files it changed (empty when the
    /// child changed nothing).
    Applied(Vec<String>),
    /// Overlaps changes of the parent or of an earlier child. Nothing was
    /// applied; `branch` holds the child's work.
    Conflicted { paths: Vec<String>, branch: String },
    /// Integration itself failed. Nothing was applied unless `git apply`
    /// reported otherwise; `branch` holds the child's work when it could be
    /// saved.
    Failed {
        reason: String,
        branch: Option<String>,
    },
}

impl Worktree {
    /// Snapshots the parent's working state at `workdir` and checks out a
    /// worktree of it under `state_root`, on a new branch.
    ///
    /// Fails when `workdir` is not in a git repository with a commit.
    pub(super) fn create(workdir: &Path, state_root: &Path) -> Result<Self> {
        let top = git_text(workdir, &["rev-parse", "--show-toplevel"], None)
            .map_err(|e| anyhow!("isolation \"worktree\" needs a git repository: {e:#}"))?;
        let repo = PathBuf::from(top);
        let workdir = workdir
            .canonicalize()
            .with_context(|| format!("resolving {}", workdir.display()))?;
        let repo = repo.canonicalize().unwrap_or(repo);
        let rel = workdir
            .strip_prefix(&repo)
            .with_context(|| {
                format!(
                    "{} is outside the repository at {}",
                    workdir.display(),
                    repo.display()
                )
            })?
            .to_path_buf();

        let head = git_text(&repo, &["rev-parse", "--verify", "HEAD^{commit}"], None)
            .context("the repository has no commit to base a worktree on")?;

        let id = uuid::Uuid::new_v4().simple().to_string();
        let id = &id[..12];
        let base = state_root.join(id);
        std::fs::create_dir_all(&base).with_context(|| format!("creating {}", base.display()))?;
        let mut wt = Self {
            dir: base.join("tree"),
            base,
            rel,
            branch: format!("sven/task-{id}"),
            snapshot_commit: String::new(),
            snapshot_tree: String::new(),
            repo,
            settled: false,
        };
        // On any error `wt` drops here and removes what exists so far.
        wt.snapshot_commit = wt.snapshot_parent(&head)?;
        wt.snapshot_tree = git_text(
            &wt.repo,
            &["rev-parse", &format!("{}^{{tree}}", wt.snapshot_commit)],
            None,
        )?;
        git_ok(
            &wt.repo,
            [
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("--quiet"),
                OsStr::new("-b"),
                OsStr::new(&wt.branch),
                wt.dir.as_os_str(),
                OsStr::new(&wt.snapshot_commit),
            ],
        )
        .context("creating the worktree")?;
        if !wt.workdir().is_dir() {
            bail!(
                "{} does not exist in the parent's snapshot (an empty directory is not tracked)",
                wt.rel.display()
            );
        }
        Ok(wt)
    }

    /// Where the child runs: the worktree's equivalent of the parent's
    /// requested directory.
    pub(super) fn workdir(&self) -> PathBuf {
        self.dir.join(&self.rel)
    }

    /// The branch holding the child's work.
    #[cfg(test)]
    pub(super) fn branch(&self) -> &str {
        &self.branch
    }

    /// Merges the child's change into the parent's tree and removes the
    /// worktree. The branch is deleted when integrated, kept otherwise.
    pub(super) fn integrate(mut self) -> Integration {
        self.settled = true;
        let outcome = self.try_integrate();
        match outcome {
            Ok(Integration::Applied(files)) => {
                self.remove_worktree();
                self.delete_branch();
                Integration::Applied(files)
            }
            Ok(Integration::Conflicted { paths, branch }) => {
                self.save_and_remove();
                Integration::Conflicted { paths, branch }
            }
            Ok(failed @ Integration::Failed { .. }) => failed,
            Err(e) => {
                let branch = self.save_and_remove();
                Integration::Failed {
                    reason: format!("{e:#}"),
                    branch,
                }
            }
        }
    }

    /// The child did not succeed: saves what it left on its branch, removes
    /// the worktree and names the branch. `None` when nothing could be saved.
    pub(super) fn keep(mut self) -> Option<String> {
        self.settled = true;
        self.save_and_remove()
    }

    fn try_integrate(&self) -> Result<Integration> {
        let (final_tree, final_commit) = self.save_final_state()?;
        let files = git_text(
            &self.repo,
            &[
                "diff",
                "--name-only",
                "--no-renames",
                &self.snapshot_tree,
                &final_tree,
            ],
            None,
        )?;
        let files: Vec<String> = files.lines().map(str::to_owned).collect();
        if files.is_empty() {
            return Ok(Integration::Applied(files));
        }

        let _serial = INTEGRATION.lock().unwrap_or_else(|e| e.into_inner());
        let head = git_text(
            &self.repo,
            &["rev-parse", "--verify", "HEAD^{commit}"],
            None,
        )?;
        let parent_commit = self.snapshot_of(&self.repo, &head, "parent")?;
        let merge = git_raw(
            &self.repo,
            &[
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                &format!("--merge-base={}", self.snapshot_commit),
                &parent_commit,
                &final_commit,
            ],
            None,
            None,
        )?;
        let stdout = String::from_utf8_lossy(&merge.stdout);
        let mut lines = stdout.lines();
        let merged_tree = lines.next().unwrap_or_default().trim().to_owned();
        match merge.status.code() {
            Some(0) => {}
            Some(1) => {
                let paths: Vec<String> = lines
                    .take_while(|l| !l.is_empty())
                    .map(str::to_owned)
                    .collect();
                return Ok(Integration::Conflicted {
                    paths,
                    branch: self.branch.clone(),
                });
            }
            _ => bail!(
                "git merge-tree failed ({}): {}",
                merge.status,
                String::from_utf8_lossy(&merge.stderr).trim()
            ),
        }

        let parent_tree = git_text(
            &self.repo,
            &["rev-parse", &format!("{parent_commit}^{{tree}}")],
            None,
        )?;
        let patch = git_raw(
            &self.repo,
            &[
                "diff",
                "--binary",
                "--no-renames",
                "--no-ext-diff",
                &parent_tree,
                &merged_tree,
            ],
            None,
            None,
        )?;
        check(&patch, "diff of the merge result")?;
        if !patch.stdout.is_empty() {
            let applied = git_raw(
                &self.repo,
                &["apply", "--whitespace=nowarn", "-"],
                Some(&patch.stdout),
                None,
            )?;
            if !applied.status.success() {
                return Ok(Integration::Failed {
                    reason: format!(
                        "applying the change to the working tree failed (the tree changed meanwhile?): {}",
                        String::from_utf8_lossy(&applied.stderr).trim()
                    ),
                    branch: self.save_and_remove(),
                });
            }
        }
        Ok(Integration::Applied(files))
    }

    /// Commits `head` plus the working state of `tree_dir` (tracked changes
    /// and untracked, non-ignored files) in a temporary index and returns
    /// the commit. The directory's real index is untouched.
    fn snapshot_of(&self, tree_dir: &Path, head: &str, label: &str) -> Result<String> {
        let index = self.base.join(format!("index-{label}"));
        let _ = std::fs::remove_file(&index);
        let result = (|| {
            git_ok_with_index(tree_dir, &index, ["read-tree", head])?;
            git_ok_with_index(tree_dir, &index, ["add", "-A", "--", "."])?;
            let tree = git_text(tree_dir, &["write-tree"], Some(&index))?;
            git_text(
                tree_dir,
                &[
                    "-c",
                    "user.name=sven",
                    "-c",
                    "user.email=sven@localhost",
                    "-c",
                    "commit.gpgsign=false",
                    "commit-tree",
                    &tree,
                    "-p",
                    head,
                    "-m",
                    "sven task snapshot",
                ],
                None,
            )
        })();
        let _ = std::fs::remove_file(&index);
        result.with_context(|| format!("snapshotting the {label} working state"))
    }

    fn snapshot_parent(&self, head: &str) -> Result<String> {
        self.snapshot_of(&self.repo, head, "parent")
    }

    /// The child's final state - its commits and its uncommitted and
    /// untracked changes - as a (tree, commit) pair.
    fn save_final_state(&self) -> Result<(String, String)> {
        let head = git_text(&self.dir, &["rev-parse", "--verify", "HEAD^{commit}"], None)?;
        let commit = self.snapshot_of(&self.dir, &head, "child")?;
        let tree = git_text(
            &self.dir,
            &["rev-parse", &format!("{commit}^{{tree}}")],
            None,
        )?;
        Ok((tree, commit))
    }

    /// Points the branch at the child's complete final state and removes the
    /// worktree. Returns the branch when it holds the work.
    fn save_and_remove(&self) -> Option<String> {
        let saved = self
            .save_final_state()
            .and_then(|(_, commit)| {
                git_ok(
                    &self.repo,
                    [
                        "update-ref",
                        &format!("refs/heads/{}", self.branch),
                        &commit,
                    ],
                )
            })
            .is_ok();
        self.remove_worktree();
        saved.then(|| self.branch.clone())
    }

    fn remove_worktree(&self) {
        let _ = git_ok(
            &self.repo,
            [
                OsStr::new("worktree"),
                OsStr::new("remove"),
                OsStr::new("--force"),
                self.dir.as_os_str(),
            ],
        );
        let _ = std::fs::remove_dir_all(&self.base);
        let _ = git_ok(&self.repo, ["worktree", "prune"]);
    }

    fn delete_branch(&self) {
        let _ = git_ok(&self.repo, ["branch", "-D", "--quiet", &self.branch]);
    }
}

impl Drop for Worktree {
    /// A guard dropped without `integrate` or `keep` - a cancelled call, an
    /// early return - still removes the directory and keeps the work.
    fn drop(&mut self) {
        if !self.settled {
            self.save_and_remove();
        }
    }
}

/// The failure of a finished command, with its stderr.
fn check(out: &Output, what: &str) -> Result<()> {
    if out.status.success() {
        Ok(())
    } else {
        bail!(
            "{what}: git exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )
    }
}

/// Runs `git args` in `dir`, with `index` as the index file when given. The
/// caller decides what a non-zero exit means.
fn git_raw(
    dir: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    index: Option<&Path>,
) -> Result<Output> {
    use std::io::Write;
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if let Some(bytes) = stdin {
        // The writer ends with the process's exit; a broken pipe is reported
        // through the exit status.
        let _ = child.stdin.take().expect("piped").write_all(bytes);
    }
    child
        .wait_with_output()
        .with_context(|| format!("waiting for git {}", args.join(" ")))
}

/// Stdout of a successful `git args`, trimmed.
fn git_text(dir: &Path, args: &[&str], index: Option<&Path>) -> Result<String> {
    let out = git_raw(dir, args, None, index)?;
    check(&out, &format!("git {}", args.join(" ")))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn git_ok<I, S>(dir: &Path, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<S> = args.into_iter().collect();
    let strs: Vec<String> = args
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();
    let refs: Vec<&str> = strs.iter().map(String::as_str).collect();
    git_text(dir, &refs, None).map(|_| ())
}

fn git_ok_with_index<const N: usize>(dir: &Path, index: &Path, args: [&str; N]) -> Result<()> {
    git_text(dir, &args, Some(index)).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) -> String {
        git_text(dir, args, None).unwrap_or_else(|e| panic!("git {args:?}: {e:#}"))
    }

    /// A repository with `a.txt` and `b.txt` committed.
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        sh(p, &["init", "--quiet", "-b", "main"]);
        sh(p, &["config", "user.name", "t"]);
        sh(p, &["config", "user.email", "t@t"]);
        std::fs::write(p.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(p.join("b.txt"), "x\ny\nz\n").unwrap();
        sh(p, &["add", "."]);
        sh(p, &["commit", "--quiet", "-m", "init"]);
        dir
    }

    #[test]
    fn a_directory_outside_a_repository_is_refused() {
        let plain = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let err = Worktree::create(plain.path(), state.path()).unwrap_err();
        assert!(format!("{err:#}").contains("git repository"), "{err:#}");
    }

    #[test]
    fn the_snapshot_carries_uncommitted_and_untracked_work_and_spares_the_index() {
        let repo = repo();
        let p = repo.path();
        std::fs::write(p.join("a.txt"), "ONE\ntwo\nthree\n").unwrap();
        std::fs::write(p.join("new.txt"), "fresh\n").unwrap();
        std::fs::write(p.join("b.txt"), "x\nY\nz\n").unwrap();
        sh(p, &["add", "b.txt"]); // staged
        let index_before = std::fs::read(p.join(".git/index")).unwrap();
        let status_before = sh(p, &["status", "--porcelain"]);

        let state = tempfile::tempdir().unwrap();
        let wt = Worktree::create(p, state.path()).unwrap();
        let dir = wt.workdir();
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "ONE\ntwo\nthree\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("new.txt")).unwrap(),
            "fresh\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("b.txt")).unwrap(),
            "x\nY\nz\n"
        );

        assert_eq!(std::fs::read(p.join(".git/index")).unwrap(), index_before);
        assert_eq!(sh(p, &["status", "--porcelain"]), status_before);
        drop(wt);
    }

    #[test]
    fn integrating_applies_the_childs_commits_and_loose_changes_and_cleans_up() {
        let repo = repo();
        let p = repo.path();
        let state = tempfile::tempdir().unwrap();
        let wt = Worktree::create(p, state.path()).unwrap();
        let dir = wt.workdir();
        let branch = wt.branch().to_owned();
        std::fs::write(dir.join("a.txt"), "one\n2\nthree\n").unwrap();
        sh(
            &dir,
            &[
                "-c",
                "user.name=c",
                "-c",
                "user.email=c@c",
                "commit",
                "-qam",
                "child",
            ],
        );
        std::fs::write(dir.join("loose.txt"), "untracked\n").unwrap();
        std::fs::write(dir.join("bin.dat"), [0u8, 159, 146, 150]).unwrap();

        let Integration::Applied(mut files) = wt.integrate() else {
            panic!("expected a clean integration")
        };
        files.sort();
        assert_eq!(files, ["a.txt", "bin.dat", "loose.txt"]);
        assert_eq!(
            std::fs::read_to_string(p.join("a.txt")).unwrap(),
            "one\n2\nthree\n"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("loose.txt")).unwrap(),
            "untracked\n"
        );
        assert_eq!(
            std::fs::read(p.join("bin.dat")).unwrap(),
            [0u8, 159, 146, 150]
        );
        assert!(
            std::fs::read_dir(state.path()).unwrap().next().is_none(),
            "worktree dir is gone"
        );
        assert!(
            sh(p, &["branch", "--list", &branch]).is_empty(),
            "integrated branch is deleted"
        );
        // The result is uncommitted: the parent's index was not updated.
        assert!(sh(p, &["diff", "--cached", "--name-only"]).is_empty());
    }

    #[test]
    fn changes_to_different_places_of_one_file_both_land_and_overlapping_ones_conflict() {
        let repo = repo();
        let p = repo.path();
        let state = tempfile::tempdir().unwrap();
        let first = Worktree::create(p, state.path()).unwrap();
        let second = Worktree::create(p, state.path()).unwrap();
        let third = Worktree::create(p, state.path()).unwrap();
        std::fs::write(first.workdir().join("a.txt"), "ONE\ntwo\nthree\n").unwrap();
        std::fs::write(second.workdir().join("a.txt"), "one\ntwo\nTHREE\n").unwrap();
        std::fs::write(third.workdir().join("a.txt"), "uno\ntwo\nthree\n").unwrap();

        assert!(matches!(first.integrate(), Integration::Applied(_)));
        assert!(matches!(second.integrate(), Integration::Applied(_)));
        assert_eq!(
            std::fs::read_to_string(p.join("a.txt")).unwrap(),
            "ONE\ntwo\nTHREE\n"
        );

        let third_branch = third.branch().to_owned();
        match third.integrate() {
            Integration::Conflicted { paths, branch } => {
                assert_eq!(paths, ["a.txt"]);
                assert_eq!(branch, third_branch);
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
        // All or nothing: the tree is exactly the first two changes, no markers.
        assert_eq!(
            std::fs::read_to_string(p.join("a.txt")).unwrap(),
            "ONE\ntwo\nTHREE\n"
        );
        assert_eq!(
            sh(p, &["show", &format!("{third_branch}:a.txt")]),
            "uno\ntwo\nthree"
        );
        assert!(std::fs::read_dir(state.path()).unwrap().next().is_none());
    }

    #[test]
    fn a_kept_worktree_names_a_branch_with_all_its_work_and_leaves_no_directory() {
        let repo = repo();
        let p = repo.path();
        let state = tempfile::tempdir().unwrap();
        let wt = Worktree::create(p, state.path()).unwrap();
        std::fs::write(wt.workdir().join("wip.txt"), "half done\n").unwrap();
        let branch = wt.keep().expect("the work is saved");
        assert_eq!(sh(p, &["show", &format!("{branch}:wip.txt")]), "half done");
        assert!(!p.join("wip.txt").exists());
        assert!(std::fs::read_dir(state.path()).unwrap().next().is_none());
    }

    #[test]
    fn a_dropped_guard_cleans_up_and_keeps_the_branch() {
        let repo = repo();
        let state = tempfile::tempdir().unwrap();
        let wt = Worktree::create(repo.path(), state.path()).unwrap();
        let branch = wt.branch().to_owned();
        drop(wt);
        assert!(std::fs::read_dir(state.path()).unwrap().next().is_none());
        assert!(!sh(repo.path(), &["branch", "--list", &branch]).is_empty());
    }

    #[test]
    fn a_subdirectory_workdir_maps_into_the_worktree() {
        let repo = repo();
        let p = repo.path();
        std::fs::create_dir(p.join("sub")).unwrap();
        std::fs::write(p.join("sub/f.txt"), "f\n").unwrap();
        sh(p, &["add", "."]);
        sh(p, &["commit", "-qm", "sub"]);
        let state = tempfile::tempdir().unwrap();
        let wt = Worktree::create(&p.join("sub"), state.path()).unwrap();
        assert!(wt.workdir().ends_with("tree/sub"));
        assert!(wt.workdir().join("f.txt").is_file());
    }
}
