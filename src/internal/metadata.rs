//! Unified scoped metadata store (lore.md §1.5/§1.10) — the SINGLE owner API
//! for all metadata scopes. Branch metadata lives in the `metadata_kv` table;
//! repo-scope metadata intentionally lives in `config_kv` under the
//! `metadata.*` namespace (see [`REPO_METADATA_PREFIX`]) so `libra config`
//! tooling keeps working on it; revision metadata merges the commit's
//! immutable trailer block with a mutable notes layer (see the revision
//! section below); file-scope metadata awaits its own design round.
//!
//! `protect` / `archive` / `lineage.*` are KEYS in this store, never separate
//! tables. Nothing enforces them yet: enforcement lands once, in the future
//! branch-policy layer (lore.md 1.13), which reads through
//! [`MetadataKv::is_protected_with_conn`] / [`MetadataKv::is_archived_with_conn`]
//! inside its authoritative transaction. The truthy parse is FAIL-CLOSED — a
//! garbage value counts as protected — so a corrupted value can never silently
//! disable protection when enforcement arrives.
//!
//! Every core operation ships a `_with_conn` variant (transaction-safe,
//! matching the `ConfigKv` convention) plus a pool-acquiring wrapper.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, sea_query::OnConflict,
};

use crate::internal::{db::get_db_conn_instance, model::metadata_kv};

/// Repo-scope metadata namespace inside `config_kv` (lore.md §1.5: repo =
/// config_kv). `libra metadata --repo <key>` reads/writes `metadata.<key>`
/// through `ConfigKv`; `libra config` operating on the same keys is an
/// intended dual surface.
pub const REPO_METADATA_PREFIX: &str = "metadata.";

/// Well-known branch-metadata key: branch protection (recorded now, enforced
/// by the future branch-policy layer, lore.md 1.13).
pub const KEY_PROTECT: &str = "protect";
/// Well-known branch-metadata key: branch archival.
pub const KEY_ARCHIVE: &str = "archive";
/// Well-known branch-metadata key prefix: branch lineage records.
pub const LINEAGE_PREFIX: &str = "lineage.";

/// Maximum metadata key length in bytes.
pub const MAX_KEY_LEN: usize = 256;
/// Maximum metadata value length in bytes (text values in v1).
pub const MAX_VALUE_LEN: usize = 1024 * 1024;

/// The metadata scope. v1 supports `Branch`; the `scope` column is TEXT so
/// future scopes (worktree, …; revision/file metadata use trailers/side-trees
/// per lore.md 1.10, not this table) need no table rebuild.
///
/// `AgentTracesInflight` (AG-20) holds the external-agent checkpoint
/// writer's in-progress markers: `target` = the Libra agent session id,
/// `key` = the write attempt's checkpoint UUID, `value` = a JSON
/// [`crate::internal::ai::history::TracesInflightMarker`]. The markers close
/// the prune windows A/B described in
/// `docs/development/tracing/agent.md` (write-sequence matrix): prune must
/// treat OIDs/commits named by a live (non-expired) marker as reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataScope {
    Branch,
    AgentTracesInflight,
    /// Durable import-side marker requiring foreground object-index repair
    /// before an otherwise idempotent replay may become a no-op.
    AgentImportIndexRepair,
    /// Repository-private authenticated SessionEnd recovery headers.
    AgentCapturePending,
    /// Retained headers requiring explicit repair; still count towards capacity.
    AgentCaptureQuarantine,
    /// Bounded base64 chunks; never enumerate through generic metadata/GC walkers.
    AgentCapturePendingChunk,
    /// Local sensitive catalog association; never hydrate through GC/export.
    AgentCaptureSessionAlias,
}

impl MetadataScope {
    pub fn as_str(self) -> &'static str {
        match self {
            MetadataScope::Branch => "branch",
            MetadataScope::AgentTracesInflight => "agent_traces_inflight",
            MetadataScope::AgentImportIndexRepair => "agent_import_index_repair",
            MetadataScope::AgentCapturePending => "agent_capture_pending",
            MetadataScope::AgentCaptureQuarantine => "agent_capture_quarantine",
            MetadataScope::AgentCapturePendingChunk => "agent_capture_pending_chunk",
            MetadataScope::AgentCaptureSessionAlias => "agent_capture_session_alias",
        }
    }
}

/// A single metadata entry as read back from the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataEntry {
    pub scope: String,
    pub target: String,
    pub key: String,
    pub value: String,
    pub value_type: String,
}

impl MetadataEntry {
    fn from_model(model: &metadata_kv::Model) -> Self {
        Self {
            scope: model.scope.clone(),
            target: model.target.clone(),
            key: model.key.clone(),
            value: model.value.clone(),
            value_type: model.value_type.clone(),
        }
    }
}

/// Validate a metadata key: non-empty, ≤ [`MAX_KEY_LEN`] bytes, no whitespace
/// or control characters. Branch/repo keys are exact, case-sensitive
/// identifiers; the REVISION scope matches keys ASCII case-insensitively
/// (the trailer convention — documented divergence).
pub fn validate_key(key: &str) -> std::result::Result<(), String> {
    if key.is_empty() {
        return Err("metadata key must not be empty".to_string());
    }
    if key.len() > MAX_KEY_LEN {
        return Err(format!("metadata key exceeds {MAX_KEY_LEN} bytes: '{key}'"));
    }
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "metadata key must not contain whitespace or control characters: '{key}'"
        ));
    }
    Ok(())
}

/// Validate a metadata value: ≤ [`MAX_VALUE_LEN`] bytes. The empty string is
/// legal and distinct from an absent key.
pub fn validate_value(value: &str) -> std::result::Result<(), String> {
    if value.len() > MAX_VALUE_LEN {
        return Err(format!(
            "metadata value exceeds {} bytes ({} given)",
            MAX_VALUE_LEN,
            value.len()
        ));
    }
    Ok(())
}

/// A metadata value's declared type (the `value_type` column reserved by the
/// 1.5 migration). The stored `value` is always TEXT: the decimal string for
/// `numeric`, standard base64 for `binary` (per the 1.5 design — raw payloads
/// therefore cap at ~3/4 of [`MAX_VALUE_LEN`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MetadataValueType {
    #[default]
    Text,
    Numeric,
    Binary,
}

impl MetadataValueType {
    pub fn as_str(self) -> &'static str {
        match self {
            MetadataValueType::Text => "text",
            MetadataValueType::Numeric => "numeric",
            MetadataValueType::Binary => "binary",
        }
    }
}

/// Validate a value against its declared type (set-time only — reads return
/// whatever the store carries). `numeric` accepts an i64 or a FINITE f64
/// (rejects empty/NaN/inf/overflow and surrounding whitespace), stored
/// exactly as given; `binary` must be valid standard base64 (an empty payload
/// is legal). Both remain bounded by [`validate_value`] on the stored text.
pub fn validate_typed_value(
    value_type: MetadataValueType,
    value: &str,
) -> std::result::Result<(), String> {
    match value_type {
        MetadataValueType::Text => Ok(()),
        MetadataValueType::Numeric => {
            // STRICT: no surrounding whitespace — the exact string that
            // validates is the exact string stored (no trim-then-store skew).
            let ok = value.parse::<i64>().is_ok()
                || value.parse::<f64>().is_ok_and(|number| number.is_finite());
            if ok {
                Ok(())
            } else {
                Err(format!(
                    "invalid --numeric value '{value}' (expected an integer or finite decimal, no surrounding whitespace)"
                ))
            }
        }
        MetadataValueType::Binary => {
            use base64::Engine;
            if base64::engine::general_purpose::STANDARD
                .decode(value)
                .is_ok()
            {
                Ok(())
            } else {
                // Do NOT echo the value: it can be up to 1 MiB of arbitrary
                // (possibly secret) data — report only its length.
                Err(format!(
                    "invalid --binary value ({} bytes given; expected standard base64 — raw payloads cap at ~{}KiB)",
                    value.len(),
                    MAX_VALUE_LEN * 3 / 4 / 1024
                ))
            }
        }
    }
}

/// Whether a recorded flag value counts as SET, parsed FAIL-CLOSED: the
/// explicit falsy spellings (`false`/`0`/`no`/`off`, case-insensitive,
/// trimmed) count as off; EVERYTHING else — including garbage — counts as on,
/// so a corrupted value can never silently disable protection.
fn truthy_fail_closed(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "false" | "0" | "no" | "off"
    )
}

/// The single owner API for the `metadata_kv` table.
pub struct MetadataKv;

impl MetadataKv {
    /// Private bounded reader. SQLite checks byte length before returning the
    /// value, so corrupted rows cannot allocate unbounded buffers in callers.
    /// `key_range` is half-open and uses the existing scope/target/key index.
    pub(crate) async fn list_bounded_with_conn<C: ConnectionTrait>(
        db: &C,
        scopes: &[MetadataScope],
        target: Option<&str>,
        key_range: Option<(&str, &str)>,
        limit: u64,
        value_cap: usize,
    ) -> Result<Vec<MetadataEntry>> {
        anyhow::ensure!(
            !scopes.is_empty()
                && scopes.len() <= 3
                && limit > 0
                && limit <= 129
                && value_cap <= MAX_VALUE_LEN,
            "invalid bounded metadata read; inspect the capture recovery configuration"
        );
        // Keys are capped like values: an out-of-band oversized key is never
        // hydrated into the caller, only reported through the safe-limit error.
        let mut values: Vec<sea_orm::Value> =
            vec![(MAX_KEY_LEN as i64).into(), (value_cap as i64).into()];
        let placeholders = scopes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut sql = format!(
            "SELECT scope, target,
             CASE WHEN length(CAST(key AS BLOB)) <= ? THEN key ELSE NULL END AS bounded_key,
             value_type,
             CASE WHEN length(CAST(value AS BLOB)) <= ? THEN value ELSE NULL END AS bounded_value
             FROM metadata_kv WHERE scope IN ({placeholders})"
        );
        values.extend(scopes.iter().map(|scope| scope.as_str().into()));
        if let Some(target) = target {
            sql.push_str(" AND target = ?");
            values.push(target.into());
        }
        if let Some((lower, upper)) = key_range {
            sql.push_str(" AND key >= ? AND key < ?");
            values.extend([lower.into(), upper.into()]);
        }
        sql.push_str(" ORDER BY target, key, scope LIMIT ?");
        values.push((limit as i64).into());
        db.query_all_raw(sea_orm::Statement::from_sql_and_values(
            db.get_database_backend(),
            sql,
            values,
        ))
        .await
        .context("cannot read bounded capture metadata; run `libra agent doctor`")?
        .into_iter()
        .map(|row| {
            let key: Option<String> = row.try_get_by("bounded_key")?;
            let value: Option<String> = row.try_get_by("bounded_value")?;
            Ok(MetadataEntry {
                scope: row.try_get_by("scope")?,
                target: row.try_get_by("target")?,
                key: key.context(
                    "capture metadata key exceeds its safe size limit; run `libra agent doctor`",
                )?,
                value_type: row.try_get_by("value_type")?,
                value: value.context(
                    "capture metadata exceeds its safe size limit; run `libra agent doctor`",
                )?,
            })
        })
        .collect()
    }

    /// Get one entry, or `None` when absent.
    pub async fn get_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
        target: &str,
        key: &str,
    ) -> Result<Option<MetadataEntry>> {
        let row = metadata_kv::Entity::find()
            .filter(metadata_kv::Column::Scope.eq(scope.as_str()))
            .filter(metadata_kv::Column::Target.eq(target))
            .filter(metadata_kv::Column::Key.eq(key))
            .one(db)
            .await
            .context("failed to query metadata_kv")?;
        Ok(row.as_ref().map(MetadataEntry::from_model))
    }

    /// Pool-acquiring counterpart of [`Self::get_with_conn`].
    pub async fn get(
        scope: MetadataScope,
        target: &str,
        key: &str,
    ) -> Result<Option<MetadataEntry>> {
        let db = get_db_conn_instance().await;
        Self::get_with_conn(&db, scope, target, key).await
    }

    /// Upsert one entry atomically (`INSERT … ON CONFLICT DO UPDATE` on the
    /// `(scope, target, key)` unique key — no find-then-insert race). Returns
    /// the PREVIOUS value when the key already existed.
    pub async fn set_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
        target: &str,
        key: &str,
        value: &str,
        value_type: MetadataValueType,
    ) -> Result<Option<String>> {
        let previous = Self::get_with_conn(db, scope, target, key)
            .await?
            .map(|entry| entry.value);
        let now = Utc::now().to_rfc3339();
        let active = metadata_kv::ActiveModel {
            scope: Set(scope.as_str().to_string()),
            target: Set(target.to_string()),
            key: Set(key.to_string()),
            value: Set(value.to_string()),
            value_type: Set(value_type.as_str().to_string()),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            ..Default::default()
        };
        let on_conflict = OnConflict::columns([
            metadata_kv::Column::Scope,
            metadata_kv::Column::Target,
            metadata_kv::Column::Key,
        ])
        .update_columns([
            metadata_kv::Column::Value,
            metadata_kv::Column::ValueType,
            metadata_kv::Column::UpdatedAt,
        ])
        .to_owned();
        metadata_kv::Entity::insert(active)
            .on_conflict(on_conflict)
            .exec(db)
            .await
            .context("failed to upsert metadata_kv entry")?;
        Ok(previous)
    }

    /// Pool-acquiring counterpart of [`Self::set_with_conn`].
    pub async fn set(
        scope: MetadataScope,
        target: &str,
        key: &str,
        value: &str,
        value_type: MetadataValueType,
    ) -> Result<Option<String>> {
        let db = get_db_conn_instance().await;
        Self::set_with_conn(&db, scope, target, key, value, value_type).await
    }

    /// Delete one entry; returns whether a row was removed.
    pub async fn unset_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
        target: &str,
        key: &str,
    ) -> Result<bool> {
        let result = metadata_kv::Entity::delete_many()
            .filter(metadata_kv::Column::Scope.eq(scope.as_str()))
            .filter(metadata_kv::Column::Target.eq(target))
            .filter(metadata_kv::Column::Key.eq(key))
            .exec(db)
            .await
            .context("failed to delete metadata_kv entry")?;
        Ok(result.rows_affected > 0)
    }

    /// Pool-acquiring counterpart of [`Self::unset_with_conn`].
    pub async fn unset(scope: MetadataScope, target: &str, key: &str) -> Result<bool> {
        let db = get_db_conn_instance().await;
        Self::unset_with_conn(&db, scope, target, key).await
    }

    /// List a target's entries, key-ordered, optionally filtered to a key
    /// prefix.
    pub async fn list_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
        target: &str,
        key_prefix: Option<&str>,
    ) -> Result<Vec<MetadataEntry>> {
        let mut query = metadata_kv::Entity::find()
            .filter(metadata_kv::Column::Scope.eq(scope.as_str()))
            .filter(metadata_kv::Column::Target.eq(target));
        if let Some(prefix) = key_prefix {
            query = query.filter(metadata_kv::Column::Key.starts_with(prefix));
        }
        let rows = query
            .order_by_asc(metadata_kv::Column::Key)
            .all(db)
            .await
            .context("failed to list metadata_kv entries")?;
        Ok(rows.iter().map(MetadataEntry::from_model).collect())
    }

    /// Pool-acquiring counterpart of [`Self::list_with_conn`].
    pub async fn list(
        scope: MetadataScope,
        target: &str,
        key_prefix: Option<&str>,
    ) -> Result<Vec<MetadataEntry>> {
        let db = get_db_conn_instance().await;
        Self::list_with_conn(&db, scope, target, key_prefix).await
    }

    /// List EVERY entry in a scope across all targets, ordered by
    /// `(target, key)`. Needed by scopes whose consumers scan the whole
    /// namespace rather than one target (e.g. the prune side of
    /// [`MetadataScope::AgentTracesInflight`], which must see the in-flight
    /// markers of every session).
    pub async fn list_scope_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
    ) -> Result<Vec<MetadataEntry>> {
        let rows = metadata_kv::Entity::find()
            .filter(metadata_kv::Column::Scope.eq(scope.as_str()))
            .order_by_asc(metadata_kv::Column::Target)
            .order_by_asc(metadata_kv::Column::Key)
            .all(db)
            .await
            .context("failed to list metadata_kv scope entries")?;
        Ok(rows.iter().map(MetadataEntry::from_model).collect())
    }

    /// Delete every entry for a target (branch-delete cascade). Returns the
    /// number of rows removed.
    pub async fn delete_all_for_target_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
        target: &str,
    ) -> Result<u64> {
        let result = metadata_kv::Entity::delete_many()
            .filter(metadata_kv::Column::Scope.eq(scope.as_str()))
            .filter(metadata_kv::Column::Target.eq(target))
            .exec(db)
            .await
            .context("failed to cascade-delete metadata_kv entries")?;
        Ok(result.rows_affected)
    }

    /// Move a target's entries to a new target name (branch rename). Any
    /// pre-existing rows under the destination are removed first so the
    /// `(scope, target, key)` unique key cannot abort mid-move (defensive —
    /// the branch CLI refuses to rename onto an existing branch).
    pub async fn rename_target_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
        old_target: &str,
        new_target: &str,
    ) -> Result<()> {
        Self::delete_all_for_target_with_conn(db, scope, new_target).await?;
        metadata_kv::Entity::update_many()
            .col_expr(
                metadata_kv::Column::Target,
                sea_orm::sea_query::Expr::value(new_target),
            )
            .filter(metadata_kv::Column::Scope.eq(scope.as_str()))
            .filter(metadata_kv::Column::Target.eq(old_target))
            .exec(db)
            .await
            .context("failed to move metadata_kv entries to the renamed target")?;
        Ok(())
    }

    /// Copy a target's entries to another target (branch copy). Destination
    /// rows are replaced (matching `branch -C`'s overwrite semantics).
    pub async fn copy_target_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
        from_target: &str,
        to_target: &str,
    ) -> Result<()> {
        // Self-copy (`branch -C x x`) must be a no-op: clearing the
        // destination first would delete the source rows and then copy
        // nothing, silently losing the metadata.
        if from_target == to_target {
            return Ok(());
        }
        Self::delete_all_for_target_with_conn(db, scope, to_target).await?;
        let entries = Self::list_with_conn(db, scope, from_target, None).await?;
        let now = Utc::now().to_rfc3339();
        for entry in entries {
            let active = metadata_kv::ActiveModel {
                scope: Set(entry.scope),
                target: Set(to_target.to_string()),
                key: Set(entry.key),
                value: Set(entry.value),
                value_type: Set(entry.value_type),
                created_at: Set(now.clone()),
                updated_at: Set(now.clone()),
                ..Default::default()
            };
            active
                .insert(db)
                .await
                .context("failed to copy metadata_kv entry")?;
        }
        Ok(())
    }

    /// Whether the branch is recorded as protected — the read entry the future
    /// branch-policy layer (lore.md 1.13) calls inside its authoritative
    /// transaction. FAIL-CLOSED: any value other than the explicit falsy
    /// spellings counts as protected.
    pub async fn is_protected_with_conn<C: ConnectionTrait>(db: &C, branch: &str) -> Result<bool> {
        Ok(
            Self::get_with_conn(db, MetadataScope::Branch, branch, KEY_PROTECT)
                .await?
                .is_some_and(|entry| truthy_fail_closed(&entry.value)),
        )
    }

    /// Whether the branch is recorded as archived (fail-closed, like
    /// [`Self::is_protected_with_conn`]).
    pub async fn is_archived_with_conn<C: ConnectionTrait>(db: &C, branch: &str) -> Result<bool> {
        Ok(
            Self::get_with_conn(db, MetadataScope::Branch, branch, KEY_ARCHIVE)
                .await?
                .is_some_and(|entry| truthy_fail_closed(&entry.value)),
        )
    }
}

// ─── Revision-scope metadata (lore.md §1.10) ────────────────────────────────
//
// Commits are immutable, so revision metadata is TWO layers behind this one
// owner API: the commit message's qualifying TRAILER block (read-only, parsed
// by the 1.9 engine) and a mutable, metadata-owned NOTES document under
// [`REVISION_NOTES_REF`] — one note blob per commit holding a versioned JSON
// doc. No new table, no migration: the doc rides the existing `notes` store
// (lore.md 1.10 explicitly directs "revision 用 trailers/notes", which is the
// recorded resolution of the §3.6 unified-table red line for this scope).
// Reads merge both layers with NOTES WINNING (the only mutable layer must be
// able to override a baked-in trailer). Key matching is ASCII
// case-insensitive across BOTH layers (the trailer convention) — a documented
// divergence from the exact-match branch/repo scopes. Note-layer values are
// LOCAL-ONLY (notes are not pushed); trailer values travel with the commit.

/// The metadata-owned notes ref. `libra notes --ref metadata` operating on the
/// same doc is an intended dual surface (like `config` for `--repo`).
pub const REVISION_NOTES_REF: &str = "refs/notes/metadata";

const REVISION_DOC_VERSION: u32 = 1;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct RevisionDoc {
    version: u32,
    /// BTreeMap for deterministic serialization ordering.
    entries: std::collections::BTreeMap<String, RevisionDocEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RevisionDocEntry {
    value: String,
    #[serde(rename = "type")]
    value_type: String,
}

/// Where a revision metadata value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionSource {
    /// The mutable notes layer (local-only).
    Note,
    /// The commit message's immutable trailer block.
    Trailer,
}

impl RevisionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            RevisionSource::Note => "note",
            RevisionSource::Trailer => "trailer",
        }
    }
}

/// One merged revision-metadata entry.
#[derive(Debug, Clone)]
pub struct RevisionEntry {
    pub key: String,
    pub value: String,
    pub value_type: String,
    pub source: RevisionSource,
}

/// Outcome of [`MetadataKv::revision_unset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionUnsetOutcome {
    /// A note entry was removed; no same-key trailer remains.
    Removed,
    /// A note entry was removed, but a same-key TRAILER value is now visible
    /// again (surface a notice).
    RemovedTrailerRemains,
    /// The key exists only as an immutable trailer — nothing to remove.
    OnlyTrailer,
    /// The key is absent from both layers.
    Absent,
}

fn corrupt_doc_error(oid: &str, detail: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "the revision metadata note for {oid} under {REVISION_NOTES_REF} is not a valid \
         metadata document ({detail}); repair or remove it with \
         'libra notes remove --ref metadata {oid}' (a hand-edited note via the dual \
         surface can cause this)"
    )
}

async fn load_revision_doc(oid: &str) -> Result<Option<RevisionDoc>> {
    Ok(load_revision_doc_with_blob(oid).await?.map(|(doc, _)| doc))
}

/// [`load_revision_doc`] plus the note BLOB hash, so a caller about to delete
/// the note can verify it still is the doc it loaded (see `revision_unset`).
async fn load_revision_doc_with_blob(oid: &str) -> Result<Option<(RevisionDoc, String)>> {
    match crate::internal::notes::show(REVISION_NOTES_REF, Some(oid)).await {
        Ok((_object, blob, text)) => {
            // Enforce the whole-doc bound on EXISTING docs too — a hand-edited
            // note via the dual surface must not bypass it (and must not feed
            // an unbounded blob to the JSON parser).
            if text.len() > MAX_VALUE_LEN {
                return Err(corrupt_doc_error(
                    oid,
                    &format!(
                        "document is {} bytes, over the {} byte bound",
                        text.len(),
                        MAX_VALUE_LEN
                    ),
                ));
            }
            let doc: RevisionDoc = serde_json::from_str(&text)
                .map_err(|error| corrupt_doc_error(oid, &error.to_string()))?;
            if doc.version != REVISION_DOC_VERSION {
                return Err(corrupt_doc_error(
                    oid,
                    &format!(
                        "unsupported document version {} (this binary supports {})",
                        doc.version, REVISION_DOC_VERSION
                    ),
                ));
            }
            Ok(Some((doc, blob)))
        }
        Err(crate::internal::notes::NotesError::NotFound { .. }) => Ok(None),
        Err(error) => Err(anyhow::anyhow!(
            "failed to read the revision metadata note for {oid}: {error}"
        )),
    }
}

async fn store_revision_doc(oid: &str, doc: &RevisionDoc) -> Result<()> {
    let text = serde_json::to_string_pretty(doc).context("failed to serialize metadata doc")?;
    // Bound the WHOLE document, not just individual values, so a commit's
    // metadata note cannot grow into a multi-MiB blob.
    if text.len() > MAX_VALUE_LEN {
        return Err(anyhow::anyhow!(
            "the revision metadata document for {oid} would exceed {} bytes ({} after this \
             change); remove entries first",
            MAX_VALUE_LEN,
            text.len()
        ));
    }
    crate::internal::notes::add(REVISION_NOTES_REF, oid, &text, true)
        .await
        .map_err(|error| anyhow::anyhow!("failed to write the revision metadata note: {error}"))?;
    Ok(())
}

impl MetadataKv {
    /// Get one revision metadata entry: the notes layer wins; otherwise the
    /// LAST matching trailer (the requested key is passed as an extra
    /// RECOGNIZED key so a mixed final block containing `<key>: v` qualifies —
    /// the 1.9 hook built for this).
    pub async fn revision_get(
        oid: &str,
        commit_message: &str,
        key: &str,
    ) -> Result<Option<RevisionEntry>> {
        if let Some(doc) = load_revision_doc(oid).await? {
            let hit = doc
                .entries
                .iter()
                .find(|(stored, _)| stored.eq_ignore_ascii_case(key));
            if let Some((stored_key, entry)) = hit {
                return Ok(Some(RevisionEntry {
                    key: stored_key.clone(),
                    value: entry.value.clone(),
                    value_type: entry.value_type.clone(),
                    source: RevisionSource::Note,
                }));
            }
        }
        let trailers =
            crate::internal::log::trailer::parse_trailers_with_recognized(commit_message, &[key]);
        Ok(trailers
            .iter()
            .rev()
            .find(|trailer| trailer.key_matches(key))
            .map(|trailer| RevisionEntry {
                key: trailer.key.clone(),
                value: trailer.value.clone(),
                value_type: "text".to_string(),
                source: RevisionSource::Trailer,
            }))
    }

    /// Set one revision metadata entry in the NOTES layer (the commit itself is
    /// never mutated). Returns the previous NOTE value (a shadowed trailer was
    /// never in the mutable layer, so it is not "previous"). Read-modify-write
    /// of the JSON doc; concurrent writers can lose an update (documented v1
    /// limitation, same class as the branch-scope pool-connection races).
    pub async fn revision_set(
        oid: &str,
        key: &str,
        value: &str,
        value_type: MetadataValueType,
    ) -> Result<Option<String>> {
        let mut doc = load_revision_doc(oid).await?.unwrap_or(RevisionDoc {
            version: REVISION_DOC_VERSION,
            entries: Default::default(),
        });
        // Case-insensitive upsert: drop any case-variant, keep the new spelling.
        let previous_key = doc
            .entries
            .keys()
            .find(|stored| stored.eq_ignore_ascii_case(key))
            .cloned();
        let previous = previous_key
            .and_then(|stored| doc.entries.remove(&stored))
            .map(|entry| entry.value);
        doc.entries.insert(
            key.to_string(),
            RevisionDocEntry {
                value: value.to_string(),
                value_type: value_type.as_str().to_string(),
            },
        );
        store_revision_doc(oid, &doc).await?;
        Ok(previous)
    }

    /// Remove one revision metadata entry from the NOTES layer. Trailer values
    /// are part of the immutable commit and cannot be unset.
    pub async fn revision_unset(
        oid: &str,
        commit_message: &str,
        key: &str,
    ) -> Result<RevisionUnsetOutcome> {
        let trailer_remains =
            crate::internal::log::trailer::parse_trailers_with_recognized(commit_message, &[key])
                .iter()
                .any(|trailer| trailer.key_matches(key));

        let Some((mut doc, loaded_blob)) = load_revision_doc_with_blob(oid).await? else {
            return Ok(if trailer_remains {
                RevisionUnsetOutcome::OnlyTrailer
            } else {
                RevisionUnsetOutcome::Absent
            });
        };
        let stored_key = doc
            .entries
            .keys()
            .find(|stored| stored.eq_ignore_ascii_case(key))
            .cloned();
        let Some(stored_key) = stored_key else {
            return Ok(if trailer_remains {
                RevisionUnsetOutcome::OnlyTrailer
            } else {
                RevisionUnsetOutcome::Absent
            });
        };
        doc.entries.remove(&stored_key);
        if doc.entries.is_empty() {
            // Deleting the whole note removes whatever blob is CURRENT — so
            // verify it still is the doc we loaded first, or a concurrent
            // `set` between our load and this delete would have its update
            // destroyed. (A residual window remains between this check and
            // the remove — same documented lost-update class as every
            // read-modify-write on this store — but the common race now gets
            // a retry instead of silent data loss.)
            match crate::internal::notes::show(REVISION_NOTES_REF, Some(oid)).await {
                Ok((_object, current_blob, _text)) if current_blob == loaded_blob => {}
                Ok(_) | Err(crate::internal::notes::NotesError::NotFound { .. }) => {
                    return Err(anyhow::anyhow!(
                        "the revision metadata note for {oid} changed while removing the last \
                         entry (concurrent writer?); re-run the command"
                    ));
                }
                Err(other) => {
                    return Err(anyhow::anyhow!(
                        "failed to re-verify the revision metadata note before removal: {other}"
                    ));
                }
            }
            crate::internal::notes::remove(REVISION_NOTES_REF, &[oid.to_string()])
                .await
                .map_err(|error| match error {
                    crate::internal::notes::NotesError::NotFound { .. } => anyhow::anyhow!(
                        "the revision metadata note for {oid} changed while removing the last \
                         entry (concurrent writer?); re-run the command"
                    ),
                    other => anyhow::anyhow!(
                        "failed to remove the emptied revision metadata note: {other}"
                    ),
                })?;
        } else {
            store_revision_doc(oid, &doc).await?;
        }
        Ok(if trailer_remains {
            RevisionUnsetOutcome::RemovedTrailerRemains
        } else {
            RevisionUnsetOutcome::Removed
        })
    }

    /// List merged revision metadata: note entries plus trailer occurrences
    /// (note shadows same-key trailers, ASCII case-insensitive). Sorted by key
    /// (case-insensitive); duplicate trailer keys keep message order. The
    /// trailer layer is parsed with the PLAIN rules here (the block must
    /// qualify on its own — the recognized-key strengthening applies to `get`
    /// only, documented). `key_prefix` filters case-insensitively (matching
    /// the scope's key semantics, unlike the branch scope's exact prefix).
    pub async fn revision_list(
        oid: &str,
        commit_message: &str,
        key_prefix: Option<&str>,
    ) -> Result<Vec<RevisionEntry>> {
        let doc = load_revision_doc(oid).await?;
        let mut entries: Vec<RevisionEntry> = Vec::new();
        if let Some(doc) = &doc {
            for (key, entry) in &doc.entries {
                entries.push(RevisionEntry {
                    key: key.clone(),
                    value: entry.value.clone(),
                    value_type: entry.value_type.clone(),
                    source: RevisionSource::Note,
                });
            }
        }
        for trailer in crate::internal::log::trailer::parse_trailers(commit_message) {
            let shadowed = doc.as_ref().is_some_and(|doc| {
                doc.entries
                    .keys()
                    .any(|stored| stored.eq_ignore_ascii_case(&trailer.key))
            });
            if !shadowed {
                entries.push(RevisionEntry {
                    key: trailer.key,
                    value: trailer.value,
                    value_type: "text".to_string(),
                    source: RevisionSource::Trailer,
                });
            }
        }
        if let Some(prefix) = key_prefix {
            let prefix = prefix.to_ascii_lowercase();
            entries.retain(|entry| entry.key.to_ascii_lowercase().starts_with(&prefix));
        }
        // Stable merged order: key (case-insensitive), notes before trailers
        // for equal keys, trailer duplicates keep message order (stable sort).
        entries.sort_by(|a, b| {
            a.key
                .to_ascii_lowercase()
                .cmp(&b.key.to_ascii_lowercase())
                .then_with(|| match (a.source, b.source) {
                    (RevisionSource::Note, RevisionSource::Trailer) => std::cmp::Ordering::Less,
                    (RevisionSource::Trailer, RevisionSource::Note) => std::cmp::Ordering::Greater,
                    _ => std::cmp::Ordering::Equal,
                })
        });
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_validation_rejects_empty_whitespace_and_oversize() {
        assert!(validate_key("protect").is_ok());
        assert!(validate_key("lineage.parent").is_ok());
        assert!(validate_key("").is_err());
        assert!(validate_key("has space").is_err());
        assert!(validate_key("has\ttab").is_err());
        assert!(validate_key(&"k".repeat(MAX_KEY_LEN + 1)).is_err());
        assert!(validate_key(&"k".repeat(MAX_KEY_LEN)).is_ok());
    }

    #[test]
    fn value_validation_allows_empty_and_bounds_size() {
        assert!(validate_value("").is_ok());
        assert!(validate_value("v").is_ok());
        assert!(validate_value(&"v".repeat(MAX_VALUE_LEN)).is_ok());
        assert!(validate_value(&"v".repeat(MAX_VALUE_LEN + 1)).is_err());
    }

    #[test]
    fn typed_value_validation_matrix() {
        use MetadataValueType::*;
        for ok in ["42", "-7", "+1", "007", "3.14", "-0"] {
            assert!(validate_typed_value(Numeric, ok).is_ok(), "{ok:?}");
        }
        for bad in [
            "", "NaN", "inf", "1e999", "0x10", "1,000", "abc", " 12 ", "12 ",
        ] {
            assert!(validate_typed_value(Numeric, bad).is_err(), "{bad:?}");
        }
        for ok in ["", "aGVsbG8=", "AAAA"] {
            assert!(validate_typed_value(Binary, ok).is_ok(), "{ok:?}");
        }
        for bad in ["not base64!", "aGVsbG8", "%%%"] {
            assert!(validate_typed_value(Binary, bad).is_err(), "{bad:?}");
        }
        assert!(validate_typed_value(Text, "anything at all").is_ok());
    }

    #[test]
    fn truthy_parse_is_fail_closed() {
        for on in ["true", "1", "yes", "on", "TRUE", " weird-garbage ", ""] {
            assert!(truthy_fail_closed(on), "{on:?} must count as set");
        }
        for off in ["false", "0", "no", "off", "FALSE", " Off "] {
            assert!(!truthy_fail_closed(off), "{off:?} must count as unset");
        }
    }
}
