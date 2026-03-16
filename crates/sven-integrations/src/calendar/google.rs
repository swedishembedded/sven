// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Google Calendar REST API backend (OAuth2).
//!
//! # Configuration
//! ```yaml
//! tools:
//!   calendar:
//!     backend: google
//!     oauth_client_id: "${GCAL_CLIENT_ID}"
//!     oauth_client_secret: "${GCAL_CLIENT_SECRET}"
//!     oauth_token_path: "~/.config/sven/gcal-token.json"
//! ```

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::debug;

use super::{CalendarEvent, CalendarProvider, DateRange, EventUpdate, NewEvent};

const GCAL_API: &str = "https://www.googleapis.com/calendar/v3";

#[derive(Debug, Serialize, Deserialize)]
struct GCalToken {
    access_token: String,
    refresh_token: Option<String>,
}

/// Google Calendar REST API provider.
pub struct GoogleCalendarProvider {
    token_path: PathBuf,
    /// Stored for OAuth2 token refresh flows.
    #[allow(dead_code)]
    client_id: String,
    /// Stored for OAuth2 token refresh flows.
    #[allow(dead_code)]
    client_secret: String,
    client: reqwest::Client,
}

impl GoogleCalendarProvider {
    /// Create a new Google Calendar provider.
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        token_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            token_path: token_path.into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Google Calendar HTTP client"),
        }
    }

    async fn access_token(&self) -> anyhow::Result<String> {
        let text = tokio::fs::read_to_string(&self.token_path)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Google Calendar token not found at {}. \
                 Run OAuth2 setup first: {e}",
                    self.token_path.display()
                )
            })?;
        let token: GCalToken = serde_json::from_str(&text)?;
        Ok(token.access_token)
    }
}

#[async_trait]
impl CalendarProvider for GoogleCalendarProvider {
    async fn list_events(&self, range: &DateRange) -> anyhow::Result<Vec<CalendarEvent>> {
        debug!(start = %range.start, end = %range.end, "Google Calendar: listing events");

        let token = self.access_token().await?;
        let url = format!(
            "{GCAL_API}/calendars/primary/events?timeMin={}&timeMax={}&singleEvents=true&orderBy=startTime",
            range.start.to_rfc3339(),
            range.end.to_rfc3339()
        );

        let resp: serde_json::Value = self
            .client
            .get(&url)
            .bearer_auth(token)
            .send()
            .await?
            .json()
            .await?;

        let items = resp["items"].as_array().cloned().unwrap_or_default();
        let mut events = Vec::new();

        for item in &items {
            if let Some(event) = parse_gcal_event(item) {
                events.push(event);
            }
        }

        Ok(events)
    }

    async fn create_event(&self, event: &NewEvent) -> anyhow::Result<CalendarEvent> {
        debug!(title = %event.title, "Google Calendar: creating event");

        let token = self.access_token().await?;
        let body = build_gcal_event_body(event);

        let resp: serde_json::Value = self
            .client
            .post(format!("{GCAL_API}/calendars/primary/events"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?
            .json()
            .await?;

        parse_gcal_event(&resp)
            .ok_or_else(|| anyhow::anyhow!("failed to parse created event response"))
    }

    async fn update_event(&self, id: &str, update: &EventUpdate) -> anyhow::Result<()> {
        debug!(id, "Google Calendar: updating event");

        let token = self.access_token().await?;

        // PATCH with only changed fields
        let mut patch = serde_json::Map::new();
        if let Some(title) = &update.title {
            patch.insert("summary".to_string(), serde_json::json!(title));
        }
        if let Some(desc) = &update.description {
            patch.insert("description".to_string(), serde_json::json!(desc));
        }
        if let Some(loc) = &update.location {
            patch.insert("location".to_string(), serde_json::json!(loc));
        }
        if let Some(start) = &update.start {
            patch.insert(
                "start".to_string(),
                serde_json::json!({ "dateTime": start.to_rfc3339() }),
            );
        }
        if let Some(end) = &update.end {
            patch.insert(
                "end".to_string(),
                serde_json::json!({ "dateTime": end.to_rfc3339() }),
            );
        }

        self.client
            .patch(format!("{GCAL_API}/calendars/primary/events/{id}"))
            .bearer_auth(token)
            .json(&serde_json::Value::Object(patch))
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    async fn delete_event(&self, id: &str) -> anyhow::Result<()> {
        debug!(id, "Google Calendar: deleting event");

        let token = self.access_token().await?;
        self.client
            .delete(format!("{GCAL_API}/calendars/primary/events/{id}"))
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }
}

fn parse_gcal_event(item: &serde_json::Value) -> Option<CalendarEvent> {
    let id = item["id"].as_str()?.to_string();
    let title = item["summary"].as_str().unwrap_or("(no title)").to_string();
    let description = item["description"].as_str().map(|s| s.to_string());
    let location = item["location"].as_str().map(|s| s.to_string());

    let (start, all_day) = parse_gcal_datetime(&item["start"])?;
    let (end, _) = parse_gcal_datetime(&item["end"])?;

    let organizer = item["organizer"]["email"].as_str().map(|s| s.to_string());
    let attendees = item["attendees"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a["email"].as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    Some(CalendarEvent {
        id,
        title,
        description,
        location,
        start,
        end,
        all_day,
        calendar: Some("primary".to_string()),
        organizer,
        attendees,
    })
}

fn parse_gcal_datetime(node: &serde_json::Value) -> Option<(DateTime<Utc>, bool)> {
    if let Some(dt_str) = node["dateTime"].as_str() {
        let dt = dt_str.parse::<DateTime<Utc>>().ok()?;
        return Some((dt, false));
    }
    if let Some(date_str) = node["date"].as_str() {
        // All-day event: parse as noon UTC to avoid timezone issues
        let dt = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
            .ok()?
            .and_hms_opt(12, 0, 0)?;
        return Some((DateTime::from_naive_utc_and_offset(dt, Utc), true));
    }
    None
}

fn build_gcal_event_body(event: &NewEvent) -> serde_json::Value {
    let mut body = serde_json::json!({
        "summary": event.title,
        "start": { "dateTime": event.start.to_rfc3339() },
        "end":   { "dateTime": event.end.to_rfc3339() }
    });

    if let Some(desc) = &event.description {
        body["description"] = serde_json::json!(desc);
    }
    if let Some(loc) = &event.location {
        body["location"] = serde_json::json!(loc);
    }
    if !event.attendees.is_empty() {
        body["attendees"] = serde_json::json!(event
            .attendees
            .iter()
            .map(|e| serde_json::json!({ "email": e }))
            .collect::<Vec<_>>());
    }

    body
}
