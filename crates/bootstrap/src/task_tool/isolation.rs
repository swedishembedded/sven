// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running several coding agents
// on one repository without them overwriting each other's work for its
// clients. If your team needs expertise in parallel agent workflows then you
// can procure our services by sending an email to info@swedishembedded.com.
//! The `isolation` argument of `task`: what it means for a spawn and what
//! becomes of an isolated child's work. The git operations are in
//! [`super::worktree`].

use std::path::PathBuf;

use sven_tool_api::tool::ToolOutput;

use super::worktree;
use serde_json::Value;

/// Where a writing child's changes happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Isolation {
    /// The parent's own tree (the default).
    None,
    /// A private git worktree whose change is merged back when the child
    /// succeeds (see `worktree`).
    Worktree,
}

impl Isolation {
    /// The `isolation` argument of a spawn running in ACP mode `acp_mode`.
    /// A read-only child changes nothing, so asking for a worktree for it is
    /// refused rather than silently ignored.
    pub(super) fn of(args: &Value, acp_mode: &str) -> Result<Self, String> {
        let isolation = match args.get("isolation") {
            None | Some(Value::Null) => Self::None,
            Some(Value::String(v)) if v == "none" => Self::None,
            Some(Value::String(v)) if v == "worktree" => Self::Worktree,
            Some(other) => {
                return Err(format!(
                    "unknown isolation {other}. Valid options: none, worktree"
                ))
            }
        };
        if isolation == Self::Worktree && acp_mode != "agent" {
            return Err(format!(
                "isolation \"worktree\" is for writing children; a '{acp_mode}' child is read-only and shares the parent's tree safely"
            ));
        }
        Ok(isolation)
    }
}

/// Where isolated worktrees are made: the user's state directory, never
/// inside a project tree.
pub(super) fn default_worktree_root() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("sven")
        .join("task-worktrees")
}

/// Snapshots the parent's state at `workdir` into a new worktree. Git runs
/// off the async runtime.
pub(super) async fn create_worktree(
    workdir: PathBuf,
    root: PathBuf,
) -> Result<worktree::Worktree, String> {
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&root)
            .map_err(|e| format!("cannot create {}: {e}", root.display()))?;
        worktree::Worktree::create(&workdir, &root).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| format!("worktree setup failed: {e}"))?
}

/// Decides what becomes of an isolated child's work and says so in its
/// result: a child that succeeded is merged into the parent's tree; one that
/// failed, or whose change cannot be merged, is never applied and keeps its
/// branch for the parent to inspect.
pub(super) async fn settle(
    worktree: worktree::Worktree,
    output: ToolOutput,
    call_id: &str,
) -> ToolOutput {
    use worktree::Integration;
    let branch_note = |branch: &str| {
        format!("Its work is kept on branch '{branch}' (inspect with `git diff HEAD...{branch}` or `git log {branch}`).")
    };
    let work = output.content.clone();
    let refuse = |note: String| {
        let mut refused = ToolOutput::err(call_id, format!("{work}\n\n{note}"));
        refused.artifacts = output.artifacts.clone();
        refused
    };
    if output.is_error {
        let kept = tokio::task::spawn_blocking(move || worktree.keep())
            .await
            .ok()
            .flatten();
        let note = match kept {
            Some(branch) => format!(
                "The sub-agent did not finish, so nothing was integrated into your tree. {}",
                branch_note(&branch)
            ),
            None => "The sub-agent did not finish, so nothing was integrated into your tree. Its work could not be saved.".to_string(),
        };
        return refuse(note);
    }
    let integration = tokio::task::spawn_blocking(move || worktree.integrate())
        .await
        .unwrap_or_else(|e| Integration::Failed {
            reason: format!("integration task died: {e}"),
            branch: None,
        });
    match integration {
        Integration::Applied(files) if files.is_empty() => {
            let mut done = output;
            done.content = format!(
                "{}\n\nIntegrated: the sub-agent changed no files.",
                done.content
            );
            done
        }
        Integration::Applied(files) => {
            let mut done = output;
            done.content = format!(
                "{}\n\nIntegrated into your working tree (uncommitted). Files changed:\n{}",
                done.content,
                files
                    .iter()
                    .map(|f| format!("- {f}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            done
        }
        Integration::Conflicted { paths, branch } => refuse(format!(
            "Integration failed: the sub-agent's change conflicts with your tree (or an earlier sub-agent's integrated change) in: {}. Nothing was applied. {}",
            paths.join(", "),
            branch_note(&branch)
        )),
        Integration::Failed { reason, branch } => refuse(format!(
            "Integration failed, nothing was applied: {reason}. {}",
            branch.map_or_else(
                || "The sub-agent's work could not be saved.".to_string(),
                |b| branch_note(&b)
            )
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::super::tests::{call, make_task};
    use super::super::TaskTool;
    use sven_tool_api::tool::{Tool, ToolCall};

    /// A child that edits files in its workdir as `edit` says, then fails
    /// when `edit` returns an error.
    struct Scripted<F>(F);

    #[async_trait::async_trait]
    impl<F> super::super::runner::ChildRunner for Scripted<F>
    where
        F: Fn(&std::path::Path) -> Result<(), String> + Send + Sync,
    {
        async fn run(&self, args: super::super::SpawnArgs) -> sven_tool_api::tool::ToolOutput {
            match (self.0)(&args.workdir) {
                Ok(()) => sven_tool_api::tool::ToolOutput::ok(&args.call_id, "child done"),
                Err(e) => sven_tool_api::tool::ToolOutput::err(&args.call_id, e),
            }
        }
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn repo_with_files() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "--quiet", "-b", "main"]);
        git(p, &["config", "user.name", "t"]);
        git(p, &["config", "user.email", "t@t"]);
        std::fs::write(p.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(p.join("b.txt"), "x\ny\nz\n").unwrap();
        git(p, &["add", "."]);
        git(p, &["commit", "--quiet", "-m", "init"]);
        dir
    }

    fn isolated_task<F>(repo: &std::path::Path, state: &std::path::Path, edit: F) -> TaskTool
    where
        F: Fn(&std::path::Path) -> Result<(), String> + Send + Sync + 'static,
    {
        make_task()
            .with_scope(sven_tool_api::PathScope::confined(repo).unwrap())
            .with_worktree_root(state.to_path_buf())
            .with_runner(Arc::new(Scripted(edit)))
    }

    fn spawn_isolated(prompt: &str) -> ToolCall {
        call(json!({"prompt": prompt, "mode": "agent", "isolation": "worktree"}))
    }

    fn is_empty_dir(dir: &std::path::Path) -> bool {
        std::fs::read_dir(dir).unwrap().next().is_none()
    }

    #[tokio::test]
    async fn an_isolated_childs_change_is_integrated_and_the_parents_own_work_is_untouched() {
        let repo = repo_with_files();
        let state = tempfile::tempdir().unwrap();
        let p = repo.path();
        std::fs::write(p.join("b.txt"), "x\nY\nz\n").unwrap(); // parent's uncommitted work
        std::fs::write(p.join("scratch.txt"), "mine\n").unwrap(); // untracked
        git(p, &["add", "b.txt"]);
        let index_before = std::fs::read(p.join(".git/index")).unwrap();

        let parent = p.canonicalize().unwrap();
        let seen_by_child = Arc::new(std::sync::Mutex::new(String::new()));
        let seen = Arc::clone(&seen_by_child);
        let task = isolated_task(p, state.path(), move |dir| {
            assert!(
                !dir.starts_with(&parent),
                "the child must not run in the parent's tree"
            );
            *seen.lock().unwrap() = std::fs::read_to_string(dir.join("b.txt")).unwrap()
                + &std::fs::read_to_string(dir.join("scratch.txt")).unwrap();
            std::fs::write(dir.join("a.txt"), "ONE\ntwo\nthree\n").map_err(|e| e.to_string())
        });
        let out = task.execute(&spawn_isolated("edit a")).await;

        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("Integrated") && out.content.contains("a.txt"),
            "{}",
            out.content
        );
        assert_eq!(
            *seen_by_child.lock().unwrap(),
            "x\nY\nz\nmine\n",
            "the child started from the parent's working state"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("a.txt")).unwrap(),
            "ONE\ntwo\nthree\n"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("b.txt")).unwrap(),
            "x\nY\nz\n"
        );
        assert_eq!(
            std::fs::read(p.join(".git/index")).unwrap(),
            index_before,
            "the parent's index is untouched"
        );
        assert!(is_empty_dir(state.path()), "the worktree directory is gone");
    }

    #[tokio::test]
    async fn two_isolated_children_editing_different_files_are_both_integrated() {
        let repo = repo_with_files();
        let state = tempfile::tempdir().unwrap();
        let p = repo.path();
        let first = isolated_task(p, state.path(), |d| {
            std::fs::write(d.join("a.txt"), "A\ntwo\nthree\n").map_err(|e| e.to_string())
        });
        let second = isolated_task(p, state.path(), |d| {
            std::fs::write(d.join("b.txt"), "x\ny\nZ\n").map_err(|e| e.to_string())
        });
        let (call_one, call_two) = (spawn_isolated("one"), spawn_isolated("two"));
        let (r1, r2) = tokio::join!(first.execute(&call_one), second.execute(&call_two));
        assert!(
            !r1.is_error && !r2.is_error,
            "{} / {}",
            r1.content,
            r2.content
        );
        assert_eq!(
            std::fs::read_to_string(p.join("a.txt")).unwrap(),
            "A\ntwo\nthree\n"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("b.txt")).unwrap(),
            "x\ny\nZ\n"
        );
        assert!(is_empty_dir(state.path()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_second_of_two_children_changing_the_same_lines_is_a_conflict_and_the_tree_keeps_only_the_first(
    ) {
        use std::sync::atomic::{AtomicBool, Ordering};
        let repo = repo_with_files();
        let state = tempfile::tempdir().unwrap();
        let p = repo.path();
        // Both children start from the same snapshot (the barrier), and the
        // second finishes only after the first has been integrated.
        let both_started = Arc::new(std::sync::Barrier::new(2));
        let first_integrated = Arc::new(AtomicBool::new(false));
        let first = isolated_task(p, state.path(), {
            let started = Arc::clone(&both_started);
            move |d| {
                started.wait();
                std::fs::write(d.join("a.txt"), "FIRST\ntwo\nthree\n").map_err(|e| e.to_string())
            }
        });
        let second = isolated_task(p, state.path(), {
            let started = Arc::clone(&both_started);
            let first_integrated = Arc::clone(&first_integrated);
            move |d| {
                started.wait();
                while !first_integrated.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                std::fs::write(d.join("a.txt"), "SECOND\ntwo\nthree\n").map_err(|e| e.to_string())
            }
        });
        let flag = Arc::clone(&first_integrated);
        let first = tokio::spawn(async move {
            let out = first.execute(&spawn_isolated("one")).await;
            flag.store(true, Ordering::SeqCst);
            out
        });
        let second = tokio::spawn(async move { second.execute(&spawn_isolated("two")).await });
        let (first, second) = (first.await.unwrap(), second.await.unwrap());

        assert!(!first.is_error, "{}", first.content);
        assert!(second.is_error, "{}", second.content);
        assert!(
            second.content.contains("Integration failed")
                && second.content.contains("a.txt")
                && second.content.contains("sven/task-"),
            "{}",
            second.content
        );
        assert_eq!(
            std::fs::read_to_string(p.join("a.txt")).unwrap(),
            "FIRST\ntwo\nthree\n",
            "exactly the first change, no conflict markers"
        );
        assert!(is_empty_dir(state.path()));
    }

    #[tokio::test]
    async fn a_failed_isolated_child_is_not_integrated_and_its_branch_is_named() {
        let repo = repo_with_files();
        let state = tempfile::tempdir().unwrap();
        let p = repo.path();
        let task = isolated_task(p, state.path(), |d| {
            std::fs::write(d.join("a.txt"), "HALF\n").map_err(|e| e.to_string())?;
            Err("child crashed".to_string())
        });
        let out = task.execute(&spawn_isolated("fail")).await;
        assert!(out.is_error);
        assert!(
            out.content.contains("child crashed") && out.content.contains("nothing was integrated"),
            "{}",
            out.content
        );
        let branch = out
            .content
            .split("branch '")
            .nth(1)
            .and_then(|r| r.split('\'').next())
            .expect("the branch is named")
            .to_owned();
        assert_eq!(git(p, &["show", &format!("{branch}:a.txt")]), "HALF");
        assert_eq!(
            std::fs::read_to_string(p.join("a.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
        assert!(is_empty_dir(state.path()));
    }

    #[tokio::test]
    async fn isolation_outside_a_git_repository_is_refused() {
        let plain = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let task = isolated_task(plain.path(), state.path(), |_| {
            panic!("the child must not start")
        });
        let out = task.execute(&spawn_isolated("x")).await;
        assert!(
            out.is_error && out.content.contains("git repository"),
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn isolation_is_refused_for_a_read_only_child_and_for_unknown_values() {
        let repo = repo_with_files();
        let state = tempfile::tempdir().unwrap();
        let task = isolated_task(repo.path(), state.path(), |_| {
            panic!("the child must not start")
        });
        let out = task
            .execute(&call(
                json!({"prompt": "x", "mode": "research", "isolation": "worktree"}),
            ))
            .await;
        assert!(
            out.is_error && out.content.contains("read-only"),
            "{}",
            out.content
        );
        let out = task
            .execute(&call(
                json!({"prompt": "x", "mode": "agent", "isolation": "vm"}),
            ))
            .await;
        assert!(
            out.is_error && out.content.contains("Valid options"),
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn isolation_none_runs_in_the_parents_tree() {
        let repo = repo_with_files();
        let state = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        let ran_in = Arc::new(std::sync::Mutex::new(std::path::PathBuf::new()));
        let seen = Arc::clone(&ran_in);
        let task = isolated_task(repo.path(), state.path(), move |d| {
            *seen.lock().unwrap() = d.to_path_buf();
            Ok(())
        });
        let out = task
            .execute(&call(
                json!({"prompt": "x", "mode": "agent", "isolation": "none"}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(ran_in.lock().unwrap().canonicalize().unwrap(), root);
    }

    #[test]
    fn the_schema_offers_isolation_as_an_optional_spawn_argument() {
        let schema = make_task().parameters_schema();
        assert_eq!(
            schema["properties"]["isolation"]["enum"],
            json!(["none", "worktree"])
        );
        assert!(make_task().description().contains("isolation=worktree"));
    }
}
