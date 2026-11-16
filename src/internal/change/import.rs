//! Bounded, offline import of immutable legacy Git commits into change facts.
//!
//! The caller supplies the current import operation, never a historical success.
//! A manifest describes one frozen base and its complete finite ancestor view.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};

use anyhow::{Context, Result, ensure};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{ChangeId, ChangeRevisionBuilder, ChangeStore};
use crate::{
    internal::{config::ConfigKv, head::Head, object_format},
    utils::{client_storage::ClientStorage, util},
};

pub const IMPORT_COMMAND: &str = "revision import";
const MAX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;
const MAX_COMMIT_BYTES: u64 = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryManifest {
    pub schema_version: u32,
    pub repo_id: String,
    pub object_format: String,
    pub base_commit: String,
    pub visibility_policy: String,
    pub cutoff_ms: Option<i64>,
    pub commit_oids: Vec<String>,
    pub commit_inventory_sha256: String,
    pub objects: Vec<HistoryObject>,
    pub object_inventory_sha256: String,
    pub shallow_cuts: Vec<ShallowCut>,
    pub sources: Vec<HistorySource>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryObject {
    pub oid: String,
    pub kind: String,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShallowCut {
    pub child: String,
    pub parent: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistorySource {
    pub commit_oid: String,
    /// External availability evidence is audited by the caller. The importer
    /// cannot infer publication time from a Git timestamp.
    pub available_at_ms: i64,
}

#[derive(Debug, Serialize)]
pub struct ImportReceipt {
    pub manifest_sha256: String,
    pub base_commit: String,
    pub imported: usize,
    pub reused: usize,
    pub finite_history: bool,
    pub mappings: Vec<ImportMapping>,
}

#[derive(Debug, Serialize)]
pub struct ImportMapping {
    pub commit_oid: String,
    pub change_id: String,
    pub historical_outcome: &'static str,
}

/// Read once with an enforced byte bound; the operation metadata pins its digest.
pub fn read_manifest(path: &Path) -> Result<(HistoryManifest, String)> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .with_context(|| {
            format!(
                "cannot open history manifest {}; check the path and permissions",
                path.display()
            )
        })?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("cannot read history manifest; check the file and retry")?;
    ensure!(
        bytes.len() as u64 <= MAX_MANIFEST_BYTES,
        "history manifest exceeds the 2 MiB limit; split the input"
    );
    let manifest = serde_json::from_slice(&bytes)
        .context("invalid history manifest; use schema version 1 and only documented fields")?;
    Ok((manifest, format!("{:x}", Sha256::digest(&bytes))))
}

struct CommitFact {
    tree: String,
    parents: Vec<String>,
    committed_at_ms: i64,
}

fn parse_commit(bytes: &[u8]) -> Result<CommitFact> {
    let header = bytes
        .split(|b| *b == b'\n')
        .take_while(|line| !line.is_empty());
    let mut parents = Vec::new();
    let mut committed_at_ms = None;
    let mut tree = None;
    for line in header {
        if let Some(value) = line.strip_prefix(b"tree ") {
            ensure!(tree.is_none(), "commit has multiple tree headers");
            tree = Some(
                std::str::from_utf8(value)
                    .context("invalid tree encoding")?
                    .to_string(),
            );
        }
        if let Some(value) = line.strip_prefix(b"parent ") {
            parents.push(
                std::str::from_utf8(value)
                    .context("invalid commit parent encoding")?
                    .to_string(),
            );
        }
        if let Some(value) = line.strip_prefix(b"committer ") {
            let timestamp = value
                .rsplit(|b| *b == b' ')
                .nth(1)
                .context("commit has no committer timestamp")?;
            let seconds: i64 = std::str::from_utf8(timestamp)
                .context("invalid timestamp encoding")?
                .parse()
                .context("invalid committer timestamp")?;
            committed_at_ms = seconds.checked_mul(1000);
        }
    }
    Ok(CommitFact {
        tree: tree.context("commit has no tree header")?,
        parents,
        committed_at_ms: committed_at_ms.context("commit has no valid committer timestamp")?,
    })
}

fn validate_shape(manifest: &HistoryManifest, repo_id: &str) -> Result<()> {
    ensure!(
        manifest.schema_version == 1,
        "unsupported history manifest version; use version 1"
    );
    ensure!(
        manifest.repo_id == repo_id,
        "history manifest belongs to another repository; regenerate it for this repository identity"
    );
    ensure!(
        matches!(manifest.object_format.as_str(), "sha1" | "sha256"),
        "history import currently supports sha1 and sha256 only"
    );
    ensure!(
        manifest.object_format == git_internal::hash::get_hash_kind().as_str(),
        "history manifest object format differs from this repository"
    );
    ensure!(
        matches!(
            manifest.visibility_policy.as_str(),
            "base_snapshot" | "strict_time"
        ),
        "unknown history visibility policy"
    );
    ensure!(
        manifest.visibility_policy != "strict_time" || manifest.cutoff_ms.is_some(),
        "strict_time requires cutoff_ms; publication time must be independently audited"
    );
    ensure!(
        !manifest.commit_oids.is_empty() && manifest.commit_oids.len() <= 5000,
        "history view must contain 1..5000 commits"
    );
    ensure!(
        !manifest.sources.is_empty() && manifest.sources.len() <= 128,
        "history import requires 1..128 sources per manifest"
    );
    ensure!(
        manifest.shallow_cuts.len() <= 5000,
        "too many shallow boundary edges"
    );
    ensure!(
        !manifest.objects.is_empty() && manifest.objects.len() <= 20000,
        "history view requires 1..20000 objects"
    );
    let mut inventory = manifest.commit_oids.clone();
    inventory.sort();
    ensure!(
        inventory.windows(2).all(|pair| pair[0] != pair[1]),
        "duplicate commit in history inventory"
    );
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("{}\n", inventory.join("\n")).as_bytes())
    );
    ensure!(
        digest == manifest.commit_inventory_sha256,
        "history commit inventory digest mismatch; regenerate the manifest"
    );
    let mut sources = BTreeSet::new();
    for source in &manifest.sources {
        ensure!(
            sources.insert(source.commit_oid.as_str()),
            "duplicate history source"
        );
        ensure!(
            source.available_at_ms >= 0,
            "history source availability must be a nonnegative timestamp"
        );
    }
    Ok(())
}

fn validate_objects(manifest: &HistoryManifest, storage: &ClientStorage) -> Result<()> {
    let kind = object_format::parse_config_value(&manifest.object_format)?;
    let mut commits = BTreeMap::new();
    let mut total_bytes = 0usize;
    let mut objects = BTreeMap::new();
    let mut object_inventory = Vec::new();
    for object in &manifest.objects {
        ensure!(
            matches!(object.kind.as_str(), "commit" | "tree" | "blob")
                && object.size <= 16 * 1024 * 1024,
            "unsupported or oversized history object"
        );
        let hash = object_format::parse_hex_for_kind(kind, &object.oid)?;
        ensure!(
            hash.to_string() == object.oid,
            "object ids must use canonical lowercase hex"
        );
        ensure!(
            storage.get_object_type(&hash)?.to_string() == object.kind,
            "history object type mismatch"
        );
        let bytes = storage.get_with_limit(&hash, 16 * 1024 * 1024)?;
        ensure!(
            bytes.len() as u64 == object.size,
            "history object size mismatch"
        );
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .context("history size overflow")?;
        ensure!(
            total_bytes <= MAX_TOTAL_BYTES,
            "history objects exceed 64 MiB; use a smaller view"
        );
        let mut canonical = format!("{} {}\0", object.kind, bytes.len()).into_bytes();
        canonical.extend_from_slice(&bytes);
        ensure!(
            object_format::digest(kind, &canonical) == hash,
            "history object {} failed its canonical hash",
            object.oid
        );
        ensure!(
            objects
                .insert(object.oid.clone(), (object.kind.as_str(), bytes))
                .is_none(),
            "duplicate history object"
        );
        object_inventory.push(format!("{} {} {}\n", object.oid, object.kind, object.size));
    }
    object_inventory.sort();
    ensure!(
        format!("{:x}", Sha256::digest(object_inventory.concat().as_bytes()))
            == manifest.object_inventory_sha256,
        "history object inventory digest mismatch"
    );
    for oid in &manifest.commit_oids {
        let hash = object_format::parse_hex_for_kind(kind, oid)?;
        ensure!(
            hash.to_string() == *oid,
            "history object ids must use canonical lowercase hex"
        );
        ensure!(
            storage.get_object_type(&hash)?
                == git_internal::internal::object::types::ObjectType::Commit,
            "history member {oid} is not a commit"
        );
        let bytes = storage
            .get_with_limit(&hash, MAX_COMMIT_BYTES)
            .with_context(|| {
                format!(
                    "history commit {oid} is missing or exceeds 1 MiB; supply a complete local view"
                )
            })?;
        let mut canonical = format!("commit {}\0", bytes.len()).into_bytes();
        canonical.extend_from_slice(&bytes);
        ensure!(
            object_format::digest(kind, &canonical) == hash,
            "history commit {oid} failed its canonical object hash"
        );
        commits.insert(oid.clone(), parse_commit(&bytes)?);
    }
    let object_commits: BTreeSet<_> = objects
        .iter()
        .filter(|(_, (kind, _))| *kind == "commit")
        .map(|(oid, _)| oid.clone())
        .collect();
    ensure!(
        object_commits == commits.keys().cloned().collect(),
        "commit and object inventories disagree"
    );
    let mut cuts = BTreeSet::new();
    for cut in &manifest.shallow_cuts {
        ensure!(
            cuts.insert((cut.child.clone(), cut.parent.clone())),
            "duplicate shallow boundary edge"
        );
        let child = commits
            .get(&cut.child)
            .context("shallow boundary child is outside the view")?;
        ensure!(
            child.parents.contains(&cut.parent) && !commits.contains_key(&cut.parent),
            "shallow boundary is not a missing parent edge"
        );
        let parent = object_format::parse_hex_for_kind(kind, &cut.parent)?;
        ensure!(
            !storage.exist_checked(&parent)?,
            "declared shallow parent is locally present; regenerate the full visible view"
        );
    }
    let mut stack = vec![manifest.base_commit.clone()];
    let mut ancestors = BTreeSet::new();
    while let Some(oid) = stack.pop() {
        if !ancestors.insert(oid.clone()) {
            continue;
        }
        let commit = commits.get(&oid).with_context(|| {
            format!("history view is missing commit {oid}; no network fetch is allowed")
        })?;
        for parent in &commit.parents {
            if !cuts.contains(&(oid.clone(), parent.clone())) {
                stack.push(parent.clone());
            }
        }
    }
    ensure!(
        ancestors.len() == commits.len(),
        "history inventory contains a non-ancestor or future commit"
    );
    let mut reachable_objects = ancestors.clone();
    let mut trees: Vec<_> = commits.values().map(|commit| commit.tree.clone()).collect();
    let oid_len = if manifest.object_format == "sha1" {
        20
    } else {
        32
    };
    while let Some(oid) = trees.pop() {
        if !reachable_objects.insert(oid.clone()) {
            continue;
        }
        let (object_kind, raw) = objects
            .get(&oid)
            .context("history tree is absent from the object inventory")?;
        ensure!(
            *object_kind == "tree",
            "history tree edge points to a different object type"
        );
        let mut cursor = 0;
        while cursor < raw.len() {
            let rest = &raw[cursor..];
            let space = rest
                .iter()
                .position(|byte| *byte == b' ')
                .context("invalid tree mode")?;
            let zero = rest
                .iter()
                .position(|byte| *byte == 0)
                .context("unterminated tree path")?;
            ensure!(
                zero > space + 1 && zero + 1 + oid_len <= rest.len(),
                "truncated tree entry"
            );
            let name = &rest[space + 1..zero];
            ensure!(
                name != b"." && name != b".." && !name.contains(&b'/'),
                "unsafe history tree path"
            );
            let target = hex::encode(&rest[zero + 1..zero + 1 + oid_len]);
            let mode = &rest[..space];
            cursor += zero + 1 + oid_len;
            if mode == b"160000" {
                continue;
            }
            if mode == b"40000" || mode == b"040000" {
                trees.push(target);
            } else {
                ensure!(
                    matches!(mode, b"100644" | b"100755" | b"120000"),
                    "unsupported tree mode"
                );
                ensure!(
                    objects
                        .get(&target)
                        .is_some_and(|(kind, _)| *kind == "blob"),
                    "history blob is absent or has the wrong type"
                );
                reachable_objects.insert(target);
            }
        }
    }
    ensure!(
        reachable_objects == objects.keys().cloned().collect(),
        "history inventory contains unreachable or future objects"
    );
    if let Some(cutoff) = manifest.cutoff_ms
        && manifest.visibility_policy == "strict_time"
    {
        ensure!(
            commits.values().all(|c| c.committed_at_ms <= cutoff),
            "strict_time rejects a commit or base newer than cutoff_ms"
        );
    }
    for source in &manifest.sources {
        ensure!(
            ancestors.contains(&source.commit_oid),
            "history source is not in the frozen base ancestor view"
        );
        if let Some(cutoff) = manifest.cutoff_ms {
            ensure!(
                source.available_at_ms <= cutoff,
                "history source became available after cutoff_ms"
            );
        }
    }
    Ok(())
}

/// Import facts through the existing revision writer in one transaction.
/// Object validation is offline and precedes writes. The caller must retain the
/// digest-addressed manifest and external availability evidence with the receipt.
pub async fn import_history(
    db: &DatabaseConnection,
    manifest: &HistoryManifest,
    manifest_sha256: &str,
    operation_id: &str,
) -> Result<ImportReceipt> {
    let repo_id = ConfigKv::get_with_conn(db, "libra.repoid")
        .await?
        .context("repository identity is missing; initialize the repository first")?
        .value;
    validate_shape(manifest, &repo_id)?;
    let storage_root = util::try_get_storage_path(None)?;
    ensure!(
        crate::command::replace::snapshot().is_empty(),
        "history import refuses replacement refs; use an isolated unmodified object view"
    );
    let storage = ClientStorage::init_local_existing(storage_root.join("objects"));
    validate_objects(manifest, &storage)?;

    let transaction = crate::internal::db::begin_write_transaction(db).await?;
    let tip = Head::current_commit_result_with_conn(&transaction)
        .await?
        .context("repository HEAD has no commit")?;
    ensure!(
        tip.to_string() == manifest.base_commit,
        "repository HEAD differs from manifest base; use a fresh base-specific repository"
    );
    let operation = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT repo_id, command_name, args_digest, status FROM operation WHERE op_id = ?",
            [operation_id.to_string().into()],
        ))
        .await?
        .context(
            "history import requires a persisted current operation; invoke 'libra revision import'",
        )?;
    ensure!(
        crate::internal::operation::current_operation_id().as_deref() == Some(operation_id)
            && operation.try_get::<String>("", "status")? == "running"
            && operation.try_get::<String>("", "repo_id")? == repo_id
            && operation
                .try_get::<Option<String>>("", "command_name")?
                .as_deref()
                == Some(IMPORT_COMMAND)
            && operation
                .try_get::<Option<String>>("", "args_digest")?
                .as_deref()
                == Some(manifest_sha256),
        "history manifest or operation changed; retry with the frozen manifest"
    );
    let rows = transaction.query_all_raw(Statement::from_sql_and_values(DbBackend::Sqlite,
        "SELECT r.commit_oid FROM change_revision r JOIN change_identity i ON i.change_id = r.change_id WHERE i.repo_id = ? LIMIT 5001",
        [repo_id.clone().into()])).await?;
    ensure!(
        rows.len() <= 5000,
        "repository has more than 5000 revision facts; use a fresh base-specific import repository"
    );
    let allowed: BTreeSet<_> = manifest.commit_oids.iter().collect();
    let admitted_sources: BTreeSet<_> = manifest
        .sources
        .iter()
        .map(|source| source.commit_oid.as_str())
        .collect();
    for row in rows {
        let commit_oid = row.try_get::<String>("", "commit_oid")?;
        ensure!(
            allowed.contains(&commit_oid),
            "repository already contains change facts outside this base view; use a fresh base-specific repository"
        );
        ensure!(
            manifest.cutoff_ms.is_none() || admitted_sources.contains(commit_oid.as_str()),
            "repository contains source facts omitted from this cutoff manifest; use a fresh base/cutoff-specific repository"
        );
    }
    let store = ChangeStore::new(db.clone());
    let kind = object_format::parse_config_value(&manifest.object_format)?;
    let mut mappings = Vec::new();
    let mut imported = 0;
    for source in &manifest.sources {
        let existing = store
            .change_id_for_commit_on(&transaction, &repo_id, &source.commit_oid)
            .await?;
        let id = match existing {
            Some(id) => {
                let revision = ChangeStore::revision_on(&transaction, id, &source.commit_oid)
                    .await?
                    .context(
                        "existing change is missing its revision; repair repository metadata",
                    )?;
                ensure!(
                    revision.visibility == super::RevisionVisibility::Visible,
                    "history source has a hidden revision; import cannot restore revoked visibility"
                );
                id
            }
            None => {
                let oid = object_format::parse_hex_for_kind(kind, &source.commit_oid)?;
                let id = ChangeId::synthetic_for_commit(kind.as_str(), oid.as_ref());
                ChangeRevisionBuilder::for_rewrite(db.clone(), &repo_id, operation_id, id)
                    .with_identity_origin("synthetic")
                    .set_commit_oid(&source.commit_oid)
                    .build_on(&transaction)
                    .await?;
                imported += 1;
                id
            }
        };
        mappings.push(ImportMapping {
            commit_oid: source.commit_oid.clone(),
            change_id: id.to_string(),
            historical_outcome: "unknown",
        });
    }
    if imported > 0 {
        // Imported facts have no fictional historical AI link. Invalidate any
        // existing generation explicitly until a normal Memory rebuild succeeds.
        transaction.execute_raw(Statement::from_sql_and_values(DbBackend::Sqlite,
            "UPDATE memory_projection_state SET fingerprint = ? WHERE repo_id = ? AND source_kind = 'commit'",
            [format!("history-import:{manifest_sha256}").into(), repo_id.into()])).await?;
    }
    transaction
        .commit()
        .await
        .context("history import did not commit; retry the same frozen manifest")?;
    Ok(ImportReceipt {
        manifest_sha256: manifest_sha256.to_string(),
        base_commit: manifest.base_commit.clone(),
        imported,
        reused: manifest.sources.len() - imported,
        finite_history: !manifest.shallow_cuts.is_empty(),
        mappings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_commit_preserves_multiple_parent_order_and_time() {
        let fact = parse_commit(b"tree tree\nparent first\nparent second\nauthor a <a> 1 +0000\ncommitter c <c> 42 +0000\n\nsubject\n").unwrap();
        assert_eq!(fact.parents, ["first", "second"]);
        assert_eq!(fact.committed_at_ms, 42000);
        assert!(parse_commit(b"tree t\n\nmissing committer").is_err());
    }

    #[test]
    fn manifests_reject_extra_fields_including_nested_gold() {
        let raw = r#"{"schema_version":1,"repo_id":"r","object_format":"sha1","base_commit":"b","visibility_policy":"base_snapshot","cutoff_ms":null,"commit_oids":[],"commit_inventory_sha256":"d","objects":[],"object_inventory_sha256":"d","shallow_cuts":[],"sources":[{"commit_oid":"a","available_at_ms":0,"patch":"hidden"}]}"#;
        let error = serde_json::from_str::<HistoryManifest>(raw).unwrap_err();
        assert!(
            error.to_string().contains("unknown field `patch`"),
            "{error}"
        );
    }
}
