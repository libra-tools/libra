//! Tree helpers for converting index entries into git trees and recursively enumerating tree contents with modes and paths.

use std::{collections::HashMap, path::Path};

use git_internal::{
    errors::GitError,
    internal::{
        index::Index,
        object::{
            ObjectTrait,
            tree::{Tree, TreeItem, TreeItemMode},
        },
    },
};

use crate::utils::object;

/// Creates a `Tree` object from the entries in an `Index`.
pub fn create_tree_from_index(index: &Index) -> Result<Tree, GitError> {
    let mut tree_items = Vec::new();

    // Convert IndexEntries to TreeItems
    for entry in index.tracked_entries(0) {
        // WT-07 (ADR-WT-05): an intent-to-add entry is a placeholder for a
        // new file and must never become tree content.
        if entry.flags.intent_to_add {
            continue;
        }
        // Stage 0 for normal files
        let mode = match entry.mode & 0o170000 {
            // Check file type from mode
            0o100000 => {
                // Regular file
                if entry.mode & 0o111 != 0 {
                    TreeItemMode::BlobExecutable
                } else {
                    TreeItemMode::Blob
                }
            }
            0o120000 => TreeItemMode::Link,
            0o040000 => TreeItemMode::Tree,
            0o160000 => TreeItemMode::Commit, // Gitlink for submodules
            _ => {
                // An unsupported file mode was found in the index.
                // This could be due to a corrupted index or a new, unsupported Git feature.
                return Err(GitError::InvalidTreeItem(format!(
                    "Unsupported file mode in index: {:o}",
                    entry.mode
                )));
            }
        };

        tree_items.push(TreeItem::new(mode, entry.hash, entry.name.clone()));
    }

    sort_tree_items_for_git(&mut tree_items);

    Tree::from_tree_items(tree_items)
}

/// Sort tree entries using Git's tree ordering.
///
/// Git compares names bytewise, treating directories as if their name ends in
/// `/`. Unsigned or server-side `index-pack --strict` rejects tree objects that
/// do not use this order.
pub fn sort_tree_items_for_git(tree_items: &mut [TreeItem]) {
    tree_items.sort_by_key(git_tree_sort_key);
}

fn git_tree_sort_key(item: &TreeItem) -> Vec<u8> {
    let mut key = item.name.as_bytes().to_vec();
    if item.mode == TreeItemMode::Tree {
        key.push(b'/');
    }
    key
}

/// Helper function to recursively get all files from a tree.
pub fn get_tree_files_recursive(
    tree: &Tree,
    git_dir: &Path,
    current_path: &Path,
) -> Result<HashMap<String, TreeItem>, String> {
    let mut files = HashMap::new();
    for item in &tree.tree_items {
        let item_path = current_path.join(&item.name);
        let item_path_str = item_path
            .to_str()
            .ok_or_else(|| format!("Invalid path: {:?}", item_path))?
            .to_string();

        if item.mode == TreeItemMode::Tree {
            let subtree_data =
                object::read_git_object(git_dir, &item.id).map_err(|e| e.to_string())?;
            let subtree = Tree::from_bytes(&subtree_data, item.id).map_err(|e| e.to_string())?;
            let sub_files = get_tree_files_recursive(&subtree, git_dir, &item_path)?;
            files.extend(sub_files);
        } else {
            files.insert(item_path_str, item.clone());
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use git_internal::{hash::ObjectHash, internal::index::IndexEntry};

    use super::*;

    fn entry(name: &str, mode: u32) -> IndexEntry {
        let mut e = IndexEntry::new_from_blob(
            name.to_string(),
            ObjectHash::new_for_kind(git_internal::hash::get_hash_kind(), &[1; 20]),
            0,
        );
        e.mode = mode;
        e
    }

    #[test]
    fn sort_tree_items_for_git_orders_names_and_directories() {
        let hash = ObjectHash::new_for_kind(git_internal::hash::get_hash_kind(), &[1; 20]);
        let mut items = vec![
            TreeItem::new(TreeItemMode::Blob, hash, "z.txt".to_string()),
            TreeItem::new(TreeItemMode::Tree, hash, "foo".to_string()),
            TreeItem::new(TreeItemMode::Blob, hash, "foo.txt".to_string()),
            TreeItem::new(TreeItemMode::Blob, hash, "a.txt".to_string()),
        ];

        sort_tree_items_for_git(&mut items);

        let ordered = items
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ordered, vec!["a.txt", "foo.txt", "foo", "z.txt"]);
    }

    fn item_mode(tree: &Tree, name: &str) -> TreeItemMode {
        tree.tree_items
            .iter()
            .find(|i| i.name == name)
            .unwrap_or_else(|| panic!("tree must contain `{name}`"))
            .mode
    }

    /// `create_tree_from_index` maps each index file-type/permission bit
    /// to the correct `TreeItemMode`. A wrong mapping corrupts the
    /// committed tree — e.g. losing the executable bit, or storing a
    /// symlink as a regular blob. Pin every supported mode.
    #[test]
    fn create_tree_maps_each_file_mode() {
        let mut index = Index::new();
        index.add(entry("regular.txt", 0o100644));
        index.add(entry("script.sh", 0o100755)); // exec bit set
        index.add(entry("link", 0o120000)); // symlink
        index.add(entry("submodule", 0o160000)); // gitlink

        let tree = create_tree_from_index(&index).expect("supported modes must build a tree");

        assert_eq!(item_mode(&tree, "regular.txt"), TreeItemMode::Blob);
        assert_eq!(item_mode(&tree, "script.sh"), TreeItemMode::BlobExecutable);
        assert_eq!(item_mode(&tree, "link"), TreeItemMode::Link);
        assert_eq!(item_mode(&tree, "submodule"), TreeItemMode::Commit);
    }

    /// The executable bit is detected via `mode & 0o111`: any of the
    /// user/group/other execute bits flips a regular file to
    /// `BlobExecutable`, and a regular file with none stays `Blob`.
    #[test]
    fn create_tree_detects_executable_bit_via_mask() {
        let mut index = Index::new();
        index.add(entry("group_exec", 0o100750)); // only group +x
        index.add(entry("other_exec", 0o100701)); // only other +x
        index.add(entry("plain", 0o100644));

        let tree = create_tree_from_index(&index).expect("build tree");
        assert_eq!(item_mode(&tree, "group_exec"), TreeItemMode::BlobExecutable);
        assert_eq!(item_mode(&tree, "other_exec"), TreeItemMode::BlobExecutable);
        assert_eq!(item_mode(&tree, "plain"), TreeItemMode::Blob);
    }

    /// An unsupported file-type mode (e.g. a FIFO, `0o010000`) is a
    /// hard error rather than a silently-mismapped tree entry —
    /// corrupted index data must not produce a malformed tree.
    #[test]
    fn create_tree_rejects_unsupported_file_mode() {
        let mut index = Index::new();
        index.add(entry("fifo", 0o010000));
        let err =
            create_tree_from_index(&index).expect_err("unsupported file mode must be rejected");
        assert!(
            matches!(err, GitError::InvalidTreeItem(_)),
            "expected InvalidTreeItem, got {err:?}",
        );
    }
}
