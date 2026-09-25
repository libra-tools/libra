use super::{metadata::sync_metadata, *};

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
/// "block follow-up work on object failure" gate).
pub(crate) async fn run_cloud_sync(
    ctx: CloudSyncContext,
    progress: &dyn CloudSyncProgress,
) -> CloudResult<CloudSyncReport> {
    if ctx.batch_size < 1 {
        return Err(CloudError::Generic(
            "Batch size must be at least 1".to_string(),
        ));
    }

    progress.on_starting();

    validate_cloud_backup_env(false).await?;

    // Initialize D1 client.
    let d1_client = D1Client::from_env()
        .await
        .map_err(|e| CloudError::D1(format!("D1 client error: {}", e.message)))?;

    // Ensure D1 table exists before any operations.
    d1_client
        .ensure_object_index_table()
        .await
        .map_err(|e| CloudError::D1(format!("Failed to create D1 table: {}", e.message)))?;

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

    // Determine project name from config 'cloud.name' or current directory name.
    let project_name = ConfigKv::get("cloud.name")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .unwrap_or_else(|| {
            util::working_dir()
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown-project".to_string())
        });

    // Ensure repositories table exists.
    d1_client.ensure_repositories_table().await.map_err(|e| {
        CloudError::D1(format!(
            "Failed to create repositories table: {}",
            e.message
        ))
    })?;

    // Upsert repository info.
    let repo_row = d1_client
        .upsert_repository(&repo_id, &project_name)
        .await
        .map_err(|e| {
            if e.message.contains("UNIQUE constraint failed: repositories.name") {
                CloudError::NameAlreadyTaken(format!(
                    "Project name '{}' is already taken by another repository. Please choose a different name in cloud.name config.",
                    project_name
                ))
            } else {
                CloudError::D1(format!("Failed to upsert repository: {}", e.message))
            }
        })?;

    // Verify repo_id matches (to detect name conflict).
    if repo_row.repo_id != repo_id {
        return Err(CloudError::NameAlreadyTaken(format!(
            "Project name '{}' is already taken by another repository (ID: {}). Please choose a different name in cloud.name config.",
            project_name, repo_row.repo_id
        )));
    }

    // Query unsynced objects.
    let query = if ctx.force {
        object_index::Entity::find().filter(object_index::Column::RepoId.eq(&repo_id))
    } else {
        object_index::Entity::find()
            .filter(object_index::Column::RepoId.eq(&repo_id))
            .filter(object_index::Column::IsSynced.eq(0))
    };

    let unsynced_objects = query
        .all(&db_conn)
        .await
        .map_err(|e| CloudError::Generic(format!("Database query failed: {}", e)))?;

    // Initialize R2 storage.
    let r2_storage = create_r2_storage(&repo_id).await?;

    let total_unsynced = unsynced_objects.len();

    if unsynced_objects.is_empty() {
        progress.on_no_objects();
        let metadata = sync_metadata(&db_conn, &r2_storage, progress).await?;
        // CEX-EntireIO §10.2: even when there are no new git objects to
        // ship, the agent_session/agent_checkpoint catalog may have new
        // rows from local hook ingestion. Mirror them on every sync.
        let agent_capture =
            match sync_agent_capture_tables(&db_conn, &d1_client, &r2_storage, &repo_id, progress)
                .await
            {
                Ok(outcome) => outcome,
                Err(err) => {
                    let err = err.to_string();
                    progress.on_agent_capture_warning(&err);
                    AgentCaptureSyncOutcome::Failed { error: err }
                }
            };
        return Ok(CloudSyncReport {
            repo_id,
            project_name,
            total_unsynced: 0,
            synced_count: 0,
            failed_count: 0,
            metadata,
            agent_capture,
        });
    }

    progress.on_object_total(total_unsynced);

    // Initialize local storage for reading objects.
    let objects_path = path::objects();
    let local_storage = LocalStorage::new(objects_path);

    let mut synced_count = 0usize;
    let mut failed_count = 0usize;

    // Process in batches.
    for batch in unsynced_objects.chunks(ctx.batch_size) {
        // Parse the batch's hashes once, then run ONE bounded-concurrency dedup
        // pre-check (`exist_batch`, lore.md §0.6) instead of a HEAD per object, so
        // objects already in R2 are skipped without a serial round-trip each.
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
                    // Update local is_synced flag.
                    let mut active: object_index::ActiveModel = obj.clone().into();
                    active.is_synced = Set(1);
                    if let Err(e) = active.update(&db_conn).await {
                        progress.on_local_status_warning(&obj.o_id, &e.to_string());
                    }
                    synced_count += 1;
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
        });
    }

    let metadata = sync_metadata(&db_conn, &r2_storage, progress).await?;
    // CEX-EntireIO §10.2: append agent capture catalog mirroring at the
    // tail of the sync flow per the plan. The report retains the detailed
    // phase outcome; `execute_sync` turns a failed mirror into a non-zero,
    // actionable partial-transfer result after rendering progress.
    let agent_capture = match sync_agent_capture_tables(
        &db_conn,
        &d1_client,
        &r2_storage,
        &repo_id,
        progress,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(err) => {
            let err = err.to_string();
            progress.on_agent_capture_warning(&err);
            AgentCaptureSyncOutcome::Failed { error: err }
        }
    };

    Ok(CloudSyncReport {
        repo_id,
        project_name,
        total_unsynced,
        synced_count,
        failed_count,
        metadata,
        agent_capture,
    })
}

/// Parse an `object_index` model's hex `o_id` into an `ObjectHash`.
fn parse_object_index_hash(obj: &object_index::Model) -> CloudResult<ObjectHash> {
    let bytes =
        hex::decode(&obj.o_id).map_err(|e| CloudError::Generic(format!("Invalid hash: {}", e)))?;
    ObjectHash::from_bytes(&bytes)
        .map_err(|e| CloudError::Generic(format!("Invalid object hash: {}", e)))
}

/// Sync a single object: R2 first (idempotent), then D1.
///
/// `remote_has` is the result of the batch dedup pre-check (`exist_batch`,
/// lore.md §0.6), so this no longer issues a per-object HEAD — the whole batch's
/// existence is probed up front in one bounded-concurrency call.
async fn sync_single_object(
    obj: &object_index::Model,
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

    // Phase 2: Upsert to D1 (idempotent - will update if exists)
    d1_client
        .upsert_object_index(
            &obj.o_id,
            &obj.o_type,
            obj.o_size,
            &obj.repo_id,
            obj.created_at,
        )
        .await
        .map_err(|e| CloudError::D1(format!("D1 write failed: {}", e.message)))?;

    Ok(())
}
