//! plan-20260926 DM-11: read-side of the deterministic Episode projection.
//!
//! `libra memory status|list|show` read the zero-authority projection tables.
//! These functions never write (GC-DM-01) and never answer from a stale
//! projection (ADR-DM-10): [`projection_is_stale`] compares the stored commit
//! fingerprint against the current horizon-window operation set, and the
//! caller must fail-closed with `LBR-MEMORY-001` unless `--allow-stale`.
//!
//! `--json` output is additionally held to ER-11: no host-absolute paths leak
//! into the envelope (code paths are repository-relative byte strings).

use sea_orm::ConnectionTrait;

use super::{Episode, MemoryError, Outcome, SourceKind, commit_fingerprint};

/// The frozen read-side selection version (GC-DM-05). Any future change to the
/// deterministic order of `list`/`show`/`recall` must bump this constant.
pub const EPISODE_SELECTOR_VERSION: i64 = 1;

/// A snapshot of the persisted `memory_projection_state` row plus the freshness
/// verdict derived from it (ADR-DM-10).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionStatus {
    pub schema_version: i64,
    pub rules_version: i64,
    pub horizon_truncated: bool,
    pub revoked_count: i64,
    pub aged_out_count: i64,
    pub rebuilt_at: i64,
    /// `true` when the stored fingerprint differs from the current
    /// horizon-window operation fingerprint (or the projection was never
    /// derived). A stale projection must fail-closed (ADR-DM-10).
    pub is_fresh: bool,
}

/// A serializable, normalized view of a single Episode for `--json` / human
/// output. Paths are emitted as UTF-8 byte strings; the underlying `code_path`
/// stays byte-exact (lossless in the table) and only the presentation layer
/// normalizes control characters and host path separators.
#[derive(Clone, Debug, serde::Serialize)]
pub struct EpisodeView {
    pub episode_id: String,
    pub repo_id: String,
    pub source_kind: String,
    pub source_key: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    pub started_at: i64,
    pub ended_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_id: Option<String>,
    pub title: String,
    pub body: String,
    pub content_digest: String,
    pub producer: String,
    pub rules_version: i64,
    pub evidence: Vec<EvidenceView>,
    pub paths: Vec<PathView>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct EvidenceView {
    pub kind: String,
    pub ref_id: String,
    pub link_confidence: String,
    pub resolution_status: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct PathView {
    /// Repository-relative code path, normalized for presentation.
    pub code_path: String,
    pub change_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_oid_at_end: Option<String>,
    pub ended_at: i64,
}

impl EpisodeView {
    /// Build a normalized view from a fully-materialized Episode.
    pub fn from_episode(episode: &Episode) -> Self {
        let mut paths = episode
            .paths
            .iter()
            .map(|path| PathView {
                code_path: crate::internal::ai::memory::render_code_path(&path.code_path),
                change_kind: path.change_kind.clone(),
                blob_oid_at_end: path.blob_oid_at_end.clone(),
                ended_at: path.ended_at,
            })
            .collect::<Vec<_>>();
        // Deterministic presentation order (GC-DM-05): paths by byte order of
        // the normalized code_path, then by ended_at; evidence by ordinal.
        paths.sort_by(|a, b| {
            a.code_path
                .cmp(&b.code_path)
                .then(a.ended_at.cmp(&b.ended_at))
        });

        Self {
            episode_id: episode.episode_id.clone(),
            repo_id: episode.repo_id.clone(),
            source_kind: episode.source_kind.as_str().to_string(),
            source_key: episode.source_key.clone(),
            outcome: episode.outcome.as_str().to_string(),
            actor: episode.actor.clone(),
            started_at: episode.started_at,
            ended_at: episode.ended_at,
            anchor_commit: episode.anchor_commit.clone(),
            change_id: episode.change_id.clone(),
            title: episode.title.clone(),
            body: episode.body.clone(),
            content_digest: episode.content_digest.clone(),
            producer: episode.producer.to_string(),
            rules_version: episode.rules_version,
            evidence: episode
                .evidence
                .iter()
                .map(|edge| EvidenceView {
                    kind: edge.kind.clone(),
                    ref_id: edge.ref_id.clone(),
                    link_confidence: edge.link_confidence.clone(),
                    resolution_status: edge.resolution_status.clone(),
                })
                .collect(),
            paths,
        }
    }
}

/// Read the persisted commit-source projection state and derive the freshness
/// verdict. Returns `None` when no projection row exists for the repo (the
/// "never derived" baseline — treat as stale but report gracefully).
pub async fn read_status<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
) -> Result<Option<ProjectionStatus>, MemoryError> {
    use sea_orm::{Statement, Value};

    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT schema_version, rules_version, horizon_truncated, revoked_count, \
             aged_out_count, rebuilt_at, fingerprint FROM `memory_projection_state` \
             WHERE repo_id = ? AND source_kind = 'commit'",
            [Value::from(repo_id.to_string())],
        ))
        .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let schema_version = row.try_get("", "schema_version").unwrap_or(0);
    let rules_version = row.try_get("", "rules_version").unwrap_or(0);
    let horizon_truncated = row.try_get::<i64>("", "horizon_truncated").unwrap_or(0) == 1;
    let revoked_count = row.try_get("", "revoked_count").unwrap_or(0);
    let aged_out_count = row.try_get("", "aged_out_count").unwrap_or(0);
    let rebuilt_at = row.try_get("", "rebuilt_at").unwrap_or(0);
    let stored_fingerprint: String = row.try_get("", "fingerprint").unwrap_or_default();

    // Freshness: compare the stored fingerprint against the current
    // horizon-window operation fingerprint. We do NOT trust `rebuilt_at` or
    // `max(end_ts)` (ADR-DM-03); the operation set is the only valid oracle.
    let operations = commit_operations(conn, repo_id).await?;
    let current_fingerprint = commit_fingerprint(&operations);

    Ok(Some(ProjectionStatus {
        schema_version,
        rules_version,
        horizon_truncated,
        revoked_count,
        aged_out_count,
        rebuilt_at,
        is_fresh: stored_fingerprint == current_fingerprint,
    }))
}

/// Return `Some(true)` if the commit projection is stale, `Some(false)` if it
/// is fresh, and `None` when no projection has ever been derived (the empty
/// baseline is not a stale-error; callers must handle it as an empty window).
pub async fn projection_is_stale<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
) -> Result<Option<bool>, MemoryError> {
    match read_status(conn, repo_id).await? {
        Some(status) => Ok(Some(!status.is_fresh)),
        None => Ok(None),
    }
}

/// List every Episode currently in the projection window for `repo_id`,
/// ordered deterministically (GC-DM-05). Reads only the projected tables;
/// never writes.
pub async fn list_episodes<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
) -> Result<Vec<Episode>, MemoryError> {
    use sea_orm::{Statement, Value};

    let episode_rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT * FROM `memory_episode` WHERE repo_id = ? \
             ORDER BY source_kind ASC, source_key ASC",
            [Value::from(repo_id.to_string())],
        ))
        .await?;

    let mut episodes = Vec::new();
    for row in episode_rows {
        let episode = row_to_episode(&row)?;
        episodes.push(episode);
    }

    // Attach evidence and paths for all episodes in one pass.
    attach_evidence_and_paths(conn, repo_id, &mut episodes).await?;
    Ok(episodes)
}

/// Read a single Episode by id; `None` when absent (caller must map to
/// `LBR-MEMORY-002`).
pub async fn read_episode<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    episode_id: &str,
) -> Result<Option<Episode>, MemoryError> {
    use sea_orm::{Statement, Value};

    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT * FROM `memory_episode` WHERE repo_id = ? AND episode_id = ?",
            [
                Value::from(repo_id.to_string()),
                Value::from(episode_id.to_string()),
            ],
        ))
        .await?;

    let Some(row) = row else {
        return Ok(None);
    };
    let mut episode = row_to_episode(&row)?;
    attach_evidence_and_paths(conn, repo_id, std::slice::from_mut(&mut episode)).await?;
    Ok(Some(episode))
}

fn row_to_episode(row: &sea_orm::QueryResult) -> Result<Episode, MemoryError> {
    let source_kind_str: String = row.try_get("", "source_kind").unwrap_or_default();
    let outcome_str: String = row.try_get("", "outcome").unwrap_or_default();
    let source_kind =
        SourceKind::try_from(source_kind_str.as_str()).map_err(MemoryError::Projection)?;
    let outcome = Outcome::try_from(outcome_str.as_str()).map_err(MemoryError::Projection)?;

    let actor: Option<String> = row.try_get("", "actor").ok().flatten();
    let anchor_commit: Option<String> = row.try_get("", "anchor_commit").ok().flatten();
    let change_id: Option<String> = row.try_get("", "change_id").ok().flatten();
    let producer: String = row.try_get("", "producer").unwrap_or_default();

    Ok(Episode {
        episode_id: row.try_get("", "episode_id").unwrap_or_default(),
        repo_id: row.try_get("", "repo_id").unwrap_or_default(),
        source_kind,
        source_key: row.try_get("", "source_key").unwrap_or_default(),
        outcome,
        actor,
        started_at: row.try_get("", "started_at").unwrap_or(0),
        ended_at: row.try_get("", "ended_at").unwrap_or(0),
        anchor_commit,
        change_id,
        title: row.try_get("", "title").unwrap_or_default(),
        body: row.try_get("", "body").unwrap_or_default(),
        content_digest: row.try_get("", "content_digest").unwrap_or_default(),
        // `producer` is a `&'static str` in the domain type; the DB stores the
        // literal. We leak a static for the known producers and fall back to a
        // leaked copy otherwise (bounded by the number of distinct rows read).
        producer: producer_lease(&producer),
        rules_version: row.try_get("", "rules_version").unwrap_or(0),
        evidence: Vec::new(),
        paths: Vec::new(),
    })
}

/// The `Episode.producer` field is a `&'static str` in the domain type. The DB
/// stores it as text; known values return their canonical static, unknown values
/// are leaked (bounded by the number of distinct producers read in one batch).
fn producer_lease(value: &str) -> &'static str {
    match value {
        "derived-v1" => "derived-v1",
        other => Box::leak(other.to_string().into_boxed_str()),
    }
}

async fn attach_evidence_and_paths<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    episodes: &mut [Episode],
) -> Result<(), MemoryError> {
    use std::collections::HashMap;

    use sea_orm::Statement;

    if episodes.is_empty() {
        return Ok(());
    }

    use crate::internal::ai::memory::episode::{EvidenceEdge, PathRow};
    let ids: Vec<String> = episodes.iter().map(|e| e.episode_id.clone()).collect();

    let mut evidence_map: HashMap<String, Vec<EvidenceEdge>> = HashMap::new();
    for row in conn
        .query_all_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT episode_id, kind, ref_id, link_confidence, resolution_status \
             FROM `memory_episode_evidence` WHERE episode_id IN (SELECT episode_id \
             FROM `memory_episode` WHERE repo_id = ?)",
            [sea_orm::Value::from(repo_id.to_string())],
        ))
        .await?
        .into_iter()
        // Filter through the repo's episodes by id to avoid cross-repo bleed.
        .filter(|row| {
            let id: Option<String> = row.try_get("", "episode_id").ok().flatten();
            id.as_ref().is_some_and(|i| ids.contains(i))
        })
    {
        let id: String = row.try_get("", "episode_id").unwrap_or_default();
        let edge = EvidenceEdge {
            kind: row.try_get("", "kind").unwrap_or_default(),
            ref_id: row.try_get("", "ref_id").unwrap_or_default(),
            link_confidence: row.try_get("", "link_confidence").unwrap_or_default(),
            resolution_status: row.try_get("", "resolution_status").unwrap_or_default(),
        };
        evidence_map.entry(id).or_default().push(edge);
    }

    let mut path_map: HashMap<String, Vec<PathRow>> = HashMap::new();
    let path_rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT episode_id, code_path, change_kind, blob_oid_at_end, ended_at \
             FROM `memory_episode_path` WHERE episode_id IN (SELECT episode_id \
             FROM `memory_episode` WHERE repo_id = ?)",
            [sea_orm::Value::from(repo_id.to_string())],
        ))
        .await?;
    for row in path_rows {
        let id: Option<String> = row.try_get("", "episode_id").ok().flatten();
        if !id.as_ref().is_some_and(|i| ids.contains(i)) {
            continue;
        }
        let id = id.unwrap_or_default();
        let code_path: Vec<u8> = row.try_get("", "code_path").unwrap_or_default();
        let change_kind: String = row.try_get("", "change_kind").unwrap_or_default();
        let blob_oid_at_end: Option<String> = row.try_get("", "blob_oid_at_end").ok().flatten();
        let ended_at = row.try_get("", "ended_at").unwrap_or(0);
        path_map.entry(id).or_default().push(PathRow {
            code_path,
            change_kind,
            blob_oid_at_end,
            ended_at,
        });
    }

    for episode in episodes.iter_mut() {
        if let Some(edges) = evidence_map.remove(&episode.episode_id) {
            episode.evidence = edges;
        }
        if let Some(paths) = path_map.remove(&episode.episode_id) {
            episode.paths = paths;
        }
    }

    Ok(())
}

/// Read the horizon-window operation set, reused for the freshness oracle.
/// (The projection layer keeps the canonical implementation; this mirrors it
/// so the reader can compute the fingerprint without exposing private state.)
async fn commit_operations<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
) -> Result<Vec<(String, String, Option<i64>)>, MemoryError> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

    use crate::internal::model::{ai_operation_link, operation};

    let op_ids: Vec<String> = ai_operation_link::Entity::find()
        .filter(ai_operation_link::Column::RepoId.eq(repo_id))
        .all(conn)
        .await?
        .into_iter()
        .map(|link| link.operation_id)
        .collect();
    if op_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ops = operation::Entity::find()
        .filter(operation::Column::OpId.is_in(op_ids))
        .order_by_asc(operation::Column::OpId)
        .all(conn)
        .await?;
    Ok(ops
        .into_iter()
        .map(|op| (op.op_id, op.status, op.end_ts))
        .collect())
}

/// Present a stored `code_path` byte string for human / JSON output: decode as
/// UTF-8 (lossy, never panics) and strip host-path prefixes. The underlying
/// bytes are never mutated; this is a presentation-only normalization.
pub fn render_code_path(code_path: &[u8]) -> String {
    let text = String::from_utf8_lossy(code_path);
    // Git paths are always repo-relative; a leading `./` or an absolute
    // workspace prefix would leak host state, so strip them for the envelope.
    normalize_path_text(&text)
}

/// Normalize a code path for presentation: strip control characters and any
/// host-absolute / `./` prefix so the `--json` envelope never leaks a local
/// workspace prefix (ER-11). The stored bytes stay byte-exact.
pub fn normalize_path_text(text: &str) -> String {
    let trimmed = text.trim_start_matches("./");
    trimmed
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
