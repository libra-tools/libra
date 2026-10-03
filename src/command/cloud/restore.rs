//! Cloud restore: object/metadata/Agent-catalog recovery from D1/R2 backends.
#![allow(unused_imports)]
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    path::PathBuf,
    sync::Arc,
};

use agent_capture::*;
use clap::{Parser, Subcommand};
use git_internal::hash::ObjectHash;
use object_format::{REBACKUP_HINT, resolve_cloud_repository_kind};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Schema, Set,
    TransactionTrait, sea_query::Expr,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
pub(crate) use sync::run_cloud_sync;
use uuid::Uuid;

use super::*;
use crate::{
    command::restore::{self as restore_cmd, RestoreArgs as RestoreWorktreeArgs},
    internal::{
        branch::Branch,
        config::ConfigKv,
        db,
        head::Head,
        model::{object_index, reference},
    },
    utils::{
        d1_client::{
            AgentCaptureGenerationManifest, AgentCaptureGenerationRow,
            AgentCaptureRestoreCatalogRows, AgentCheckpointPruneTombstoneRow, AgentCheckpointV2Row,
            AgentImportTombstoneRow, AgentSessionV2Row, AgentSubagentContentClaimRow,
            AgentSubagentContentRevisionRow, AgentSubagentLinkRow, D1Client, ObjectIndexRow,
        },
        error::{CliError, CliResult, StableErrorCode, emit_warning},
        output::{OutputConfig, ProgressMode, emit_json_data},
        path,
        storage::{Storage, local::LocalStorage, remote::RemoteStorage},
        util,
    },
};

pub(crate) fn render_cloud_restore_output(
    result: &CloudRestoreOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("cloud.restore", result, output);
    }
    Ok(())
}

/// D1 name lookups are a trust boundary: the returned id is later used in
/// output, local config, and as the remote-storage namespace prefix.
/// Accept only Libra's canonical UUID representation before any of those
/// sinks can observe it.
fn validate_remote_repository_id(repo_id: String) -> CloudResult<String> {
    let valid = Uuid::parse_str(&repo_id)
        .ok()
        .is_some_and(|parsed| parsed.hyphenated().to_string() == repo_id);
    if valid {
        Ok(repo_id)
    } else {
        Err(CloudError::NameNotFound(
            "cloud repository lookup returned an invalid repository id; verify the cloud catalog or retry with an explicit repository id"
                .to_string(),
        ))
    }
}

pub(crate) async fn restore_indexed_objects_from_remote(
    indexes: &[ObjectIndexRow],
    r2_storage: &RemoteStorage,
    local_storage: &LocalStorage,
    kind: git_internal::hash::HashKind,
) -> CloudResult<ObjectRestoreReport> {
    let mut report = ObjectRestoreReport::default();

    for idx in indexes {
        let decoded = hex::decode(&idx.o_id).map_err(|_| {
            CloudError::Generic("cloud object index contains an invalid object id".to_string())
        })?;
        let hash = match ObjectHash::from_bytes_for_kind(kind, &decoded) {
            Ok(hash) => hash,
            Err(_) => {
                report
                    .warnings
                    .push("error: cloud object index contains an invalid object id".to_string());
                report.failed += 1;
                continue;
            }
        };

        if let Ok((data, object_type)) = local_storage.get(&hash).await
            && ObjectHash::from_type_and_data_for_kind(kind, object_type, &data)
                .ok()
                .is_some_and(|computed| computed == hash)
        {
            report.skipped += 1;
            continue;
        }

        // lore.md 2.5: never RESTORE an intentionally-obliterated object from
        // the durable tier (拒绝重建). Fail CLOSED (Codex P1): if the tombstone
        // table cannot be read, do NOT restore (an unreadable table must not
        // let an obliterated payload resurrect) — skip with a warning.
        match crate::internal::obliteration::ObliterationStore::lookup(&hash).await {
            Ok(Some(_)) => {
                report.skipped += 1;
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                report.warnings.push(format!(
                    "warning: cannot verify obliteration tombstone; not restoring: {e}"
                ));
                report.skipped += 1;
                continue;
            }
        }

        match r2_storage.get(&hash).await {
            Ok((data, obj_type)) => {
                let computed = match ObjectHash::from_type_and_data_for_kind(kind, obj_type, &data)
                {
                    Ok(computed) => computed,
                    Err(e) => {
                        report
                            .warnings
                            .push(format!("warning: failed to hash restored object: {e}"));
                        report.failed += 1;
                        continue;
                    }
                };
                if computed != hash {
                    report.warnings.push(
                        "warning: restored object hash does not match the cloud object index"
                            .to_string(),
                    );
                    report.failed += 1;
                    continue;
                }

                if let Err(e) = local_storage.put(&hash, &data, obj_type).await {
                    report
                        .warnings
                        .push(format!("error: failed to save restored object: {e}"));
                    report.failed += 1;
                    continue;
                }
                report.downloaded += 1;
            }
            Err(e) => {
                report
                    .warnings
                    .push(format!("error: failed to download a cloud object: {e}"));
                report.failed += 1;
            }
        }
    }

    Ok(report)
}

pub(crate) async fn run_cloud_restore(args: RestoreArgs) -> CloudResult<CloudRestoreOutput> {
    validate_cloud_backup_env(args.metadata_only).await?;

    let d1_client = D1Client::from_env()
        .await
        .map_err(|error| cloud_d1_failure("initialize cloud client", &error))?;

    let repo_id = if let Some(name) = &args.name {
        d1_client
            .ensure_repositories_table()
            .await
            .map_err(|error| cloud_d1_failure("ensure repositories table", &error))?;

        let id = d1_client
            .get_repo_id_by_name(name)
            .await
            .map_err(|error| cloud_d1_failure("resolve repository name", &error))?;
        let id = id.ok_or_else(|| {
            CloudError::NameNotFound(format!("Repository with name '{}' not found", name))
        })?;
        validate_remote_repository_id(id)?
    } else {
        args.repo_id
            .clone()
            .ok_or_else(|| CloudError::NameNotFound("repo_id is required".to_string()))?
    };

    // Converge repositories schema and read the authoritative object-format
    // written by backup (B3-09 restore plan field). Missing/NULL stays None
    // for B3-14 fail-closed consumers.
    d1_client
        .ensure_repositories_table()
        .await
        .map_err(|error| cloud_d1_failure("ensure repositories table", &error))?;
    let object_format = d1_client
        .find_repository(&repo_id)
        .await
        .map_err(|error| cloud_d1_failure("load repository metadata", &error))?
        .and_then(|row| row.object_format);

    let indexes = d1_client
        .get_object_indexes(&repo_id)
        .await
        .map_err(|error| cloud_d1_failure("list object indexes", &error))?;

    // B3-14: refuse width inference; kind comes only from repository metadata
    // (or the legacy all-40 → sha1 window when metadata is absent).
    let repository_kind = resolve_cloud_repository_kind(object_format.as_deref(), &indexes)?;
    // Only emit the parser's canonical enum spelling; the D1 value itself is
    // untrusted input even after validation (for example it may include
    // surrounding whitespace).
    let resolved_object_format =
        crate::internal::object_format::as_str(repository_kind).to_string();

    let db_conn = db::get_db_conn_instance().await;
    if !args.metadata_only {
        preflight_agent_capture_prune_fences(&db_conn, &d1_client, &repo_id).await?;
    }
    for idx in &indexes {
        let existing = object_index::Entity::find()
            .filter(object_index::Column::OId.eq(&idx.o_id))
            .filter(object_index::Column::RepoId.eq(&idx.repo_id))
            .one(&db_conn)
            .await
            .map_err(|_| {
                CloudError::Generic(
                    "failed to query local object-index state while restoring cloud metadata"
                        .to_string(),
                )
            })?;

        if let Some(existing_model) = existing {
            let mut active: object_index::ActiveModel = existing_model.into();
            active.is_synced = Set(1);
            if active.update(&db_conn).await.is_err() {
                emit_warning("failed to update a cloud object index".to_string());
            }
        } else {
            let entry = object_index::ActiveModel {
                o_id: Set(idx.o_id.clone()),
                o_type: Set(idx.o_type.clone()),
                o_size: Set(idx.o_size),
                repo_id: Set(idx.repo_id.clone()),
                created_at: Set(idx.created_at),
                is_synced: Set(1),
                ..Default::default()
            };

            if entry.insert(&db_conn).await.is_err() {
                emit_warning("failed to insert a cloud object index".to_string());
            }
        }
    }

    let _ = ConfigKv::set("libra.repoid", &repo_id, false).await;

    if args.metadata_only {
        return Ok(CloudRestoreOutput {
            repo_id,
            object_format: Some(resolved_object_format),
            metadata_only: true,
            total_objects: indexes.len(),
            indexes_restored: indexes.len(),
            object_restore: None,
            metadata: CloudRestoreMetadataOutput {
                status: "not_run".to_string(),
                warning: None,
            },
            agent_capture: CloudRestoreAgentCaptureOutput {
                status: "not_run".to_string(),
            },
        });
    }

    let r2_storage = create_r2_storage(&repo_id).await?;
    let objects_path = path::objects();
    let local_storage = LocalStorage::new(objects_path);

    let object_report =
        restore_indexed_objects_from_remote(&indexes, &r2_storage, &local_storage, repository_kind)
            .await?;
    for warning in &object_report.warnings {
        eprintln!("{warning}");
    }
    if object_report.failed > 0 {
        return Err(CloudError::PartialTransfer(format!(
            "{} objects failed to restore",
            object_report.failed
        )));
    }

    let (metadata, deferred_capture_refs) = match restore_metadata(&db_conn, &r2_storage).await {
        Ok(deferred_capture_refs) => (
            CloudRestoreMetadataOutput {
                status: "restored".to_string(),
                warning: None,
            },
            deferred_capture_refs,
        ),
        Err(e) => {
            emit_warning(format!("failed to restore metadata: {}", e));
            (
                CloudRestoreMetadataOutput {
                    status: "warning".to_string(),
                    warning: Some(e.to_string()),
                },
                Vec::new(),
            )
        }
    };

    let head_commit = Head::current_commit_result()
        .await
        .map_err(|error| CloudError::Generic(format!("failed to resolve HEAD commit: {error}")))?;
    if head_commit.is_some() {
        let _ = restore_worktree_to_head(false).await;
    } else {
        let main_branch = Branch::find_branch_result("main", None)
            .await
            .map_err(|error| {
                CloudError::Generic(format!("failed to resolve main branch: {error}"))
            })?;
        if main_branch.is_some() {
            Head::update(Head::Branch("main".to_string()), None).await;
            let _ = restore_worktree_to_head(false).await;
        }
    }

    let capture_outcome = restore_agent_capture_from_d1(&db_conn, &d1_client, &repo_id, false)
        .await
        .map_err(agent_capture_restore_failure)?;
    restore_legacy_capture_refs_if_unowned(&db_conn, deferred_capture_refs, capture_outcome)
        .await?;

    Ok(CloudRestoreOutput {
        repo_id,
        object_format: Some(resolved_object_format),
        metadata_only: false,
        total_objects: indexes.len(),
        indexes_restored: indexes.len(),
        object_restore: Some(CloudRestoreObjectOutput {
            downloaded: object_report.downloaded,
            skipped: object_report.skipped,
            failed: object_report.failed,
        }),
        metadata,
        agent_capture: CloudRestoreAgentCaptureOutput {
            status: "restored".to_string(),
        },
    })
}

pub(crate) async fn execute_restore(args: RestoreArgs) -> CloudResult<()> {
    validate_cloud_backup_env(args.metadata_only).await?;

    // Initialize D1 client
    let d1_client = D1Client::from_env()
        .await
        .map_err(|error| cloud_d1_failure("initialize cloud client", &error))?;

    let repo_id = if let Some(name) = &args.name {
        // Ensure repositories table exists before resolving name
        // This handles cases where the D1 database is old/uninitialized and missing the table
        d1_client
            .ensure_repositories_table()
            .await
            .map_err(|error| cloud_d1_failure("ensure repositories table", &error))?;

        let id = d1_client
            .get_repo_id_by_name(name)
            .await
            .map_err(|error| cloud_d1_failure("resolve repository name", &error))?;
        let id = id.ok_or_else(|| {
            CloudError::NameNotFound(format!("Repository with name '{}' not found", name))
        })?;
        validate_remote_repository_id(id)?
    } else {
        args.repo_id
            .clone()
            .ok_or_else(|| CloudError::NameNotFound("repo_id is required".to_string()))?
    };

    println!("Starting restore for repo: {}", repo_id);

    d1_client
        .ensure_repositories_table()
        .await
        .map_err(|error| cloud_d1_failure("ensure repositories table", &error))?;
    let object_format = d1_client
        .find_repository(&repo_id)
        .await
        .map_err(|error| cloud_d1_failure("load repository metadata", &error))?
        .and_then(|row| row.object_format);

    // Get object indexes from D1
    let indexes = d1_client
        .get_object_indexes(&repo_id)
        .await
        .map_err(|error| cloud_d1_failure("list object indexes", &error))?;

    let repository_kind = resolve_cloud_repository_kind(object_format.as_deref(), &indexes)?;

    println!("Found {} objects in cloud for repo.", indexes.len());

    if indexes.is_empty() {
        println!("No objects found for this repo.");
    }

    // Get database connection and insert indexes
    let db_conn = db::get_db_conn_instance().await;
    if !args.metadata_only {
        preflight_agent_capture_prune_fences(&db_conn, &d1_client, &repo_id).await?;
    }

    for idx in &indexes {
        // Check if exists
        let existing = object_index::Entity::find()
            .filter(object_index::Column::OId.eq(&idx.o_id))
            .filter(object_index::Column::RepoId.eq(&idx.repo_id))
            .one(&db_conn)
            .await
            .map_err(|_| {
                CloudError::Generic(
                    "failed to query local object-index state while restoring cloud metadata"
                        .to_string(),
                )
            })?;

        if let Some(existing_model) = existing {
            let mut active: object_index::ActiveModel = existing_model.into();
            active.is_synced = Set(1);
            if active.update(&db_conn).await.is_err() {
                emit_warning("failed to update a cloud object index".to_string());
            }
        } else {
            let entry = object_index::ActiveModel {
                o_id: Set(idx.o_id.clone()),
                o_type: Set(idx.o_type.clone()),
                o_size: Set(idx.o_size),
                repo_id: Set(idx.repo_id.clone()),
                created_at: Set(idx.created_at),
                is_synced: Set(1), // Already synced since we're restoring from cloud
                ..Default::default()
            };

            if entry.insert(&db_conn).await.is_err() {
                emit_warning("failed to insert a cloud object index".to_string());
            }
        }
    }

    println!(
        "Restored {} object indexes to local database.",
        indexes.len()
    );

    // Update local config with restored repo_id
    let _ = ConfigKv::set("libra.repoid", &repo_id, false).await;

    if args.metadata_only {
        println!("Metadata-only restore complete.");
        return Ok(());
    }

    // Download objects from R2
    let r2_storage = create_r2_storage(&repo_id).await?;
    let objects_path = path::objects();
    let local_storage = LocalStorage::new(objects_path);

    let report =
        restore_indexed_objects_from_remote(&indexes, &r2_storage, &local_storage, repository_kind)
            .await?;
    for warning in &report.warnings {
        eprintln!("{warning}");
    }

    println!(
        "Restore complete: {} downloaded, {} skipped (already exist), {} failed",
        report.downloaded, report.skipped, report.failed
    );

    if report.failed > 0 {
        Err(CloudError::PartialTransfer(format!(
            "{} objects failed to restore",
            report.failed
        )))
    } else {
        // Restore metadata
        let deferred_capture_refs = match restore_metadata(&db_conn, &r2_storage).await {
            Ok(deferred) => deferred,
            Err(e) => {
                emit_warning(format!("failed to restore metadata: {}", e));
                Vec::new()
            }
        };

        // Post-restore: update HEAD and restore worktree if we're in a fresh repo state.
        // We do this BEFORE the agent-capture restore so that a strict
        // agent-capture failure (Codex Q2: hard-fail on partial restore)
        // doesn't leave the user with a populated objects/refs but no
        // worktree. The agent_session / agent_checkpoint catalogue is
        // metadata about external agent runs — it's not blocking for the
        // user to start working in the restored tree (Codex Q3).

        // Check if HEAD has a commit (either restored or existing)
        let head_commit = Head::current_commit_result().await.map_err(|error| {
            CloudError::Generic(format!("failed to resolve HEAD commit: {error}"))
        })?;

        if let Some(commit) = head_commit {
            println!("Restoring working directory to HEAD ({})", commit);
            let _ = restore_worktree_to_head(true).await;
        } else {
            println!("Restoring working directory (fallback)...");

            // Try to find 'main' branch in references
            // We look for 'main' branch in the reference table as a fallback
            let main_branch = Branch::find_branch_result("main", None)
                .await
                .map_err(|error| {
                    CloudError::Generic(format!("failed to resolve main branch: {error}"))
                })?;

            if let Some(branch) = main_branch {
                println!("Found main branch: {}", branch.commit);

                // Update HEAD to point to main
                Head::update(Head::Branch("main".to_string()), None).await;

                let _ = restore_worktree_to_head(true).await;
            } else {
                println!("No HEAD commit or main branch found. Skipping worktree restore.");
            }
        }

        // CEX-EntireIO §14.3 acceptance: pull `agent_session` /
        // `agent_checkpoint` rows back from D1 so the new machine sees the
        // captured-agent listing without having to re-ingest hooks. This
        // runs LAST (after worktree restore) per Codex Q3 — the inner
        // helper is strict (Q2), so propagating its error here surfaces
        // partial-restore problems to the caller without blocking the
        // worktree materialization that runs above.
        let capture_outcome = restore_agent_capture_from_d1(&db_conn, &d1_client, &repo_id, true)
            .await
            .map_err(agent_capture_restore_failure)?;
        restore_legacy_capture_refs_if_unowned(&db_conn, deferred_capture_refs, capture_outcome)
            .await?;

        Ok(())
    }
}

pub(crate) async fn restore_worktree_to_head(render_human: bool) -> CloudResult<()> {
    let restore_args = RestoreWorktreeArgs {
        overlay: false,
        no_overlay: false,
        ours: false,
        theirs: false,
        ignore_unmerged: false,
        merge: false,
        conflict: None,
        pathspec: vec![".".to_string()], // restore everything
        source: Some("HEAD".to_string()),
        worktree: true,
        staged: true,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        no_progress: false,
    };

    if let Err(e) = restore_cmd::execute_checked(restore_args).await {
        emit_warning(format!("failed to restore worktree files: {}", e));
        Err(CloudError::Generic(format!(
            "failed to restore worktree files: {e}"
        )))
    } else {
        if render_human {
            println!("Successfully restored working directory files.");
        }
        Ok(())
    }
}

pub(crate) async fn restore_agent_capture_from_d1(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    repo_id: &str,
    render_human: bool,
) -> CloudResult<AgentCaptureRestoreOutcome> {
    tokio::time::timeout(
        AGENT_CAPTURE_CLOUD_DEADLINE,
        restore_agent_capture_from_d1_inner(db_conn, d1_client, repo_id, render_human),
    )
    .await
    .map_err(|_| {
        CloudError::PartialTransfer(
            "agent capture cloud restore exceeded its 120-second deadline; retry the operation"
                .to_string(),
        )
    })?
}

pub(crate) async fn restore_agent_capture_from_d1_inner(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    repo_id: &str,
    render_human: bool,
) -> CloudResult<AgentCaptureRestoreOutcome> {
    use sea_orm::{ConnectionTrait, Statement};

    // Codex round-2 follow-up: check BOTH tables locally — a partial
    // schema (e.g. `agent_session` exists but `agent_checkpoint` does not
    // because a half-applied legacy migration left things mid-flight)
    // would otherwise bypass the warning and either fail loudly later or
    // silently succeed with no checkpoint rows. Warn and bail in that
    // case so the user gets a single actionable hint.
    let backend = db_conn.get_database_backend();
    let session_present = db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_session' LIMIT 1",
            [],
        ))
        .await
        .map_err(|e| local_agent_capture_restore_failure("probe local capture schema", e))?
        .is_some();
    let checkpoint_present = db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_checkpoint' LIMIT 1",
            [],
        ))
        .await
        .map_err(|e| local_agent_capture_restore_failure("probe local capture schema", e))?
        .is_some();
    if !session_present || !checkpoint_present {
        // Codex review Q4: emit an actionable hint instead of silently
        // succeeding so a user on an old binary knows why their session
        // list is empty after restore. Round-2 expanded this check to
        // include `agent_checkpoint` so a partial schema can't sneak past.
        emit_warning(
            "agent_session / agent_checkpoint table absent locally — restore skipped. \
             Run `libra init` (or upgrade libra) to create the schema, \
             then rerun `libra cloud restore`.",
        );
        return Ok(AgentCaptureRestoreOutcome::NoGeneration);
    }

    if render_human {
        println!("Restoring agent capture catalog from D1...");
    }

    // Restore is a read-only consumer of remote capture schema. In
    // particular, it must not call `ensure_agent_capture_generation_table`:
    // that sync-only migration installs persistent old-writer barriers and
    // adopts legacy rows. A failed restore must never change which clients can
    // write the backup. Legacy remotes with no capture rows are valid Git-only
    // backups and simply have no capture layer to restore.
    let generation_table_present = d1_client
        .agent_capture_generation_table_exists()
        .await
        .map_err(|error| cloud_d1_failure("probe agent-capture generation schema", &error))?;
    if !generation_table_present {
        let has_capture_rows = d1_client
            .agent_capture_catalog_has_rows(repo_id)
            .await
            .map_err(|error| cloud_d1_failure("probe legacy agent-capture catalog", &error))?;
        validate_missing_capture_manifest(has_capture_rows)?;
        if render_human {
            println!("Agent capture restore: remote catalog is empty (skipped).");
        }
        return Ok(AgentCaptureRestoreOutcome::NoGeneration);
    }
    let subagent_content_present = d1_client
        .agent_subagent_content_tables_exist()
        .await
        .map_err(|error| cloud_d1_failure("probe subagent-content schema", &error))?;

    let local_cloud_base = load_local_agent_capture_cloud_base(db_conn, repo_id).await?;
    let mut coherent = None;
    for _ in 0..3 {
        let before = d1_client
            .get_agent_capture_generation(repo_id)
            .await
            .map_err(|error| cloud_d1_failure("read agent-capture generation", &error))?;
        let Some(before) = before else {
            let has_capture_rows = d1_client
                .agent_capture_catalog_has_rows(repo_id)
                .await
                .map_err(|error| {
                    cloud_d1_failure("probe unmanifested agent-capture catalog", &error)
                })?;
            validate_missing_capture_manifest(has_capture_rows)?;
            if render_human {
                println!("Agent capture restore: remote catalog is empty (skipped).");
            }
            return Ok(AgentCaptureRestoreOutcome::NoGeneration);
        };
        if before.state != "complete" {
            return Err(CloudError::PartialTransfer(
                "remote agent capture publication is incomplete; retry `libra cloud sync`, then restore"
                    .to_string(),
            ));
        }
        let expected_object_digest = before.object_index_digest.as_deref().ok_or_else(|| {
            CloudError::PartialTransfer(
                "remote agent capture manifest predates object-index fencing; run `libra cloud sync` with the current version, then restore"
                    .to_string(),
            )
        })?;
        let expected_object_count = before.object_index_count.ok_or_else(|| {
            CloudError::PartialTransfer(
                "remote agent capture manifest has no object-index count; run `libra cloud sync` with the current version, then restore"
                .to_string(),
            )
        })?;
        let object_manifest_scope =
            AgentCaptureObjectManifestScope::parse(before.object_index_scope.as_deref())?;
        let expected_object_generation = before.object_index_generation.ok_or_else(|| {
            CloudError::PartialTransfer(
                "remote agent capture manifest has no object-index catalog generation; run `libra cloud sync` with the current version, then restore"
                    .to_string(),
            )
        })?;
        let (mut rows, remaining_restore_rows) =
            load_remote_agent_capture_rows(d1_client, repo_id, subagent_content_present).await?;
        rows.sessions = project_agent_capture_restore_sessions(&rows.sessions, "remote restore")?;
        validate_agent_capture_traces_shape(
            &rows.checkpoints,
            before.traces_head.as_deref(),
            "remote",
        )?;
        validate_agent_capture_companions(
            &rows.checkpoints,
            &rows.claims,
            &rows.revisions,
            &rows.links,
            "remote restore",
            CompanionValidationMode::Complete,
        )?;
        validate_agent_capture_session_dependencies(
            &rows.sessions,
            &rows.checkpoints,
            &rows.claims,
            "remote restore",
        )?;
        let durability_specs = rows
            .checkpoints
            .iter()
            .map(
                |checkpoint| crate::internal::ai::history::CheckpointDurabilitySpec {
                    checkpoint_id: &checkpoint.checkpoint_id,
                    traces_commit: &checkpoint.traces_commit,
                    tree_oid: &checkpoint.tree_oid,
                    metadata_blob_oid: &checkpoint.metadata_blob_oid,
                },
            )
            .collect::<Vec<_>>();
        let durability_deadline =
            std::time::Instant::now().checked_add(std::time::Duration::from_secs(110));
        let durable_oids = if durability_specs.is_empty() {
            HashSet::new()
        } else {
            let traces_head = before.traces_head.as_deref().ok_or_else(|| {
                CloudError::PartialTransfer(
                    "remote agent capture manifest has checkpoints but no fenced traces head; run `libra cloud sync` with the current version, then restore"
                        .to_string(),
                )
            })?;
            let cataloged_commits = rows
                .checkpoints
                .iter()
                .map(|row| row.traces_commit.clone())
                .collect::<Vec<_>>();
            crate::internal::ai::history::checkpoint_rows_snapshot_durable_oids_from_head(
                &util::storage_path(),
                traces_head,
                &cataloged_commits,
                &durability_specs,
                durability_deadline,
            )
            .await
            .map_err(|_| {
                CloudError::PartialTransfer(
                    "restored agent-capture objects failed content or reachability validation; retry cloud restore, or run `libra agent doctor --repair` if the local object store remains damaged"
                        .to_string(),
                )
            })?
        };
        let mut required_oids = durable_oids.iter().cloned().collect::<Vec<_>>();
        required_oids.sort();
        let (object_indexes, observed_object_generation) = match object_manifest_scope {
            AgentCaptureObjectManifestScope::CheckpointProjection => {
                if required_oids.len() > remaining_restore_rows {
                    return Err(CloudError::PartialTransfer(format!(
                        "remote agent-capture restore exceeds its aggregate {}-row safety bound before reading object indexes",
                        AGENT_CAPTURE_RESTORE_MAX_ROWS
                    )));
                }
                d1_client
                    .get_object_indexes_by_oids_with_generation(repo_id, &required_oids)
                    .await
                    .map_err(|error| {
                        cloud_d1_failure("read fenced agent-capture object indexes", &error)
                    })?
            }
            AgentCaptureObjectManifestScope::FullRemoteIndex => d1_client
                .get_object_indexes_bounded_with_generation(repo_id, remaining_restore_rows)
                .await
                .map_err(|error| {
                    cloud_d1_failure("read retained agent-capture object manifest", &error)
                })?,
        };
        remaining_restore_rows
            .checked_sub(object_indexes.len())
            .ok_or_else(|| {
                CloudError::PartialTransfer(format!(
                    "remote agent-capture restore exceeds its aggregate {}-row safety bound while reading object indexes",
                    AGENT_CAPTURE_RESTORE_MAX_ROWS
                ))
            })?;
        let fenced_oids = object_indexes
            .iter()
            .map(|index| index.o_id.as_str())
            .collect::<HashSet<_>>();
        if let Some(unsynced) = object_indexes.iter().find(|index| index.is_synced != 1) {
            let _ = unsynced;
            return Err(CloudError::PartialTransfer(
                "agent-capture manifest includes an object that is not marked synced".to_string(),
            ));
        }
        if let Some(missing) = durable_oids
            .iter()
            .find(|oid| !fenced_oids.contains(oid.as_str()))
        {
            let _ = missing;
            return Err(CloudError::PartialTransfer(
                "agent-capture generation requires an object whose fenced index row is missing; retry `libra cloud sync`, then restore"
                    .to_string(),
            ));
        }
        let (observed_object_digest, observed_object_count) =
            agent_capture_object_index_digest(&object_indexes)?;
        let after = d1_client
            .get_agent_capture_generation(repo_id)
            .await
            .map_err(|error| cloud_d1_failure("recheck agent-capture generation", &error))?;
        if after.as_ref() == Some(&before)
            && observed_object_digest == expected_object_digest
            && observed_object_count == expected_object_count
            && observed_object_generation >= expected_object_generation
        {
            coherent = Some((rows, before.traces_head.clone(), before.generation));
            break;
        }
    }
    let (rows, traces_head, remote_generation) = coherent.ok_or_else(|| {
        CloudError::PartialTransfer(
            "remote agent capture changed during three bounded restore reads; retry when cloud sync is idle"
                .to_string(),
        )
    })?;

    restore_agent_capture_from_rows_with_subagents(
        db_conn,
        AgentCaptureRestoreRows {
            sessions: &rows.sessions,
            checkpoints: &rows.checkpoints,
            claims: &rows.claims,
            revisions: &rows.revisions,
            links: &rows.links,
            traces_head: traces_head.as_deref(),
            remote_is_known_ancestor: local_cloud_base == Some(remote_generation),
        },
        render_human,
    )
    .await?;
    // PD-03: propagate the remote session tombstones to THIS machine, so
    // a later local import cannot resurrect a session erased elsewhere.
    persist_local_import_tombstones(db_conn, &rows.import_tombstones).await?;
    store_local_agent_capture_cloud_base(db_conn, repo_id, remote_generation).await?;
    Ok(AgentCaptureRestoreOutcome::GenerationInstalled)
}

#[allow(dead_code)]
pub(crate) async fn restore_agent_capture_from_rows(
    db_conn: &sea_orm::DatabaseConnection,
    session_rows: &[AgentSessionV2Row],
    checkpoint_rows: &[AgentCheckpointV2Row],
    render_human: bool,
) -> CloudResult<()> {
    restore_agent_capture_from_rows_with_subagents(
        db_conn,
        AgentCaptureRestoreRows {
            sessions: session_rows,
            checkpoints: checkpoint_rows,
            claims: &[],
            revisions: &[],
            links: &[],
            traces_head: None,
            remote_is_known_ancestor: true,
        },
        render_human,
    )
    .await
}

pub(crate) async fn restore_agent_capture_from_rows_with_subagents(
    db_conn: &sea_orm::DatabaseConnection,
    rows: AgentCaptureRestoreRows<'_>,
    render_human: bool,
) -> CloudResult<()> {
    use sea_orm::Statement;

    let AgentCaptureRestoreRows {
        sessions: session_rows,
        checkpoints: checkpoint_rows,
        claims: claim_rows,
        revisions: revision_rows,
        links: link_rows,
        traces_head,
        remote_is_known_ancestor,
    } = rows;

    let session_rows = project_agent_capture_restore_sessions(session_rows, "restored remote")?;
    let session_rows = &session_rows[..];
    validate_agent_capture_companions(
        checkpoint_rows,
        claim_rows,
        revision_rows,
        link_rows,
        "restored remote",
        CompanionValidationMode::Complete,
    )?;
    validate_agent_capture_session_dependencies(
        session_rows,
        checkpoint_rows,
        claim_rows,
        "restored remote",
    )?;
    // Reads the existing refs/rows before rewriting them (`restore fenced
    // traces ref` below), so the write lock is taken up front.
    let txn = crate::internal::db::begin_write_transaction(db_conn)
        .await
        .map_err(|error| {
            local_agent_capture_restore_failure("begin atomic agent capture restore", error)
        })?;
    let backend = txn.get_database_backend();

    // An ordinary retention prune is a durable local deletion intent. The
    // remote may still expose its previous complete generation until the next
    // sync publishes that tombstone, so fail before changing the traces ref or
    // catalog instead of resurrecting a checkpoint and its companion rows.
    if !checkpoint_rows.is_empty() {
        let tombstone_rows = txn
            .query_all_raw(Statement::from_string(
                backend,
                format!(
                    "SELECT checkpoint_id FROM agent_checkpoint_prune_tombstone \
                     ORDER BY checkpoint_id LIMIT {}",
                    AGENT_CAPTURE_MAX_ROWS_PER_TABLE.saturating_add(1)
                ),
            ))
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure(
                    "inspect local checkpoint prune tombstones before restore",
                    error,
                )
            })?;
        if tombstone_rows.len() > AGENT_CAPTURE_MAX_ROWS_PER_TABLE {
            return Err(CloudError::Generic(format!(
                "local checkpoint prune tombstones exceed the {}-row restore safety bound; \
                 run `libra cloud sync` before restoring",
                AGENT_CAPTURE_MAX_ROWS_PER_TABLE
            )));
        }
        let remote_checkpoint_ids = checkpoint_rows
            .iter()
            .map(|row| row.checkpoint_id.as_str())
            .collect::<HashSet<_>>();
        for row in tombstone_rows {
            let checkpoint_id = row
                .try_get_by::<String, _>("checkpoint_id")
                .map_err(|error| {
                    local_agent_capture_restore_failure(
                        "decode local checkpoint prune tombstone before restore",
                        error,
                    )
                })?;
            if remote_checkpoint_ids.contains(checkpoint_id.as_str()) {
                return Err(CloudError::PartialTransfer(
                    "a remote checkpoint was already pruned locally; run `libra cloud sync` to publish the prune tombstone before restoring"
                        .to_string(),
                ));
            }
        }
    }

    let existing_traces_ref = txn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT id, `commit` FROM reference
             WHERE name = ? AND kind = 'Branch' AND remote IS NULL LIMIT 1",
            [crate::internal::branch::TRACES_BRANCH.into()],
        ))
        .await
        .map_err(|error| local_agent_capture_restore_failure("inspect local traces ref", error))?;
    let existing_head = existing_traces_ref
        .as_ref()
        .map(|row| row.try_get_by::<Option<String>, _>("commit"))
        .transpose()
        .map_err(|error| local_agent_capture_restore_failure("decode local traces ref", error))?
        .flatten();
    if existing_head.as_deref() != traces_head {
        let local_checkpoint_count = txn
            .query_one_raw(Statement::from_string(
                backend,
                "SELECT COUNT(*) AS n FROM agent_checkpoint".to_string(),
            ))
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure("count local checkpoints before restore", error)
            })?
            .ok_or_else(|| {
                CloudError::Generic("local checkpoint count returned no row".to_string())
            })?
            .try_get_by::<i64, _>("n")
            .map_err(|error| {
                local_agent_capture_restore_failure("decode checkpoint count", error)
            })?;
        if local_checkpoint_count != 0 {
            return Err(CloudError::Generic(
                "the fenced cloud traces head conflicts with existing local checkpoint history; restore into an empty repository or sync the newer local history first"
                    .to_string(),
            ));
        }
    }
    if let Some(row) = existing_traces_ref {
        let ref_id: i64 = row
            .try_get_by("id")
            .map_err(|error| local_agent_capture_restore_failure("decode traces ref id", error))?;
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "UPDATE reference SET `commit` = ? WHERE id = ?",
            [traces_head.map(str::to_string).into(), ref_id.into()],
        ))
        .await
        .map_err(|error| local_agent_capture_restore_failure("restore fenced traces ref", error))?;
    } else {
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO reference (name, kind, `commit`, remote, worktree_id)
             VALUES (?, 'Branch', ?, NULL, NULL)",
            [
                crate::internal::branch::TRACES_BRANCH.into(),
                traces_head.map(str::to_string).into(),
            ],
        ))
        .await
        .map_err(|error| local_agent_capture_restore_failure("create fenced traces ref", error))?;
    }

    // PD-03 tombstone-first, LOCAL side. The remote catalog is filtered by
    // the tombstones the mirror knows about, but a session erased here and
    // not yet propagated is invisible to that filter — restoring from a
    // stale mirror would otherwise bring it back. The schema triggers would
    // abort such a write, which is the right outcome but the wrong
    // experience: the whole restore fails on an opaque RAISE(ABORT) instead
    // of quietly declining the rows the user already erased. Read the local
    // tombstones inside the SAME transaction and skip them.
    let mut locally_erased_sessions: HashSet<(String, String)> = HashSet::new();
    let mut locally_erased_session_ids: HashSet<String> = HashSet::new();
    for row in txn
        .query_all_raw(Statement::from_string(
            backend,
            "SELECT agent_kind, provider_session_id, erased_session_id \
             FROM agent_import_tombstone",
        ))
        .await
        .map_err(|error| {
            local_agent_capture_restore_failure("read local erasure tombstones", error)
        })?
    {
        let agent_kind: String = row.try_get_by("agent_kind").map_err(|error| {
            local_agent_capture_restore_failure("decode local tombstone", error)
        })?;
        let provider_session_id: String =
            row.try_get_by("provider_session_id").map_err(|error| {
                local_agent_capture_restore_failure("decode local tombstone", error)
            })?;
        let erased_session_id: String = row.try_get_by("erased_session_id").map_err(|error| {
            local_agent_capture_restore_failure("decode local tombstone", error)
        })?;
        locally_erased_sessions.insert((agent_kind, provider_session_id));
        locally_erased_session_ids.insert(erased_session_id);
    }
    // Collect the dropped sessions' REMOTE ids before filtering: a
    // checkpoint references its session by id, and the mirror's id for an
    // erased session need not match the local `erased_session_id`.
    let dropped_session_ids: HashSet<String> = session_rows
        .iter()
        .filter(|row| {
            locally_erased_sessions
                .contains(&(row.agent_kind.clone(), row.provider_session_id.clone()))
        })
        .map(|row| row.session_id.clone())
        .collect();
    let session_rows: Vec<_> = session_rows
        .iter()
        .filter(|row| {
            !locally_erased_sessions
                .contains(&(row.agent_kind.clone(), row.provider_session_id.clone()))
        })
        .cloned()
        .collect();
    let session_rows = &session_rows[..];
    let checkpoint_rows: Vec<_> = checkpoint_rows
        .iter()
        .filter(|row| {
            !locally_erased_session_ids.contains(&row.session_id)
                && !dropped_session_ids.contains(&row.session_id)
        })
        .cloned()
        .collect();
    let checkpoint_rows = &checkpoint_rows[..];

    // Validate every immutable/mutable companion conflict before applying any
    // row. The surrounding transaction guarantees a later SQL/FK failure also
    // rolls back sessions, checkpoints, skeleton claims, revisions, and links.
    let mut newer_local_sessions = HashSet::new();
    for row in session_rows {
        let existing = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT session_id, agent_kind, provider_session_id, state, working_dir,
                        worktree_id, parent_commit, parent_session_id, metadata_json,
                        redaction_report, started_at, last_event_at, stopped_at,
                        schema_version, sync_revision
                 FROM agent_session WHERE agent_kind = ? AND provider_session_id = ?",
                [
                    row.agent_kind.clone().into(),
                    row.provider_session_id.clone().into(),
                ],
            ))
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure("inspect local agent session", error)
            })?;
        if let Some(existing) = existing {
            let local = AgentSessionV2Row {
                session_id: existing.try_get_by("session_id").map_err(|error| {
                    local_agent_capture_restore_failure("decode session", error)
                })?,
                agent_kind: existing.try_get_by("agent_kind").map_err(|error| {
                    local_agent_capture_restore_failure("decode session", error)
                })?,
                provider_session_id: existing.try_get_by("provider_session_id").map_err(
                    |error| local_agent_capture_restore_failure("decode session", error),
                )?,
                state: existing.try_get_by("state").map_err(|error| {
                    local_agent_capture_restore_failure("decode session", error)
                })?,
                working_dir: existing.try_get_by("working_dir").map_err(|error| {
                    local_agent_capture_restore_failure("decode session", error)
                })?,
                worktree_id: existing.try_get_by("worktree_id").map_err(|error| {
                    local_agent_capture_restore_failure("decode session", error)
                })?,
                parent_commit: existing.try_get_by("parent_commit").map_err(|error| {
                    local_agent_capture_restore_failure("decode session", error)
                })?,
                parent_session_id: existing.try_get_by("parent_session_id").map_err(|error| {
                    local_agent_capture_restore_failure("decode session", error)
                })?,
                metadata_json: existing.try_get_by("metadata_json").map_err(|error| {
                    local_agent_capture_restore_failure("decode session", error)
                })?,
                redaction_report: existing.try_get_by("redaction_report").map_err(|error| {
                    local_agent_capture_restore_failure("decode session", error)
                })?,
                started_at: existing.try_get_by("started_at").map_err(|error| {
                    local_agent_capture_restore_failure("decode session", error)
                })?,
                last_event_at: existing.try_get_by("last_event_at").map_err(|error| {
                    local_agent_capture_restore_failure("decode session", error)
                })?,
                stopped_at: existing.try_get_by("stopped_at").map_err(|error| {
                    local_agent_capture_restore_failure("decode session", error)
                })?,
                schema_version: existing.try_get_by("schema_version").map_err(|error| {
                    local_agent_capture_restore_failure("decode session", error)
                })?,
                sync_revision: existing.try_get_by("sync_revision").map_err(|error| {
                    local_agent_capture_restore_failure("decode session", error)
                })?,
            };
            if local.sync_revision > row.sync_revision {
                if !remote_is_known_ancestor {
                    return Err(CloudError::Generic(
                        "a local agent session has a larger divergent sync revision without a recorded cloud ancestor; sync or restore from the clone that owns the current cloud lineage"
                            .to_string(),
                    ));
                }
                newer_local_sessions
                    .insert((row.agent_kind.clone(), row.provider_session_id.clone()));
            } else if local.sync_revision == row.sync_revision
                // The remote copy is a projection, so compare like with like;
                // the upsert below leaves equal-generation local evidence
                // (including legacy ownership) untouched.
                && !project_agent_session_for_cloud(&local, "local")
                    .is_ok_and(|projected| projected == *row)
            {
                return Err(CloudError::Generic(
                    "restored agent session conflicts with local state at the same sync generation"
                        .to_string(),
                ));
            } else if local.session_id != row.session_id {
                return Err(CloudError::Generic(
                    "restored agent session conflicts with local provider ownership".to_string(),
                ));
            }
        }
    }
    for row in checkpoint_rows {
        let existing = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT session_id, parent_checkpoint_id, scope, parent_commit, tree_oid,
                        metadata_blob_oid, traces_commit, tool_use_id, subagent_session_id,
                        description, created_at, sync_revision
                 FROM agent_checkpoint WHERE checkpoint_id = ?",
                [row.checkpoint_id.clone().into()],
            ))
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure("inspect local checkpoint", error)
            })?;
        if let Some(existing) = existing {
            let local = AgentCheckpointV2Row {
                checkpoint_id: row.checkpoint_id.clone(),
                session_id: existing.try_get_by("session_id").map_err(|error| {
                    local_agent_capture_restore_failure("decode checkpoint", error)
                })?,
                parent_checkpoint_id: existing.try_get_by("parent_checkpoint_id").map_err(
                    |error| local_agent_capture_restore_failure("decode checkpoint", error),
                )?,
                scope: existing.try_get_by("scope").map_err(|error| {
                    local_agent_capture_restore_failure("decode checkpoint", error)
                })?,
                parent_commit: existing.try_get_by("parent_commit").map_err(|error| {
                    local_agent_capture_restore_failure("decode checkpoint", error)
                })?,
                tree_oid: existing.try_get_by("tree_oid").map_err(|error| {
                    local_agent_capture_restore_failure("decode checkpoint", error)
                })?,
                metadata_blob_oid: existing.try_get_by("metadata_blob_oid").map_err(|error| {
                    local_agent_capture_restore_failure("decode checkpoint", error)
                })?,
                traces_commit: existing.try_get_by("traces_commit").map_err(|error| {
                    local_agent_capture_restore_failure("decode checkpoint", error)
                })?,
                tool_use_id: existing.try_get_by("tool_use_id").map_err(|error| {
                    local_agent_capture_restore_failure("decode checkpoint", error)
                })?,
                subagent_session_id: existing.try_get_by("subagent_session_id").map_err(
                    |error| local_agent_capture_restore_failure("decode checkpoint", error),
                )?,
                description: existing.try_get_by("description").map_err(|error| {
                    local_agent_capture_restore_failure("decode checkpoint", error)
                })?,
                created_at: existing.try_get_by("created_at").map_err(|error| {
                    local_agent_capture_restore_failure("decode checkpoint", error)
                })?,
                sync_revision: existing.try_get_by("sync_revision").map_err(|error| {
                    local_agent_capture_restore_failure("decode checkpoint", error)
                })?,
            };
            if local.sync_revision == row.sync_revision && local != *row {
                return Err(CloudError::Generic(
                    "restored checkpoint conflicts with local history at the same sync generation"
                        .to_string(),
                ));
            }
            if local.sync_revision > row.sync_revision && !remote_is_known_ancestor {
                return Err(CloudError::Generic(
                    "a local checkpoint has a larger divergent sync revision without a recorded cloud ancestor; sync or restore from the clone that owns the current cloud lineage"
                        .to_string(),
                ));
            }
            if local.sync_revision != row.sync_revision
                && !checkpoint_rewrite_compatible(&local, row)
            {
                return Err(CloudError::Generic(
                    "restored checkpoint conflicts with immutable local history".to_string(),
                ));
            }
        }
    }
    let mut newer_local_claims = HashSet::new();
    for row in revision_rows {
        let existing = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT checkpoint_id, content_digest, source_channel, partial, created_at
                 FROM agent_subagent_content_revision
                 WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
                   AND content_schema_version = ? AND revision = ?",
                [
                    row.parent_session_id.clone().into(),
                    row.provider_kind.clone().into(),
                    row.source_key.clone().into(),
                    row.content_schema_version.into(),
                    row.revision.into(),
                ],
            ))
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure("inspect local subagent revision", error)
            })?;
        if let Some(existing) = existing {
            let exact = existing
                .try_get_by::<String, _>("checkpoint_id")
                .map_err(|error| local_agent_capture_restore_failure("decode revision", error))?
                == row.checkpoint_id
                && existing
                    .try_get_by::<String, _>("content_digest")
                    .map_err(|error| {
                        local_agent_capture_restore_failure("decode revision", error)
                    })?
                    == row.content_digest
                && existing
                    .try_get_by::<String, _>("source_channel")
                    .map_err(|error| {
                        local_agent_capture_restore_failure("decode revision", error)
                    })?
                    == row.source_channel
                && existing.try_get_by::<i64, _>("partial").map_err(|error| {
                    local_agent_capture_restore_failure("decode revision", error)
                })? == row.partial
                && existing
                    .try_get_by::<i64, _>("created_at")
                    .map_err(|error| {
                        local_agent_capture_restore_failure("decode revision", error)
                    })?
                    == row.created_at;
            if !exact {
                return Err(CloudError::Generic(
                    "restored subagent revision conflicts with immutable local history".to_string(),
                ));
            }
        }
    }
    for row in claim_rows {
        let existing = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT revision_cursor, sync_revision, current_revision, current_checkpoint_id,
                        current_digest, state
                 FROM agent_subagent_content_claim
                 WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
                   AND content_schema_version = ?",
                [
                    row.parent_session_id.clone().into(),
                    row.provider_kind.clone().into(),
                    row.source_key.clone().into(),
                    row.content_schema_version.into(),
                ],
            ))
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure("inspect local subagent claim", error)
            })?;
        if let Some(existing) = existing {
            let state: String = existing.try_get_by("state").map_err(|error| {
                local_agent_capture_restore_failure("decode claim state", error)
            })?;
            if state != "idle" {
                return Err(CloudError::Generic(
                    "cannot restore a subagent claim while a local writer reservation is active"
                        .to_string(),
                ));
            }
            let sync_revision: i64 = existing.try_get_by("sync_revision").map_err(|error| {
                local_agent_capture_restore_failure("decode claim sync generation", error)
            })?;
            if sync_revision > row.sync_revision {
                if !remote_is_known_ancestor {
                    return Err(CloudError::Generic(
                        "a local subagent claim has a larger divergent sync revision without a recorded cloud ancestor; restore the current cloud snapshot before retrying"
                            .to_string(),
                    ));
                }
                newer_local_claims.insert(claim_key(row));
            } else if sync_revision == row.sync_revision {
                let exact = existing
                    .try_get_by::<i64, _>("revision_cursor")
                    .map_err(|error| {
                        local_agent_capture_restore_failure("decode claim cursor", error)
                    })?
                    == row.revision_cursor
                    && existing
                        .try_get_by::<i64, _>("current_revision")
                        .map_err(|error| {
                            local_agent_capture_restore_failure("decode claim revision", error)
                        })?
                        == row.current_revision
                    && existing
                        .try_get_by::<Option<String>, _>("current_checkpoint_id")
                        .map_err(|error| {
                            local_agent_capture_restore_failure("decode claim checkpoint", error)
                        })?
                        == row.current_checkpoint_id
                    && existing
                        .try_get_by::<Option<String>, _>("current_digest")
                        .map_err(|error| {
                            local_agent_capture_restore_failure("decode claim digest", error)
                        })?
                        == row.current_digest;
                if !exact {
                    return Err(CloudError::Generic(
                        "restored subagent claim conflicts with local state at the same sync generation"
                            .to_string(),
                    ));
                }
            }
        }
    }
    let mut newer_local_links = HashSet::new();
    for row in link_rows {
        let existing = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT parent_session_id, link_state, boundary_checkpoint_id,
                        stable_subagent_id, sync_revision, created_at, updated_at
                 FROM agent_subagent_link WHERE content_checkpoint_id = ?",
                [row.content_checkpoint_id.clone().into()],
            ))
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure("inspect local subagent link", error)
            })?;
        if let Some(existing) = existing {
            let sync_revision: i64 = existing.try_get_by("sync_revision").map_err(|error| {
                local_agent_capture_restore_failure("decode link revision", error)
            })?;
            if sync_revision > row.sync_revision {
                if !remote_is_known_ancestor {
                    return Err(CloudError::Generic(
                        "a local subagent link has a larger divergent sync revision without a recorded cloud ancestor"
                            .to_string(),
                    ));
                }
                newer_local_links.insert(row.content_checkpoint_id.clone());
            } else if sync_revision == row.sync_revision {
                let exact = existing
                    .try_get_by::<String, _>("parent_session_id")
                    .map_err(|error| local_agent_capture_restore_failure("decode link", error))?
                    == row.parent_session_id
                    && existing
                        .try_get_by::<String, _>("link_state")
                        .map_err(|error| {
                            local_agent_capture_restore_failure("decode link", error)
                        })?
                        == row.link_state
                    && existing
                        .try_get_by::<Option<String>, _>("boundary_checkpoint_id")
                        .map_err(|error| {
                            local_agent_capture_restore_failure("decode link", error)
                        })?
                        == row.boundary_checkpoint_id
                    && existing
                        .try_get_by::<Option<String>, _>("stable_subagent_id")
                        .map_err(|error| {
                            local_agent_capture_restore_failure("decode link", error)
                        })?
                        == row.stable_subagent_id
                    && existing
                        .try_get_by::<i64, _>("created_at")
                        .map_err(|error| {
                            local_agent_capture_restore_failure("decode link", error)
                        })?
                        == row.created_at
                    && existing
                        .try_get_by::<i64, _>("updated_at")
                        .map_err(|error| {
                            local_agent_capture_restore_failure("decode link", error)
                        })?
                        == row.updated_at;
                if !exact {
                    return Err(CloudError::Generic(
                        "restored subagent link conflicts with local state at the same generation"
                            .to_string(),
                    ));
                }
            }
        }
    }

    let mut skipped_scoped_sessions = 0usize;
    for row in session_rows {
        if newer_local_sessions.contains(&(row.agent_kind.clone(), row.provider_session_id.clone()))
        {
            continue;
        }
        // §C.4.1.1: a LOCALLY SCOPED session belongs to a live workspace
        // owner; cloud content carries no scope columns, so overwriting the
        // row would graft foreign material under an immutable owner claim.
        // The UPSERT below skips such rows via its scope predicate — count
        // them so the user learns the restore was partial.
        let scoped_conflict = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT 1 AS hit FROM agent_session \
                 WHERE agent_kind = ? AND provider_session_id = ? \
                   AND scope_state IS 'scoped' AND sync_revision < ?",
                [
                    row.agent_kind.clone().into(),
                    row.provider_session_id.clone().into(),
                    row.sync_revision.into(),
                ],
            ))
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure("probe scoped session conflict", error)
            })?
            .is_some();
        if scoped_conflict {
            skipped_scoped_sessions += 1;
        }
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                worktree_id, parent_commit, parent_session_id, metadata_json,
                redaction_report, started_at, last_event_at, stopped_at, schema_version,
                sync_revision
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(agent_kind, provider_session_id) DO UPDATE SET
                state = excluded.state, working_dir = excluded.working_dir,
                worktree_id = excluded.worktree_id, parent_commit = excluded.parent_commit,
                parent_session_id = excluded.parent_session_id,
                metadata_json = excluded.metadata_json,
                redaction_report = excluded.redaction_report,
                started_at = excluded.started_at,
                last_event_at = excluded.last_event_at,
                stopped_at = excluded.stopped_at,
                schema_version = excluded.schema_version,
                sync_revision = excluded.sync_revision
             WHERE excluded.sync_revision > agent_session.sync_revision
               AND agent_session.scope_state IS NOT 'scoped'",
            [
                row.session_id.clone().into(),
                row.agent_kind.clone().into(),
                row.provider_session_id.clone().into(),
                row.state.clone().into(),
                row.working_dir.clone().into(),
                row.worktree_id.clone().into(),
                row.parent_commit.clone().into(),
                row.parent_session_id.clone().into(),
                row.metadata_json.clone().into(),
                row.redaction_report.clone().into(),
                row.started_at.into(),
                row.last_event_at.into(),
                row.stopped_at.into(),
                row.schema_version.into(),
                row.sync_revision.into(),
            ],
        ))
        .await
        .map_err(|_| CloudError::Generic("failed to restore an agent session".to_string()))?;
    }
    if skipped_scoped_sessions > 0 {
        emit_warning(format!(
            "cloud restore left {skipped_scoped_sessions} locally SCOPED agent session(s) \
             untouched: their rows are owned by a live workspace and cloud content carries \
             no ownership; inspect with `libra worktree doctor`"
        ));
    }
    for row in checkpoint_rows {
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                subagent_session_id, description, created_at, sync_revision
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(checkpoint_id) DO UPDATE SET
                tree_oid = excluded.tree_oid,
                metadata_blob_oid = excluded.metadata_blob_oid,
                traces_commit = excluded.traces_commit,
                sync_revision = excluded.sync_revision
             WHERE excluded.sync_revision > agent_checkpoint.sync_revision",
            [
                row.checkpoint_id.clone().into(),
                row.session_id.clone().into(),
                row.parent_checkpoint_id.clone().into(),
                row.scope.clone().into(),
                row.parent_commit.clone().into(),
                row.tree_oid.clone().into(),
                row.metadata_blob_oid.clone().into(),
                row.traces_commit.clone().into(),
                row.tool_use_id.clone().into(),
                row.subagent_session_id.clone().into(),
                row.description.clone().into(),
                row.created_at.into(),
                row.sync_revision.into(),
            ],
        ))
        .await
        .map_err(|_| CloudError::Generic("failed to restore an agent checkpoint".to_string()))?;
    }

    // Skeleton claims satisfy the revision FK but remain invisible outside the
    // transaction. Current leaves are advanced only after revisions and links.
    for row in claim_rows {
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_subagent_content_claim (
                parent_session_id, provider_kind, source_key, content_schema_version,
                revision_cursor, sync_revision, current_revision, current_checkpoint_id, current_digest,
                state, attempt_digest, attempt_checkpoint_id, owner, lease_expires_at,
                fence_token, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 0, 0, 0, NULL, NULL, 'idle', NULL, NULL, NULL, NULL,
                       ?, ?, ?)
             ON CONFLICT(parent_session_id, provider_kind, source_key,
                         content_schema_version) DO NOTHING",
            [
                row.parent_session_id.clone().into(),
                row.provider_kind.clone().into(),
                row.source_key.clone().into(),
                row.content_schema_version.into(),
                row.fence_token.into(),
                row.created_at.into(),
                row.updated_at.into(),
            ],
        ))
        .await
        .map_err(|error| local_agent_capture_restore_failure("stage restored claim", error))?;
    }
    for row in revision_rows {
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_subagent_content_revision (
                parent_session_id, provider_kind, source_key, content_schema_version,
                revision, checkpoint_id, content_digest, source_channel, partial, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(parent_session_id, provider_kind, source_key,
                         content_schema_version, revision) DO NOTHING",
            [
                row.parent_session_id.clone().into(),
                row.provider_kind.clone().into(),
                row.source_key.clone().into(),
                row.content_schema_version.into(),
                row.revision.into(),
                row.checkpoint_id.clone().into(),
                row.content_digest.clone().into(),
                row.source_channel.clone().into(),
                row.partial.into(),
                row.created_at.into(),
            ],
        ))
        .await
        .map_err(|_| CloudError::Generic("failed to restore a subagent revision".to_string()))?;
    }
    for row in link_rows {
        if newer_local_links.contains(&row.content_checkpoint_id) {
            continue;
        }
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_subagent_link (
                content_checkpoint_id, parent_session_id, link_state,
                boundary_checkpoint_id, stable_subagent_id, sync_revision,
                created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(content_checkpoint_id) DO UPDATE SET
                parent_session_id = excluded.parent_session_id,
                link_state = excluded.link_state,
                boundary_checkpoint_id = excluded.boundary_checkpoint_id,
                stable_subagent_id = excluded.stable_subagent_id,
                sync_revision = excluded.sync_revision,
                created_at = excluded.created_at, updated_at = excluded.updated_at
             WHERE excluded.sync_revision > agent_subagent_link.sync_revision",
            [
                row.content_checkpoint_id.clone().into(),
                row.parent_session_id.clone().into(),
                row.link_state.clone().into(),
                row.boundary_checkpoint_id.clone().into(),
                row.stable_subagent_id.clone().into(),
                row.sync_revision.into(),
                row.created_at.into(),
                row.updated_at.into(),
            ],
        ))
        .await
        .map_err(|_| CloudError::Generic("failed to restore a subagent link".to_string()))?;
    }
    for row in claim_rows {
        if newer_local_claims.contains(&claim_key(row)) {
            continue;
        }
        let result = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE agent_subagent_content_claim
                 SET revision_cursor = ?, sync_revision = ?, current_revision = ?, current_checkpoint_id = ?,
                     current_digest = ?, fence_token = MAX(fence_token, ?),
                     created_at = MIN(created_at, ?), updated_at = MAX(updated_at, ?)
                 WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
                   AND content_schema_version = ? AND state = 'idle'
                   AND (sync_revision < ? OR (
                        sync_revision = ? AND revision_cursor = ? AND current_revision = ?
                        AND current_checkpoint_id IS ? AND current_digest IS ?))",
                [
                    row.revision_cursor.into(),
                    row.sync_revision.into(),
                    row.current_revision.into(),
                    row.current_checkpoint_id.clone().into(),
                    row.current_digest.clone().into(),
                    row.fence_token.into(),
                    row.created_at.into(),
                    row.updated_at.into(),
                    row.parent_session_id.clone().into(),
                    row.provider_kind.clone().into(),
                    row.source_key.clone().into(),
                    row.content_schema_version.into(),
                    row.sync_revision.into(),
                    row.sync_revision.into(),
                    row.revision_cursor.into(),
                    row.current_revision.into(),
                    row.current_checkpoint_id.clone().into(),
                    row.current_digest.clone().into(),
                ],
            ))
            .await
            .map_err(|error| local_agent_capture_restore_failure("advance restored claim", error))?;
        if result.rows_affected() != 1 {
            return Err(CloudError::Generic(
                "restored subagent claim lost its atomic monotonic update fence".to_string(),
            ));
        }
    }

    txn.commit().await.map_err(|error| {
        local_agent_capture_restore_failure("commit atomic agent capture restore", error)
    })?;
    if render_human {
        println!(
            "Agent capture restore: {}/{} sessions, {}/{} checkpoints, {}/{} subagent claims, {}/{} subagent revisions, {}/{} subagent links (0 failed).",
            session_rows.len(),
            session_rows.len(),
            checkpoint_rows.len(),
            checkpoint_rows.len(),
            claim_rows.len(),
            claim_rows.len(),
            revision_rows.len(),
            revision_rows.len(),
            link_rows.len(),
            link_rows.len()
        );
    }
    Ok(())
}

/// Project restored session rows through the cloud ownership boundary before
/// a restore path can create a local transaction or copy either JSON column
/// into the catalog: malformed or invalid V2 records are rejected, and legacy
/// ownership never becomes a new local value. The direct row helper is used
/// by tests and recovery callers, so it needs the same boundary as the
/// D1-backed restore path.
fn project_agent_capture_restore_sessions(
    session_rows: &[AgentSessionV2Row],
    side: &str,
) -> CloudResult<Vec<AgentSessionV2Row>> {
    session_rows
        .iter()
        .map(|session| project_agent_session_for_cloud(session, side))
        .collect()
}

/// Report an agent-capture restore failure with the shipped network-class
/// contract (`LBR-NET-002`). Every inner reason is a fixed diagnostic: D1 and
/// local driver text is redacted where each error is built, so the remedy
/// (prune tombstone, bounded reads, deadline, connectivity) stays visible.
fn agent_capture_restore_failure(error: CloudError) -> CloudError {
    CloudError::D1(format!("agent capture restore failed: {error}"))
}

pub(crate) async fn restore_metadata(
    db_conn: &sea_orm::DatabaseConnection,
    r2_storage: &RemoteStorage,
) -> CloudResult<Vec<reference::Model>> {
    println!("Restoring metadata...");

    let data = match r2_storage.get_metadata().await {
        Ok(data) => data,
        Err(e) => {
            println!("warning: failed to download metadata: {}", e);
            return Ok(Vec::new());
        }
    };
    let deferred_capture_refs = restore_metadata_from_bytes(db_conn, &data).await?;
    println!("Metadata restored.");
    Ok(deferred_capture_refs)
}

pub(crate) async fn restore_metadata_from_bytes(
    db_conn: &sea_orm::DatabaseConnection,
    data: &[u8],
) -> CloudResult<Vec<reference::Model>> {
    let references: Vec<reference::Model> = serde_json::from_slice(data)
        .map_err(|e| CloudError::Generic(format!("Failed to deserialize metadata: {}", e)))?;
    restore_metadata_models(db_conn, references, false).await
}

pub(crate) async fn restore_metadata_models(
    db_conn: &sea_orm::DatabaseConnection,
    references: Vec<reference::Model>,
    strict: bool,
) -> CloudResult<Vec<reference::Model>> {
    restore_metadata_models_with_capture_policy(db_conn, references, strict, true).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::error::StableErrorCode;

    #[test]
    fn remote_repository_id_boundary_rejects_and_redacts_untrusted_text() {
        const REMOTE_SENTINEL: &str =
            "REMOTE_REPOSITORY_ID=/private/provider/session-capture.jsonl";

        let error = validate_remote_repository_id(REMOTE_SENTINEL.to_string())
            .expect_err("remote repository id must be canonical UUID");
        let detail = error.to_string();
        let cli = error.into_cli_error("restore");
        assert!(detail.contains("invalid repository id"));
        assert!(
            !detail.contains(REMOTE_SENTINEL),
            "CloudError must not echo remote repository id: {detail}"
        );
        assert!(
            !cli.message().contains(REMOTE_SENTINEL),
            "human CLI error must not echo remote repository id"
        );
        assert!(
            !format!("{:?}", cli.details()).contains(REMOTE_SENTINEL),
            "structured CLI details must not retain remote repository id"
        );
        assert_eq!(cli.stable_code(), StableErrorCode::CliInvalidTarget);
    }

    #[test]
    fn agent_capture_restore_failure_keeps_head_network_contract() {
        const REMOTE_SENTINEL: &str =
            "REMOTE_PRUNED_CHECKPOINT=/private/provider/session-capture.jsonl";
        let prune_conflict = reject_local_prune_conflicts(
            &[REMOTE_SENTINEL.to_string()],
            &HashSet::from([REMOTE_SENTINEL.to_string()]),
        )
        .expect_err("a locally pruned remote checkpoint must fail restore");
        let d1_failure = cloud_d1_failure(
            "read agent-capture generation",
            &D1Error {
                code: 7500,
                message: REMOTE_SENTINEL.to_string(),
            },
        );
        let local_failure = local_agent_capture_restore_failure(
            "decode session",
            std::io::Error::other(REMOTE_SENTINEL),
        );
        let unstable = CloudError::PartialTransfer(
            "remote agent capture changed during three bounded restore reads; retry when cloud sync is idle"
                .to_string(),
        );
        for (inner, remedy) in [
            (
                prune_conflict,
                "run `libra cloud sync` to publish the prune tombstone before restoring",
            ),
            (d1_failure, "verify cloud connectivity and credentials"),
            (local_failure, "retry `libra cloud restore`"),
            (unstable, "retry when cloud sync is idle"),
        ] {
            let inner_detail = inner.to_string();
            let cli = agent_capture_restore_failure(inner).into_cli_error("restore");
            assert_eq!(
                cli.stable_code(),
                StableErrorCode::NetworkProtocol,
                "agent-capture restore failures keep the shipped LBR-NET-002 contract"
            );
            assert_eq!(
                cli.message(),
                format!("agent capture restore failed: {inner_detail}")
            );
            assert!(
                cli.message().contains(remedy),
                "the specific remedy must survive: {}",
                cli.message()
            );
            assert!(!cli.message().contains(REMOTE_SENTINEL));
            assert!(!format!("{:?}", cli.details()).contains(REMOTE_SENTINEL));
        }
    }

    #[test]
    fn local_agent_capture_restore_failure_redacts_driver_text() {
        const LOCAL_SENTINEL: &str = "LOCAL_SQLITE_ROW=/private/provider/session-capture.jsonl";
        let failure = local_agent_capture_restore_failure(
            "inspect local agent session",
            std::io::Error::other(LOCAL_SENTINEL),
        );
        let detail = failure.to_string();
        assert_eq!(
            detail,
            "local agent-capture catalog inspect local agent session failed during cloud restore; run `libra agent doctor --repair` and retry `libra cloud restore`"
        );
        assert!(!detail.contains(LOCAL_SENTINEL));
    }

    #[test]
    fn remote_repository_id_boundary_accepts_only_canonical_uuid() {
        let canonical = "5eec7796-ced5-4a49-8e26-68b0326a8c70";
        assert_eq!(
            validate_remote_repository_id(canonical.to_string()).expect("canonical UUID"),
            canonical
        );
        assert!(validate_remote_repository_id(canonical.to_uppercase()).is_err());
        assert!(validate_remote_repository_id(format!(" {canonical}")).is_err());
    }
}
