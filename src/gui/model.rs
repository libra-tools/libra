//! Mock sessions shipped with the desktop window.
//!
//! The rows come from the sessions prototype (`libra-sessions-v2`). They are
//! demonstration data, not a live capture index.

use std::sync::OnceLock;

use serde::Deserialize;

/// One catalog load. A parse failure is shown in the window instead of panicking.
pub fn catalog() -> &'static Result<Catalog, String> {
    static CATALOG: OnceLock<Result<Catalog, String>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("sessions.json"))
            .map_err(|error| format!("the bundled session catalog is unreadable: {error}"))
    })
}

/// The prototype's session list and capability map.
#[derive(Debug, Clone, Deserialize)]
pub struct Catalog {
    pub today: String,
    pub sessions: Vec<Session>,
    pub capability_map: Vec<Capability>,
}

/// One captured agent session.
#[derive(Debug, Clone, Deserialize)]
pub struct Session {
    pub id: String,
    pub agent: String,
    pub title: String,
    pub summary: String,
    pub summary_source: String,
    pub model: String,
    pub start: String,
    pub end: String,
    pub worktree: String,
    pub head: String,
    pub running_label: String,
    pub capture: String,
    pub capture_label: String,
    pub readable_label: String,
    pub evidence: String,
    #[serde(default)]
    pub readable: bool,
    pub usage: Usage,
    pub verification: Vec<Verification>,
    pub files: Vec<FileRecord>,
    pub commits: Vec<CommitRecord>,
    pub ai_summary: Vec<String>,
    pub turns: Vec<Turn>,
}

/// Token and call counts from the prototype.
#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    pub input: String,
    pub output: String,
    pub calls: String,
    pub reasoning: String,
    pub cost_note: String,
}

/// One recorded command the session ran.
#[derive(Debug, Clone, Deserialize)]
pub struct Verification {
    pub label: String,
    pub result: String,
    pub detail: String,
}

/// A file the capture associated with the session.
#[derive(Debug, Clone, Deserialize)]
pub struct FileRecord {
    pub path: String,
    pub source: String,
    pub note: String,
}

/// A commit linked from the session.
#[derive(Debug, Clone, Deserialize)]
pub struct CommitRecord {
    pub sha: String,
    pub message: String,
    pub kind: String,
}

/// One conversation turn.
#[derive(Debug, Clone, Deserialize)]
pub struct Turn {
    pub id: String,
    pub n: usize,
    pub time: String,
    pub user: String,
    pub events: Vec<Event>,
}

/// A reasoning block, tool call, assistant reply, or note inside a turn.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Reasoning {
        status: String,
        provider: String,
        text: String,
    },
    Tool {
        name: String,
        input: String,
        output: String,
        status: String,
    },
    Assistant {
        text: String,
        model: String,
    },
    Note {
        label: String,
        text: String,
    },
}

/// One row of the capability map.
#[derive(Debug, Clone, Deserialize)]
pub struct Capability {
    pub capability: String,
    pub basis: String,
    pub data: String,
    pub missing: String,
}

/// `today`, `yesterday`, or `earlier` relative to the catalog's today.
pub fn date_bucket(start: &str, today: &str) -> &'static str {
    let day = start.get(..10).unwrap_or("");
    if day == today {
        "today"
    } else if yesterday(today).as_deref() == Some(day) {
        "yesterday"
    } else {
        "earlier"
    }
}

fn yesterday(today: &str) -> Option<String> {
    let mut parts = today.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    let days = days_from_civil(year, month, day)?.checked_sub(1)?;
    let (year, month, day) = civil_from_days(days);
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i32> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = year - i32::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let doy = (153 * (month as i32 + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i32 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

fn civil_from_days(days: i32) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (mp + if mp < 10 { 3 } else { -9 }) as u32;
    (y + i32::from(month <= 2), month, day)
}

/// Clock `HH:MM` from an ISO local timestamp.
pub fn clock(iso: &str) -> &str {
    // A missing value ("—") must render as itself, not as an empty string.
    iso.get(11..16).unwrap_or(iso)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_loads_the_prototype_sessions() {
        let catalog = catalog().as_ref().expect("catalog json");
        assert_eq!(catalog.today, "2026-10-20");
        assert_eq!(catalog.sessions.len(), 8);
        assert!(!catalog.capability_map.is_empty());
        assert!(
            catalog
                .sessions
                .iter()
                .any(|session| session.id == "s-cc-arch"),
            "the prototype's opening session should be present"
        );
    }

    #[test]
    fn date_bucket_uses_the_catalog_today() {
        assert_eq!(date_bucket("2026-10-20T10:42:00", "2026-10-20"), "today");
        assert_eq!(
            date_bucket("2026-10-19T09:00:00", "2026-10-20"),
            "yesterday"
        );
        assert_eq!(date_bucket("2026-06-12T09:00:00", "2026-10-20"), "earlier");
    }
}
