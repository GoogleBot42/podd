//! Query handling for the `/api/metrics/*` time-series endpoints.
//!
//! The UI sends `?startTime=&endTime=&side=` on every sleep / vitals / movement
//! request and does no client-side filtering of its own, so the filtering has
//! to happen here (#108). `startTime` / `endTime` are ISO-8601 instants (what
//! `moment().toISOString()` and the sleep records' `entered_bed_at` produce);
//! `side` is `left` or `right`.
//!
//! Record timestamps come in two flavours, matching the UI's zod schemas:
//! sleep records carry ISO-8601 strings (`entered_bed_at`), vitals and movement
//! samples carry **epoch seconds** (`z.number().int()`).

use crate::wire::Side;
use jiff::civil;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// query
// ---------------------------------------------------------------------------

/// Raw `?startTime=&endTime=&side=` query string, before validation.
///
/// Everything is a `String` here on purpose: parsing by hand lets bad input
/// produce this crate's `Invalid request data` body instead of axum's default
/// `Query` rejection text.
#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MetricsQuery {
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub side: Option<String>,
}

/// A validated [`MetricsQuery`]. `None` in every field means "return everything".
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MetricsFilter {
    pub start: Option<Timestamp>,
    pub end: Option<Timestamp>,
    pub side: Option<Side>,
}

impl MetricsQuery {
    /// Validate the raw query, or collect per-parameter error messages.
    pub fn parse(&self) -> Result<MetricsFilter, Vec<String>> {
        let mut errors = Vec::new();

        let start = parse_param("startTime", self.start_time.as_deref(), &mut errors);
        let end = parse_param("endTime", self.end_time.as_deref(), &mut errors);

        let side = match self
            .side
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            None => None,
            Some("left") => Some(Side::Left),
            Some("right") => Some(Side::Right),
            Some(other) => {
                errors.push(format!(
                    "side must be \"left\" or \"right\" (got {other:?})"
                ));
                None
            }
        };

        if errors.is_empty() {
            Ok(MetricsFilter { start, end, side })
        } else {
            Err(errors)
        }
    }
}

fn parse_param(name: &str, raw: Option<&str>, errors: &mut Vec<String>) -> Option<Timestamp> {
    let raw = raw.map(str::trim).filter(|s| !s.is_empty())?;
    match parse_instant(raw) {
        Some(ts) => Some(ts),
        None => {
            errors.push(format!(
                "{name} must be an ISO-8601 timestamp (got {raw:?})"
            ));
            None
        }
    }
}

/// ISO-8601 instant. Offsetless local times (`2026-08-22T21:00:00`) are read as
/// UTC rather than rejected — the UI has sent both shapes historically.
fn parse_instant(raw: &str) -> Option<Timestamp> {
    if let Ok(ts) = raw.parse::<Timestamp>() {
        return Some(ts);
    }
    raw.parse::<civil::DateTime>()
        .ok()
        .and_then(|dt| dt.to_zoned(jiff::tz::TimeZone::UTC).ok())
        .map(|z| z.timestamp())
}

// ---------------------------------------------------------------------------
// filtering
// ---------------------------------------------------------------------------

/// A record the metrics filter can place in time and on a side.
pub trait MetricRecord {
    /// Instant this record is anchored at, or `None` if it can't be parsed.
    fn instant(&self) -> Option<Timestamp>;
    /// Bed side, or `None` if it isn't one of the two known sides.
    fn side(&self) -> Option<Side>;
}

impl MetricsFilter {
    /// Whether `record` falls inside the window (bounds inclusive) and side.
    pub fn matches<T: MetricRecord>(&self, record: &T) -> bool {
        if let Some(side) = self.side {
            if record.side() != Some(side) {
                return false;
            }
        }
        let Some(instant) = record.instant() else {
            // Undatable record: keep it only when no window was requested,
            // so a bad timestamp can't leak into a bounded query.
            return self.start.is_none() && self.end.is_none();
        };
        if self.start.is_some_and(|start| instant < start) {
            return false;
        }
        if self.end.is_some_and(|end| instant > end) {
            return false;
        }
        true
    }
}

/// Apply a validated filter to a batch of records.
pub fn filter_records<T: MetricRecord>(records: Vec<T>, filter: &MetricsFilter) -> Vec<T> {
    records.into_iter().filter(|r| filter.matches(r)).collect()
}

// ---------------------------------------------------------------------------
// record shapes (field names match the UI's zod schemas exactly)
// ---------------------------------------------------------------------------

/// One vitals sample. `timestamp` is **epoch seconds**.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct VitalsRecord {
    pub side: Side,
    pub timestamp: i64,
    pub heart_rate: i64,
    pub hrv: i64,
    pub breathing_rate: i64,
}

impl MetricRecord for VitalsRecord {
    fn instant(&self) -> Option<Timestamp> {
        Timestamp::from_second(self.timestamp).ok()
    }
    fn side(&self) -> Option<Side> {
        Some(self.side)
    }
}

/// One movement sample. `timestamp` is **epoch seconds**.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MovementRecord {
    pub id: i64,
    pub side: Side,
    pub timestamp: i64,
    pub total_movement: i64,
}

impl MetricRecord for MovementRecord {
    fn instant(&self) -> Option<Timestamp> {
        Timestamp::from_second(self.timestamp).ok()
    }
    fn side(&self) -> Option<Side> {
        Some(self.side)
    }
}

/// One sleep record. Timestamps are ISO-8601 strings, and `side` is a plain
/// string upstream, so both are parsed leniently.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SleepRecord {
    pub id: i64,
    pub side: String,
    pub entered_bed_at: String,
    pub left_bed_at: String,
    pub sleep_period_seconds: i64,
    pub times_exited_bed: i64,
    pub present_intervals: Vec<(String, String)>,
    pub not_present_intervals: Vec<(String, String)>,
}

impl MetricRecord for SleepRecord {
    /// Anchored at `entered_bed_at`, matching how the UI's week picker reasons
    /// about a night.
    fn instant(&self) -> Option<Timestamp> {
        parse_instant(&self.entered_bed_at)
    }
    fn side(&self) -> Option<Side> {
        match self.side.as_str() {
            "left" => Some(Side::Left),
            "right" => Some(Side::Right),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(start: Option<&str>, end: Option<&str>, side: Option<&str>) -> MetricsQuery {
        MetricsQuery {
            start_time: start.map(str::to_string),
            end_time: end.map(str::to_string),
            side: side.map(str::to_string),
        }
    }

    fn vitals(timestamp: i64, side: Side) -> VitalsRecord {
        VitalsRecord {
            side,
            timestamp,
            heart_rate: 55,
            hrv: 60,
            breathing_rate: 12,
        }
    }

    fn movement(id: i64, timestamp: i64, side: Side) -> MovementRecord {
        MovementRecord {
            id,
            side,
            timestamp,
            total_movement: 100,
        }
    }

    fn sleep(id: i64, entered: &str, side: &str) -> SleepRecord {
        SleepRecord {
            id,
            side: side.to_string(),
            entered_bed_at: entered.to_string(),
            left_bed_at: entered.to_string(),
            sleep_period_seconds: 0,
            times_exited_bed: 0,
            present_intervals: vec![],
            not_present_intervals: vec![],
        }
    }

    #[test]
    fn empty_query_is_an_empty_filter() {
        let filter = query(None, None, None).parse().unwrap();
        assert_eq!(filter, MetricsFilter::default());
    }

    #[test]
    fn blank_params_are_ignored() {
        // `new URLSearchParams()` entries the UI omits never arrive, but an
        // empty `side=` must not be treated as a bad value.
        let filter = query(Some(""), Some("  "), Some("")).parse().unwrap();
        assert_eq!(filter, MetricsFilter::default());
    }

    #[test]
    fn parses_iso_instants_and_sides() {
        let filter = query(
            Some("2026-08-20T00:00:00.000Z"),
            Some("2026-08-21T12:30:00+02:00"),
            Some("right"),
        )
        .parse()
        .unwrap();
        assert_eq!(filter.start.unwrap().as_second(), 1_787_184_000);
        // 12:30+02:00 is 10:30Z, i.e. start + 1d10h30m.
        assert_eq!(filter.end.unwrap().as_second(), 1_787_308_200);
        assert_eq!(filter.side, Some(Side::Right));
    }

    #[test]
    fn offsetless_local_times_are_read_as_utc() {
        let filter = query(Some("2026-08-20T00:00:00"), None, None)
            .parse()
            .unwrap();
        assert_eq!(filter.start.unwrap().as_second(), 1_787_184_000);
    }

    #[test]
    fn bad_params_are_reported_together() {
        let errors = query(Some("last tuesday"), None, Some("middle"))
            .parse()
            .unwrap_err();
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("startTime"));
        assert!(errors[1].contains("side"));
    }

    #[test]
    fn filters_epoch_second_records_by_window_inclusively() {
        let filter = query(
            Some("1970-01-01T00:01:40Z"), // 100
            Some("1970-01-01T00:05:00Z"), // 300
            None,
        )
        .parse()
        .unwrap();
        let records = vec![
            vitals(99, Side::Left),
            vitals(100, Side::Left),
            vitals(200, Side::Right),
            vitals(300, Side::Left),
            vitals(301, Side::Left),
        ];
        let kept: Vec<i64> = filter_records(records, &filter)
            .iter()
            .map(|r| r.timestamp)
            .collect();
        assert_eq!(kept, vec![100, 200, 300]);
    }

    #[test]
    fn filters_by_side() {
        let filter = query(None, None, Some("left")).parse().unwrap();
        let records = vec![
            movement(1, 100, Side::Left),
            movement(2, 200, Side::Right),
            movement(3, 300, Side::Left),
        ];
        let kept: Vec<i64> = filter_records(records, &filter)
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(kept, vec![1, 3]);
    }

    #[test]
    fn side_and_window_combine() {
        let filter = query(
            Some("1970-01-01T00:00:00Z"),
            Some("1970-01-01T00:03:20Z"), // 200
            Some("right"),
        )
        .parse()
        .unwrap();
        let records = vec![
            movement(1, 100, Side::Left),
            movement(2, 150, Side::Right),
            movement(3, 250, Side::Right),
        ];
        let kept: Vec<i64> = filter_records(records, &filter)
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(kept, vec![2]);
    }

    #[test]
    fn sleep_records_filter_on_entered_bed_at() {
        let filter = query(
            Some("2026-08-20T00:00:00Z"),
            Some("2026-08-22T00:00:00Z"),
            Some("left"),
        )
        .parse()
        .unwrap();
        let records = vec![
            sleep(1, "2026-08-19T23:00:00Z", "left"),  // before window
            sleep(2, "2026-08-20T22:00:00Z", "left"),  // kept
            sleep(3, "2026-08-21T22:00:00Z", "right"), // wrong side
            sleep(4, "2026-08-23T22:00:00Z", "left"),  // after window
        ];
        let kept: Vec<i64> = filter_records(records, &filter)
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(kept, vec![2]);
    }

    #[test]
    fn undatable_records_survive_only_unbounded_queries() {
        let record = sleep(1, "not a timestamp", "left");
        assert!(MetricsFilter::default().matches(&record));

        let bounded = query(Some("2026-08-20T00:00:00Z"), None, None)
            .parse()
            .unwrap();
        assert!(!bounded.matches(&record));
    }

    #[test]
    fn unknown_record_side_never_matches_a_side_query() {
        let record = sleep(1, "2026-08-20T22:00:00Z", "solo");
        let filter = query(None, None, Some("left")).parse().unwrap();
        assert!(!filter.matches(&record));
        assert!(MetricsFilter::default().matches(&record));
    }
}
