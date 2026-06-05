//! Checkpoint / rollback effect executor.
//!
//! Handles [`Effect::CreateCheckpoint`] and [`Effect::RollbackToCheckpoint`]
//! by running `git stash` commands in the injected repository directory.
//!
//! `CreateCheckpoint { label }` → `git stash push --include-untracked -m <label>`
//! `RollbackToCheckpoint { label }` → finds the stash entry whose message
//!   matches `label` and applies it (destructive: drops the stash entry).

use std::path::PathBuf;

use async_trait::async_trait;
use sven_hsm::{Effect, EffectExecutor, Event, EventSink, ObservationSink};

/// Executes checkpoint effects using `git stash` in `repo_dir`.
pub struct CheckpointExecutor {
    repo_dir: PathBuf,
}

impl CheckpointExecutor {
    /// Creates an executor that operates in `repo_dir`.
    pub fn new(repo_dir: impl Into<PathBuf>) -> Self {
        Self {
            repo_dir: repo_dir.into(),
        }
    }
}

#[async_trait]
impl EffectExecutor for CheckpointExecutor {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
        match effect {
            Effect::CreateCheckpoint { label } => {
                self.create_checkpoint(&label, sink).await;
            }
            Effect::RollbackToCheckpoint { label } => {
                self.rollback_checkpoint(&label, sink).await;
            }
            _ => {}
        }
    }
}

impl CheckpointExecutor {
    async fn create_checkpoint(&self, label: &str, sink: &EventSink) {
        tracing::info!(
            label,
            "CheckpointExecutor: creating checkpoint via git stash"
        );
        let output = tokio::process::Command::new("git")
            .args(["stash", "push", "--include-untracked", "-m", label])
            .current_dir(&self.repo_dir)
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => {
                tracing::info!(label, "CheckpointExecutor: checkpoint created");
                // Emit an internal signal so the machine can update its
                // checkpoint list.
                let _ = sink
                    .emit(Event::Internal(sven_hsm::event::InternalEvent::Custom {
                        name: "checkpoint_created".into(),
                        payload: serde_json::json!({ "label": label }),
                    }))
                    .await;
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                tracing::warn!(label, %stderr, "CheckpointExecutor: git stash push failed");
                let _ = sink
                    .emit(Event::Internal(sven_hsm::event::InternalEvent::Custom {
                        name: "checkpoint_failed".into(),
                        payload: serde_json::json!({ "label": label, "error": stderr.as_ref() }),
                    }))
                    .await;
            }
            Err(e) => {
                tracing::warn!(label, error = %e, "CheckpointExecutor: failed to spawn git");
                let _ = sink
                    .emit(Event::Internal(sven_hsm::event::InternalEvent::Custom {
                        name: "checkpoint_failed".into(),
                        payload: serde_json::json!({ "label": label, "error": e.to_string() }),
                    }))
                    .await;
            }
        }
    }

    async fn rollback_checkpoint(&self, label: &str, sink: &EventSink) {
        tracing::info!(label, "CheckpointExecutor: rolling back to checkpoint");

        // First, list stashes to find the one with the matching label.
        let list_output = tokio::process::Command::new("git")
            .args(["stash", "list", "--format=%gd %s"])
            .current_dir(&self.repo_dir)
            .output()
            .await;

        let stash_ref = match list_output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                find_stash_ref_for_label(&stdout, label)
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                tracing::warn!(%stderr, "CheckpointExecutor: git stash list failed");
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "CheckpointExecutor: failed to spawn git stash list");
                None
            }
        };

        let Some(stash_ref) = stash_ref else {
            tracing::warn!(
                label,
                "CheckpointExecutor: no stash found with matching label"
            );
            let _ = sink
                .emit(Event::Internal(sven_hsm::event::InternalEvent::Custom {
                    name: "rollback_failed".into(),
                    payload: serde_json::json!({ "label": label, "error": "stash not found" }),
                }))
                .await;
            return;
        };

        // Apply (pop) the stash.
        let pop_output = tokio::process::Command::new("git")
            .args(["stash", "pop", &stash_ref])
            .current_dir(&self.repo_dir)
            .output()
            .await;

        match pop_output {
            Ok(out) if out.status.success() => {
                tracing::info!(label, "CheckpointExecutor: rollback complete");
                let _ = sink
                    .emit(Event::Internal(sven_hsm::event::InternalEvent::Custom {
                        name: "rollback_complete".into(),
                        payload: serde_json::json!({ "label": label }),
                    }))
                    .await;
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                tracing::warn!(label, %stderr, "CheckpointExecutor: git stash pop failed");
                let _ = sink
                    .emit(Event::Internal(sven_hsm::event::InternalEvent::Custom {
                        name: "rollback_failed".into(),
                        payload: serde_json::json!({ "label": label, "error": stderr.as_ref() }),
                    }))
                    .await;
            }
            Err(e) => {
                tracing::warn!(label, error = %e, "CheckpointExecutor: failed to spawn git stash pop");
                let _ = sink
                    .emit(Event::Internal(sven_hsm::event::InternalEvent::Custom {
                        name: "rollback_failed".into(),
                        payload: serde_json::json!({ "label": label, "error": e.to_string() }),
                    }))
                    .await;
            }
        }
    }
}

/// Finds the stash ref (e.g. `stash@{0}`) whose subject line ends with `label`.
///
/// `git stash list --format=%gd %s` produces lines like:
/// ```text
/// stash@{0} On main: my-checkpoint-label
/// stash@{1} On main: another-label
/// ```
fn find_stash_ref_for_label(list_output: &str, label: &str) -> Option<String> {
    for line in list_output.lines() {
        let mut parts = line.splitn(2, ' ');
        let stash_ref = parts.next()?;
        let subject = parts.next().unwrap_or("");
        if subject.ends_with(label) {
            return Some(stash_ref.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::process::Command as StdCmd;

    use sven_hsm::{
        Context, Effect, EffectExecutor, Event, EventSink, Hsm, MachineId, ObservationSink,
        PermissionPolicy, Reaction, Runtime,
    };
    use tempfile::TempDir;

    use super::CheckpointExecutor;

    fn init_git_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        let path = dir.path();
        StdCmd::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .unwrap();
        StdCmd::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .unwrap();
        StdCmd::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .unwrap();
        // Initial commit so stash has a base
        std::fs::write(path.join("README"), "hello").unwrap();
        StdCmd::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .unwrap();
        StdCmd::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(path)
            .output()
            .unwrap();
        dir
    }

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    enum TS {
        Top,
        Idle,
        Done,
    }
    struct OneShotMachine(MachineId);
    impl OneShotMachine {
        fn new() -> Self {
            Self(MachineId::new())
        }
    }
    impl sven_hsm::Machine for OneShotMachine {
        type State = TS;
        fn id(&self) -> MachineId {
            self.0
        }
        fn top(&self) -> TS {
            TS::Top
        }
        fn initial(&self) -> TS {
            TS::Idle
        }
        fn superstate(&self, s: TS) -> TS {
            match s {
                TS::Top => TS::Top,
                _ => TS::Top,
            }
        }
        fn is_terminal(&self, s: TS) -> bool {
            s == TS::Done
        }
        fn dispatch_state(&mut self, s: TS, e: &Event, ctx: &mut Context) -> Reaction<TS> {
            match s {
                TS::Top => Reaction::Handled(vec![]),
                TS::Idle => {
                    if e.is_lifecycle() {
                        return Reaction::Handled(vec![]);
                    }
                    ctx.set_fact("received", "true");
                    Reaction::Transition {
                        target: TS::Done,
                        effects: vec![],
                        rationale: "got event".into(),
                    }
                }
                TS::Done => Reaction::Handled(vec![]),
            }
        }
    }

    struct NoOpExec;
    #[async_trait::async_trait]
    impl EffectExecutor for NoOpExec {
        async fn execute(&mut self, _: Effect, _: &EventSink, _: &ObservationSink) {}
    }

    async fn run_ckpt_effect(exec: &mut CheckpointExecutor, effect: Effect) -> bool {
        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();
        exec.execute(effect, &sink, &sven_hsm::ObservationSink::default()).await;
        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        report.ctx.fact("received").is_some()
    }

    #[tokio::test]
    async fn create_checkpoint_emits_internal_event() {
        let dir = init_git_repo();
        // Make an uncommitted change to stash
        std::fs::write(dir.path().join("newfile.txt"), "content").unwrap();
        let mut exec = CheckpointExecutor::new(dir.path());
        let effect = Effect::CreateCheckpoint {
            label: "cp-test-1".into(),
        };
        assert!(run_ckpt_effect(&mut exec, effect).await);
    }

    #[tokio::test]
    async fn rollback_with_nonexistent_label_emits_rollback_failed() {
        let dir = init_git_repo();
        let mut exec = CheckpointExecutor::new(dir.path());
        let effect = Effect::RollbackToCheckpoint {
            label: "nonexistent-label-xyz".into(),
        };
        assert!(run_ckpt_effect(&mut exec, effect).await);
    }

    #[test]
    fn find_stash_ref_matches_label() {
        let output = "stash@{0} On main: my-label\nstash@{1} On main: other-label\n";
        let r = super::find_stash_ref_for_label(output, "my-label");
        assert_eq!(r.as_deref(), Some("stash@{0}"));
    }

    #[test]
    fn find_stash_ref_returns_none_when_no_match() {
        let output = "stash@{0} On main: some-label\n";
        let r = super::find_stash_ref_for_label(output, "missing");
        assert!(r.is_none());
    }
}
