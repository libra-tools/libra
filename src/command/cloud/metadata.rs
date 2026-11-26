#[cfg(test)]
use std::hash::Hash;
use std::{
    collections::hash_map::DefaultHasher,
    fs::File,
    hash::Hasher,
    io::{self, Read, Write},
    path::Path,
};

use sea_orm::{ActiveEnum, ConnectionTrait, Statement};

use super::*;

/// The `cloud.metadata_hash` of a whole metadata JSON held in memory; the
/// sync path computes the same value from the spool file with
/// [`metadata_hash_of_file`].
#[cfg(test)]
pub(super) fn calculate_metadata_hash(json: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    json.hash(&mut hasher);
    hasher.finish()
}

/// Writes through to `inner` while digesting every byte written, so a file
/// can later be checked against exactly what was written to it.
pub(super) struct DigestWriter<W: Write> {
    inner: W,
    digest: DefaultHasher,
}

impl<W: Write> DigestWriter<W> {
    pub(super) fn new(inner: W) -> Self {
        Self {
            inner,
            digest: DefaultHasher::new(),
        }
    }

    /// The digest of everything written so far.
    pub(super) fn digest(&self) -> u64 {
        self.digest.finish()
    }

    pub(super) fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for DigestWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.digest.write(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Read a file `chunk_size` bytes at a time and return its
/// [`calculate_metadata_hash`] (`<[u8] as Hash>::hash` writes the length and
/// then the bytes, and the hasher consumes bytes as a stream, so the value is
/// the same as for the whole buffer), the [`DigestWriter`] digest of its
/// bytes, and its length.
pub(super) fn metadata_hash_of_file(path: &Path, chunk_size: usize) -> io::Result<(u64, u64, u64)> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut hasher = DefaultHasher::new();
    hasher.write_usize(usize::try_from(len).map_err(io::Error::other)?);
    let mut digest = DefaultHasher::new();
    let mut buffer = vec![0u8; chunk_size.max(1)];
    let mut hashed = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.write(&buffer[..read]);
        digest.write(&buffer[..read]);
        hashed += read as u64;
    }
    if hashed != len {
        return Err(io::Error::other(format!(
            "the file changed while it was hashed ({len} bytes expected, {hashed} read)"
        )));
    }
    Ok((hasher.finish(), digest.finish(), len))
}

/// Checks that the bytes an upload sends are the frozen refs: the frozen
/// length and the same `cloud.metadata_hash` the skip decision used, so the
/// stored hash always describes what was uploaded.
struct FrozenRefsCheck {
    hasher: DefaultHasher,
    sent: u64,
    len: u64,
    hash: u64,
}

impl FrozenRefsCheck {
    fn new(len: u64, hash: u64) -> CloudResult<Self> {
        let mut hasher = DefaultHasher::new();
        hasher.write_usize(usize::try_from(len).map_err(|_| {
            CloudError::Generic("the frozen refs are too large for this platform".to_string())
        })?);
        Ok(Self {
            hasher,
            sent: 0,
            len,
            hash,
        })
    }
}

impl crate::utils::storage::remote::UploadCheck for FrozenRefsCheck {
    fn update(&mut self, bytes: &[u8]) {
        self.hasher.write(bytes);
        self.sent += bytes.len() as u64;
    }

    fn verify(&mut self) -> Result<(), String> {
        if self.sent == self.len && self.hasher.finish() == self.hash {
            return Ok(());
        }
        Err(format!(
            "the frozen refs spool changed after it was hashed ({} bytes frozen, {} read for the upload); no metadata was uploaded and cloud.metadata_hash was not updated. Re-run `libra cloud sync`",
            self.len, self.sent
        ))
    }
}

/// Upload the refs projection frozen in this run's snapshot spool (PI-09),
/// streaming it from the spool file (PI-27); refs written after the freeze
/// wait for the next sync.
pub(super) async fn sync_metadata(
    r2_storage: &RemoteStorage,
    progress: &dyn CloudSyncProgress,
    frozen: &super::snapshot::FrozenRefs,
) -> CloudResult<MetadataSyncOutcome> {
    progress.on_metadata_starting();
    let current_hash = frozen.hash;

    // Check if hash matches last sync.
    if let Some(stored) = ConfigKv::get("cloud.metadata_hash")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        && let Ok(stored_hash) = stored.parse::<u64>()
        && stored_hash == current_hash
    {
        progress.on_metadata_skipped();
        return Ok(MetadataSyncOutcome::Skipped);
    }

    // Clear the stored hash before the upload and record the new one only
    // after it succeeds, so the stored hash never describes bytes other than
    // the uploaded ones: if recording fails, no hash is stored and the next
    // sync uploads again instead of skipping on a stale value.
    ConfigKv::unset("cloud.metadata_hash").await.map_err(|error| {
        CloudError::Generic(format!(
            "clear cloud.metadata_hash before the metadata upload: {error}; no metadata was uploaded. Re-run `libra cloud sync`"
        ))
    })?;

    #[cfg(test)]
    if let Some(before_upload) = &frozen.before_upload {
        before_upload(frozen.path.clone()).await;
    }
    let mut check = FrozenRefsCheck::new(frozen.len, frozen.hash)?;
    r2_storage
        .put_metadata_from_file(
            &frozen.path,
            frozen.upload_chunk_size,
            &mut check,
            std::sync::Arc::clone(&frozen.report),
        )
        .await
        .map_err(|e| CloudError::R2(format!("Failed to upload metadata: {}", e)))?;

    #[cfg(test)]
    let recorded = if frozen.fail_hash_record {
        Err(anyhow::anyhow!("injected config write failure"))
    } else {
        ConfigKv::set("cloud.metadata_hash", &current_hash.to_string(), false).await
    };
    #[cfg(not(test))]
    let recorded = ConfigKv::set("cloud.metadata_hash", &current_hash.to_string(), false).await;
    recorded.map_err(|error| {
        CloudError::Generic(format!(
            "the refs metadata was uploaded, but recording cloud.metadata_hash failed: {error}; no hash is stored, so the next `libra cloud sync` uploads the metadata again"
        ))
    })?;

    progress.on_metadata_synced(frozen.references);
    Ok(MetadataSyncOutcome::Synced {
        references: frozen.references,
    })
}

/// The connection-private TEMP table the freeze copies the refs into
/// (PI-27). Its key columns spell out the sort of
/// [`serialize_reference_projection`] — `name`, then `remote`, each NULL
/// first and then by bytes, then `kind` — with `id` breaking ties as the
/// stable sort does over refs read in `id` order; an index on them lets every
/// projection page seek instead of sorting the whole set again. TEMP objects
/// live in the connection's own temporary database (a file, see
/// `PRAGMA temp_store`), never in the repository database or its lock.
const REFS_COPY_TABLE: &str = "temp.libra_cloud_sync_refs";
const REFS_COPY_ORDER: &str = "has_name, name_key, has_remote, remote_key, kind, id";
const REFS_COPY_COLUMNS: &str = "id, name, kind, \"commit\", remote, worktree_id";

/// The projection position of a ref: where the next page starts.
#[derive(Debug, Clone)]
pub(super) struct ReferenceProjectionKey {
    has_name: bool,
    name: String,
    has_remote: bool,
    remote: String,
    kind: String,
    id: i64,
}

impl ReferenceProjectionKey {
    pub(super) fn of(model: &reference::Model) -> Self {
        Self {
            has_name: model.name.is_some(),
            name: model.name.clone().unwrap_or_default(),
            has_remote: model.remote.is_some(),
            remote: model.remote.clone().unwrap_or_default(),
            kind: model.kind.to_value(),
            id: model.id,
        }
    }
}

/// (Re)create the empty TEMP copy of the refs and its order index.
pub(super) async fn prepare_reference_copy<C: ConnectionTrait>(
    conn: &C,
) -> Result<(), sea_orm::DbErr> {
    for sql in [
        format!("DROP TABLE IF EXISTS {REFS_COPY_TABLE}"),
        format!(
            "CREATE TABLE {REFS_COPY_TABLE} (id INTEGER PRIMARY KEY, name TEXT, kind TEXT NOT NULL, \"commit\" TEXT, remote TEXT, worktree_id TEXT, has_name INTEGER NOT NULL, name_key TEXT NOT NULL, has_remote INTEGER NOT NULL, remote_key TEXT NOT NULL)"
        ),
        format!(
            "CREATE INDEX temp.libra_cloud_sync_refs_order ON libra_cloud_sync_refs ({REFS_COPY_ORDER})"
        ),
    ] {
        conn.execute_unprepared(&sql).await?;
    }
    Ok(())
}

/// Copy the next page of refs after `after_id`, in `id` order and up to
/// `boundary`, into the TEMP copy: one short statement on the repository
/// database. Returns how many refs it copied and the last copied id.
pub(super) async fn copy_reference_page<C: ConnectionTrait>(
    conn: &C,
    after_id: i64,
    boundary: i64,
    limit: usize,
) -> Result<(u64, Option<i64>), sea_orm::DbErr> {
    let backend = conn.get_database_backend();
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let copied = conn
        .execute_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "INSERT INTO {REFS_COPY_TABLE} ({REFS_COPY_COLUMNS}, has_name, name_key, has_remote, remote_key) SELECT {REFS_COPY_COLUMNS}, name IS NOT NULL, IFNULL(name, ''), remote IS NOT NULL, IFNULL(remote, '') FROM main.reference WHERE id > ? AND id <= ? ORDER BY id LIMIT ?"
            ),
            [after_id.into(), boundary.into(), limit.into()],
        ))
        .await?
        .rows_affected();
    let last = conn
        .query_one_raw(Statement::from_string(
            backend,
            format!("SELECT MAX(id) AS max_id FROM {REFS_COPY_TABLE}"),
        ))
        .await?
        .map(|row| row.try_get::<Option<i64>>("", "max_id"))
        .transpose()?
        .flatten();
    Ok((copied, last))
}

/// The SQL of one projection page from the TEMP copy: with `after`, the
/// keyset seek past that position.
fn reference_projection_page_sql(after: bool) -> String {
    if after {
        format!(
            "SELECT {REFS_COPY_COLUMNS} FROM {REFS_COPY_TABLE} WHERE ({REFS_COPY_ORDER}) > (?, ?, ?, ?, ?, ?) ORDER BY {REFS_COPY_ORDER} LIMIT ?"
        )
    } else {
        format!(
            "SELECT {REFS_COPY_COLUMNS} FROM {REFS_COPY_TABLE} ORDER BY {REFS_COPY_ORDER} LIMIT ?"
        )
    }
}

/// One page of the refs projection in upload order: at most `limit` refs of
/// the TEMP copy after `after`, read through its order index.
pub(super) async fn reference_projection_page<C: ConnectionTrait>(
    conn: &C,
    after: Option<&ReferenceProjectionKey>,
    limit: usize,
) -> Result<Vec<reference::Model>, sea_orm::DbErr> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let values: Vec<sea_orm::Value> = match after {
        None => vec![limit.into()],
        Some(key) => vec![
            i64::from(key.has_name).into(),
            key.name.clone().into(),
            i64::from(key.has_remote).into(),
            key.remote.clone().into(),
            key.kind.clone().into(),
            key.id.into(),
            limit.into(),
        ],
    };
    reference::Entity::find()
        .from_raw_sql(Statement::from_sql_and_values(
            conn.get_database_backend(),
            reference_projection_page_sql(after.is_some()),
            values,
        ))
        .all(conn)
        .await
}

/// Writes the refs projection one ref at a time as exactly the bytes
/// [`serialize_reference_projection`] produces for the same refs in the same
/// order: `[`, the refs separated by `,`, then `]`.
pub(super) struct ReferenceProjectionWriter<W: Write> {
    out: W,
    references: usize,
}

impl<W: Write> ReferenceProjectionWriter<W> {
    pub(super) fn new(mut out: W) -> io::Result<Self> {
        out.write_all(b"[")?;
        Ok(Self { out, references: 0 })
    }

    pub(super) fn push(&mut self, model: &reference::Model) -> io::Result<()> {
        if self.references > 0 {
            self.out.write_all(b",")?;
        }
        serde_json::to_writer(&mut self.out, model)?;
        self.references += 1;
        Ok(())
    }

    /// Flush what has been written so far.
    pub(super) fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    /// Close the array; returns the writer and how many refs it holds.
    pub(super) fn finish(mut self) -> io::Result<(W, usize)> {
        self.out.write_all(b"]")?;
        self.out.flush()?;
        Ok((self.out, self.references))
    }
}

/// Keep the cloud metadata contract limited to references. Repository-private
/// `metadata_kv` scopes never enter this projection. The sync path streams
/// the same bytes with [`reference_projection_page`] and
/// [`ReferenceProjectionWriter`]; this whole-collection form is the
/// reference they are checked against.
#[cfg(test)]
pub(super) fn serialize_reference_projection(
    references: Vec<reference::Model>,
) -> CloudResult<(Vec<reference::Model>, Vec<u8>)> {
    let mut sorted_refs = references;
    sorted_refs.sort_by(|a, b| {
        let a_kind = format!("{:?}", a.kind);
        let b_kind = format!("{:?}", b.kind);
        let a_key = (&a.name, &a.remote, a_kind);
        let b_key = (&b.name, &b.remote, b_kind);
        a_key.cmp(&b_key)
    });

    let json = serde_json::to_vec(&sorted_refs)
        .map_err(|e| CloudError::Generic(format!("Failed to serialize metadata: {}", e)))?;
    Ok((sorted_refs, json))
}

/// Convert a storage-layer [`crate::storage::git_store::RefRecord`] into the
/// cloud wire [`reference::Model`] representation, keeping the R2 metadata JSON
/// wire protocol identical between SQLite and Redb backends.
#[cfg(feature = "storage-redb")]
pub(super) fn ref_record_to_model(
    id: i64,
    record: crate::storage::git_store::RefRecord,
) -> reference::Model {
    use crate::storage::git_store::{RefKey, RefTarget};

    match (record.key, record.target) {
        (RefKey::LocalHead { worktree_id }, RefTarget::Symbolic(branch_name)) => reference::Model {
            id,
            name: Some(branch_name),
            kind: reference::ConfigKind::Head,
            commit: None,
            remote: None,
            worktree_id,
        },
        (RefKey::LocalHead { worktree_id }, RefTarget::Direct(oid)) => reference::Model {
            id,
            name: None,
            kind: reference::ConfigKind::Head,
            commit: Some(oid),
            remote: None,
            worktree_id,
        },
        (RefKey::RemoteHead { remote }, RefTarget::Symbolic(branch_name)) => reference::Model {
            id,
            name: Some(branch_name),
            kind: reference::ConfigKind::Head,
            commit: None,
            remote: Some(remote),
            worktree_id: None,
        },
        (RefKey::RemoteHead { remote }, RefTarget::Direct(oid)) => reference::Model {
            id,
            name: None,
            kind: reference::ConfigKind::Head,
            commit: Some(oid),
            remote: Some(remote),
            worktree_id: None,
        },
        (RefKey::LocalBranch { name }, target) => {
            let commit = match target {
                RefTarget::Direct(oid) | RefTarget::Symbolic(oid) => Some(oid),
            };
            reference::Model {
                id,
                name: Some(name),
                kind: reference::ConfigKind::Branch,
                commit,
                remote: None,
                worktree_id: None,
            }
        }
        (RefKey::RemoteBranch { remote, name }, target) => {
            let commit = match target {
                RefTarget::Direct(oid) | RefTarget::Symbolic(oid) => Some(oid),
            };
            reference::Model {
                id,
                name: Some(name),
                kind: reference::ConfigKind::Branch,
                commit,
                remote: Some(remote),
                worktree_id: None,
            }
        }
        (RefKey::Tag { name }, target) => {
            let commit = match target {
                RefTarget::Direct(oid) | RefTarget::Symbolic(oid) => Some(oid),
            };
            reference::Model {
                id,
                name: Some(name),
                kind: reference::ConfigKind::Tag,
                commit,
                remote: None,
                worktree_id: None,
            }
        }
    }
}

/// Sort [`reference::Model`] rows in the exact cloud metadata projection order
/// (`name`, `remote`, `kind`, with `id` tie-breaking) used by
/// [`reference_projection_page`] and [`serialize_reference_projection`].
#[cfg(feature = "storage-redb")]
pub(super) fn sort_reference_models(references: &mut [reference::Model]) {
    references.sort_by(|a, b| {
        let a_kind = format!("{:?}", a.kind);
        let b_kind = format!("{:?}", b.kind);
        (&a.name, &a.remote, a_kind, a.id).cmp(&(&b.name, &b.remote, b_kind, b.id))
    });
}

#[cfg(test)]
mod tests {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};

    use super::*;
    use crate::internal::{
        metadata::{MetadataKv, MetadataScope, MetadataValueType},
        model::reference::ConfigKind,
    };

    #[tokio::test]
    async fn reasoning_identity_is_not_in_cloud_projection() {
        let dir = tempfile::tempdir().unwrap();
        let conn =
            crate::internal::db::create_database(dir.path().join("libra.db").to_str().unwrap())
                .await
                .unwrap();
        crate::internal::model::reference::ActiveModel {
            name: Set(Some("refs/heads/main".to_owned())),
            kind: Set(ConfigKind::Branch),
            commit: Set(Some("a".repeat(40))),
            remote: Set(None),
            worktree_id: Set(None),
            ..Default::default()
        }
        .insert(&conn)
        .await
        .unwrap();
        MetadataKv::set_with_conn(
            &conn,
            MetadataScope::AgentReasoningIdentity,
            "session-private-target",
            "identity-v1",
            "private-identity-canary",
            MetadataValueType::Text,
        )
        .await
        .unwrap();

        let references = reference::Entity::find().all(&conn).await.unwrap();
        let (_, projected) = serialize_reference_projection(references).unwrap();
        let projection = String::from_utf8(projected).unwrap();
        assert!(projection.contains("refs/heads/main"));
        assert!(!projection.contains("private-identity-canary"));
        assert!(!projection.contains("agent_reasoning_identity"));
    }

    #[test]
    fn reference_projection_kind_order_matches_the_stored_value() {
        use sea_orm::Iterable;

        // `serialize_reference_projection` sorts by the Debug name of the
        // kind and the SQL order by the stored value; they must be equal.
        for kind in ConfigKind::iter() {
            assert_eq!(format!("{kind:?}"), kind.to_value());
        }
    }

    #[tokio::test]
    async fn capture_snapshot_paging_ref_pages_seek_the_order_index() {
        let dir = tempfile::tempdir().unwrap();
        let conn =
            crate::internal::db::create_database(dir.path().join("libra.db").to_str().unwrap())
                .await
                .unwrap();
        prepare_reference_copy(&conn).await.unwrap();
        // Both the first page and every later keyset page walk the order
        // index; none sorts the copy again.
        for after in [false, true] {
            let values: Vec<sea_orm::Value> = if after {
                vec![
                    1i64.into(),
                    "main".into(),
                    0i64.into(),
                    "".into(),
                    "Branch".into(),
                    7i64.into(),
                    10i64.into(),
                ]
            } else {
                vec![10i64.into()]
            };
            let plan = conn
                .query_all_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    format!(
                        "EXPLAIN QUERY PLAN {}",
                        reference_projection_page_sql(after)
                    ),
                    values,
                ))
                .await
                .unwrap();
            let details: Vec<String> = plan
                .iter()
                .map(|row| row.try_get::<String>("", "detail").unwrap())
                .collect();
            // The first page walks the index from its start; every later page
            // seeks straight to its cursor instead of scanning up to it.
            let access = if after { "SEARCH" } else { "SCAN" };
            assert!(
                details.iter().any(|detail| detail.starts_with(access)
                    && detail.contains("libra_cloud_sync_refs_order")),
                "{access}: {details:?}"
            );
            assert!(
                !details.iter().any(|detail| detail.contains("TEMP B-TREE")),
                "{details:?}"
            );
        }
    }
}
