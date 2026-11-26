//! Durable, append-only, 0600 audit log for obliteration (lore.md §7.8).
//!
//! A destructive compliance operation MUST leave a durable record. The
//! production AI-runtime `AuditSink` only emits `tracing` events (ephemeral),
//! so obliteration writes its OWN JSONL file at
//! `.libra/obliteration-audit.jsonl`, opened in APPEND mode (never truncated
//! by this writer), fsynced per record, and enforced to 0600 (a failure to
//! lock it down aborts the operation). The append-only property is enforced
//! by this writer's open mode + a hard 0600 lock, not by a filesystem
//! immutability attribute — a privileged external actor could still rewrite
//! the file; that is out of scope for v1 and documented. Each line is one
//! record; the payload NEVER contains erased content or cleartext — only the
//! OID (an address, not the payload), the actor, the approval source, and the
//! outcome (§7.8 redaction constraint).

use std::io::Write;

use serde::{Deserialize, Serialize};

use crate::utils::{
    error::{CliError, CliResult, StableErrorCode},
    util,
};

/// One append-only audit record. Field-aligned with the durable-record
/// convention (timestamped, actor-attributed, outcome-bearing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    /// RFC3339 UTC timestamp (caller-supplied — the module never reads the
    /// clock so it stays deterministic in tests).
    pub at: String,
    /// `obliterate` / `obliterate-recover`.
    pub operation: String,
    /// The object address (NOT its payload).
    pub oid: String,
    /// `human` / `agent` / `automation`.
    pub approval_source: String,
    /// Who requested it (never secret material).
    pub actor: String,
    /// Redaction-clean reason string, if any.
    pub reason: Option<String>,
    /// `requested` / `payload_deleted` / `already_obliterated` / `failed`.
    pub outcome: String,
}

/// The durable audit log lives in the metadata dir (`.libra/`).
fn audit_path() -> std::path::PathBuf {
    util::storage_path().join("obliteration-audit.jsonl")
}

/// Append one record durably (create-if-missing, 0600, fsync). Failing to
/// write the mandatory audit ABORTS the operation — a destructive op must not
/// proceed unaudited.
pub fn append(record: &AuditRecord) -> CliResult<()> {
    let path = audit_path();
    let mut line = serde_json::to_vec(record).map_err(|e| {
        CliError::internal(format!(
            "failed to serialize the obliteration audit record: {e}"
        ))
    })?;
    line.push(b'\n');

    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&path).map_err(|e| {
        CliError::fatal(format!("failed to open the obliteration audit log: {e}"))
            .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;
    file.write_all(&line).map_err(|e| {
        CliError::fatal(format!(
            "failed to write the obliteration audit record: {e}"
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;
    file.sync_all().map_err(|e| {
        CliError::fatal(format!(
            "failed to fsync the obliteration audit record: {e}"
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;
    // Enforce 0600 (in case the file pre-existed with a looser mode). A
    // failure to lock the compliance log down is itself a failure of the
    // durable-audit contract, so it aborts (Codex P1).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            CliError::fatal(format!(
                "failed to lock down the obliteration audit log: {e}"
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    }
    Ok(())
}

/// Read every audit record (for `file obliterate --audit` / tests).
pub fn read_all() -> Vec<AuditRecord> {
    let path = audit_path();
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<AuditRecord>(line).ok())
        .collect()
}
