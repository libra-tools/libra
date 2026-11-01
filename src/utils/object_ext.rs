//! Extension traits for Tree/Commit/Blob to load from storage, expand items recursively with modes, save blobs, and support LFS-backed files.

use std::{
    fs,
    io::{self, BufReader, Read},
    path::{Path, PathBuf},
};

use colored::Colorize;
use git_internal::{
    errors::GitError,
    hash::ObjectHash,
    internal::object::{
        ObjectTrait,
        blob::Blob,
        commit::Commit,
        tree::{Tree, TreeItemMode},
    },
};

use crate::utils::{client_storage::ClientStorage, lfs, util};

/// A recursive tree expansion failed before a complete snapshot was available.
#[derive(Debug, thiserror::Error)]
#[error("cannot read tree '{oid}': {source}")]
pub struct TreeExpansionError {
    pub oid: ObjectHash,
    #[source]
    pub source: GitError,
}

pub trait TreeExt {
    fn load(hash: &ObjectHash) -> io::Result<Tree>;
    fn try_load(hash: &ObjectHash) -> Option<Tree>;
    fn get_plain_items(&self) -> io::Result<Vec<(PathBuf, ObjectHash)>>;
    /// Get all the items in the tree recursively with mode information
    /// Returns (path, hash, mode) tuples
    fn get_plain_items_with_mode(&self) -> io::Result<Vec<(PathBuf, ObjectHash, TreeItemMode)>>;
    fn get_items_with_mode(&self) -> io::Result<Vec<(PathBuf, ObjectHash, TreeItemMode)>>;
}

/// Expand nested trees without panicking or returning a partial snapshot.
/// Gitlinks remain leaf entries; their commit objects need not exist locally.
///
/// Storage is resolved the first time a nested tree is actually read, then
/// reused for the rest of this expansion. An empty tree does not open storage.
/// The first read failure is still attributed to the subtree being expanded.
pub fn try_get_plain_items_with_mode(
    tree: &Tree,
) -> Result<Vec<(PathBuf, ObjectHash, TreeItemMode)>, TreeExpansionError> {
    let mut storage = None;
    try_get_plain_items_with_mode_cached(tree, &mut storage)
}

pub(crate) fn try_get_plain_items_with_storage(
    tree: &Tree,
    storage: &ClientStorage,
) -> Result<Vec<(PathBuf, ObjectHash, TreeItemMode)>, TreeExpansionError> {
    let mut storage = Some(storage.clone());
    try_get_plain_items_with_mode_cached(tree, &mut storage)
}

fn try_get_plain_items_with_mode_cached(
    tree: &Tree,
    storage: &mut Option<ClientStorage>,
) -> Result<Vec<(PathBuf, ObjectHash, TreeItemMode)>, TreeExpansionError> {
    let mut items = Vec::new();
    for item in &tree.tree_items {
        if item.mode != TreeItemMode::Tree {
            items.push((PathBuf::from(&item.name), item.id, item.mode));
            continue;
        }

        let mut load = || {
            let resolved = match storage {
                Some(storage) => storage,
                None => storage.insert(util::try_objects_storage().map_err(GitError::IOError)?),
            };
            let data = resolved.get(&item.id)?;
            Tree::from_bytes(&data, item.id)
        };
        let subtree = load().map_err(|source| TreeExpansionError {
            oid: item.id,
            source,
        })?;
        let entries = try_get_plain_items_with_mode_cached(&subtree, storage)?;
        items.extend(
            entries
                .into_iter()
                .map(|(path, hash, mode)| (PathBuf::from(&item.name).join(path), hash, mode)),
        );
    }
    Ok(items)
}

pub(crate) fn load_tree_with_storage(
    storage: &ClientStorage,
    hash: &ObjectHash,
) -> io::Result<Tree> {
    let tree_data = storage.get(hash).map_err(|err| {
        io::Error::other(format!(
            "Tree::load({hash}): failed to read object from storage: {err}"
        ))
    })?;
    Tree::from_bytes(&tree_data, *hash).map_err(|err| {
        io::Error::other(format!(
            "Tree::load({hash}): failed to decode tree bytes: {err:?}"
        ))
    })
}

pub(crate) fn load_commit_with_storage(
    storage: &ClientStorage,
    hash: &ObjectHash,
) -> io::Result<Commit> {
    let commit_data = storage.get(hash).map_err(|err| {
        io::Error::other(format!(
            "Commit::load({hash}): failed to read object from storage: {err}"
        ))
    })?;
    Commit::from_bytes(&commit_data, *hash).map_err(|err| {
        io::Error::other(format!(
            "Commit::load({hash}): failed to decode commit bytes: {err:?}"
        ))
    })
}

pub trait CommitExt {
    fn load(hash: &ObjectHash) -> io::Result<Commit>;
    fn try_load(hash: &ObjectHash) -> Option<Commit>;
}

pub trait BlobExt {
    fn load(hash: &ObjectHash) -> io::Result<Blob>;
    fn from_file(path: impl AsRef<Path>) -> Blob;
    fn from_lfs_file(path: impl AsRef<Path>) -> Blob;
    fn save(&self) -> std::io::Result<ObjectHash>;
    fn try_save(&self) -> std::io::Result<ObjectHash> {
        self.save()
    }
}

impl TreeExt for Tree {
    /// Load a tree object by hash.
    ///
    /// A missing object, a storage read failure, or corrupt bytes returns
    /// `Err` naming the hash. [`TreeExt::try_load`] maps those failures to `None`.
    fn load(hash: &ObjectHash) -> io::Result<Tree> {
        load_tree_with_storage(&util::try_objects_storage()?, hash)
    }

    fn try_load(hash: &ObjectHash) -> Option<Tree> {
        let storage = util::try_objects_storage().ok()?;
        storage
            .get(hash)
            .ok()
            .and_then(|tree_data| Tree::from_bytes(&tree_data, *hash).ok())
    }

    /// Get all the items in the tree recursively (to workdir path)
    fn get_plain_items(&self) -> io::Result<Vec<(PathBuf, ObjectHash)>> {
        let mut items = Vec::new();
        for item in self.tree_items.iter() {
            if item.mode != TreeItemMode::Tree {
                // `160000` gitlink entries (submodules) reference commits that are
                // not guaranteed to exist in this repository's object database.
                // Treat them as unsupported for plain-file expansion to avoid
                // panicking later when callers assume Blob objects.
                if item.mode == TreeItemMode::Commit {
                    eprintln!(
                        "{}",
                        format!(
                            "Warning: Submodule '{}' is not supported yet; skipping checkout entry",
                            item.name
                        )
                        .red()
                    );
                    continue;
                }
                // Not Tree, maybe Blob, link, etc.
                items.push((PathBuf::from(item.name.clone()), item.id));
            } else {
                let sub_tree = Tree::load(&item.id)?;
                let sub_entries = sub_tree.get_plain_items()?;

                items.append(
                    sub_entries
                        .iter()
                        .map(|(path, hash)| (PathBuf::from(item.name.clone()).join(path), *hash))
                        .collect::<Vec<(PathBuf, ObjectHash)>>()
                        .as_mut(),
                );
            }
        }
        Ok(items)
    }

    /// Get all the items in the tree recursively with mode information
    fn get_plain_items_with_mode(&self) -> io::Result<Vec<(PathBuf, ObjectHash, TreeItemMode)>> {
        let mut items = Vec::new();
        for item in self.tree_items.iter() {
            if item.mode != TreeItemMode::Tree {
                // Not Tree, maybe Blob, link, etc.
                items.push((PathBuf::from(item.name.clone()), item.id, item.mode));
            } else {
                let sub_tree = Tree::load(&item.id)?;
                let sub_entries = sub_tree.get_plain_items_with_mode()?;

                // Use extend() instead of append() to avoid intermediate allocation
                items.extend(sub_entries.into_iter().map(|(path, hash, mode)| {
                    (PathBuf::from(item.name.clone()).join(path), hash, mode)
                }));
            }
        }
        Ok(items)
    }

    /// Get all the items in the tree recursively with mode information
    fn get_items_with_mode(&self) -> io::Result<Vec<(PathBuf, ObjectHash, TreeItemMode)>> {
        let mut items = Vec::new();
        items.push((PathBuf::from("/"), self.id, TreeItemMode::Tree));
        for item in self.tree_items.iter() {
            if item.mode != TreeItemMode::Tree {
                // Not Tree, maybe Blob, link, etc.
                items.push((PathBuf::from(item.name.clone()), item.id, item.mode));
            } else {
                let sub_tree = Tree::load(&item.id)?;
                let sub_entries = sub_tree.get_items_with_mode()?;

                // Use extend() instead of append() to avoid intermediate allocation
                items.extend(sub_entries.into_iter().map(|(path, hash, mode)| {
                    (PathBuf::from(item.name.clone()).join(path), hash, mode)
                }));
            }
        }
        Ok(items)
    }
}

impl CommitExt for Commit {
    /// Load a commit object by hash.
    ///
    /// A missing object, a storage read failure, or corrupt bytes returns
    /// `Err` naming the hash. [`CommitExt::try_load`] maps those failures to `None`.
    fn load(hash: &ObjectHash) -> io::Result<Commit> {
        load_commit_with_storage(&util::try_objects_storage()?, hash)
    }

    fn try_load(hash: &ObjectHash) -> Option<Commit> {
        let storage = util::try_objects_storage().ok()?;
        storage
            .get(hash)
            .ok()
            .and_then(|commit_data| Commit::from_bytes(&commit_data, *hash).ok())
    }
}

impl BlobExt for Blob {
    /// Load a blob object by hash.
    ///
    /// A missing object or a storage read failure returns `Err` naming the hash.
    /// Blob payloads are unparsed bytes, so a readable payload does not fail decode.
    fn load(hash: &ObjectHash) -> io::Result<Blob> {
        let storage = util::try_objects_storage()?;
        let blob_data = storage.get(hash).map_err(|err| {
            io::Error::other(format!(
                "Blob::load({hash}): failed to read object from storage: {err}"
            ))
        })?;
        Blob::from_bytes(&blob_data, *hash).map_err(|err| {
            io::Error::other(format!(
                "Blob::load({hash}): failed to decode blob bytes: {err:?}"
            ))
        })
    }

    /// Create a blob from a file
    /// - `path`: absolute  or relative path to current dir
    fn from_file(path: impl AsRef<Path>) -> Blob {
        let path = path.as_ref();
        let mut data = Vec::new();
        let file = fs::File::open(path).unwrap_or_else(|err| {
            panic!("Blob::from_file({}): open failed: {err}", path.display())
        });
        let mut reader = BufReader::new(file);
        reader.read_to_end(&mut data).unwrap_or_else(|err| {
            panic!("Blob::from_file({}): read failed: {err}", path.display())
        });
        Blob::from_content_bytes(data)
    }

    /// Create a blob from an LFS file
    /// - include: create a pointer file & copy the file to `.libra/lfs/objects`
    /// - `path`: absolute  or relative path to current dir
    fn from_lfs_file(path: impl AsRef<Path>) -> Blob {
        let path = path.as_ref();
        let (pointer, oid) = lfs::generate_pointer_file(path);
        tracing::debug!("\n{}", pointer);
        lfs::backup_lfs_file(path, &oid).unwrap_or_else(|err| {
            panic!(
                "Blob::from_lfs_file({}): backup to .libra/lfs/objects failed: {err}",
                path.display()
            )
        });
        Blob::from_content(&pointer)
    }

    fn save(&self) -> std::io::Result<ObjectHash> {
        let storage = util::objects_storage()?;
        let id = self.id;
        match storage.exist_checked(&id) {
            Ok(false) => {
                storage.put(&id, &self.data, self.get_type())?;
            }
            Ok(true) => {
                // A prior call may have stored the payload and then failed while
                // creating its durable index marker. Re-register existing content
                // so a normal retry cannot silently leave cloud sync incomplete.
                storage.ensure_existing_object_index(&id, self.data.len(), self.get_type())?;
            }
            Err(error) => return Err(std::io::Error::other(error.to_string())),
        }
        Ok(self.id)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use git_internal::internal::object::{tree::TreeItem, types::ObjectType};
    use serial_test::serial;

    use super::*;
    use crate::utils::test::{ChangeDirGuard, setup_with_new_libra_in};

    fn with_repo<T>(body: impl FnOnce(&Path) -> T) -> T {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = ChangeDirGuard::new(tmp.path());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(setup_with_new_libra_in(tmp.path()));
        body(tmp.path())
    }

    fn plant_unindexed_pack(repo: &Path) {
        let pack_dir = repo.join(".libra/objects/pack");
        std::fs::create_dir_all(&pack_dir).expect("pack dir");
        std::fs::write(pack_dir.join("orphan.pack"), b"not-a-pack").expect("unindexed pack");
    }

    fn parent_of(child: ObjectHash) -> Tree {
        Tree {
            id: ObjectHash::from_type_and_data(ObjectType::Tree, b"parent-tree"),
            tree_items: vec![TreeItem::new(TreeItemMode::Tree, child, "sub".into())],
        }
    }

    fn write_loose_garbage(repo: &Path, hash: &ObjectHash) {
        let hex = hash.to_string();
        let path = repo.join(".libra/objects").join(&hex[..2]).join(&hex[2..]);
        std::fs::create_dir_all(path.parent().expect("shard parent")).expect("shard");
        std::fs::write(&path, b"not-a-loose-object").expect("garbage loose object");
    }

    #[test]
    #[serial(cwd, env)]
    fn blob_save_storage_read_error_does_not_write() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = ChangeDirGuard::new(tmp.path());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(setup_with_new_libra_in(tmp.path()));
        let pack_dir = tmp.path().join(".libra/objects/pack");
        std::fs::create_dir_all(&pack_dir).expect("pack dir");
        std::fs::write(pack_dir.join("orphan.pack"), b"not-a-pack").expect("unindexed pack");
        let blob = Blob::from_content("unread");
        let oid = blob.id.to_string();
        let loose = tmp
            .path()
            .join(".libra/objects")
            .join(&oid[..2])
            .join(&oid[2..]);
        let error = blob
            .save()
            .expect_err("a storage read failure must not store the blob");
        assert!(!loose.exists(), "save must not write after a read error");
        assert!(
            error.to_string().contains("no complete index"),
            "unexpected error: {error}"
        );
    }

    #[test]
    #[serial(cwd, env)]
    fn tree_load_returns_io_result_on_storage_err() {
        with_repo(|repo| {
            plant_unindexed_pack(repo);
            let hash = ObjectHash::from_type_and_data(ObjectType::Tree, b"tree-storage-err");
            let error = Tree::load(&hash).expect_err("storage read must return Err");
            let rendered = error.to_string();
            assert!(rendered.contains(&hash.to_string()), "{rendered}");
            assert!(rendered.contains("Tree::load"), "{rendered}");
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn commit_load_returns_io_result_on_storage_err() {
        with_repo(|repo| {
            plant_unindexed_pack(repo);
            let hash = ObjectHash::from_type_and_data(ObjectType::Commit, b"commit-storage-err");
            let error = Commit::load(&hash).expect_err("storage read must return Err");
            let rendered = error.to_string();
            assert!(rendered.contains(&hash.to_string()), "{rendered}");
            assert!(rendered.contains("Commit::load"), "{rendered}");
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn blob_load_returns_io_result_on_storage_err() {
        with_repo(|repo| {
            plant_unindexed_pack(repo);
            let hash = ObjectHash::from_type_and_data(ObjectType::Blob, b"blob-storage-err");
            let error = Blob::load(&hash).expect_err("storage read must return Err");
            let rendered = error.to_string();
            assert!(rendered.contains(&hash.to_string()), "{rendered}");
            assert!(rendered.contains("Blob::load"), "{rendered}");
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn get_plain_items_propagates_subtree_load_err() {
        with_repo(|repo| {
            plant_unindexed_pack(repo);
            let child = ObjectHash::from_type_and_data(ObjectType::Tree, b"subtree-plain");
            let error = parent_of(child)
                .get_plain_items()
                .expect_err("subtree load must return Err");
            assert!(error.to_string().contains(&child.to_string()), "{error}");
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn get_plain_items_with_mode_propagates_subtree_load_err() {
        with_repo(|repo| {
            plant_unindexed_pack(repo);
            let child = ObjectHash::from_type_and_data(ObjectType::Tree, b"subtree-mode");
            let error = parent_of(child)
                .get_plain_items_with_mode()
                .expect_err("subtree load must return Err");
            assert!(error.to_string().contains(&child.to_string()), "{error}");
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn get_items_with_mode_propagates_subtree_load_err() {
        with_repo(|repo| {
            plant_unindexed_pack(repo);
            let child = ObjectHash::from_type_and_data(ObjectType::Tree, b"subtree-items");
            let error = parent_of(child)
                .get_items_with_mode()
                .expect_err("subtree load must return Err");
            assert!(error.to_string().contains(&child.to_string()), "{error}");
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn tree_load_invalid_bytes_returns_err_not_panic() {
        with_repo(|_repo| {
            let hash = ObjectHash::from_type_and_data(ObjectType::Tree, b"invalid-tree");
            crate::utils::util::try_objects_storage()
                .expect("storage")
                .put(&hash, b"not a tree", ObjectType::Tree)
                .expect("store invalid tree bytes");
            let error = Tree::load(&hash).expect_err("corrupt tree bytes must return Err");
            let rendered = error.to_string();
            assert!(rendered.contains(&hash.to_string()), "{rendered}");
            assert!(
                rendered.contains("failed to decode tree bytes"),
                "{rendered}"
            );
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn commit_load_invalid_bytes_returns_err_not_panic() {
        with_repo(|_repo| {
            let hash = ObjectHash::from_type_and_data(ObjectType::Commit, b"invalid-commit");
            crate::utils::util::try_objects_storage()
                .expect("storage")
                .put(&hash, b"not a commit", ObjectType::Commit)
                .expect("store invalid commit bytes");
            let error = Commit::load(&hash).expect_err("corrupt commit bytes must return Err");
            let rendered = error.to_string();
            assert!(rendered.contains(&hash.to_string()), "{rendered}");
            assert!(
                rendered.contains("failed to decode commit bytes"),
                "{rendered}"
            );
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn blob_load_invalid_bytes_returns_err_not_panic() {
        with_repo(|repo| {
            let hash = ObjectHash::from_type_and_data(ObjectType::Blob, b"invalid-blob");
            write_loose_garbage(repo, &hash);
            let error = Blob::load(&hash).expect_err("invalid blob bytes must return Err");
            let rendered = error.to_string();
            assert!(rendered.contains(&hash.to_string()), "{rendered}");
            assert!(rendered.contains("Blob::load"), "{rendered}");
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn get_items_with_mode_corrupt_subtree_returns_err() {
        with_repo(|_repo| {
            let child = ObjectHash::from_type_and_data(ObjectType::Tree, b"corrupt-subtree");
            crate::utils::util::try_objects_storage()
                .expect("storage")
                .put(&child, b"not-a-tree", ObjectType::Tree)
                .expect("store corrupt subtree");
            let error = parent_of(child)
                .get_items_with_mode()
                .expect_err("corrupt subtree must return Err");
            let rendered = error.to_string();
            assert!(rendered.contains(&child.to_string()), "{rendered}");
            assert!(
                rendered.contains("failed to decode tree bytes"),
                "{rendered}"
            );
        });
    }
}
