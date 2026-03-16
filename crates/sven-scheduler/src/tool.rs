// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `schedule` tool — lets the agent create, list, and delete cron/interval jobs.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use sven_tools::{
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolDisplay, ToolOutput},
};

use crate::{
    job::{Job, Schedule},
    store::JobStore,
};

/// Tool that lets the agent manage scheduled jobs.
///
/// # Actions
///
/// - `create` — create a new cron/interval/one-shot job
/// - `list` — list all scheduled jobs
/// - `delete` — remove a job by ID
/// - `enable` / `disable` — toggle a job without deleting it
/// - `run_now` — fire a job immediately (sends a due event)
///
/// # Example tool calls
///
/// ```json
/// { "action": "create", "name": "daily-briefing", "every": "1h",
///   "prompt": "Send a summary of today's news to Telegram chat 123." }
/// ```
///
/// ```json
/// { "action": "create", "name": "morning-email", "cron": "0 8 * * *",
///   "prompt": "Review unread emails and summarise the important ones." }
/// ```
pub struct ScheduleTool {
    store: Arc<JobStore>,
}

impl ScheduleTool {
    pub fn new(store: Arc<JobStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for ScheduleTool {
    fn name(&self) -> &str {
        "schedule"
    }

    fn description(&self) -> &str {
        "Manage scheduled agent jobs: create recurring tasks (cron or interval), \
         list existing jobs, delete or toggle them.\n\
         Use this to automate daily briefings, monitoring, email responses, \
         content creation, and any other periodic or time-triggered workflows.\n\
         Actions: create | list | delete | enable | disable"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "delete", "enable", "disable"],
                    "description": "Operation to perform"
                },
                "name": {
                    "type": "string",
                    "description": "(create) Human-readable name for the job"
                },
                "prompt": {
                    "type": "string",
                    "description": "(create) Prompt sent to the agent when the job fires"
                },
                "every": {
                    "type": "string",
                    "description": "(create, interval schedule) Duration string, e.g. '30m', '1h', '24h'"
                },
                "cron": {
                    "type": "string",
                    "description": "(create, cron schedule) 5-field cron expression, e.g. '0 8 * * *' for daily at 08:00 UTC"
                },
                "at": {
                    "type": "string",
                    "description": "(create, one-shot) ISO 8601 UTC datetime, e.g. '2026-04-01T09:00:00Z'"
                },
                "deliver_to": {
                    "type": "string",
                    "description": "(create, optional) Channel and recipient for job output, e.g. 'telegram:123456'"
                },
                "isolated": {
                    "type": "boolean",
                    "description": "(create) Run in isolated session instead of main session. Default: false"
                },
                "job_id": {
                    "type": "string",
                    "description": "(delete/enable/disable) UUID of the job to act on"
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let action = match call.args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolOutput::err(&call.id, "missing 'action'"),
        };

        match action {
            "create" => self.create(call).await,
            "list" => self.list(call).await,
            "delete" => self.delete(call).await,
            "enable" => self.set_enabled(call, true).await,
            "disable" => self.set_enabled(call, false).await,
            other => ToolOutput::err(
                &call.id,
                format!("unknown action {other:?}; expected create|list|delete|enable|disable"),
            ),
        }
    }
}

impl ScheduleTool {
    async fn create(&self, call: &ToolCall) -> ToolOutput {
        let name = match call.args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => return ToolOutput::err(&call.id, "create requires 'name'"),
        };
        let prompt = match call.args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "create requires 'prompt'"),
        };

        let schedule = if let Some(every) = call.args.get("every").and_then(|v| v.as_str()) {
            // Validate the interval string
            if humantime::parse_duration(every).is_err() {
                return ToolOutput::err(
                    &call.id,
                    format!("invalid interval {every:?} — use units like '30m', '1h', '24h'"),
                );
            }
            Schedule::Interval {
                every: every.to_string(),
            }
        } else if let Some(expr) = call.args.get("cron").and_then(|v| v.as_str()) {
            if expr.parse::<cron::Schedule>().is_err() {
                return ToolOutput::err(&call.id, format!("invalid cron expression {expr:?}"));
            }
            Schedule::Cron {
                expr: expr.to_string(),
                timezone: None,
            }
        } else if let Some(at_str) = call.args.get("at").and_then(|v| v.as_str()) {
            match at_str.parse::<chrono::DateTime<chrono::Utc>>() {
                Ok(at) => Schedule::Once { at },
                Err(e) => {
                    return ToolOutput::err(&call.id, format!("invalid datetime {at_str:?}: {e}"))
                }
            }
        } else {
            return ToolOutput::err(
                &call.id,
                "create requires one of: 'every' (interval), 'cron' (cron expression), or 'at' (one-shot datetime)",
            );
        };

        let mut job = Job::new(name.clone(), schedule, prompt);
        job.deliver_to = call
            .args
            .get("deliver_to")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        job.isolated = call
            .args
            .get("isolated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        match self.store.add(job).await {
            Ok(id) => ToolOutput::ok(&call.id, format!("Job '{name}' created with ID {id}.")),
            Err(e) => ToolOutput::err(&call.id, format!("failed to create job: {e}")),
        }
    }

    async fn list(&self, call: &ToolCall) -> ToolOutput {
        let jobs = self.store.list().await;
        if jobs.is_empty() {
            return ToolOutput::ok(&call.id, "No scheduled jobs.");
        }

        let mut lines = vec!["Scheduled jobs:".to_string()];
        for job in &jobs {
            let status = if job.enabled { "enabled" } else { "disabled" };
            let next = job
                .next_run
                .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "no next run".to_string());
            lines.push(format!(
                "- {} [{}] | ID: {} | next: {} | {}",
                job.name, status, job.id, next, job.prompt
            ));
        }

        ToolOutput::ok(&call.id, lines.join("\n"))
    }

    async fn delete(&self, call: &ToolCall) -> ToolOutput {
        let id = match parse_job_id(call) {
            Ok(id) => id,
            Err(e) => return ToolOutput::err(&call.id, e),
        };

        match self.store.remove(id).await {
            Ok(true) => ToolOutput::ok(&call.id, format!("Job {id} deleted.")),
            Ok(false) => ToolOutput::err(&call.id, format!("No job with ID {id}.")),
            Err(e) => ToolOutput::err(&call.id, format!("delete failed: {e}")),
        }
    }

    async fn set_enabled(&self, call: &ToolCall, enabled: bool) -> ToolOutput {
        let id = match parse_job_id(call) {
            Ok(id) => id,
            Err(e) => return ToolOutput::err(&call.id, e),
        };

        match self.store.set_enabled(id, enabled).await {
            Ok(true) => ToolOutput::ok(
                &call.id,
                format!("Job {id} {}.", if enabled { "enabled" } else { "disabled" }),
            ),
            Ok(false) => ToolOutput::err(&call.id, format!("No job with ID {id}.")),
            Err(e) => ToolOutput::err(&call.id, format!("set_enabled failed: {e}")),
        }
    }
}

fn parse_job_id(call: &ToolCall) -> Result<uuid::Uuid, String> {
    let s = call
        .args
        .get("job_id")
        .and_then(|v| v.as_str())
        .ok_or("missing 'job_id'")?;
    s.parse::<uuid::Uuid>()
        .map_err(|e| format!("invalid job_id {s:?}: {e}"))
}

impl ToolDisplay for ScheduleTool {
    fn display_name(&self) -> &str {
        "Schedule"
    }
    fn icon(&self) -> &str {
        "⏰"
    }
    fn category(&self) -> &str {
        "automation"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("?");
        let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() {
            action.to_string()
        } else {
            format!("{action} '{name}'")
        }
    }
}
