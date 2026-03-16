// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Calendar integration — CalDAV and Google Calendar.

pub mod caldav;
pub mod google;
pub mod tool;
pub mod types;

pub use caldav::CalDavProvider;
pub use google::GoogleCalendarProvider;
pub use tool::CalendarTool;
pub use types::{CalendarEvent, DateRange, EventUpdate, NewEvent};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Unified calendar provider trait.
#[async_trait]
pub trait CalendarProvider: Send + Sync {
    /// List events in a date range.
    async fn list_events(&self, range: &DateRange) -> anyhow::Result<Vec<CalendarEvent>>;

    /// Create a new event.
    async fn create_event(&self, event: &NewEvent) -> anyhow::Result<CalendarEvent>;

    /// Update an existing event.
    async fn update_event(&self, id: &str, update: &EventUpdate) -> anyhow::Result<()>;

    /// Delete an event.
    async fn delete_event(&self, id: &str) -> anyhow::Result<()>;

    /// List events for today.
    async fn today(&self) -> anyhow::Result<Vec<CalendarEvent>> {
        let now = Utc::now();
        let start = now.date_naive().and_hms_opt(0, 0, 0).unwrap();
        let end = now.date_naive().and_hms_opt(23, 59, 59).unwrap();
        self.list_events(&DateRange {
            start: DateTime::from_naive_utc_and_offset(start, Utc),
            end: DateTime::from_naive_utc_and_offset(end, Utc),
        })
        .await
    }

    /// List events for the next N days.
    async fn upcoming(&self, days: u32) -> anyhow::Result<Vec<CalendarEvent>> {
        let now = Utc::now();
        let end = now + chrono::Duration::days(days as i64);
        self.list_events(&DateRange { start: now, end }).await
    }
}
