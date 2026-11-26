use super::{
    metadata::sync_metadata,
    snapshot::{FrozenObject, SnapshotSpool, SnapshotTuning, SpoolWarningSink},
    *,
};

/// Kinds the current D1 capture CHECK accepts: the same list the D1 DDL is
/// built from, so the fence and the DDL cannot drift (PI-05). `hermes` and
/// any later tag stay off this list. `--force` does not widen it. PI-09 still
/// owns the frozen snapshot; this check only refuses before the first remote
/// call.
pub(crate) const CLOUD_CAPTURE_KIND_ALLOWLIST: &[&str] =
    crate::utils::d1_client::D1_CAPTURE_AGENT_KINDS;

async fn unsupported_capture_kinds() -> CloudResult<Vec<String>> {
    Ok(local_capture_kinds()
        .await?
        .into_iter()
        .filter(|kind| !CLOUD_CAPTURE_KIND_ALLOWLIST.contains(&kind.as_str()))
        .collect())
}

/// Distinct local `agent_session.agent_kind` values, sorted.
async fn local_capture_kinds() -> CloudResult<Vec<String>> {
    let storage = util::try_get_storage_path(None).map_err(|error| {
        CloudError::Generic(format!(
            "locate repository storage before cloud sync: {error}"
        ))
    })?;
    match crate::storage::runtime::detect_repo_backend(&storage).map_err(|error| {
        CloudError::StorageIo(format!(
            "detect repository storage backend before cloud sync: {error}"
        ))
    })? {
        crate::storage::backend::StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let store = crate::storage::runtime::open_capture_store_read_only(&storage)
                    .await
                    .map_err(|error| {
                        CloudError::StorageIo(format!(
                            "open capture store before cloud sync: {error}"
                        ))
                    })?;
                let sessions = store
                    .list_sessions_by_started_desc()
                    .await
                    .map_err(|error| {
                        CloudError::StorageIo(format!(
                            "read agent_session before cloud sync: {error}"
                        ))
                    })?;
                let mut kinds = std::collections::BTreeSet::new();
                for session in sessions {
                    kinds.insert(session.agent_kind.as_str().to_string());
                }
                Ok(kinds.into_iter().collect())
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err(CloudError::Generic(
                    "this Libra binary was compiled without `storage-redb`".to_string(),
                ))
            }
        }
        crate::storage::backend::StorageBackendKind::Sqlite => {
            use sea_orm::Statement;

            let repository = db::get_db_conn_instance().await;
            let db_conn = super::agent_capture::local_capture_catalog(&repository).await?;
            let backend = db_conn.get_database_backend();
            let present = db_conn
                .query_one_raw(Statement::from_string(
                    backend,
                    "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_session' LIMIT 1"
                        .to_string(),
                ))
                .await
                .map_err(|error| {
                    CloudError::Generic(format!("read agent_session before cloud sync: {error}"))
                })?;
            if present.is_none() {
                return Ok(Vec::new());
            }
            let rows = db_conn
                .query_all_raw(Statement::from_string(
                    backend,
                    "SELECT DISTINCT agent_kind FROM agent_session ORDER BY agent_kind".to_string(),
                ))
                .await
                .map_err(|error| {
                    CloudError::Generic(format!(
                        "read agent_session.agent_kind before cloud sync: {error}"
                    ))
                })?;
            rows.into_iter()
                .map(|row| {
                    row.try_get_by::<String, _>("agent_kind").map_err(|error| {
                        CloudError::Generic(format!(
                            "decode agent_session.agent_kind before cloud sync: {error}"
                        ))
                    })
                })
                .collect()
        }
    }
}

/// PI-18: refuse when an existing D1 capture table's `agent_kind` CHECK would
/// reject a local kind. D1 tables are never rebuilt, so a table created before
/// GK-01 still lacks `grok`; without this check the objects upload first and
/// the session mirror fails afterwards, leaving a partial backup. Only
/// read-only `sqlite_master` queries run here. Every existing table is probed
/// even without local sessions, and a CHECK that cannot be read (including a
/// failed probe) refuses the sync.
pub(super) async fn refuse_kinds_rejected_by_remote_check(
    d1_client: &D1Client,
    local_kinds: &[String],
) -> CloudResult<()> {
    use crate::utils::d1_client::{REMOTE_CAPTURE_KIND_TABLES, RemoteAgentKindCheck};

    for table in REMOTE_CAPTURE_KIND_TABLES {
        let check = match d1_client.agent_kind_check(table).await {
            Ok(check) => check,
            Err(error) => {
                // The D1 message may quote remote text; only the code is shown.
                tracing::warn!(
                    d1_operation = "read remote agent_kind CHECK",
                    d1_error_code = error.code,
                    "cloud D1 request failed"
                );
                return Err(CloudError::RemoteCaptureKindRefused(format!(
                    "cloud sync refused before any remote write: the agent_kind CHECK of D1 table {table} could not be read (D1 code {})",
                    error.code
                )));
            }
        };
        match check {
            RemoteAgentKindCheck::Absent => {}
            RemoteAgentKindCheck::Allowed(allowed) => {
                let rejected: Vec<&str> = local_kinds
                    .iter()
                    .filter(|kind| !allowed.contains(kind))
                    .map(String::as_str)
                    .collect();
                if !rejected.is_empty() {
                    return Err(CloudError::RemoteCaptureKindRefused(format!(
                        "cloud sync refused before any remote write: existing D1 table {table} does not accept agent_kind {}",
                        rejected.join(", ")
                    )));
                }
            }
            RemoteAgentKindCheck::Unparseable => {
                return Err(CloudError::RemoteCaptureKindRefused(format!(
                    "cloud sync refused before any remote write: the agent_kind CHECK of existing D1 table {table} could not be read"
                )));
            }
        }
    }
    Ok(())
}

/// Phase 1 helper extracted from `execute_sync`.
///
/// Runs the full `libra cloud sync` flow without printing directly to
/// stdout / stderr: env validation → D1 / R2 init → object stream →
/// metadata refresh → agent_capture mirror. Human-readable progress
/// flows through the [`CloudSyncProgress`] trait so callers can plug
/// in their own renderer (`ConsoleCloudSyncProgress` for the legacy
/// CLI, a quieter or structured one for `libra publish` later).
///
/// Returns a [`CloudSyncReport`] for the completed run. Hard errors
/// (env, D1, R2, repo-id, db-query, metadata-sync) short-circuit as
/// `Err`. Per-object failures are captured in `failed_count` and skip
/// the metadata + agent_capture phases (preserving the pre-Phase-1
/// "block follow-up work on object failure" gate). A kind outside
/// [`CLOUD_CAPTURE_KIND_ALLOWLIST`] returns before progress or any
/// remote call.
pub(crate) async fn run_cloud_sync(
    ctx: CloudSyncContext,
    progress: &dyn CloudSyncProgress,
) -> CloudResult<CloudSyncReport> {
    if ctx.batch_size < 1 {
        return Err(CloudError::Generic(
            "Batch size must be at least 1".to_string(),
        ));
    }

    // Leftovers of earlier runs first, on every start (PI-25): a later
    // refusal or configuration error still names the ones kept.
    let tuning = SnapshotTuning::default();
    let residue_notes = clean_residues_at_start(&tuning);
    let prepared = async {
        // Before progress, env validation, D1, or R2. `force` is intentionally unused.
        let rejected = unsupported_capture_kinds().await?;
        if !rejected.is_empty() {
            return Err(CloudError::LocalCaptureKindRefused(format!(
                "cloud sync refused before any remote write: agent_session.agent_kind {} is outside the cloud capture allowlist and was not mirrored",
                rejected.join(", ")
            )));
        }

        progress.on_starting();

        validate_cloud_backup_env(false).await?;

        // Initialize D1 client.
        D1Client::from_env()
            .await
            .map_err(|error| cloud_d1_failure("initialize cloud client", &error))
    }
    .await;
    match prepared {
        Ok(d1_client) => {
            sync_after_cleanup(ctx, progress, d1_client, None, &tuning, residue_notes).await
        }
        Err(error) => Err(with_notes(error, &residue_notes)),
    }
}

/// Remove the leftovers of earlier runs whose owner provably exited and
/// describe the rest (PI-25); no notes when the repository storage cannot be
/// located, which then fails the sync on its own.
fn clean_residues_at_start(tuning: &SnapshotTuning) -> Vec<String> {
    let Ok(storage) = util::try_get_storage_path(None) else {
        return Vec::new();
    };
    let raw_root = util::working_dir().join(util::ROOT_DIR);
    super::snapshot::clean_residues(&storage, &raw_root, tuning)
}

/// `error` with each of `notes` appended.
fn with_notes(error: CloudError, notes: &[String]) -> CloudError {
    notes
        .iter()
        .fold(error, |error, note| error.with_note(note))
}

/// Remote phases of [`run_cloud_sync`] with an injected D1 client and, in
/// tests, an injected R2 store (`None` builds R2 from the environment). The
/// first remote request is the read-only `agent_kind` CHECK probe (PI-18), so
/// a refusal happens before any remote write.
#[cfg(test)]
pub(super) async fn run_cloud_sync_with_clients(
    ctx: CloudSyncContext,
    progress: &dyn CloudSyncProgress,
    d1_client: D1Client,
    r2_override: Option<RemoteStorage>,
) -> CloudResult<CloudSyncReport> {
    run_cloud_sync_from_snapshot(
        ctx,
        progress,
        d1_client,
        r2_override,
        &SnapshotTuning::default(),
    )
    .await
}

/// [`run_cloud_sync_with_clients`] with explicit snapshot knobs. After the
/// PI-18 probe and before the first remote write, the object identities and
/// the refs projection to upload are frozen into this run's spool (PI-09);
/// every later phase reads the spool, not the live tables.
#[cfg(test)]
pub(super) async fn run_cloud_sync_from_snapshot(
    ctx: CloudSyncContext,
    progress: &dyn CloudSyncProgress,
    d1_client: D1Client,
    r2_override: Option<RemoteStorage>,
    tuning: &SnapshotTuning,
) -> CloudResult<CloudSyncReport> {
    let residue_notes = clean_residues_at_start(tuning);
    sync_after_cleanup(ctx, progress, d1_client, r2_override, tuning, residue_notes).await
}

/// The sync after the leftover cleanup: every outcome carries
/// `residue_notes` — reported with a success, appended to an error.
async fn sync_after_cleanup(
    ctx: CloudSyncContext,
    progress: &dyn CloudSyncProgress,
    d1_client: D1Client,
    r2_override: Option<RemoteStorage>,
    tuning: &SnapshotTuning,
    residue_notes: Vec<String>,
) -> CloudResult<CloudSyncReport> {
    let local_kinds = local_capture_kinds()
        .await
        .map_err(|error| with_notes(error, &residue_notes))?;
    refuse_kinds_rejected_by_remote_check(&d1_client, &local_kinds)
        .await
        .map_err(|error| with_notes(error, &residue_notes))?;

    // Freeze what this run uploads before any remote write (PI-09).
    let storage = util::try_get_storage_path(None).map_err(|error| {
        with_notes(
            CloudError::Generic(format!(
                "locate the repository storage for the snapshot: {error}"
            )),
            &residue_notes,
        )
    })?;
    let raw_root = util::working_dir().join(util::ROOT_DIR);
    let warning_sink = if progress.spool_warnings_as_ndjson() {
        SpoolWarningSink::NdjsonProgress
    } else {
        SpoolWarningSink::Stderr
    };
    match crate::storage::runtime::detect_repo_backend(&storage).map_err(|error| {
        with_notes(
            CloudError::StorageIo(format!(
                "detect repository storage backend for cloud sync: {error}"
            )),
            &residue_notes,
        )
    })? {
        crate::storage::backend::StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let repo_id = ensure_repo_id().await;
                let mut spool =
                    match SnapshotSpool::create(&storage, &raw_root, tuning, warning_sink) {
                        Ok(spool) => spool,
                        Err(error) => return Err(with_notes(error, &residue_notes)),
                    };
                let result = sync_from_spool_redb(
                    &mut spool,
                    ctx,
                    progress,
                    d1_client,
                    r2_override,
                    tuning,
                    repo_id,
                    &storage,
                )
                .await;
                finish_spool_result(spool, result, progress, residue_notes)
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err(with_notes(
                    CloudError::Generic(
                        "this Libra binary was compiled without `storage-redb`".to_string(),
                    ),
                    &residue_notes,
                ))
            }
        }
        crate::storage::backend::StorageBackendKind::Sqlite => {
            // Get database connection.
            let db_conn = db::get_db_conn_instance().await;

            // Check if object_index table exists locally, create if not.
            let builder = db_conn.get_database_backend();
            let schema = Schema::new(builder);
            let stmt = schema
                .create_table_from_entity(object_index::Entity)
                .if_not_exists()
                .to_owned();

            let _ = db_conn.execute_raw(builder.build(&stmt)).await;

            let repo_id = ensure_repo_id().await;

            let mut spool = match SnapshotSpool::create(&storage, &raw_root, tuning, warning_sink) {
                Ok(spool) => spool,
                Err(error) => return Err(with_notes(error, &residue_notes)),
            };
            let result = sync_from_spool(
                &mut spool,
                ctx,
                progress,
                d1_client,
                r2_override,
                tuning,
                &db_conn,
                repo_id,
                &storage,
            )
            .await;
            finish_spool_result(spool, result, progress, residue_notes)
        }
    }
}

fn finish_spool_result(
    spool: SnapshotSpool,
    result: CloudResult<CloudSyncReport>,
    progress: &dyn CloudSyncProgress,
    residue_notes: Vec<String>,
) -> CloudResult<CloudSyncReport> {
    // Remove the spool on every outcome; a directory that cannot be removed
    // is reported, never silently kept, after any leftover of earlier runs.
    let mut notes = residue_notes;
    notes.extend(spool.finish());
    match result {
        Ok(mut report) => {
            for note in &notes {
                progress.on_spool_warning(note);
            }
            report.spool_warnings = notes;
            Ok(report)
        }
        Err(error) => Err(with_notes(error, &notes)),
    }
}

#[cfg(feature = "storage-redb")]
#[allow(clippy::too_many_arguments)]
async fn sync_from_spool_redb(
    spool: &mut SnapshotSpool,
    ctx: CloudSyncContext,
    progress: &dyn CloudSyncProgress,
    d1_client: D1Client,
    r2_override: Option<RemoteStorage>,
    tuning: &SnapshotTuning,
    repo_id: String,
    storage: &std::path::Path,
) -> CloudResult<CloudSyncReport> {
    let page_size = tuning.page_size();
    spool
        .freeze(&storage.join(util::DATABASE), &repo_id, ctx.force, tuning)
        .await?;
    #[cfg(test)]
    if let Some(after_freeze) = &tuning.after_freeze {
        after_freeze(spool.dir().to_path_buf()).await;
    }

    // Ensure D1 table exists before any operations.
    d1_client
        .ensure_object_index_table()
        .await
        .map_err(|error| cloud_d1_failure("ensure object-index table", &error))?;

    let project_name = spool.repository()?.project_name;

    d1_client
        .ensure_repositories_table()
        .await
        .map_err(|error| cloud_d1_failure("ensure repositories table", &error))?;

    let object_format = crate::internal::object_format::as_str(git_internal::hash::get_hash_kind());

    let repo_row = d1_client
        .upsert_repository_with_format(&repo_id, &project_name, Some(object_format))
        .await
        .map_err(|error| repository_upsert_failure(&project_name, &error))?;

    if repo_row.repo_id != repo_id {
        return Err(project_name_taken(&project_name));
    }

    let r2_storage = match r2_override {
        Some(r2_storage) => r2_storage,
        None => create_r2_storage(&repo_id).await?,
    };

    let total_unsynced = usize::try_from(spool.object_count()).map_err(|_| {
        CloudError::Generic("the snapshot holds more objects than this platform can count".into())
    })?;

    if total_unsynced == 0 {
        spool.object_pages(page_size)?.next_page()?;
        progress.on_no_objects();
        let metadata = sync_metadata(&r2_storage, progress, &spool.refs(tuning)?).await?;
        let agent_capture = mirror_frozen_agent_capture_redb(
            storage,
            &d1_client,
            &r2_storage,
            &repo_id,
            progress,
            &frozen_agent_capture(spool),
        )
        .await;
        return Ok(CloudSyncReport {
            repo_id,
            project_name,
            total_unsynced: 0,
            synced_count: 0,
            failed_count: 0,
            metadata,
            agent_capture,
            spool_warnings: Vec::new(),
        });
    }

    progress.on_object_total(total_unsynced);

    let objects_path = path::objects();
    let local_storage = LocalStorage::try_new_with_alternates(objects_path)?;
    let git_store = crate::storage::runtime::open_git_store_read_write(storage)
        .await
        .map_err(|error| {
            CloudError::StorageIo(format!(
                "open git store to update local sync status: {error}"
            ))
        })?;

    let mut synced_count = 0usize;
    let mut failed_count = 0usize;

    let mut pages = spool.object_pages(page_size)?;
    while let Some(page) = pages.next_page()? {
        for batch in page.chunks(ctx.batch_size) {
            let parsed: Vec<CloudResult<ObjectHash>> =
                batch.iter().map(parse_object_index_hash).collect();
            let probe_hashes: Vec<ObjectHash> = parsed
                .iter()
                .filter_map(|r| r.as_ref().ok().copied())
                .collect();
            let already_in_remote: std::collections::HashSet<ObjectHash> = {
                let flags = r2_storage.exist_batch(&probe_hashes).await;
                probe_hashes
                    .iter()
                    .copied()
                    .zip(flags)
                    .filter_map(|(hash, exists)| exists.then_some(hash))
                    .collect()
            };

            for (obj, hash_result) in batch.iter().zip(parsed) {
                let result = match hash_result {
                    Ok(hash) => {
                        let remote_has = already_in_remote.contains(&hash);
                        sync_single_object(
                            obj,
                            &local_storage,
                            &r2_storage,
                            &d1_client,
                            hash,
                            remote_has,
                        )
                        .await
                    }
                    Err(err) => Err(err),
                };

                match result {
                    Ok(_) => match mark_frozen_object_synced_redb(git_store.as_ref(), obj, tuning)
                        .await
                    {
                        Ok(()) => synced_count += 1,
                        Err(e) => {
                            progress.on_local_status_warning(&obj.o_id, &e);
                            failed_count += 1;
                        }
                    },
                    Err(e) => {
                        let err = e.to_string();
                        progress.on_object_error(&obj.o_id, &err);
                        failed_count += 1;
                    }
                }
            }
            progress.on_batch_progress(synced_count, total_unsynced, failed_count);
        }
    }

    drop(git_store);
    progress.on_sync_complete(synced_count, failed_count);

    if failed_count > 0 {
        return Ok(CloudSyncReport {
            repo_id,
            project_name,
            total_unsynced,
            synced_count,
            failed_count,
            metadata: MetadataSyncOutcome::NotRun,
            agent_capture: AgentCaptureSyncOutcome::NotRun,
            spool_warnings: Vec::new(),
        });
    }

    let metadata = sync_metadata(&r2_storage, progress, &spool.refs(tuning)?).await?;
    let agent_capture = mirror_frozen_agent_capture_redb(
        storage,
        &d1_client,
        &r2_storage,
        &repo_id,
        progress,
        &frozen_agent_capture(spool),
    )
    .await;

    Ok(CloudSyncReport {
        repo_id,
        project_name,
        total_unsynced,
        synced_count,
        failed_count,
        metadata,
        agent_capture,
        spool_warnings: Vec::new(),
    })
}

#[cfg(feature = "storage-redb")]
async fn mirror_frozen_agent_capture_redb(
    storage: &std::path::Path,
    d1_client: &D1Client,
    r2_storage: &RemoteStorage,
    repo_id: &str,
    progress: &dyn CloudSyncProgress,
    frozen: &super::agent_capture::FrozenAgentCapture,
) -> AgentCaptureSyncOutcome {
    match super::agent_capture::sync_frozen_agent_capture_tables_redb(
        storage, d1_client, r2_storage, repo_id, progress, frozen,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            let reason = agent_capture_sync_failure_reason(&error);
            progress.on_agent_capture_warning(&reason);
            AgentCaptureSyncOutcome::Failed { error: reason }
        }
    }
}

#[cfg(feature = "storage-redb")]
async fn mark_frozen_object_synced_redb(
    git_store: &dyn crate::storage::git_store::GitStore,
    obj: &FrozenObject,
    tuning: &SnapshotTuning,
) -> Result<(), String> {
    #[cfg(test)]
    if tuning.fail_status_update {
        return Err("injected local status update failure".to_string());
    }
    #[cfg(not(test))]
    let _ = tuning;
    let existing = git_store
        .get_object_index(&obj.repo_id, &obj.o_id)
        .await
        .map_err(|error| error.to_string())?;
    let Some(row) = existing else {
        return Err(format!(
            "expected unchanged local object_index row for frozen id {}, row not found",
            obj.id
        ));
    };
    if row.o_type != obj.o_type || row.o_size != obj.o_size || row.created_at != obj.created_at {
        return Err(format!(
            "local object_index row for frozen id {} changed after snapshot freeze",
            obj.id
        ));
    }
    if row.is_synced {
        return Ok(());
    }
    let updated = git_store
        .mark_object_index_synced(&obj.repo_id, std::slice::from_ref(&obj.o_id))
        .await
        .map_err(|error| error.to_string())?;
    match updated {
        1 => Ok(()),
        rows => Err(format!(
            "expected to mark exactly one unchanged local object_index row for frozen id {}, updated {rows}",
            obj.id
        )),
    }
}

/// The phases of one sync run that read from `spool`. The caller owns the
/// spool's removal.
#[allow(clippy::too_many_arguments)]
async fn sync_from_spool(
    spool: &mut SnapshotSpool,
    ctx: CloudSyncContext,
    progress: &dyn CloudSyncProgress,
    d1_client: D1Client,
    r2_override: Option<RemoteStorage>,
    tuning: &SnapshotTuning,
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: String,
    storage: &std::path::Path,
) -> CloudResult<CloudSyncReport> {
    let page_size = tuning.page_size();
    spool
        .freeze(&storage.join(util::DATABASE), &repo_id, ctx.force, tuning)
        .await?;
    #[cfg(test)]
    if let Some(after_freeze) = &tuning.after_freeze {
        after_freeze(spool.dir().to_path_buf()).await;
    }

    // Ensure D1 table exists before any operations.
    d1_client
        .ensure_object_index_table()
        .await
        .map_err(|error| cloud_d1_failure("ensure object-index table", &error))?;

    // Project name: the `cloud.name` frozen with the snapshot, or the
    // working directory name.
    let project_name = spool.repository()?.project_name;

    // Ensure repositories table exists.
    d1_client
        .ensure_repositories_table()
        .await
        .map_err(|error| cloud_d1_failure("ensure repositories table", &error))?;

    // Persist the repository object-format alongside the backup catalog so
    // restore can read an authoritative kind (B3-09 storage axis; B3-14
    // consumes it fail-closed).
    let object_format = crate::internal::object_format::as_str(git_internal::hash::get_hash_kind());

    // Upsert repository info.
    let repo_row = d1_client
        .upsert_repository_with_format(&repo_id, &project_name, Some(object_format))
        .await
        .map_err(|error| repository_upsert_failure(&project_name, &error))?;

    // Verify repo_id matches (to detect name conflict).
    if repo_row.repo_id != repo_id {
        return Err(project_name_taken(&project_name));
    }

    // Initialize R2 storage.
    let r2_storage = match r2_override {
        Some(r2_storage) => r2_storage,
        None => create_r2_storage(&repo_id).await?,
    };

    let total_unsynced = usize::try_from(spool.object_count()).map_err(|_| {
        CloudError::Generic("the snapshot holds more objects than this platform can count".into())
    })?;

    if total_unsynced == 0 {
        // Still read the object spool: with nothing frozen, any record in it
        // is a read failure.
        spool.object_pages(page_size)?.next_page()?;
        progress.on_no_objects();
        let metadata = sync_metadata(&r2_storage, progress, &spool.refs(tuning)?).await?;
        // CEX-EntireIO §10.2: even when there are no new git objects to
        // ship, the agent_session/agent_checkpoint catalog may have new
        // rows from local hook ingestion. Mirror them on every sync.
        let agent_capture = mirror_frozen_agent_capture(
            db_conn,
            &d1_client,
            &r2_storage,
            &repo_id,
            progress,
            &frozen_agent_capture(spool),
        )
        .await;
        return Ok(CloudSyncReport {
            repo_id,
            project_name,
            total_unsynced: 0,
            synced_count: 0,
            failed_count: 0,
            metadata,
            agent_capture,
            spool_warnings: Vec::new(),
        });
    }

    progress.on_object_total(total_unsynced);

    // Initialize local storage for reading objects.
    let objects_path = path::objects();
    let local_storage = LocalStorage::try_new_with_alternates(objects_path)?;

    let mut synced_count = 0usize;
    let mut failed_count = 0usize;

    // Read the frozen identities a page at a time, then process each page in
    // batches.
    let mut pages = spool.object_pages(page_size)?;
    while let Some(page) = pages.next_page()? {
        for batch in page.chunks(ctx.batch_size) {
            // Parse the batch's hashes once, then run ONE bounded-concurrency
            // dedup pre-check (`exist_batch`, lore.md §0.6) instead of a HEAD
            // per object, so objects already in R2 are skipped without a
            // serial round-trip each.
            let parsed: Vec<CloudResult<ObjectHash>> =
                batch.iter().map(parse_object_index_hash).collect();
            let probe_hashes: Vec<ObjectHash> = parsed
                .iter()
                .filter_map(|r| r.as_ref().ok().copied())
                .collect();
            let already_in_remote: std::collections::HashSet<ObjectHash> = {
                let flags = r2_storage.exist_batch(&probe_hashes).await;
                probe_hashes
                    .iter()
                    .copied()
                    .zip(flags)
                    .filter_map(|(hash, exists)| exists.then_some(hash))
                    .collect()
            };

            for (obj, hash_result) in batch.iter().zip(parsed) {
                let result = match hash_result {
                    Ok(hash) => {
                        let remote_has = already_in_remote.contains(&hash);
                        sync_single_object(
                            obj,
                            &local_storage,
                            &r2_storage,
                            &d1_client,
                            hash,
                            remote_has,
                        )
                        .await
                    }
                    Err(err) => Err(err),
                };

                match result {
                    Ok(_) => {
                        // Mark only this frozen identity as synced. A failed
                        // local update leaves it unsynced, so it counts as a
                        // failure rather than a synced object.
                        match mark_frozen_object_synced(db_conn, obj, tuning).await {
                            Ok(()) => synced_count += 1,
                            Err(e) => {
                                progress.on_local_status_warning(&obj.o_id, &e);
                                failed_count += 1;
                            }
                        }
                    }
                    Err(e) => {
                        let err = e.to_string();
                        progress.on_object_error(&obj.o_id, &err);
                        failed_count += 1;
                    }
                }
            }
            progress.on_batch_progress(synced_count, total_unsynced, failed_count);
        }
    }

    progress.on_sync_complete(synced_count, failed_count);

    if failed_count > 0 {
        return Ok(CloudSyncReport {
            repo_id,
            project_name,
            total_unsynced,
            synced_count,
            failed_count,
            metadata: MetadataSyncOutcome::NotRun,
            agent_capture: AgentCaptureSyncOutcome::NotRun,
            spool_warnings: Vec::new(),
        });
    }

    let metadata = sync_metadata(&r2_storage, progress, &spool.refs(tuning)?).await?;
    // CEX-EntireIO §10.2: append agent capture catalog mirroring at the
    // tail of the sync flow per the plan. The report retains the detailed
    // phase outcome; `execute_sync` turns a failed mirror into a non-zero,
    // actionable partial-transfer result after rendering progress.
    let agent_capture = mirror_frozen_agent_capture(
        db_conn,
        &d1_client,
        &r2_storage,
        &repo_id,
        progress,
        &frozen_agent_capture(spool),
    )
    .await;

    Ok(CloudSyncReport {
        repo_id,
        project_name,
        total_unsynced,
        synced_count,
        failed_count,
        metadata,
        agent_capture,
        spool_warnings: Vec::new(),
    })
}

/// The agent-capture catalog frozen in `spool` (PI-26); a spool that cannot
/// be read back fails the mirror phase like an unreadable catalog, with a
/// fixed reason (the detail, which names the run directory, is logged).
fn frozen_agent_capture(spool: &SnapshotSpool) -> super::agent_capture::FrozenAgentCapture {
    spool.agent_capture().unwrap_or_else(|error| {
        tracing::warn!(error = %error, "could not read the frozen agent-capture catalog");
        super::agent_capture::FrozenAgentCapture::Unreadable {
            reason: FROZEN_CATALOG_UNREADABLE.to_string(),
        }
    })
}

/// Reason reported when the frozen agent-capture catalog cannot be read
/// back from this sync's spool.
pub(super) const FROZEN_CATALOG_UNREADABLE: &str = "the agent-capture catalog frozen in this sync's snapshot spool could not be read back; retry `libra cloud sync`";

/// Freeze the live catalog and run the mirror phase on it (tests).
#[cfg(test)]
pub(super) async fn mirror_agent_capture(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    r2_storage: &RemoteStorage,
    repo_id: &str,
    progress: &dyn CloudSyncProgress,
) -> AgentCaptureSyncOutcome {
    let frozen = match super::agent_capture::local_capture_catalog(db_conn).await {
        Ok(catalog) => super::agent_capture::freeze_agent_capture(&catalog, None).await,
        Err(error) => super::agent_capture::FrozenAgentCapture::Unreadable {
            reason: error.to_string(),
        },
    };
    mirror_frozen_agent_capture(db_conn, d1_client, r2_storage, repo_id, progress, &frozen).await
}

/// Run the agent-capture mirror phase. This is the single place a phase
/// failure is reported: exactly one fixed `agent_capture` warning carrying the
/// content-free reason that the report (and the final error) also carries.
pub(super) async fn mirror_frozen_agent_capture(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    r2_storage: &RemoteStorage,
    repo_id: &str,
    progress: &dyn CloudSyncProgress,
    frozen: &super::agent_capture::FrozenAgentCapture,
) -> AgentCaptureSyncOutcome {
    match super::agent_capture::sync_frozen_agent_capture_tables(
        db_conn, d1_client, r2_storage, repo_id, progress, frozen,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            let reason = agent_capture_sync_failure_reason(&error);
            progress.on_agent_capture_warning(&reason);
            AgentCaptureSyncOutcome::Failed { error: reason }
        }
    }
}

/// Set `is_synced` for one frozen identity, matched by both `id` and `o_id`.
async fn mark_frozen_object_synced(
    db_conn: &sea_orm::DatabaseConnection,
    obj: &FrozenObject,
    tuning: &SnapshotTuning,
) -> Result<(), String> {
    #[cfg(test)]
    if tuning.fail_status_update {
        return Err("injected local status update failure".to_string());
    }
    #[cfg(not(test))]
    let _ = tuning;
    let result = object_index::Entity::update_many()
        .col_expr(object_index::Column::IsSynced, Expr::value(1))
        .filter(object_index::Column::Id.eq(obj.id))
        .filter(object_index::Column::OId.eq(obj.o_id.as_str()))
        .filter(object_index::Column::RepoId.eq(obj.repo_id.as_str()))
        // A row whose uploaded fields changed since the freeze (for example a
        // promoted `o_type`) is not the identity that was uploaded.
        .filter(object_index::Column::OType.eq(obj.o_type.as_str()))
        .filter(object_index::Column::OSize.eq(obj.o_size))
        .filter(object_index::Column::CreatedAt.eq(obj.created_at))
        .exec(db_conn)
        .await
        .map_err(|error| error.to_string())?;
    // The frozen row may have been deleted or replaced since the freeze.
    match result.rows_affected {
        1 => Ok(()),
        rows => Err(format!(
            "expected to mark exactly one unchanged local object_index row for frozen id {}, updated {rows}",
            obj.id
        )),
    }
}

/// Parse a frozen identity's hex `o_id` into an `ObjectHash`.
fn parse_object_index_hash(obj: &FrozenObject) -> CloudResult<ObjectHash> {
    let bytes =
        hex::decode(&obj.o_id).map_err(|e| CloudError::Generic(format!("Invalid hash: {}", e)))?;
    ObjectHash::from_bytes_for_kind(git_internal::hash::get_hash_kind(), &bytes)
        .map_err(|e| CloudError::Generic(format!("Invalid object hash: {}", e)))
}

/// Sync a single object: R2 first (idempotent), then D1.
///
/// `remote_has` is the result of the batch dedup pre-check (`exist_batch`,
/// lore.md §0.6), so this no longer issues a per-object HEAD — the whole batch's
/// existence is probed up front in one bounded-concurrency call.
async fn sync_single_object(
    obj: &FrozenObject,
    local_storage: &LocalStorage,
    r2_storage: &RemoteStorage,
    d1_client: &D1Client,
    hash: ObjectHash,
    remote_has: bool,
) -> CloudResult<()> {
    // Phase 1: Upload to R2 only if the dedup pre-check says it is absent
    // (idempotent - same hash would just overwrite).
    if !remote_has {
        let (data, obj_type) = local_storage
            .get(&hash)
            .await
            .map_err(|e| CloudError::Generic(format!("Failed to read local object: {}", e)))?;

        r2_storage
            .put(&hash, &data, obj_type)
            .await
            .map_err(|e| CloudError::R2(format!("R2 upload failed: {}", e)))?;
    }

    // Phase 2: Upsert to D1 (idempotent - will update if exists). Tag the
    // row with the repository object-format so consumers never infer kind
    // from `o_id` width (B3-09 storage; B3-14 semantics).
    let object_format = crate::internal::object_format::as_str(git_internal::hash::get_hash_kind());
    d1_client
        .upsert_object_index_with_format(
            &obj.o_id,
            &obj.o_type,
            obj.o_size,
            &obj.repo_id,
            obj.created_at,
            Some(object_format),
        )
        .await
        .map_err(|error| cloud_d1_failure("write object index", &error))?;

    Ok(())
}

fn project_name_taken(project_name: &str) -> CloudError {
    CloudError::NameAlreadyTaken(format!(
        "Project name '{project_name}' is already taken by another repository. Please choose a different name in cloud.name config."
    ))
}

/// A concurrent registration of the same project name surfaces as the remote
/// UNIQUE constraint. Classify it as the same name conflict as the post-upsert
/// check, using only the locally configured name; every other failure keeps
/// the redacted D1 diagnostic.
fn repository_upsert_failure(project_name: &str, error: &D1Error) -> CloudError {
    if error
        .message
        .contains("UNIQUE constraint failed: repositories.name")
    {
        project_name_taken(project_name)
    } else {
        cloud_d1_failure("upsert repository", error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_project_name_collision_stays_a_redacted_name_conflict() {
        const REMOTE_SENTINEL: &str = "REMOTE_D1_ROW_SENTINEL";
        let collision = repository_upsert_failure(
            "demo",
            &D1Error {
                code: 7500,
                message: format!("UNIQUE constraint failed: repositories.name {REMOTE_SENTINEL}"),
            },
        );
        match &collision {
            CloudError::NameAlreadyTaken(detail) => {
                assert!(detail.contains("'demo'"), "{detail}");
                assert!(!detail.contains(REMOTE_SENTINEL), "{detail}");
            }
            other => panic!("UNIQUE name collision must stay a name conflict: {other:?}"),
        }
        // Both name-conflict paths keep the shipped text and stable code.
        assert_eq!(collision, project_name_taken("demo"));
        assert_eq!(
            collision.to_string(),
            "Project name 'demo' is already taken by another repository. Please choose a different name in cloud.name config."
        );
        assert_eq!(
            collision.into_cli_error("sync").stable_code(),
            crate::utils::error::StableErrorCode::ConflictOperationBlocked
        );
        let other = repository_upsert_failure(
            "demo",
            &D1Error {
                code: 7500,
                message: REMOTE_SENTINEL.to_string(),
            },
        );
        match &other {
            CloudError::D1(detail) => assert!(!detail.contains(REMOTE_SENTINEL), "{detail}"),
            other => panic!("other upsert failures stay redacted D1 errors: {other:?}"),
        }
        assert_eq!(
            other.into_cli_error("sync").stable_code(),
            crate::utils::error::StableErrorCode::NetworkProtocol
        );
    }
}
