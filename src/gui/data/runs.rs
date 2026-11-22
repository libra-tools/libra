//! Review/Investigate run records for the window: item-level bounded reads.
//!
//! Scans `.libra/sessions/agent-runs/<run_id>/` (metadata cap 5000, parsed
//! states 500, per-file cap 64 KiB), validates `kind` BEFORE parsing the rest,
//! and turns corrupt/oversized/unknown-kind runs into item-level states instead
//! of failing the page.

use std::path::Path;

use super::{snapshot::DataError, state::SourceState};

/// One run record shown by Reviews/Investigate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub run_id: String,
    pub kind: String,
    pub started_at: i64,
    /// Millisecond sort key preserving wire timestamp precision.
    pub started_at_ms: i64,
    pub status: String,
    /// Redacted summary excerpt (≤200 chars).
    pub summary: String,
    pub reviewers: Vec<String>,
    /// Recorded rounds/turns when the kind reports them.
    pub rounds: Option<u32>,
    /// Redacted conclusion excerpt (≤200 chars).
    pub conclusion: String,
    /// recorded / empty / unavailable / oversized.
    pub conclusion_state: String,
    /// Item-level state: available / malformed / incomplete / oversized.
    pub item_state: String,
}

/// One runs read: rows plus typed source state and truncation flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunsRead {
    pub state: SourceState,
    pub rows: Vec<RunRow>,
    pub truncated: bool,
}

/// Metadata entries scanned per page.
pub const RUN_METADATA_CAP: usize = 5000;
/// State files parsed per page.
pub const RUN_STATE_CAP: usize = 500;
/// Per-file bounded read cap.
pub const RUN_FILE_CAP: usize = 64 * 1024;
/// Rows shown per kind.
pub const RUN_ROWS_PER_KIND: usize = 100;

/// Read bounded run records from the repository's session root.
pub async fn read_runs(
    scope: &crate::internal::worktree_scope::RequestScope,
) -> Result<RunsRead, DataError> {
    let root = scope.storage.join("sessions").join("agent-runs");
    // Directory-specific probe: never open a FIFO/special file here.
    match std::fs::metadata(&root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(DataError::new(
                SourceState::Unreadable {
                    kind: std::io::ErrorKind::NotADirectory,
                },
                "the agent run path is not a directory",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RunsRead {
                state: SourceState::Absent,
                rows: Vec::new(),
                truncated: false,
            });
        }
        Err(error) => {
            return Err(DataError::new(
                SourceState::Unreadable { kind: error.kind() },
                "the agent run directory cannot be read",
            ));
        }
    }
    let mut entries = std::fs::read_dir(&root).map_err(|error| {
        DataError::new(
            SourceState::Unreadable { kind: error.kind() },
            "the agent run directory could not be listed",
        )
    })?;
    let mut candidates: Vec<(i64, String)> = Vec::new();
    let mut truncated = false;
    for (inspected, entry) in entries.by_ref().enumerate() {
        if inspected >= RUN_METADATA_CAP {
            truncated = true;
            break;
        }
        let entry = entry.map_err(|error| {
            DataError::new(
                SourceState::Unreadable { kind: error.kind() },
                "the agent run directory could not be listed",
            )
        })?;
        let file_type = entry.file_type().map_err(|error| {
            DataError::new(
                SourceState::Unreadable { kind: error.kind() },
                "an agent run entry could not be inspected",
            )
        })?;
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        let Some(run_id) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        // Metadata mtime orders candidates newest-first; a missing mtime is
        // treated as oldest rather than failing the page.
        let mtime = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        candidates.push((mtime, run_id.to_string()));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let redactor = crate::internal::ai::observed_agents::redaction::Redactor::new_default();
    let mut rows: Vec<RunRow> = Vec::new();
    for (parsed, (_mtime, run_id)) in candidates.into_iter().enumerate() {
        if parsed >= RUN_STATE_CAP {
            truncated = true;
            break;
        }
        let dir = root.join(&run_id);
        let (row, oversized) = read_one(&dir, &run_id, &redactor);
        if oversized {
            truncated = true;
        }
        rows.push(row);
    }
    // Sort by the RECORD's time first, then keep the newest 100 per kind.
    rows.sort_by(|a, b| {
        b.started_at_ms
            .cmp(&a.started_at_ms)
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
    let mut per_kind: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    rows.retain(|row| {
        let count = per_kind.entry(row.kind.clone()).or_insert(0);
        *count += 1;
        if *count > RUN_ROWS_PER_KIND {
            truncated = true;
            false
        } else {
            true
        }
    });
    rows.sort_by(|a, b| {
        b.started_at_ms
            .cmp(&a.started_at_ms)
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
    let state = if rows.is_empty() {
        SourceState::Absent
    } else {
        SourceState::Available
    };
    Ok(RunsRead {
        state,
        rows,
        truncated,
    })
}

/// Read one run directory with bounded files. Returns the row plus whether any
/// file exceeded the cap (item-level `oversized`).
fn read_one(
    dir: &Path,
    run_id: &str,
    redactor: &crate::internal::ai::observed_agents::redaction::Redactor,
) -> (RunRow, bool) {
    let state_path = dir.join("state.json");
    let (bytes, oversized) = read_bounded(&state_path);
    let mut row = RunRow {
        run_id: run_id.to_string(),
        kind: "unknown".to_string(),
        started_at: 0,
        started_at_ms: 0,
        rounds: None,
        conclusion: String::new(),
        conclusion_state: "unavailable".to_string(),
        status: "unknown".to_string(),
        summary: String::new(),
        reviewers: Vec::new(),
        item_state: "available".to_string(),
    };
    let Some(bytes) = bytes else {
        row.item_state = if oversized {
            "oversized".to_string()
        } else {
            "incomplete".to_string()
        };
        return (row, oversized);
    };
    // Kind-first parsing: read and validate `kind` before any other field.
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            row.item_state = "malformed".to_string();
            return (row, oversized);
        }
    };
    let kind = value
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match kind {
        "review" | "investigate" => row.kind = kind.to_string(),
        _ => {
            row.kind = "unknown".to_string();
            row.summary = sanitize(&redact(redactor, kind));
            row.item_state = "malformed".to_string();
            return (row, oversized);
        }
    }
    if oversized {
        row.item_state = "oversized".to_string();
        return (row, true);
    }
    match kind {
        "review" => {
            let wire: crate::internal::ai::review::store::ReviewRunState =
                match serde_json::from_value::<crate::internal::ai::review::store::ReviewRunState>(
                    value,
                ) {
                    Ok(wire)
                        if wire.run_id == run_id
                            && wire.kind == "review"
                            && (wire.schema_version == 1
                                || wire.schema_version
                                    == crate::internal::ai::review::store::REVIEW_MANIFEST_SCHEMA_VERSION) =>
                    {
                        wire
                    }
                    _ => {
                        row.item_state = "malformed".to_string();
                        return (row, oversized);
                    }
                };
            let Some(ms) = parse_ts_ms(&wire.created_at) else {
                row.item_state = "malformed".to_string();
                return (row, oversized);
            };
            row.started_at_ms = ms;
            row.started_at = ms.div_euclid(1000);
            row.status = wire
                .terminal_state
                .map(|state| state.as_str().to_string())
                .unwrap_or_else(|| "running".to_string());
            row.summary = sanitize(&redact(redactor, &wire.target_scope));
            let (findings, findings_oversized) = read_bounded(&dir.join("findings.md"));
            if findings_oversized {
                row.conclusion_state = "oversized".to_string();
            } else if let Some(bytes) = findings {
                let text = extract_review_conclusion(&String::from_utf8_lossy(&bytes));
                row.conclusion = sanitize(&redact(redactor, &text));
                row.conclusion_state = if row.conclusion.is_empty() {
                    "empty".to_string()
                } else {
                    "recorded".to_string()
                };
            } else {
                row.conclusion_state = "unavailable".to_string();
            }
            row.reviewers = wire
                .agents
                .iter()
                .map(|agent| sanitize(&redact(redactor, &agent.slug)))
                .collect();
        }
        "investigate" => {
            let wire: crate::internal::ai::investigate::store::InvestigateRunState =
                match serde_json::from_value::<
                    crate::internal::ai::investigate::store::InvestigateRunState,
                >(value)
                {
                    Ok(wire)
                        if wire.run_id == run_id
                            && wire.kind == "investigate"
                            && (wire.schema_version == 1
                                || wire.schema_version
                                    == crate::internal::ai::investigate::store::INVESTIGATE_MANIFEST_SCHEMA_VERSION) =>
                    {
                        wire
                    }
                    _ => {
                        row.item_state = "malformed".to_string();
                        return (row, oversized);
                    }
                };
            let Some(ms) = parse_ts_ms(&wire.started_at) else {
                row.item_state = "malformed".to_string();
                return (row, oversized);
            };
            row.started_at_ms = ms;
            row.started_at = ms.div_euclid(1000);
            row.status = match wire.terminal_state {
                Some(state) => state.as_str().to_string(),
                None if wire.pending_turn.is_some() => "paused".to_string(),
                None => "running".to_string(),
            };
            row.summary = sanitize(&redact(redactor, &wire.topic));
            row.rounds = Some(wire.completed_rounds);
            row.conclusion = wire
                .stances
                .last()
                .map(|stance| sanitize(&redact(redactor, &stance.summary)))
                .unwrap_or_default();
            row.conclusion_state = if row.conclusion.is_empty() {
                "empty".to_string()
            } else {
                "recorded".to_string()
            };
            row.reviewers = wire
                .agents
                .iter()
                .map(|agent| sanitize(&redact(redactor, agent)))
                .collect();
        }
        _ => {
            row.kind = if kind.is_empty() { "unknown" } else { kind }.to_string();
            row.item_state = "malformed".to_string();
            return (row, oversized);
        }
    }
    // The manifest is part of the bounded item contract too.
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.is_file() {
        row.item_state = "incomplete".to_string();
    } else {
        let (manifest, manifest_oversized) = read_bounded(&manifest_path);
        if manifest_oversized {
            row.item_state = "oversized".to_string();
        } else if let Some(manifest) = manifest {
            let ok = match row.kind.as_str() {
                "review" => serde_json::from_slice::<
                    crate::internal::ai::review::store::ReviewManifest,
                >(&manifest)
                .map(|wire| {
                    wire.run_id == run_id
                        && wire.kind == "review"
                        && (wire.schema_version == 1
                            || wire.schema_version
                                == crate::internal::ai::review::store::REVIEW_MANIFEST_SCHEMA_VERSION)
                })
                .unwrap_or(false),
                "investigate" => serde_json::from_slice::<
                    crate::internal::ai::investigate::store::InvestigateManifest,
                >(&manifest)
                .map(|wire| {
                    wire.run_id == run_id
                        && wire.kind == "investigate"
                        && (wire.schema_version == 1
                            || wire.schema_version
                                == crate::internal::ai::investigate::store::INVESTIGATE_MANIFEST_SCHEMA_VERSION)
                })
                .unwrap_or(false),
                _ => true,
            };
            if !ok {
                row.item_state = "malformed".to_string();
            }
        } else {
            row.item_state = "incomplete".to_string();
        }
    }
    (row, false)
}

/// Extract reviewer content from the production findings.md format: skip the
/// metadata header and `<<<untrusted-reviewer-output>>>` marker lines.
fn extract_review_conclusion(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty()
                && !trimmed.starts_with("<<<")
                && !trimmed.starts_with("## ")
                && !trimmed.starts_with("# Review findings")
                && !trimmed.starts_with("- run_id:")
                && !trimmed.starts_with("- target_scope:")
                && !trimmed.starts_with("- starting_sha:")
                && !trimmed.starts_with("- terminal_state:")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_ts_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|value| value.timestamp_millis())
}

/// Bounded read on an OPEN handle: reads at most `RUN_FILE_CAP + 1` bytes, so
/// replacement/growth between stat and read cannot exceed the cap.
fn read_bounded(path: &Path) -> (Option<Vec<u8>>, bool) {
    use std::io::Read as _;
    // Reject nonregular entries BEFORE opening, so a FIFO cannot block the
    // shared worker during open.
    // Nonblocking open on Unix: a FIFO cannot stall the shared worker.
    let file = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            match std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)
            {
                Ok(file) => file,
                Err(_) => return (None, false),
            }
        }
        #[cfg(not(unix))]
        {
            match std::fs::File::open(path) {
                Ok(file) => file,
                Err(_) => return (None, false),
            }
        }
    };
    // Validate the OPENED handle's type (replacement-safe).
    match file.metadata() {
        Ok(metadata) if metadata.is_file() => {}
        _ => return (None, false),
    }
    let mut bytes = Vec::new();
    let mut handle = file.take((RUN_FILE_CAP as u64) + 1);
    if handle.read_to_end(&mut bytes).is_err() {
        return (None, false);
    }
    if bytes.len() > RUN_FILE_CAP {
        (None, true)
    } else {
        (Some(bytes), false)
    }
}

/// Which page may show this row: its own kind, or an unclassified item state
/// (damaged/unknown rows stay visible).
pub fn visible_on(row: &RunRow, page_kind: &str) -> bool {
    // Damaged rows of a KNOWN kind stay on their own page; unknown/missing
    // kinds are unclassified and stay visible on both pages.
    let unclassified = row.kind != "review" && row.kind != "investigate";
    row.kind == page_kind || unclassified
}

/// Filter predicate over real rows (kind and free-text search).
pub fn matches_filter(row: &RunRow, kind: &str, query: &str) -> bool {
    let kind = kind.trim().to_lowercase();
    let query = query.trim().to_lowercase();
    (kind.is_empty() || row.kind == kind)
        && (query.is_empty()
            || row.run_id.to_lowercase().contains(&query)
            || row.summary.to_lowercase().contains(&query)
            || row.status.to_lowercase().contains(&query))
}

fn redact(
    redactor: &crate::internal::ai::observed_agents::redaction::Redactor,
    text: &str,
) -> String {
    let (bytes, _report) = redactor.redact(text.as_bytes());
    String::from_utf8_lossy(bytes.bytes()).into_owned()
}

/// Bound a display excerpt: no absolute paths, ≤200 chars.
fn sanitize(text: &str) -> String {
    super::operations::sanitize_paths(text, Path::new(""), Path::new(""))
        .chars()
        .take(200)
        .collect()
}
