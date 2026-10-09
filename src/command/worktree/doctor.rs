//! Worktree doctor/repair: confirmations, layout preview, scope diagnostics, legacy
//! capture-scope adoption, migration recovery, and the repair driver.
#![allow(unused_imports)]
use std::{
    collections::HashSet,
    env, fs, io,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
use sea_orm::{ConnectionTrait, Statement};
use serde::Serialize;

use super::*;
#[cfg(unix)]
use crate::utils::fuse as fuse_utils;
use crate::{
    command::restore::{self, RestoreArgs},
    internal::{
        branch::Branch,
        head::Head,
        sequencer::WorktreeControl,
        workspace::{
            self, RepoIdentity, WorkspaceKind, WorkspaceRecord, WorkspaceState, WorkspaceStore,
        },
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeDoctorOutput {
    pub(crate) schema_version: u32,
    pub(crate) diagnostics: Vec<WorktreeDiagnostic>,
    pub(crate) next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorktreeDiagnostic {
    /// Stable worktree identity; `None` for main (`worktree_id IS NULL`).
    pub(crate) worktree_id: Option<String>,
    pub(crate) path: String,
    pub(crate) is_main: bool,
    /// Same vocabulary as `worktree list`: `main`, `linked-v2`,
    /// `legacy-symlink`, `missing`, `corrupt`, `task-fuse`.
    pub(crate) layout: &'static str,
    /// `active`, `detached_from_registry`, or `tombstone`.
    pub(crate) state: &'static str,
    /// Whether the registry's identity for this entry is one the entry itself
    /// still carries. `false` means mutations there are refused until
    /// `worktree repair` restores it.
    pub(crate) identity_registered: bool,
    /// W0 §C.4.1.1 origin inventory: which worktree-local `info/*` sources
    /// THIS worktree's ignore/attributes engines read (`info/exclude`,
    /// `info/attributes` under its own gitdir). Surfaced in the TEXT report
    /// today; the machine surface joins the W4 pagination extension of the
    /// frozen `worktree.doctor` envelope (which deliberately carries only
    /// the workspace half pre-W4). Per-match origin for excludes is
    /// `check-ignore -v`.
    pub(crate) info_sources: Vec<String>,
    /// Human-readable findings; empty means nothing to report.
    pub(crate) findings: Vec<String>,
}

pub(crate) fn repair_invocation_refused_without_confirmation(command: &WorktreeSubcommand) -> bool {
    match command {
        WorktreeSubcommand::Repair {
            migrate_layout,
            dry_run,
            confirm,
            resolve_identity,
            yes,
            ..
        } => {
            if *resolve_identity {
                // `--yes` is this action's dedicated confirmation.
                !*yes
            } else if *migrate_layout && *dry_run {
                // The read-only preview is confirmation-free by design.
                false
            } else {
                !*confirm
            }
        }
        _ => false,
    }
}

pub(crate) fn repair_readonly_layout_preview(command: &WorktreeSubcommand) -> bool {
    matches!(
        command,
        WorktreeSubcommand::Repair {
            migrate_layout: true,
            dry_run: true,
            ..
        }
    )
}

pub(crate) fn require_repair_confirmation(confirm: bool, action: &str) -> CliResult<()> {
    if confirm {
        return Ok(());
    }
    Err(WorktreeError::OperationBlocked(format!(
        "refusing to run {action} without confirmation; re-run with --confirm"
    ))
    .into_cli_error())
}

#[derive(Debug, Default)]
pub(crate) struct RepairOperationBoundary;

pub(crate) async fn begin_repair_operation(
    command_name: &str,
    target: Option<&str>,
) -> CliResult<RepairOperationBoundary> {
    let _ = (command_name, target);
    Ok(RepairOperationBoundary)
}

pub(crate) async fn finish_repair_operation<T>(
    boundary: RepairOperationBoundary,
    result: CliResult<T>,
) -> CliResult<T> {
    let _ = boundary;
    result
}

pub(crate) async fn collect_worktree_scope_report(
    conn: &sea_orm::DatabaseConnection,
) -> CliResult<WorktreeDoctorOutput> {
    let listed = run_list_worktrees().map_err(WorktreeError::into_cli_error)?;
    // Which identities are claimed by more than one entry, and by which paths.
    let mut duplicate_identity_paths: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for entry in &listed.worktrees {
        if entry.is_main {
            continue;
        }
        // Only LIVE registrations claim an identity — mirroring the mutation
        // guard (`identity_conflict`): a detached/tombstoned entry keeps its
        // id for attribution, and the documented `--resolve-identity` fix
        // produces exactly one Active + one Detached entry sharing an id.
        // Counting those as a collision would report a problem the
        // recommended command then refuses to act on.
        if entry.state != "active" {
            continue;
        }
        if let Some(id) = entry.worktree_id.as_deref() {
            duplicate_identity_paths
                .entry(id.to_string())
                .or_default()
                .push(entry.path.clone());
        }
    }
    let mut diagnostics = Vec::with_capacity(listed.worktrees.len());
    for entry in listed.worktrees {
        // Compare the worktree's OWN on-disk identity against the registry —
        // NOT `entry.worktree_id`, which `run_list_worktrees` already
        // resolves from the registry and which would therefore always agree
        // with it. The question this answers is whether the directory still
        // knows which registry row it belongs to.
        let identity_registered = if entry.is_main {
            // Main is spelled "no id" by convention, not by damage.
            true
        } else {
            resolve_entry_worktree_id(&entry.path, false)
                .and_then(|id| {
                    registry_knows_linked_worktree_in_storage(
                        &util::storage_path(),
                        &id,
                        Some(std::path::Path::new(&entry.path)),
                    )
                })
                .unwrap_or(false)
        };
        let mut findings = Vec::new();
        match entry.layout {
            "legacy-symlink" => findings.push(
                "uses the pre-isolation shared-`.libra` symlink layout; mutations here are \
                  refused because they would move the MAIN worktree's HEAD/index. Migrate with \
                  `libra worktree repair --migrate-layout --confirm <path>` from the main \
                  worktree."
                    .to_string(),
            ),
            "missing" => findings.push(
                "the registered directory is gone; `libra worktree prune` removes entries whose \
                  path is confirmed missing."
                    .to_string(),
            ),
            "corrupt" => findings.push(
                "the worktree's `.libra` metadata could not be read; `libra worktree repair \
                  --confirm <path>` restores its identity from the registry."
                    .to_string(),
            ),
            _ => {}
        }
        if !identity_registered && entry.layout != "missing" {
            findings.push(
                "this worktree's identity is not one the registry knows, so mutations here are \
                  refused; `libra worktree repair --confirm <path>` restores it from the \
                  registry's persisted id."
                    .to_string(),
            );
        }
        if entry.state == "detached_from_registry" {
            findings.push(
                "detached from the registry: re-attach with `libra worktree add <path>`, or \
                  finish the removal with `libra worktree remove --delete-dir <path>`."
                    .to_string(),
            );
        }
        if entry.state == "tombstone" {
            findings.push(
                "a removal did not finish cleaning up; `libra worktree repair --confirm` \
                  retries it."
                    .to_string(),
            );
        }
        // §C.4.3: layer/sparse settings whose provenance cannot be established.
        //
        // The scope migrations attributed pre-existing repository-global rows to
        // MAIN. That is right when main was always the only worktree, and
        // unprovable once a linked worktree existed and was removed before the
        // migration — its entry, and its HEAD row, are gone. Adoption is not
        // destructive (the overlay files stay on disk and their ownership rows
        // keep them unstageable), so this is REPORTED rather than blocked: the
        // user is the only one who knows whether these settings are theirs, and
        // `layer remove` / `sparse-view clear` are the ordinary way to drop them.
        if entry.is_main && crate::command::maintenance::repository_had_linked_worktrees() {
            match adopted_scope_settings_present(conn).await {
                Ok(Some(kinds)) => findings.push(format!(
                    "this worktree holds {kinds} that may have been adopted from a linked \
                      worktree removed before the scope migration — their provenance cannot be \
                      established (§C.4.3). Review them with `libra layer list` / \
                      `libra sparse-view list`; `libra layer remove <name>` and \
                      `libra sparse-view clear` drop the ones that are not yours. No file in the \
                      working tree is affected either way."
                )),
                Ok(None) => {}
                // Fail closed (§C.13): a diagnostic that could not look must say
                // so, not report "nothing to see".
                Err(error) => findings.push(format!(
                    "this repository has linked-worktree history, and whether it holds \
                      layer/sparse settings of unknown provenance COULD NOT BE DETERMINED: \
                      {error}. Treat the answer as unknown until the repository database is \
                      readable."
                )),
            }
        }
        // W0 §C.4.1.1 (plan line 2262) origin diagnostics: info files are
        // worktree-local since W0, so common `.libra/info/*` applies ONLY to
        // main. When linked worktrees exist, say so on the main entry and
        // name the explicit adopt/clear actions — never auto-copy.
        if entry.is_main && !duplicate_identity_paths.is_empty() {
            let common_info: Vec<&str> = WORKTREE_INFO_FILE_NAMES
                .iter()
                .copied()
                .filter(|name| {
                    let worktree_root = std::path::Path::new(&entry.path);
                    let path = worktree_root
                        .join(crate::utils::util::ROOT_DIR)
                        .join("info")
                        .join(name);
                    path.is_file() && info_file_has_effective_content(&path, name, worktree_root)
                })
                .collect();
            if !common_info.is_empty() {
                findings.push(format!(
                    "common info file(s) {} apply ONLY to this main worktree since W0 \
                      (info files are worktree-local; linked worktrees read their own \
                      `.libra/info/*`). Copy them into one linked worktree with \
                      `libra worktree doctor --adopt-info-to <path> --confirm`, or delete \
                      them with `libra worktree doctor --clear-common-info --confirm`.",
                    common_info
                        .iter()
                        .map(|name| format!("info/{name}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        // A registry an older binary could produce: `add A` → `move A B` →
        // `add A` keeps A's path-derived id on the moved entry, so two entries
        // claim one identity. Every MUTATION is refused while that holds, so
        // doctor has to be the place it becomes visible — and has to name both
        // paths, because the fix is choosing which one to keep.
        if let Some(id) = entry.worktree_id.as_deref()
            && !entry.is_main
            && let Some(other) = duplicate_identity_paths.get(id)
            && other.len() > 1
        {
            findings.push(format!(
                "this identity ('{id}') is claimed by {} entries ({}); every worktree MUTATION \
                  is refused until one is unregistered — \
                  `libra worktree repair <path> --resolve-identity --yes` on the one you do not \
                  want. Ordinary remove cannot do it: it needs the same loader that is refusing.",
                other.len(),
                other.join(", ")
            ));
        }
        // W0 §C.4.1.1 origin inventory: which local info sources THIS
        // worktree's ignore/attributes engines actually read — via the SAME
        // resolver the engines use (`worktree_info_file_paths`), so a
        // dual-layout tree's `.git/info/*` candidates are reported too. For
        // a legacy-symlink layout the resolution follows the symlink to
        // main's gitdir — truthfully that worktree's view until migrated.
        let info_sources: Vec<String> = {
            let worktree_root = std::path::Path::new(&entry.path);
            let mut sources = Vec::new();
            for name in WORKTREE_INFO_FILE_NAMES {
                for candidate in crate::utils::util::worktree_info_file_paths(worktree_root, name) {
                    if candidate.is_file()
                        && info_file_has_effective_content(&candidate, name, worktree_root)
                    {
                        let label = candidate
                            .strip_prefix(worktree_root)
                            .map(|relative| relative.display().to_string())
                            .unwrap_or_else(|_| candidate.display().to_string());
                        sources.push(label);
                    }
                }
            }
            sources
        };
        diagnostics.push(WorktreeDiagnostic {
            worktree_id: entry.worktree_id,
            path: entry.path,
            is_main: entry.is_main,
            layout: entry.layout,
            state: entry.state,
            identity_registered,
            info_sources,
            findings,
        });
    }
    // Stable order so a diff between two runs is meaningful.
    diagnostics.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(WorktreeDoctorOutput {
        schema_version: 1,
        diagnostics,
        next_cursor: None,
    })
}

pub(crate) fn print_worktree_scope_report(report: &WorktreeDoctorOutput) {
    let mut healthy = true;
    for diagnostic in &report.diagnostics {
        let identity = diagnostic.worktree_id.as_deref().unwrap_or("(main)");
        println!(
            "{} [{}] identity {} layout {} state {}",
            diagnostic.path,
            if diagnostic.is_main { "main" } else { "linked" },
            identity,
            diagnostic.layout,
            diagnostic.state
        );
        if !diagnostic.info_sources.is_empty() {
            println!(
                "  info sources (this worktree's own gitdir): {}",
                diagnostic.info_sources.join(", ")
            );
        }
        for finding in &diagnostic.findings {
            healthy = false;
            println!("  ! {finding}");
        }
    }
    if healthy {
        println!("no problems detected");
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct VersionedWorktreeList<'a> {
    pub(crate) schema_version: u32,
    pub(crate) worktrees: &'a [WorktreeListEntry],
}

pub(crate) async fn list_worktrees(
    output: &OutputConfig,
    porcelain: bool,
    schema_version: u32,
) -> CliResult<()> {
    if schema_version != 2 {
        return Err(CliError::failure(format!(
            "unsupported worktree list schema version {schema_version}: the shipped shape is \
              version 2 (worktree_id/layout/epoch fields); the pre-identity v1 shape gained \
              those fields in place and no frozen v1 remains to serve"
        ))
        .with_exit_code(129)
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    let result = run_list_worktrees().map_err(WorktreeError::into_cli_error)?;
    if output.is_json() {
        return emit_json_data(
            "worktree.list",
            &VersionedWorktreeList {
                schema_version: 2,
                worktrees: &result.worktrees,
            },
            output,
        );
    }
    if output.quiet {
        return Ok(());
    }
    if porcelain {
        let porcelain = format_worktree_porcelain(&result.worktrees)
            .await
            .map_err(|error| {
                CliError::fatal(format!("cannot list worktrees: {error}"))
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
                    .with_hint("a stored HEAD reference is corrupt; repair it and retry")
            })?;
        print!("{porcelain}");
        return Ok(());
    }
    for w in result.worktrees {
        let mut line = String::new();
        if w.is_main {
            line.push_str("main ");
        } else {
            line.push_str("worktree ");
        }
        line.push_str(&w.path);
        if w.locked {
            line.push_str(" [locked");
            if let Some(reason) = w.lock_reason.as_ref()
                && !reason.is_empty()
            {
                line.push_str(": ");
                line.push_str(reason);
            }
            line.push(']');
        }
        println!("{}", line);
    }
    Ok(())
}

pub(crate) fn encode_doctor_cursor(workspace_id: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(format!("{DOCTOR_CURSOR_TAG}{workspace_id}"))
}

pub(crate) fn decode_doctor_cursor(raw: &str) -> CliResult<String> {
    use base64::Engine as _;
    let invalid = || {
        CliError::fatal(format!(
            "pagination cursor '{raw}' was not issued by `libra worktree doctor` (or has expired)"
        ))
        .with_stable_code(StableErrorCode::WorktreeCursorInvalid)
        .with_hint("drop the cursor and re-read the first page")
    };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| invalid())?;
    let decoded = String::from_utf8(bytes).map_err(|_| invalid())?;
    let workspace_id = decoded
        .strip_prefix(DOCTOR_CURSOR_TAG)
        .ok_or_else(invalid)?;
    if workspace_id.is_empty() {
        return Err(invalid());
    }
    Ok(workspace_id.to_string())
}

pub(crate) fn doctor_scope_corrupt(message: impl Into<String>) -> CliError {
    CliError::fatal(message.into())
        .with_stable_code(StableErrorCode::WorktreeScopeCorrupt)
        .with_hint("repair the reported scope, then re-run `libra worktree doctor`")
}

#[derive(Debug, Serialize)]
pub(crate) struct ScopeDiagnostic {
    pub code: &'static str,
    pub severity: &'static str,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorkspaceDiagnostic {
    workspace_id: String,
    repo_id: String,
    kind: &'static str,
    state: &'static str,
    path: String,
    worktree_id: Option<String>,
    /// `none` (no lease taken) | `held` | `expired`.
    lease_state: &'static str,
    lease_owner: Option<String>,
    lease_fence: i64,
    lease_expires_at: Option<i64>,
    scope_diagnostics: Vec<ScopeDiagnostic>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeDoctorPage {
    schema_version: u32,
    diagnostics: Vec<WorkspaceDiagnostic>,
    next_cursor: Option<String>,
    /// The WORKTREE-scope findings (§C.11 W0). Carried in the same envelope
    /// as the workspace page rather than replacing it: the two halves answer
    /// different questions (is this working tree's layout/identity sound vs
    /// is this Agent workspace's lease sound) and a caller that parses one
    /// must not lose the other.
    #[serde(skip_serializing)]
    worktrees: Vec<WorktreeDiagnostic>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeDoctorSingle {
    schema_version: u32,
    diagnostic: WorkspaceDiagnostic,
}

pub(crate) fn doctor_registry_entry<'a>(
    registry: &'a WorktreeState,
    record: &WorkspaceRecord,
) -> Option<&'a WorktreeEntry> {
    if let Some(worktree_id) = record.worktree_id.as_deref()
        && let Some(entry) = registry
            .entries
            .iter()
            .find(|entry| entry.worktree_id.as_deref() == Some(worktree_id))
    {
        return Some(entry);
    }
    registry
        .entries
        .iter()
        .find(|entry| paths_are_same(&entry.path, &record.path))
}

pub(crate) fn paths_are_same(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

pub(crate) async fn stale_fence_capture_finding(
    conn: &sea_orm::DatabaseConnection,
    record: &WorkspaceRecord,
) -> Option<ScopeDiagnostic> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let mut stale = 0i64;
    for table in ["agent_session", "agent_export_job", "agent_import_identity"] {
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                format!(
                    "SELECT COUNT(*) AS n FROM {table} \
                      WHERE scope_state = 'scoped' AND workspace_id = ? \
                        AND workspace_fence <> ?"
                ),
                [
                    record.workspace_id.clone().into(),
                    record.lease_fence.into(),
                ],
            ))
            .await
            .ok()??;
        stale += row.try_get_by::<i64, _>("n").unwrap_or(0);
    }
    (stale > 0).then(|| {
        ScopeDiagnostic::warning(
            "capture_rows_stale_fence",
            format!(
                "{stale} scoped capture row(s) carry an earlier lease fence of this \
                  workspace; their owner claims are immutable, so capture/import/export \
                  writes for those provider sessions fail closed under the current fence — \
                  the rows remain readable provenance"
            ),
        )
    })
}

pub(crate) fn diagnose_workspace(
    record: &WorkspaceRecord,
    current_repo_id: &str,
    registry: &WorktreeState,
    now_ms: i64,
) -> WorkspaceDiagnostic {
    let lease_state = match (&record.lease_owner, record.lease_expires_at) {
        (None, _) => "none",
        (Some(_), Some(deadline)) if now_ms >= deadline => "expired",
        // `release` deliberately RETAINS lease_owner (provenance) while
        // nulling the expiry and leaving the live state set — so an owner
        // with no deadline is "held" only while the record is still live; a
        // released/adopted record's retained owner is history, not a lease.
        (Some(_), None)
            if matches!(
                record.state,
                crate::internal::workspace::WorkspaceState::Released
                    | crate::internal::workspace::WorkspaceState::Orphaned
            ) =>
        {
            "none"
        }
        (Some(_), _) => "held",
    };
    let mut findings = Vec::new();

    if record.repo_id != current_repo_id {
        findings.push(ScopeDiagnostic::error(
            "foreign_repository_identity",
            format!(
                "the record was written under repository identity {} but this repository is \
                  now {current_repo_id}; it is invisible to the normal listings and blocks new \
                  workspace registrations until it is settled",
                record.repo_id
            ),
        ));
    }
    if matches!(record.state, WorkspaceState::Orphaned) {
        findings.push(ScopeDiagnostic::warning(
            "workspace_orphaned",
            "teardown failed or the owner vanished; the workspace still holds recovery state",
        ));
    }
    if lease_state == "expired" {
        findings.push(ScopeDiagnostic::warning(
            "lease_expired",
            format!(
                "the lease deadline passed (expires_at {}, now {now_ms}); the lease still \
                  belongs to its owner until it is explicitly reclaimed",
                record.lease_expires_at.unwrap_or_default()
            ),
        ));
    }
    if !Path::new(&record.path).exists() {
        findings.push(ScopeDiagnostic::warning(
            "workspace_path_missing",
            format!("no directory at the claimed path '{}'", record.path),
        ));
    }

    if matches!(record.kind, WorkspaceKind::Linked) {
        match doctor_registry_entry(registry, record) {
            None => findings.push(ScopeDiagnostic::warning(
                "registry_entry_missing",
                format!(
                    "no worktree registry entry owns this scope (worktree_id {}, path '{}')",
                    record.worktree_id.as_deref().unwrap_or("<unset>"),
                    record.path
                ),
            )),
            Some(entry) => {
                if !paths_are_same(&entry.path, &record.path) {
                    findings.push(ScopeDiagnostic::error(
                        "registry_path_mismatch",
                        format!(
                            "the registry entry for this scope lives at '{}' but the workspace \
                              record claims '{}'",
                            entry.path, record.path
                        ),
                    ));
                }
                match entry.state {
                    WorktreeEntryState::Active => {}
                    WorktreeEntryState::DetachedFromRegistry => {
                        findings.push(ScopeDiagnostic::warning(
                            "registry_entry_detached",
                            "the worktree was unregistered with `worktree remove` (keep-dir); \
                              commands inside it fail closed until it is re-added",
                        ));
                    }
                    WorktreeEntryState::Tombstone => {
                        findings.push(ScopeDiagnostic::warning(
                            "registry_entry_tombstoned",
                            "the worktree directory was deleted but its scoped rows are still \
                              pending cleanup; `libra worktree repair --confirm` retries it",
                        ));
                    }
                }
                match detect_entry_layout(Path::new(&entry.path), entry.is_main) {
                    "corrupt" => findings.push(ScopeDiagnostic::error(
                        "scope_layout_corrupt",
                        format!("the gitdir layout at '{}' is unrecognizable", entry.path),
                    )),
                    "legacy-symlink" => findings.push(ScopeDiagnostic::warning(
                        "scope_layout_legacy_symlink",
                        format!(
                            "'{}' still uses the pre-isolation shared-`.libra` symlink layout; \
                              migrate it with `libra worktree repair --migrate-layout --confirm`",
                            entry.path
                        ),
                    )),
                    _ => {}
                }
            }
        }
    }

    WorkspaceDiagnostic {
        workspace_id: record.workspace_id.clone(),
        repo_id: record.repo_id.clone(),
        kind: record.kind.as_db_value(),
        state: record.state.as_db_value(),
        path: record.path.clone(),
        worktree_id: record.worktree_id.clone(),
        lease_state,
        lease_owner: record.lease_owner.clone(),
        lease_fence: record.lease_fence,
        lease_expires_at: record.lease_expires_at,
        scope_diagnostics: findings,
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct CaptureScopeAdoptionOutput {
    schema_version: u32,
    session_id: String,
    workspace_id: String,
    repo_id: String,
    worktree_id: String,
    workspace_fence: i64,
}

pub(crate) async fn adopt_or_clear_legacy_approved_project(
    legacy_project_id: &str,
    confirm: bool,
    clear: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    use crate::internal::ai::permission::ApprovedRulesetStore;

    let action = if clear {
        "worktree doctor --clear-approved-project"
    } else {
        "worktree doctor --adopt-approved-project"
    };
    require_repair_confirmation(confirm, action)?;

    let db_path = crate::utils::path::database();
    let conn = crate::internal::db::get_db_conn_instance_for_path(&db_path)
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "cannot open the repository database for approved_permission recovery: {error}"
            ))
        })?;

    // Open the audit boundary first. Only if the sole blocker is a missing
    // libra.repoid do we mint one and retry — so a held control slot never
    // causes an unaudited identity write.
    let boundary = match begin_repair_operation(action, Some(legacy_project_id)).await {
        Ok(boundary) => boundary,
        Err(error)
            if error
                .to_string()
                .contains("repository identity (libra.repoid) is missing") =>
        {
            ApprovedRulesetStore::ensure_repo_identity(&conn)
                .await
                .map_err(|init_error| {
                    CliError::fatal(format!(
                        "cannot initialize libra.repoid before approved_permission recovery: \
                          {init_error}"
                    ))
                })?;
            begin_repair_operation(action, Some(legacy_project_id)).await?
        }
        Err(error) => return Err(error),
    };

    let result = async {
        if clear {
            let removed = ApprovedRulesetStore::clear_legacy_project_id(&conn, legacy_project_id)
                .await
                .map_err(|error| {
                    CliError::fatal(format!(
                        "cannot clear approved_permission project_id '{legacy_project_id}': \
                              {error}"
                    ))
                })?;
            Ok(serde_json::json!({
                "action": "clear",
                "legacy_project_id": legacy_project_id,
                "rows_affected": removed,
            }))
        } else {
            let adopted = ApprovedRulesetStore::adopt_legacy_project_id(&conn, legacy_project_id)
                .await
                .map_err(|error| {
                    CliError::fatal(format!(
                        "cannot adopt approved_permission project_id '{legacy_project_id}': \
                              {error}"
                    ))
                })?;
            Ok(serde_json::json!({
                "action": "adopt",
                "legacy_project_id": legacy_project_id,
                "rows_affected": adopted,
            }))
        }
    }
    .await;

    let payload = finish_repair_operation(boundary, result).await?;
    if output.is_json() {
        let command = if clear {
            "worktree.doctor.clear_approved_project"
        } else {
            "worktree.doctor.adopt_approved_project"
        };
        return emit_json_data(command, &payload, output);
    }
    if clear {
        println!(
            "cleared {} approved_permission row(s) under legacy project_id '{legacy_project_id}'",
            payload["rows_affected"]
        );
    } else {
        println!(
            "adopted {} approved_permission row(s) from legacy project_id '{legacy_project_id}' \
              onto libra.repoid",
            payload["rows_affected"]
        );
    }
    Ok(())
}

pub(crate) async fn adopt_legacy_capture_scope(
    workspace_id: &str,
    session_id: &str,
    confirm: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    if !confirm {
        return Err(CliError::command_usage(
            "legacy capture adoption changes persistent ownership; re-run with --confirm after \
              verifying the workspace and session",
        ));
    }
    let db_path = crate::utils::path::database();
    let conn = crate::internal::db::get_db_conn_instance_for_path(&db_path)
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "cannot open the repository database for capture-scope adoption: {error}"
            ))
        })?;
    let record = WorkspaceStore::get_with_conn(&conn, workspace_id)
        .await
        .map_err(|error| {
            CliError::fatal(format!("cannot read workspace '{workspace_id}': {error}"))
        })?
        .ok_or_else(|| {
            CliError::fatal(format!(
                "no workspace matches id '{workspace_id}'; list them with `libra worktree doctor`"
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?;
    if !record.state.holds_identity() || record.lease_owner.is_none() || record.lease_fence <= 0 {
        return Err(CliError::fatal(format!(
            "workspace '{workspace_id}' has no live lease fence; refuse to attribute capture \
              state to a released, orphaned, or unleased scope"
        )));
    }
    if record
        .lease_expires_at
        .is_none_or(|deadline| deadline <= workspace::now_ms())
    {
        return Err(CliError::fatal(format!(
            "workspace '{workspace_id}' lease has expired; refuse to attribute capture state \
              until its owner renews or an explicit reclaim issues a new fence"
        )));
    }
    let identity = RepoIdentity::resolve(&conn).await.map_err(|error| {
        CliError::fatal(format!(
            "cannot resolve this repository's identity for capture-scope adoption: {error}"
        ))
    })?;
    let target_worktree_id = record.worktree_id.clone().unwrap_or_default();
    let txn = crate::internal::db::begin_write_transaction(&conn)
        .await
        .map_err(|error| CliError::fatal(format!("begin capture-scope adoption: {error}")))?;
    let legacy = txn
         .query_one_raw(Statement::from_sql_and_values(
             txn.get_database_backend(),
             "SELECT agent_kind, provider_session_id FROM (
                  SELECT 0 AS match_priority, agent_kind, provider_session_id FROM agent_session
                   WHERE scope_state = 'legacy_unknown' AND session_id = ?
                  UNION ALL
                  SELECT 1 AS match_priority, agent_kind, provider_session_id FROM agent_session
                   WHERE scope_state = 'legacy_unknown' AND provider_session_id = ?
                  UNION ALL
                  SELECT 1 AS match_priority, agent_kind, provider_session_id FROM agent_export_job
                   WHERE scope_state = 'legacy_unknown' AND provider_session_id = ?
                  UNION ALL
                  SELECT 1 AS match_priority, agent_kind, provider_session_id FROM agent_import_identity
                   WHERE scope_state = 'legacy_unknown' AND provider_session_id = ?
              ) ORDER BY match_priority LIMIT 1",
             [
                 session_id.into(),
                 session_id.into(),
                 session_id.into(),
                 session_id.into(),
             ],
         ))
         .await
         .map_err(|error| CliError::fatal(format!("read legacy capture session: {error}")))?
         .ok_or_else(|| {
             CliError::fatal(format!(
                 "capture session identifier '{session_id}' has no legacy unscoped capture row; \
                  doctor adoption accepts an agent session id or an orphan provider session id"
             ))
         })?;
    let agent_kind: String = legacy
        .try_get_by("agent_kind")
        .map_err(|error| CliError::fatal(format!("decode legacy capture agent kind: {error}")))?;
    let provider_session_id: String =
        legacy.try_get_by("provider_session_id").map_err(|error| {
            CliError::fatal(format!("decode legacy provider session identity: {error}"))
        })?;
    let foreign_scoped = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 FROM (
                  SELECT provider_session_id FROM agent_session
                   WHERE provider_session_id = ? AND scope_state = 'scoped'
                  UNION ALL
                  SELECT provider_session_id FROM agent_export_job
                   WHERE provider_session_id = ? AND scope_state = 'scoped'
                  UNION ALL
                  SELECT provider_session_id FROM agent_import_identity
                   WHERE provider_session_id = ? AND scope_state = 'scoped'
              ) LIMIT 1",
            [
                provider_session_id.clone().into(),
                provider_session_id.clone().into(),
                provider_session_id.clone().into(),
            ],
        ))
        .await
        .map_err(|error| {
            CliError::fatal(format!("check existing scoped capture ownership: {error}"))
        })?;
    if foreign_scoped.is_some() {
        txn.rollback().await.ok();
        return Err(CliError::fatal(format!(
            "provider session '{provider_session_id}' already has a scoped capture claim; \
              refusing to merge a legacy row into it"
        )));
    }
    let target_repo_id = identity.as_str().to_string();
    let target_workspace_id = record.workspace_id.clone();
    let target_workspace_fence = record.lease_fence;
    let mut adopted_rows = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_session
              SET repo_id = ?, worktree_id = ?, workspace_id = ?, workspace_fence = ?,
                  scope_state = 'scoped'
              WHERE provider_session_id = ? AND scope_state = 'legacy_unknown'
                AND EXISTS (
                    SELECT 1 FROM workspace_record
                    WHERE workspace_id = ? AND repo_id = ? AND lease_fence = ?
                      AND state IN ('provisioning', 'active', 'releasing')
                      AND lease_owner IS NOT NULL
                      AND lease_expires_at > (unixepoch('now') * 1000)
                )",
            [
                target_repo_id.clone().into(),
                target_worktree_id.clone().into(),
                target_workspace_id.clone().into(),
                target_workspace_fence.into(),
                provider_session_id.clone().into(),
                target_workspace_id.clone().into(),
                target_repo_id.clone().into(),
                target_workspace_fence.into(),
            ],
        ))
        .await
        .map_err(|error| CliError::fatal(format!("adopt legacy capture session: {error}")))?
        .rows_affected();
    for table in ["agent_export_job", "agent_import_identity"] {
        let sql = format!(
            "UPDATE {table}
              SET repo_id = ?, worktree_id = ?, workspace_id = ?, workspace_fence = ?,
                  scope_state = 'scoped'
              WHERE provider_session_id = ? AND scope_state = 'legacy_unknown'
                AND EXISTS (
                    SELECT 1 FROM workspace_record
                    WHERE workspace_id = ? AND repo_id = ? AND lease_fence = ?
                      AND state IN ('provisioning', 'active', 'releasing')
                      AND lease_owner IS NOT NULL
                      AND lease_expires_at > (unixepoch('now') * 1000)
                )"
        );
        adopted_rows += txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                sql,
                [
                    target_repo_id.clone().into(),
                    target_worktree_id.clone().into(),
                    target_workspace_id.clone().into(),
                    target_workspace_fence.into(),
                    provider_session_id.clone().into(),
                    target_workspace_id.clone().into(),
                    target_repo_id.clone().into(),
                    target_workspace_fence.into(),
                ],
            ))
            .await
            .map_err(|error| CliError::fatal(format!("adopt legacy {table} scope: {error}")))?
            .rows_affected();
    }
    if adopted_rows == 0 {
        txn.rollback().await.ok();
        return Err(CliError::fatal(
            "capture-scope adoption was fenced out because the target workspace changed; rerun \
              doctor and choose the current live workspace",
        ));
    }
    let actor = env::var("LIBRA_ACTOR")
        .ok()
        .or_else(|| env::var("USER").ok());
    txn.execute_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO agent_workspace_scope_audit (
             audit_id, action, agent_kind, provider_session_id, repo_id, worktree_id,
             workspace_id, workspace_fence, actor, created_at
          ) VALUES (?, 'adopt_legacy_capture_scope', ?, ?, ?, ?, ?, ?, ?, ?)",
        [
            uuid::Uuid::new_v4().to_string().into(),
            agent_kind.into(),
            provider_session_id.into(),
            target_repo_id.into(),
            target_worktree_id.clone().into(),
            target_workspace_id.into(),
            target_workspace_fence.into(),
            actor.into(),
            workspace::now_ms().into(),
        ],
    ))
    .await
    .map_err(|error| CliError::fatal(format!("audit capture-scope adoption: {error}")))?;
    txn.commit()
        .await
        .map_err(|error| CliError::fatal(format!("commit capture-scope adoption: {error}")))?;

    let payload = CaptureScopeAdoptionOutput {
        schema_version: DOCTOR_SCHEMA_VERSION,
        session_id: session_id.to_string(),
        workspace_id: record.workspace_id,
        repo_id: identity.as_str().to_string(),
        worktree_id: target_worktree_id,
        workspace_fence: record.lease_fence,
    };
    if output.is_json() {
        return emit_json_data("worktree.doctor.adopt_capture", &payload, output);
    }
    if !output.quiet {
        println!(
            "adopted legacy capture session into workspace {} at lease fence {}",
            payload.workspace_id, payload.workspace_fence
        );
    }
    Ok(())
}

pub(crate) async fn legacy_capture_scope_exists(
    conn: &sea_orm::DatabaseConnection,
) -> CliResult<bool> {
    let result = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT 1 FROM (
                  SELECT 1 FROM agent_session WHERE scope_state = 'legacy_unknown'
                  UNION ALL
                  SELECT 1 FROM agent_export_job WHERE scope_state = 'legacy_unknown'
                  UNION ALL
                  SELECT 1 FROM agent_import_identity WHERE scope_state = 'legacy_unknown'
              ) LIMIT 1"
                .to_string(),
        ))
        .await;
    match result {
        Ok(row) => Ok(row.is_some()),
        Err(error)
            if error
                .to_string()
                .to_ascii_lowercase()
                .contains("no such column")
                || error
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("no such table") =>
        {
            Ok(false)
        }
        Err(error) => Err(doctor_scope_corrupt(format!(
            "cannot inspect legacy capture scope state: {error}"
        ))),
    }
}

pub(crate) fn print_legacy_capture_scope_guidance(output: &OutputConfig, legacy_exists: bool) {
    if legacy_exists && !output.is_json() && !output.quiet {
        println!(
            "legacy capture scope: unscoped capture rows exist and are intentionally excluded \
              from new writes; inspect the session and adopt only its verified owner with \
              `libra worktree doctor <workspace-id> --adopt-capture-session <session-id> --confirm`"
        );
    }
}

pub(crate) fn print_legacy_approved_project_guidance(output: &OutputConfig, legacy_ids: &[String]) {
    if legacy_ids.is_empty() || output.quiet {
        return;
    }
    let listed = legacy_ids.join(", ");
    let message = format!(
        "legacy approved_permission project_id(s): {listed}; they are invisible to the runtime \
          until adopted with `libra worktree doctor --adopt-approved-project <id> --confirm` or \
          removed with `libra worktree doctor --clear-approved-project <id> --confirm`"
    );
    if output.is_json() {
        // Keep the frozen worktree.doctor JSON page schema untouched; surface
        // the recovery IDs on stderr so machine callers can still discover them.
        eprintln!("{message}");
    } else {
        println!("{message}");
    }
}

pub(crate) async fn list_legacy_approved_project_ids_readonly(
    conn: &sea_orm::DatabaseConnection,
) -> CliResult<Vec<String>> {
    use crate::internal::ai::permission::ApprovedRulesetStore;

    match ApprovedRulesetStore::list_legacy_project_ids(conn).await {
        Ok(ids) => Ok(ids),
        Err(error) => {
            let text = error.to_string().to_ascii_lowercase();
            if text.contains("no such table") || text.contains("no such column") {
                Ok(Vec::new())
            } else {
                Err(doctor_scope_corrupt(format!(
                    "cannot list legacy approved_permission project_id values: {error}"
                )))
            }
        }
    }
}

pub(crate) async fn run_worktree_doctor(
    workspace_id: Option<String>,
    limit: Option<u64>,
    cursor: Option<String>,
    output: &OutputConfig,
) -> CliResult<()> {
    // A single scope is not a page: rejecting the combination keeps the two
    // frozen response shapes unambiguous (Codex R19).
    if workspace_id.is_some() && (limit.is_some() || cursor.is_some()) {
        return Err(CliError::command_usage(
            "`libra worktree doctor <workspace-id>` diagnoses one scope and takes no \
              --limit/--cursor; drop the id for the paginated view",
        ));
    }

    // Read-only registry snapshot (lockless reader — never rewrites the file).
    // A registry this command cannot parse IS corrupt scope state: refuse
    // rather than report a diagnosis built on unknown ownership.
    let registry = load_state_readonly().map_err(|error| {
        doctor_scope_corrupt(format!("cannot read the worktree registry: {error}"))
    })?;

    // Open WITHOUT applying migrations (§C.11 W0): running the pending
    // migrations of the repository you are diagnosing is a write, and it
    // changes the very thing you came to observe — on a repository behind
    // schema, `doctor` is the one command you must be able to run without
    // committing to an upgrade. `cli.rs` already exempts this command from
    // the schema guard for the same reason.
    let db_path = crate::utils::path::database();
    let conn = crate::internal::db::open_database_without_migrations(&db_path)
        .await
        .map_err(|error| {
            doctor_scope_corrupt(format!(
                "cannot open the repository database at '{}': {error}",
                db_path.display()
            ))
        })?;
    let now = workspace::now_ms();
    let legacy_capture_exists = legacy_capture_scope_exists(&conn).await?;
    let legacy_approved_ids = list_legacy_approved_project_ids_readonly(&conn).await?;

    if let Some(workspace_id) = workspace_id {
        let record = match WorkspaceStore::doctor_record_with_conn(&conn, &workspace_id).await {
            Ok(record) => record,
            // Same special case as the paginated path below: a repository
            // that never ran the workspace migration has no records — its
            // TRUE state, not scope corruption, and the actionable answer
            // is "no such workspace", not LBR-WORKTREE-002.
            Err(error) if workspace_table_absent(&error) => None,
            Err(error) => {
                return Err(doctor_scope_corrupt(format!(
                    "cannot read the workspace record '{workspace_id}': {error}"
                )));
            }
        }
        .ok_or_else(|| {
            CliError::fatal(format!(
                "no workspace matches id '{workspace_id}'; list them with \
                      `libra worktree doctor`"
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?;
        let repo_id = doctor_repo_identity(&conn).await?;
        let mut diagnostic = diagnose_workspace(&record, &repo_id, &registry, now);
        if let Some(finding) = stale_fence_capture_finding(&conn, &record).await {
            diagnostic.scope_diagnostics.push(finding);
        }
        let result = render_doctor_single(diagnostic, output);
        print_legacy_capture_scope_guidance(output, legacy_capture_exists);
        print_legacy_approved_project_guidance(output, &legacy_approved_ids);
        return result;
    }

    let after = cursor.as_deref().map(decode_doctor_cursor).transpose()?;
    let page = match WorkspaceStore::doctor_page_with_conn(&conn, limit, after.as_deref()).await {
        Ok(page) => page,
        // A repository that has not yet run the workspace migration has no
        // records to diagnose — that is its true state, not a corrupt one.
        // Every other read failure still fails closed.
        Err(error) if workspace_table_absent(&error) => workspace::WorkspacePage {
            items: Vec::new(),
            next_cursor: None,
        },
        Err(error) => {
            return Err(doctor_scope_corrupt(format!(
                "cannot read the workspace registry: {error}"
            )));
        }
    };
    // The human doctor also renders the W0 worktree-scope report on the same
    // no-migration connection. Do not build it for JSON/machine pagination:
    // it scans every registry entry, would make a capped workspace page
    // unbounded, and is not part of the frozen W4 JSON payload.
    let worktrees = if output.is_json() {
        Vec::new()
    } else {
        collect_worktree_scope_report(&conn).await?.diagnostics
    };
    if page.items.is_empty() {
        // Nothing to classify, so a missing/unreadable repository identity is
        // not worth failing on: an empty diagnosis IS the whole truth here.
        let result = render_doctor_page(
            WorktreeDoctorPage {
                schema_version: DOCTOR_SCHEMA_VERSION,
                diagnostics: Vec::new(),
                next_cursor: None,
                worktrees,
            },
            output,
        );
        print_legacy_capture_scope_guidance(output, legacy_capture_exists);
        print_legacy_approved_project_guidance(output, &legacy_approved_ids);
        return result;
    }
    let repo_id = doctor_repo_identity(&conn).await?;
    let diagnostics = page
        .items
        .iter()
        .map(|record| diagnose_workspace(record, &repo_id, &registry, now))
        .collect();
    let result = render_doctor_page(
        WorktreeDoctorPage {
            schema_version: DOCTOR_SCHEMA_VERSION,
            diagnostics,
            next_cursor: page.next_cursor.as_deref().map(encode_doctor_cursor),
            worktrees,
        },
        output,
    );
    print_legacy_capture_scope_guidance(output, legacy_capture_exists);
    print_legacy_approved_project_guidance(output, &legacy_approved_ids);
    result
}

pub(crate) fn workspace_table_absent(error: &workspace::WorkspaceError) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains("no such table") && text.contains("workspace_record")
}

pub(crate) async fn doctor_repo_identity(conn: &sea_orm::DatabaseConnection) -> CliResult<String> {
    RepoIdentity::resolve(conn)
        .await
        .map(|identity| identity.as_str().to_string())
        .map_err(|error| {
            doctor_scope_corrupt(format!(
                "cannot read this repository's identity, so workspace records cannot be \
                  attributed: {error}"
            ))
        })
}

pub(crate) fn render_doctor_single(
    diagnostic: WorkspaceDiagnostic,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        let payload = WorktreeDoctorSingle {
            schema_version: DOCTOR_SCHEMA_VERSION,
            diagnostic,
        };
        return emit_json_data("worktree.doctor", &payload, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!("workspace_id: {}", diagnostic.workspace_id);
    println!("repo_id:      {}", diagnostic.repo_id);
    println!("kind:         {}", diagnostic.kind);
    println!("state:        {}", diagnostic.state);
    println!("path:         {}", diagnostic.path);
    if let Some(worktree_id) = &diagnostic.worktree_id {
        println!("worktree_id:  {worktree_id}");
    }
    println!("lease_state:  {}", diagnostic.lease_state);
    if let Some(owner) = &diagnostic.lease_owner {
        println!("lease_owner:  {owner} (fence {})", diagnostic.lease_fence);
    }
    print_scope_diagnostics(&diagnostic.scope_diagnostics);
    Ok(())
}

pub(crate) fn render_doctor_page(page: WorktreeDoctorPage, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.doctor", &page, output);
    }
    if output.quiet {
        return Ok(());
    }
    print_worktree_scope_report(&WorktreeDoctorOutput {
        schema_version: page.schema_version,
        diagnostics: page.worktrees.clone(),
        next_cursor: None,
    });
    if page.diagnostics.is_empty() {
        println!("(no workspace records to diagnose)");
        return Ok(());
    }
    for diagnostic in &page.diagnostics {
        println!(
            "{}  {:12} lease={:7} {}",
            diagnostic.workspace_id, diagnostic.state, diagnostic.lease_state, diagnostic.path
        );
        print_scope_diagnostics(&diagnostic.scope_diagnostics);
    }
    if let Some(cursor) = &page.next_cursor {
        println!("(more rows: --cursor {cursor})");
    }
    Ok(())
}

pub(crate) fn print_scope_diagnostics(findings: &[ScopeDiagnostic]) {
    for finding in findings {
        println!(
            "  {:7} {}: {}",
            finding.severity, finding.code, finding.detail
        );
    }
}

pub(crate) fn restore_marker_on_refusal(
    entry_is_detached: bool,
    target: &Path,
    worktree_id: Option<&str>,
) -> bool {
    // Restore UNCONDITIONALLY for detached entries: the marker may have
    // been lifted for the dirty check, or may have been missing already
    // (a crash between registry publication and marker creation) — either
    // way, a refused delete must leave the directory frozen again.
    if !entry_is_detached {
        return true;
    }
    let Some(id) = worktree_id else {
        return false;
    };
    match write_detached_marker(target, id) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                %error,
                "could not restore the detached marker after a refused delete; the \
                 pending journal row lets `worktree repair` re-freeze the directory"
            );
            false
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct MigrateLayoutOutput {
    dry_run: bool,
    migrated: Vec<String>,
    planned: Vec<String>,
    skipped: Vec<String>,
}

pub(crate) async fn migrate_layout_run(
    filter: Option<String>,
    dry_run: bool,
) -> WorktreeResult<MigrateLayoutOutput> {
    if util::current_worktree_id().is_some() || util::is_legacy_symlink_worktree() {
        return Err(WorktreeError::OperationBlocked(
            "run `worktree repair --migrate-layout --confirm` from the MAIN worktree".to_string(),
        ));
    }
    // Dry run is READ-ONLY end to end: no lock file creation, no registry
    // upgrade, and NO database open at all — the lockless reader is enough to
    // enumerate layouts, and even a migration-applying connection would be a
    // write on the repository being previewed (§C.11, Codex R20).
    let (_registry_lock, state) = if dry_run {
        (None, load_state_readonly()?)
    } else {
        let guard = acquire_registry_lock_async().await?;
        let state = load_state()?;
        (Some(guard), state)
    };
    let filter_path = match &filter {
        Some(raw) => Some(resolve_path(raw, "worktree path")?),
        None => None,
    };

    let mut targets = Vec::new();
    let mut skipped = Vec::new();
    for (idx, entry) in state.entries.iter().enumerate() {
        if entry.is_main {
            continue;
        }
        if let Some(want) = &filter_path
            && Path::new(&entry.path) != want
        {
            continue;
        }
        match detect_entry_layout(Path::new(&entry.path), false) {
            "legacy-symlink" => targets.push(idx),
            layout => {
                if filter_path.is_some() {
                    return Err(WorktreeError::InvalidTarget(format!(
                        "'{}' is not a legacy-symlink worktree (layout: {layout})",
                        entry.path
                    )));
                }
                skipped.push(format!("{} ({layout})", entry.path));
            }
        }
    }
    if let Some(want) = &filter_path
        && targets.is_empty()
    {
        return Err(WorktreeError::NoSuchWorktree {
            path: want.to_string_lossy().to_string(),
        });
    }

    if dry_run {
        return Ok(MigrateLayoutOutput {
            dry_run: true,
            migrated: Vec::new(),
            planned: targets
                .into_iter()
                .map(|idx| state.entries[idx].path.clone())
                .collect(),
            skipped,
        });
    }

    // Confirmed execution only: the dry-run preview returned above without
    // any database access, so this connection never resolves on the read-only
    // path. (Migrations were already applied by the CLI preflight and the
    // dispatch-time open — the §C.7 contract for confirmed repair actions.)
    let db = crate::internal::db::get_db_conn_instance().await;

    // Preconditions shared by every target (§C.6.2 step 4): the SHARED
    // index must be conflict-free, no repository-global (main-scope)
    // sequencer may be active, and HEAD must be readable and born.
    let shared_index = git_internal::internal::index::Index::load(crate::utils::path::index())
        .map_err(|e| WorktreeError::IoRead(format!("cannot read the shared index: {e}")))?;
    if !crate::command::unmerged::collect(&shared_index).is_empty() {
        return Err(WorktreeError::OperationBlocked(
            "the shared index has unmerged (conflict) entries; resolve or abort the \
              conflict in the main worktree first"
                .to_string(),
        ));
    }
    if scoped_state_active(&db, "").await {
        return Err(WorktreeError::OperationBlocked(
            "an in-progress rebase/cherry-pick/bisect is active in the main worktree; \
              finish or abort it first"
                .to_string(),
        ));
    }
    // A legacy-symlink worktree SHARES common storage, so an active merge/
    // revert or held autostash there belongs to some worktree that must
    // conclude it before the layout underneath it changes.
    refuse_active_sidecar_state(&util::storage_path(), "migrating the layout")?;
    let head_commit = Head::current_commit_result()
        .await
        .map_err(|e| WorktreeError::IoRead(format!("cannot read the shared HEAD: {e}")))?
        .ok_or_else(|| {
            WorktreeError::OperationBlocked(
                "the repository has no commits yet; nothing to migrate onto".to_string(),
            )
        })?;

    let mut state = state;
    let mut migrated = Vec::new();
    for idx in targets {
        let path = state.entries[idx].path.clone();
        migrate_one_worktree(&db, &mut state, idx, head_commit).await?;
        migrated.push(path);
    }

    Ok(MigrateLayoutOutput {
        dry_run: false,
        migrated,
        planned: Vec::new(),
        skipped,
    })
}

pub(crate) async fn migrate_one_worktree(
    db: &sea_orm::DatabaseConnection,
    state: &mut WorktreeState,
    index: usize,
    head_commit: git_internal::hash::ObjectHash,
) -> WorktreeResult<()> {
    let target = PathBuf::from(&state.entries[index].path);
    let gitdir = target.join(util::ROOT_DIR);
    let storage = util::storage_path();
    let canonical_storage = fs::canonicalize(&storage).unwrap_or_else(|_| storage.clone());

    // Step 1: no-follow — `.libra` must BE a symlink resolving exactly to
    // the common storage. Anything else is refused untouched.
    let meta = fs::symlink_metadata(&gitdir)
        .map_err(|e| WorktreeError::IoRead(format!("cannot stat '{}': {e}", gitdir.display())))?;
    if !meta.file_type().is_symlink()
        || fs::canonicalize(&gitdir).ok().as_deref() != Some(canonical_storage.as_path())
    {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' is not a legacy symlink at this repository's storage; refusing",
            gitdir.display()
        )));
    }

    let worktree_id = state.entries[index].worktree_id.clone().ok_or_else(|| {
        WorktreeError::OperationBlocked(format!(
            "registry entry '{}' has no persisted worktree id; run `libra worktree repair \
                  --confirm` first",
            target.display()
        ))
    })?;
    // A target with an UNRESOLVED earlier migrate intent must be settled by
    // `worktree repair` first — starting a second migration would leave the
    // first row permanently ambiguous (recovery adopts only its own
    // journal-stamped marker).
    let pending = journal_pending(db)
        .await
        .map_err(WorktreeError::OperationBlocked)?;
    if pending.iter().any(|intent| {
        intent.op == "migrate"
            && intent.payload["path"].as_str() == Some(target.to_string_lossy().as_ref())
    }) {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' has an unresolved earlier migration journal; run `libra worktree \
              repair --confirm` to settle it, then retry",
            target.display()
        )));
    }
    let payload = serde_json::json!({
        "path": target.to_string_lossy(),
        "head": head_commit.to_string(),
    });
    let journal_id = journal_append(
        db,
        WorktreeControl::MigrateLayout.declare(),
        Some(&worktree_id),
        &payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;
    let set_stage = |stage: &'static str| {
        let db = db.clone();
        async move {
            journal_set_stage(&db, journal_id, stage)
                .await
                .map_err(WorktreeError::OperationBlocked)
        }
    };

    // Step 2: prepared gitdir with identity marker, fsynced.
    let prepared = target.join(format!(".libra.migrate-{journal_id}"));
    fs::create_dir_all(&prepared).map_err(|e| {
        WorktreeError::IoWrite(format!("cannot create '{}': {e}", prepared.display()))
    })?;
    let write = |name: &str, contents: String| -> WorktreeResult<()> {
        crate::utils::atomic_write::write_atomic(&prepared.join(name), contents.as_bytes(), true)
            .map_err(|e| {
                WorktreeError::IoWrite(format!("cannot write '{}/{name}': {e}", prepared.display()))
            })
    };
    // The MARKER is written FIRST: recovery identifies the prepared dir by
    // this marker, so a crash after `create_dir_all` but before the marker
    // used to manufacture a directory recovery could never settle (the
    // journal stayed pending forever, and a fresh migration was refused by
    // the pending-journal guard). Marker-first closes that window — every
    // crash from here on leaves an identifiable artifact.
    write("migrate-marker", format!("journal {journal_id}\n"))?;
    write("commondir", format!("{}\n", canonical_storage.display()))?;
    write("worktree_id", format!("{worktree_id}\n"))?;
    fsync_parent_best_effort(&prepared.join("commondir"));
    fsync_parent_best_effort(&prepared);
    set_stage("PreparedLocalGitdir").await?;

    // Step 5a: atomic rename of the symlink to a journal-identified backup.
    let backup = target.join(format!(".libra.legacy-backup-{journal_id}"));
    match fs::symlink_metadata(&backup) {
        Ok(_) => {
            return Err(WorktreeError::OperationBlocked(format!(
                "'{}' already exists; remove it manually, then retry",
                backup.display()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(WorktreeError::IoRead(format!(
                "cannot inspect '{}': {error}",
                backup.display()
            )));
        }
    }
    let _gate_bypass = util::bypass_migration_gate();
    // Atomic NO-REPLACE backup on Unix: symlink(2) fails with EEXIST if a
    // concurrent actor claimed the name after the check above, so user
    // material can never be silently overwritten by a replace-capable
    // rename. (A crash between the two steps leaves the legacy link intact
    // — the pre-backup recovery branch removes this identity-named backup.)
    #[cfg(unix)]
    {
        let dest = fs::read_link(&gitdir).map_err(|e| {
            WorktreeError::IoRead(format!("cannot read the legacy link target: {e}"))
        })?;
        std::os::unix::fs::symlink(&dest, &backup).map_err(|e| {
            WorktreeError::IoWrite(format!("cannot back up the legacy symlink: {e}"))
        })?;
        // Durability order: the backup's directory entry must be on disk
        // BEFORE the original link disappears — a power loss in between
        // must never leave neither. This sync is STRICT: if the filesystem
        // cannot prove the entry durable, abort with the backup removed and
        // the legacy link untouched.
        if let Err(error) = fsync_parent_strict(&backup) {
            let _ = fs::remove_file(&backup);
            return Err(WorktreeError::IoWrite(format!(
                "cannot durably record the backup link ({error}); migration aborted with \
                  the legacy link untouched"
            )));
        }
        fs::remove_file(&gitdir).map_err(|e| {
            WorktreeError::IoWrite(format!("cannot retire the legacy symlink: {e}"))
        })?;
    }
    #[cfg(not(unix))]
    fs::rename(&gitdir, &backup)
        .map_err(|e| WorktreeError::IoWrite(format!("cannot back up the legacy symlink: {e}")))?;
    fsync_parent_best_effort(&backup);
    set_stage("OldLinkBackedUp").await?;

    // Step 5b: install the prepared gitdir.
    fs::rename(&prepared, &gitdir).map_err(|e| {
        WorktreeError::IoWrite(format!(
            "cannot install the new gitdir (legacy backup kept at '{}'): {e}",
            backup.display()
        ))
    })?;
    fsync_parent_best_effort(&gitdir);
    validate_installed_gitdir(&gitdir, journal_id, &worktree_id, &canonical_storage).map_err(
        |error| {
            WorktreeError::OperationBlocked(format!(
                "installed gitdir failed identity validation ({error}); materials kept — \
                  investigate, then rerun `worktree repair --confirm`"
            ))
        },
    )?;
    set_stage("NewGitdirInstalled").await?;

    // Step 3: seed the scoped HEAD (detached at the shared snapshot) and
    // build the private index from that commit — WITHOUT touching files.
    seed_migrated_worktree(&target, head_commit).await?;
    set_stage("HeadIndexSeeded").await?;

    // Registry: the entry's layout is now derivable as linked-v2; persist
    // the (possibly id-backfilled) state.
    write_state(state)?;
    set_stage("RegistryCommitted").await?;

    // Step 6: verify from inside the worktree before any cleanup.
    verify_migrated_worktree(&target, head_commit).await?;
    set_stage("Verified").await?;

    // Cleanup: drop the backup symlink and the marker, resolve the journal.
    match fs::symlink_metadata(&backup) {
        Ok(meta)
            if meta.file_type().is_symlink()
                && fs::canonicalize(&backup).ok().as_deref()
                    == Some(canonical_storage.as_path()) =>
        {
            fs::remove_file(&backup).map_err(|error| {
                WorktreeError::IoWrite(format!(
                    "migrated, but the legacy backup '{}' could not be removed: {error}; \
                      remove it and rerun `worktree repair --confirm`",
                    backup.display()
                ))
            })?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        _ => {
            return Err(WorktreeError::OperationBlocked(format!(
                "'{}' is no longer the expected legacy symlink; not deleting it — \
                  investigate, then rerun `worktree repair --confirm`",
                backup.display()
            )));
        }
    }
    // Marker LAST: recovery adopts an install ONLY via this journal-stamped
    // marker, so it must survive until the journal resolve below has landed
    // (resolve-before-marker is what makes marker-only adoption safe).
    journal_resolve(db, journal_id)
        .await
        .map_err(WorktreeError::OperationBlocked)?;
    let _ = fs::remove_file(gitdir.join("migrate-marker"));
    Ok(())
}

pub(crate) async fn seed_migrated_worktree(
    target: &Path,
    head_commit: git_internal::hash::ObjectHash,
) -> WorktreeResult<()> {
    let _guard = DirGuard::change_to(target)
        .map_err(|e| WorktreeError::IoRead(format!("cannot enter '{}': {e}", target.display())))?;
    // §C.4.2: the private index this builds belongs to `target`, not to the
    // worktree the invocation pinned — re-pin so `path::index()` writes it
    // there instead of rebuilding the invoker's index from `target`'s HEAD.
    let _scope =
        crate::internal::worktree_scope::WorktreeScope::override_scope(target.to_path_buf());
    Head::update_result(Head::Detached(head_commit), None)
        .await
        .map_err(|e| {
            WorktreeError::IoWrite(format!("cannot seed HEAD for '{}': {e}", target.display()))
        })?;
    restore::execute_checked(RestoreArgs {
        overlay: false,
        no_overlay: false,
        ours: false,
        theirs: false,
        ignore_unmerged: false,
        merge: false,
        conflict: None,
        pathspec: vec![util::working_dir_string()],
        source: Some("HEAD".to_string()),
        worktree: false,
        staged: true,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        no_progress: false,
    })
    .await
    .map_err(|e| {
        WorktreeError::IoWrite(format!(
            "cannot build the private index for '{}': {e}",
            target.display()
        ))
    })
}

pub(crate) async fn verify_migrated_worktree(
    target: &Path,
    head_commit: git_internal::hash::ObjectHash,
) -> WorktreeResult<()> {
    let _guard = DirGuard::change_to(target)
        .map_err(|e| WorktreeError::IoRead(format!("cannot enter '{}': {e}", target.display())))?;
    let seen = Head::current_commit_result()
        .await
        .map_err(|e| WorktreeError::IoRead(format!("verification failed reading HEAD: {e}")))?;
    if seen != Some(head_commit) {
        return Err(WorktreeError::OperationBlocked(format!(
            "verification failed: '{}' resolves HEAD to {:?}, expected {head_commit}; \
              materials kept — rerun `worktree repair --confirm`",
            target.display(),
            seen
        )));
    }
    if !target.join(util::ROOT_DIR).join("index").exists() {
        return Err(WorktreeError::OperationBlocked(format!(
            "verification failed: '{}' has no private index; materials kept — rerun \
              `worktree repair --confirm`",
            target.display()
        )));
    }
    crate::command::load_object::<git_internal::internal::object::commit::Commit>(&head_commit)
        .map_err(|e| {
            WorktreeError::IoRead(format!(
                "verification failed reading the HEAD commit through the new commondir: {e}"
            ))
        })?;
    Ok(())
}

pub(crate) fn render_migrate_layout(
    result: &MigrateLayoutOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.repair", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    if result.dry_run {
        if result.planned.is_empty() {
            println!("No legacy-symlink worktrees to migrate.");
        }
        for path in &result.planned {
            println!("would migrate {path}");
        }
    } else {
        for path in &result.migrated {
            println!("migrated {path}");
        }
        if result.migrated.is_empty() {
            println!("No legacy-symlink worktrees to migrate.");
        }
    }
    for entry in &result.skipped {
        println!("skipped {entry}");
    }
    Ok(())
}

pub(crate) fn validate_installed_gitdir(
    gitdir: &Path,
    journal_id: i64,
    worktree_id: &str,
    canonical_storage: &Path,
) -> Result<(), String> {
    if !no_follow_real_dir(gitdir) {
        return Err(format!("'{}' is not a real directory", gitdir.display()));
    }
    let marker_ok = fs::read_to_string(gitdir.join("migrate-marker"))
        .is_ok_and(|raw| raw.trim() == format!("journal {journal_id}"));
    if !marker_ok {
        return Err(format!(
            "'{}' does not carry this migration's marker",
            gitdir.display()
        ));
    }
    let commondir_ok = fs::read_to_string(gitdir.join("commondir"))
        .ok()
        .and_then(|raw| raw.lines().next().map(str::trim).map(PathBuf::from))
        .map(|p| {
            let abs = if p.is_absolute() { p } else { gitdir.join(p) };
            fs::canonicalize(&abs).unwrap_or(abs)
        })
        .is_some_and(|p| p == canonical_storage);
    if !commondir_ok {
        return Err(format!(
            "'{}' commondir does not resolve to this repository's storage",
            gitdir.display()
        ));
    }
    let id_ok =
        fs::read_to_string(gitdir.join("worktree_id")).is_ok_and(|raw| raw.trim() == worktree_id);
    if !id_ok {
        return Err(format!(
            "'{}' worktree_id does not match the registry",
            gitdir.display()
        ));
    }
    Ok(())
}

pub(crate) fn no_follow_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

pub(crate) fn write_detached_marker(target: &Path, worktree_id: &str) -> WorktreeResult<()> {
    let marker = target.join(util::ROOT_DIR).join(DETACHED_MARKER);
    crate::utils::atomic_write::write_atomic(
        &marker,
        format!(
            "{worktree_id}\nremoved from the worktree registry; re-add or delete this \
              directory\n"
        )
        .as_bytes(),
        true,
    )
    .map_err(|source| {
        WorktreeError::IoWrite(format!(
            "cannot write the detached marker '{}': {source}",
            marker.display()
        ))
    })
}

#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn fsync_parent_strict(target: &Path) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    fs::File::open(parent)?.sync_all()
}

pub(crate) fn gitdir_identity_at(path: &str) -> Option<String> {
    fs::read_to_string(Path::new(path).join(util::ROOT_DIR).join("worktree_id"))
        .ok()
        .map(|raw| raw.trim().to_string())
}

pub(crate) fn prepared_dir_is_settled_leftover(prepared: &Path) -> bool {
    let Ok(entries) = fs::read_dir(prepared) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else { return false };
        let known = matches!(
            entry.file_name().to_str(),
            Some("migrate-marker" | "commondir" | "worktree_id")
        );
        if !known {
            return false;
        }
    }
    true
}

pub(crate) fn fsync_parent_best_effort(target: &Path) {
    if let Some(parent) = target.parent()
        && let Ok(dir) = fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
}

pub(crate) fn refuse_active_sidecar_state(gitdir: &Path, action: &str) -> WorktreeResult<()> {
    for name in [
        "merge-state.json",
        "revert-state.json",
        "merge-autostash.json",
        // An interrupted `stash branch`'s rollback record: removing the
        // gitdir would strand the half-created branch AND delete the only
        // instruction for undoing it.
        "stash-branch-journal.json",
    ] {
        let path = gitdir.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(WorktreeError::OperationBlocked(format!(
                    "'{}' holds in-progress state ({name}); conclude the merge/revert \
                      (or run any `libra stash` command there to finish a journaled \
                      rollback) before {action}",
                    gitdir.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(WorktreeError::IoRead(format!(
                    "cannot inspect '{}' before {action}: {error}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

pub(crate) async fn worktree_is_dirty(target: &Path) -> WorktreeResult<bool> {
    let _guard = DirGuard::change_to(target).map_err(|e| {
        WorktreeError::IoRead(format!("cannot enter worktree '{}': {e}", target.display()))
    })?;
    // §C.4.2: this gate decides whether a DESTRUCTIVE delete may proceed, and
    // it must judge `target`'s own index. Inheriting the invoker's pin would
    // compare `target`'s files against ANOTHER worktree's staged state — a
    // clean invoker would then read a dirty target as clean and delete it.
    let _scope =
        crate::internal::worktree_scope::WorktreeScope::override_scope(target.to_path_buf());
    // W1 §C.4.1.1: applied layer overlays are excluded from status by
    // design, so they alone are not "uncommitted changes". This is a
    // DESTRUCTIVE gate (`remove_dir_all` follows), so it must NOT consult
    // the process-global advisory exclusion snapshot — another scope's
    // refresh could hide REAL uncommitted files behind same-named overlay
    // paths. The target scope's overlay set is read straight from the DB
    // (fail-closed on error) and subtracted explicitly from the UNSTAGED
    // side only; anything staged always refuses.
    let overlay: std::collections::HashSet<String> =
        crate::internal::layer::LayerStore::materialized_paths(
            &crate::internal::worktree_scope::WorktreeScope::for_workdir(target),
        )
        .await
        .map_err(|e| {
            WorktreeError::IoRead(format!(
                "cannot verify layer-owned paths before the dirty check: {e}"
            ))
        })?
        .into_iter()
        .map(|p| p.path)
        .collect();
    let staged = crate::command::status::changes_to_be_committed_safe()
        .await
        .map_err(|e| WorktreeError::IoRead(format!("failed to inspect worktree status: {e}")))?;
    let unstaged = crate::command::status::changes_to_be_staged()
        .map_err(|e| WorktreeError::IoRead(format!("failed to inspect worktree status: {e}")))?;
    let is_real_change = |path: &std::path::PathBuf| {
        crate::internal::layer::normalize_key(path).is_none_or(|key| !overlay.contains(&key))
    };
    let unstaged_dirty = unstaged
        .new
        .iter()
        .chain(unstaged.modified.iter())
        .chain(unstaged.deleted.iter())
        .any(is_real_change)
        || !unstaged.renamed.is_empty();
    Ok(!staged.is_empty() || unstaged_dirty)
}

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeRepairIdentityOutput {
    path: String,
    worktree_id: String,
    worktree_id_restored: bool,
    commondir_restored: bool,
}

pub(crate) async fn repair_worktree_identity(
    path: String,
) -> WorktreeResult<WorktreeRepairIdentityOutput> {
    let _registry_lock = acquire_registry_lock_async().await?;
    // A legacy v1 registry carries NO persisted identities — refuse before
    // the locked loader would durably upgrade it (backfilling ids from the
    // possibly-damaged gitdirs this command is meant to repair). The no-arg
    // repair is the explicit, documented upgrade step.
    if let Ok(raw) = fs::read(state_path())
        && !raw.is_empty()
        && matches!(
            WorktreeState::parse_document(&raw),
            Ok((_, RegistryShape::V1))
        )
    {
        return Err(WorktreeError::OperationBlocked(
            "the worktree registry still uses the legacy v1 format with no persisted \
              identities; run `libra worktree repair --confirm` (no argument) once to upgrade \
              it, then retry"
                .to_string(),
        ));
    }
    let state = load_state()?;
    let target = resolve_path(&path, "worktree path")?;
    let entry =
        find_entry(&state, &target).ok_or(WorktreeError::NoSuchWorktree { path: path.clone() })?;
    if entry.is_main {
        return Err(WorktreeError::MainWorktree {
            action: WorktreeControl::Repair.declare(),
            path: target.to_string_lossy().to_string(),
        });
    }
    // A TOMBSTONE's directory was durably deleted; whatever exists at the
    // path now is a stranger, and stamping identity files into it would
    // adopt on the identity surface what `retry_tombstones` explicitly
    // refuses to adopt — while the entry stays in cleanup limbo.
    if entry.state == WorktreeEntryState::Tombstone {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' is a tombstone (its directory was already deleted; only scoped-row \
              cleanup is pending) — run the no-arg `libra worktree repair --confirm` to \
              retry the cleanup instead of re-stamping identity into a recreated directory",
            target.display()
        )));
    }
    let Some(stable_id) = entry.worktree_id.clone() else {
        return Err(WorktreeError::OperationBlocked(format!(
            "the registry entry for '{}' predates registry v2 and carries no persisted \
              worktree id; run the no-arg `libra worktree repair --confirm` once to upgrade \
              the registry, then retry",
            target.display()
        )));
    };
    let gitdir = target.join(util::ROOT_DIR);
    // No-follow: a LEGACY symlink gitdir resolves to MAIN storage — writing
    // identity files through it would alter main metadata. Refuse with the
    // migration hint; only a REAL directory is repairable.
    if detect_entry_layout(&target, false) == "legacy-symlink" {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' uses the legacy shared-.libra symlink layout; run `libra worktree \
              repair --migrate-layout --confirm {}` first",
            target.display(),
            target.display()
        )));
    }
    if !no_follow_real_dir(&gitdir) {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' has no real .libra gitdir to repair; re-add the worktree instead",
            target.display()
        )));
    }

    // Classify the commondir pointer FIRST — a foreign-storage refusal must
    // happen before ANY write, or a failed repair would still have mutated
    // the target worktree's identity.
    let commondir_path = gitdir.join("commondir");
    let common = util::storage_path();
    // A commondir pointer needs restoring when it is MISSING or CORRUPT
    // (unreadable / empty first line — the same states the storage resolver
    // fails closed on). A VALID pointer at a DIFFERENT storage is refused:
    // that worktree belongs to another repository and silently re-homing it
    // would alias two repos' state. Relative pointers resolve against the
    // local gitdir, exactly like the storage resolver.
    let current_common = match fs::read_to_string(&commondir_path) {
        Ok(contents) => contents
            .lines()
            .next()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(PathBuf::from),
        Err(_) => None,
    };
    let commondir_restored = match current_common {
        Some(existing) => {
            let existing_abs = if existing.is_absolute() {
                existing.clone()
            } else {
                gitdir.join(&existing)
            };
            match fs::canonicalize(&existing_abs) {
                Ok(existing_resolved) => {
                    let common_resolved =
                        fs::canonicalize(&common).unwrap_or_else(|_| common.clone());
                    if existing_resolved != common_resolved {
                        // An EXISTING different storage: refusing is the
                        // never-re-home rule.
                        return Err(WorktreeError::OperationBlocked(format!(
                            "'{}' already points at a different common storage ('{}'); \
                              refusing to re-home the worktree — remove and re-add it if \
                              this is intended",
                            commondir_path.display(),
                            existing_resolved.display()
                        )));
                    }
                    false
                }
                // A DANGLING pointer proves nothing about a different
                // storage — the target does not exist (the classic moved-
                // repository corruption the resolver's errors send users
                // here to fix). It is CORRUPT, and corrupt pointers are
                // exactly what this command restores from the registry.
                Err(error) if error.kind() == io::ErrorKind::NotFound => true,
                Err(error) => {
                    return Err(WorktreeError::IoRead(format!(
                        "cannot inspect the commondir target '{}' recorded in '{}': {error}",
                        existing_abs.display(),
                        commondir_path.display()
                    )));
                }
            }
        }
        None => true,
    };

    let id_path = gitdir.join("worktree_id");
    let current_id = fs::read_to_string(&id_path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    // §C.9: declare before the first possible write. `repair` writes identity
    // FILES rather than a journal row (there is no crash window a journal
    // could roll forward — each write is a single atomic rename), so the
    // declaration is asserted here rather than at an append site.
    let _declared = WorktreeControl::Repair.declare();
    let worktree_id_restored = current_id.as_deref() != Some(stable_id.as_str());
    if worktree_id_restored {
        crate::utils::atomic_write::write_atomic(
            &id_path,
            format!("{stable_id}\n").as_bytes(),
            true,
        )
        .map_err(|source| {
            WorktreeError::IoWrite(format!(
                "failed to restore '{}': {source}",
                id_path.display()
            ))
        })?;
    }

    if commondir_restored {
        crate::utils::atomic_write::write_atomic(
            &commondir_path,
            format!("{}\n", common.display()).as_bytes(),
            true,
        )
        .map_err(|source| {
            WorktreeError::IoWrite(format!(
                "failed to restore '{}': {source}",
                commondir_path.display()
            ))
        })?;
    }

    Ok(WorktreeRepairIdentityOutput {
        path: target.to_string_lossy().to_string(),
        worktree_id: stable_id,
        worktree_id_restored,
        commondir_restored,
    })
}

pub(crate) fn render_repair_identity(
    result: &WorktreeRepairIdentityOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.repair", result, output);
    }
    if !output.quiet {
        println!(
            "repaired '{}': worktree_id {}{}{}",
            result.path,
            result.worktree_id,
            if result.worktree_id_restored {
                " (restored)"
            } else {
                " (already correct)"
            },
            if result.commondir_restored {
                "; commondir restored"
            } else {
                ""
            }
        );
    }
    Ok(())
}

pub(crate) async fn repair_worktrees() -> WorktreeResult<WorktreeRepairOutput> {
    let _registry_lock = acquire_registry_lock_async().await?;
    // The healing loader may itself rewrite the file (v1 upgrade, identity
    // invariants); report that as a change too.
    let bytes_before = fs::read(state_path()).ok();
    let mut state = load_state_for_repair()?;
    let mut changed = fs::read(state_path()).ok() != bytes_before;
    let mut notes = Vec::new();

    let mut seen = HashSet::<PathBuf>::new();
    state.entries.retain(|w| {
        let p = PathBuf::from(&w.path);
        if !seen.insert(p) {
            changed = true;
            false
        } else {
            true
        }
    });

    if ensure_main_entry(&mut state).map_err(|source| WorktreeError::StateRepair { source })? {
        changed = true;
    }
    if normalize_v2_ids(&mut state) {
        changed = true;
    }

    // §C.7 W3-s1b recovery, in dependency order: stale intents first (they
    // may settle an interrupted detach/move/re-attach), then tombstone
    // retries, then marker + lifecycle-mirror reconciliation.
    let db = crate::internal::db::get_db_conn_instance().await;
    let journal_recovered =
        recover_pending_intents(&db, &mut state, &mut changed, &mut notes).await?;
    let (tombstones_cleaned, tombstones_pending) =
        retry_tombstones(&db, &mut state, &mut changed, &mut notes).await;
    reconcile_lifecycle(&db, &mut state, &mut notes).await;

    if changed {
        let _ = normalize_v2_ids(&mut state);
        write_state(&state)?;
    }

    Ok(WorktreeRepairOutput {
        changed,
        journal_recovered,
        tombstones_cleaned,
        tombstones_pending,
        notes,
    })
}

pub(crate) async fn recover_pending_intents(
    db: &sea_orm::DatabaseConnection,
    state: &mut WorktreeState,
    changed: &mut bool,
    notes: &mut Vec<String>,
) -> WorktreeResult<usize> {
    let pending = journal_pending(db)
        .await
        .map_err(WorktreeError::OperationBlocked)?;
    let mut recovered = 0usize;
    for intent in pending {
        let PendingIntent {
            id,
            op,
            worktree_id,
            payload,
        } = intent;
        let mut resolve_row = true;
        match op.as_str() {
            "remove" => {
                let path = payload["path"].as_str().unwrap_or_default().to_string();
                let delete_dir = payload["delete_dir"].as_bool().unwrap_or(false);
                let entry_index = state
                    .entries
                    .iter()
                    .position(|w| w.path == path && !w.is_main);
                if delete_dir {
                    let presence = probe_path(Path::new(&path));
                    if let PathPresence::Unknown(error) = &presence {
                        resolve_row = false;
                        notes.push(format!(
                            "cannot determine whether '{path}' still exists ({error}); \
                              journal kept for the next repair"
                        ));
                    }
                    if matches!(presence, PathPresence::Missing) {
                        // Deletion happened; finish the cleanup.
                        if let Some(idx) = entry_index {
                            let entry_id = state.entries[idx].worktree_id.clone();
                            if let Some(id_str) = entry_id.as_deref().or(worktree_id.as_deref()) {
                                match gc_worktree_scoped_rows_strict(db, id_str, true).await {
                                    Ok(()) => {
                                        let _ = lifecycle_delete(db, id_str).await;
                                        state.entries.remove(idx);
                                        *changed = true;
                                        notes.push(format!(
                                            "completed interrupted remove of '{path}'"
                                        ));
                                    }
                                    Err(error) => {
                                        state.entries[idx].state = WorktreeEntryState::Tombstone;
                                        if let Err(mirror_error) = lifecycle_upsert(
                                            db,
                                            id_str,
                                            WorktreeEntryState::Tombstone.as_str(),
                                            &path,
                                        )
                                        .await
                                        {
                                            resolve_row = false;
                                            notes.push(format!(
                                                "tombstone mirror write for '{path}' failed \
                                                  ({mirror_error}); journal kept for the \
                                                  next repair"
                                            ));
                                        }
                                        *changed = true;
                                        notes.push(format!(
                                            "remove of '{path}' left a tombstone (cleanup \
                                              failed again: {error})"
                                        ));
                                    }
                                }
                            }
                        } else if let Some(id_str) = worktree_id.as_deref() {
                            match gc_worktree_scoped_rows_strict(db, id_str, true).await {
                                Ok(()) => {
                                    let _ = lifecycle_delete(db, id_str).await;
                                }
                                Err(error) => {
                                    resolve_row = false;
                                    notes.push(format!(
                                        "scoped cleanup for the removed '{path}' failed \
                                          ({error}); journal kept for the next repair"
                                    ));
                                }
                            }
                        }
                    } else if matches!(presence, PathPresence::Present) {
                        // Deletion never completed. NON-DESTRUCTIVE roll
                        // back: restore the detached marker if this entry
                        // was detached (the dirty-check window lifts it).
                        if let Some(idx) = entry_index
                            && state.entries[idx].state == WorktreeEntryState::DetachedFromRegistry
                            && let Some(id_str) = state.entries[idx].worktree_id.clone().as_deref()
                            && let Err(error) = write_detached_marker(Path::new(&path), id_str)
                        {
                            resolve_row = false;
                            notes.push(format!(
                                "could not re-freeze detached '{path}' ({error}); journal \
                                  kept for the next repair"
                            ));
                        }
                        notes.push(format!(
                            "interrupted `remove --delete-dir` of '{path}' rolled back \
                              (directory still present; nothing was deleted by repair)"
                        ));
                    }
                } else {
                    // Keep-dir detach: roll FORWARD to the detached state —
                    // unless the still-unfrozen worktree started NEW work
                    // between the crash and this repair. remove itself
                    // refuses on active sequencer/sidecar state; recovery
                    // honoring less would strand that work behind the
                    // fail-closed marker. The user reruns remove when done.
                    if let Some(idx) = entry_index {
                        let entry_id = state.entries[idx].worktree_id.clone();
                        let active_now = match entry_id.as_deref().or(worktree_id.as_deref()) {
                            Some(id_str) => scoped_state_active(db, id_str).await,
                            None => false,
                        };
                        if active_now {
                            resolve_row = false;
                            notes.push(format!(
                                "interrupted detach of '{path}' NOT rolled forward: the \
                                  worktree has in-progress sequencer/bisect state that \
                                  started after the crash; finish or abort it, rerun \
                                  `libra worktree remove {path}`, then repair"
                            ));
                        } else if let Some(id_str) = entry_id.as_deref().or(worktree_id.as_deref())
                        {
                            if let Err(error) = lifecycle_upsert(
                                db,
                                id_str,
                                WorktreeEntryState::DetachedFromRegistry.as_str(),
                                &path,
                            )
                            .await
                            {
                                resolve_row = false;
                                notes.push(format!(
                                    "lifecycle mirror write for '{path}' failed ({error}); \
                                      journal kept for the next repair"
                                ));
                            }
                            if let Err(error) = write_detached_marker(Path::new(&path), id_str) {
                                resolve_row = false;
                                notes.push(format!(
                                    "could not freeze detached '{path}' ({error}); journal \
                                      kept for the next repair"
                                ));
                            }
                        }
                        if !active_now {
                            if state.entries[idx].state != WorktreeEntryState::DetachedFromRegistry
                            {
                                state.entries[idx].state = WorktreeEntryState::DetachedFromRegistry;
                                *changed = true;
                            }
                            notes.push(format!("completed interrupted detach of '{path}'"));
                        }
                    }
                }
            }
            "add" => {
                let path = payload["path"].as_str().unwrap_or_default().to_string();
                if payload["reattach"].as_bool().unwrap_or(false) {
                    // Finish a crashed re-attach ONLY for an entry already
                    // PUBLISHED as Active (the crash sat between the
                    // registry write and the marker removal). A still-
                    // detached entry is NOT rolled forward: linked ids are
                    // deterministic (path-derived), so this journal could
                    // predate a delete/re-add/re-detach cycle at the same
                    // path — unfreezing would betray that LATER detach. It
                    // stays frozen (rerun `worktree add` to re-attach) and
                    // the row resolves as rolled back.
                    if let Some(idx) = state.entries.iter().position(|w| w.path == path) {
                        match state.entries[idx].state {
                            WorktreeEntryState::Active
                                if gitdir_identity_at(&path)
                                    != state.entries[idx].worktree_id.clone() =>
                            {
                                // The directory no longer carries THIS
                                // entry's identity — it was replaced after
                                // the crash. Lifting the marker would
                                // unfreeze a stranger; reconcile_lifecycle
                                // applies the same rule.
                                resolve_row = false;
                                notes.push(format!(
                                    "re-attach of '{path}' not completed: the directory's \
                                      identity no longer matches the registry entry; \
                                      journal kept — investigate, then rerun repair"
                                ));
                            }
                            WorktreeEntryState::Active => {
                                let marker =
                                    Path::new(&path).join(util::ROOT_DIR).join(DETACHED_MARKER);
                                match fs::remove_file(&marker) {
                                    Ok(()) => {}
                                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                                    Err(error) => {
                                        resolve_row = false;
                                        notes.push(format!(
                                            "could not lift the marker while completing \
                                              the re-attach of '{path}' ({error}); journal \
                                              kept for the next repair"
                                        ));
                                    }
                                }
                                if resolve_row {
                                    if let Some(id_str) =
                                        state.entries[idx].worktree_id.clone().as_deref()
                                    {
                                        let _ = lifecycle_delete(db, id_str).await;
                                    }
                                    notes.push(format!(
                                        "completed interrupted re-attach of '{path}'"
                                    ));
                                }
                            }
                            WorktreeEntryState::DetachedFromRegistry => {
                                notes.push(format!(
                                    "stale re-attach intent for '{path}' rolled back — \
                                      the entry is (still or again) detached and stays \
                                      frozen; rerun `libra worktree add {path}` to \
                                      re-attach it"
                                ));
                            }
                            WorktreeEntryState::Tombstone => {
                                notes.push(format!(
                                    "stale re-attach intent for '{path}' resolved — the \
                                      entry is now a tombstone"
                                ));
                            }
                        }
                    }
                    if *changed {
                        write_state(state)?;
                    }
                    if resolve_row {
                        journal_resolve(db, id)
                            .await
                            .map_err(WorktreeError::OperationBlocked)?;
                        recovered += 1;
                    }
                    continue;
                }
                let registered = state.entries.iter().any(|w| w.path == path);
                if !registered
                    && let Some(spec) = payload.get("create_branch")
                    && let Some(name) = spec["name"].as_str()
                {
                    // The `-b` branch may have been created before the
                    // crash: roll it back tip-conditionally under the
                    // attach lock — a moved tip means someone committed on
                    // it, which is not ours to delete (journal kept).
                    match spec["start"]
                        .as_str()
                        .and_then(|raw| crate::internal::object_format::parse_repo_oid(raw).ok())
                    {
                        Some(start) => {
                            // FAIL CLOSED on lock failure: without the
                            // attach lock a concurrent scope could be
                            // attaching this branch right now. And never
                            // delete a branch ANY scope has attached, even
                            // at the original tip.
                            match util::acquire_branch_attach_lock() {
                                Err(error) => {
                                    resolve_row = false;
                                    notes.push(format!(
                                        "cannot acquire the branch-attach lock to roll \
                                          back branch '{name}' ({error}); journal kept for \
                                          the next repair"
                                    ));
                                }
                                Ok(_attach_lock) => {
                                    // Result-returning probe over EVERY
                                    // scope: a read failure fails CLOSED
                                    // (no delete, journal kept) — never
                                    // "could not check, so not attached".
                                    match Head::branch_checked_out_anywhere_result(name).await {
                                        Err(error) => {
                                            resolve_row = false;
                                            notes.push(format!(
                                                "cannot verify whether branch '{name}' is \
                                                  attached ({error}); not deleting it — \
                                                  journal kept for the next repair"
                                            ));
                                        }
                                        Ok(Some(scope)) => {
                                            resolve_row = false;
                                            notes.push(format!(
                                                "branch '{name}' from an interrupted \
                                                  `worktree add -b` is checked out at \
                                                  worktree '{scope}'; not deleting it — \
                                                  journal kept, resolve manually"
                                            ));
                                        }
                                        Ok(None) => {
                                            match Branch::delete_branch_if_tip_result(name, &start)
                                             .await
                                         {
                                 Ok(crate::internal::branch::ConditionalDeleteOutcome::Deleted) => {
                                     notes.push(format!(
                                         "rolled back branch '{name}' from an interrupted \
                                          `worktree add -b`"
                                     ));
                                 }
                                 Ok(crate::internal::branch::ConditionalDeleteOutcome::NotFound) => {
                                 }
                                 Ok(crate::internal::branch::ConditionalDeleteOutcome::TipMoved) => {
                                     resolve_row = false;
                                     notes.push(format!(
                                         "branch '{name}' from an interrupted `worktree add \
                                          -b` has NEW commits; not deleting it — journal \
                                          kept, resolve manually"
                                     ));
                                 }
                                 Err(error) => {
                                     resolve_row = false;
                                     notes.push(format!(
                                         "could not roll back branch '{name}' ({error}); \
                                          journal kept for the next repair"
                                     ));
                                 }
                                         }
                                        }
                                    }
                                }
                            }
                        }
                        None => {
                            resolve_row = false;
                            notes.push(format!(
                                "interrupted `worktree add -b {name}' journal has an \
                                  unparsable start tip; journal kept — resolve manually"
                            ));
                        }
                    }
                }
                if !registered && let Some(id_str) = worktree_id.as_deref() {
                    // The add never published; sweep the scope it may have
                    // seeded. The half-created directory (if any) is left
                    // for the user — recovery never deletes directories.
                    match gc_worktree_scoped_rows_strict(db, id_str, true).await {
                        Ok(()) => {
                            let _ = lifecycle_delete(db, id_str).await;
                            notes.push(format!(
                                "rolled back interrupted add of '{path}' (scoped rows \
                                  swept; any partial directory was left in place)"
                            ));
                        }
                        Err(error) => {
                            resolve_row = false;
                            notes.push(format!(
                                "sweep for the unpublished add of '{path}' failed \
                                  ({error}); journal kept for the next repair"
                            ));
                        }
                    }
                }
            }
            "move" => {
                let src = payload["src"].as_str().unwrap_or_default().to_string();
                let dest = payload["dest"].as_str().unwrap_or_default().to_string();
                let src_presence = probe_path(Path::new(&src));
                let dest_presence = probe_path(Path::new(&dest));
                if let PathPresence::Unknown(error) = &src_presence {
                    resolve_row = false;
                    notes.push(format!(
                        "cannot determine whether '{src}' still exists ({error}); journal \
                          kept for the next repair"
                    ));
                }
                if let PathPresence::Unknown(error) = &dest_presence {
                    resolve_row = false;
                    notes.push(format!(
                        "cannot determine whether '{dest}' exists ({error}); journal kept \
                          for the next repair"
                    ));
                }
                let src_exists = matches!(src_presence, PathPresence::Present);
                let dest_missing = matches!(dest_presence, PathPresence::Missing);
                let dest_exists = matches!(dest_presence, PathPresence::Present);
                let src_missing = matches!(src_presence, PathPresence::Missing);
                // Bind the intent to ITS worktree via the journal's persisted
                // id — matching by path could adopt an unrelated entry that
                // later came to occupy the source/destination (e.g. a
                // tombstone), and rename a stranger's directory. Anything
                // that does not line up EXACTLY is ambiguous: keep the row.
                let expected_id = worktree_id.as_deref();
                let entry_of_intent = expected_id.and_then(|journal_id| {
                    state
                        .entries
                        .iter()
                        .position(|w| w.worktree_id.as_deref() == Some(journal_id))
                });
                let dest_taken_by_other = state
                    .entries
                    .iter()
                    .any(|w| w.path == dest && w.worktree_id.as_deref() != expected_id);
                let src_taken_by_other = state
                    .entries
                    .iter()
                    .any(|w| w.path == src && w.worktree_id.as_deref() != expected_id);
                if resolve_row {
                    match entry_of_intent {
                        _ if expected_id.is_none() => {
                            resolve_row = false;
                            notes.push(format!(
                                "interrupted move '{src}' -> '{dest}' carries no worktree \
                                  id; journal kept — investigate manually"
                            ));
                        }
                        _ if dest_taken_by_other || src_taken_by_other => {
                            resolve_row = false;
                            notes.push(format!(
                                "interrupted move '{src}' -> '{dest}': another registry \
                                  entry now occupies one of the paths; journal kept — \
                                  resolve manually, then rerun repair"
                            ));
                        }
                        Some(idx) if state.entries[idx].path == dest => {
                            if src_exists
                                && dest_missing
                                && gitdir_identity_at(&src) != expected_id.map(str::to_string)
                            {
                                // Whatever occupies `src` is NOT this
                                // intent's worktree (no gitdir, or another
                                // identity) — renaming it would relocate a
                                // stranger's material. Keep the journal.
                                resolve_row = false;
                                notes.push(format!(
                                    "interrupted move '{src}' -> '{dest}': the directory \
                                      at the source no longer carries this worktree's \
                                      identity; journal kept — resolve manually, then \
                                      rerun repair"
                                ));
                            } else if src_exists && dest_missing {
                                // Registry updated, rename never happened:
                                // finish it (or roll the registry back).
                                match fs::rename(&src, &dest) {
                                    Ok(()) => {
                                        notes.push(format!(
                                            "completed interrupted move '{src}' -> '{dest}'"
                                        ));
                                    }
                                    Err(error) => {
                                        state.entries[idx].path = src.clone();
                                        *changed = true;
                                        notes.push(format!(
                                            "rolled back interrupted move '{src}' -> \
                                              '{dest}' (rename failed: {error})"
                                        ));
                                    }
                                }
                            } else if src_missing && dest_exists {
                                notes.push(format!(
                                    "interrupted move '{src}' -> '{dest}' was already \
                                      complete"
                                ));
                            } else {
                                resolve_row = false;
                                notes.push(format!(
                                    "interrupted move '{src}' -> '{dest}' is ambiguous \
                                      (src present: {src_exists}, dest present: \
                                      {dest_exists}); journal kept — resolve the \
                                      directories manually, then rerun repair"
                                ));
                            }
                        }
                        Some(idx) if state.entries[idx].path == src => {
                            if src_missing
                                && dest_exists
                                && gitdir_identity_at(&dest) != expected_id.map(str::to_string)
                            {
                                resolve_row = false;
                                notes.push(format!(
                                    "interrupted move '{src}' -> '{dest}': the directory \
                                      at the destination does not carry this worktree's \
                                      identity; journal kept — resolve manually, then \
                                      rerun repair"
                                ));
                            } else if src_missing && dest_exists {
                                // Directory moved but the registry write was
                                // lost: finish it.
                                state.entries[idx].path = dest.clone();
                                *changed = true;
                                notes.push(format!(
                                    "finished registry update for interrupted move \
                                      '{src}' -> '{dest}'"
                                ));
                            } else if src_exists && dest_missing {
                                notes.push(format!(
                                    "interrupted move '{src}' -> '{dest}' never started; \
                                      nothing to do"
                                ));
                            } else {
                                resolve_row = false;
                                notes.push(format!(
                                    "interrupted move '{src}' -> '{dest}' is ambiguous \
                                      (src present: {src_exists}, dest present: \
                                      {dest_exists}); journal kept — resolve the \
                                      directories manually, then rerun repair"
                                ));
                            }
                        }
                        Some(idx) => {
                            resolve_row = false;
                            let elsewhere = state.entries[idx].path.clone();
                            notes.push(format!(
                                "interrupted move '{src}' -> '{dest}': its worktree is \
                                  now registered at '{elsewhere}'; journal kept — \
                                  investigate manually"
                            ));
                        }
                        None => {
                            resolve_row = false;
                            notes.push(format!(
                                "interrupted move '{src}' -> '{dest}': no registry entry \
                                  carries its worktree id; journal kept — investigate \
                                  manually, then rerun repair"
                            ));
                        }
                    }
                }
            }
            "prune" => {
                if let Some(paths) = payload["paths"].as_array() {
                    for value in paths {
                        let path = value.as_str().unwrap_or_default().to_string();
                        if state.entries.iter().any(|w| w.path == path && !w.is_main)
                            && let PathPresence::Unknown(error) = probe_path(Path::new(&path))
                        {
                            resolve_row = false;
                            notes.push(format!(
                                "cannot determine whether '{path}' still exists ({error}); \
                                  journal kept for the next repair"
                            ));
                            continue;
                        }
                        if let Some(idx) = state.entries.iter().position(|w| {
                            w.path == path
                                && !w.is_main
                                && matches!(probe_path(Path::new(&w.path)), PathPresence::Missing)
                        }) {
                            let entry_id = state.entries[idx].worktree_id.clone();
                            let cleaned = if let Some(id_str) = entry_id.as_deref() {
                                match gc_worktree_scoped_rows_strict(db, id_str, true).await {
                                    Ok(()) => {
                                        let _ = lifecycle_delete(db, id_str).await;
                                        true
                                    }
                                    Err(error) => {
                                        state.entries[idx].state = WorktreeEntryState::Tombstone;
                                        if let Err(mirror_error) = lifecycle_upsert(
                                            db,
                                            id_str,
                                            WorktreeEntryState::Tombstone.as_str(),
                                            &path,
                                        )
                                        .await
                                        {
                                            resolve_row = false;
                                            notes.push(format!(
                                                "tombstone mirror write for '{path}' failed \
                                                  ({mirror_error}); journal kept for the \
                                                  next repair"
                                            ));
                                        }
                                        *changed = true;
                                        notes.push(format!(
                                            "prune of '{path}' left a tombstone (cleanup \
                                              failed: {error})"
                                        ));
                                        false
                                    }
                                }
                            } else {
                                true
                            };
                            if cleaned {
                                state.entries.remove(idx);
                                *changed = true;
                                notes.push(format!("completed interrupted prune of '{path}'"));
                            }
                        }
                    }
                }
            }
            "migrate" => {
                // §C.6.2 step 5 recovery: decide by IDENTITY (no-follow
                // symlink target, our own journal-stamped marker), never by
                // bare existence. Ambiguity keeps the row.
                let path = PathBuf::from(payload["path"].as_str().unwrap_or_default());
                let head = payload["head"]
                    .as_str()
                    .and_then(|raw| crate::internal::object_format::parse_repo_oid(raw).ok());
                let gitdir = path.join(util::ROOT_DIR);
                let prepared = path.join(format!(".libra.migrate-{id}"));
                let backup = path.join(format!(".libra.legacy-backup-{id}"));
                let storage = util::storage_path();
                let canonical_storage =
                    fs::canonicalize(&storage).unwrap_or_else(|_| storage.clone());
                let is_our_marker = |dir: &Path| {
                    fs::read_to_string(dir.join("migrate-marker"))
                        .is_ok_and(|raw| raw.trim() == format!("journal {id}"))
                };
                let is_legacy_link = |candidate: &Path| {
                    fs::symlink_metadata(candidate)
                        .map(|meta| meta.file_type().is_symlink())
                        .unwrap_or(false)
                        && fs::canonicalize(candidate).ok().as_deref()
                            == Some(canonical_storage.as_path())
                };
                let gitdir_meta = fs::symlink_metadata(&gitdir);
                if is_legacy_link(&gitdir) {
                    // Pre-backup crash: the legacy link is untouched. Roll
                    // BACK — drop only OUR (marker-verified) prepared dir;
                    // any unexpected artifact or a failed removal keeps the
                    // journal (nothing is guessed or force-deleted).
                    match fs::symlink_metadata(&prepared) {
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {
                            if is_legacy_link(&backup)
                                && let Err(remove_error) = fs::remove_file(&backup)
                            {
                                resolve_row = false;
                                notes.push(format!(
                                    "cannot remove the backup link '{}' ({remove_error}); \
                                      journal kept",
                                    backup.display()
                                ));
                            } else {
                                notes.push(format!(
                                    "rolled back interrupted layout migration of '{}' \
                                      (legacy link untouched)",
                                    path.display()
                                ));
                            }
                        }
                        Ok(meta) if meta.is_dir() && is_our_marker(&prepared) => {
                            let mut backup_leaked = false;
                            if is_legacy_link(&backup)
                                && let Err(remove_error) = fs::remove_file(&backup)
                            {
                                backup_leaked = true;
                                resolve_row = false;
                                notes.push(format!(
                                    "cannot remove the backup link '{}' ({remove_error}); \
                                      journal kept",
                                    backup.display()
                                ));
                            }
                            if backup_leaked {
                                // Keep the prepared dir too: with the journal
                                // pending, the next repair retries both.
                            } else if let Err(error) = fs::remove_dir_all(&prepared) {
                                resolve_row = false;
                                notes.push(format!(
                                    "cannot remove the prepared dir '{}' ({error}); \
                                      journal kept",
                                    prepared.display()
                                ));
                            } else {
                                notes.push(format!(
                                    "rolled back interrupted layout migration of '{}' \
                                      (legacy link untouched)",
                                    path.display()
                                ));
                            }
                        }
                        Ok(meta)
                            if meta.is_dir() && prepared_dir_is_settled_leftover(&prepared) =>
                        {
                            // Marker-less but IDENTIFIED: the directory name
                            // embeds this exact journal's id (nothing else
                            // creates that name), and it holds at most the
                            // three files the engine writes — a crash before
                            // the marker landed, or mid-rollback after the
                            // marker was already removed. Both roll back.
                            let mut backup_leaked = false;
                            if is_legacy_link(&backup)
                                && let Err(remove_error) = fs::remove_file(&backup)
                            {
                                backup_leaked = true;
                                resolve_row = false;
                                notes.push(format!(
                                    "cannot remove the backup link '{}' ({remove_error}); \
                                      journal kept",
                                    backup.display()
                                ));
                            }
                            if backup_leaked {
                                // Keep the prepared dir too: with the journal
                                // pending, the next repair retries both.
                            } else if let Err(error) = fs::remove_dir_all(&prepared) {
                                resolve_row = false;
                                notes.push(format!(
                                    "cannot remove the prepared dir '{}' ({error}); \
                                      journal kept",
                                    prepared.display()
                                ));
                            } else {
                                notes.push(format!(
                                    "rolled back interrupted layout migration of '{}' \
                                      (legacy link untouched)",
                                    path.display()
                                ));
                            }
                        }
                        _ => {
                            resolve_row = false;
                            notes.push(format!(
                                "prepared artifact '{}' does not carry this journal's \
                                  marker; journal kept — investigate manually, nothing \
                                  was deleted",
                                prepared.display()
                            ));
                        }
                    }
                } else if matches!(&gitdir_meta, Err(e) if e.kind() == io::ErrorKind::NotFound)
                    && is_legacy_link(&backup)
                    && no_follow_real_dir(&prepared)
                    && is_our_marker(&prepared)
                {
                    // Between 5a and 5b: install ours, then finish.
                    if let Err(error) = fs::rename(&prepared, &gitdir) {
                        resolve_row = false;
                        notes.push(format!(
                            "cannot finish installing the migrated gitdir for '{}' \
                              ({error}); journal kept",
                            path.display()
                        ));
                    } else {
                        fsync_parent_best_effort(&gitdir);
                        if let Err(error) = validate_installed_gitdir(
                            &gitdir,
                            id,
                            worktree_id.as_deref().unwrap_or_default(),
                            &canonical_storage,
                        ) {
                            // `continue` skips the shared tail, so the
                            // journal row stays pending by construction.
                            notes.push(format!(
                                "installed gitdir for '{}' failed identity validation \
                                  ({error}); journal kept — investigate manually",
                                path.display()
                            ));
                            if *changed {
                                write_state(state)?;
                            }
                            continue;
                        }
                        let _gate_bypass = util::bypass_migration_gate();
                        match finish_migration_recovery(&path, &backup, head).await {
                            Ok(()) => {
                                if *changed {
                                    write_state(state)?;
                                }
                                journal_resolve(db, id)
                                    .await
                                    .map_err(WorktreeError::OperationBlocked)?;
                                let _ = fs::remove_file(
                                    path.join(util::ROOT_DIR).join("migrate-marker"),
                                );
                                recovered += 1;
                                notes.push(format!(
                                    "completed interrupted layout migration of '{}'",
                                    path.display()
                                ));
                                continue;
                            }
                            Err(error) => {
                                resolve_row = false;
                                notes.push(format!(
                                    "layout migration of '{}' still incomplete ({error}); \
                                      journal kept",
                                    path.display()
                                ));
                            }
                        }
                    }
                } else if validate_installed_gitdir(
                    &gitdir,
                    id,
                    worktree_id.as_deref().unwrap_or_default(),
                    &canonical_storage,
                )
                .is_ok()
                {
                    // ONLY the journal-stamped marker may adopt an install:
                    // ids are path-derived, so a pruned/re-added worktree
                    // at the same path would satisfy a commondir/id match
                    // and be silently re-seeded from the stale snapshot.
                    // (The resolve-before-marker ordering guarantees a
                    // marker-less install never has a live journal.)
                    // Installed; finish seed/verify/cleanup idempotently.
                    // Journal resolves BEFORE the marker is lifted, so a
                    // crash in between still leaves a recognizable install.
                    let _gate_bypass = util::bypass_migration_gate();
                    match finish_migration_recovery(&path, &backup, head).await {
                        Ok(()) => {
                            if *changed {
                                write_state(state)?;
                            }
                            journal_resolve(db, id)
                                .await
                                .map_err(WorktreeError::OperationBlocked)?;
                            let _ =
                                fs::remove_file(path.join(util::ROOT_DIR).join("migrate-marker"));
                            recovered += 1;
                            notes.push(format!(
                                "completed interrupted layout migration of '{}'",
                                path.display()
                            ));
                            continue;
                        }
                        Err(error) => {
                            resolve_row = false;
                            notes.push(format!(
                                "layout migration of '{}' still incomplete ({error}); \
                                  journal kept",
                                path.display()
                            ));
                        }
                    }
                } else {
                    resolve_row = false;
                    notes.push(format!(
                        "interrupted layout migration of '{}': on-disk state matches no \
                          known stage (identity check failed); journal kept — investigate \
                          manually, nothing was deleted",
                        path.display()
                    ));
                }
            }
            other => {
                // FAIL CLOSED: an op this binary does not know was written
                // by a NEWER binary — resolving (deleting) it would destroy
                // that binary's crash-recovery anchor. Keep it pending.
                resolve_row = false;
                notes.push(format!(
                    "unknown intent op '{other}' (id {id}); journal kept — rerun repair \
                      with the binary that recorded it"
                ));
            }
        }
        // Persist the registry BEFORE resolving the intent: a crash after
        // the resolve with only in-memory registry changes would silently
        // lose the recovery (e.g. a reattach unfrozen with a still-detached
        // persisted entry).
        if *changed {
            write_state(state)?;
        }
        if resolve_row {
            journal_resolve(db, id)
                .await
                .map_err(WorktreeError::OperationBlocked)?;
            recovered += 1;
        }
    }
    Ok(recovered)
}

pub(crate) async fn finish_migration_recovery(
    target: &Path,
    backup: &Path,
    head: Option<git_internal::hash::ObjectHash>,
) -> Result<(), String> {
    let Some(head_commit) = head else {
        return Err("journal carries no parsable HEAD snapshot".to_string());
    };
    seed_migrated_worktree(target, head_commit)
        .await
        .map_err(|e| e.into_cli_error().to_string())?;
    verify_migrated_worktree(target, head_commit)
        .await
        .map_err(|e| e.into_cli_error().to_string())?;
    let storage = util::storage_path();
    let canonical_storage = fs::canonicalize(&storage).unwrap_or(storage);
    match fs::symlink_metadata(backup) {
        Ok(meta) if meta.file_type().is_symlink() => {
            // Only the EXPECTED legacy link (resolving to this repo's
            // storage) may be deleted; a foreign symlink is kept.
            if fs::canonicalize(backup).ok().as_deref() != Some(canonical_storage.as_path()) {
                return Err(format!(
                    "backup '{}' does not resolve to this repository's storage; not \
                      touching it",
                    backup.display()
                ));
            }
            fs::remove_file(backup).map_err(|e| format!("cannot remove the backup: {e}"))?;
        }
        Ok(_) => {
            return Err(format!(
                "backup '{}' is not the expected symlink; not touching it",
                backup.display()
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot inspect the backup: {error}")),
    }
    Ok(())
}

pub(crate) async fn retry_tombstones(
    db: &sea_orm::DatabaseConnection,
    state: &mut WorktreeState,
    changed: &mut bool,
    notes: &mut Vec<String>,
) -> (usize, usize) {
    let mut cleaned = 0usize;
    let mut pending = 0usize;
    let mut index = 0usize;
    while index < state.entries.len() {
        if state.entries[index].state != WorktreeEntryState::Tombstone {
            index += 1;
            continue;
        }
        let path = state.entries[index].path.clone();
        let dir_missing = matches!(
            fs::symlink_metadata(Path::new(&path)),
            Err(ref error) if error.kind() == io::ErrorKind::NotFound
        );
        if !dir_missing {
            pending += 1;
            notes.push(format!(
                "tombstone '{path}': a directory now exists at that path; not adopting it \
                  — remove or rename it, then rerun repair"
            ));
            index += 1;
            continue;
        }
        let Some(id) = state.entries[index].worktree_id.clone() else {
            pending += 1;
            index += 1;
            continue;
        };
        match gc_worktree_scoped_rows_strict(db, &id, true).await {
            Ok(()) => {
                let _ = lifecycle_delete(db, &id).await;
                state.entries.remove(index);
                *changed = true;
                cleaned += 1;
                notes.push(format!("tombstone '{path}': scoped cleanup completed"));
            }
            Err(error) => {
                pending += 1;
                notes.push(format!(
                    "tombstone '{path}': scoped cleanup failed again ({error}); will retry \
                      on the next repair"
                ));
                index += 1;
            }
        }
    }
    (cleaned, pending)
}

pub(crate) async fn reconcile_lifecycle(
    db: &sea_orm::DatabaseConnection,
    state: &mut WorktreeState,
    notes: &mut Vec<String>,
) {
    for entry in &state.entries {
        let Some(id) = entry.worktree_id.as_deref() else {
            continue;
        };
        let marker = Path::new(&entry.path)
            .join(util::ROOT_DIR)
            .join(DETACHED_MARKER);
        match entry.state {
            WorktreeEntryState::DetachedFromRegistry => {
                // Restore the marker only into a directory that still
                // carries THIS entry's identity — writing it into a
                // recreated stranger directory would fabricate a `.libra`
                // (write_atomic creates parents) and freeze the user's
                // unrelated material. The Active arm below applies the same
                // identity rule when LIFTING a marker.
                if !marker.exists()
                    && Path::new(&entry.path).is_dir()
                    && gitdir_identity_at(&entry.path).as_deref() == Some(id)
                {
                    match write_detached_marker(Path::new(&entry.path), id) {
                        Ok(()) => {
                            notes.push(format!("restored the detached marker for '{}'", entry.path))
                        }
                        Err(error) => notes.push(format!(
                            "FAILED to restore the detached marker for '{}' ({error}); the \
                              directory is NOT frozen — rerun repair after fixing the cause",
                            entry.path
                        )),
                    }
                }
                let _ = lifecycle_upsert(
                    db,
                    id,
                    WorktreeEntryState::DetachedFromRegistry.as_str(),
                    &entry.path,
                )
                .await;
            }
            WorktreeEntryState::Tombstone => {
                let _ =
                    lifecycle_upsert(db, id, WorktreeEntryState::Tombstone.as_str(), &entry.path)
                        .await;
            }
            WorktreeEntryState::Active => {
                if marker.exists() {
                    let gitdir_id = fs::read_to_string(
                        Path::new(&entry.path)
                            .join(util::ROOT_DIR)
                            .join("worktree_id"),
                    )
                    .ok()
                    .map(|value| value.trim().to_string());
                    if gitdir_id.as_deref() == Some(id) {
                        if fs::remove_file(&marker).is_ok() {
                            notes.push(format!(
                                "lifted a stale detached marker from active '{}'",
                                entry.path
                            ));
                        }
                    } else {
                        notes.push(format!(
                            "active '{}' carries a detached marker but its gitdir identity \
                              does not match; leaving it frozen — investigate manually",
                            entry.path
                        ));
                    }
                }
                let _ = lifecycle_delete(db, id).await;
            }
        }
    }

    // Stray migrate-markers (§C.6.2 cleanup retry): a marker whose journal
    // already RESOLVED (no pending migrate row for the path) only freezes a
    // finished worktree — clear it once the install identity is verified
    // (real dir + commondir at this storage). Unverifiable states are
    // reported, never guessed.
    if let Ok(pending) = journal_pending(db).await {
        let storage = util::storage_path();
        let canonical_storage = fs::canonicalize(&storage).unwrap_or(storage);
        for entry in &state.entries {
            let gitdir = Path::new(&entry.path).join(util::ROOT_DIR);
            let marker = gitdir.join("migrate-marker");
            if !marker.exists() {
                continue;
            }
            let has_pending = pending.iter().any(|intent| {
                intent.op == "migrate"
                    && (intent.payload["path"].as_str() == Some(entry.path.as_str())
                        || (intent.worktree_id.is_some()
                            && intent.worktree_id.as_deref() == entry.worktree_id.as_deref()))
            });
            if has_pending {
                continue;
            }
            let commondir_ok = fs::read_to_string(gitdir.join("commondir"))
                .ok()
                .and_then(|raw| raw.lines().next().map(str::trim).map(PathBuf::from))
                .map(|p| {
                    let abs = if p.is_absolute() { p } else { gitdir.join(p) };
                    fs::canonicalize(&abs).unwrap_or(abs)
                })
                .is_some_and(|p| p == canonical_storage);
            if no_follow_real_dir(&gitdir) && commondir_ok {
                match fs::remove_file(&marker) {
                    Ok(()) => notes.push(format!(
                        "cleared a resolved migration marker from '{}'",
                        entry.path
                    )),
                    Err(error) => notes.push(format!(
                        "FAILED to clear the resolved migration marker from '{}' \
                          ({error}); the worktree stays frozen — fix the cause and rerun \
                          repair",
                        entry.path
                    )),
                }
            } else {
                notes.push(format!(
                    "'{}' carries a migration marker with no pending journal but its \
                      install identity cannot be verified; leaving it frozen",
                    entry.path
                ));
            }
        }
    }

    // Sweep mirror rows with no matching non-active entry: they would block
    // the down migration with nothing left to finish.
    if let Ok(rows) = lifecycle_rows(db).await {
        for (row_id, row_state) in rows {
            let matching = state.entries.iter().find(|entry| {
                entry.worktree_id.as_deref() == Some(row_id.as_str()) && !entry.state.is_active()
            });
            if matching.is_none() {
                let _ = lifecycle_delete(db, &row_id).await;
                notes.push(format!(
                    "cleared an orphaned lifecycle row ({row_id}: {row_state})"
                ));
            }
        }
    }
}

pub(crate) fn render_repair_worktrees(
    result: &WorktreeRepairOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.repair", result, output);
    }
    if !output.quiet {
        for note in &result.notes {
            println!("{note}");
        }
        if result.tombstones_pending > 0 {
            println!(
                "{} tombstone(s) still pending; rerun `libra worktree repair --confirm` \
                  after addressing the notes above",
                result.tombstones_pending
            );
        }
    }
    Ok(())
}
