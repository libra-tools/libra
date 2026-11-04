//! PI-35: bounded selection and reading of one Pi session source.
//!
//! The hook receiver only knows the Pi session id and the verified repository
//! cwd. This module turns those into the one matching
//! `<timestamp>_<session-id>.jsonl` file under the default Pi session
//! directory, opening the directory and the file through pinned descriptors
//! and never re-opening by path. Directory listing, candidate opens, header
//! validation and the body read all run inside the shared killable helper
//! process, so a stalled filesystem call cannot outlive the capture deadline.
//!
//! Only the default layout `<agent-dir>/sessions/--<encoded-cwd>--/` is
//! consulted here; an explicit Pi session directory is not granted a root and
//! yields no source (the caller degrades to a metadata-only checkpoint).

use std::{
    io::Write,
    path::{Component, Path},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

#[cfg(unix)]
use super::transcript_source::{
    PinnedEntryType, open_file_beneath_pinned_provider_directory,
    read_dir_pinned_provider_directory,
};
use super::{
    AgentKind,
    builtin::pi::{
        PiDiscoveryLimits, open_pi_provider_directory_for_source, pi_session_id_is_valid,
    },
};
use crate::internal::ai::authorized_read::{
    AUTHORIZED_READ_HELPER_ARG, AUTHORIZED_READ_HELPER_CAP_ENV, AUTHORIZED_READ_HELPER_MAX_CAP,
    AUTHORIZED_READ_HELPER_MODE_ENV, RegisteredHelperOutput, StrictBoundedRead,
    read_strictly_bounded, registered_helper_command, run_registered_bounded_helper_until,
};

/// Private helper protocol mode for one Pi session source.
pub(crate) const PI_SESSION_SOURCE_MODE: &str = "pi-session-source-v1";

const PI_SOURCE_COMPLETE: u8 = 0;
const PI_SOURCE_OVERSIZE: u8 = 1;
const PI_SOURCE_READ_ERROR: u8 = 2;
const PI_SOURCE_ABSENT: u8 = 3;
const PI_SOURCE_UNTRUSTED: u8 = 4;

/// One request to the helper. The directory is absolute and inside the
/// configured Pi provider root; every other field is data the hook receiver
/// already verified.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PiSourceControl {
    pub(crate) mode: String,
    /// Absolute default session directory for the repository cwd.
    pub(crate) directory: String,
    pub(crate) session_id: String,
    /// Verified repository cwd recorded in the session header.
    pub(crate) cwd: String,
    pub(crate) remaining_ms: u64,
    pub(crate) max_entries: u64,
    pub(crate) max_header_bytes: u64,
    pub(crate) max_total_bytes: u64,
}

/// Result of one bounded session-source read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PiSessionSourceRead {
    Bytes(Vec<u8>),
    /// No unique usable session file exists (missing, ambiguous, wrong cwd).
    Absent,
    /// A limit was exceeded, at the directory, header or source level.
    Oversize,
    /// The directory is outside the configured provider root.
    Untrusted,
    /// The helper did not finish before the capture deadline.
    DeadlineExceeded,
    /// The helper or its protocol failed.
    Failed,
}

impl PiSessionSourceRead {
    pub(crate) fn bytes(self) -> Option<Vec<u8>> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }
}

/// Build the default Pi session directory for `cwd` under the first
/// configured provider root (`~/.pi`): `<root>/agent/sessions/--<cwd>--`
/// where the leading separator is dropped and `/`, `\` and `:` become `-`
/// (Pi `session-manager.ts:589-595`).
pub fn default_pi_session_directory(cwd: &Path) -> Option<std::path::PathBuf> {
    let root = super::transcript_source::configured_provider_roots(super::agent_for(AgentKind::Pi))
        .into_iter()
        .next()?;
    let cwd = cwd.to_str()?;
    let trimmed = cwd.trim_start_matches(['/', '\\']);
    let encoded: String = trimmed
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect();
    Some(
        root.join("agent")
            .join("sessions")
            .join(format!("--{encoded}--")),
    )
}

/// Read one Pi session source through the bounded helper. Inputs are absolute
/// and `..`-free; an invalid input reports `Absent` rather than spawning.
pub async fn read_pi_session_source_until(
    directory: &Path,
    session_id: &str,
    verified_cwd: &Path,
    limits: PiDiscoveryLimits,
    deadline: Instant,
) -> PiSessionSourceRead {
    let Some(control) = source_control(directory, session_id, verified_cwd, limits, deadline)
    else {
        return PiSessionSourceRead::Absent;
    };
    #[cfg(not(unix))]
    {
        let _ = control;
        return PiSessionSourceRead::Failed;
    }
    #[cfg(unix)]
    {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return PiSessionSourceRead::DeadlineExceeded;
        }
        let cap = limits
            .max_total_bytes
            .clamp(1, AUTHORIZED_READ_HELPER_MAX_CAP);
        let Ok(encoded) = serde_json::to_string(&control) else {
            return PiSessionSourceRead::Failed;
        };
        let mut command = match pi_helper_program() {
            Some(program) => {
                let mut command = tokio::process::Command::new(program);
                command.arg(AUTHORIZED_READ_HELPER_ARG);
                command
            }
            None => match registered_helper_command(AUTHORIZED_READ_HELPER_ARG) {
                Some(command) => command,
                None => {
                    let Ok(executable) = std::env::current_exe() else {
                        return PiSessionSourceRead::Failed;
                    };
                    let mut command = tokio::process::Command::new(executable);
                    command.arg(AUTHORIZED_READ_HELPER_ARG);
                    command
                }
            },
        };
        command
            .env(AUTHORIZED_READ_HELPER_CAP_ENV, cap.to_string())
            .env(AUTHORIZED_READ_HELPER_MODE_ENV, encoded);
        match run_registered_bounded_helper_until(command, &[], cap.saturating_add(9), deadline)
            .await
        {
            RegisteredHelperOutput::Output(frame) => decode_source_frame(&frame, cap),
            RegisteredHelperOutput::DeadlineExceeded => PiSessionSourceRead::DeadlineExceeded,
            RegisteredHelperOutput::Failed => PiSessionSourceRead::Failed,
        }
    }
}

fn source_control(
    directory: &Path,
    session_id: &str,
    verified_cwd: &Path,
    limits: PiDiscoveryLimits,
    deadline: Instant,
) -> Option<PiSourceControl> {
    if !safe_absolute(directory)
        || !safe_absolute(verified_cwd)
        || !pi_session_id_is_valid(session_id)
    {
        return None;
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let remaining_ms = u64::try_from(remaining.as_millis()).ok()?.clamp(1, 60_000);
    Some(PiSourceControl {
        mode: PI_SESSION_SOURCE_MODE.to_string(),
        directory: directory.to_str()?.to_string(),
        session_id: session_id.to_string(),
        cwd: verified_cwd.to_str()?.to_string(),
        remaining_ms,
        max_entries: limits.max_entries as u64,
        max_header_bytes: limits.max_header_bytes,
        max_total_bytes: limits.max_total_bytes,
    })
}

/// The built libra binary for integration tests. Production resolves the
/// helper through `registered_helper_command` or the running executable.
fn pi_helper_program() -> Option<std::path::PathBuf> {
    std::env::var_os("LIBRA_TEST")?;
    std::env::var_os("LIBRA_TEST_PI_SOURCE_HELPER").map(std::path::PathBuf::from)
}

fn safe_absolute(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

fn decode_source_frame(frame: &[u8], cap: u64) -> PiSessionSourceRead {
    if frame.len() < 9 {
        return PiSessionSourceRead::Failed;
    }
    let status = frame[0];
    let Ok(length) = <[u8; 8]>::try_from(&frame[1..9]) else {
        return PiSessionSourceRead::Failed;
    };
    let length = u64::from_be_bytes(length);
    if length > cap || frame.len() as u64 != length.saturating_add(9) {
        return PiSessionSourceRead::Failed;
    }
    match status {
        PI_SOURCE_COMPLETE => PiSessionSourceRead::Bytes(frame[9..].to_vec()),
        PI_SOURCE_OVERSIZE => PiSessionSourceRead::Oversize,
        PI_SOURCE_READ_ERROR => PiSessionSourceRead::Failed,
        PI_SOURCE_ABSENT => PiSessionSourceRead::Absent,
        PI_SOURCE_UNTRUSTED => PiSessionSourceRead::Untrusted,
        _ => PiSessionSourceRead::Failed,
    }
}

/// Helper entry point: decode one control and answer with one frame. Called
/// from the shared helper dispatcher before the ordinary CLI starts.
#[cfg(unix)]
pub(crate) fn run_pi_session_source_helper_if_requested(cap: u64, mode: &str) -> Option<i32> {
    if !mode.starts_with('{') {
        return None;
    }
    let control = serde_json::from_str::<PiSourceControl>(mode).ok()?;
    if control.mode != PI_SESSION_SOURCE_MODE {
        return None;
    }
    Some(run_pi_session_source_helper(cap, control))
}

#[cfg(not(unix))]
pub(crate) fn run_pi_session_source_helper_if_requested(_cap: u64, _mode: &str) -> Option<i32> {
    None
}

#[cfg(unix)]
fn run_pi_session_source_helper(cap: u64, control: PiSourceControl) -> i32 {
    let selection = select_pi_session_source(&control, cap);
    if write_source_frame(selection.status, &selection.bytes) {
        0
    } else {
        1
    }
}

struct SourceSelection {
    status: u8,
    bytes: Vec<u8>,
}

fn failure(status: u8) -> SourceSelection {
    SourceSelection {
        status,
        bytes: Vec::new(),
    }
}

#[cfg(unix)]
fn select_pi_session_source(control: &PiSourceControl, cap: u64) -> SourceSelection {
    if control.mode != PI_SESSION_SOURCE_MODE
        || !pi_session_id_is_valid(&control.session_id)
        || !control.cwd.starts_with('/')
        || control.remaining_ms == 0
        || control.max_entries == 0
        || control.max_header_bytes == 0
        || control.max_total_bytes == 0
    {
        return failure(PI_SOURCE_READ_ERROR);
    }
    let directory = Path::new(&control.directory);
    if !safe_absolute(directory) {
        return failure(PI_SOURCE_UNTRUSTED);
    }
    let deadline = Instant::now() + Duration::from_millis(control.remaining_ms);
    let Ok(Some(directory)) = open_pi_provider_directory_for_source(directory) else {
        return failure(PI_SOURCE_UNTRUSTED);
    };
    test_stall("list");
    let Ok(entries) = read_dir_pinned_provider_directory(&directory) else {
        return failure(PI_SOURCE_READ_ERROR);
    };
    let suffix = format!("_{}.jsonl", control.session_id);
    let mut candidates = Vec::new();
    let mut inspected: u64 = 0;
    for entry in entries {
        let Ok(entry) = entry else {
            return failure(PI_SOURCE_READ_ERROR);
        };
        if Instant::now() >= deadline {
            return failure(PI_SOURCE_OVERSIZE);
        }
        inspected = inspected.saturating_add(1);
        if inspected > control.max_entries {
            return failure(PI_SOURCE_OVERSIZE);
        }
        if entry.file_type != PinnedEntryType::File {
            continue;
        }
        let Some(name) = entry.file_name.to_str() else {
            continue;
        };
        if !name.ends_with(&suffix) {
            continue;
        }
        let Ok(mut file) =
            open_file_beneath_pinned_provider_directory(&directory, Path::new(&entry.file_name))
        else {
            return failure(PI_SOURCE_READ_ERROR);
        };
        #[cfg(any(test, debug_assertions))]
        pi_live_counters::opened();
        test_stall("header");
        let header = match read_strictly_bounded(&mut file, control.max_header_bytes) {
            StrictBoundedRead::Complete(bytes) => bytes,
            StrictBoundedRead::Oversize { .. } => return failure(PI_SOURCE_OVERSIZE),
            StrictBoundedRead::Failed { .. } => return failure(PI_SOURCE_READ_ERROR),
        };
        if !header_matches(&header, &control.session_id, &control.cwd) {
            continue;
        }
        candidates.push((entry.file_name, file));
    }
    if candidates.len() != 1 {
        return failure(PI_SOURCE_ABSENT);
    }
    let Some((_, mut file)) = candidates.pop() else {
        return failure(PI_SOURCE_ABSENT);
    };
    use std::io::{Seek, SeekFrom};
    if file.seek(SeekFrom::Start(0)).is_err() {
        return failure(PI_SOURCE_READ_ERROR);
    }
    test_stall("body");
    match read_strictly_bounded(&mut file, cap) {
        StrictBoundedRead::Complete(bytes) => SourceSelection {
            status: PI_SOURCE_COMPLETE,
            bytes,
        },
        StrictBoundedRead::Oversize { .. } => failure(PI_SOURCE_OVERSIZE),
        StrictBoundedRead::Failed { .. } => failure(PI_SOURCE_READ_ERROR),
    }
}

#[cfg(not(unix))]
fn select_pi_session_source(_control: &PiSourceControl, _cap: u64) -> SourceSelection {
    failure(PI_SOURCE_READ_ERROR)
}

/// In-process selection entry for tests: returns `(status, bytes)` without a
/// helper subprocess, so open counters and limit outcomes can be asserted.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn select_pi_session_source_for_test(control_json: &str, cap: u64) -> (u8, Vec<u8>) {
    let Ok(control) = serde_json::from_str::<PiSourceControl>(control_json) else {
        return (PI_SOURCE_READ_ERROR, Vec::new());
    };
    let selection = select_pi_session_source(&control, cap);
    (selection.status, selection.bytes)
}

/// A control JSON builder for the in-process tests.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn pi_source_control_for_test(
    directory: &Path,
    session_id: &str,
    cwd: &Path,
    limits: PiDiscoveryLimits,
    remaining_ms: u64,
) -> String {
    serde_json::to_string(&PiSourceControl {
        mode: PI_SESSION_SOURCE_MODE.to_string(),
        directory: directory.display().to_string(),
        session_id: session_id.to_string(),
        cwd: cwd.display().to_string(),
        remaining_ms,
        max_entries: limits.max_entries as u64,
        max_header_bytes: limits.max_header_bytes,
        max_total_bytes: limits.max_total_bytes,
    })
    .unwrap_or_default()
}

/// Status byte constants for the in-process tests.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod pi_source_status {
    pub const COMPLETE: u8 = super::PI_SOURCE_COMPLETE;
    pub const OVERSIZE: u8 = super::PI_SOURCE_OVERSIZE;
    pub const ABSENT: u8 = super::PI_SOURCE_ABSENT;
    pub const UNTRUSTED: u8 = super::PI_SOURCE_UNTRUSTED;
}

/// First parsed line must be Pi's session header for this id and cwd.
fn header_matches(bytes: &[u8], session_id: &str, cwd: &str) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Some(line) = text.lines().find(|line| !line.trim().is_empty()) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    value.get("type").and_then(serde_json::Value::as_str) == Some("session")
        && value.get("id").and_then(serde_json::Value::as_str) == Some(session_id)
        && value.get("cwd").and_then(serde_json::Value::as_str) == Some(cwd)
}

fn write_source_frame(status: u8, bytes: &[u8]) -> bool {
    let mut out = std::io::stdout().lock();
    out.write_all(&[status]).is_ok()
        && out.write_all(&(bytes.len() as u64).to_be_bytes()).is_ok()
        && out.write_all(bytes).is_ok()
        && out.flush().is_ok()
}

fn test_stall(stage: &str) {
    if std::env::var_os("LIBRA_TEST").is_none() {
        return;
    }
    if std::env::var("LIBRA_TEST_PI_SOURCE_STALL").ok().as_deref() == Some(stage) {
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
}

#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod pi_live_counters {
    use std::sync::atomic::{AtomicU64, Ordering};

    static OPENED: AtomicU64 = AtomicU64::new(0);

    pub fn reset() {
        OPENED.store(0, Ordering::SeqCst);
    }

    pub fn opened() {
        OPENED.fetch_add(1, Ordering::SeqCst);
    }

    pub fn opened_count() -> u64 {
        OPENED.load(Ordering::SeqCst)
    }
}
