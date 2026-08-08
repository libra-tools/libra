//! `libra file obliterate` — index-flagged payload obliteration (lore.md 2.5).
//!
//! An AI-native Libra extension (no Git equivalent). Physically removes an
//! object's PAYLOAD bytes while PRESERVING its address (referencing history
//! stays traversable). Destructive and IRREVERSIBLE, so it is fail-closed: a
//! `--dry-run` preview by default, an explicit `--yes` required to proceed, a
//! mandatory durable audit record (§7.8), and a refusal to touch packed-only
//! objects (no pack surgery — that is history-rewrite territory, declined).

use std::str::FromStr;

use clap::{Parser, Subcommand};
use git_internal::hash::ObjectHash;

use crate::{
    internal::obliteration::{self, ObjectPresence, ObliterationStore, audit::AuditRecord},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

pub const FILE_EXAMPLES: &str = "\
EXAMPLES:
    libra file obliterate <oid> --dry-run       Preview the blast radius, delete nothing
    libra file obliterate <oid> --reason \"gdpr erasure\" --yes
    libra file obliterate <oid> --recover       Resume an interrupted obliteration";

/// Object-level operations (Libra extension).
#[derive(Parser, Debug)]
#[command(after_help = FILE_EXAMPLES)]
pub struct FileArgs {
    #[command(subcommand)]
    pub command: FileCommand,
}

#[derive(Subcommand, Debug)]
pub enum FileCommand {
    /// Permanently remove an object's payload bytes, keeping its address
    /// (lore.md 2.5). Destructive and irreversible.
    Obliterate {
        /// Object id (blob/tree/commit/tag) to obliterate. Omit with
        /// `--recover`.
        #[arg(required_unless_present = "recover")]
        oid: Option<String>,
        /// Redaction-clean reason recorded in the audit log.
        #[arg(long)]
        reason: Option<String>,
        /// Preview only: print the blast radius and delete nothing.
        #[arg(long)]
        dry_run: bool,
        /// Confirm the irreversible deletion (required for a real run).
        #[arg(long)]
        yes: bool,
        /// Resume any interrupted obliteration(s) left in the `obliterating`
        /// state by a crash, instead of obliterating a specific object.
        #[arg(long, conflicts_with_all = ["oid", "dry_run", "yes"])]
        recover: bool,
    },
}

pub async fn execute_safe(args: FileArgs, output: &OutputConfig) -> CliResult<()> {
    match args.command {
        FileCommand::Obliterate {
            oid,
            reason,
            dry_run,
            yes,
            recover,
        } => {
            if recover {
                return run_recover(output).await;
            }
            let oid = oid.ok_or_else(|| {
                CliError::command_usage("an object id is required (or use --recover)")
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
            })?;
            run_obliterate(&oid, reason.as_deref(), dry_run, yes, output).await
        }
    }
}

async fn run_recover(output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    // W0 §C.11 release gate: recovery DELETES payloads, so it is a deletion
    // entry point and passes the same borrower gate as a fresh obliteration.
    // `delete_payload` re-checks, but refusing up front means the audit
    // record for a recovery we will not perform is never written.
    // §C.4.3 writer-vs-deleter: held across the whole recovery, so no
    // publisher can be mid-publication while payloads are unlinked. The
    // re-check inside `delete_payload` re-enters this same hold.
    //
    // Taken BEFORE the borrower gate: registering a borrower is a
    // publication, so a gate evaluated outside the hold can be overtaken —
    // and recovery would then write a durable `payload_deleted` audit record
    // for a deletion the under-lock recheck goes on to refuse.
    let _obliteration_lock =
        crate::internal::maintenance_lock::MaintenanceLock::exclusive_or_refuse(
            &crate::utils::util::storage_path(),
            "recover an interrupted obliteration",
        )?;
    crate::internal::alternates::ensure_no_live_borrowers(
        "recover an interrupted obliteration",
        // A borrowed store is a CONFLICT, not a missing confirmation:
        // `LBR-OBLITERATE-003` documents "re-run with --yes", which the user
        // may already have done.
        StableErrorCode::ConflictOperationBlocked,
    )?;
    let completed = obliteration::recover_incomplete().await?;
    if output.is_json() {
        return emit_json_data(
            "file",
            &serde_json::json!({ "action": "obliterate-recover", "completed": completed }),
            output,
        );
    }
    if !output.quiet {
        println!("recovered {completed} interrupted obliteration(s)");
    }
    Ok(())
}

async fn run_obliterate(
    oid: &str,
    reason: Option<&str>,
    dry_run: bool,
    yes: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    // W0 §C.11 release gate: the borrower gate must come BEFORE the recovery
    // pass below, because recovery itself deletes payloads. It used to sit
    // after it, so an interrupted obliteration was completed — objects
    // unlinked — before anyone asked whether a borrower still needed them.
    // §C.4.3 writer-vs-deleter: take the EXCLUSIVE maintenance lock HERE —
    // before the borrower gate, before the recovery pass below (which
    // deletes), and before the tombstone and audit record this command
    // writes.
    //
    // Acquiring later would mean a contended lock is discovered only after a
    // durable `obliterating` row and an append-only audit line already exist,
    // and the next unrelated `file obliterate` would then run recovery and
    // complete an erasure THIS invocation reported as refused. Acquiring
    // first makes contention a clean no-op: nothing written, nothing to
    // recover. The lock is re-entrant within the process, so the re-check
    // inside `delete_payload` sees this hold rather than blocking on it.
    let _obliteration_lock =
        crate::internal::maintenance_lock::MaintenanceLock::exclusive_or_refuse(
            &crate::utils::util::storage_path(),
            "obliterate an object",
        )?;
    crate::internal::alternates::ensure_no_live_borrowers(
        "obliterate an object",
        // See `run_recover`: a borrowed store is a conflict, not a missing
        // confirmation.
        StableErrorCode::ConflictOperationBlocked,
    )?;
    // Fail CLOSED (Codex P1): finish any interrupted obliteration BEFORE a new
    // one; a recovery failure aborts rather than proceeding over an unresolved
    // mid-state tombstone.
    obliteration::recover_incomplete().await.map_err(|e| {
        e.with_hint("resolve the interrupted obliteration ('libra file obliterate --recover')")
    })?;

    let hash = ObjectHash::from_str(oid.trim()).map_err(|_| {
        CliError::fatal(format!("'{oid}' is not a valid object id"))
            .with_stable_code(StableErrorCode::ObliterateNotFound)
    })?;

    // Already obliterated → idempotent success.
    if let Some(tomb) = ObliterationStore::lookup(&hash)
        .await
        .map_err(|e| CliError::fatal(e).with_stable_code(StableErrorCode::IoReadFailed))?
        && !tomb.is_obliterating()
    {
        if !output.quiet {
            println!("{hash} is already obliterated (no-op)");
        }
        return Ok(());
    }

    // Phase A — evaluate (read-only).
    let presence = obliteration::classify_presence(&hash);
    match presence {
        ObjectPresence::Absent => {
            return Err(CliError::fatal(format!(
                "no payload found for {hash} (already absent or unknown object)"
            ))
            .with_stable_code(StableErrorCode::ObliterateNotFound));
        }
        ObjectPresence::PackedOnly => {
            // v1 refuses pack surgery (history-rewrite adjacent).
            return Err(CliError::fatal(format!(
                "{hash} exists only inside a packfile; v1 obliteration cannot rewrite packs"
            ))
            .with_stable_code(StableErrorCode::ObliteratePacked)
            .with_hint("run 'libra repack' / loosen the object first, then retry"));
        }
        ObjectPresence::LooseOnly => {}
    }

    if dry_run {
        return emit_preview(&hash, &presence, output);
    }
    if !yes {
        return Err(CliError::fatal(
            "refusing to obliterate without confirmation — this is irreversible",
        )
        .with_stable_code(StableErrorCode::ObliterateConfirm)
        .with_hint("re-run with --dry-run to preview, or --yes to confirm the deletion"));
    }

    // Phase B — Live → Obliterating (tombstone written & fsynced BEFORE any
    // payload touch).
    ObliterationStore::begin_obliterating(&hash, reason, Some("cli"), Some("human"))
        .await
        .map_err(|e| CliError::fatal(e).with_stable_code(StableErrorCode::IoWriteFailed))?;

    // Mandatory durable audit BEFORE the destructive delete.
    let now = chrono::Utc::now().to_rfc3339();
    obliteration::audit::append(&AuditRecord {
        at: now.clone(),
        operation: "obliterate".to_string(),
        oid: hash.to_string(),
        approval_source: "human".to_string(),
        actor: "cli".to_string(),
        reason: reason.map(str::to_string),
        outcome: "requested".to_string(),
    })?;

    // Phase C — physical payload delete (loose + durable tier + cache).
    obliteration::delete_payload(&hash).await?;

    // Phase D — Obliterating → Obliterated.
    ObliterationStore::mark_obliterated(&hash)
        .await
        .map_err(|e| CliError::fatal(e).with_stable_code(StableErrorCode::IoWriteFailed))?;

    obliteration::audit::append(&AuditRecord {
        at: now,
        operation: "obliterate".to_string(),
        oid: hash.to_string(),
        approval_source: "human".to_string(),
        actor: "cli".to_string(),
        reason: reason.map(str::to_string),
        outcome: "payload_deleted".to_string(),
    })?;

    if output.is_json() {
        return emit_json_data(
            "file",
            &serde_json::json!({
                "action": "obliterate",
                "oid": hash.to_string(),
                "state": "obliterated",
            }),
            output,
        );
    }
    if !output.quiet {
        println!("obliterated {hash} (payload removed; address preserved)");
    }
    Ok(())
}

fn emit_preview(
    hash: &ObjectHash,
    presence: &ObjectPresence,
    output: &OutputConfig,
) -> CliResult<()> {
    let where_str = match presence {
        ObjectPresence::LooseOnly => "loose",
        ObjectPresence::PackedOnly => "packed",
        ObjectPresence::Absent => "absent",
    };
    if output.is_json() {
        return emit_json_data(
            "file",
            &serde_json::json!({
                "action": "obliterate",
                "dry_run": true,
                "oid": hash.to_string(),
                "presence": where_str,
            }),
            output,
        );
    }
    if !output.quiet {
        println!("DRY RUN — would obliterate {hash} (payload {where_str}); address preserved");
        println!("re-run with --yes to permanently delete the payload (IRREVERSIBLE)");
    }
    Ok(())
}
