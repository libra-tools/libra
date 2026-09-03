use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    str::FromStr,
};

use git_internal::{
    hash::ObjectHash,
    internal::object::{
        ObjectTrait,
        commit::Commit,
        signature::{Signature, SignatureType},
        tree::{Tree, TreeItem, TreeItemMode},
    },
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    error::{MemoryDamagePoint, MemoryWriterError, MemoryWriterErrorKind},
    policy::policy_snapshot_digest,
    validation::{parse_memory_event_v1, parse_memory_note_v1},
};
use crate::utils::{
    object::{read_git_object_bounded_validated, write_git_object},
    tree::sort_tree_items_for_git,
};

const MAX_COMMIT_BYTES: u64 = 1024 * 1024;
const MAX_TREE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 32 * 1024;
const MAX_MEMORY_TREE_ENTRIES: usize = 65_536;
const MEMORY_MANIFEST_SCHEMA_V1: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct MemoryManifestV1 {
    pub(super) schema_version: u32,
    pub(super) scope_key: String,
    pub(super) last_event_seq: u64,
    pub(super) commit_count: u64,
    pub(super) policy_version: String,
    pub(super) policy_snapshot_digest: String,
    pub(super) writer_version: u32,
    pub(super) index_version: u32,
}

impl MemoryManifestV1 {
    pub(super) fn initial(policy_version: String) -> Result<Self, MemoryWriterError> {
        let policy_snapshot_digest = policy_snapshot_digest(&policy_version)?;
        Ok(Self {
            schema_version: MEMORY_MANIFEST_SCHEMA_V1,
            scope_key: "repo".to_string(),
            last_event_seq: 0,
            commit_count: 0,
            policy_snapshot_digest,
            policy_version,
            writer_version: 1,
            index_version: 1,
        })
    }

    pub(super) fn validate(&self) -> Result<(), MemoryWriterError> {
        let expected_policy_digest =
            policy_snapshot_digest(&self.policy_version).map_err(|_| {
                MemoryWriterError::new(
                    MemoryWriterErrorKind::CorruptHistory,
                    "Memory manifest names an unsupported policy snapshot",
                )
            })?;
        if self.schema_version != MEMORY_MANIFEST_SCHEMA_V1
            || self.scope_key != "repo"
            || self.policy_version.is_empty()
            || self.policy_snapshot_digest != expected_policy_digest
            || self.writer_version != 1
            || self.index_version == 0
        {
            return Err(corrupt(
                "Memory manifest has an unsupported or invalid shape",
            ));
        }
        Ok(())
    }
}

pub(super) struct MemoryTreeSnapshot {
    pub(super) root_items: Vec<TreeItem>,
    pub(super) manifest: MemoryManifestV1,
}

pub(super) struct MemoryCommitObjects {
    pub(super) revision_oid: ObjectHash,
    pub(super) commit_oid: ObjectHash,
}

#[derive(Clone)]
pub(super) struct MemoryHistoryRecord {
    pub(super) source_commit_oid: ObjectHash,
    pub(super) event: super::domain::MemoryEventV1,
    pub(super) revision_oid: Option<ObjectHash>,
    pub(super) note: Option<super::domain::MemoryNoteV1>,
}

pub(super) struct MemoryHistoryDelta {
    pub(super) manifest: MemoryManifestV1,
    pub(super) records: Vec<MemoryHistoryRecord>,
}

pub(super) struct MemoryCommitInput<'a> {
    pub(super) note_id: &'a str,
    pub(super) namespace: &'a str,
    pub(super) note_bytes: &'a [u8],
    pub(super) events: &'a [MemoryEventInput<'a>],
}

pub(super) struct MemoryEventInput<'a> {
    pub(super) event_seq: u64,
    pub(super) event_id: &'a str,
    pub(super) event_bytes: &'a [u8],
}

pub(super) fn load_snapshot(
    storage_path: &Path,
    head: Option<ObjectHash>,
    policy_version: &str,
) -> Result<MemoryTreeSnapshot, MemoryWriterError> {
    let Some(head) = head else {
        return Ok(MemoryTreeSnapshot {
            root_items: Vec::new(),
            manifest: MemoryManifestV1::initial(policy_version.to_string())?,
        });
    };

    let commit = load_commit(storage_path, head)?;
    if commit.parent_commit_ids.len() > 1 {
        return Err(corrupt("Memory history contains a merge commit"));
    }
    let root_items = load_tree(storage_path, commit.tree_id)?;
    let manifest_entry = root_items
        .iter()
        .find(|item| item.name == "manifest.json" && item.mode == TreeItemMode::Blob)
        .ok_or_else(|| corrupt("Memory commit is missing manifest.json"))?;
    let (object_type, manifest_bytes) =
        read_git_object_bounded_validated(storage_path, &manifest_entry.id, MAX_MANIFEST_BYTES)
            .map_err(|error| history_error("read Memory manifest", error))?;
    if object_type != "blob" {
        return Err(corrupt("Memory manifest entry is not a blob"));
    }
    let manifest: MemoryManifestV1 = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| corrupt("Memory manifest is not valid JSON"))?;
    manifest.validate()?;
    if manifest.commit_count == 0 {
        return Err(corrupt("persisted Memory manifest has no commits"));
    }
    let expected_parent_count = usize::from(manifest.commit_count > 1);
    if commit.parent_commit_ids.len() != expected_parent_count {
        return Err(corrupt(
            "Memory commit parent count does not match its event sequence",
        ));
    }
    let _ = validate_append_edge(storage_path, &commit, &root_items, &manifest)?;
    if manifest.policy_version != policy_version {
        return Err(MemoryWriterError::new(
            MemoryWriterErrorKind::PolicyRejected,
            "proposal policy version does not match the Memory history policy",
        ));
    }
    Ok(MemoryTreeSnapshot {
        root_items,
        manifest,
    })
}

/// Read only the current authoritative manifest for O(1) watermark checks.
///
/// This validates the head commit, root shape, manifest contract, and policy,
/// but deliberately does not enumerate event or note trees. Full closure and
/// replay validation belongs to `plan_rebuild` / `rebuild --dry-run`.
pub(super) fn load_head_manifest(
    storage_path: &Path,
    head: ObjectHash,
    policy_version: &str,
) -> Result<MemoryManifestV1, MemoryWriterError> {
    let head_damage = MemoryDamagePoint::MemoryHead { oid: head };
    let commit =
        load_commit(storage_path, head).map_err(|error| error.with_damage_point(head_damage))?;
    if commit.parent_commit_ids.len() > 1 {
        return Err(
            corrupt("Memory history contains a merge commit").with_damage_point(head_damage)
        );
    }
    let root_items = load_tree(storage_path, commit.tree_id)
        .map_err(|error| error.with_damage_point(head_damage))?;
    validate_root_shape(&root_items).map_err(|error| error.with_damage_point(head_damage))?;
    let manifest = load_manifest(storage_path, &root_items, "head status")
        .map_err(|error| error.with_damage_point(head_damage))?;
    if manifest.commit_count == 0 {
        return Err(
            corrupt("persisted Memory manifest has no commits").with_damage_point(head_damage)
        );
    }
    let expected_parent_count = usize::from(manifest.commit_count > 1);
    if commit.parent_commit_ids.len() != expected_parent_count {
        return Err(
            corrupt("Memory commit parent count does not match its manifest")
                .with_damage_point(head_damage),
        );
    }
    if manifest.policy_version != policy_version {
        return Err(MemoryWriterError::new(
            MemoryWriterErrorKind::PolicyRejected,
            "Memory status policy does not match the authoritative history",
        ));
    }
    Ok(manifest)
}

fn validate_append_edge(
    storage_path: &Path,
    commit: &Commit,
    root_items: &[TreeItem],
    manifest: &MemoryManifestV1,
) -> Result<Vec<MemoryHistoryRecord>, MemoryWriterError> {
    validate_root_shape(root_items)?;
    let (parent_items, parent_manifest) = match commit.parent_commit_ids.as_slice() {
        [] => {
            if manifest.commit_count != 1 || manifest.last_event_seq == 0 {
                return Err(corrupt(
                    "Memory root commit has an invalid manifest sequence",
                ));
            }
            (None, None)
        }
        [parent_oid] => {
            let parent = load_commit(storage_path, *parent_oid)?;
            let parent_items = load_tree(storage_path, parent.tree_id)?;
            validate_root_shape(&parent_items)?;
            let parent_manifest = load_manifest(storage_path, &parent_items, "parent")?;
            let expected_parent_count = usize::from(parent_manifest.commit_count > 1);
            if parent.parent_commit_ids.len() != expected_parent_count
                || parent_manifest.commit_count.checked_add(1) != Some(manifest.commit_count)
                || parent_manifest.last_event_seq >= manifest.last_event_seq
                || parent_manifest.scope_key != manifest.scope_key
                || parent_manifest.policy_version != manifest.policy_version
                || parent_manifest.policy_snapshot_digest != manifest.policy_snapshot_digest
                || parent_manifest.writer_version != manifest.writer_version
                || parent_manifest.index_version != manifest.index_version
            {
                return Err(corrupt("Memory parent manifest edge is discontinuous"));
            }
            (Some(parent_items), Some(parent_manifest))
        }
        _ => return Err(corrupt("Memory history contains a merge commit")),
    };

    let previous_seq = parent_manifest
        .as_ref()
        .map_or(0, |parent| parent.last_event_seq);
    validate_event_edge(
        storage_path,
        parent_items.as_deref(),
        root_items,
        previous_seq,
        manifest.last_event_seq,
        commit.id,
    )
}

/// Read and validate the first-parent suffix ending at `head`.
///
/// `after` is the already-projected ancestor and is excluded from the result.
/// Supplying a non-ancestor fails closed rather than silently rebuilding from
/// an unrelated history.
pub(super) fn load_history_delta(
    storage_path: &Path,
    head: ObjectHash,
    after: Option<ObjectHash>,
    policy_version: &str,
) -> Result<MemoryHistoryDelta, MemoryWriterError> {
    load_history_delta_bounded(storage_path, head, after, policy_version, 4_096)
}

pub(super) fn load_history_delta_bounded(
    storage_path: &Path,
    head: ObjectHash,
    after: Option<ObjectHash>,
    policy_version: &str,
    max_commits: usize,
) -> Result<MemoryHistoryDelta, MemoryWriterError> {
    if max_commits == 0 {
        return Err(corrupt("Memory replay commit budget must be positive"));
    }

    if Some(head) == after {
        let snapshot = load_snapshot(storage_path, Some(head), policy_version)?;
        return Ok(MemoryHistoryDelta {
            manifest: snapshot.manifest,
            records: Vec::new(),
        });
    }

    let mut suffix = Vec::new();
    let mut cursor = head;
    loop {
        if Some(cursor) == after {
            break;
        }
        if suffix.len() == max_commits {
            return Err(corrupt("Memory replay exceeds the commit budget"));
        }
        let commit = load_commit(storage_path, cursor)?;
        if commit.parent_commit_ids.len() > 1 {
            return Err(corrupt("Memory history contains a merge commit"));
        }
        let root_items = load_tree(storage_path, commit.tree_id)?;
        let manifest = load_manifest(storage_path, &root_items, "replay")?;
        if manifest.policy_version != policy_version {
            return Err(MemoryWriterError::new(
                MemoryWriterErrorKind::PolicyRejected,
                "Memory replay policy does not match the authoritative history",
            ));
        }
        let parent = commit.parent_commit_ids.first().copied();
        suffix.push((cursor, commit, root_items, manifest));
        match parent {
            Some(parent) => cursor = parent,
            None if after.is_none() => break,
            None => {
                return Err(corrupt(
                    "Memory projection watermark is not an ancestor of the pinned head",
                ));
            }
        }
    }
    suffix.reverse();
    let manifest = suffix
        .last()
        .map(|(_, _, _, manifest)| manifest.clone())
        .ok_or_else(|| corrupt("Memory replay suffix is empty"))?;
    let mut records = Vec::new();
    for (_, commit, root_items, commit_manifest) in &suffix {
        records.extend(validate_append_edge(
            storage_path,
            commit,
            root_items,
            commit_manifest,
        )?);
    }
    Ok(MemoryHistoryDelta { manifest, records })
}

fn validate_root_shape(items: &[TreeItem]) -> Result<(), MemoryWriterError> {
    if items.len() != 3
        || items.iter().any(|item| {
            !matches!(item.name.as_str(), "manifest.json" | "events" | "notes")
                || (item.name == "manifest.json" && item.mode != TreeItemMode::Blob)
                || (item.name != "manifest.json" && item.mode != TreeItemMode::Tree)
        })
    {
        return Err(corrupt("Memory root tree contains an unsupported entry"));
    }
    Ok(())
}

fn load_manifest(
    storage_path: &Path,
    items: &[TreeItem],
    label: &'static str,
) -> Result<MemoryManifestV1, MemoryWriterError> {
    let entry = items
        .iter()
        .find(|item| item.name == "manifest.json" && item.mode == TreeItemMode::Blob)
        .ok_or_else(|| corrupt("Memory commit is missing manifest.json"))?;
    let (object_type, bytes) =
        read_git_object_bounded_validated(storage_path, &entry.id, MAX_MANIFEST_BYTES)
            .map_err(|error| history_error("read Memory manifest", error))?;
    if object_type != "blob" {
        return Err(corrupt("Memory manifest entry is not a blob"));
    }
    let manifest: MemoryManifestV1 = serde_json::from_slice(&bytes).map_err(|_| {
        MemoryWriterError::new(
            MemoryWriterErrorKind::CorruptHistory,
            format!("{label} Memory manifest is not valid JSON"),
        )
    })?;
    manifest.validate()?;
    Ok(manifest)
}

pub(super) fn write_revision_commit(
    storage_path: &Path,
    parent: Option<ObjectHash>,
    mut snapshot: MemoryTreeSnapshot,
    input: MemoryCommitInput<'_>,
) -> Result<MemoryCommitObjects, MemoryWriterError> {
    let note_entry_count = if snapshot.root_items.is_empty() {
        0
    } else {
        note_blob_map(storage_path, &snapshot.root_items)?.entry_count
    };
    ensure_append_capacity(snapshot.manifest.last_event_seq, note_entry_count)?;
    let revision_oid = write_git_object(storage_path, "blob", input.note_bytes)
        .map_err(|error| storage_error("write MemoryNote blob", error))?;
    let revision_file = format!("{revision_oid}.json");
    let note_path = [
        "notes".to_string(),
        encode_segment(input.namespace),
        input.note_id.to_string(),
        revision_file,
    ];
    upsert_blob(
        storage_path,
        &mut snapshot.root_items,
        &note_path,
        revision_oid,
    )?;

    for event in input.events {
        let expected_seq = snapshot
            .manifest
            .last_event_seq
            .checked_add(1)
            .ok_or_else(|| corrupt("Memory event sequence overflowed"))?;
        if event.event_seq != expected_seq {
            return Err(corrupt(
                "Memory commit input contains a non-contiguous event",
            ));
        }
        let event_oid = write_git_object(storage_path, "blob", event.event_bytes)
            .map_err(|error| storage_error("write MemoryEvent blob", error))?;
        snapshot.manifest.last_event_seq = expected_seq;
        let event_file = format!("{expected_seq:020}-{}.json", event.event_id);
        let event_path = ["events".to_string(), event_file];
        upsert_blob(
            storage_path,
            &mut snapshot.root_items,
            &event_path,
            event_oid,
        )?;
    }
    snapshot.manifest.commit_count = snapshot
        .manifest
        .commit_count
        .checked_add(1)
        .ok_or_else(|| corrupt("Memory commit count overflowed"))?;

    let manifest_bytes = serde_json::to_vec(&snapshot.manifest)
        .map_err(|_| corrupt("Memory manifest could not be serialized"))?;
    let manifest_oid = write_git_object(storage_path, "blob", &manifest_bytes)
        .map_err(|error| storage_error("write Memory manifest", error))?;
    upsert_blob(
        storage_path,
        &mut snapshot.root_items,
        &["manifest.json".to_string()],
        manifest_oid,
    )?;

    let root_oid = write_tree(storage_path, snapshot.root_items)?;
    let author = Signature::new(
        SignatureType::Author,
        "Libra Memory".to_string(),
        "memory@libra".to_string(),
    );
    let committer = Signature::new(
        SignatureType::Committer,
        "Libra Memory".to_string(),
        "memory@libra".to_string(),
    );
    let commit = Commit::new(
        author,
        committer,
        root_oid,
        parent.into_iter().collect(),
        &format!("Record Memory event {}", snapshot.manifest.last_event_seq),
    );
    let commit_bytes = commit
        .to_data()
        .map_err(|error| storage_error("serialize Memory commit", error))?;
    let commit_oid = write_git_object(storage_path, "commit", &commit_bytes)
        .map_err(|error| storage_error("write Memory commit", error))?;
    Ok(MemoryCommitObjects {
        revision_oid,
        commit_oid,
    })
}

fn ensure_append_capacity(
    current_event_count: u64,
    current_note_entry_count: usize,
) -> Result<(), MemoryWriterError> {
    let max_events_before_append = u64::try_from(MAX_MEMORY_TREE_ENTRIES - 2)
        .map_err(|_| corrupt("Memory validation budget is invalid"))?;
    if current_event_count > max_events_before_append
        || current_note_entry_count > MAX_MEMORY_TREE_ENTRIES - 3
    {
        return Err(MemoryWriterError::new(
            MemoryWriterErrorKind::StorageFailure,
            "Memory history reached the writer validation capacity",
        ));
    }
    Ok(())
}

pub(super) fn load_note_bytes(
    storage_path: &Path,
    revision_oid: ObjectHash,
) -> Result<Vec<u8>, MemoryWriterError> {
    let (object_type, bytes) =
        read_git_object_bounded_validated(storage_path, &revision_oid, 256 * 1024)
            .map_err(|error| history_error("read MemoryNote revision", error))?;
    if object_type != "blob" {
        return Err(corrupt("Memory revision OID does not name a blob"));
    }
    Ok(bytes)
}

fn upsert_blob(
    storage_path: &Path,
    tree_items: &mut Vec<TreeItem>,
    path: &[String],
    blob_oid: ObjectHash,
) -> Result<(), MemoryWriterError> {
    let (name, rest) = path
        .split_first()
        .ok_or_else(|| corrupt("empty Memory tree path"))?;
    if rest.is_empty() {
        tree_items.retain(|item| item.name != *name);
        tree_items.push(TreeItem::new(TreeItemMode::Blob, blob_oid, name.clone()));
        sort_tree_items_for_git(tree_items);
        return Ok(());
    }

    let mut child_items = match tree_items.iter().find(|item| item.name == *name) {
        Some(item) if item.mode == TreeItemMode::Tree => load_tree(storage_path, item.id)?,
        Some(_) => return Err(corrupt("Memory tree path collides with a non-tree entry")),
        None => Vec::new(),
    };
    upsert_blob(storage_path, &mut child_items, rest, blob_oid)?;
    let child_oid = write_tree(storage_path, child_items)?;
    tree_items.retain(|item| item.name != *name);
    tree_items.push(TreeItem::new(TreeItemMode::Tree, child_oid, name.clone()));
    sort_tree_items_for_git(tree_items);
    Ok(())
}

fn write_tree(
    storage_path: &Path,
    mut items: Vec<TreeItem>,
) -> Result<ObjectHash, MemoryWriterError> {
    sort_tree_items_for_git(&mut items);
    let tree = Tree::from_tree_items(items)
        .map_err(|error| storage_error("construct Memory tree", error))?;
    let bytes = tree
        .to_data()
        .map_err(|error| storage_error("serialize Memory tree", error))?;
    write_git_object(storage_path, "tree", &bytes)
        .map_err(|error| storage_error("write Memory tree", error))
}

fn load_commit(storage_path: &Path, oid: ObjectHash) -> Result<Commit, MemoryWriterError> {
    let (object_type, bytes) =
        read_git_object_bounded_validated(storage_path, &oid, MAX_COMMIT_BYTES)
            .map_err(|error| history_error("read Memory commit", error))?;
    if object_type != "commit" {
        return Err(corrupt("Memory ref does not name a commit"));
    }
    Commit::from_bytes(&bytes, oid).map_err(|error| history_error("parse Memory commit", error))
}

fn load_tree(storage_path: &Path, oid: ObjectHash) -> Result<Vec<TreeItem>, MemoryWriterError> {
    let (object_type, bytes) =
        read_git_object_bounded_validated(storage_path, &oid, MAX_TREE_BYTES)
            .map_err(|error| history_error("read Memory tree", error))?;
    if object_type != "tree" {
        return Err(corrupt("Memory tree OID does not name a tree"));
    }
    Tree::from_bytes(&bytes, oid)
        .map(|tree| tree.tree_items)
        .map_err(|error| history_error("parse Memory tree", error))
}

fn validate_event_edge(
    storage_path: &Path,
    parent_root: Option<&[TreeItem]>,
    root_items: &[TreeItem],
    previous_seq: u64,
    last_event_seq: u64,
    source_commit_oid: ObjectHash,
) -> Result<Vec<MemoryHistoryRecord>, MemoryWriterError> {
    let events = tree_entries(storage_path, root_items, "events")?;
    if events.len() > MAX_MEMORY_TREE_ENTRIES {
        return Err(corrupt(
            "Memory event tree exceeds the writer validation budget",
        ));
    }
    if u64::try_from(events.len()).ok() != Some(last_event_seq) {
        return Err(corrupt(
            "Memory manifest sequence does not match the event count",
        ));
    }
    let parent_events = match parent_root {
        Some(parent) => tree_entries(storage_path, parent, "events")?,
        None => Vec::new(),
    };
    if u64::try_from(parent_events.len()).ok() != Some(previous_seq)
        || events.len() <= parent_events.len()
        || !events.starts_with(&parent_events)
    {
        return Err(corrupt("Memory event tree did not append to its parent"));
    }

    let mut event_ids = HashSet::with_capacity(events.len());
    for (index, item) in events.iter().enumerate() {
        let sequence = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| corrupt("Memory event index overflowed"))?;
        let damage_point = MemoryDamagePoint::EventObject {
            commit_oid: source_commit_oid,
            event_seq: sequence,
            event_oid: item.id,
        };
        if item.mode != TreeItemMode::Blob {
            return Err(corrupt("Memory event entry is not a blob").with_damage_point(damage_point));
        }
        let event_id = event_id_from_filename(&item.name, sequence)
            .map_err(|error| error.with_damage_point(damage_point))?;
        if !event_ids.insert(event_id) {
            return Err(corrupt("Memory event ID is duplicated").with_damage_point(damage_point));
        }
    }

    let parent_notes = match parent_root {
        Some(parent) => note_blob_map(storage_path, parent)?,
        None => NoteTreeIndex::default(),
    };
    let notes = note_blob_map(storage_path, root_items)?;
    if parent_notes
        .blobs
        .iter()
        .any(|(path, oid)| notes.blobs.get(path) != Some(oid))
    {
        return Err(corrupt("Memory note tree did not append to its parent"));
    }

    let tail = &events[parent_events.len()..];
    let mut revision_events = BTreeMap::new();
    let mut records = Vec::with_capacity(tail.len());
    for (offset, item) in tail.iter().enumerate() {
        let sequence = previous_seq
            .checked_add(u64::try_from(offset).map_err(|_| corrupt("Memory event overflowed"))?)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| corrupt("Memory event sequence overflowed"))?;
        let damage_point = MemoryDamagePoint::EventObject {
            commit_oid: source_commit_oid,
            event_seq: sequence,
            event_oid: item.id,
        };
        let event_id = event_id_from_filename(&item.name, sequence)
            .map_err(|error| error.with_damage_point(damage_point))?;
        let (object_type, bytes) =
            read_git_object_bounded_validated(storage_path, &item.id, 128 * 1024).map_err(
                |error| {
                    history_error("read MemoryEvent blob", error).with_damage_point(damage_point)
                },
            )?;
        if object_type != "blob" {
            return Err(
                corrupt("Memory event OID does not name a blob").with_damage_point(damage_point)
            );
        }
        let event = parse_memory_event_v1(&bytes).map_err(|error| {
            MemoryWriterError::new(
                MemoryWriterErrorKind::CorruptHistory,
                format!("persisted MemoryEvent is invalid: {error}"),
            )
            .with_damage_point(damage_point)
        })?;
        if event.event_seq != sequence || event.event_id != event_id {
            return Err(
                corrupt("Memory event filename and payload identity disagree")
                    .with_damage_point(damage_point),
            );
        }
        if event.action != super::domain::MemoryEventAction::TaxonomyExpanded {
            let note_id = event.note_id.ok_or_else(|| {
                corrupt("Memory revision event has no note ID").with_damage_point(damage_point)
            })?;
            let revision_oid = event
                .revision_oid
                .as_deref()
                .ok_or_else(|| {
                    corrupt("Memory revision event has no revision OID")
                        .with_damage_point(damage_point)
                })
                .and_then(|value| {
                    parse_oid(value).map_err(|error| error.with_damage_point(damage_point))
                })?;
            let note_bytes = load_note_bytes(storage_path, revision_oid)
                .map_err(|error| error.with_damage_point(damage_point))?;
            let note = parse_memory_note_v1(&note_bytes).map_err(|error| {
                MemoryWriterError::new(
                    MemoryWriterErrorKind::CorruptHistory,
                    format!("event references an invalid MemoryNote revision: {error}"),
                )
                .with_damage_point(damage_point)
            })?;
            let expected_path = format!(
                "{}/{}/{}.json",
                encode_segment(&note.namespace),
                note.note_id,
                revision_oid
            );
            if note.note_id != note_id || notes.blobs.get(&expected_path) != Some(&revision_oid) {
                return Err(corrupt(
                    "MemoryEvent revision is not reachable from its canonical note path",
                )
                .with_damage_point(damage_point));
            }
            if matches!(
                event.action,
                super::domain::MemoryEventAction::Created
                    | super::domain::MemoryEventAction::Revised
            ) && revision_events
                .insert(expected_path, event.action)
                .is_some()
            {
                return Err(
                    corrupt("Memory revision is introduced by more than one transition")
                        .with_damage_point(damage_point),
                );
            }
            records.push(MemoryHistoryRecord {
                source_commit_oid,
                event,
                revision_oid: Some(revision_oid),
                note: Some(note),
            });
        } else {
            records.push(MemoryHistoryRecord {
                source_commit_oid,
                event,
                revision_oid: None,
                note: None,
            });
        }
    }

    let added_notes = notes
        .blobs
        .keys()
        .filter(|path| !parent_notes.blobs.contains_key(*path))
        .cloned()
        .collect::<HashSet<_>>();
    if added_notes.len() != revision_events.len()
        || added_notes
            .iter()
            .any(|path| !revision_events.contains_key(path))
    {
        return Err(
            corrupt("Memory note tree additions do not match revision transitions")
                .with_damage_point(MemoryDamagePoint::Commit {
                    oid: source_commit_oid,
                }),
        );
    }
    for (path, action) in revision_events {
        let note_prefix = path
            .rsplit_once('/')
            .map(|(prefix, _)| format!("{prefix}/"))
            .ok_or_else(|| corrupt("Memory note path is not canonical"))?;
        let existed = parent_notes
            .blobs
            .keys()
            .any(|candidate| candidate.starts_with(&note_prefix));
        let expected = if existed {
            super::domain::MemoryEventAction::Revised
        } else {
            super::domain::MemoryEventAction::Created
        };
        if action != expected {
            return Err(corrupt(
                "Memory revision transition disagrees with note ancestry",
            ));
        }
    }
    Ok(records)
}

fn tree_entries(
    storage_path: &Path,
    root_items: &[TreeItem],
    name: &'static str,
) -> Result<Vec<TreeItem>, MemoryWriterError> {
    let item = root_items
        .iter()
        .find(|item| item.name == name && item.mode == TreeItemMode::Tree)
        .ok_or_else(|| corrupt("Memory commit is missing a required tree"))?;
    load_tree(storage_path, item.id)
}

fn event_id_from_filename(name: &str, sequence: u64) -> Result<Uuid, MemoryWriterError> {
    let prefix = format!("{sequence:020}-");
    name.strip_prefix(&prefix)
        .and_then(|name| name.strip_suffix(".json"))
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| corrupt("Memory event filename is not canonical"))
}

#[derive(Default)]
struct NoteTreeIndex {
    blobs: BTreeMap<String, ObjectHash>,
    entry_count: usize,
}

fn note_blob_map(
    storage_path: &Path,
    root_items: &[TreeItem],
) -> Result<NoteTreeIndex, MemoryWriterError> {
    let notes = root_items
        .iter()
        .find(|item| item.name == "notes" && item.mode == TreeItemMode::Tree)
        .ok_or_else(|| corrupt("Memory commit is missing the notes tree"))?;
    let mut output = NoteTreeIndex::default();
    let mut remaining = MAX_MEMORY_TREE_ENTRIES;
    collect_note_blobs(
        storage_path,
        notes.id,
        0,
        String::new(),
        &mut remaining,
        &mut output.blobs,
    )?;
    output.entry_count = MAX_MEMORY_TREE_ENTRIES - remaining;
    Ok(output)
}

fn collect_note_blobs(
    storage_path: &Path,
    tree_oid: ObjectHash,
    depth: usize,
    prefix: String,
    remaining: &mut usize,
    output: &mut BTreeMap<String, ObjectHash>,
) -> Result<(), MemoryWriterError> {
    if depth >= 3 {
        return Err(corrupt("Memory note tree exceeds its canonical depth"));
    }
    for item in load_tree(storage_path, tree_oid)? {
        if *remaining == 0 {
            return Err(corrupt(
                "Memory note tree exceeds the writer validation budget",
            ));
        }
        *remaining -= 1;
        let path = if prefix.is_empty() {
            item.name.clone()
        } else {
            format!("{prefix}/{}", item.name)
        };
        if depth == 2 {
            if item.mode != TreeItemMode::Blob || output.insert(path, item.id).is_some() {
                return Err(corrupt("Memory note revision entry is invalid"));
            }
        } else if item.mode == TreeItemMode::Tree {
            collect_note_blobs(storage_path, item.id, depth + 1, path, remaining, output)?;
        } else {
            return Err(corrupt("Memory note tree has a non-canonical shape"));
        }
    }
    Ok(())
}

fn encode_segment(value: &str) -> String {
    format!("x{}", hex::encode(value.as_bytes()))
}

fn corrupt(summary: &'static str) -> MemoryWriterError {
    MemoryWriterError::new(MemoryWriterErrorKind::CorruptHistory, summary)
}

fn storage_error(action: &'static str, error: impl std::fmt::Display) -> MemoryWriterError {
    MemoryWriterError::new(
        MemoryWriterErrorKind::StorageFailure,
        format!("{action} failed: {error}"),
    )
}

fn history_error(action: &'static str, error: impl std::fmt::Display) -> MemoryWriterError {
    MemoryWriterError::new(
        MemoryWriterErrorKind::CorruptHistory,
        format!("{action} failed for authoritative history: {error}"),
    )
}

pub(super) fn parse_oid(value: &str) -> Result<ObjectHash, MemoryWriterError> {
    ObjectHash::from_str(value).map_err(|_| corrupt("projection contains an invalid object ID"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_capacity_reserves_the_complete_writer_delta() {
        let max = MAX_MEMORY_TREE_ENTRIES;
        let max_events = u64::try_from(max).expect("test budget fits u64");
        assert!(ensure_append_capacity(max_events - 2, max - 3).is_ok());
        assert_eq!(
            ensure_append_capacity(max_events - 1, 0)
                .expect_err("two event slots must remain")
                .kind(),
            MemoryWriterErrorKind::StorageFailure
        );
        assert_eq!(
            ensure_append_capacity(0, max - 2)
                .expect_err("three note-tree entry slots must remain")
                .kind(),
            MemoryWriterErrorKind::StorageFailure
        );
    }
}
