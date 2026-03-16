// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `calendar` tool — agent access to calendar events.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use sven_tools::{
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolDisplay, ToolOutput},
};

use super::{CalendarProvider, DateRange, EventUpdate, NewEvent};

/// Tool providing the agent with calendar access.
///
/// # Actions
///
/// - `today` — list today's events
/// - `upcoming` — list events in the next N days
/// - `list` — list events in a specific date range
/// - `create` — create a new event
/// - `update` — update an existing event
/// - `delete` — delete an event
pub struct CalendarTool {
    provider: Arc<dyn CalendarProvider>,
}

impl CalendarTool {
    pub fn new(provider: Arc<dyn CalendarProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Tool for CalendarTool {
    fn name(&self) -> &str {
        "calendar"
    }

    fn description(&self) -> &str {
        "Access and manage calendar events via the configured calendar provider.\n\
         Actions: today | upcoming | list | create | update | delete\n\
         Use today/upcoming to check schedule, create for booking meetings, \
         update/delete for modifications."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["today", "upcoming", "list", "create", "update", "delete"],
                    "description": "Calendar operation to perform"
                },
                "days": {
                    "type": "integer",
                    "description": "(upcoming) Number of days ahead to check. Default: 7"
                },
                "start": {
                    "type": "string",
                    "description": "(list/create) ISO 8601 UTC datetime, e.g. '2026-04-01T09:00:00Z'"
                },
                "end": {
                    "type": "string",
                    "description": "(list/create) ISO 8601 UTC datetime"
                },
                "title": {
                    "type": "string",
                    "description": "(create/update) Event title"
                },
                "description": {
                    "type": "string",
                    "description": "(create/update) Event description or notes"
                },
                "location": {
                    "type": "string",
                    "description": "(create/update) Event location"
                },
                "attendees": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "(create) Attendee email addresses to invite"
                },
                "id": {
                    "type": "string",
                    "description": "(update/delete) Event ID"
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
            "today" => self.today(call).await,
            "upcoming" => self.upcoming(call).await,
            "list" => self.list(call).await,
            "create" => self.create(call).await,
            "update" => self.update(call).await,
            "delete" => self.delete(call).await,
            other => ToolOutput::err(
                &call.id,
                format!(
                    "unknown action {other:?}; expected today|upcoming|list|create|update|delete"
                ),
            ),
        }
    }
}

impl CalendarTool {
    async fn today(&self, call: &ToolCall) -> ToolOutput {
        match self.provider.today().await {
            Ok(events) => format_events(&call.id, &events, "Today's events"),
            Err(e) => ToolOutput::err(&call.id, format!("calendar today failed: {e}")),
        }
    }

    async fn upcoming(&self, call: &ToolCall) -> ToolOutput {
        let days = call.args.get("days").and_then(|v| v.as_u64()).unwrap_or(7) as u32;

        match self.provider.upcoming(days).await {
            Ok(events) => format_events(
                &call.id,
                &events,
                &format!("Events in the next {days} days"),
            ),
            Err(e) => ToolOutput::err(&call.id, format!("calendar upcoming failed: {e}")),
        }
    }

    async fn list(&self, call: &ToolCall) -> ToolOutput {
        let start = match parse_datetime(call, "start") {
            Ok(dt) => dt,
            Err(e) => return ToolOutput::err(&call.id, e),
        };
        let end = match parse_datetime(call, "end") {
            Ok(dt) => dt,
            Err(e) => return ToolOutput::err(&call.id, e),
        };

        match self.provider.list_events(&DateRange { start, end }).await {
            Ok(events) => format_events(&call.id, &events, "Events"),
            Err(e) => ToolOutput::err(&call.id, format!("calendar list failed: {e}")),
        }
    }

    async fn create(&self, call: &ToolCall) -> ToolOutput {
        let title = match call.args.get("title").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return ToolOutput::err(&call.id, "create requires 'title'"),
        };
        let start = match parse_datetime(call, "start") {
            Ok(dt) => dt,
            Err(e) => return ToolOutput::err(&call.id, e),
        };
        let end = match parse_datetime(call, "end") {
            Ok(dt) => dt,
            Err(e) => return ToolOutput::err(&call.id, e),
        };

        let attendees = call
            .args
            .get("attendees")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let event = NewEvent {
            title,
            description: call
                .args
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            location: call
                .args
                .get("location")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            start,
            end,
            all_day: false,
            attendees,
        };

        match self.provider.create_event(&event).await {
            Ok(created) => ToolOutput::ok(
                &call.id,
                format!(
                    "Event created: ID={} | {}",
                    created.id,
                    created.summary_line()
                ),
            ),
            Err(e) => ToolOutput::err(&call.id, format!("calendar create failed: {e}")),
        }
    }

    async fn update(&self, call: &ToolCall) -> ToolOutput {
        let id = match call.args.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return ToolOutput::err(&call.id, "update requires 'id'"),
        };

        let update = EventUpdate {
            title: call
                .args
                .get("title")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            description: call
                .args
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            location: call
                .args
                .get("location")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            start: call
                .args
                .get("start")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok()),
            end: call
                .args
                .get("end")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok()),
        };

        match self.provider.update_event(&id, &update).await {
            Ok(()) => ToolOutput::ok(&call.id, format!("Event {id} updated.")),
            Err(e) => ToolOutput::err(&call.id, format!("calendar update failed: {e}")),
        }
    }

    async fn delete(&self, call: &ToolCall) -> ToolOutput {
        let id = match call.args.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return ToolOutput::err(&call.id, "delete requires 'id'"),
        };

        match self.provider.delete_event(&id).await {
            Ok(()) => ToolOutput::ok(&call.id, format!("Event {id} deleted.")),
            Err(e) => ToolOutput::err(&call.id, format!("calendar delete failed: {e}")),
        }
    }
}

fn format_events(
    call_id: &str,
    events: &[super::types::CalendarEvent],
    header: &str,
) -> ToolOutput {
    if events.is_empty() {
        return ToolOutput::ok(call_id, format!("{header}: none."));
    }
    let mut lines = vec![format!("{header}:")];
    for event in events {
        lines.push(format!("  - {} | ID: {}", event.summary_line(), event.id));
    }
    ToolOutput::ok(call_id, lines.join("\n"))
}

fn parse_datetime(call: &ToolCall, field: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    let s = call
        .args
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing '{field}'"))?;
    s.parse::<chrono::DateTime<chrono::Utc>>()
        .map_err(|e| format!("invalid datetime for '{field}': {e}"))
}

impl ToolDisplay for CalendarTool {
    fn display_name(&self) -> &str {
        "Calendar"
    }
    fn icon(&self) -> &str {
        "📅"
    }
    fn category(&self) -> &str {
        "integrations"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        args.get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string()
    }
}
