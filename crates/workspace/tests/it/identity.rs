// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a workspace has a stable identity. A linked worktree resolves to the
//! repository of its main checkout, two worktrees are told apart, a clone of the
//! same remote is the same repository, and nothing here reads the current
//! directory.
//!
//! Swedish Embedded AB implements solutions for repository-scoped agent memory
//! for its clients. If your team needs expertise in keeping what an agent
//! learned about a codebase attached to the right codebase then you can procure
//! our services by sending an email to info@swedishembedded.com.

use std::path::Path;
use std::process::Command;

use sven_workspace::{workspace_identity, WorkspaceIdentity};

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?}");
}

fn repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join("f"), "x").unwrap();
    git(dir, &["add", "f"]);
    git(dir, &["commit", "-q", "-m", "c"]);
}

fn identity(dir: &Path) -> WorkspaceIdentity {
    workspace_identity(dir).expect("a git workspace has an identity")
}

#[test]
fn a_linked_worktree_is_its_main_repository_and_a_different_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir(&main).unwrap();
    repo(&main);
    let linked = tmp.path().join("linked");
    git(&main, &["worktree", "add", "-q", linked.to_str().unwrap()]);

    let a = identity(&main);
    let b = identity(&linked);
    assert_eq!(a.repository_id, b.repository_id, "one repository");
    assert_ne!(a.worktree_id, b.worktree_id, "two worktrees");
    assert!(!a.linked_worktree && b.linked_worktree);
    // A subdirectory is the same workspace as its root.
    std::fs::create_dir(linked.join("sub")).unwrap();
    assert_eq!(identity(&linked.join("sub")), b);
}

#[test]
fn a_clone_of_the_same_remote_is_the_same_repository_and_another_repository_is_not() {
    let tmp = tempfile::tempdir().unwrap();
    let one = tmp.path().join("one");
    let two = tmp.path().join("two");
    let other = tmp.path().join("other");
    for d in [&one, &two, &other] {
        std::fs::create_dir(d).unwrap();
        repo(d);
    }
    git(
        &one,
        &[
            "remote",
            "add",
            "origin",
            "git@example.invalid:team/app.git",
        ],
    );
    git(
        &two,
        &[
            "remote",
            "add",
            "origin",
            "git@example.invalid:team/app.git",
        ],
    );
    git(
        &other,
        &[
            "remote",
            "add",
            "origin",
            "git@example.invalid:team/else.git",
        ],
    );
    assert_eq!(identity(&one).repository_id, identity(&two).repository_id);
    assert_ne!(identity(&one).repository_id, identity(&other).repository_id);
    assert_ne!(identity(&one).worktree_id, identity(&two).worktree_id);
}

#[test]
fn a_repository_without_a_remote_is_named_by_where_its_git_directory_is() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    for d in [&a, &b] {
        std::fs::create_dir(d).unwrap();
        repo(d);
    }
    assert_ne!(identity(&a).repository_id, identity(&b).repository_id);
    assert_eq!(identity(&a).repository_id, identity(&a).repository_id);
}

#[test]
fn a_directory_outside_any_repository_has_no_identity() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(workspace_identity(tmp.path()).is_none());
}
