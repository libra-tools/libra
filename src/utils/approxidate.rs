//! A focused port of Git's `parse_expiry_date` / `approxidate` subset used by
//! `libra config --type=expiry-date`.
//!
//! The full approxidate grammar is large; this module implements the forms the
//! compatibility matrix pins: `never`/`false`, `now`/`all`, the special words
//! `today`/`yesterday`/`tomorrow`/`noon`/`midnight`, relative
//! `<n>.<unit>.ago` / `<n> <unit> ago`, and absolute dates (ISO 8601,
//! `YYYY-MM-DD`, and RFC 2822-style). Anything else is an error so a config
//! value is never silently widened.

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone};

/// The value `now`/`all` maps to: far enough in the future that any real
/// timestamp compares as expired.
pub const TIME_MAX: i64 = i64::MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiryDateError {
    value: String,
}

impl std::fmt::Display for ExpiryDateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "'{}' is not a valid timestamp", self.value)
    }
}

impl std::error::Error for ExpiryDateError {}

fn unit_seconds(unit: &str) -> Option<i64> {
    let unit = unit.trim_end_matches('s');
    Some(match unit {
        "second" | "sec" => 1,
        "minute" | "min" => 60,
        "hour" | "hr" => 60 * 60,
        "day" => 24 * 60 * 60,
        "week" | "wk" => 7 * 24 * 60 * 60,
        // Git's approxidate uses calendar-ish months/years; the matrix only
        // pins day/hour/week forms, so keep the simple 30/365 approximation.
        "month" | "mon" => 30 * 24 * 60 * 60,
        "year" | "yr" => 365 * 24 * 60 * 60,
        _ => return None,
    })
}

fn local_midnight(date: NaiveDate) -> Option<i64> {
    let naive = date.and_hms_opt(0, 0, 0)?;
    Local
        .from_local_datetime(&naive)
        .single()
        .map(|dt| dt.timestamp())
}

fn relative_ago(value: &str) -> Option<i64> {
    let lower = value.to_ascii_lowercase();
    let lower = lower.trim();
    // Accept `2.weeks.ago`, `2 weeks ago`, and `2.weeks`.
    let stripped = lower.strip_suffix(".ago").or_else(|| {
        lower
            .strip_suffix(" ago")
            .or_else(|| lower.strip_suffix(".ago"))
    });
    let body = stripped.unwrap_or(lower);
    let body = body.replace('.', " ");
    let mut parts = body.split_whitespace();
    let number: i64 = parts.next()?.parse().ok()?;
    let unit = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let seconds = unit_seconds(unit)?;
    let now = Local::now().timestamp();
    Some(now - number.saturating_mul(seconds))
}

/// Parse a Git expiry-date value.
pub fn parse_expiry_date(value: &str) -> Result<i64, ExpiryDateError> {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();

    match lower.as_str() {
        "never" | "false" => return Ok(0),
        "now" | "all" => return Ok(TIME_MAX),
        "" => {
            return Err(ExpiryDateError {
                value: value.to_string(),
            });
        }
        _ => {}
    }

    let now = Local::now();
    let today = now.date_naive();
    let special: Option<i64> = match lower.as_str() {
        "today" => local_midnight(today),
        "yesterday" => today
            .pred_opt()
            .and_then(local_midnight)
            .or(Some(now.timestamp() - 86_400)),
        "tomorrow" => Some(now.timestamp() + 86_400),
        "noon" => today
            .and_hms_opt(12, 0, 0)
            .and_then(|naive| Local.from_local_datetime(&naive).single())
            .map(|dt| dt.timestamp()),
        "midnight" => local_midnight(today),
        _ => None,
    };
    if let Some(ts) = special {
        return Ok(ts);
    }

    if let Some(ts) = relative_ago(trimmed) {
        return Ok(ts);
    }

    if let Some(ts) = parse_absolute(trimmed, now) {
        return Ok(ts);
    }

    Err(ExpiryDateError {
        value: value.to_string(),
    })
}

fn parse_absolute(value: &str, now: DateTime<Local>) -> Option<i64> {
    // `YYYY-MM-DD` → local midnight.
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return local_midnight(date);
    }
    // RFC 3339 / ISO 8601 with an explicit offset.
    if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
        return Some(dt.timestamp());
    }
    // `YYYY-MM-DD HH:MM:SS` / `YYYY-MM-DDTHH:MM:SS` (no offset) → local time.
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, format)
            && let Some(dt) = Local.from_local_datetime(&naive).single()
        {
            return Some(dt.timestamp());
        }
    }
    // RFC 2822-style `Fri Jun 4 15:46:55 2010` (no zone) → local time.
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, "%a %b %e %H:%M:%S %Y")
        && let Some(dt) = Local.from_local_datetime(&naive).single()
    {
        return Some(dt.timestamp());
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, "%a, %d %b %Y %H:%M:%S %z") {
        return Some(naive.and_utc().timestamp());
    }
    // A bare unix timestamp is accepted like `log --since=<epoch>`.
    if let Ok(ts) = value.parse::<i64>() {
        return Some(ts);
    }
    let _ = now.day();
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_and_now_match_git() {
        assert_eq!(parse_expiry_date("never").unwrap(), 0);
        assert_eq!(parse_expiry_date("false").unwrap(), 0);
        assert_eq!(parse_expiry_date("now").unwrap(), TIME_MAX);
        assert_eq!(parse_expiry_date("all").unwrap(), TIME_MAX);
    }

    #[test]
    fn relative_forms_are_in_the_past() {
        let now = Local::now().timestamp();
        for (value, expected_seconds) in [
            ("2.weeks.ago", 14 * 24 * 60 * 60),
            ("3.days.ago", 3 * 24 * 60 * 60),
            ("1.hour.ago", 60 * 60),
            ("3 days ago", 3 * 24 * 60 * 60),
        ] {
            let ts = parse_expiry_date(value).expect(value);
            let delta = now - ts;
            assert!(
                (delta - expected_seconds).abs() <= 2,
                "{value}: expected ~{expected_seconds}s ago, got {delta}"
            );
        }
    }

    #[test]
    fn absolute_forms_parse() {
        let ts = parse_expiry_date("2017-11-11T23:11:11").expect("iso");
        assert_eq!(
            Local
                .timestamp_opt(ts, 0)
                .single()
                .map(|dt| dt.naive_local()),
            NaiveDate::from_ymd_opt(2017, 11, 11)
                .unwrap()
                .and_hms_opt(23, 11, 11)
        );
        assert!(parse_expiry_date("2010-06-04").expect("date") > 0);
        assert!(parse_expiry_date("Fri Jun 4 15:46:55 2010").expect("rfc2822") > 0);
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(parse_expiry_date("abc").is_err());
        assert!(parse_expiry_date("").is_err());
    }
}
