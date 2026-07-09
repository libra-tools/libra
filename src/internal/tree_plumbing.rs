//! Index ↔ tree plumbing: the single source of truth for converting the index
//! into a nested Git tree and reading a tree back into an index.
//!
//! `git write-tree` / `read-tree`, and the tree-building steps of `merge` /
//! `cherry-pick`, all go through [`write_tree_from_index`] so there is exactly
//! one nested-tree construction rule in the tree. The builder handles arbitrary
//! nesting, **including intermediate directories that contain no direct files**
//! (e.g. `a/b/c.txt` where nothing lives directly in `a` or `a/b`) — a case the
//! earlier per-command builders mishandled.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use git_internal::{
    errors::GitError,
    hash::ObjectHash,
    internal::{
        index::{Index, IndexEntry},
        object::{
            ObjectTrait,
            tree::{Tree, TreeItem, TreeItemMode},
            types::ObjectType,
        },
    },
};

use crate::utils::{tree::sort_tree_items_for_git, util};

/// Errors from the index ↔ tree plumbing. Domain-specific so callers can map to
/// their own error type with `.to_string()` without parsing strings.
#[derive(Debug, thiserror::Error)]
pub enum TreePlumbingError {
    /// An index entry carried a file-type mode the tree format cannot represent.
    #[error("unsupported file mode {mode:#o} for index entry '{path}'")]
    UnsupportedMode { path: String, mode: u32 },
    /// An index entry points at an object that cannot be read.
    #[error(
        "index entry '{path}' points to missing or unreadable {expected} object {object}: {detail}"
    )]
    MissingOrUnreadableObject {
        path: String,
        object: ObjectHash,
        expected: ObjectType,
        detail: String,
    },
    /// An index entry points at an object whose type does not match the mode.
    #[error(
        "index entry '{path}' points to {object}, expected {expected} object but found {actual}"
    )]
    WrongObjectType {
        path: String,
        object: ObjectHash,
        expected: ObjectType,
        actual: ObjectType,
    },
    /// A path could not be represented as UTF-8.
    #[error("non-UTF-8 path in index: {0}")]
    NonUtf8Path(String),
    /// A tree object could not be (de)serialized or built.
    #[error("tree object error: {0}")]
    Tree(String),
    /// The object store rejected a read or write.
    #[error("object store error: {0}")]
    Storage(String),
}

impl From<GitError> for TreePlumbingError {
    fn from(error: GitError) -> Self {
        TreePlumbingError::Storage(error.to_string())
    }
}

/// Build a nested Git tree from the index's stage-0 entries, writing every tree
/// object (root and subtrees) to the object store, and return the root tree's
/// object id. An empty index yields the canonical empty tree. File modes are
/// preserved (regular / executable / symlink / gitlink) and the object format
/// (SHA-1 / SHA-256) follows the process hash kind, since the tree id is derived
/// from the serialized tree bytes.
pub fn write_tree_from_index(index: &Index) -> Result<ObjectHash, TreePlumbingError> {
    validate_index_objects(index)?;

    let mut leaves = Vec::new();
    for path in index.tracked_files() {
        let key = path
            .to_str()
            .ok_or_else(|| TreePlumbingError::NonUtf8Path(path.display().to_string()))?;
        let Some(entry) = index.get(key, 0) else {
            continue;
        };
        let mode = index_mode_to_tree_mode(entry.mode, key)?;
        leaves.push((path, mode, entry.hash));
    }
    write_tree_from_leaves(leaves)
}

/// Validate the object ids referenced by every stage-0 index entry before any
/// tree or commit object is written. Gitlinks (`160000`) intentionally are not
/// checked: their ids belong to the submodule repository, not necessarily this
/// object database.
pub fn validate_index_objects(index: &Index) -> Result<(), TreePlumbingError> {
    let storage = util::objects_storage();

    for entry in index.tracked_entries(0) {
        let mode = index_mode_to_tree_mode(entry.mode, &entry.name)?;
        let Some(expected) = expected_object_type(mode) else {
            continue;
        };
        let actual = storage.get_object_type(&entry.hash).map_err(|error| {
            TreePlumbingError::MissingOrUnreadableObject {
                path: entry.name.clone(),
                object: entry.hash,
                expected,
                detail: error.to_string(),
            }
        })?;
        if actual != expected {
            return Err(TreePlumbingError::WrongObjectType {
                path: entry.name.clone(),
                object: entry.hash,
                expected,
                actual,
            });
        }
    }

    Ok(())
}

fn expected_object_type(mode: TreeItemMode) -> Option<ObjectType> {
    match mode {
        TreeItemMode::Blob | TreeItemMode::BlobExecutable | TreeItemMode::Link => {
            Some(ObjectType::Blob)
        }
        TreeItemMode::Tree => Some(ObjectType::Tree),
        TreeItemMode::Commit => None,
    }
}

/// Build a nested Git tree from a flat list of leaf entries `(full path, mode,
/// object id)`, writing every tree object (root and subtrees) and returning the
/// root tree id. This is the shared core used by [`write_tree_from_index`] and
/// by the tree-building steps of `merge` / `cherry-pick`, so there is one
/// nested-tree construction rule. Intermediate directories with no direct files
/// are handled. An empty list yields the canonical empty tree.
pub fn write_tree_from_leaves(
    leaves: impl IntoIterator<Item = (PathBuf, TreeItemMode, ObjectHash)>,
) -> Result<ObjectHash, TreePlumbingError> {
    let mut entries_map: HashMap<PathBuf, Vec<TreeItem>> = HashMap::new();
    for (path, mode, id) in leaves {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| TreePlumbingError::NonUtf8Path(path.display().to_string()))?
            .to_string();
        let parent = path.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        // Register every ancestor directory so the recursion reaches a subtree
        // even when no file lives directly in an intermediate directory.
        ensure_ancestor_dirs(&mut entries_map, &parent);
        entries_map
            .entry(parent)
            .or_default()
            .push(TreeItem::new(mode, id, name));
    }

    build_tree_recursively(Path::new(""), &mut entries_map)
}

/// Read a tree (by object id) into a fresh [`Index`], flattening nested subtrees
/// into stage-0 entries keyed by their full path. Blob sizes are not populated
/// (the tree carries no size); the tree id round-trips regardless because a tree
/// is derived from `(mode, id, name)` only. This is the index half of
/// `read-tree` — it does **not** touch the working tree.
pub fn read_tree_into_index(tree_id: &ObjectHash) -> Result<Index, TreePlumbingError> {
    let mut files: Vec<(String, TreeItem)> = Vec::new();
    collect_tree_leaves(tree_id, "", &mut files)?;

    let mut index = Index::new();
    for (path, item) in files {
        let mut entry = IndexEntry::new_from_blob(path, item.id, 0);
        entry.mode = tree_mode_to_index_mode(item.mode);
        index.add(entry);
    }
    Ok(index)
}

/// Register `dir` and all of its ancestors as (initially empty) directory keys.
fn ensure_ancestor_dirs(entries_map: &mut HashMap<PathBuf, Vec<TreeItem>>, dir: &Path) {
    let mut current = Some(dir);
    while let Some(path) = current {
        if path.as_os_str().is_empty() {
            break;
        }
        entries_map.entry(path.to_path_buf()).or_default();
        current = path.parent();
    }
}

/// Recursively assemble and persist the tree rooted at `current_path`.
fn build_tree_recursively(
    current_path: &Path,
    entries_map: &mut HashMap<PathBuf, Vec<TreeItem>>,
) -> Result<ObjectHash, TreePlumbingError> {
    let mut current_items = entries_map.remove(current_path).unwrap_or_default();

    let mut subdirs: Vec<PathBuf> = entries_map
        .keys()
        .filter(|path| path.parent() == Some(current_path))
        .cloned()
        .collect();
    // Deterministic recursion order (the final tree is sorted regardless).
    subdirs.sort();

    for subdir in subdirs {
        let name = subdir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| TreePlumbingError::NonUtf8Path(subdir.display().to_string()))?
            .to_string();
        let subtree_id = build_tree_recursively(&subdir, entries_map)?;
        current_items.push(TreeItem::new(TreeItemMode::Tree, subtree_id, name));
    }

    sort_tree_items_for_git(&mut current_items);
    let tree = if current_items.is_empty() {
        // `Tree::from_tree_items` rejects an empty item list, but an empty
        // directory — most importantly an empty index at the root — must
        // serialize to the canonical empty tree object (`4b825dc…` for SHA-1),
        // matching `git write-tree`.
        let empty_id = ObjectHash::from_type_and_data(ObjectType::Tree, &[]);
        Tree::from_bytes(&[], empty_id)
            .map_err(|error| TreePlumbingError::Tree(error.to_string()))?
    } else {
        Tree::from_tree_items(current_items)
            .map_err(|error| TreePlumbingError::Tree(error.to_string()))?
    };
    save_tree_object(&tree)?;
    Ok(tree.id)
}

/// Persist a tree object to the local object store.
fn save_tree_object(tree: &Tree) -> Result<(), TreePlumbingError> {
    let storage = util::objects_storage();
    let data = tree
        .to_data()
        .map_err(|error| TreePlumbingError::Tree(error.to_string()))?;
    storage
        .put(&tree.id, &data, tree.get_type())
        .map_err(|error| TreePlumbingError::Storage(error.to_string()))?;
    Ok(())
}

/// Depth-first flatten of a tree's leaf entries into `(path, item)` pairs.
fn collect_tree_leaves(
    tree_id: &ObjectHash,
    prefix: &str,
    out: &mut Vec<(String, TreeItem)>,
) -> Result<(), TreePlumbingError> {
    let storage = util::objects_storage();
    let data = storage
        .get(tree_id)
        .map_err(|error| TreePlumbingError::Storage(error.to_string()))?;
    let tree = Tree::from_bytes(&data.to_vec(), *tree_id)
        .map_err(|error| TreePlumbingError::Tree(error.to_string()))?;

    for item in &tree.tree_items {
        let path = if prefix.is_empty() {
            item.name.clone()
        } else {
            format!("{prefix}/{}", item.name)
        };
        if item.mode == TreeItemMode::Tree {
            collect_tree_leaves(&item.id, &path, out)?;
        } else {
            out.push((path, item.clone()));
        }
    }
    Ok(())
}

/// Map an index entry's stat mode to a tree-item mode. The executable bit is
/// detected via the `0o111` mask, matching Git.
fn index_mode_to_tree_mode(mode: u32, path: &str) -> Result<TreeItemMode, TreePlumbingError> {
    match mode & 0o170000 {
        0o100000 => Ok(if mode & 0o111 != 0 {
            TreeItemMode::BlobExecutable
        } else {
            TreeItemMode::Blob
        }),
        0o120000 => Ok(TreeItemMode::Link),
        0o040000 => Ok(TreeItemMode::Tree),
        0o160000 => Ok(TreeItemMode::Commit),
        _ => Err(TreePlumbingError::UnsupportedMode {
            path: path.to_string(),
            mode,
        }),
    }
}

/// Map a tree-item mode back to the canonical index stat mode.
fn tree_mode_to_index_mode(mode: TreeItemMode) -> u32 {
    match mode {
        TreeItemMode::Blob => 0o100644,
        TreeItemMode::BlobExecutable => 0o100755,
        TreeItemMode::Link => 0o120000,
        TreeItemMode::Commit => 0o160000,
        TreeItemMode::Tree => 0o040000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the public contract: the signatures `write-tree` / `read-tree` and
    /// the merge/cherry-pick callers depend on must not drift silently.
    #[test]
    fn public_api_signatures_are_frozen() {
        let _write: fn(&Index) -> Result<ObjectHash, TreePlumbingError> = write_tree_from_index;
        let _validate: fn(&Index) -> Result<(), TreePlumbingError> = validate_index_objects;
        let _read: fn(&ObjectHash) -> Result<Index, TreePlumbingError> = read_tree_into_index;
    }

    #[test]
    fn index_mode_mapping_round_trips() {
        for (index_mode, tree_mode) in [
            (0o100644u32, TreeItemMode::Blob),
            (0o100755, TreeItemMode::BlobExecutable),
            (0o120000, TreeItemMode::Link),
            (0o160000, TreeItemMode::Commit),
        ] {
            let mapped = index_mode_to_tree_mode(index_mode, "p").expect("supported mode");
            assert_eq!(mapped, tree_mode, "index {index_mode:o} -> tree mode");
            assert_eq!(
                tree_mode_to_index_mode(tree_mode),
                index_mode,
                "tree mode -> index {index_mode:o}"
            );
        }
    }

    #[test]
    fn executable_bit_detected_via_mask() {
        assert_eq!(
            index_mode_to_tree_mode(0o100750, "p").unwrap(),
            TreeItemMode::BlobExecutable
        );
        assert_eq!(
            index_mode_to_tree_mode(0o100644, "p").unwrap(),
            TreeItemMode::Blob
        );
    }

    #[test]
    fn unsupported_file_mode_is_rejected() {
        let error = index_mode_to_tree_mode(0o010000, "fifo").unwrap_err();
        assert!(matches!(error, TreePlumbingError::UnsupportedMode { .. }));
    }

    /// The builder must register intermediate directories so a deeply-nested
    /// path with no sibling files is not dropped — the bug in the earlier
    /// per-command builders.
    #[test]
    fn ensure_ancestor_dirs_registers_every_level() {
        let mut map: HashMap<PathBuf, Vec<TreeItem>> = HashMap::new();
        ensure_ancestor_dirs(&mut map, Path::new("a/b"));
        assert!(
            map.contains_key(Path::new("a")),
            "intermediate 'a' registered"
        );
        assert!(map.contains_key(Path::new("a/b")), "'a/b' registered");
        assert!(
            !map.contains_key(Path::new("")),
            "root is not a directory key"
        );
    }
}
