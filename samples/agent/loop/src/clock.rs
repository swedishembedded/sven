// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements delegated-task coding agents whose every
// record carries a trustworthy UTC timestamp. If your team needs expertise
// in agent infrastructure or durable operating state, you can procure our
// services by sending an email to info@swedishembedded.com.

//! UTC wall-clock time, rendered without a date dependency.
//!
//! The trace's event timestamps and the store's run ids are the same instant
//! seen through two formats, so the arithmetic lives here once: [`now`]
//! gives the components, [`utc_now`] the RFC-3339-shaped string the trace
//! writes, and a run id is the same components in their compact order.

/// One UTC instant, broken into the parts both consumers need.
#[derive(Clone, Copy, Debug)]
pub struct Stamp {
    pub year: i64,
    /// 1-12.
    pub month: i64,
    /// 1-31.
    pub day: i64,
    pub hour: i64,
    pub min: i64,
    pub sec: i64,
    pub millis: u32,
}

/// The current UTC instant.
#[must_use]
pub fn now() -> Stamp {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    let (y, m, d) = civil_from_days((secs / 86400) as i64);
    let rem = secs % 86400;
    Stamp {
        year: y,
        month: m,
        day: d,
        hour: (rem / 3600) as i64,
        min: ((rem % 3600) / 60) as i64,
        sec: (rem % 60) as i64,
        millis,
    }
}

/// A UTC timestamp with millisecond precision, `YYYY-MM-DDTHH:MM:SS.mmmZ` -
/// the form every trace event carries.
#[must_use]
pub fn utc_now() -> String {
    let t = now();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year, t.month, t.day, t.hour, t.min, t.sec, t.millis
    )
}

/// Days-since-epoch to (year, month, day) - Howard Hinnant's algorithm, the
/// same arithmetic a date library would apply, without the dependency.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_and_a_pinned_date_render_exactly() {
        // 1970-01-01, via the same arithmetic now() uses.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2026-09-27: pins the leap-year correction (the `yoe / 100` term -
        // miswritten once as `yoe / 146_100`, which shifted dates up to
        // three days inside part of every 400-year era).
        assert_eq!(civil_from_days(20_723), (2026, 9, 27));
        let stamp = utc_now();
        assert_eq!(stamp.len(), 24, "{stamp}");
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert_eq!(&stamp[4..5], "-");
        assert_eq!(&stamp[10..11], "T");
    }
}
