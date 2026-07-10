//! AG-24a compliance surface (plan.md Task A8.5): retention-window
//! configuration and append-only audit-log writes for raw
//! (un-redacted) checkpoint access / export.
//!
//! Two concerns live here:
//!
//! - **Retention settings** — typed getters over `ConfigKv` for
//!   `agent.retention.{transcript,stderr,findings}_days` and
//!   `agent.max_transcript_read_bytes`. Each returns its documented
//!   default when the key is absent and rejects a non-positive /
//!   non-numeric override (fail-safe, mirroring `usage prune`'s
//!   validation and the `trust.rs` gate pattern).
//! - **Audit log** — [`AuditRecord`] plus [`write_audit_record`], which
//!   appends one row to the append-only `agent_audit_log` table for every
//!   raw checkpoint access (granted or denied). The table rejects
//!   `UPDATE`/`DELETE` at the database level (see
//!   `sql/migrations/2026070803_agent_audit_log.sql`), so this module only
//!   ever `INSERT`s.

use anyhow::{Context, Result, bail};
use sea_orm::{ConnectionTrait, Statement, Value};

use crate::internal::config::ConfigKv;

/// `agent.retention.transcript_days` — stopped-session transcript/prompt/
/// context retention window (default 90). Active sessions are unaffected.
pub const RETENTION_TRANSCRIPT_DAYS_KEY: &str = "agent.retention.transcript_days";
pub const DEFAULT_RETENTION_TRANSCRIPT_DAYS: u32 = 90;

/// `agent.retention.stderr_days` — stderr / redaction-report blob
/// retention window (default 30). Aggregate counts survive GC.
pub const RETENTION_STDERR_DAYS_KEY: &str = "agent.retention.stderr_days";
pub const DEFAULT_RETENTION_STDERR_DAYS: u32 = 30;

/// `agent.retention.findings_days` — review/investigate run-state and
/// findings retention window (default 90). The GC that consumes this key
/// is A7/A8-dependent; the setting itself is defined here so the contract
/// is stable ahead of that work.
pub const RETENTION_FINDINGS_DAYS_KEY: &str = "agent.retention.findings_days";
pub const DEFAULT_RETENTION_FINDINGS_DAYS: u32 = 90;

/// `agent.max_transcript_read_bytes` — per-read cap for the redacted
/// detail/transcript path (default 256 MiB). Reads beyond the cap must
/// truncate + redact + flag `truncated:true`.
pub const MAX_TRANSCRIPT_READ_BYTES_KEY: &str = "agent.max_transcript_read_bytes";
pub const DEFAULT_MAX_TRANSCRIPT_READ_BYTES: u64 = 268_435_456; // 256 MiB

/// Parse a stored config value as a strictly-positive integer, or fail
/// with an actionable message. An absent key yields the supplied default.
fn positive_u64_setting(raw: Option<String>, key: &str, default: u64) -> Result<u64> {
    let Some(value) = raw else { return Ok(default) };
    let trimmed = value.trim();
    let parsed: u64 = trimmed.parse().map_err(|_| {
        anyhow::anyhow!("config '{key}' must be a positive integer, found {trimmed:?}")
    })?;
    if parsed == 0 {
        bail!("config '{key}' must be greater than 0");
    }
    Ok(parsed)
}

async fn read_setting(key: &str) -> Result<Option<String>> {
    Ok(ConfigKv::get(key)
        .await
        .with_context(|| format!("read config '{key}'"))?
        .map(|entry| entry.value))
}

/// Narrow a validated positive day-count to `u32`, rejecting values that would
/// silently wrap on an `as u32` cast (a huge config must fail loudly rather
/// than becoming a tiny — and dangerously destructive — retention window).
fn u32_days(value: u64, key: &str) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| anyhow::anyhow!("config '{key}' exceeds the maximum of {} days", u32::MAX))
}

/// Resolve `agent.retention.transcript_days` (default 90, must be > 0).
pub async fn retention_transcript_days() -> Result<u32> {
    let v = positive_u64_setting(
        read_setting(RETENTION_TRANSCRIPT_DAYS_KEY).await?,
        RETENTION_TRANSCRIPT_DAYS_KEY,
        DEFAULT_RETENTION_TRANSCRIPT_DAYS as u64,
    )?;
    u32_days(v, RETENTION_TRANSCRIPT_DAYS_KEY)
}

/// Resolve `agent.retention.stderr_days` (default 30, must be > 0).
pub async fn retention_stderr_days() -> Result<u32> {
    let v = positive_u64_setting(
        read_setting(RETENTION_STDERR_DAYS_KEY).await?,
        RETENTION_STDERR_DAYS_KEY,
        DEFAULT_RETENTION_STDERR_DAYS as u64,
    )?;
    u32_days(v, RETENTION_STDERR_DAYS_KEY)
}

/// Resolve `agent.retention.findings_days` (default 90, must be > 0).
pub async fn retention_findings_days() -> Result<u32> {
    let v = positive_u64_setting(
        read_setting(RETENTION_FINDINGS_DAYS_KEY).await?,
        RETENTION_FINDINGS_DAYS_KEY,
        DEFAULT_RETENTION_FINDINGS_DAYS as u64,
    )?;
    u32_days(v, RETENTION_FINDINGS_DAYS_KEY)
}

/// Resolve `agent.max_transcript_read_bytes` (default 256 MiB, must be > 0).
pub async fn max_transcript_read_bytes() -> Result<u64> {
    positive_u64_setting(
        read_setting(MAX_TRANSCRIPT_READ_BYTES_KEY).await?,
        MAX_TRANSCRIPT_READ_BYTES_KEY,
        DEFAULT_MAX_TRANSCRIPT_READ_BYTES,
    )
}

/// The read scope recorded in an audit row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditScope {
    Transcript,
    Prompt,
    Context,
    Stderr,
    Full,
}

impl AuditScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transcript => "transcript",
            Self::Prompt => "prompt",
            Self::Context => "context",
            Self::Stderr => "stderr",
            Self::Full => "full",
        }
    }
}

/// One append-only audit record for a raw checkpoint access/export. Field
/// set mirrors the `agent_audit_log` schema (agent.md §Audit log 规格).
#[derive(Debug, Clone)]
pub struct AuditRecord {
    pub audit_id: String,
    /// UTC ISO-8601 timestamp.
    pub timestamp: String,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
    /// Audited action, e.g. `raw_export`.
    pub action: String,
    pub checkpoint_id: String,
    pub scope: AuditScope,
    pub export_path: Option<String>,
    pub justification: Option<String>,
    /// Whether the access was granted; a denial still records a row.
    pub granted: bool,
}

impl AuditRecord {
    /// Build a record for `action` on `checkpoint_id`, stamping the actor
    /// from the caller-resolved identity and `timestamp` from the caller
    /// (kept as a parameter so callers control the clock — tests and the
    /// no-`Date::now` workflow constraint both need that).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        audit_id: String,
        timestamp: String,
        identity: (Option<String>, Option<String>),
        action: impl Into<String>,
        checkpoint_id: impl Into<String>,
        scope: AuditScope,
        export_path: Option<String>,
        justification: Option<String>,
        granted: bool,
    ) -> Self {
        let (user_id, user_name) = identity;
        Self {
            audit_id,
            timestamp,
            user_id,
            user_name,
            action: action.into(),
            checkpoint_id: checkpoint_id.into(),
            scope,
            export_path,
            justification,
            granted,
        }
    }
}

/// Append `record` to the append-only `agent_audit_log`. Only ever an
/// `INSERT` — the table's triggers reject `UPDATE`/`DELETE`.
pub async fn write_audit_record<C: ConnectionTrait>(conn: &C, record: &AuditRecord) -> Result<()> {
    let backend = conn.get_database_backend();
    conn.execute(Statement::from_sql_and_values(
        backend,
        "INSERT INTO agent_audit_log \
         (audit_id, timestamp, user_id, user_name, action, checkpoint_id, scope, export_path, justification, granted) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        [
            Value::from(record.audit_id.clone()),
            Value::from(record.timestamp.clone()),
            Value::from(record.user_id.clone()),
            Value::from(record.user_name.clone()),
            Value::from(record.action.clone()),
            Value::from(record.checkpoint_id.clone()),
            Value::from(record.scope.as_str().to_string()),
            Value::from(record.export_path.clone()),
            Value::from(record.justification.clone()),
            Value::from(i64::from(record.granted)),
        ],
    ))
    .await
    .context("append agent_audit_log record")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_setting_defaults_and_validates() {
        assert_eq!(positive_u64_setting(None, "k", 90).unwrap(), 90);
        assert_eq!(
            positive_u64_setting(Some("15".into()), "k", 90).unwrap(),
            15
        );
        assert_eq!(
            positive_u64_setting(Some("  7 ".into()), "k", 90).unwrap(),
            7,
            "whitespace trimmed"
        );
        assert!(positive_u64_setting(Some("0".into()), "k", 90).is_err());
        assert!(positive_u64_setting(Some("-3".into()), "k", 90).is_err());
        assert!(positive_u64_setting(Some("abc".into()), "k", 90).is_err());
    }

    #[test]
    fn audit_scope_strings_are_stable() {
        assert_eq!(AuditScope::Transcript.as_str(), "transcript");
        assert_eq!(AuditScope::Full.as_str(), "full");
    }

    #[test]
    fn u32_days_rejects_values_that_would_wrap() {
        // In range: passes through unchanged.
        assert_eq!(u32_days(90, "k").unwrap(), 90);
        assert_eq!(u32_days(u64::from(u32::MAX), "k").unwrap(), u32::MAX);
        // Just over u32::MAX would wrap on `as u32` (2^32 + 1 -> 1); it must
        // fail loudly instead of becoming a tiny, over-destructive window.
        assert!(u32_days(u64::from(u32::MAX) + 1, "k").is_err());
        assert!(u32_days(u64::MAX, "k").is_err());
    }
}
