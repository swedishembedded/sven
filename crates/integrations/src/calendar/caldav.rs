// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! CalDAV calendar backend.
//!
//! Connects to any CalDAV server (Nextcloud, Radicale, iCloud, Fastmail, etc.)
//! using HTTP REPORT and PUT/DELETE requests. iCalendar (RFC 5545) is used
//! for event serialization.
//!
//! # Configuration
//! ```yaml
//! tools:
//!   calendar:
//!     backend: caldav
//!     url: "https://nextcloud.example.com/remote.php/dav/calendars/user/personal"
//!     username: "${CALDAV_USER}"
//!     password: "${CALDAV_PASSWORD}"
//! ```

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tracing::debug;

use super::{CalendarEvent, CalendarProvider, DateRange, EventUpdate, NewEvent};

/// CalDAV calendar provider.
pub struct CalDavProvider {
    calendar_url: String,
    username: String,
    password: String,
    client: reqwest::Client,
}

impl CalDavProvider {
    /// Create a new CalDAV provider.
    pub fn new(
        calendar_url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            calendar_url: calendar_url.into().trim_end_matches('/').to_string(),
            username: username.into(),
            password: password.into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("CalDAV HTTP client"),
        }
    }

    async fn report_calendar(&self, range: &DateRange) -> anyhow::Result<String> {
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><d:getetag/><c:calendar-data/></d:prop>
  <c:filter>
    <c:comp-filter name="VCALENDAR">
      <c:comp-filter name="VEVENT">
        <c:time-range start="{}" end="{}"/>
      </c:comp-filter>
    </c:comp-filter>
  </c:filter>
</c:calendar-query>"#,
            range.start.format("%Y%m%dT%H%M%SZ"),
            range.end.format("%Y%m%dT%H%M%SZ")
        );

        let resp = self
            .client
            .request(
                reqwest::Method::from_bytes(b"REPORT").unwrap(),
                &self.calendar_url,
            )
            .basic_auth(&self.username, Some(&self.password))
            .header("Content-Type", "application/xml; charset=utf-8")
            .header("Depth", "1")
            .body(body)
            .send()
            .await?;

        Ok(resp.text().await?)
    }
}

#[async_trait]
impl CalendarProvider for CalDavProvider {
    async fn list_events(&self, range: &DateRange) -> anyhow::Result<Vec<CalendarEvent>> {
        debug!(start = %range.start, end = %range.end, "CalDAV: listing events");

        let xml = self.report_calendar(range).await?;
        let events = parse_caldav_response(&xml);
        Ok(events)
    }

    async fn create_event(&self, event: &NewEvent) -> anyhow::Result<CalendarEvent> {
        debug!(title = %event.title, "CalDAV: creating event");

        let uid = uuid::Uuid::new_v4();
        let ical = build_ical(event, &uid.to_string());
        let url = format!("{}/{}.ics", self.calendar_url, uid);

        self.client
            .put(&url)
            .basic_auth(&self.username, Some(&self.password))
            .header("Content-Type", "text/calendar; charset=utf-8")
            .body(ical)
            .send()
            .await?
            .error_for_status()?;

        Ok(CalendarEvent {
            id: uid.to_string(),
            title: event.title.clone(),
            description: event.description.clone(),
            location: event.location.clone(),
            start: event.start,
            end: event.end,
            all_day: event.all_day,
            calendar: None,
            organizer: None,
            attendees: event.attendees.clone(),
        })
    }

    async fn update_event(&self, id: &str, update: &EventUpdate) -> anyhow::Result<()> {
        debug!(id, "CalDAV: updating event (full rewrite)");
        // CalDAV update: GET current, modify, PUT back
        let url = format!("{}/{}.ics", self.calendar_url, id);
        let current_ical = self
            .client
            .get(&url)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await?
            .text()
            .await?;

        let updated = apply_ical_update(&current_ical, update);

        self.client
            .put(&url)
            .basic_auth(&self.username, Some(&self.password))
            .header("Content-Type", "text/calendar; charset=utf-8")
            .body(updated)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    async fn delete_event(&self, id: &str) -> anyhow::Result<()> {
        debug!(id, "CalDAV: deleting event");
        let url = format!("{}/{}.ics", self.calendar_url, id);
        self.client
            .delete(&url)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

fn build_ical(event: &NewEvent, uid: &str) -> String {
    let now = Utc::now();
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//sven-agent//EN".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:{uid}"),
        format!("DTSTAMP:{}", now.format("%Y%m%dT%H%M%SZ")),
        format!("DTSTART:{}", event.start.format("%Y%m%dT%H%M%SZ")),
        format!("DTEND:{}", event.end.format("%Y%m%dT%H%M%SZ")),
        format!("SUMMARY:{}", ical_escape(&event.title)),
    ];

    if let Some(desc) = &event.description {
        lines.push(format!("DESCRIPTION:{}", ical_escape(desc)));
    }
    if let Some(loc) = &event.location {
        lines.push(format!("LOCATION:{}", ical_escape(loc)));
    }
    for attendee in &event.attendees {
        lines.push(format!("ATTENDEE;CN={attendee}:mailto:{attendee}"));
    }

    lines.push("END:VEVENT".to_string());
    lines.push("END:VCALENDAR".to_string());
    lines.join("\r\n")
}

fn ical_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

fn parse_caldav_response(xml: &str) -> Vec<CalendarEvent> {
    // Very basic iCalendar parser from XML CDATA
    let mut events = Vec::new();

    for cal_data in extract_calendar_data_blocks(xml) {
        if let Some(event) = parse_vevent(&cal_data) {
            events.push(event);
        }
    }

    events
}

fn extract_calendar_data_blocks(xml: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let tag_start = "<cal:calendar-data>";
    let tag_end = "</cal:calendar-data>";

    let mut pos = 0;
    while let Some(start) = xml[pos..].find(tag_start) {
        let abs_start = pos + start + tag_start.len();
        if let Some(end_rel) = xml[abs_start..].find(tag_end) {
            blocks.push(xml[abs_start..abs_start + end_rel].to_string());
            pos = abs_start + end_rel + tag_end.len();
        } else {
            break;
        }
    }

    blocks
}

fn parse_vevent(ical: &str) -> Option<CalendarEvent> {
    let mut in_vevent = false;
    let mut uid = String::new();
    let mut summary = String::new();
    let mut description = None;
    let mut location = None;
    let mut dtstart: Option<DateTime<Utc>> = None;
    let mut dtend: Option<DateTime<Utc>> = None;
    let mut attendees = Vec::new();

    for line in ical.lines() {
        let line = line.trim();
        if line == "BEGIN:VEVENT" {
            in_vevent = true;
            continue;
        }
        if line == "END:VEVENT" {
            break;
        }
        if !in_vevent {
            continue;
        }

        if let Some(val) = line.strip_prefix("UID:") {
            uid = val.to_string();
        } else if let Some(val) = line.strip_prefix("SUMMARY:") {
            summary = ical_unescape(val);
        } else if let Some(val) = line.strip_prefix("DESCRIPTION:") {
            description = Some(ical_unescape(val));
        } else if let Some(val) = line.strip_prefix("LOCATION:") {
            location = Some(ical_unescape(val));
        } else if line.starts_with("DTSTART") {
            dtstart = parse_ical_datetime(line);
        } else if line.starts_with("DTEND") {
            dtend = parse_ical_datetime(line);
        } else if line.starts_with("ATTENDEE") {
            if let Some(email) = extract_mailto(line) {
                attendees.push(email);
            }
        }
    }

    if uid.is_empty() || summary.is_empty() {
        return None;
    }

    Some(CalendarEvent {
        id: uid,
        title: summary,
        description,
        location,
        start: dtstart?,
        end: dtend?,
        all_day: false,
        calendar: None,
        organizer: None,
        attendees,
    })
}

fn ical_unescape(s: &str) -> String {
    s.replace("\\n", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

fn parse_ical_datetime(line: &str) -> Option<DateTime<Utc>> {
    let val = line.split(':').next_back()?.trim();
    // Try YYYYMMDDTHHMMSSZ format
    chrono::NaiveDateTime::parse_from_str(val, "%Y%m%dT%H%M%SZ")
        .ok()
        .map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc))
}

fn extract_mailto(line: &str) -> Option<String> {
    line.to_lowercase()
        .find("mailto:")
        .map(|pos| line[pos + 7..].to_string())
}

fn apply_ical_update(ical: &str, update: &EventUpdate) -> String {
    let mut lines: Vec<String> = ical.lines().map(|l| l.to_string()).collect();
    let now = Utc::now();

    for line in lines.iter_mut() {
        let lower = line.to_lowercase();
        if let Some(new_title) = &update.title {
            if lower.starts_with("summary:") {
                *line = format!("SUMMARY:{}", ical_escape(new_title));
                continue;
            }
        }
        if let Some(new_desc) = &update.description {
            if lower.starts_with("description:") {
                *line = format!("DESCRIPTION:{}", ical_escape(new_desc));
                continue;
            }
        }
        if let Some(new_loc) = &update.location {
            if lower.starts_with("location:") {
                *line = format!("LOCATION:{}", ical_escape(new_loc));
                continue;
            }
        }
        if let Some(new_start) = &update.start {
            if lower.starts_with("dtstart") {
                *line = format!("DTSTART:{}", new_start.format("%Y%m%dT%H%M%SZ"));
                continue;
            }
        }
        if let Some(new_end) = &update.end {
            if lower.starts_with("dtend") {
                *line = format!("DTEND:{}", new_end.format("%Y%m%dT%H%M%SZ"));
                continue;
            }
        }
        if lower.starts_with("dtstamp:") {
            *line = format!("DTSTAMP:{}", now.format("%Y%m%dT%H%M%SZ"));
        }
    }

    lines.join("\r\n")
}
