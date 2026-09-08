//! Real, explicit-path repositories for SQLite HEAD capture tests.

use std::{collections::BTreeMap, fs, path::PathBuf};

use git_internal::{
    hash::ObjectHash,
    internal::{
        index::Index,
        object::{ObjectTrait, commit::Commit, types::ObjectType},
    },
};
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};

use crate::{
    internal::{
        config::ConfigKv,
        db::create_database,
        model::reference::{self, ConfigKind},
        operation::{PinnedRequestScope, WorkspaceSnapshotter, WorkspaceStatePointer},
        worktree_scope::WorktreeScope,
    },
    utils::{client_storage::ClientStorage, util},
};

pub(super) struct Fixture {
    pub(super) db: DatabaseConnection,
    pub(super) main: PinnedRequestScope,
    pub(super) storage: ClientStorage,
    pub(super) commit_oid: ObjectHash,
    directory: tempfile::TempDir,
}

impl Fixture {
    pub(super) fn object_files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let objects = self.main.storage.join("objects");
        assert!(
            fs::symlink_metadata(&objects)
                .expect("fixture object root metadata")
                .file_type()
                .is_dir(),
            "fixture object root must remain a real directory"
        );
        let mut directories = vec![objects.clone()];
        let mut files = BTreeMap::new();
        while let Some(directory) = directories.pop() {
            for entry in fs::read_dir(directory).expect("inspect fixture object directory") {
                let entry = entry.expect("fixture object entry");
                let kind = entry.file_type().expect("fixture object entry type");
                if kind.is_dir() {
                    directories.push(entry.path());
                } else {
                    assert!(
                        kind.is_file(),
                        "fixture object entry must remain a regular file"
                    );
                    let path = entry.path();
                    files.insert(
                        path.strip_prefix(&objects)
                            .expect("fixture-relative object path")
                            .to_path_buf(),
                        fs::read(&path).expect("fixture object bytes"),
                    );
                }
            }
        }
        files
    }

    pub(super) async fn new() -> Self {
        let directory = tempfile::tempdir().expect("isolated HEAD repository");
        let root = directory.path().canonicalize().expect("canonical sandbox");
        let main = root.join("main");
        let gitdir = main.join(".libra");
        fs::create_dir_all(gitdir.join("objects")).expect("object directory");
        Index::new()
            .save(gitdir.join("index"))
            .expect("real empty index");
        let db = create_database(gitdir.join("libra.db").to_str().expect("database path"))
            .await
            .expect("real current repository schema");
        ConfigKv::set_with_conn(
            &db,
            "libra.repoid",
            "d8000000-0000-4000-8000-000000000001",
            false,
        )
        .await
        .expect("explicit repository identity");
        let main = PinnedRequestScope::try_resolve(main)
            .expect("resolve main scope")
            .expect("discover real repository");
        assert_eq!(main.scope, WorktreeScope::Main);
        assert!(!main.gitdir.join("HEAD").exists());
        let storage = ClientStorage::init_local(gitdir.join("objects"));
        let tree_oid = ObjectHash::from_type_and_data(ObjectType::Tree, b"");
        storage
            .put(&tree_oid, b"", ObjectType::Tree)
            .expect("empty tree");
        let bytes = format!(
            "tree {tree_oid}\nauthor Test <test@example.invalid> 0 +0000\n\
             committer Test <test@example.invalid> 0 +0000\n\nHEAD fixture\n"
        );
        let commit_oid = ObjectHash::from_type_and_data(ObjectType::Commit, bytes.as_bytes());
        Commit::from_bytes(bytes.as_bytes(), commit_oid).expect("valid detached commit");
        storage
            .put(&commit_oid, bytes.as_bytes(), ObjectType::Commit)
            .expect("commit object");
        Self {
            db,
            main,
            storage,
            commit_oid,
            directory,
        }
    }

    pub(super) fn snapshotter(&self, scope: &PinnedRequestScope) -> WorkspaceSnapshotter {
        WorkspaceSnapshotter::new(
            scope.clone(),
            WorkspaceStatePointer::new(
                "head-fixture",
                ObjectHash::from_type_and_data(ObjectType::Blob, b"seed"),
                0,
            ),
        )
    }

    pub(super) fn linked_scope(&self) -> PinnedRequestScope {
        let root = self.directory.path().join("linked");
        let gitdir = root.join(".libra");
        fs::create_dir_all(&gitdir).expect("private linked gitdir");
        let root = root.canonicalize().expect("canonical linked worktree");
        let gitdir = root.join(".libra");
        let worktree_id = util::worktree_instance_id(&root);
        fs::write(gitdir.join("worktree_id"), &worktree_id).expect("linked identity");
        fs::write(
            gitdir.join("commondir"),
            self.main.storage.to_string_lossy().as_bytes(),
        )
        .expect("real common storage marker");
        Index::new()
            .save(gitdir.join("index"))
            .expect("private linked index");
        let scope = PinnedRequestScope::try_resolve(root.clone())
            .expect("resolve linked scope")
            .expect("discover linked repository");
        assert_eq!(scope.scope, WorktreeScope::Linked(worktree_id));
        assert_eq!(scope.worktree_root, root);
        assert_eq!(scope.gitdir, gitdir);
        assert_eq!(scope.storage, self.main.storage);
        assert_ne!(scope.gitdir, self.main.gitdir);
        assert!(!scope.gitdir.join("HEAD").exists());
        scope
    }

    pub(super) async fn seed_branch(&self, name: &str) {
        reference::ActiveModel {
            kind: Set(ConfigKind::Branch),
            name: Set(Some(name.to_owned())),
            commit: Set(Some(self.commit_oid.to_string())),
            remote: Set(None),
            worktree_id: Set(None),
            ..Default::default()
        }
        .insert(&self.db)
        .await
        .expect("seed born branch through explicit connection");
    }

    pub(super) async fn seed_head(
        &self,
        scope: &WorktreeScope,
        name: Option<&str>,
        commit: Option<&str>,
    ) -> reference::Model {
        reference::ActiveModel {
            kind: Set(ConfigKind::Head),
            name: Set(name.map(str::to_owned)),
            commit: Set(commit.map(str::to_owned)),
            remote: Set(None),
            worktree_id: Set(scope.worktree_id().map(str::to_owned)),
            ..Default::default()
        }
        .insert(&self.db)
        .await
        .expect("seed scoped HEAD through explicit connection")
    }

    pub(super) async fn heads(&self, scope: &WorktreeScope) -> Vec<reference::Model> {
        let query = reference::Entity::find()
            .filter(reference::Column::Kind.eq(ConfigKind::Head))
            .filter(reference::Column::Remote.is_null());
        let query = match scope.worktree_id() {
            Some(id) => query.filter(reference::Column::WorktreeId.eq(id)),
            None => query.filter(reference::Column::WorktreeId.is_null()),
        };
        query
            .all(&self.db)
            .await
            .expect("inspect seeded scope rows")
    }
}
