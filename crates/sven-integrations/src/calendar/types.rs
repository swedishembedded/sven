// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Calendar event types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Date range for querying calendar events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DateRange {
    /// Start of the range (inclusive).
    pub start: DateTime<Utc>,
    /// End of the range (inclusive).
    pub end: DateTime<Utc>,
}

/// A calendar event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEvent {
    /// Provider-specific event identifier.
    pub id: String,
    /// Event title / summary.
    pub title: String,
    /// Event description / notes.
    pub description: Option<String>,
    /// Location string.
    pub location: Option<String>,
    /// Event start time.
    pub start: DateTime<Utc>,
    /// Event end time.
    pub end: DateTime<Utc>,
    /// Whether this is an all-day event.
    pub all_day: bool,
    /// Calendar or calendar name this event belongs to.
    pub calendar: Option<String>,
    /// Organizer email.
    pub organizer: Option<String>,
    /// Attendee emails.
    pub attendees: Vec<String>,
}

impl CalendarEvent {
    /// Return a brief single-line summary of the event.
    pub fn summary_line(&self) -> String {
        let time = if self.all_day {
            self.start.format("%Y-%m-%d (all day)").to_string()
        } else {
            format!(
                "{} – {}",
                self.start.format("%Y-%m-%d %H:%M"),
                self.end.format("%H:%M UTC")
            )
        };
        format!("{} | {}", time, self.title)
    }
}

/// Parameters for creating a new calendar event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewEvent {
    /// Event title.
    pub title: String,
    /// Optional description.
    pub description: Option<String>,
    /// Optional location.
    pub location: Option<String>,
    /// Start time.
    pub start: DateTime<Utc>,
    /// End time.
    pub end: DateTime<Utc>,
    /// Mark as all-day event.
    pub all_day: bool,
    /// Attendee email addresses to invite.
    pub attendees: Vec<String>,
}

/// Fields that can be updated on an existing event.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventUpdate {
    pub title: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_event() -> CalendarEvent {
        CalendarEvent {
            id: "evt-1".to_string(),
            title: "Team Standup".to_string(),
            description: Some("Daily sync".to_string()),
            location: None,
            start: Utc.with_ymd_and_hms(2026, 3, 16, 9, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 3, 16, 9, 30, 0).unwrap(),
            all_day: false,
            calendar: Some("Work".to_string()),
            organizer: None,
            attendees: vec!["alice@example.com".to_string()],
        }
    }

    #[test]
    fn summary_line_formats_times() {
        let evt = sample_event();
        let line = evt.summary_line();
        assert!(line.contains("Team Standup"));
        assert!(line.contains("09:00"));
        assert!(line.contains("09:30"));
    }

    #[test]
    fn summary_line_all_day() {
        let mut evt = sample_event();
        evt.all_day = true;
        let line = evt.summary_line();
        assert!(line.contains("all day"));
    }

    #[test]
    fn calendar_event_roundtrips_json() {
        let evt = sample_event();
        let json = serde_json::to_string(&evt).unwrap();
        let decoded: CalendarEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.id, "evt-1");
        assert_eq!(decoded.title, "Team Standup");
    }

    #[test]
    fn event_update_default_has_none_fields() {
        let upd = EventUpdate::default();
        assert!(upd.title.is_none());
        assert!(upd.start.is_none());
    }

    #[test]
    fn date_range_roundtrips_json() {
        let range = DateRange {
            start: Utc.with_ymd_and_hms(2026, 3, 16, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 3, 17, 0, 0, 0).unwrap(),
        };
        let json = serde_json::to_string(&range).unwrap();
        let decoded: DateRange = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.start, range.start);
        assert_eq!(decoded.end, range.end);
    }
}
