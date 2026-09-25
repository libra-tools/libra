use super::*;

const AGENT_CAPTURE_LOCAL_PAGE_SIZE: usize = 256;
pub(super) const AGENT_CAPTURE_MAX_ROWS_PER_TABLE: usize = 100_000;
pub(super) const AGENT_CAPTURE_RESTORE_MAX_ROWS: usize = 100_000;
const AGENT_CAPTURE_D1_BATCH_SIZE: usize = 128;
const AGENT_CAPTURE_OBJECT_VERIFY_CONCURRENCY: usize = 32;
pub(super) const AGENT_CAPTURE_CLOUD_DEADLINE: std::time::Duration =
    std::time::Duration::from_secs(120);

pub(super) fn agent_capture_batches<T>(rows: &[T]) -> std::slice::Chunks<'_, T> {
    rows.chunks(AGENT_CAPTURE_D1_BATCH_SIZE)
}

pub(super) fn agent_capture_object_verification_batches<T>(
    rows: &[T],
) -> std::slice::Chunks<'_, T> {
    rows.chunks(AGENT_CAPTURE_OBJECT_VERIFY_CONCURRENCY)
}

pub(super) async fn load_local_agent_capture_cloud_base(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
) -> CloudResult<Option<i64>> {
    use sea_orm::Statement;

    let backend = db_conn.get_database_backend();
    let table_present = db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'agent_capture_cloud_base' LIMIT 1",
            [],
        ))
        .await
        .map_err(|error| {
            CloudError::Generic(format!("probe local agent-capture cloud base: {error}"))
        })?
        .is_some();
    if !table_present {
        return Ok(None);
    }
    db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT remote_generation FROM agent_capture_cloud_base WHERE repo_id = ?",
            [repo_id.into()],
        ))
        .await
        .map_err(|error| {
            CloudError::Generic(format!("read local agent-capture cloud base: {error}"))
        })?
        .map(|row| {
            row.try_get_by("remote_generation").map_err(|error| {
                CloudError::Generic(format!("decode local agent-capture cloud base: {error}"))
            })
        })
        .transpose()
}

pub(super) async fn store_local_agent_capture_cloud_base(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    remote_generation: i64,
) -> CloudResult<()> {
    use sea_orm::Statement;

    db_conn
        .execute_raw(Statement::from_sql_and_values(
            db_conn.get_database_backend(),
            "INSERT INTO agent_capture_cloud_base (repo_id, remote_generation, updated_at)
             VALUES (?, ?, ?)
             ON CONFLICT(repo_id) DO UPDATE SET
                remote_generation = excluded.remote_generation,
                updated_at = excluded.updated_at
             WHERE excluded.remote_generation > agent_capture_cloud_base.remote_generation",
            [
                repo_id.into(),
                remote_generation.into(),
                chrono::Utc::now().timestamp_millis().into(),
            ],
        ))
        .await
        .map_err(|error| {
            CloudError::Generic(format!("record local agent-capture cloud base: {error}"))
        })?;
    Ok(())
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct AgentCaptureSnapshot {
    pub(super) sessions: Vec<AgentSessionV2Row>,
    pub(super) checkpoints: Vec<AgentCheckpointV2Row>,
    pub(super) claims: Vec<AgentSubagentContentClaimRow>,
    pub(super) revisions: Vec<AgentSubagentContentRevisionRow>,
    pub(super) links: Vec<AgentSubagentLinkRow>,
    pub(super) prune_tombstones: Vec<AgentCheckpointPruneTombstoneRow>,
    /// PD-03 session-erasure tombstones from the local
    /// `agent_import_tombstone` table.
    pub(super) import_tombstones: Vec<AgentImportTombstoneRow>,
    pub(super) required_oids: HashSet<String>,
    pub(super) traces_head: Option<String>,
}

/// PD-03: union local and remote session tombstones, keeping the newest
/// `erased_at` (and any known fingerprint) per provider identity —
/// delete/restore replays stay idempotent.
pub(super) fn merge_import_tombstones(
    local: &[AgentImportTombstoneRow],
    remote: &[AgentImportTombstoneRow],
) -> Vec<AgentImportTombstoneRow> {
    let mut merged: std::collections::BTreeMap<(String, String), AgentImportTombstoneRow> =
        std::collections::BTreeMap::new();
    for row in remote.iter().chain(local.iter()) {
        let key = (row.agent_kind.clone(), row.provider_session_id.clone());
        match merged.get_mut(&key) {
            None => {
                merged.insert(key, row.clone());
            }
            Some(existing) => {
                if row.erased_at > existing.erased_at {
                    let fingerprint = existing
                        .source_fingerprint
                        .clone()
                        .or_else(|| row.source_fingerprint.clone());
                    *existing = row.clone();
                    existing.source_fingerprint = row.source_fingerprint.clone().or(fingerprint);
                } else if existing.source_fingerprint.is_none() {
                    existing.source_fingerprint = row.source_fingerprint.clone();
                }
            }
        }
    }
    merged.into_values().collect()
}

fn agent_capture_catalog_row_count(snapshot: &AgentCaptureSnapshot) -> CloudResult<usize> {
    [
        snapshot.sessions.len(),
        snapshot.checkpoints.len(),
        snapshot.prune_tombstones.len(),
        snapshot.import_tombstones.len(),
        snapshot.claims.len(),
        snapshot.revisions.len(),
        snapshot.links.len(),
    ]
    .into_iter()
    .try_fold(0_usize, |total, count| {
        total.checked_add(count).ok_or_else(|| {
            CloudError::PartialTransfer(
                "agent-capture catalog row count exceeds the platform size range".to_string(),
            )
        })
    })
}

fn validate_agent_capture_restore_row_budget(
    snapshot: &AgentCaptureSnapshot,
    object_index_rows: usize,
) -> CloudResult<()> {
    let total = agent_capture_catalog_row_count(snapshot)?
        .checked_add(object_index_rows)
        .ok_or_else(|| {
            CloudError::PartialTransfer(
                "agent-capture restore row count exceeds the platform size range".to_string(),
            )
        })?;
    if total > AGENT_CAPTURE_RESTORE_MAX_ROWS {
        return Err(CloudError::PartialTransfer(format!(
            "agent-capture catalog and object manifest require {total} rows, exceeding the aggregate {}-row restore safety bound",
            AGENT_CAPTURE_RESTORE_MAX_ROWS
        )));
    }
    Ok(())
}

pub(super) struct AgentCaptureRestoreRows<'a> {
    pub(super) sessions: &'a [AgentSessionV2Row],
    pub(super) checkpoints: &'a [AgentCheckpointV2Row],
    pub(super) claims: &'a [AgentSubagentContentClaimRow],
    pub(super) revisions: &'a [AgentSubagentContentRevisionRow],
    pub(super) links: &'a [AgentSubagentLinkRow],
    pub(super) traces_head: Option<&'a str>,
    /// Numeric row revisions are meaningful only when this clone recorded the
    /// completed remote generation as its base. Without that lineage proof, a
    /// larger local counter may be an unrelated clone's divergent history.
    pub(super) remote_is_known_ancestor: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentCaptureObjectManifestScope {
    CheckpointProjection,
    FullRemoteIndex,
}

impl AgentCaptureObjectManifestScope {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::CheckpointProjection => "checkpoint_projection",
            Self::FullRemoteIndex => "full_remote_index",
        }
    }

    pub(super) fn parse(value: Option<&str>) -> CloudResult<Self> {
        match value {
            Some("checkpoint_projection") => Ok(Self::CheckpointProjection),
            Some("full_remote_index") => Ok(Self::FullRemoteIndex),
            _ => Err(CloudError::PartialTransfer(
                "agent-capture generation has no supported object-index scope; run a current-version cloud sync"
                    .to_string(),
            )),
        }
    }
}

pub(super) fn agent_capture_object_index_digest(
    indexes: &[ObjectIndexRow],
) -> CloudResult<(String, i64)> {
    let mut rows = indexes.iter().collect::<Vec<_>>();
    rows.sort_by(|left, right| left.o_id.cmp(&right.o_id));
    let mut previous: Option<&str> = None;
    let mut digest = Sha256::new();
    for row in rows {
        if previous == Some(row.o_id.as_str()) {
            return Err(CloudError::Generic(format!(
                "remote object index contains duplicate oid {}",
                row.o_id
            )));
        }
        previous = Some(row.o_id.as_str());
        for value in [row.o_id.as_bytes(), row.o_type.as_bytes()] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value);
        }
        digest.update(row.o_size.to_be_bytes());
    }
    let count = i64::try_from(indexes.len()).map_err(|error| {
        CloudError::Generic(format!("object-index count cannot be represented: {error}"))
    })?;
    Ok((hex::encode(digest.finalize()), count))
}

pub(super) fn validate_checkpoint_object_index_roots(
    checkpoints: &[AgentCheckpointV2Row],
    indexes: &[ObjectIndexRow],
    side: &str,
) -> CloudResult<()> {
    let indexed_oids = indexes
        .iter()
        .map(|row| row.o_id.as_str())
        .collect::<HashSet<_>>();
    for checkpoint in checkpoints {
        for (label, oid) in [
            ("traces commit", checkpoint.traces_commit.as_str()),
            ("tree", checkpoint.tree_oid.as_str()),
            ("metadata blob", checkpoint.metadata_blob_oid.as_str()),
        ] {
            if !indexed_oids.contains(oid) {
                return Err(CloudError::PartialTransfer(format!(
                    "{side} checkpoint {} references {label} object {oid}, but the fenced object index does not contain it",
                    checkpoint.checkpoint_id
                )));
            }
        }
    }
    Ok(())
}

pub(super) async fn load_local_capture_pages<C: ConnectionTrait>(
    conn: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
    label: &str,
    remaining_rows: &mut usize,
) -> CloudResult<Vec<sea_orm::QueryResult>> {
    let mut rows = Vec::new();
    let mut offset = 0_usize;
    loop {
        // Read one sentinel row beyond the shared remaining budget so an
        // aggregate overflow fails before the generation can be advertised.
        let page_limit = AGENT_CAPTURE_LOCAL_PAGE_SIZE.min(remaining_rows.saturating_add(1));
        let mut page_values = values.clone();
        page_values.extend([
            i64::try_from(page_limit)
                .map_err(|error| CloudError::Generic(format!("encode {label} page size: {error}")))?
                .into(),
            i64::try_from(offset)
                .map_err(|error| {
                    CloudError::Generic(format!("encode {label} page offset: {error}"))
                })?
                .into(),
        ]);
        let page = conn
            .query_all_raw(sea_orm::Statement::from_sql_and_values(
                conn.get_database_backend(),
                format!("{sql} LIMIT ? OFFSET ?"),
                page_values,
            ))
            .await
            .map_err(|error| CloudError::Generic(format!("query {label} page: {error}")))?;
        let page_len = page.len();
        if page_len > *remaining_rows {
            return Err(CloudError::PartialTransfer(format!(
                "local agent-capture catalog exceeds the aggregate {}-row restore safety bound while reading {label}",
                AGENT_CAPTURE_RESTORE_MAX_ROWS
            )));
        }
        *remaining_rows -= page_len;
        rows.extend(page);
        if page_len < page_limit {
            break;
        }
        offset = offset.saturating_add(page_len);
    }
    Ok(rows)
}

pub(super) async fn load_synced_required_object_oids<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    required_oids: &HashSet<String>,
) -> CloudResult<HashSet<String>> {
    if required_oids.len() > AGENT_CAPTURE_MAX_ROWS_PER_TABLE {
        return Err(CloudError::Generic(format!(
            "agent checkpoint reachability exceeds the {}-object cloud safety bound",
            AGENT_CAPTURE_MAX_ROWS_PER_TABLE
        )));
    }
    let mut required = required_oids.iter().cloned().collect::<Vec<_>>();
    required.sort();
    let mut synced = HashSet::with_capacity(required.len());
    for page in required.chunks(AGENT_CAPTURE_LOCAL_PAGE_SIZE) {
        let placeholders = vec!["?"; page.len()].join(", ");
        let mut values = Vec::with_capacity(page.len().saturating_add(1));
        values.push(repo_id.into());
        values.extend(page.iter().cloned().map(Into::into));
        let rows = conn
            .query_all_raw(sea_orm::Statement::from_sql_and_values(
                conn.get_database_backend(),
                format!(
                    "SELECT o_id FROM object_index
                     WHERE repo_id = ? AND is_synced = 1 AND o_id IN ({placeholders})"
                ),
                values,
            ))
            .await
            .map_err(|error| {
                CloudError::Generic(format!(
                    "query checkpoint-reachable synced object indexes: {error}"
                ))
            })?;
        for row in rows {
            synced.insert(row.try_get_by::<String, _>("o_id").map_err(|error| {
                CloudError::Generic(format!(
                    "decode checkpoint-reachable synced object index: {error}"
                ))
            })?);
        }
    }
    Ok(synced)
}

async fn load_required_local_object_indexes(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    required_oids: &HashSet<String>,
) -> CloudResult<HashMap<String, object_index::Model>> {
    if required_oids.len() > AGENT_CAPTURE_MAX_ROWS_PER_TABLE {
        return Err(CloudError::Generic(format!(
            "agent checkpoint reachability exceeds the {}-object cloud safety bound",
            AGENT_CAPTURE_MAX_ROWS_PER_TABLE
        )));
    }
    let mut required = required_oids.iter().cloned().collect::<Vec<_>>();
    required.sort();
    let mut rows = HashMap::with_capacity(required.len());
    for page in required.chunks(AGENT_CAPTURE_LOCAL_PAGE_SIZE) {
        let models = object_index::Entity::find()
            .filter(object_index::Column::RepoId.eq(repo_id))
            .filter(object_index::Column::OId.is_in(page.iter().cloned()))
            .all(db_conn)
            .await
            .map_err(|error| {
                CloudError::Generic(format!(
                    "load checkpoint-reachable local object indexes: {error}"
                ))
            })?;
        rows.extend(models.into_iter().map(|model| (model.o_id.clone(), model)));
    }
    Ok(rows)
}

async fn load_agent_capture_catalog_snapshot(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    subagent_content_present: bool,
) -> CloudResult<AgentCaptureSnapshot> {
    use sea_orm::Statement;

    let txn = db_conn
        .begin()
        .await
        .map_err(|error| CloudError::Generic(format!("begin agent capture snapshot: {error}")))?;
    let backend = txn.get_database_backend();
    let unsynced = txn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM object_index
             WHERE repo_id = ? AND COALESCE(is_synced, 0) = 0",
            [repo_id.into()],
        ))
        .await
        .map_err(|error| {
            CloudError::Generic(format!("verify agent capture object generation: {error}"))
        })?
        .ok_or_else(|| CloudError::Generic("object generation count returned no row".into()))?
        .try_get_by::<i64, _>("n")
        .map_err(|error| CloudError::Generic(format!("decode unsynced object count: {error}")))?;
    if unsynced != 0 {
        return Err(CloudError::PartialTransfer(format!(
            "agent capture snapshot found {unsynced} object(s) outside the completed object upload generation; retry `libra cloud sync`"
        )));
    }
    let mut remaining_restore_rows = AGENT_CAPTURE_RESTORE_MAX_ROWS;

    let session_rows = load_local_capture_pages(
        &txn,
        "SELECT session_id, agent_kind, provider_session_id, state, working_dir,
                worktree_id, parent_commit, parent_session_id, metadata_json,
                redaction_report, started_at, last_event_at, stopped_at, schema_version,
                sync_revision
         FROM agent_session ORDER BY session_id",
        Vec::new(),
        "agent session",
        &mut remaining_restore_rows,
    )
    .await?;
    let sessions: Vec<AgentSessionV2Row> = session_rows
        .into_iter()
        .map(|row| {
            Ok(AgentSessionV2Row {
                session_id: row.try_get_by("session_id")?,
                agent_kind: row.try_get_by("agent_kind")?,
                provider_session_id: row.try_get_by("provider_session_id")?,
                state: row.try_get_by("state")?,
                working_dir: row.try_get_by("working_dir")?,
                worktree_id: row.try_get_by("worktree_id")?,
                parent_commit: row.try_get_by("parent_commit")?,
                parent_session_id: row.try_get_by("parent_session_id")?,
                metadata_json: row.try_get_by("metadata_json")?,
                redaction_report: row.try_get_by("redaction_report")?,
                started_at: row.try_get_by("started_at")?,
                last_event_at: row.try_get_by("last_event_at")?,
                stopped_at: row.try_get_by("stopped_at")?,
                schema_version: row.try_get_by("schema_version")?,
                sync_revision: row.try_get_by("sync_revision")?,
            })
        })
        .collect::<Result<_, sea_orm::DbErr>>()
        .map_err(|error| CloudError::Generic(format!("decode agent session snapshot: {error}")))?;

    let checkpoint_rows = load_local_capture_pages(
        &txn,
        "SELECT checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                subagent_session_id, description, created_at, sync_revision
         FROM agent_checkpoint ORDER BY created_at, checkpoint_id",
        Vec::new(),
        "agent checkpoint",
        &mut remaining_restore_rows,
    )
    .await?;
    let checkpoints: Vec<AgentCheckpointV2Row> = checkpoint_rows
        .into_iter()
        .map(|row| {
            Ok(AgentCheckpointV2Row {
                checkpoint_id: row.try_get_by("checkpoint_id")?,
                session_id: row.try_get_by("session_id")?,
                parent_checkpoint_id: row.try_get_by("parent_checkpoint_id")?,
                scope: row.try_get_by("scope")?,
                parent_commit: row.try_get_by("parent_commit")?,
                tree_oid: row.try_get_by("tree_oid")?,
                metadata_blob_oid: row.try_get_by("metadata_blob_oid")?,
                traces_commit: row.try_get_by("traces_commit")?,
                tool_use_id: row.try_get_by("tool_use_id")?,
                subagent_session_id: row.try_get_by("subagent_session_id")?,
                description: row.try_get_by("description")?,
                created_at: row.try_get_by("created_at")?,
                sync_revision: row.try_get_by("sync_revision")?,
            })
        })
        .collect::<Result<_, sea_orm::DbErr>>()
        .map_err(|error| {
            CloudError::Generic(format!("decode agent checkpoint snapshot: {error}"))
        })?;

    let prune_tombstones = if subagent_content_present {
        let tombstone_rows = load_local_capture_pages(
            &txn,
            "SELECT checkpoint_id, session_id, pruned_at
             FROM agent_checkpoint_prune_tombstone ORDER BY checkpoint_id",
            Vec::new(),
            "checkpoint prune tombstone",
            &mut remaining_restore_rows,
        )
        .await?;
        tombstone_rows
            .into_iter()
            .map(|row| {
                Ok(AgentCheckpointPruneTombstoneRow {
                    checkpoint_id: row.try_get_by("checkpoint_id")?,
                    session_id: row.try_get_by("session_id")?,
                    pruned_at: row.try_get_by("pruned_at")?,
                })
            })
            .collect::<Result<Vec<_>, sea_orm::DbErr>>()
            .map_err(|error| CloudError::Generic(format!("decode prune tombstones: {error}")))?
    } else {
        Vec::new()
    };
    let traces_head = txn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT `commit` FROM reference
             WHERE name = ? AND kind = 'Branch' AND remote IS NULL LIMIT 1",
            [crate::internal::branch::TRACES_BRANCH.into()],
        ))
        .await
        .map_err(|error| CloudError::Generic(format!("resolve traces snapshot head: {error}")))?
        .map(|row| row.try_get_by::<Option<String>, _>("commit"))
        .transpose()
        .map_err(|error| CloudError::Generic(format!("decode traces snapshot head: {error}")))?
        .flatten();

    let import_tombstone_present = txn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'agent_import_tombstone' LIMIT 1"
                .to_string(),
        ))
        .await
        .map_err(|error| CloudError::Generic(format!("query import-tombstone schema: {error}")))?
        .is_some();
    let import_tombstones = if import_tombstone_present {
        let tombstone_rows = load_local_capture_pages(
            &txn,
            "SELECT agent_kind, provider_session_id, erased_session_id,
                    source_fingerprint, erased_at
             FROM agent_import_tombstone ORDER BY agent_kind, provider_session_id",
            Vec::new(),
            "agent import tombstone",
            &mut remaining_restore_rows,
        )
        .await?;
        tombstone_rows
            .into_iter()
            .map(|row| {
                Ok(AgentImportTombstoneRow {
                    agent_kind: row.try_get_by("agent_kind")?,
                    provider_session_id: row.try_get_by("provider_session_id")?,
                    erased_session_id: row.try_get_by("erased_session_id")?,
                    source_fingerprint: row.try_get_by("source_fingerprint")?,
                    erased_at: row.try_get_by("erased_at")?,
                })
            })
            .collect::<Result<Vec<_>, sea_orm::DbErr>>()
            .map_err(|error| CloudError::Generic(format!("decode import tombstones: {error}")))?
    } else {
        Vec::new()
    };
    let mut snapshot = AgentCaptureSnapshot {
        sessions,
        checkpoints,
        prune_tombstones,
        import_tombstones,
        traces_head,
        ..AgentCaptureSnapshot::default()
    };
    if subagent_content_present {
        let claim_rows = load_local_capture_pages(
            &txn,
            "SELECT parent_session_id, provider_kind, source_key,
                    content_schema_version, revision_cursor, sync_revision, current_revision,
                    current_checkpoint_id, current_digest, fence_token, created_at, updated_at
             FROM agent_subagent_content_claim
             ORDER BY parent_session_id, provider_kind, source_key, content_schema_version",
            Vec::new(),
            "subagent content claim",
            &mut remaining_restore_rows,
        )
        .await?;
        snapshot.claims = claim_rows
            .into_iter()
            .map(|row| {
                Ok(AgentSubagentContentClaimRow {
                    parent_session_id: row.try_get_by("parent_session_id")?,
                    provider_kind: row.try_get_by("provider_kind")?,
                    source_key: row.try_get_by("source_key")?,
                    content_schema_version: row.try_get_by("content_schema_version")?,
                    revision_cursor: row.try_get_by("revision_cursor")?,
                    sync_revision: row.try_get_by("sync_revision")?,
                    current_revision: row.try_get_by("current_revision")?,
                    current_checkpoint_id: row.try_get_by("current_checkpoint_id")?,
                    current_digest: row.try_get_by("current_digest")?,
                    fence_token: row.try_get_by("fence_token")?,
                    created_at: row.try_get_by("created_at")?,
                    updated_at: row.try_get_by("updated_at")?,
                })
            })
            .collect::<Result<_, sea_orm::DbErr>>()
            .map_err(|error| {
                CloudError::Generic(format!("decode subagent claim snapshot: {error}"))
            })?;
        let revision_rows = load_local_capture_pages(
            &txn,
            "SELECT parent_session_id, provider_kind, source_key,
                    content_schema_version, revision, checkpoint_id, content_digest,
                    source_channel, partial, created_at
             FROM agent_subagent_content_revision
             ORDER BY parent_session_id, provider_kind, source_key,
                      content_schema_version, revision",
            Vec::new(),
            "subagent content revision",
            &mut remaining_restore_rows,
        )
        .await?;
        snapshot.revisions = revision_rows
            .into_iter()
            .map(|row| {
                Ok(AgentSubagentContentRevisionRow {
                    parent_session_id: row.try_get_by("parent_session_id")?,
                    provider_kind: row.try_get_by("provider_kind")?,
                    source_key: row.try_get_by("source_key")?,
                    content_schema_version: row.try_get_by("content_schema_version")?,
                    revision: row.try_get_by("revision")?,
                    checkpoint_id: row.try_get_by("checkpoint_id")?,
                    content_digest: row.try_get_by("content_digest")?,
                    source_channel: row.try_get_by("source_channel")?,
                    partial: row.try_get_by("partial")?,
                    created_at: row.try_get_by("created_at")?,
                })
            })
            .collect::<Result<_, sea_orm::DbErr>>()
            .map_err(|error| {
                CloudError::Generic(format!("decode subagent revision snapshot: {error}"))
            })?;
        let link_rows = load_local_capture_pages(
            &txn,
            "SELECT content_checkpoint_id, parent_session_id, link_state,
                    boundary_checkpoint_id, stable_subagent_id, sync_revision,
                    created_at, updated_at
             FROM agent_subagent_link ORDER BY created_at, content_checkpoint_id",
            Vec::new(),
            "subagent association link",
            &mut remaining_restore_rows,
        )
        .await?;
        snapshot.links = link_rows
            .into_iter()
            .map(|row| {
                Ok(AgentSubagentLinkRow {
                    content_checkpoint_id: row.try_get_by("content_checkpoint_id")?,
                    parent_session_id: row.try_get_by("parent_session_id")?,
                    link_state: row.try_get_by("link_state")?,
                    boundary_checkpoint_id: row.try_get_by("boundary_checkpoint_id")?,
                    stable_subagent_id: row.try_get_by("stable_subagent_id")?,
                    sync_revision: row.try_get_by("sync_revision")?,
                    created_at: row.try_get_by("created_at")?,
                    updated_at: row.try_get_by("updated_at")?,
                })
            })
            .collect::<Result<_, sea_orm::DbErr>>()
            .map_err(|error| {
                CloudError::Generic(format!("decode subagent link snapshot: {error}"))
            })?;
    }

    validate_agent_capture_companions(
        &snapshot.checkpoints,
        &snapshot.claims,
        &snapshot.revisions,
        &snapshot.links,
        "local",
        CompanionValidationMode::Complete,
    )?;
    validate_agent_capture_session_dependencies(
        &snapshot.sessions,
        &snapshot.checkpoints,
        &snapshot.claims,
        "local",
    )?;
    txn.commit()
        .await
        .map_err(|error| CloudError::Generic(format!("commit agent capture snapshot: {error}")))?;
    Ok(snapshot)
}

pub(super) fn validate_agent_capture_traces_shape(
    checkpoints: &[AgentCheckpointV2Row],
    traces_head: Option<&str>,
    side: &str,
) -> CloudResult<()> {
    match (checkpoints.is_empty(), traces_head) {
        (true, Some(_)) => Err(CloudError::PartialTransfer(format!(
            "{side} agent-capture catalog is empty but its fenced traces head is nonempty; run `libra agent doctor --repair` before retrying"
        ))),
        (false, None) => Err(CloudError::PartialTransfer(format!(
            "{side} agent-capture catalog has checkpoints but no fenced traces head; run `libra agent doctor --repair` before retrying"
        ))),
        _ => Ok(()),
    }
}

pub(super) async fn load_agent_capture_snapshot(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    subagent_content_present: bool,
) -> CloudResult<AgentCaptureSnapshot> {
    // Keep the rollback-journal read transaction limited to SQLite paging.
    // Object decoding can take up to 110 seconds and must not hold a SHARED
    // lock that makes hook/import commits exhaust their busy timeout.
    let mut snapshot =
        load_agent_capture_catalog_snapshot(db_conn, repo_id, subagent_content_present).await?;
    validate_agent_capture_traces_shape(
        &snapshot.checkpoints,
        snapshot.traces_head.as_deref(),
        "local",
    )?;

    if cfg!(debug_assertions)
        && let Ok(delay) = std::env::var("LIBRA_TEST_CLOUD_AGENT_SNAPSHOT_DELAY_MS")
        && let Ok(delay) = delay.parse::<u64>()
        && delay > 0
    {
        tokio::time::sleep(std::time::Duration::from_millis(delay.min(5_000))).await;
    }

    let durability_specs = snapshot
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
    let required_oids = if durability_specs.is_empty() {
        HashSet::new()
    } else {
        let traces_head = snapshot.traces_head.as_deref().ok_or_else(|| {
            CloudError::PartialTransfer(
                "local agent-capture snapshot lost its fenced traces head".to_string(),
            )
        })?;
        let cataloged_commits = snapshot
            .checkpoints
            .iter()
            .map(|row| row.traces_commit.clone())
            .collect::<Vec<_>>();
        crate::internal::ai::history::checkpoint_rows_snapshot_durable_oids_from_head(
            &util::storage_path(),
            traces_head,
            &cataloged_commits,
            &durability_specs,
            std::time::Instant::now().checked_add(std::time::Duration::from_secs(110)),
        )
        .await
        .map_err(|error| {
            CloudError::PartialTransfer(format!(
                "agent checkpoint snapshot is not fully reachable and durable: {error:#}; run `libra agent doctor --repair`, then retry cloud sync"
            ))
        })?
    };
    let synced_oids = load_synced_required_object_oids(db_conn, repo_id, &required_oids).await?;
    for oid in &required_oids {
        if !synced_oids.contains(oid) {
            return Err(CloudError::PartialTransfer(format!(
                "agent capture cannot be published because reachable object {oid} is not in the completed local object upload generation; run `libra agent doctor --repair`, then retry cloud sync"
            )));
        }
    }
    validate_agent_capture_restore_row_budget(&snapshot, required_oids.len())?;

    let rechecked =
        load_agent_capture_catalog_snapshot(db_conn, repo_id, subagent_content_present).await?;
    if snapshot != rechecked {
        return Err(CloudError::PartialTransfer(
            "local agent-capture catalog changed during durability verification; retry cloud sync"
                .to_string(),
        ));
    }
    snapshot.required_oids = required_oids;
    Ok(snapshot)
}

async fn ensure_agent_capture_objects_remote(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    r2_storage: &RemoteStorage,
    repo_id: &str,
    required_oids: &HashSet<String>,
) -> CloudResult<(Vec<ObjectIndexRow>, i64)> {
    let local_map = load_required_local_object_indexes(db_conn, repo_id, required_oids).await?;
    let mut required = required_oids.iter().cloned().collect::<Vec<_>>();
    required.sort();
    let mut hashes = Vec::with_capacity(required.len());
    for oid in &required {
        if !local_map.contains_key(oid.as_str()) {
            return Err(CloudError::PartialTransfer(format!(
                "agent capture requires object {oid}, but its local object_index row is missing; run `libra agent doctor --repair`, then retry cloud sync"
            )));
        }
        let bytes = hex::decode(oid).map_err(|error| {
            CloudError::Generic(format!("invalid required agent-capture oid {oid}: {error}"))
        })?;
        hashes.push(ObjectHash::from_bytes(&bytes).map_err(|error| {
            CloudError::Generic(format!("invalid required agent-capture oid {oid}: {error}"))
        })?);
    }

    let remote_rows = d1_client
        .get_object_indexes_by_oids(repo_id, &required)
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "list remote object indexes for agent capture: {}",
                error.message
            ))
        })?;
    let remote_map = remote_rows
        .iter()
        .map(|row| (row.o_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    let local_storage = LocalStorage::new(path::objects());
    let verification_rows = required.iter().zip(&hashes).collect::<Vec<_>>();
    for page in agent_capture_object_verification_batches(&verification_rows) {
        // Full content verification remains mandatory, but a fixed-size page
        // overlaps R2 latency without allowing an unbounded fan-out for large
        // histories. Each page completes before the next one starts.
        futures::future::try_join_all(page.iter().map(|(oid, hash)| {
            let local_map = &local_map;
            let remote_map = &remote_map;
            let local_storage = &local_storage;
            async move {
                let local = local_map.get(oid.as_str()).ok_or_else(|| {
                    CloudError::Generic(format!(
                        "local object index {oid} disappeared during cloud sync"
                    ))
                })?;
                let remote_index_matches = remote_map.get(oid.as_str()).is_some_and(|remote| {
                    remote.o_type == local.o_type
                        && remote.o_size == local.o_size
                        && remote.is_synced == 1
                });
                publish_validated_agent_capture_object(local_storage, r2_storage, oid, hash)
                    .await?;
                if !remote_index_matches {
                    d1_client
                        .upsert_object_index(
                            &local.o_id,
                            &local.o_type,
                            local.o_size,
                            &local.repo_id,
                            local.created_at,
                        )
                        .await
                        .map_err(|error| {
                            CloudError::D1(format!(
                                "publish required agent-capture object index {oid}: {}",
                                error.message
                            ))
                        })?;
                }
                Ok::<(), CloudError>(())
            }
        }))
        .await?;
    }
    let verified_rows = d1_client
        .get_object_indexes_by_oids_with_generation(repo_id, &required)
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "verify remote object indexes for agent capture: {}",
                error.message
            ))
        })?;
    let verified_map = verified_rows
        .0
        .iter()
        .map(|row| (row.o_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    for oid in &required {
        let local = local_map.get(oid.as_str()).ok_or_else(|| {
            CloudError::Generic(format!(
                "local object index {oid} disappeared during verification"
            ))
        })?;
        let valid = verified_map.get(oid.as_str()).is_some_and(|remote| {
            remote.o_type == local.o_type && remote.o_size == local.o_size && remote.is_synced == 1
        });
        if !valid {
            return Err(CloudError::PartialTransfer(format!(
                "required agent-capture object index {oid} is absent or inconsistent in D1"
            )));
        }
    }
    Ok(verified_rows)
}

/// Verify a required capture object before its D1 manifest row can participate
/// in a completed generation. An existence probe is not a content proof: a
/// previous interrupted or corrupted upload may leave bytes under the right
/// key whose hash no longer matches that key. Valid remote payloads avoid a
/// rewrite; missing or corrupt payloads are replaced from validated local data
/// and read back once before publication continues.
pub(super) async fn publish_validated_agent_capture_object(
    local_storage: &LocalStorage,
    r2_storage: &RemoteStorage,
    oid: &str,
    hash: &ObjectHash,
) -> CloudResult<()> {
    if let Ok((remote_bytes, remote_type)) = r2_storage.get(hash).await
        && ObjectHash::from_type_and_data(remote_type, &remote_bytes) == *hash
    {
        return Ok(());
    }
    let (bytes, object_type) = local_storage.get(hash).await.map_err(|error| {
        CloudError::PartialTransfer(format!(
            "read required agent-capture object {oid} for cloud publication: {error}"
        ))
    })?;
    let local_hash = ObjectHash::from_type_and_data(object_type, &bytes);
    if local_hash != *hash {
        return Err(CloudError::PartialTransfer(format!(
            "required local agent-capture object {oid} failed content verification: computed {local_hash}"
        )));
    }
    r2_storage
        .put(hash, &bytes, object_type)
        .await
        .map_err(|error| {
            CloudError::R2(format!(
                "upload required agent-capture object {oid}: {error}"
            ))
        })?;
    let (remote_bytes, remote_type) = r2_storage.get(hash).await.map_err(|error| {
        CloudError::R2(format!(
            "read back required agent-capture object {oid}: {error}"
        ))
    })?;
    let remote_hash = ObjectHash::from_type_and_data(remote_type, &remote_bytes);
    if remote_hash != *hash {
        return Err(CloudError::PartialTransfer(format!(
            "required remote agent-capture object {oid} failed post-upload verification: computed {remote_hash}"
        )));
    }
    Ok(())
}

async fn load_full_remote_object_manifest(
    d1_client: &D1Client,
    r2_storage: &RemoteStorage,
    repo_id: &str,
) -> CloudResult<(Vec<ObjectIndexRow>, i64)> {
    let (rows, generation) = d1_client
        .get_object_indexes_bounded_with_generation(repo_id, AGENT_CAPTURE_MAX_ROWS_PER_TABLE)
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "read full retained agent-capture object manifest: {}",
                error.message
            ))
        })?;
    for page in rows.chunks(AGENT_CAPTURE_LOCAL_PAGE_SIZE) {
        let mut hashes = Vec::with_capacity(page.len());
        for row in page {
            if row.is_synced != 1 {
                return Err(CloudError::PartialTransfer(format!(
                    "retained remote object {} is not marked synced",
                    row.o_id
                )));
            }
            let bytes = hex::decode(&row.o_id).map_err(|error| {
                CloudError::Generic(format!(
                    "invalid retained remote object id {}: {error}",
                    row.o_id
                ))
            })?;
            hashes.push(ObjectHash::from_bytes(&bytes).map_err(|error| {
                CloudError::Generic(format!(
                    "invalid retained remote object id {}: {error}",
                    row.o_id
                ))
            })?);
        }
        let exists = r2_storage.exist_batch(&hashes).await;
        if let Some((missing, _)) = page.iter().zip(exists).find(|(_, exists)| !*exists) {
            return Err(CloudError::PartialTransfer(format!(
                "retained remote object {} is absent from remote storage",
                missing.o_id
            )));
        }
    }
    Ok((rows, generation))
}

#[cfg(test)]
pub(super) async fn project_agent_capture_object_indexes(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    required_oids: &HashSet<String>,
) -> CloudResult<Vec<ObjectIndexRow>> {
    let local_map = load_required_local_object_indexes(db_conn, repo_id, required_oids).await?;
    let mut projected = Vec::with_capacity(required_oids.len());
    for oid in required_oids {
        let local = local_map.get(oid.as_str()).ok_or_else(|| {
            CloudError::PartialTransfer(format!(
                "agent capture requires object {oid}, but its local object_index row is missing; run `libra agent doctor --repair`, then retry cloud sync"
            ))
        })?;
        let row = ObjectIndexRow {
            o_id: local.o_id.clone(),
            o_type: local.o_type.clone(),
            o_size: local.o_size,
            repo_id: local.repo_id.clone(),
            created_at: local.created_at,
            is_synced: 1,
        };
        projected.push(row);
    }
    Ok(projected)
}

type SubagentSourceKey = (String, String, String, i64);
type SubagentRevisionKey = (String, String, String, i64, i64);

pub(super) fn claim_key(row: &AgentSubagentContentClaimRow) -> SubagentSourceKey {
    (
        row.parent_session_id.clone(),
        row.provider_kind.clone(),
        row.source_key.clone(),
        row.content_schema_version,
    )
}

fn revision_key(row: &AgentSubagentContentRevisionRow) -> SubagentRevisionKey {
    (
        row.parent_session_id.clone(),
        row.provider_kind.clone(),
        row.source_key.clone(),
        row.content_schema_version,
        row.revision,
    )
}

fn claim_same_generation(
    left: &AgentSubagentContentClaimRow,
    right: &AgentSubagentContentClaimRow,
) -> bool {
    claim_key(left) == claim_key(right)
        && left.sync_revision == right.sync_revision
        && left.revision_cursor == right.revision_cursor
        && left.current_revision == right.current_revision
        && left.current_checkpoint_id == right.current_checkpoint_id
        && left.current_digest == right.current_digest
}

pub(super) fn should_publish_claim(
    local: &AgentSubagentContentClaimRow,
    remote: Option<&AgentSubagentContentClaimRow>,
    remote_is_known_ancestor: bool,
) -> CloudResult<bool> {
    let Some(remote) = remote else {
        return Ok(true);
    };
    if claim_same_generation(remote, local) && remote.fence_token >= local.fence_token {
        return Ok(false);
    }
    if !remote_is_known_ancestor {
        return Err(CloudError::Generic(
            "subagent claim differs from a remote generation that is not this clone's known ancestor; restore the current cloud snapshot before syncing"
                .to_string(),
        ));
    }
    if remote.revision_cursor > local.revision_cursor {
        return Err(CloudError::Generic(
            "subagent claim revision high-water would regress; restore the current cloud snapshot before syncing"
                .to_string(),
        ));
    }
    if remote.sync_revision > local.sync_revision {
        return Err(CloudError::Generic(
            "subagent claim is older than its recorded remote ancestor; restore the current cloud snapshot before syncing"
                .to_string(),
        ));
    }
    if remote.sync_revision == local.sync_revision && !claim_same_generation(remote, local) {
        return Err(CloudError::Generic(
            "subagent claim conflicts with the remote at the same sync generation".to_string(),
        ));
    }
    Ok(remote.sync_revision < local.sync_revision || remote.fence_token < local.fence_token)
}

pub(super) fn should_publish_session(
    local: &AgentSessionV2Row,
    remote: Option<&AgentSessionV2Row>,
    remote_is_known_ancestor: bool,
) -> CloudResult<bool> {
    let Some(remote) = remote else {
        return Ok(true);
    };
    if remote == local {
        return Ok(false);
    }
    if !remote_is_known_ancestor {
        return Err(CloudError::Generic(format!(
            "agent session {} differs from a remote generation that is not this clone's known ancestor; restore the current cloud snapshot before syncing",
            local.session_id
        )));
    }
    if remote.sync_revision < local.sync_revision {
        return Ok(true);
    }
    Err(CloudError::Generic(format!(
        "agent session {} does not descend monotonically from its recorded remote ancestor",
        local.session_id
    )))
}

pub(super) fn remote_catalog_is_legacy_generation_zero_bootstrap(
    has_remote_generation: bool,
    local_cloud_base: Option<i64>,
    rows: &AgentCaptureRestoreCatalogRows,
) -> bool {
    !has_remote_generation
        && local_cloud_base.is_none()
        && (!rows.sessions.is_empty() || !rows.checkpoints.is_empty())
        && rows.sessions.iter().all(|row| row.sync_revision == 0)
        && rows.checkpoints.iter().all(|row| row.sync_revision == 0)
        && rows.prune_tombstones.is_empty()
        && rows.claims.is_empty()
        && rows.revisions.is_empty()
        && rows.links.is_empty()
}

pub(super) fn remote_generation_is_known_ancestor(
    remote: Option<&AgentCaptureGenerationRow>,
    local_cloud_base: Option<i64>,
) -> bool {
    remote.is_some_and(|generation| match generation.state.as_str() {
        "complete" => local_cloud_base == Some(generation.generation),
        // A publishing generation is the immediate child of the last
        // completed base observed by its writer. Allow preflight to reconcile
        // that staged catalog so the server-side lease/CAS can eventually
        // resume an abandoned publication. This does not let an active writer
        // be displaced: begin_agent_capture_generation_from still enforces the
        // five-minute server-timestamped lease before issuing a new token.
        "publishing" => match local_cloud_base {
            Some(base) => base.checked_add(1) == Some(generation.generation),
            None => generation.generation == 1,
        },
        _ => false,
    })
}

pub(super) fn should_publish_link(
    local: &AgentSubagentLinkRow,
    remote: Option<&AgentSubagentLinkRow>,
    remote_is_known_ancestor: bool,
) -> CloudResult<bool> {
    let Some(remote) = remote else {
        return Ok(true);
    };
    if remote == local {
        return Ok(false);
    }
    if !remote_is_known_ancestor {
        return Err(CloudError::Generic(format!(
            "subagent link {} differs from a remote generation that is not this clone's known ancestor; restore the current cloud snapshot before syncing",
            local.content_checkpoint_id
        )));
    }
    if remote.sync_revision < local.sync_revision {
        return Ok(true);
    }
    Err(CloudError::Generic(format!(
        "subagent link {} does not descend monotonically from its recorded remote ancestor",
        local.content_checkpoint_id
    )))
}

pub(super) fn checkpoint_rewrite_compatible(
    left: &AgentCheckpointV2Row,
    right: &AgentCheckpointV2Row,
) -> bool {
    left.checkpoint_id == right.checkpoint_id
        && left.session_id == right.session_id
        && left.parent_checkpoint_id == right.parent_checkpoint_id
        && left.scope == right.scope
        && left.parent_commit == right.parent_commit
        && left.tool_use_id == right.tool_use_id
        && left.subagent_session_id == right.subagent_session_id
        && left.description == right.description
        && left.created_at == right.created_at
}

pub(super) fn should_publish_checkpoint(
    local: &AgentCheckpointV2Row,
    remote: Option<&AgentCheckpointV2Row>,
    remote_is_known_ancestor: bool,
) -> CloudResult<bool> {
    let Some(remote) = remote else {
        return Ok(true);
    };
    if remote.sync_revision == local.sync_revision {
        if remote == local {
            return Ok(false);
        }
        return Err(CloudError::Generic(format!(
            "agent checkpoint {} diverges from the remote at the same sync generation",
            local.checkpoint_id
        )));
    }
    if !checkpoint_rewrite_compatible(local, remote) {
        return Err(CloudError::Generic(format!(
            "agent checkpoint {} conflicts with the remote immutable identity",
            local.checkpoint_id
        )));
    }
    if local.sync_revision > remote.sync_revision && !remote_is_known_ancestor {
        return Err(CloudError::Generic(format!(
            "agent checkpoint {} differs from a remote generation that is not this clone's known ancestor; restore the current cloud snapshot before syncing",
            local.checkpoint_id
        )));
    }
    Ok(local.sync_revision > remote.sync_revision)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CompanionValidationMode {
    Complete,
    Publishing,
}

pub(super) fn validate_agent_capture_companions(
    checkpoints: &[AgentCheckpointV2Row],
    claims: &[AgentSubagentContentClaimRow],
    revisions: &[AgentSubagentContentRevisionRow],
    links: &[AgentSubagentLinkRow],
    side: &str,
    mode: CompanionValidationMode,
) -> CloudResult<()> {
    let checkpoint_map: HashMap<&str, &AgentCheckpointV2Row> = checkpoints
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect();
    let claim_keys: HashSet<SubagentSourceKey> = claims.iter().map(claim_key).collect();
    let revision_map: HashMap<SubagentRevisionKey, &AgentSubagentContentRevisionRow> = revisions
        .iter()
        .map(|row| (revision_key(row), row))
        .collect();
    let link_map: HashMap<&str, &AgentSubagentLinkRow> = links
        .iter()
        .map(|row| (row.content_checkpoint_id.as_str(), row))
        .collect();
    let revision_checkpoint_ids: HashSet<&str> = revisions
        .iter()
        .map(|row| row.checkpoint_id.as_str())
        .collect();
    for revision in revisions {
        let source_key = (
            revision.parent_session_id.clone(),
            revision.provider_kind.clone(),
            revision.source_key.clone(),
            revision.content_schema_version,
        );
        if !claim_keys.contains(&source_key) && mode == CompanionValidationMode::Complete {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision {} has no source claim dependency",
                revision.checkpoint_id
            )));
        }
        let Some(checkpoint) = checkpoint_map.get(revision.checkpoint_id.as_str()) else {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision {} has no checkpoint dependency",
                revision.checkpoint_id
            )));
        };
        if checkpoint.session_id != revision.parent_session_id {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision {} disagrees with its checkpoint parent",
                revision.checkpoint_id
            )));
        }
        if checkpoint.scope != "subagent" {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision {} references a non-subagent checkpoint",
                revision.checkpoint_id
            )));
        }
        if mode == CompanionValidationMode::Complete {
            let link = link_map
                .get(revision.checkpoint_id.as_str())
                .ok_or_else(|| {
                    CloudError::Generic(format!(
                        "{side} subagent revision {} has no association link dependency",
                        revision.checkpoint_id
                    ))
                })?;
            if link.parent_session_id != revision.parent_session_id {
                return Err(CloudError::Generic(format!(
                    "{side} subagent revision {} disagrees with its association parent",
                    revision.checkpoint_id
                )));
            }
        }
        if mode == CompanionValidationMode::Complete
            && let Some(claim) = claims.iter().find(|claim| claim_key(claim) == source_key)
            && revision.revision > claim.revision_cursor
        {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision {} is newer than its completed source claim",
                revision.checkpoint_id
            )));
        }
    }
    for link in links {
        let Some(checkpoint) = checkpoint_map.get(link.content_checkpoint_id.as_str()) else {
            return Err(CloudError::Generic(format!(
                "{side} subagent link {} has no checkpoint dependency",
                link.content_checkpoint_id
            )));
        };
        if checkpoint.session_id != link.parent_session_id {
            return Err(CloudError::Generic(format!(
                "{side} subagent link {} disagrees with its checkpoint parent",
                link.content_checkpoint_id
            )));
        }
        if checkpoint.scope != "subagent" {
            return Err(CloudError::Generic(format!(
                "{side} subagent link {} references a non-subagent content checkpoint",
                link.content_checkpoint_id
            )));
        }
        if mode == CompanionValidationMode::Complete
            && !revision_checkpoint_ids.contains(link.content_checkpoint_id.as_str())
        {
            return Err(CloudError::Generic(format!(
                "{side} subagent link {} has no immutable revision dependency",
                link.content_checkpoint_id
            )));
        }
        if let Some(boundary) = link.boundary_checkpoint_id.as_deref() {
            let boundary_checkpoint = checkpoint_map.get(boundary).ok_or_else(|| {
                CloudError::Generic(format!(
                    "{side} resolved subagent link {} has no boundary checkpoint dependency",
                    link.content_checkpoint_id
                ))
            })?;
            if boundary_checkpoint.scope != "subagent"
                || boundary_checkpoint.session_id != link.parent_session_id
                || revision_checkpoint_ids.contains(boundary)
            {
                return Err(CloudError::Generic(format!(
                    "{side} resolved subagent link {} references an invalid boundary checkpoint",
                    link.content_checkpoint_id
                )));
            }
        }
    }
    for claim in claims {
        if claim.revision_cursor < claim.current_revision {
            return Err(CloudError::Generic(format!(
                "{side} subagent claim cursor is behind its current revision"
            )));
        }
        if claim.current_revision == 0 {
            if claim.current_checkpoint_id.is_some() || claim.current_digest.is_some() {
                return Err(CloudError::Generic(format!(
                    "{side} zero-revision subagent claim has a materialized current leaf"
                )));
            }
            continue;
        }
        let checkpoint_id = claim.current_checkpoint_id.as_deref().ok_or_else(|| {
            CloudError::Generic(format!("{side} current subagent claim has no checkpoint"))
        })?;
        let digest = claim.current_digest.as_deref().ok_or_else(|| {
            CloudError::Generic(format!("{side} current subagent claim has no digest"))
        })?;
        let revision = revision_map
            .get(&(
                claim.parent_session_id.clone(),
                claim.provider_kind.clone(),
                claim.source_key.clone(),
                claim.content_schema_version,
                claim.current_revision,
            ))
            .ok_or_else(|| {
                CloudError::Generic(format!(
                    "{side} current subagent claim has no immutable revision dependency"
                ))
            })?;
        if revision.checkpoint_id != checkpoint_id || revision.content_digest != digest {
            return Err(CloudError::Generic(format!(
                "{side} current subagent claim disagrees with its immutable revision"
            )));
        }
        let link = link_map.get(checkpoint_id).ok_or_else(|| {
            CloudError::Generic(format!(
                "{side} current subagent claim has no association link dependency"
            ))
        })?;
        if link.parent_session_id != claim.parent_session_id {
            return Err(CloudError::Generic(format!(
                "{side} current subagent claim disagrees with its association parent"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_agent_capture_session_dependencies(
    sessions: &[AgentSessionV2Row],
    checkpoints: &[AgentCheckpointV2Row],
    claims: &[AgentSubagentContentClaimRow],
    side: &str,
) -> CloudResult<()> {
    let session_ids = sessions
        .iter()
        .map(|row| row.session_id.as_str())
        .collect::<HashSet<_>>();
    for checkpoint in checkpoints {
        if !session_ids.contains(checkpoint.session_id.as_str()) {
            return Err(CloudError::Generic(format!(
                "{side} checkpoint {} has no session dependency",
                checkpoint.checkpoint_id
            )));
        }
    }
    for claim in claims {
        if !session_ids.contains(claim.parent_session_id.as_str()) {
            return Err(CloudError::Generic(format!(
                "{side} subagent claim has no parent session dependency"
            )));
        }
    }
    Ok(())
}

pub(super) fn object_manifest_scope_for_remote_catalog(
    local: &[AgentCheckpointV2Row],
    effective: &[AgentCheckpointV2Row],
) -> AgentCaptureObjectManifestScope {
    let local_rows = local
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    if effective
        .iter()
        .any(|row| local_rows.get(row.checkpoint_id.as_str()).copied() != Some(row))
    {
        AgentCaptureObjectManifestScope::FullRemoteIndex
    } else {
        AgentCaptureObjectManifestScope::CheckpointProjection
    }
}

pub(super) fn build_effective_checkpoint_catalog(
    local: &[AgentCheckpointV2Row],
    remote: &[AgentCheckpointV2Row],
    local_tombstones: &[AgentCheckpointPruneTombstoneRow],
    remote_tombstones: &[AgentCheckpointPruneTombstoneRow],
    remote_is_known_ancestor: bool,
) -> CloudResult<(Vec<AgentCheckpointV2Row>, Vec<AgentCheckpointV2Row>)> {
    let local_map = local
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    let remote_map = remote
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    let local_tombstone_ids = local_tombstones
        .iter()
        .map(|row| row.checkpoint_id.as_str())
        .collect::<HashSet<_>>();
    let remote_tombstone_ids = remote_tombstones
        .iter()
        .map(|row| row.checkpoint_id.as_str())
        .collect::<HashSet<_>>();

    if let Some(row) = local
        .iter()
        .find(|row| remote_tombstone_ids.contains(row.checkpoint_id.as_str()))
    {
        return Err(CloudError::Generic(format!(
            "agent checkpoint {} was already pruned by another cloud writer; restore the current cloud snapshot before syncing this stale clone",
            row.checkpoint_id
        )));
    }

    let mut pending = Vec::new();
    let mut effective = Vec::new();
    for remote_row in remote {
        if local_tombstone_ids.contains(remote_row.checkpoint_id.as_str()) {
            continue;
        }
        let Some(local_row) = local_map.get(remote_row.checkpoint_id.as_str()).copied() else {
            return Err(CloudError::Generic(format!(
                "remote checkpoint {} is absent locally without an ordinary-prune tombstone; cloud session-erasure propagation is deferred, so restore or purge the remote capture before publishing a new generation",
                remote_row.checkpoint_id
            )));
        };
        if remote_row.sync_revision > local_row.sync_revision {
            return Err(CloudError::Generic(format!(
                "remote checkpoint {} is newer than this clone's traces history; restore the current cloud snapshot before syncing",
                remote_row.checkpoint_id
            )));
        }
        if should_publish_checkpoint(local_row, Some(remote_row), remote_is_known_ancestor)? {
            pending.push(local_row.clone());
            effective.push(local_row.clone());
        } else {
            effective.push(remote_row.clone());
        }
    }
    for local_row in local {
        if !remote_map.contains_key(local_row.checkpoint_id.as_str()) {
            pending.push(local_row.clone());
            effective.push(local_row.clone());
        }
    }
    effective.sort_by(|left, right| left.checkpoint_id.cmp(&right.checkpoint_id));
    Ok((pending, effective))
}

fn checkpoint_catalog_matches(
    left: &[AgentCheckpointV2Row],
    right: &[AgentCheckpointV2Row],
) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let right_map = right
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    left.iter()
        .all(|row| right_map.get(row.checkpoint_id.as_str()).copied() == Some(row))
}

/// Mirror one coherent local agent-capture/object generation to D1. Remote
/// state is paged and used as an incremental high-water mark; only missing or
/// strictly newer rows are sent, in bounded multi-row requests. Immutable
/// conflicts fail before publication. Dependencies publish first and claims
/// publish last.
pub(super) async fn sync_agent_capture_tables(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    r2_storage: &RemoteStorage,
    repo_id: &str,
    progress: &dyn CloudSyncProgress,
) -> CloudResult<AgentCaptureSyncOutcome> {
    tokio::time::timeout(
        AGENT_CAPTURE_CLOUD_DEADLINE,
        sync_agent_capture_tables_inner(db_conn, d1_client, r2_storage, repo_id, progress),
    )
    .await
    .map_err(|_| {
        CloudError::PartialTransfer(
            "agent capture cloud sync exceeded its 120-second deadline; retry the operation"
                .to_string(),
        )
    })?
}

async fn sync_agent_capture_tables_inner(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    r2_storage: &RemoteStorage,
    repo_id: &str,
    progress: &dyn CloudSyncProgress,
) -> CloudResult<AgentCaptureSyncOutcome> {
    use sea_orm::Statement;

    let backend = db_conn.get_database_backend();
    let session_present = db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_session' LIMIT 1",
            [],
        ))
        .await
        .map_err(|error| CloudError::Generic(format!("query sqlite_master: {error}")))?
        .is_some();
    if !session_present {
        return Ok(AgentCaptureSyncOutcome::SkippedLegacySchema);
    }
    let subagent_content_present = db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'agent_subagent_content_claim' LIMIT 1",
            [],
        ))
        .await
        .map_err(|error| CloudError::Generic(format!("query subagent-content schema: {error}")))?
        .is_some();

    progress.on_agent_capture_starting();
    let snapshot = load_agent_capture_snapshot(db_conn, repo_id, subagent_content_present).await?;

    d1_client
        .ensure_agent_session_table()
        .await
        .map_err(|error| {
            CloudError::D1(format!("ensure_agent_session_table: {}", error.message))
        })?;
    d1_client
        .ensure_agent_checkpoint_table()
        .await
        .map_err(|error| {
            CloudError::D1(format!("ensure_agent_checkpoint_table: {}", error.message))
        })?;
    d1_client
        .ensure_agent_capture_generation_table()
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "ensure_agent_capture_generation_table: {}",
                error.message
            ))
        })?;
    d1_client
        .ensure_agent_checkpoint_prune_tombstone_table()
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "ensure checkpoint prune tombstones: {}",
                error.message
            ))
        })?;
    d1_client
        .ensure_agent_import_tombstone_table()
        .await
        .map_err(|error| {
            CloudError::D1(format!("ensure agent import tombstones: {}", error.message))
        })?;
    if subagent_content_present {
        d1_client
            .ensure_agent_subagent_content_tables()
            .await
            .map_err(|error| {
                CloudError::D1(format!(
                    "ensure_agent_subagent_content_tables: {}",
                    error.message
                ))
            })?;
    }

    let remote_generation = d1_client
        .get_agent_capture_generation(repo_id)
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "read agent-capture generation before sync: {}",
                error.message
            ))
        })?;
    let local_cloud_base = load_local_agent_capture_cloud_base(db_conn, repo_id).await?;
    let remote_catalog = d1_client
        .list_agent_capture_restore_catalog_rows(
            repo_id,
            subagent_content_present,
            AGENT_CAPTURE_RESTORE_MAX_ROWS,
        )
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "list aggregate-bounded remote agent-capture catalog before sync: {}",
                error.message
            ))
        })?;
    // The one-time legacy adoption copies only session/checkpoint rows at
    // generation zero and deliberately has no completed generation manifest.
    // Treat that exact projection as the bootstrap ancestor so the first
    // current client can replace revision zero under its first fenced
    // generation. Any current-only row type or nonzero revision fails closed.
    let remote_is_known_ancestor =
        remote_generation_is_known_ancestor(remote_generation.as_ref(), local_cloud_base)
            || remote_catalog_is_legacy_generation_zero_bootstrap(
                remote_generation.is_some(),
                local_cloud_base,
                &remote_catalog,
            );
    let AgentCaptureRestoreCatalogRows {
        sessions: remote_sessions,
        checkpoints: remote_checkpoints,
        prune_tombstones: remote_prune_tombstones,
        import_tombstones: remote_import_tombstones,
        claims: remote_claims,
        revisions: remote_revisions,
        links: remote_links,
        remaining_rows: _,
    } = remote_catalog;
    // PD-03: the effective tombstone set is the UNION of local and remote
    // (newest erased_at per provider identity), and every remote catalog
    // row belonging to an erased session id is dropped BEFORE conflict/
    // effective-set computation — the publish cascade deletes the same
    // rows remotely, so the post-publish verification stays coherent.
    let merged_import_tombstones =
        merge_import_tombstones(&snapshot.import_tombstones, &remote_import_tombstones);
    let erased_session_ids: HashSet<&str> = merged_import_tombstones
        .iter()
        .map(|row| row.erased_session_id.as_str())
        .collect();
    let remote_sessions: Vec<AgentSessionV2Row> = remote_sessions
        .into_iter()
        .filter(|row| !erased_session_ids.contains(row.session_id.as_str()))
        .collect();
    let erased_checkpoint_ids: HashSet<&str> = remote_checkpoints
        .iter()
        .filter(|row| erased_session_ids.contains(row.session_id.as_str()))
        .map(|row| row.checkpoint_id.as_str())
        .collect();
    let remote_checkpoints: Vec<AgentCheckpointV2Row> = remote_checkpoints
        .iter()
        .filter(|row| !erased_session_ids.contains(row.session_id.as_str()))
        .cloned()
        .collect();
    let remote_claims: Vec<AgentSubagentContentClaimRow> = remote_claims
        .into_iter()
        .filter(|row| !erased_session_ids.contains(row.parent_session_id.as_str()))
        .collect();
    let remote_revisions: Vec<AgentSubagentContentRevisionRow> = remote_revisions
        .into_iter()
        .filter(|row| !erased_checkpoint_ids.contains(row.checkpoint_id.as_str()))
        .collect();
    let remote_links: Vec<AgentSubagentLinkRow> = remote_links
        .into_iter()
        .filter(|row| !erased_checkpoint_ids.contains(row.content_checkpoint_id.as_str()))
        .collect();
    validate_agent_capture_companions(
        &remote_checkpoints,
        &remote_claims,
        &remote_revisions,
        &remote_links,
        "remote",
        CompanionValidationMode::Publishing,
    )?;
    validate_agent_capture_session_dependencies(
        &remote_sessions,
        &remote_checkpoints,
        &remote_claims,
        "remote",
    )?;
    let remote_session_map: HashMap<&str, &AgentSessionV2Row> = remote_sessions
        .iter()
        .map(|row| (row.session_id.as_str(), row))
        .collect();
    let mut pending_sessions = Vec::new();
    for row in &snapshot.sessions {
        if should_publish_session(
            row,
            remote_session_map.get(row.session_id.as_str()).copied(),
            remote_is_known_ancestor,
        )? {
            pending_sessions.push(row.clone());
        }
    }
    let (pending_checkpoints, effective_checkpoints) = build_effective_checkpoint_catalog(
        &snapshot.checkpoints,
        &remote_checkpoints,
        &snapshot.prune_tombstones,
        &remote_prune_tombstones,
        remote_is_known_ancestor,
    )?;

    let object_manifest_scope =
        object_manifest_scope_for_remote_catalog(&snapshot.checkpoints, &effective_checkpoints);
    let (required_object_indexes, required_object_generation) =
        ensure_agent_capture_objects_remote(
            db_conn,
            d1_client,
            r2_storage,
            repo_id,
            &snapshot.required_oids,
        )
        .await?;
    let (object_manifest_rows, object_index_generation) = match object_manifest_scope {
        AgentCaptureObjectManifestScope::CheckpointProjection => {
            (required_object_indexes, required_object_generation)
        }
        AgentCaptureObjectManifestScope::FullRemoteIndex => {
            load_full_remote_object_manifest(d1_client, r2_storage, repo_id).await?
        }
    };
    validate_agent_capture_restore_row_budget(&snapshot, object_manifest_rows.len())?;
    validate_checkpoint_object_index_roots(
        &effective_checkpoints,
        &object_manifest_rows,
        "projected remote",
    )?;
    let (object_index_digest, object_index_count) =
        agent_capture_object_index_digest(&object_manifest_rows)?;

    let remote_revision_map: HashMap<SubagentRevisionKey, &AgentSubagentContentRevisionRow> =
        remote_revisions
            .iter()
            .map(|row| (revision_key(row), row))
            .collect();
    let mut pending_revisions = Vec::new();
    for row in &snapshot.revisions {
        match remote_revision_map.get(&revision_key(row)) {
            None => pending_revisions.push(row.clone()),
            Some(remote) if *remote == row => {}
            Some(_) => {
                return Err(CloudError::Generic(format!(
                    "immutable subagent revision {} conflicts with the remote",
                    row.checkpoint_id
                )));
            }
        }
    }

    let remote_link_map: HashMap<&str, &AgentSubagentLinkRow> = remote_links
        .iter()
        .map(|row| (row.content_checkpoint_id.as_str(), row))
        .collect();
    let mut pending_links = Vec::new();
    for row in &snapshot.links {
        if should_publish_link(
            row,
            remote_link_map
                .get(row.content_checkpoint_id.as_str())
                .copied(),
            remote_is_known_ancestor,
        )? {
            pending_links.push(row.clone());
        }
    }
    let prune_ids = snapshot
        .prune_tombstones
        .iter()
        .map(|row| row.checkpoint_id.as_str())
        .collect::<HashSet<_>>();
    let (pre_prune_links, pending_links): (Vec<_>, Vec<_>) =
        pending_links.into_iter().partition(|row| {
            !prune_ids.contains(row.content_checkpoint_id.as_str())
                && row.boundary_checkpoint_id.is_none()
                && remote_link_map
                    .get(row.content_checkpoint_id.as_str())
                    .and_then(|remote| remote.boundary_checkpoint_id.as_deref())
                    .is_some_and(|boundary| prune_ids.contains(boundary))
        });
    if let Some(link) = remote_links.iter().find(|remote| {
        !prune_ids.contains(remote.content_checkpoint_id.as_str())
            && remote
                .boundary_checkpoint_id
                .as_deref()
                .is_some_and(|boundary| prune_ids.contains(boundary))
            && !pre_prune_links
                .iter()
                .any(|local| local.content_checkpoint_id == remote.content_checkpoint_id)
    }) {
        return Err(CloudError::Generic(format!(
            "remote subagent link {} still resolves through a checkpoint being pruned, but this clone has no newer unresolved link generation; restore the current cloud snapshot before syncing",
            link.content_checkpoint_id
        )));
    }

    let remote_claim_map: HashMap<SubagentSourceKey, &AgentSubagentContentClaimRow> = remote_claims
        .iter()
        .map(|row| (claim_key(row), row))
        .collect();
    let mut pending_claims = Vec::new();
    for row in &snapshot.claims {
        if should_publish_claim(
            row,
            remote_claim_map.get(&claim_key(row)).copied(),
            remote_is_known_ancestor,
        )? {
            pending_claims.push(row.clone());
        }
    }

    // All remote conflict and object-durability checks happen before this
    // transition. A transient preflight failure therefore leaves the last
    // complete manifest restorable instead of needlessly wedging it in
    // `publishing` before any fenced capture-catalog mutation.
    let publish_token = Uuid::new_v4().to_string();
    d1_client
        .begin_agent_capture_generation_from(
            repo_id,
            &publish_token,
            remote_generation
                .as_ref()
                .map(|generation| generation.generation),
            AgentCaptureGenerationManifest {
                object_index_digest: &object_index_digest,
                object_index_count,
                object_index_scope: object_manifest_scope.as_str(),
                object_index_generation,
                traces_head: snapshot.traces_head.as_deref(),
            },
        )
        .await
        .map_err(|error| {
            CloudError::D1(format!("begin agent capture generation: {}", error.message))
        })?;
    for rows in agent_capture_batches(&pending_sessions) {
        d1_client
            .sync_agent_sessions_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                let row_id = rows
                    .first()
                    .map(|row| row.session_id.as_str())
                    .unwrap_or("agent-session-batch");
                progress.on_agent_capture_session_warning(row_id, &error.message);
                CloudError::D1(format!("sync agent session batch: {}", error.message))
            })?;
    }
    // Boundary associations must become unresolved before their boundary
    // checkpoint is deleted. This preserves a Publishing-valid graph at every
    // request boundary; new content links still publish after checkpoints.
    for rows in agent_capture_batches(&pre_prune_links) {
        d1_client
            .sync_agent_subagent_links_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                CloudError::D1(format!(
                    "sync pre-prune subagent link batch: {}",
                    error.message
                ))
            })?;
    }
    for rows in agent_capture_batches(&snapshot.prune_tombstones) {
        d1_client
            .sync_agent_checkpoint_prune_tombstones_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                CloudError::D1(format!(
                    "sync checkpoint prune tombstones: {}",
                    error.message
                ))
            })?;
    }
    for rows in agent_capture_batches(&pending_checkpoints) {
        d1_client
            .sync_agent_checkpoints_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                let row_id = rows
                    .first()
                    .map(|row| row.checkpoint_id.as_str())
                    .unwrap_or("agent-checkpoint-batch");
                progress.on_agent_capture_checkpoint_warning(row_id, &error.message);
                CloudError::D1(format!("sync agent checkpoint batch: {}", error.message))
            })?;
    }
    for rows in agent_capture_batches(&pending_revisions) {
        d1_client
            .sync_agent_subagent_revisions_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                let row_id = rows
                    .first()
                    .map(|row| row.checkpoint_id.as_str())
                    .unwrap_or("subagent-revision-batch");
                progress.on_agent_capture_checkpoint_warning(row_id, &error.message);
                CloudError::D1(format!("sync subagent revision batch: {}", error.message))
            })?;
    }
    for rows in agent_capture_batches(&pending_links) {
        d1_client
            .sync_agent_subagent_links_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                let row_id = rows
                    .first()
                    .map(|row| row.content_checkpoint_id.as_str())
                    .unwrap_or("subagent-link-batch");
                progress.on_agent_capture_checkpoint_warning(row_id, &error.message);
                CloudError::D1(format!("sync subagent link batch: {}", error.message))
            })?;
    }
    for rows in agent_capture_batches(&pending_claims) {
        d1_client
            .sync_agent_subagent_claims_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                progress.on_agent_capture_warning(&error.message);
                CloudError::D1(format!("sync subagent claim batch: {}", error.message))
            })?;
    }
    // PD-03: the session tombstones publish LAST so their cascade delete
    // is final regardless of what earlier batches upserted.
    for rows in agent_capture_batches(&merged_import_tombstones) {
        d1_client
            .sync_agent_import_tombstones_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                CloudError::D1(format!("sync agent import tombstones: {}", error.message))
            })?;
    }

    let AgentCaptureRestoreCatalogRows {
        sessions: completed_sessions,
        checkpoints: completed_checkpoints,
        prune_tombstones: _,
        import_tombstones: _,
        claims: completed_claims,
        revisions: completed_revisions,
        links: completed_links,
        remaining_rows: completed_remaining_rows,
    } = d1_client
        .list_agent_capture_restore_catalog_rows(
            repo_id,
            subagent_content_present,
            AGENT_CAPTURE_RESTORE_MAX_ROWS,
        )
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "verify aggregate-bounded agent-capture catalog: {}",
                error.message
            ))
        })?;
    if !checkpoint_catalog_matches(&completed_checkpoints, &effective_checkpoints) {
        return Err(CloudError::PartialTransfer(
            "remote checkpoint catalog changed during agent-capture publication; retry cloud sync"
                .to_string(),
        ));
    }
    validate_agent_capture_companions(
        &completed_checkpoints,
        &completed_claims,
        &completed_revisions,
        &completed_links,
        "completed remote",
        CompanionValidationMode::Complete,
    )?;
    validate_agent_capture_session_dependencies(
        &completed_sessions,
        &completed_checkpoints,
        &completed_claims,
        "completed remote",
    )?;
    let completed_scope =
        object_manifest_scope_for_remote_catalog(&snapshot.checkpoints, &effective_checkpoints);
    if completed_scope != object_manifest_scope {
        return Err(CloudError::PartialTransfer(
            "remote checkpoint catalog changed its object-manifest scope during publication; retry cloud sync"
                .to_string(),
        ));
    }
    let mut required_oids = snapshot.required_oids.iter().cloned().collect::<Vec<_>>();
    required_oids.sort();
    let (completed_object_indexes, completed_object_generation) = match object_manifest_scope {
        AgentCaptureObjectManifestScope::CheckpointProjection => {
            if required_oids.len() > completed_remaining_rows {
                return Err(CloudError::PartialTransfer(format!(
                    "completed remote agent-capture verification exceeds its aggregate {}-row safety bound before reading object indexes",
                    AGENT_CAPTURE_RESTORE_MAX_ROWS
                )));
            }
            d1_client
                .get_object_indexes_by_oids_with_generation(repo_id, &required_oids)
                .await
                .map_err(|error| {
                    CloudError::D1(format!(
                        "verify fenced agent-capture object indexes: {}",
                        error.message
                    ))
                })?
        }
        AgentCaptureObjectManifestScope::FullRemoteIndex => d1_client
            .get_object_indexes_bounded_with_generation(repo_id, completed_remaining_rows)
            .await
            .map_err(|error| {
                CloudError::D1(format!(
                    "verify full agent-capture object manifest within the aggregate row budget: {}",
                    error.message
                ))
            })?,
    };
    completed_remaining_rows
        .checked_sub(completed_object_indexes.len())
        .ok_or_else(|| {
            CloudError::PartialTransfer(format!(
                "completed remote agent-capture verification exceeds its aggregate {}-row safety bound while reading object indexes",
                AGENT_CAPTURE_RESTORE_MAX_ROWS
            ))
        })?;
    validate_checkpoint_object_index_roots(
        &completed_checkpoints,
        &completed_object_indexes,
        "completed remote",
    )?;
    let completed_object_manifest = agent_capture_object_index_digest(&completed_object_indexes)?;
    if completed_object_manifest != (object_index_digest.clone(), object_index_count)
        || completed_object_generation != object_index_generation
    {
        return Err(CloudError::PartialTransfer(
            "remote object indexes changed during agent-capture publication; retry cloud sync"
                .to_string(),
        ));
    }
    let completed_generation = d1_client
        .complete_agent_capture_generation(repo_id, &publish_token, object_index_generation)
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "complete agent capture generation: {}",
                error.message
            ))
        })?;
    store_local_agent_capture_cloud_base(db_conn, repo_id, completed_generation.generation).await?;

    let sessions_synced = pending_sessions.len();
    let checkpoints_synced = pending_checkpoints.len();
    let subagent_rows_synced = pending_claims
        .len()
        .saturating_add(pending_revisions.len())
        .saturating_add(pending_links.len())
        .saturating_add(pre_prune_links.len());
    progress.on_agent_capture_done_with_subagents(
        sessions_synced,
        0,
        checkpoints_synced,
        0,
        subagent_rows_synced,
        0,
    );
    Ok(AgentCaptureSyncOutcome::Completed {
        sessions_synced,
        sessions_failed: 0,
        checkpoints_synced,
        checkpoints_failed: 0,
    })
}
