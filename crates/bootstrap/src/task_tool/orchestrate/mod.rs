// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for splitting software work into
// independent, verifiable pieces that agents build in parallel for its
// clients. If your team needs expertise in agent orchestration then you can
// procure our services by sending an email to info@swedishembedded.com.
//! `plan_tasks`: the model submits a task graph, sven validates it and runs
//! it. Planning is an explicit, checked step; the running of the graph is
//! deterministic code that reuses the `task` tool's isolated writing
//! children and their serialized integration. The model gets one structured
//! result and is the only judge of what to do next: repair, or submit another
//! graph. A child's claim of success is never verification; the plan's
//! command is.

mod graph;
mod schedule;
mod spawner;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};
use sven_tool_api::ApprovalPolicy;
use sven_tool_api::PathScope;
use sven_tool_registry::ToolRegistry;
use sven_vocab::AgentMode;

pub use graph::{Limits, Plan, TaskSpec};
use schedule::{run_graph, Spawner, Status, TaskReport};
use spawner::TaskToolSpawner;

use super::TaskTool;
use crate::config::{Config, Orchestration};

/// Extra time beyond a child's own budget for its worktree to be integrated
/// before the controller gives up on it.
const INTEGRATION_GRACE: Duration = Duration::from_secs(120);
/// The longest verification output a result carries, in characters (its end,
/// where a test run says what failed).
const VERIFY_OUTPUT_CHARS: usize = 6000;

/// Registers `task` and, when `agent.orchestration` is `plan`, `plan_tasks`
/// beside it. A sub-agent never plans: it is not offered, and `task` refuses
/// it anyway.
pub(crate) fn register(
    registry: &mut ToolRegistry,
    task: Arc<TaskTool>,
    config: &Config,
    scope: &PathScope,
) {
    registry.register_arc(task.clone());
    if config.agent.orchestration != Orchestration::Plan || task.is_subagent {
        return;
    }
    let agent = &config.agent;
    registry.register(PlanTasksTool::new(
        Arc::new(TaskToolSpawner { task }),
        Arc::new(config.tools.shell_tool(scope.clone())),
        Limits {
            max_parallel: agent.orchestration_max_parallel.max(1),
            max_fanout: agent.orchestration_max_fanout.max(1),
            max_tasks: agent.orchestration_max_tasks.max(1),
            max_graphs: agent.orchestration_max_graphs,
        },
        agent.child_run_timeout().map(|t| t + INTEGRATION_GRACE),
    ));
}

/// The `plan_tasks` tool.
pub struct PlanTasksTool {
    spawner: Arc<dyn Spawner>,
    /// Runs the verification command: the session's shell, under its policy.
    verifier: Arc<dyn Tool>,
    limits: Limits,
    time_limit: Option<Duration>,
    graphs_run: AtomicU32,
}

impl PlanTasksTool {
    pub(crate) fn new(
        spawner: Arc<dyn Spawner>,
        verifier: Arc<dyn Tool>,
        limits: Limits,
        time_limit: Option<Duration>,
    ) -> Self {
        Self {
            spawner,
            verifier,
            limits,
            time_limit,
            graphs_run: AtomicU32::new(0),
        }
    }
}

/// The prompt of one child: everything it will know.
fn brief(plan: &Plan, task: &TaskSpec) -> String {
    let mut text = format!(
        "You are one worker in a parallel build. Your task `{}`: {}\n\n{}\n\n\
         You own these paths and only these: {}. Other workers are editing the rest of \
         the project at the same time, so do not change files outside your paths; if you \
         need a change elsewhere, say so in your report.\n",
        task.id,
        task.title,
        task.prompt,
        task.owns.join(", ")
    );
    if !task.acceptance.is_empty() {
        text.push_str(&format!("\nAcceptance criteria: {}\n", task.acceptance));
    }
    if !task.depends_on.is_empty() {
        text.push_str(&format!(
            "\nThese tasks are already integrated into your tree: {}.\n",
            task.depends_on.join(", ")
        ));
    }
    if let Some(command) = &plan.verify_command {
        text.push_str(&format!(
            "\nThe whole build is verified later with `{command}`; run the part of it that \
             concerns your paths before you finish.\n"
        ));
    }
    text.push_str("\nFinish with a short report: what you changed and how you checked it.");
    text
}

fn report_json(report: &TaskReport) -> Value {
    let outcome = &report.outcome;
    json!({
        "id": report.id,
        "title": report.title,
        "status": outcome.status.as_str(),
        "files_changed": outcome.files_changed,
        "branch": outcome.branch,
        "report": outcome.report,
        "tokens": outcome.tokens.map(|(input, output)| json!({"input": input, "output": output})),
        "wall_secs": (report.wall.as_secs_f64() * 10.0).round() / 10.0,
    })
}

fn tail(text: &str, chars: usize) -> String {
    let total = text.chars().count();
    if total <= chars {
        return text.to_string();
    }
    let kept: String = text.chars().skip(total - chars).collect();
    format!("[earlier output cut]\n{kept}")
}

fn refusal(call: &ToolCall, problems: &[String]) -> ToolOutput {
    let list: String = problems.iter().map(|p| format!("- {p}\n")).collect();
    ToolOutput::err(
        &call.id,
        format!(
            "The task graph was refused and nothing ran. Fix these and submit it again:\n{list}"
        ),
    )
}

#[async_trait]
impl Tool for PlanTasksTool {
    fn name(&self) -> &str {
        "plan_tasks"
    }

    fn summary(&self) -> &str {
        "Submit a task graph; independent tasks are built in parallel by isolated sub-agents."
    }

    fn description(&self) -> &str {
        "Split a large piece of work into tasks and have sub-agents build them in parallel.\n\
         Each task has an id, a self-contained 'prompt' (the sub-agent sees nothing else of this\n\
         conversation), the files or directories it alone writes ('owns'), 'acceptance' criteria\n\
         and the ids it 'depends_on'. Tasks with no ordering between them must own disjoint\n\
         files; otherwise order them with depends_on. sven validates the graph, refuses it with\n\
         the reasons if it breaks a rule, and otherwise runs it: ready tasks run at once, each in\n\
         its own git worktree, and every finished change is merged into your tree. You get one\n\
         result: per task integrated, conflict, failed or skipped (with the kept branch, files\n\
         changed and the sub-agent's report). A sub-agent's report is its own claim; set\n\
         'verify_command' (for example the test suite) and sven runs it after the graph and\n\
         returns its output. Then repair what is left yourself or submit a follow-up graph.\n\
         Use it for independent modules or files that can be built in parallel; do small tasks\n\
         directly. Sub-agents cannot plan or spawn."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "description": format!(
                        "At most {} tasks, at most {} runnable side by side",
                        self.limits.max_tasks, self.limits.max_fanout
                    ),
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string", "description": "Unique: letters, digits, _ - ."},
                            "title": {"type": "string"},
                            "prompt": {"type": "string", "description": "Complete, self-contained brief"},
                            "owns": {
                                "type": "array", "items": {"type": "string"},
                                "description": "Relative files, directories or globs only this task writes"
                            },
                            "acceptance": {"type": "string", "description": "How the task is known to be done"},
                            "depends_on": {
                                "type": "array", "items": {"type": "string"},
                                "description": "Ids that must be integrated first"
                            }
                        },
                        "required": ["id", "prompt", "owns"]
                    }
                },
                "verify_command": {
                    "type": "string",
                    "description": "Shell command run after the graph (tests, build); its output comes back"
                }
            },
            "required": ["tasks"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    /// It runs shell commands (the verification) and starts writing children.
    fn kernel_capability(&self) -> sven_tool_api::ToolCapability {
        sven_tool_api::ToolCapability::ExecuteShell
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let plan = match Plan::parse(&call.args) {
            Ok(plan) => plan,
            Err(problems) => return refusal(call, &problems),
        };
        if let Err(problems) = plan.validate(&self.limits) {
            return refusal(call, &problems);
        }
        if self.graphs_run.fetch_add(1, Ordering::SeqCst) >= self.limits.max_graphs {
            self.graphs_run.fetch_sub(1, Ordering::SeqCst);
            return ToolOutput::err(
                &call.id,
                format!(
                    "This session has already run {} task graphs, the most it may. Do the remaining work yourself.",
                    self.limits.max_graphs
                ),
            );
        }
        let reports = run_graph(
            &plan,
            &self.limits,
            self.spawner.as_ref(),
            &call.id,
            &|task| brief(&plan, task),
            self.time_limit,
        )
        .await;
        let verification = match &plan.verify_command {
            Some(command) => Some(self.verify(call, command).await),
            None => None,
        };
        ToolOutput::ok(&call.id, self.render(&reports, verification))
    }
}

impl PlanTasksTool {
    async fn verify(&self, call: &ToolCall, command: &str) -> Value {
        let out = self
            .verifier
            .execute(&ToolCall {
                id: call.id.clone(),
                name: "shell".into(),
                args: json!({
                    "shell_command": command,
                    "timeout_secs": sven_vocab::goal::DEFAULT_CHECK_TIMEOUT_SECS,
                    "output_mode": "tail",
                }),
            })
            .await;
        json!({
            "command": command,
            "passed": !out.is_error,
            "output": tail(&out.content, VERIFY_OUTPUT_CHARS),
        })
    }

    fn render(&self, reports: &[TaskReport], verification: Option<Value>) -> String {
        let count = |s: Status| reports.iter().filter(|r| r.outcome.status == s).count();
        let unfinished = reports.len() - count(Status::Integrated);
        let verified = verification.as_ref().map(|v| v["passed"] == true);
        let graphs_left = self
            .limits
            .max_graphs
            .saturating_sub(self.graphs_run.load(Ordering::SeqCst));
        let next = match (unfinished, verified) {
            (0, Some(true)) => "Every task is integrated and the verification command passes.".to_string(),
            (0, None) => "Every task is integrated. No verification command was set, so nothing here shows the work is correct: check it yourself.".to_string(),
            _ => format!(
                "Not everything is in your tree or verified. Repair it yourself, or submit a follow-up graph ({graphs_left} left). A conflict or failure keeps its work on the named branch."
            ),
        };
        let result = json!({
            "summary": format!(
                "{} integrated, {} conflict, {} failed, {} skipped",
                count(Status::Integrated), count(Status::Conflict),
                count(Status::Failed), count(Status::Skipped)
            ),
            "tasks": reports.iter().map(report_json).collect::<Vec<_>>(),
            "verification": verification,
            "graphs_left": graphs_left,
            "next": next,
        });
        serde_json::to_string_pretty(&result)
            .unwrap_or_else(|e| format!("result not encodable: {e}"))
    }
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
