//! Explicit-path repository fixtures for snapshot boundary tests.

use std::fs;

use git_internal::{
    hash::ObjectHash,
    internal::{
        index::{Index, IndexEntry},
        object::{ObjectTrait, tree::Tree, types::ObjectType},
    },
};
use sea_orm::{ActiveModelTrait, ActiveValue::Set};

use crate::{
    internal::{
        db::create_database,
        model::reference,
        operation::{PinnedRequestScope, WorkspaceSnapshotter, WorkspaceStatePointer},
        worktree_scope::WorktreeScope,
    },
    utils::client_storage::ClientStorage,
};

pub(super) const NESTED: &[u8] = b"opaque nested repository payload, never a parent blob\n";
pub(super) const TRACKED: &[u8] = b"ordinary tracked sibling\n";
pub(super) const UNTRACKED: &[u8] = b"ordinary untracked sibling\n";
pub(super) const MAIN_ONLY: &[u8] = b"other worktree payload, outside this pinned scope\n";

pub(super) struct Fixture {
    _directory: tempfile::TempDir,
    pub(super) snapshotter: WorkspaceSnapshotter,
    pub(super) storage: ClientStorage,
}

pub(super) async fn fixture(linked: bool) -> Fixture {
    let directory = tempfile::tempdir().expect("temporary repository");
    let root = directory.path().canonicalize().expect("canonical root");
    let main = root.join("main");
    let storage_path = main.join(".libra");
    let worktree = if linked {
        root.join("linked")
    } else {
        main.clone()
    };
    let gitdir = if linked {
        storage_path.join("worktrees/linked-test")
    } else {
        storage_path.clone()
    };
    fs::create_dir_all(&gitdir).expect("gitdir");
    fs::create_dir_all(&worktree).expect("worktree");
    let connection = create_database(
        storage_path
            .join("libra.db")
            .to_str()
            .expect("database path"),
    )
    .await
    .expect("real repository schema");
    let mut heads = vec![("main", None)];
    if linked {
        heads.push(("linked-test", Some("linked-test".to_string())));
    }
    for (name, worktree_id) in heads {
        reference::ActiveModel {
            name: Set(Some(name.to_string())),
            kind: Set(reference::ConfigKind::Head),
            commit: Set(None),
            remote: Set(None),
            worktree_id: Set(worktree_id),
            ..Default::default()
        }
        .insert(&connection)
        .await
        .expect("scoped unborn HEAD");
    }
    connection.close().await.expect("close setup pool");
    if linked {
        fs::write(main.join("main-only.txt"), MAIN_ONLY).expect("main-only file");
        let mut index = Index::new();
        index.add(IndexEntry::new_from_blob(
            "main-only.txt".into(),
            blob_oid(MAIN_ONLY),
            0,
        ));
        index.save(storage_path.join("index")).expect("main index");
    }
    let head_name = if linked { "linked-test" } else { "main" };
    fs::write(
        gitdir.join("HEAD"),
        format!("ref: refs/heads/{head_name}\n"),
    )
    .expect("HEAD");
    let scope = PinnedRequestScope {
        scope: if linked {
            WorktreeScope::Linked("linked-test".into())
        } else {
            WorktreeScope::Main
        },
        workdir: worktree.clone(),
        worktree_root: worktree,
        gitdir,
        storage: storage_path.clone(),
    };
    Fixture {
        _directory: directory,
        snapshotter: WorkspaceSnapshotter::new(
            scope,
            WorkspaceStatePointer::new("fixture", blob_oid(b"seed"), 0),
        ),
        storage: ClientStorage::init_local(storage_path.join("objects")),
    }
}

pub(super) fn blob_oid(bytes: &[u8]) -> ObjectHash {
    ObjectHash::from_type_and_data(ObjectType::Blob, bytes)
}

pub(super) fn gitlink_index(stage: u8) -> (Index, ObjectHash) {
    let oid = ObjectHash::from_type_and_data(ObjectType::Commit, b"external opaque commit");
    let mut entry = IndexEntry::new_from_blob("vendor/sub".into(), oid, 0);
    entry.mode = 0o160000;
    entry.flags.stage = stage;
    let mut index = Index::new();
    index.add(entry);
    (index, oid)
}

pub(super) fn tree(storage: &ClientStorage, oid: ObjectHash) -> Tree {
    Tree::from_bytes(&storage.get(&oid).expect("tree bytes"), oid).expect("tree decodes")
}
