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

/// Ensure the local repository's `core.objectformat` and process `HashKind`
/// match the authoritative object format resolved from the cloud backup.
///
/// A freshly initialized empty repository (for example after `libra init`
/// followed by `libra cloud restore --name <sha256-repo>`) adopts the backup's
/// object format automatically. A non-empty local repository with a different
/// format fails closed before any local rows, objects, or refs are written.
async fn reconcile_local_object_format(
    db_conn: &sea_orm::DatabaseConnection,
    objects_dir: &std::path::Path,
    index_path: &std::path::Path,
    target_kind: git_internal::hash::HashKind,
) -> CloudResult<()> {
    reconcile_local_object_format_with(
        db_conn,
        objects_dir,
        index_path,
        git_internal::hash::get_hash_kind(),
        target_kind,
        true,
    )
    .await
}

async fn reconcile_local_object_format_with(
    db_conn: &sea_orm::DatabaseConnection,
    objects_dir: &std::path::Path,
    index_path: &std::path::Path,
    fallback_kind: git_internal::hash::HashKind,
    target_kind: git_internal::hash::HashKind,
    apply_process_kind: bool,
) -> CloudResult<()> {
    let local_kind = match ConfigKv::get_with_conn(db_conn, "core.objectformat").await {
        Ok(Some(entry)) => crate::internal::object_format::parse_config_value(&entry.value)
            .map_err(|_| {
                CloudError::Generic(format!(
                    "local repository has an unsupported core.objectformat '{}'",
                    entry.value
                ))
            })?,
        Ok(None) => fallback_kind,
        Err(error) => {
            return Err(CloudError::Generic(format!(
                "failed to read local core.objectformat before cloud restore: {error}"
            )));
        }
    };

    if local_kind == target_kind {
        if apply_process_kind {
            git_internal::hash::set_hash_kind(target_kind);
        }
        return Ok(());
    }

    let local_fmt = crate::internal::object_format::as_str(local_kind);
    let target_fmt = crate::internal::object_format::as_str(target_kind);

    if !local_repository_is_empty_for_format_switch(db_conn, objects_dir, index_path).await? {
        return Err(CloudError::ObjectFormatMismatch {
            local: local_fmt.to_string(),
            remote: target_fmt.to_string(),
            notes: Vec::new(),
        });
    }

    ConfigKv::set_with_conn(db_conn, "core.objectformat", target_fmt, false)
        .await
        .map_err(|error| {
            CloudError::Generic(format!(
                "failed to update local core.objectformat to '{target_fmt}': {error}"
            ))
        })?;

    if index_path.exists() {
        std::fs::remove_file(index_path).map_err(|error| {
            CloudError::Generic(format!(
                "failed to clear empty pre-restore index file before switching object format: {error}"
            ))
        })?;
    }

    if apply_process_kind {
        git_internal::hash::set_hash_kind(target_kind);
    }
    Ok(())
}

async fn local_repository_is_empty_for_format_switch(
    db_conn: &sea_orm::DatabaseConnection,
    objects_dir: &std::path::Path,
    index_path: &std::path::Path,
) -> CloudResult<bool> {
    use sea_orm::Statement;

    let backend = db_conn.get_database_backend();
    let has_object_index_table = crate::internal::db::connection_has_table(db_conn, "object_index")
        .await
        .map_err(|error| {
            CloudError::Generic(format!(
                "failed to probe local object_index table before switching object format: {error}"
            ))
        })?;
    if has_object_index_table {
        let has_object_index = db_conn
            .query_one_raw(Statement::from_string(
                backend,
                "SELECT 1 FROM object_index LIMIT 1".to_string(),
            ))
            .await
            .map_err(|error| {
                CloudError::Generic(format!(
                    "failed to inspect local object_index before switching object format: {error}"
                ))
            })?
            .is_some();
        if has_object_index {
            return Ok(false);
        }
    }

    let has_reference_table = crate::internal::db::connection_has_table(db_conn, "reference")
        .await
        .map_err(|error| {
            CloudError::Generic(format!(
                "failed to probe local reference table before switching object format: {error}"
            ))
        })?;
    if has_reference_table {
        let has_committed_ref = db_conn
            .query_one_raw(Statement::from_string(
                backend,
                "SELECT 1 FROM reference WHERE \"commit\" IS NOT NULL AND TRIM(\"commit\") != '' LIMIT 1"
                    .to_string(),
            ))
            .await
            .map_err(|error| {
                CloudError::Generic(format!(
                    "failed to inspect local references before switching object format: {error}"
                ))
            })?
            .is_some();
        if has_committed_ref {
            return Ok(false);
        }
    }

    if objects_dir.exists() {
        let entries = std::fs::read_dir(objects_dir).map_err(|error| {
            CloudError::Generic(format!(
                "failed to inspect local objects directory before switching object format: {error}"
            ))
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                CloudError::Generic(format!(
                    "failed to read local objects directory entry before switching object format: {error}"
                ))
            })?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
                let mut shard = std::fs::read_dir(&path).map_err(|error| {
                    CloudError::Generic(format!(
                        "failed to inspect local object shard '{name}' before switching object format: {error}"
                    ))
                })?;
                if shard
                    .next()
                    .transpose()
                    .map_err(|error| {
                        CloudError::Generic(format!(
                            "failed to read local object shard '{name}' before switching object format: {error}"
                        ))
                    })?
                    .is_some()
                {
                    return Ok(false);
                }
            } else if name == "pack" {
                let pack_entries = std::fs::read_dir(&path).map_err(|error| {
                    CloudError::Generic(format!(
                        "failed to inspect local pack directory before switching object format: {error}"
                    ))
                })?;
                for pack_entry in pack_entries {
                    let pack_entry = pack_entry.map_err(|error| {
                        CloudError::Generic(format!(
                            "failed to read local pack directory entry before switching object format: {error}"
                        ))
                    })?;
                    let ext = pack_entry
                        .path()
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(str::to_string);
                    if matches!(ext.as_deref(), Some("pack" | "idx")) {
                        return Ok(false);
                    }
                }
            }
        }
    }

    if index_path.exists() {
        match git_internal::internal::index::Index::load(index_path) {
            Ok(index) if index.tracked_files().is_empty() => {}
            // Fail closed on non-empty or unreadable index files so we never
            // discard local staged state when switching object format.
            Ok(_) | Err(_) => return Ok(false),
        }
    }

    Ok(true)
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

    run_cloud_restore_with_clients(args, d1_client, None, false).await
}

pub(crate) async fn execute_restore(args: RestoreArgs) -> CloudResult<()> {
    validate_cloud_backup_env(args.metadata_only).await?;

    let d1_client = D1Client::from_env()
        .await
        .map_err(|error| cloud_d1_failure("initialize cloud client", &error))?;

    let _ = run_cloud_restore_with_clients(args, d1_client, None, true).await?;
    Ok(())
}

pub(crate) async fn run_cloud_restore_with_clients(
    args: RestoreArgs,
    d1_client: D1Client,
    r2_override: Option<RemoteStorage>,
    render_human: bool,
) -> CloudResult<CloudRestoreOutput> {
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

    if render_human {
        println!("Starting restore for repo: {}", repo_id);
    }

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

    if render_human {
        println!("Found {} objects in cloud for repo.", indexes.len());
        if indexes.is_empty() {
            println!("No objects found for this repo.");
        }
    }

    let storage = util::try_get_storage_path(None).map_err(|error| {
        CloudError::Generic(format!(
            "locate repository storage for cloud restore: {error}"
        ))
    })?;
    match crate::storage::runtime::detect_repo_backend(&storage).map_err(|error| {
        CloudError::StorageIo(format!(
            "detect repository storage backend for cloud restore: {error}"
        ))
    })? {
        crate::storage::backend::StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                restore_into_redb_repository(
                    &storage,
                    &args,
                    &d1_client,
                    r2_override,
                    repo_id,
                    repository_kind,
                    resolved_object_format,
                    indexes,
                    render_human,
                )
                .await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                let _ = r2_override;
                Err(CloudError::Generic(
                    "this Libra binary was compiled without `storage-redb`".to_string(),
                ))
            }
        }
        crate::storage::backend::StorageBackendKind::Sqlite => {
            let db_conn = db::get_db_conn_instance().await;
            if !args.metadata_only {
                preflight_agent_capture_prune_fences(&db_conn, &d1_client, &repo_id).await?;
            }
            reconcile_local_object_format(
                &db_conn,
                &path::objects(),
                &path::index(),
                repository_kind,
            )
            .await?;
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

            if render_human {
                println!(
                    "Restored {} object indexes to local database.",
                    indexes.len()
                );
            }

            let _ = ConfigKv::set("libra.repoid", &repo_id, false).await;

            if args.metadata_only {
                if render_human {
                    println!("Metadata-only restore complete.");
                }
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

            let r2_storage = match r2_override {
                Some(r2) => r2,
                None => create_r2_storage(&repo_id).await?,
            };
            let objects_path = path::objects();
            let local_storage = LocalStorage::try_new(objects_path)?;

            let object_report = restore_indexed_objects_from_remote(
                &indexes,
                &r2_storage,
                &local_storage,
                repository_kind,
            )
            .await?;
            for warning in &object_report.warnings {
                eprintln!("{warning}");
            }
            if render_human {
                println!(
                    "Restore complete: {} downloaded, {} skipped (already exist), {} failed",
                    object_report.downloaded, object_report.skipped, object_report.failed
                );
            }
            if object_report.failed > 0 {
                return Err(CloudError::PartialTransfer(format!(
                    "{} objects failed to restore",
                    object_report.failed
                )));
            }

            let (metadata, deferred_capture_refs) =
                match restore_metadata_with_output(&db_conn, &r2_storage, render_human).await {
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

            let head_commit = Head::current_commit_result().await.map_err(|error| {
                CloudError::Generic(format!("failed to resolve HEAD commit: {error}"))
            })?;
            if let Some(commit) = head_commit {
                if render_human {
                    println!("Restoring working directory to HEAD ({})", commit);
                }
                let _ = restore_worktree_to_head(render_human).await;
            } else {
                if render_human {
                    println!("Restoring working directory (fallback)...");
                }
                let main_branch =
                    Branch::find_branch_result("main", None)
                        .await
                        .map_err(|error| {
                            CloudError::Generic(format!("failed to resolve main branch: {error}"))
                        })?;
                if let Some(branch) = main_branch {
                    if render_human {
                        println!("Found main branch: {}", branch.commit);
                    }
                    Head::update(Head::Branch("main".to_string()), None).await;
                    let _ = restore_worktree_to_head(render_human).await;
                } else if render_human {
                    println!("No HEAD commit or main branch found. Skipping worktree restore.");
                }
            }

            let capture_outcome =
                restore_agent_capture_from_d1(&db_conn, &d1_client, &repo_id, render_human)
                    .await
                    .map_err(agent_capture_restore_failure)?;
            restore_legacy_capture_refs_if_unowned(
                &db_conn,
                deferred_capture_refs,
                capture_outcome,
            )
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
    }
}

#[cfg(feature = "storage-redb")]
#[allow(clippy::too_many_arguments)]
async fn restore_into_redb_repository(
    storage: &std::path::Path,
    args: &RestoreArgs,
    d1_client: &D1Client,
    r2_override: Option<RemoteStorage>,
    repo_id: String,
    repository_kind: git_internal::hash::HashKind,
    resolved_object_format: String,
    indexes: Vec<ObjectIndexRow>,
    render_human: bool,
) -> CloudResult<CloudRestoreOutput> {
    use crate::storage::git_store::ObjectIndexRecord;

    if !args.metadata_only {
        preflight_agent_capture_prune_fences_redb(storage, d1_client, &repo_id).await?;
    }
    reconcile_local_object_format_redb(storage, &path::objects(), &path::index(), repository_kind)
        .await?;

    // Upsert restored object indexes and mark them synced in `git.redb`.
    {
        let git_store = crate::storage::runtime::open_git_store_read_write(storage)
            .await
            .map_err(|error| {
                CloudError::StorageIo(format!(
                    "open git redb store while restoring object indexes: {error}"
                ))
            })?;
        let records: Vec<ObjectIndexRecord> = indexes
            .iter()
            .map(|idx| ObjectIndexRecord {
                repo_id: idx.repo_id.clone(),
                o_id: idx.o_id.clone(),
                o_type: idx.o_type.clone(),
                o_size: idx.o_size,
                created_at: idx.created_at,
                is_synced: true,
            })
            .collect();
        if !records.is_empty() {
            git_store
                .upsert_object_index_batch(&records)
                .await
                .map_err(|error| {
                    CloudError::StorageIo(format!(
                        "upsert restored object indexes into git redb store: {error}"
                    ))
                })?;
            let mut by_repo: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for rec in &records {
                by_repo
                    .entry(rec.repo_id.clone())
                    .or_default()
                    .push(rec.o_id.clone());
            }
            for (index_repo_id, oids) in &by_repo {
                git_store
                    .mark_object_index_synced(index_repo_id, oids)
                    .await
                    .map_err(|error| {
                        CloudError::StorageIo(format!(
                            "mark restored object indexes synced in git redb store: {error}"
                        ))
                    })?;
            }
        }
    }

    if render_human {
        println!(
            "Restored {} object indexes to local database.",
            indexes.len()
        );
    }

    let _ = ConfigKv::set("libra.repoid", &repo_id, false).await;

    if args.metadata_only {
        ensure_redb_storage_manifest(storage)?;
        if render_human {
            println!("Metadata-only restore complete.");
        }
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

    let r2_storage = match r2_override {
        Some(r2) => r2,
        None => create_r2_storage(&repo_id).await?,
    };
    let objects_path = path::objects();
    let local_storage = LocalStorage::try_new(objects_path)?;

    let object_report =
        restore_indexed_objects_from_remote(&indexes, &r2_storage, &local_storage, repository_kind)
            .await?;
    for warning in &object_report.warnings {
        eprintln!("{warning}");
    }
    if render_human {
        println!(
            "Restore complete: {} downloaded, {} skipped (already exist), {} failed",
            object_report.downloaded, object_report.skipped, object_report.failed
        );
    }
    if object_report.failed > 0 {
        return Err(CloudError::PartialTransfer(format!(
            "{} objects failed to restore",
            object_report.failed
        )));
    }

    let (metadata, deferred_capture_refs) =
        match restore_metadata_redb(storage, &r2_storage, render_human).await {
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
    if let Some(commit) = head_commit {
        if render_human {
            println!("Restoring working directory to HEAD ({})", commit);
        }
        let _ = restore_worktree_to_head(render_human).await;
    } else {
        if render_human {
            println!("Restoring working directory (fallback)...");
        }
        let main_branch = Branch::find_branch_result("main", None)
            .await
            .map_err(|error| {
                CloudError::Generic(format!("failed to resolve main branch: {error}"))
            })?;
        if let Some(branch) = main_branch {
            if render_human {
                println!("Found main branch: {}", branch.commit);
            }
            Head::update(Head::Branch("main".to_string()), None).await;
            let _ = restore_worktree_to_head(render_human).await;
        } else if render_human {
            println!("No HEAD commit or main branch found. Skipping worktree restore.");
        }
    }

    let (capture_outcome, restored_generation) =
        restore_agent_capture_from_d1_redb(storage, d1_client, &repo_id, render_human)
            .await
            .map_err(agent_capture_restore_failure)?;
    if capture_outcome == AgentCaptureRestoreOutcome::NoGeneration
        && !deferred_capture_refs.is_empty()
    {
        install_reference_models_redb(storage, deferred_capture_refs, false).await?;
    }

    // RB-28b AC2 & AC3: publish `.libra/storage-format.json` and align
    // `META_GENERATION` across both `git.redb` and `capture.redb`.
    sync_redb_meta_generation_and_manifest(storage, restored_generation).await?;

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

#[cfg(feature = "storage-redb")]
async fn reconcile_local_object_format_redb(
    storage: &std::path::Path,
    objects_dir: &std::path::Path,
    index_path: &std::path::Path,
    target_kind: git_internal::hash::HashKind,
) -> CloudResult<()> {
    let fallback_kind = git_internal::hash::get_hash_kind();
    let local_kind = match ConfigKv::get("core.objectformat").await {
        Ok(Some(entry)) => crate::internal::object_format::parse_config_value(&entry.value)
            .map_err(|_| {
                CloudError::Generic(format!(
                    "local repository has an unsupported core.objectformat '{}'",
                    entry.value
                ))
            })?,
        Ok(None) => fallback_kind,
        Err(error) => {
            return Err(CloudError::Generic(format!(
                "failed to read local core.objectformat before cloud restore: {error}"
            )));
        }
    };

    if local_kind == target_kind {
        git_internal::hash::set_hash_kind(target_kind);
        return Ok(());
    }

    let local_fmt = crate::internal::object_format::as_str(local_kind);
    let target_fmt = crate::internal::object_format::as_str(target_kind);

    if !local_redb_repository_is_empty_for_format_switch(storage, objects_dir, index_path).await? {
        return Err(CloudError::ObjectFormatMismatch {
            local: local_fmt.to_string(),
            remote: target_fmt.to_string(),
            notes: Vec::new(),
        });
    }

    ConfigKv::set("core.objectformat", target_fmt, false)
        .await
        .map_err(|error| {
            CloudError::Generic(format!(
                "failed to update local core.objectformat to '{target_fmt}': {error}"
            ))
        })?;

    if index_path.exists() {
        std::fs::remove_file(index_path).map_err(|error| {
            CloudError::Generic(format!(
                "failed to clear empty pre-restore index file before switching object format: {error}"
            ))
        })?;
    }

    git_internal::hash::set_hash_kind(target_kind);
    Ok(())
}

#[cfg(feature = "storage-redb")]
async fn local_redb_repository_is_empty_for_format_switch(
    storage: &std::path::Path,
    objects_dir: &std::path::Path,
    index_path: &std::path::Path,
) -> CloudResult<bool> {
    use redb::ReadableTable;

    use crate::storage::{
        git_store::{GitStore, RedbGitStore, RefTarget, redb::OBJECT_INDEX_TABLE},
        runtime::resolve_repo_git_redb_path,
    };

    let git_path = resolve_repo_git_redb_path(storage).map_err(|error| {
        CloudError::StorageIo(format!(
            "resolve git redb path before switching object format: {error}"
        ))
    })?;
    let git_ro = RedbGitStore::open_read_only(&git_path).map_err(|error| {
        CloudError::StorageIo(format!(
            "open git redb store before switching object format: {error}"
        ))
    })?;

    let has_object_index = {
        let git_ro_clone = git_ro.clone();
        tokio::task::spawn_blocking(move || -> CloudResult<bool> {
            let rtx = git_ro_clone.begin_checked_read_tx().map_err(|error| {
                CloudError::StorageIo(format!(
                    "begin read tx to inspect local object_index before switching object format: {error}"
                ))
            })?;
            match rtx.open_table(OBJECT_INDEX_TABLE) {
                Ok(table) => Ok(table
                    .iter()
                    .map_err(|error| {
                        CloudError::StorageIo(format!(
                            "scan local object_index before switching object format: {error}"
                        ))
                    })?
                    .next()
                    .is_some()),
                Err(redb::TableError::TableDoesNotExist(_)) => Ok(false),
                Err(error) => Err(CloudError::StorageIo(format!(
                    "open local object_index before switching object format: {error}"
                ))),
            }
        })
        .await
        .map_err(|error| {
            CloudError::StorageIo(format!(
                "join object_index emptiness check before switching object format: {error}"
            ))
        })??
    };
    if has_object_index {
        return Ok(false);
    }

    let refs = git_ro.list_refs(None).await.map_err(|error| {
        CloudError::StorageIo(format!(
            "inspect local references before switching object format: {error}"
        ))
    })?;
    if refs
        .iter()
        .any(|rec| matches!(&rec.target, RefTarget::Direct(oid) if !oid.trim().is_empty()))
    {
        return Ok(false);
    }

    if objects_dir.exists() {
        let entries = std::fs::read_dir(objects_dir).map_err(|error| {
            CloudError::Generic(format!(
                "failed to inspect local objects directory before switching object format: {error}"
            ))
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                CloudError::Generic(format!(
                    "failed to read local objects directory entry before switching object format: {error}"
                ))
            })?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
                let mut shard = std::fs::read_dir(&path).map_err(|error| {
                    CloudError::Generic(format!(
                        "failed to inspect local object shard '{name}' before switching object format: {error}"
                    ))
                })?;
                if shard
                    .next()
                    .transpose()
                    .map_err(|error| {
                        CloudError::Generic(format!(
                            "failed to read local object shard '{name}' before switching object format: {error}"
                        ))
                    })?
                    .is_some()
                {
                    return Ok(false);
                }
            } else if name == "pack" {
                let pack_entries = std::fs::read_dir(&path).map_err(|error| {
                    CloudError::Generic(format!(
                        "failed to inspect local pack directory before switching object format: {error}"
                    ))
                })?;
                for pack_entry in pack_entries {
                    let pack_entry = pack_entry.map_err(|error| {
                        CloudError::Generic(format!(
                            "failed to read local pack directory entry before switching object format: {error}"
                        ))
                    })?;
                    let ext = pack_entry
                        .path()
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(str::to_string);
                    if matches!(ext.as_deref(), Some("pack" | "idx")) {
                        return Ok(false);
                    }
                }
            }
        }
    }

    if index_path.exists() {
        match git_internal::internal::index::Index::load(index_path) {
            Ok(index) if index.tracked_files().is_empty() => {}
            Ok(_) | Err(_) => return Ok(false),
        }
    }

    Ok(true)
}

#[cfg(feature = "storage-redb")]
fn ref_model_to_redb_key_target(
    ref_model: &reference::Model,
) -> Option<(
    crate::storage::git_store::RefKey,
    crate::storage::git_store::RefTarget,
)> {
    use crate::storage::git_store::{RefKey, RefTarget};

    match ref_model.kind {
        reference::ConfigKind::Head => {
            let key = match &ref_model.remote {
                Some(remote) if !remote.is_empty() => RefKey::RemoteHead {
                    remote: remote.clone(),
                },
                _ => RefKey::LocalHead {
                    worktree_id: ref_model.worktree_id.clone(),
                },
            };
            let target = match (&ref_model.name, &ref_model.commit) {
                (Some(branch), _) if !branch.is_empty() => RefTarget::Symbolic(branch.clone()),
                (_, Some(commit)) if !commit.is_empty() => RefTarget::Direct(commit.clone()),
                _ => return None,
            };
            Some((key, target))
        }
        reference::ConfigKind::Branch => {
            let name = ref_model.name.as_deref().filter(|s| !s.is_empty())?;
            let commit = ref_model.commit.as_deref().filter(|s| !s.is_empty())?;
            let key = match &ref_model.remote {
                Some(remote) if !remote.is_empty() => RefKey::RemoteBranch {
                    remote: remote.clone(),
                    name: name.to_string(),
                },
                _ => RefKey::LocalBranch {
                    name: name.to_string(),
                },
            };
            Some((key, RefTarget::Direct(commit.to_string())))
        }
        reference::ConfigKind::Tag => {
            let name = ref_model.name.as_deref().filter(|s| !s.is_empty())?;
            let commit = ref_model.commit.as_deref().filter(|s| !s.is_empty())?;
            Some((
                RefKey::Tag {
                    name: name.to_string(),
                },
                RefTarget::Direct(commit.to_string()),
            ))
        }
    }
}

#[cfg(feature = "storage-redb")]
async fn install_reference_models_redb(
    storage: &std::path::Path,
    references: Vec<reference::Model>,
    defer_capture_refs: bool,
) -> CloudResult<Vec<reference::Model>> {
    let git_store = crate::storage::runtime::open_git_store_read_write(storage)
        .await
        .map_err(|error| {
            CloudError::StorageIo(format!(
                "open git redb store while restoring references: {error}"
            ))
        })?;
    let mut deferred = Vec::new();
    for ref_model in references {
        if defer_capture_refs
            && ref_model.kind == reference::ConfigKind::Branch
            && ref_model.remote.is_none()
            && ref_model.name.as_deref().is_some_and(|name| {
                name == crate::internal::branch::TRACES_BRANCH
                    || name == crate::internal::branch::LEGACY_TRACES_BRANCH
            })
        {
            deferred.push(ref_model);
            continue;
        }
        let Some((key, target)) = ref_model_to_redb_key_target(&ref_model) else {
            continue;
        };
        let current = git_store.get_ref(&key).await.map_err(|error| {
            CloudError::StorageIo(format!(
                "read existing reference {:?} during restore: {error}",
                ref_model.name
            ))
        })?;
        if let Err(error) = git_store
            .update_ref_cas(&key, current.as_ref(), Some(&target), &[])
            .await
        {
            eprintln!(
                "warning: failed to restore reference {:?}: {error}",
                ref_model.name
            );
        }
    }
    Ok(deferred)
}

#[cfg(feature = "storage-redb")]
async fn restore_metadata_redb(
    storage: &std::path::Path,
    r2_storage: &RemoteStorage,
    render_human: bool,
) -> CloudResult<Vec<reference::Model>> {
    if render_human {
        println!("Restoring metadata...");
    }
    let data = match r2_storage.get_metadata().await {
        Ok(data) => data,
        Err(e) => {
            if render_human {
                println!("warning: failed to download metadata: {}", e);
                return Ok(Vec::new());
            }
            return Err(CloudError::R2(format!("failed to download metadata: {e}")));
        }
    };
    let references: Vec<reference::Model> = serde_json::from_slice(&data)
        .map_err(|e| CloudError::Generic(format!("Failed to deserialize metadata: {}", e)))?;
    let deferred = install_reference_models_redb(storage, references, true).await?;
    if render_human {
        println!("Metadata restored.");
    }
    Ok(deferred)
}

#[cfg(feature = "storage-redb")]
async fn restore_agent_capture_from_d1_redb(
    storage: &std::path::Path,
    d1_client: &D1Client,
    repo_id: &str,
    render_human: bool,
) -> CloudResult<(AgentCaptureRestoreOutcome, Option<u64>)> {
    tokio::time::timeout(
        AGENT_CAPTURE_CLOUD_DEADLINE,
        restore_agent_capture_from_d1_inner_redb(storage, d1_client, repo_id, render_human),
    )
    .await
    .map_err(|_| {
        CloudError::PartialTransfer(
            "agent capture cloud restore exceeded its 120-second deadline; retry the operation"
                .to_string(),
        )
    })?
}

#[cfg(feature = "storage-redb")]
async fn restore_agent_capture_from_d1_inner_redb(
    storage: &std::path::Path,
    d1_client: &D1Client,
    repo_id: &str,
    render_human: bool,
) -> CloudResult<(AgentCaptureRestoreOutcome, Option<u64>)> {
    if render_human {
        println!("Restoring agent capture catalog from D1...");
    }

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
        return Ok((AgentCaptureRestoreOutcome::NoGeneration, None));
    }
    let subagent_content_present = d1_client
        .agent_subagent_content_tables_exist()
        .await
        .map_err(|error| cloud_d1_failure("probe subagent-content schema", &error))?;

    let local_cloud_base = {
        let cap_ro = crate::storage::runtime::open_capture_store_read_only(storage)
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure("open local capture store", error)
            })?;
        cap_ro
            .get_cloud_base(repo_id)
            .await
            .map_err(|error| local_agent_capture_restore_failure("read local cloud base", error))?
            .map(|rec| rec.remote_generation)
    };

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
            return Ok((AgentCaptureRestoreOutcome::NoGeneration, None));
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
                storage,
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
        if object_indexes.iter().any(|index| index.is_synced != 1) {
            return Err(CloudError::PartialTransfer(
                "agent-capture manifest includes an object that is not marked synced".to_string(),
            ));
        }
        if durable_oids
            .iter()
            .any(|oid| !fenced_oids.contains(oid.as_str()))
        {
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

    restore_agent_capture_from_rows_with_subagents_redb(
        storage,
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
    persist_local_import_tombstones_redb(storage, &rows.import_tombstones).await?;
    store_local_agent_capture_cloud_base_redb(storage, repo_id, remote_generation).await?;

    let restored_gen_u64 = u64::try_from(remote_generation).ok();
    Ok((
        AgentCaptureRestoreOutcome::GenerationInstalled,
        restored_gen_u64,
    ))
}

#[cfg(feature = "storage-redb")]
async fn restore_agent_capture_from_rows_with_subagents_redb(
    storage: &std::path::Path,
    rows: AgentCaptureRestoreRows<'_>,
    render_human: bool,
) -> CloudResult<()> {
    use redb::ReadableTable;

    use crate::storage::{
        capture_store::{
            AgentCheckpointRecord, AgentSessionRecord, AgentSubagentContentClaimRecord,
            AgentSubagentContentRevisionRecord, AgentSubagentLinkRecord, CaptureAgentKind,
            CaptureCheckpointScope, CaptureScopeState, CaptureSessionState, CaptureStore,
            CaptureSubagentClaimState, CaptureSubagentLinkState, CaptureSubagentSourceChannel,
            RedbCaptureStore,
            redb_capture_store::{AGENT_CHECKPOINT_TABLE, AGENT_SUBAGENT_CONTENT_REVISION_TABLE},
        },
        git_store::{RefKey, RefTarget},
        opener::RedbRepairContext,
        runtime::resolve_repo_capture_redb_path,
    };

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

    if !checkpoint_rows.is_empty() {
        let local_pruned =
            read_redb_prune_tombstone_checkpoint_ids(storage, AGENT_CAPTURE_MAX_ROWS_PER_TABLE + 1)
                .await?;
        if local_pruned.len() > AGENT_CAPTURE_MAX_ROWS_PER_TABLE {
            return Err(CloudError::Generic(format!(
                "local checkpoint prune tombstones exceed the {}-row restore safety bound; \
                 run `libra cloud sync` before restoring",
                AGENT_CAPTURE_MAX_ROWS_PER_TABLE
            )));
        }
        let remote_checkpoint_ids: HashSet<&str> = checkpoint_rows
            .iter()
            .map(|row| row.checkpoint_id.as_str())
            .collect();
        for pruned_id in &local_pruned {
            if remote_checkpoint_ids.contains(pruned_id.as_str()) {
                return Err(CloudError::PartialTransfer(
                    "a remote checkpoint was already pruned locally; run `libra cloud sync` to publish the prune tombstone before restoring"
                        .to_string(),
                ));
            }
        }
    }

    let git_store = crate::storage::runtime::open_git_store_read_write(storage)
        .await
        .map_err(|error| {
            local_agent_capture_restore_failure("open git redb store for traces ref", error)
        })?;
    let cap_path = resolve_repo_capture_redb_path(storage)
        .map_err(|error| local_agent_capture_restore_failure("resolve capture redb path", error))?;
    let cap_store = RedbCaptureStore::open_read_write(&cap_path, RedbRepairContext::WriteCommand)
        .await
        .map_err(|error| local_agent_capture_restore_failure("open capture redb store", error))?;

    let traces_key = RefKey::LocalBranch {
        name: crate::internal::branch::TRACES_BRANCH.to_string(),
    };
    let existing_traces_ref = git_store
        .get_ref(&traces_key)
        .await
        .map_err(|error| local_agent_capture_restore_failure("inspect local traces ref", error))?;
    let existing_head = match &existing_traces_ref {
        Some(RefTarget::Direct(oid)) => Some(oid.as_str()),
        _ => None,
    };

    if existing_head != traces_head {
        let cap_clone = cap_store.clone();
        let has_local_checkpoints = tokio::task::spawn_blocking(move || -> CloudResult<bool> {
            let rtx = cap_clone.begin_checked_read_tx().map_err(|error| {
                local_agent_capture_restore_failure("count local checkpoints before restore", error)
            })?;
            match rtx.open_table(AGENT_CHECKPOINT_TABLE) {
                Ok(table) => Ok(table
                    .iter()
                    .map_err(|error| {
                        local_agent_capture_restore_failure(
                            "count local checkpoints before restore",
                            error,
                        )
                    })?
                    .next()
                    .is_some()),
                Err(redb::TableError::TableDoesNotExist(_)) => Ok(false),
                Err(error) => Err(local_agent_capture_restore_failure(
                    "count local checkpoints before restore",
                    error,
                )),
            }
        })
        .await
        .map_err(|error| {
            local_agent_capture_restore_failure("join local checkpoint count", error)
        })??;
        if has_local_checkpoints {
            return Err(CloudError::Generic(
                "the fenced cloud traces head conflicts with existing local checkpoint history; restore into an empty repository or sync the newer local history first"
                    .to_string(),
            ));
        }
    }

    // PD-03 tombstone-first, LOCAL side.
    let local_tombstones = cap_store.list_import_tombstones().await.map_err(|error| {
        local_agent_capture_restore_failure("read local erasure tombstones", error)
    })?;
    let mut locally_erased_sessions: HashSet<(String, String)> = HashSet::new();
    let mut locally_erased_session_ids: HashSet<String> = HashSet::new();
    for tomb in local_tombstones {
        locally_erased_sessions.insert((tomb.agent_kind, tomb.provider_session_id));
        locally_erased_session_ids.insert(tomb.erased_session_id);
    }
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
    let checkpoint_rows: Vec<_> = checkpoint_rows
        .iter()
        .filter(|row| {
            !locally_erased_session_ids.contains(&row.session_id)
                && !dropped_session_ids.contains(&row.session_id)
        })
        .cloned()
        .collect();

    // Load existing sessions indexed by `(agent_kind, provider_session_id)` for conflict checks.
    let existing_sessions = cap_store
        .list_sessions_by_started_desc()
        .await
        .map_err(|error| {
            local_agent_capture_restore_failure("inspect local agent sessions", error)
        })?;
    let existing_sessions_by_provider: HashMap<(String, String), AgentSessionRecord> =
        existing_sessions
            .into_iter()
            .map(|rec| {
                (
                    (
                        rec.agent_kind.as_str().to_string(),
                        rec.provider_session_id.clone(),
                    ),
                    rec,
                )
            })
            .collect();

    let mut newer_local_sessions = HashSet::new();
    let mut skipped_scoped_sessions = 0usize;
    for row in &session_rows {
        let key = (row.agent_kind.clone(), row.provider_session_id.clone());
        if let Some(existing) = existing_sessions_by_provider.get(&key) {
            let local = AgentSessionV2Row {
                session_id: existing.session_id.clone(),
                agent_kind: existing.agent_kind.as_str().to_string(),
                provider_session_id: existing.provider_session_id.clone(),
                state: existing.state.as_str().to_string(),
                working_dir: existing.working_dir.clone(),
                worktree_id: existing.worktree_id.clone(),
                parent_commit: existing.parent_commit.clone(),
                parent_session_id: existing.parent_session_id.clone(),
                metadata_json: existing.metadata_json.clone(),
                redaction_report: existing.redaction_report.clone(),
                started_at: existing.started_at,
                last_event_at: existing.last_event_at,
                stopped_at: existing.stopped_at,
                schema_version: existing.schema_version,
                sync_revision: existing.sync_revision,
            };
            if local.sync_revision > row.sync_revision {
                if !remote_is_known_ancestor {
                    return Err(CloudError::Generic(
                        "a local agent session has a larger divergent sync revision without a recorded cloud ancestor; sync or restore from the clone that owns the current cloud lineage"
                            .to_string(),
                    ));
                }
                newer_local_sessions.insert(key);
            } else if local.sync_revision == row.sync_revision
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

    for row in &checkpoint_rows {
        if let Some(existing) =
            cap_store
                .get_checkpoint(&row.checkpoint_id)
                .await
                .map_err(|error| {
                    local_agent_capture_restore_failure("inspect local checkpoint", error)
                })?
        {
            let local = AgentCheckpointV2Row {
                checkpoint_id: existing.checkpoint_id,
                session_id: existing.session_id,
                parent_checkpoint_id: existing.parent_checkpoint_id,
                scope: existing.scope.as_str().to_string(),
                parent_commit: existing.parent_commit,
                tree_oid: existing.tree_oid,
                metadata_blob_oid: existing.metadata_blob_oid,
                traces_commit: existing.traces_commit,
                tool_use_id: existing.tool_use_id,
                subagent_session_id: existing.subagent_session_id,
                description: existing.description,
                created_at: existing.created_at,
                sync_revision: existing.sync_revision,
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

    // Validate subagent revisions against existing immutable local history.
    if !revision_rows.is_empty() {
        let cap_clone = cap_store.clone();
        let incoming_revs: Vec<AgentSubagentContentRevisionRow> = revision_rows.to_vec();
        tokio::task::spawn_blocking(move || -> CloudResult<()> {
            let rtx = cap_clone.begin_checked_read_tx().map_err(|error| {
                local_agent_capture_restore_failure("inspect local subagent revisions", error)
            })?;
            let tbl = match rtx.open_table(AGENT_SUBAGENT_CONTENT_REVISION_TABLE) {
                Ok(t) => t,
                Err(redb::TableError::TableDoesNotExist(_)) => return Ok(()),
                Err(error) => {
                    return Err(local_agent_capture_restore_failure(
                        "inspect local subagent revisions",
                        error,
                    ));
                }
            };
            for row in &incoming_revs {
                if let Some(guard) = tbl
                    .get((
                        row.parent_session_id.as_str(),
                        row.provider_kind.as_str(),
                        row.source_key.as_str(),
                        row.content_schema_version,
                        row.revision,
                    ))
                    .map_err(|error| {
                        local_agent_capture_restore_failure("inspect local subagent revision", error)
                    })?
                {
                    let existing: AgentSubagentContentRevisionRecord =
                        crate::storage::codec::decode_versioned(
                            guard.value(),
                            crate::storage::capture_store::redb_capture_store::CAPTURE_CODEC_VERSION,
                        )
                        .map_err(|error| {
                            local_agent_capture_restore_failure("decode revision", error)
                        })?;
                    let exact = existing.checkpoint_id == row.checkpoint_id
                        && existing.content_digest == row.content_digest
                        && existing.source_channel.as_str() == row.source_channel
                        && i64::from(existing.partial) == row.partial
                        && existing.created_at == row.created_at;
                    if !exact {
                        return Err(CloudError::Generic(
                            "restored subagent revision conflicts with immutable local history"
                                .to_string(),
                        ));
                    }
                }
            }
            Ok(())
        })
        .await
        .map_err(|error| {
            local_agent_capture_restore_failure("join subagent revision validation", error)
        })??;
    }

    let mut newer_local_claims = HashSet::new();
    let mut existing_claims_map: HashMap<
        (String, String, String, i64),
        AgentSubagentContentClaimRecord,
    > = HashMap::new();
    for row in claim_rows {
        if let Some(existing) = cap_store
            .get_subagent_content_claim(
                &row.parent_session_id,
                &row.provider_kind,
                &row.source_key,
                row.content_schema_version,
            )
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure("inspect local subagent claim", error)
            })?
        {
            if existing.state != CaptureSubagentClaimState::Idle {
                return Err(CloudError::Generic(
                    "cannot restore a subagent claim while a local writer reservation is active"
                        .to_string(),
                ));
            }
            if existing.sync_revision > row.sync_revision {
                if !remote_is_known_ancestor {
                    return Err(CloudError::Generic(
                        "a local subagent claim has a larger divergent sync revision without a recorded cloud ancestor; restore the current cloud snapshot before retrying"
                            .to_string(),
                    ));
                }
                newer_local_claims.insert(claim_key(row));
            } else if existing.sync_revision == row.sync_revision {
                let exact = existing.revision_cursor == row.revision_cursor
                    && existing.current_revision == row.current_revision
                    && existing.current_checkpoint_id == row.current_checkpoint_id
                    && existing.current_digest == row.current_digest;
                if !exact {
                    return Err(CloudError::Generic(
                        "restored subagent claim conflicts with local state at the same sync generation"
                            .to_string(),
                    ));
                }
            }
            existing_claims_map.insert(claim_key(row), existing);
        }
    }

    let mut newer_local_links = HashSet::new();
    for row in link_rows {
        if let Some(existing) = cap_store
            .get_subagent_link(&row.content_checkpoint_id)
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure("inspect local subagent link", error)
            })?
        {
            if existing.sync_revision > row.sync_revision {
                if !remote_is_known_ancestor {
                    return Err(CloudError::Generic(
                        "a local subagent link has a larger divergent sync revision without a recorded cloud ancestor"
                            .to_string(),
                    ));
                }
                newer_local_links.insert(row.content_checkpoint_id.clone());
            } else if existing.sync_revision == row.sync_revision {
                let exact = existing.parent_session_id == row.parent_session_id
                    && existing.link_state.as_str() == row.link_state
                    && existing.boundary_checkpoint_id == row.boundary_checkpoint_id
                    && existing.stable_subagent_id == row.stable_subagent_id
                    && existing.created_at == row.created_at
                    && existing.updated_at == row.updated_at;
                if !exact {
                    return Err(CloudError::Generic(
                        "restored subagent link conflicts with local state at the same generation"
                            .to_string(),
                    ));
                }
            }
        }
    }

    // Update fenced `TRACES_BRANCH` ref in `git.redb` first (matching GC-RB-04 order).
    let new_traces_target = traces_head.map(|oid| RefTarget::Direct(oid.to_string()));
    git_store
        .update_ref_cas(
            &traces_key,
            existing_traces_ref.as_ref(),
            new_traces_target.as_ref(),
            &[],
        )
        .await
        .map_err(|error| local_agent_capture_restore_failure("restore fenced traces ref", error))?;

    // 1. Sessions.
    for row in &session_rows {
        let key = (row.agent_kind.clone(), row.provider_session_id.clone());
        if newer_local_sessions.contains(&key) {
            continue;
        }
        if let Some(existing) = existing_sessions_by_provider.get(&key) {
            if existing.sync_revision >= row.sync_revision {
                continue;
            }
            if existing.scope_state == CaptureScopeState::Scoped {
                skipped_scoped_sessions += 1;
                continue;
            }
        }
        let agent_kind = CaptureAgentKind::parse(&row.agent_kind)
            .map_err(|_| CloudError::Generic("failed to restore an agent session".to_string()))?;
        let state = CaptureSessionState::parse(&row.state)
            .map_err(|_| CloudError::Generic("failed to restore an agent session".to_string()))?;
        let rec = AgentSessionRecord {
            session_id: row.session_id.clone(),
            agent_kind,
            provider_session_id: row.provider_session_id.clone(),
            thread_id: None,
            state,
            working_dir: row.working_dir.clone(),
            worktree_id: row.worktree_id.clone(),
            parent_commit: row.parent_commit.clone(),
            parent_session_id: row.parent_session_id.clone(),
            metadata_json: row.metadata_json.clone(),
            redaction_report: row.redaction_report.clone(),
            started_at: row.started_at,
            last_event_at: row.last_event_at,
            stopped_at: row.stopped_at,
            schema_version: row.schema_version,
            sync_revision: row.sync_revision,
            repo_id: None,
            workspace_id: None,
            workspace_fence: None,
            scope_state: CaptureScopeState::LegacyUnknown,
        };
        cap_store
            .upsert_session(&rec)
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

    // 2. Checkpoints.
    for row in &checkpoint_rows {
        if let Some(existing) = cap_store
            .get_checkpoint(&row.checkpoint_id)
            .await
            .map_err(|_| CloudError::Generic("failed to restore an agent checkpoint".to_string()))?
            && existing.sync_revision >= row.sync_revision
        {
            continue;
        }
        let scope = CaptureCheckpointScope::parse(&row.scope).map_err(|_| {
            CloudError::Generic("failed to restore an agent checkpoint".to_string())
        })?;
        let rec = AgentCheckpointRecord {
            checkpoint_id: row.checkpoint_id.clone(),
            session_id: row.session_id.clone(),
            parent_checkpoint_id: row.parent_checkpoint_id.clone(),
            scope,
            parent_commit: row.parent_commit.clone(),
            tree_oid: row.tree_oid.clone(),
            metadata_blob_oid: row.metadata_blob_oid.clone(),
            traces_commit: row.traces_commit.clone(),
            tool_use_id: row.tool_use_id.clone(),
            subagent_session_id: row.subagent_session_id.clone(),
            description: row.description.clone(),
            created_at: row.created_at,
            sync_revision: row.sync_revision,
        };
        cap_store.upsert_checkpoint(&rec).await.map_err(|_| {
            CloudError::Generic("failed to restore an agent checkpoint".to_string())
        })?;
    }

    // 3. Skeleton claims to satisfy revision FK.
    for row in claim_rows {
        let key = claim_key(row);
        if existing_claims_map.contains_key(&key) {
            continue;
        }
        let skeleton = AgentSubagentContentClaimRecord {
            parent_session_id: row.parent_session_id.clone(),
            provider_kind: row.provider_kind.clone(),
            source_key: row.source_key.clone(),
            content_schema_version: row.content_schema_version,
            current_revision: 0,
            current_checkpoint_id: None,
            current_digest: None,
            state: CaptureSubagentClaimState::Idle,
            attempt_digest: None,
            attempt_checkpoint_id: None,
            owner: None,
            lease_expires_at: None,
            fence_token: row.fence_token,
            created_at: row.created_at,
            updated_at: row.updated_at,
            revision_cursor: 0,
            sync_revision: 0,
        };
        cap_store
            .upsert_subagent_content_claim(&skeleton)
            .await
            .map_err(|error| local_agent_capture_restore_failure("stage restored claim", error))?;
    }

    // 4. Subagent content revisions.
    for row in revision_rows {
        let source_channel =
            CaptureSubagentSourceChannel::parse(&row.source_channel).map_err(|_| {
                CloudError::Generic("failed to restore a subagent revision".to_string())
            })?;
        let rec = AgentSubagentContentRevisionRecord {
            parent_session_id: row.parent_session_id.clone(),
            provider_kind: row.provider_kind.clone(),
            source_key: row.source_key.clone(),
            content_schema_version: row.content_schema_version,
            revision: row.revision,
            checkpoint_id: row.checkpoint_id.clone(),
            content_digest: row.content_digest.clone(),
            source_channel,
            partial: row.partial != 0,
            created_at: row.created_at,
        };
        cap_store
            .put_subagent_content_revision(&rec)
            .await
            .map_err(|_| {
                CloudError::Generic("failed to restore a subagent revision".to_string())
            })?;
    }

    // 5. Subagent links.
    for row in link_rows {
        if newer_local_links.contains(&row.content_checkpoint_id) {
            continue;
        }
        if let Some(existing) = cap_store
            .get_subagent_link(&row.content_checkpoint_id)
            .await
            .map_err(|_| CloudError::Generic("failed to restore a subagent link".to_string()))?
            && existing.sync_revision >= row.sync_revision
        {
            continue;
        }
        let link_state = CaptureSubagentLinkState::parse(&row.link_state)
            .map_err(|_| CloudError::Generic("failed to restore a subagent link".to_string()))?;
        let rec = AgentSubagentLinkRecord {
            content_checkpoint_id: row.content_checkpoint_id.clone(),
            parent_session_id: row.parent_session_id.clone(),
            link_state,
            boundary_checkpoint_id: row.boundary_checkpoint_id.clone(),
            stable_subagent_id: row.stable_subagent_id.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
            sync_revision: row.sync_revision,
        };
        cap_store
            .upsert_subagent_link(&rec)
            .await
            .map_err(|_| CloudError::Generic("failed to restore a subagent link".to_string()))?;
    }

    // 6. Advance final claims.
    for row in claim_rows {
        let key = claim_key(row);
        if newer_local_claims.contains(&key) {
            continue;
        }
        let existing = existing_claims_map.get(&key);
        let rec = AgentSubagentContentClaimRecord {
            parent_session_id: row.parent_session_id.clone(),
            provider_kind: row.provider_kind.clone(),
            source_key: row.source_key.clone(),
            content_schema_version: row.content_schema_version,
            current_revision: row.current_revision,
            current_checkpoint_id: row.current_checkpoint_id.clone(),
            current_digest: row.current_digest.clone(),
            state: CaptureSubagentClaimState::Idle,
            attempt_digest: None,
            attempt_checkpoint_id: None,
            owner: None,
            lease_expires_at: None,
            fence_token: existing.map_or(row.fence_token, |e| e.fence_token.max(row.fence_token)),
            created_at: existing.map_or(row.created_at, |e| e.created_at.min(row.created_at)),
            updated_at: existing.map_or(row.updated_at, |e| e.updated_at.max(row.updated_at)),
            revision_cursor: row.revision_cursor,
            sync_revision: row.sync_revision,
        };
        cap_store
            .upsert_subagent_content_claim(&rec)
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure("advance restored claim", error)
            })?;
    }

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

#[cfg(feature = "storage-redb")]
async fn persist_local_import_tombstones_redb(
    storage: &std::path::Path,
    tombstones: &[AgentImportTombstoneRow],
) -> CloudResult<()> {
    use crate::storage::{
        capture_store::{AgentImportTombstoneRecord, CaptureStore, RedbCaptureStore},
        opener::RedbRepairContext,
        runtime::resolve_repo_capture_redb_path,
    };

    if tombstones.is_empty() {
        return Ok(());
    }
    let cap_path = resolve_repo_capture_redb_path(storage).map_err(|error| {
        local_agent_capture_restore_failure("resolve capture redb path for tombstones", error)
    })?;
    let cap_store = RedbCaptureStore::open_read_write(&cap_path, RedbRepairContext::WriteCommand)
        .await
        .map_err(|error| {
            local_agent_capture_restore_failure("open capture redb store for tombstones", error)
        })?;
    let existing = cap_store.list_import_tombstones().await.map_err(|error| {
        local_agent_capture_restore_failure("read local import tombstones", error)
    })?;
    let mut by_provider: HashMap<(String, String), AgentImportTombstoneRecord> = HashMap::new();
    let mut by_session: HashMap<String, AgentImportTombstoneRecord> = HashMap::new();
    for rec in existing {
        by_provider.insert(
            (rec.agent_kind.clone(), rec.provider_session_id.clone()),
            rec.clone(),
        );
        by_session.insert(rec.erased_session_id.clone(), rec);
    }
    for row in tombstones {
        let provider_key = (row.agent_kind.clone(), row.provider_session_id.clone());
        let matched = by_provider
            .get(&provider_key)
            .or_else(|| by_session.get(&row.erased_session_id));
        let rec = if let Some(prev) = matched {
            AgentImportTombstoneRecord {
                tombstone_id: prev.tombstone_id.clone(),
                agent_kind: prev.agent_kind.clone(),
                provider_session_id: prev.provider_session_id.clone(),
                erased_session_id: prev.erased_session_id.clone(),
                source_fingerprint: prev.source_fingerprint.clone(),
                erased_at: prev.erased_at.max(row.erased_at),
            }
        } else {
            AgentImportTombstoneRecord {
                tombstone_id: format!("tombstone-{}", Uuid::new_v4()),
                agent_kind: row.agent_kind.clone(),
                provider_session_id: row.provider_session_id.clone(),
                erased_session_id: row.erased_session_id.clone(),
                source_fingerprint: None,
                erased_at: row.erased_at,
            }
        };
        cap_store
            .put_import_tombstone(&rec)
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure("persist restored import tombstone", error)
            })?;
        by_provider.insert(provider_key, rec.clone());
        by_session.insert(rec.erased_session_id.clone(), rec);
    }
    Ok(())
}

#[cfg(feature = "storage-redb")]
fn ensure_redb_storage_manifest(storage: &std::path::Path) -> CloudResult<()> {
    use crate::storage::manifest::{
        INITIAL_STORAGE_GENERATION, StorageFormatManifest, publish_storage_manifest,
        read_storage_manifest,
    };

    let manifest = match read_storage_manifest(storage).map_err(|error| {
        CloudError::StorageIo(format!(
            "read storage-format.json during cloud restore: {error}"
        ))
    })? {
        Some(existing)
            if existing.backend == crate::storage::backend::StorageBackendKind::Redb
                && existing.finalized =>
        {
            existing
        }
        Some(existing) => {
            return Err(CloudError::StorageIo(format!(
                "refusing to finalize cloud restore against non-finalized or non-redb storage manifest (backend={:?}, finalized={})",
                existing.backend, existing.finalized
            )));
        }
        None => StorageFormatManifest::new_redb_finalized(INITIAL_STORAGE_GENERATION).map_err(
            |error| {
                CloudError::StorageIo(format!(
                    "construct redb storage-format.json during cloud restore: {error}"
                ))
            },
        )?,
    };
    publish_storage_manifest(storage, &manifest)
        .map(|_| ())
        .map_err(|error| {
            CloudError::StorageIo(format!(
                "publish storage-format.json during cloud restore: {error}"
            ))
        })
}

#[cfg(feature = "storage-redb")]
async fn sync_redb_meta_generation_and_manifest(
    storage: &std::path::Path,
    restored_generation: Option<u64>,
) -> CloudResult<()> {
    use crate::storage::{
        capture_store::RedbCaptureStore,
        git_store::RedbGitStore,
        opener::RedbRepairContext,
        runtime::{resolve_repo_capture_redb_path, resolve_repo_git_redb_path},
    };

    ensure_redb_storage_manifest(storage)?;

    let git_path = resolve_repo_git_redb_path(storage).map_err(|error| {
        CloudError::StorageIo(format!(
            "resolve git redb path while finalizing restore generation: {error}"
        ))
    })?;
    let capture_path = resolve_repo_capture_redb_path(storage).map_err(|error| {
        CloudError::StorageIo(format!(
            "resolve capture redb path while finalizing restore generation: {error}"
        ))
    })?;
    let git_rw = RedbGitStore::open_read_write(&git_path).map_err(|error| {
        CloudError::StorageIo(format!(
            "open git redb store while finalizing restore generation: {error}"
        ))
    })?;
    let cap_rw = RedbCaptureStore::open_read_write(&capture_path, RedbRepairContext::WriteCommand)
        .await
        .map_err(|error| {
            CloudError::StorageIo(format!(
                "open capture redb store while finalizing restore generation: {error}"
            ))
        })?;

    tokio::task::spawn_blocking(move || -> CloudResult<()> {
        let git_wtx = git_rw.begin_checked_write_tx().map_err(|error| {
            CloudError::StorageIo(format!(
                "begin git write tx for restore META_GENERATION sync: {error}"
            ))
        })?;
        let cap_wtx = cap_rw.begin_checked_write_tx().map_err(|error| {
            CloudError::StorageIo(format!(
                "begin capture write tx for restore META_GENERATION sync: {error}"
            ))
        })?;

        let git_gen =
            RedbGitStore::read_cross_store_generation_in_wtx(&git_wtx).map_err(|error| {
                CloudError::StorageIo(format!("read git META_GENERATION during restore: {error}"))
            })?;
        let cap_gen =
            RedbCaptureStore::read_cross_store_generation_in_wtx(&cap_wtx).map_err(|error| {
                CloudError::StorageIo(format!(
                    "read capture META_GENERATION during restore: {error}"
                ))
            })?;

        let target_gen = git_gen
            .max(cap_gen)
            .max(restored_generation.unwrap_or(0))
            .max(1);

        RedbGitStore::write_cross_store_generation_in_wtx(&git_wtx, target_gen).map_err(
            |error| {
                CloudError::StorageIo(format!("write git META_GENERATION during restore: {error}"))
            },
        )?;
        let _ = RedbGitStore::prune_reconciled_cross_store_intents_in_wtx(&git_wtx, target_gen)
            .map_err(|error| {
                CloudError::StorageIo(format!(
                    "prune reconciled cross-store intents during restore: {error}"
                ))
            })?;
        RedbCaptureStore::write_cross_store_generation_in_wtx(&cap_wtx, target_gen).map_err(
            |error| {
                CloudError::StorageIo(format!(
                    "write capture META_GENERATION during restore: {error}"
                ))
            },
        )?;

        git_wtx.commit().map_err(|error| {
            CloudError::StorageIo(format!(
                "commit git META_GENERATION during restore: {error}"
            ))
        })?;
        cap_wtx.commit().map_err(|error| {
            CloudError::StorageIo(format!(
                "commit capture META_GENERATION during restore: {error}"
            ))
        })?;
        cap_rw.record_write_tx_commit();
        Ok(())
    })
    .await
    .map_err(|error| {
        CloudError::StorageIo(format!("join restore META_GENERATION sync task: {error}"))
    })??;

    Ok(())
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
    // The catalog lives in `capture.db` (or in a combined database that has
    // not been split yet); refs and object rows stay in `libra.db`.
    let catalog = super::agent_capture::local_capture_catalog(db_conn).await?;
    let backend = catalog.get_database_backend();
    let session_present = catalog
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_session' LIMIT 1",
            [],
        ))
        .await
        .map_err(|e| local_agent_capture_restore_failure("probe local capture schema", e))?
        .is_some();
    let checkpoint_present = catalog
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

    let local_cloud_base = load_local_agent_capture_cloud_base(&catalog, repo_id).await?;
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
    persist_local_import_tombstones(&catalog, &rows.import_tombstones).await?;
    store_local_agent_capture_cloud_base(&catalog, repo_id, remote_generation).await?;
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
    // traces ref` below), so the write lock is taken up front. After the
    // catalog moved to `capture.db`, the traces ref stays in `libra.db`: the
    // repository writer is taken first and the catalog writer second (the
    // order every split writer uses), the ref commits first and the catalog
    // last. A catalog COMMIT failure after the ref commit leaves a
    // ref-reachable checkpoint without its row, which `libra agent doctor
    // --repair` rebuilds. The two files are never attached.
    let catalog = super::agent_capture::local_capture_catalog(db_conn).await?;
    let split_catalog = !crate::internal::db::connection_has_table(db_conn, "agent_session")
        .await
        .map_err(|error| {
            local_agent_capture_restore_failure("probe local capture schema", error)
        })?;
    let repository_txn = if split_catalog {
        Some(
            crate::internal::db::begin_write_transaction(db_conn)
                .await
                .map_err(|error| {
                    local_agent_capture_restore_failure("begin traces ref restore", error)
                })?,
        )
    } else {
        None
    };
    let txn = if split_catalog {
        crate::internal::db::begin_catalog_or_repository_write(&catalog).await
    } else {
        crate::internal::db::begin_write_transaction(db_conn).await
    }
    .map_err(|error| {
        local_agent_capture_restore_failure("begin atomic agent capture restore", error)
    })?;
    let ref_txn = repository_txn.as_ref().unwrap_or(&txn);
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

    let existing_traces_ref = ref_txn
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
        ref_txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE reference SET `commit` = ? WHERE id = ?",
                [traces_head.map(str::to_string).into(), ref_id.into()],
            ))
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure("restore fenced traces ref", error)
            })?;
    } else {
        ref_txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT INTO reference (name, kind, `commit`, remote, worktree_id)
             VALUES (?, 'Branch', ?, NULL, NULL)",
                [
                    crate::internal::branch::TRACES_BRANCH.into(),
                    traces_head.map(str::to_string).into(),
                ],
            ))
            .await
            .map_err(|error| {
                local_agent_capture_restore_failure("create fenced traces ref", error)
            })?;
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

    if let Some(repository_txn) = repository_txn {
        repository_txn.commit().await.map_err(|error| {
            local_agent_capture_restore_failure("commit restored traces ref", error)
        })?;
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

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn restore_metadata(
    db_conn: &sea_orm::DatabaseConnection,
    r2_storage: &RemoteStorage,
) -> CloudResult<Vec<reference::Model>> {
    restore_metadata_with_output(db_conn, r2_storage, true).await
}

pub(crate) async fn restore_metadata_with_output(
    db_conn: &sea_orm::DatabaseConnection,
    r2_storage: &RemoteStorage,
    render_human: bool,
) -> CloudResult<Vec<reference::Model>> {
    if render_human {
        println!("Restoring metadata...");
    }

    let data = match r2_storage.get_metadata().await {
        Ok(data) => data,
        Err(e) => {
            if render_human {
                println!("warning: failed to download metadata: {}", e);
                return Ok(Vec::new());
            }
            return Err(CloudError::R2(format!("failed to download metadata: {e}")));
        }
    };
    let deferred_capture_refs = restore_metadata_from_bytes(db_conn, &data).await?;
    if render_human {
        println!("Metadata restored.");
    }
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

    #[tokio::test]
    async fn reconcile_local_object_format_switches_empty_repo_to_backup_kind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("libra.db");
        let objects_dir = dir.path().join("objects");
        let index_path = dir.path().join("index");
        std::fs::create_dir_all(objects_dir.join("pack")).expect("create pack dir");

        let conn = crate::internal::db::create_database(db_path.to_str().expect("utf8 db path"))
            .await
            .expect("create db");
        ConfigKv::set_with_conn(&conn, "core.objectformat", "sha1", false)
            .await
            .expect("seed sha1 format");

        reconcile_local_object_format_with(
            &conn,
            &objects_dir,
            &index_path,
            git_internal::hash::HashKind::Sha1,
            git_internal::hash::HashKind::Sha256,
            false,
        )
        .await
        .expect("empty fresh repo should adopt backup object format");

        let stored = ConfigKv::get_with_conn(&conn, "core.objectformat")
            .await
            .expect("read config")
            .expect("core.objectformat entry");
        assert_eq!(stored.value, "sha256");
    }

    #[tokio::test]
    async fn reconcile_local_object_format_refuses_non_empty_repo_without_mutating_config() {
        // Case 1: local object_index row blocks format switch.
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let db_path = dir.path().join("libra.db");
            let objects_dir = dir.path().join("objects");
            let index_path = dir.path().join("index");
            let conn =
                crate::internal::db::create_database(db_path.to_str().expect("utf8 db path"))
                    .await
                    .expect("create db");
            ConfigKv::set_with_conn(&conn, "core.objectformat", "sha1", false)
                .await
                .expect("seed sha1");
            object_index::ActiveModel {
                o_id: Set("a".repeat(40)),
                o_type: Set("blob".to_string()),
                o_size: Set(5),
                repo_id: Set("repo".to_string()),
                created_at: Set(1),
                is_synced: Set(0),
                ..Default::default()
            }
            .insert(&conn)
            .await
            .expect("insert object_index");

            let err = reconcile_local_object_format_with(
                &conn,
                &objects_dir,
                &index_path,
                git_internal::hash::HashKind::Sha1,
                git_internal::hash::HashKind::Sha256,
                false,
            )
            .await
            .expect_err("non-empty object_index must refuse format switch");
            assert!(matches!(
                err,
                CloudError::ObjectFormatMismatch {
                    ref local,
                    ref remote,
                    ..
                } if local == "sha1" && remote == "sha256"
            ));
            let stored = ConfigKv::get_with_conn(&conn, "core.objectformat")
                .await
                .expect("read config")
                .expect("entry");
            assert_eq!(stored.value, "sha1");
        }

        // Case 2: local committed reference blocks format switch.
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let db_path = dir.path().join("libra.db");
            let objects_dir = dir.path().join("objects");
            let index_path = dir.path().join("index");
            let conn =
                crate::internal::db::create_database(db_path.to_str().expect("utf8 db path"))
                    .await
                    .expect("create db");
            ConfigKv::set_with_conn(&conn, "core.objectformat", "sha1", false)
                .await
                .expect("seed sha1");
            reference::ActiveModel {
                name: Set(Some("main".to_string())),
                kind: Set(reference::ConfigKind::Branch),
                commit: Set(Some("b".repeat(40))),
                remote: Set(None),
                worktree_id: Set(None),
                ..Default::default()
            }
            .insert(&conn)
            .await
            .expect("insert committed ref");

            let err = reconcile_local_object_format_with(
                &conn,
                &objects_dir,
                &index_path,
                git_internal::hash::HashKind::Sha1,
                git_internal::hash::HashKind::Sha256,
                false,
            )
            .await
            .expect_err("committed ref must refuse format switch");
            assert!(matches!(err, CloudError::ObjectFormatMismatch { .. }));
        }

        // Case 3: loose object file blocks format switch.
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let db_path = dir.path().join("libra.db");
            let objects_dir = dir.path().join("objects");
            let index_path = dir.path().join("index");
            std::fs::create_dir_all(objects_dir.join("aa")).expect("shard dir");
            std::fs::write(objects_dir.join("aa").join("bb"), b"obj").expect("loose obj");
            let conn =
                crate::internal::db::create_database(db_path.to_str().expect("utf8 db path"))
                    .await
                    .expect("create db");
            ConfigKv::set_with_conn(&conn, "core.objectformat", "sha1", false)
                .await
                .expect("seed sha1");

            let err = reconcile_local_object_format_with(
                &conn,
                &objects_dir,
                &index_path,
                git_internal::hash::HashKind::Sha1,
                git_internal::hash::HashKind::Sha256,
                false,
            )
            .await
            .expect_err("loose object must refuse format switch");
            assert!(matches!(err, CloudError::ObjectFormatMismatch { .. }));
        }
    }

    #[tokio::test]
    async fn restore_metadata_silent_mode_returns_r2_error_on_missing_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("libra.db");
        let conn = crate::internal::db::create_database(db_path.to_str().expect("utf8 db path"))
            .await
            .expect("create db");
        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));

        let err = restore_metadata_with_output(&conn, &remote, false)
            .await
            .expect_err("silent mode must surface missing metadata instead of printing to stdout");
        assert!(
            matches!(err, CloudError::R2(ref detail) if detail.contains("failed to download metadata")),
            "unexpected error: {err:?}"
        );
    }

    #[cfg(feature = "storage-redb")]
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn redb_concurrent_snapshot_restore_manifest_and_gen_parity() {
        use std::{
            sync::{
                Arc,
                atomic::{AtomicUsize, Ordering},
            },
            time::Duration,
        };

        use super::super::snapshot::redb_test_helpers::*;
        use crate::{
            storage::{
                backend::StorageBackendKind,
                capture_store::{
                    CaptureBatchItem, CaptureBatchPayload, CaptureStore, RedbCaptureStore,
                },
                cross_store::{CrossStoreCoordinator, CrossStoreRefCasOp},
                git_store::{GitStore, RedbGitStore, RefKey, RefTarget},
                manifest::read_storage_manifest,
                opener::RedbRepairContext,
                runtime::{resolve_repo_capture_redb_path, resolve_repo_git_redb_path},
            },
            utils::{
                d1_client::test_support::MockD1, storage::remote::RemoteStorage,
                test::ChangeDirGuard,
            },
        };

        let src_dir = tempfile::tempdir().expect("src tempdir");
        let repo_id = "22222222-3333-4444-8555-666666666666";
        let project_name = "rb28b-restore-demo";
        let mock_d1 = MockD1::spawn().await;
        let memory_r2 = Arc::new(object_store::memory::InMemory::new());

        let (main_commit, session_id, cp1_id, cp2_id, cp3_id, cp3_traces) = {
            let _src_cwd = ChangeDirGuard::new(src_dir.path());
            let src_storage =
                init_empty_redb_repo(src_dir.path(), Some(repo_id), Some(project_name)).await;

            let main_commit = write_worktree_commit(
                &src_storage,
                "hello.txt",
                b"restored from redb cloud snapshot\n",
                "initial commit",
            );
            {
                let git_path = resolve_repo_git_redb_path(&src_storage).expect("src git path");
                let git_rw = RedbGitStore::open_read_write(&git_path).expect("src git rw");
                git_rw
                    .update_ref_cas(
                        &RefKey::LocalBranch {
                            name: "main".to_string(),
                        },
                        None,
                        Some(&RefTarget::Direct(main_commit.clone())),
                        &[],
                    )
                    .await
                    .expect("set main");
                git_rw
                    .update_ref_cas(
                        &RefKey::LocalHead { worktree_id: None },
                        None,
                        Some(&RefTarget::Symbolic("main".to_string())),
                        &[],
                    )
                    .await
                    .expect("set HEAD");
            }

            let session = sample_session("rb28b-sess-1", src_dir.path());
            let cp1_objs =
                write_checkpoint_objects(&src_storage, "aa1122334455", &session.session_id, None);
            let cp2_objs = write_checkpoint_objects(
                &src_storage,
                "bb2233445566",
                &session.session_id,
                Some(&cp1_objs.traces_commit),
            );
            let cp3_objs = write_checkpoint_objects(
                &src_storage,
                "cc3344556677",
                &session.session_id,
                Some(&cp2_objs.traces_commit),
            );
            index_all_loose_objects_redb(&src_storage, repo_id).await;

            let cp1 = sample_checkpoint(&cp1_objs, &session.session_id, None, 1_700_000_201);
            let cp2 = sample_checkpoint(
                &cp2_objs,
                &session.session_id,
                Some(&cp1.checkpoint_id),
                1_700_000_202,
            );
            let cp3 = sample_checkpoint(
                &cp3_objs,
                &session.session_id,
                Some(&cp2.checkpoint_id),
                1_700_000_203,
            );

            let g1 = commit_checkpoint_cross_store(&src_storage, None, Some(&session), &cp1).await;
            assert_eq!(g1, 1);

            // Simulate concurrent checkpoint writes during snapshot freeze:
            // attempt 0 sees gen 1 -> concurrent commit advances to gen 2;
            // attempt 1 sees gen 2 -> concurrent commit advances to gen 3;
            // attempt 2 freezes consistent gen 3 (g1 == c1 == g2 == 3).
            let concurrent_commits = Arc::new(AtomicUsize::new(0));
            let concurrent_commits_cb = Arc::clone(&concurrent_commits);
            let storage_hook = src_storage.clone();
            let cp1_traces = cp1.traces_commit.clone();
            let cp2_clone = cp2.clone();
            let cp3_clone = cp3.clone();
            let handle = tokio::runtime::Handle::current();

            let tuning = super::super::snapshot::SnapshotTuning {
                redb_retry_backoff: Some(Duration::ZERO),
                redb_between_c1_and_g2: Some(Arc::new(move |attempt, obs_g1, obs_c1| {
                    let (expected_traces, next_cp, expected_next_gen) = match attempt {
                        0 => {
                            assert_eq!((obs_g1, obs_c1), (1, 1));
                            (cp1_traces.clone(), cp2_clone.clone(), 2u64)
                        }
                        1 => {
                            assert_eq!((obs_g1, obs_c1), (2, 2));
                            (cp2_clone.traces_commit.clone(), cp3_clone.clone(), 3u64)
                        }
                        _ => return,
                    };
                    concurrent_commits_cb.fetch_add(1, Ordering::SeqCst);
                    let git_path =
                        resolve_repo_git_redb_path(&storage_hook).expect("hook git path");
                    let cap_path =
                        resolve_repo_capture_redb_path(&storage_hook).expect("hook cap path");
                    handle.block_on(async move {
                        let git_rw = RedbGitStore::open_read_write(&git_path).expect("git rw");
                        let cap_rw = RedbCaptureStore::open_read_write(
                            &cap_path,
                            RedbRepairContext::WriteCommand,
                        )
                        .await
                        .expect("cap rw");
                        let coord = CrossStoreCoordinator::new(git_rw, cap_rw);
                        let op = CrossStoreRefCasOp {
                            key: RefKey::LocalBranch {
                                name: crate::internal::branch::TRACES_BRANCH.to_string(),
                            },
                            expected: Some(RefTarget::Direct(expected_traces)),
                            new_target: Some(RefTarget::Direct(next_cp.traces_commit.clone())),
                            reflogs: vec![],
                            publication: None,
                        };
                        let out = coord
                            .commit_ref_cas_with_capture(
                                &op,
                                &[CaptureBatchItem {
                                    dedup_key: format!("cp:{}", next_cp.checkpoint_id),
                                    payload: CaptureBatchPayload::UpsertCheckpoint(next_cp),
                                }],
                            )
                            .await
                            .expect("concurrent checkpoint commit during snapshot");
                        assert_eq!(out.generation, expected_next_gen);
                    });
                })),
                ..Default::default()
            };

            let report = super::super::sync::run_cloud_sync_from_snapshot(
                CloudSyncContext {
                    batch_size: 32,
                    force: false,
                },
                &SilentCloudSyncProgress,
                mock_d1.client(),
                Some(RemoteStorage::new(
                    Arc::clone(&memory_r2) as Arc<dyn object_store::ObjectStore>
                )),
                &tuning,
            )
            .await
            .expect("source cloud sync must freeze consistent snapshot across concurrent writes");

            assert_eq!(concurrent_commits.load(Ordering::SeqCst), 2);
            assert_eq!(report.failed_count, 0);
            assert!(matches!(
                report.agent_capture,
                AgentCaptureSyncOutcome::Completed {
                    sessions_synced: 1,
                    checkpoints_synced: 3,
                    ..
                }
            ));

            (
                main_commit,
                session.session_id,
                cp1.checkpoint_id,
                cp2.checkpoint_id,
                cp3.checkpoint_id,
                cp3.traces_commit,
            )
        };

        // Read the authoritative D1 generation published by sync.
        let d1_gen = mock_d1
            .client()
            .get_agent_capture_generation(repo_id)
            .await
            .expect("read d1 generation")
            .expect("d1 generation row");
        assert_eq!(d1_gen.state, "complete");
        let expected_restored_gen =
            u64::try_from(d1_gen.generation).expect("positive remote generation");
        assert!(expected_restored_gen >= 1);

        // Restore into a brand-new empty Redb repository.
        let dst_dir = tempfile::tempdir().expect("dst tempdir");
        let _dst_cwd = ChangeDirGuard::new(dst_dir.path());
        let dst_storage = init_empty_redb_repo(dst_dir.path(), None, None).await;

        // Sanity check: empty target repo starts at META_GENERATION == 0 in both stores.
        {
            let git_path = resolve_repo_git_redb_path(&dst_storage).expect("dst git path");
            let cap_path = resolve_repo_capture_redb_path(&dst_storage).expect("dst cap path");
            assert_eq!(
                RedbGitStore::open_read_only(&git_path)
                    .expect("dst git ro")
                    .read_cross_store_generation()
                    .expect("initial dst git gen"),
                0
            );
            assert_eq!(
                RedbCaptureStore::open_read_only(&cap_path)
                    .await
                    .expect("dst cap ro")
                    .read_cross_store_generation()
                    .expect("initial dst cap gen"),
                0
            );
        }

        let restore_out = run_cloud_restore_with_clients(
            RestoreArgs {
                repo_id: None,
                name: Some(project_name.to_string()),
                metadata_only: false,
            },
            mock_d1.client(),
            Some(RemoteStorage::new(
                Arc::clone(&memory_r2) as Arc<dyn object_store::ObjectStore>
            )),
            false,
        )
        .await
        .expect("cloud restore into empty Redb repository must succeed");

        assert_eq!(restore_out.repo_id, repo_id);
        assert_eq!(restore_out.metadata.status, "restored");
        assert_eq!(restore_out.agent_capture.status, "restored");
        assert!(
            !dst_storage.join("libra.db").exists(),
            "Redb restore must never create libra.db"
        );
        assert!(
            !dst_storage.join("private/capture.db").exists(),
            "Redb restore must never create private/capture.db"
        );

        // RB-28b AC2: `.libra/storage-format.json` manifest is valid, finalized, and Redb.
        let manifest = read_storage_manifest(&dst_storage)
            .expect("read restored storage-format.json")
            .expect("storage-format.json must exist after Redb restore");
        assert_eq!(manifest.backend, StorageBackendKind::Redb);
        assert!(manifest.finalized, "storage-format.json must be finalized");
        let (git_db_path, capture_db_path) = manifest
            .resolve_paths(&dst_storage)
            .expect("resolve paths from restored storage-format.json");
        assert!(git_db_path.is_file());
        assert!(capture_db_path.is_file());

        // RB-28b AC1 & AC3: Git refs + Capture catalog parity + matching META_GENERATION.
        let dst_git = RedbGitStore::open_read_only(&git_db_path).expect("dst git ro");
        let dst_cap = RedbCaptureStore::open_read_only(&capture_db_path)
            .await
            .expect("dst cap ro");

        assert_eq!(
            dst_git
                .get_ref(&RefKey::LocalBranch {
                    name: "main".to_string(),
                })
                .await
                .expect("dst main ref"),
            Some(RefTarget::Direct(main_commit))
        );
        assert_eq!(
            dst_git
                .get_ref(&RefKey::LocalBranch {
                    name: crate::internal::branch::TRACES_BRANCH.to_string(),
                })
                .await
                .expect("dst traces ref"),
            Some(RefTarget::Direct(cp3_traces.clone())),
            "restored TRACES_BRANCH must match frozen generation's traces tip"
        );

        let restored_sessions = dst_cap
            .list_sessions_by_started_desc()
            .await
            .expect("list restored sessions");
        assert_eq!(restored_sessions.len(), 1);
        assert_eq!(restored_sessions[0].session_id, session_id);

        let restored_checkpoints = dst_cap
            .list_checkpoints_by_session(&session_id)
            .await
            .expect("list restored checkpoints");
        let restored_cp_ids: HashSet<String> = restored_checkpoints
            .iter()
            .map(|c| c.checkpoint_id.clone())
            .collect();
        assert_eq!(
            restored_cp_ids,
            HashSet::from([cp1_id, cp2_id, cp3_id.clone()]),
            "restored capture catalog must contain exact frozen generation checkpoints"
        );
        let tip_cp = restored_checkpoints
            .iter()
            .find(|c| c.checkpoint_id == cp3_id)
            .expect("tip checkpoint");
        assert_eq!(
            tip_cp.traces_commit, cp3_traces,
            "Git TRACES_BRANCH ref and Capture catalog tip must be in exact parity"
        );

        let restored_git_gen = dst_git
            .read_cross_store_generation()
            .expect("restored git META_GENERATION");
        let restored_cap_gen = dst_cap
            .read_cross_store_generation()
            .expect("restored capture META_GENERATION");
        assert_eq!(restored_git_gen, expected_restored_gen);
        assert_eq!(restored_cap_gen, expected_restored_gen);
        assert!(
            dst_git
                .list_cross_store_intent_generations()
                .expect("restored intents")
                .is_empty()
        );

        // Fresh 3-step read-only consistent snapshot on the restored repo succeeds on attempt 1.
        let coord = CrossStoreCoordinator::new(dst_git, dst_cap);
        let snap = coord
            .snapshot_consistent_read_only()
            .await
            .expect("consistent snapshot on restored repo");
        assert_eq!(snap.generation(), expected_restored_gen);
        assert_eq!(snap.attempts(), 1);

        // Restored worktree file matches source commit.
        let restored_file =
            std::fs::read_to_string(dst_dir.path().join("hello.txt")).expect("read hello.txt");
        assert_eq!(restored_file, "restored from redb cloud snapshot\n");
    }
}
