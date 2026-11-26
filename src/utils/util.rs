//! Root `utils::util` facade: re-exports all items from `libra_core::utils::util`
//! and ensures the root promisor lazy-fetch hook is registered before any direct
//! library call enters `validate_referenced_object`.

use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};
pub use libra_core::utils::util::*;

pub async fn resolve_object_spec_typed(name: &str) -> Result<ObjectHash, CommitBaseError> {
    crate::ensure_runtime_hooks();
    libra_core::utils::util::resolve_object_spec_typed(name).await
}

pub async fn resolve_object_spec_with_auto_merge_typed(
    name: &str,
) -> Result<ObjectHash, CommitBaseError> {
    crate::ensure_runtime_hooks();
    libra_core::utils::util::resolve_object_spec_with_auto_merge_typed(name).await
}

pub async fn resolve_tree_ish_typed(name: &str) -> Result<ObjectHash, CommitBaseError> {
    crate::ensure_runtime_hooks();
    libra_core::utils::util::resolve_tree_ish_typed(name).await
}

pub async fn resolve_tree_ish_with_auto_merge_typed(
    name: &str,
) -> Result<ObjectHash, CommitBaseError> {
    crate::ensure_runtime_hooks();
    libra_core::utils::util::resolve_tree_ish_with_auto_merge_typed(name).await
}

pub async fn get_commit_base_typed(name: &str) -> Result<ObjectHash, CommitBaseError> {
    crate::ensure_runtime_hooks();
    libra_core::utils::util::get_commit_base_typed(name).await
}

pub fn peel_to_non_tag_typed(
    object_id: ObjectHash,
    display_name: &str,
) -> Result<(ObjectHash, ObjectType), CommitBaseError> {
    crate::ensure_runtime_hooks();
    libra_core::utils::util::peel_to_non_tag_typed(object_id, display_name)
}

pub async fn get_commit_base(name: &str) -> Result<ObjectHash, String> {
    crate::ensure_runtime_hooks();
    libra_core::utils::util::get_commit_base(name).await
}

#[cfg(test)]
mod root_tests {
    use std::{env, fs};

    use git_internal::{
        hash::ObjectHash,
        internal::{
            index::Index,
            object::{
                commit::Commit,
                signature::{Signature, SignatureType},
                tag::Tag as GitTag,
                tree::Tree,
                types::ObjectType,
            },
        },
    };
    use sea_orm::{ActiveModelTrait, Set};
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        command::{
            add::{self, AddArgs},
            commit::{self, CommitArgs},
            load_object, save_object,
        },
        internal::{
            config::ConfigKv, db::get_db_conn_instance, head::Head, model::reference,
            tag as internal_tag,
        },
        utils::{path, test},
    };

    fn test_tag_object(object_hash: ObjectHash, object_type: ObjectType, name: &str) -> GitTag {
        GitTag::new(
            object_hash,
            object_type,
            name.to_string(),
            Signature {
                signature_type: SignatureType::Tagger,
                name: "tester".to_string(),
                email: "tester@example.com".to_string(),
                timestamp: 1,
                timezone: "+0000".to_string(),
            },
            format!("{name} message"),
        )
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn direct_util_call_registers_lazy_fetch_hook_without_cli_parse() {
        let dir = tempdir().expect("create temp repo");
        test::setup_with_new_libra_in(dir.path()).await;
        let _cwd = test::ChangeDirGuard::new(dir.path());
        ConfigKv::set("extensions.partialClone", "origin", false)
            .await
            .expect("set extensions.partialClone");

        // Direct call to `utils::util::get_commit_base` without going through
        // `cli::parse` must automatically install the root lazy-fetch hook so
        // `libra-core` never reports LazyFetchUnregisteredError.
        let res = super::get_commit_base("HEAD").await;
        assert!(
            res.as_ref()
                .err()
                .is_none_or(|msg| !msg.contains("lazy-fetch hook is not registered")),
            "direct util call must register lazy-fetch hook, got {res:?}"
        );

        let missing = ObjectHash::from_type_and_data(ObjectType::Blob, b"direct-load-missing");
        let load_err = load_object::<git_internal::internal::object::blob::Blob>(&missing)
            .expect_err("missing object without remote should error");
        assert!(
            !load_err
                .to_string()
                .contains("lazy-fetch hook is not registered"),
            "direct command::load_object must register lazy-fetch hook, got {load_err}"
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn get_commit_base_typed_rejects_unborn_branch_before_hash_fallback() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        test::ensure_file("tracked.txt", Some("tracked\n"));
        add::execute(AddArgs {
            intent_to_add: false,
            sparse: false,
            pathspec: vec!["tracked.txt".into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
            resolved: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        })
        .await;
        commit::execute(CommitArgs {
            message: Some("base".into()),
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;

        let head_commit = Head::current_commit()
            .await
            .expect("expected committed HEAD");
        let branch_name = head_commit.to_string()[..7].to_string();

        let db = get_db_conn_instance().await;
        reference::ActiveModel {
            name: Set(Some(branch_name.clone())),
            kind: Set(reference::ConfigKind::Branch),
            commit: Set(None),
            remote: Set(None),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("failed to insert unborn branch");

        let error = get_commit_base_typed(&branch_name)
            .await
            .expect_err("unborn branch must not fall back to hash prefix resolution");
        assert!(matches!(error, CommitBaseError::InvalidReference(_)));
        assert!(
            error.to_string().contains(&format!(
                "branch '{branch_name}' does not point to a commit"
            )),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn get_commit_base_typed_resolves_bare_remote_via_cached_head() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        test::ensure_file("tracked.txt", Some("tracked\n"));
        add::execute(AddArgs {
            intent_to_add: false,
            sparse: false,
            pathspec: vec!["tracked.txt".into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
            resolved: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        })
        .await;
        commit::execute(CommitArgs {
            message: Some("base".into()),
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;

        let head_commit = Head::current_commit()
            .await
            .expect("expected committed HEAD");
        let db = get_db_conn_instance().await;

        // Cached remote HEAD (symbolic: kind Head + remote column, branch in name).
        reference::ActiveModel {
            name: Set(Some("main".to_string())),
            kind: Set(reference::ConfigKind::Head),
            commit: Set(Some(head_commit.to_string())),
            remote: Set(Some("origin".to_string())),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("failed to insert cached remote HEAD");

        // Mapped tracking branch under the fully-qualified writer shape.
        reference::ActiveModel {
            name: Set(Some("refs/remotes/origin/main".to_string())),
            kind: Set(reference::ConfigKind::Branch),
            commit: Set(Some(head_commit.to_string())),
            remote: Set(Some("origin".to_string())),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("failed to insert tracking branch");

        // Git DWIM: bare remote, `origin/HEAD` and its fully-qualified form all
        // resolve through the cached remote HEAD (issue #464 companion fix).
        assert_eq!(get_commit_base_typed("origin").await.unwrap(), head_commit);
        assert_eq!(
            get_commit_base_typed("origin/HEAD").await.unwrap(),
            head_commit
        );
        assert_eq!(
            get_commit_base_typed("refs/remotes/origin/HEAD")
                .await
                .unwrap(),
            head_commit
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn get_commit_base_typed_tag_object_hash_with_caret_zero_resolves_commit() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        test::ensure_file("tracked.txt", Some("tracked\n"));
        add::execute(AddArgs {
            intent_to_add: false,
            sparse: false,
            pathspec: vec!["tracked.txt".into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
            resolved: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        })
        .await;
        commit::execute(CommitArgs {
            message: Some("base".into()),
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;

        let head_commit = Head::current_commit()
            .await
            .expect("expected committed HEAD");
        let created = internal_tag::create("v1.0.0", Some("release".into()), false, false)
            .await
            .expect("failed to create annotated tag");

        let resolved = get_commit_base_typed(&format!("{}^0", created.target))
            .await
            .expect("tag object hash ^0 should resolve to the tagged commit");
        assert_eq!(resolved, head_commit);
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn get_commit_base_typed_peels_nested_tag_object_hash_to_commit() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        test::ensure_file("tracked.txt", Some("tracked\n"));
        add::execute(AddArgs {
            intent_to_add: false,
            sparse: false,
            pathspec: vec!["tracked.txt".into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
            resolved: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        })
        .await;
        commit::execute(CommitArgs {
            message: Some("base".into()),
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;

        let head_commit = Head::current_commit()
            .await
            .expect("expected committed HEAD");
        let inner = internal_tag::create("inner", Some("inner tag".into()), false, false)
            .await
            .expect("failed to create inner tag");
        let outer = test_tag_object(inner.target, ObjectType::Tag, "outer");
        save_object(&outer, &outer.id).expect("failed to save outer tag object");

        let resolved = get_commit_base_typed(&outer.id.to_string())
            .await
            .expect("nested tag object hash should resolve to commit");

        assert_eq!(resolved, head_commit);
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn get_commit_base_typed_reports_tag_cycle_as_corruption() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        test::ensure_file("tracked.txt", Some("tracked\n"));
        add::execute(AddArgs {
            intent_to_add: false,
            sparse: false,
            pathspec: vec!["tracked.txt".into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
            resolved: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        })
        .await;
        commit::execute(CommitArgs {
            message: Some("base".into()),
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;

        let head_commit = Head::current_commit()
            .await
            .expect("expected committed HEAD");
        let tag_a = test_tag_object(head_commit, ObjectType::Commit, "tag-a");
        let tag_b = test_tag_object(tag_a.id, ObjectType::Tag, "tag-b");
        let tag_a_cycle = test_tag_object(tag_b.id, ObjectType::Tag, "tag-a");
        save_object(&tag_b, &tag_b.id).expect("failed to save tag-b");
        save_object(&tag_a_cycle, &tag_a.id).expect("failed to save cyclic tag-a");

        let error = get_commit_base_typed(&tag_a.id.to_string())
            .await
            .expect_err("tag cycle should fail");

        assert!(matches!(error, CommitBaseError::CorruptReference(_)));
        assert!(error.to_string().contains("tag cycle detected"));
    }

    /// Commit `tracked.txt` in the current test repository and return HEAD.
    async fn commit_tracked_file_for_peel_test() -> ObjectHash {
        test::ensure_file("tracked.txt", Some("tracked\n"));
        add::execute(AddArgs {
            intent_to_add: false,
            sparse: false,
            pathspec: vec!["tracked.txt".into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
            resolved: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        })
        .await;
        commit::execute(CommitArgs {
            message: Some("base".into()),
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;
        Head::current_commit()
            .await
            .expect("expected committed HEAD")
    }

    /// issues/498 TT-05 G27: a tag of a tag of a commit peels to that commit.
    #[tokio::test]
    #[serial(cwd, env)]
    async fn peel_to_non_tag_typed_follows_nested_chain() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());
        let head_commit = commit_tracked_file_for_peel_test().await;
        let inner = test_tag_object(head_commit, ObjectType::Commit, "inner");
        let outer = test_tag_object(inner.id, ObjectType::Tag, "outer");
        save_object(&inner, &inner.id).expect("failed to save inner tag object");
        save_object(&outer, &outer.id).expect("failed to save outer tag object");

        let peeled = peel_to_non_tag_typed(outer.id, "outer").expect("nested chain should peel");

        assert_eq!(peeled, (head_commit, ObjectType::Commit));
    }

    /// issues/498 TT-05 G28: a tag whose chain ends at a tree reports the tree.
    #[tokio::test]
    #[serial(cwd, env)]
    async fn peel_to_non_tag_typed_reports_tree_end() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());
        let head_commit = commit_tracked_file_for_peel_test().await;
        let commit: Commit = load_object(&head_commit).expect("failed to load HEAD commit");
        let tree_tag = test_tag_object(commit.tree_id, ObjectType::Tree, "tree-tag");
        save_object(&tree_tag, &tree_tag.id).expect("failed to save tree tag object");

        let peeled =
            peel_to_non_tag_typed(tree_tag.id, "tree-tag").expect("tree-ending chain should peel");

        assert_eq!(peeled, (commit.tree_id, ObjectType::Tree));
    }

    /// issues/498 TT-05 G29: a tag cycle is corruption, not a peel result.
    #[tokio::test]
    #[serial(cwd, env)]
    async fn peel_to_non_tag_typed_reports_cycle_as_corruption() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());
        let head_commit = commit_tracked_file_for_peel_test().await;
        let tag_a = test_tag_object(head_commit, ObjectType::Commit, "tag-a");
        let tag_b = test_tag_object(tag_a.id, ObjectType::Tag, "tag-b");
        let tag_a_cycle = test_tag_object(tag_b.id, ObjectType::Tag, "tag-a");
        save_object(&tag_b, &tag_b.id).expect("failed to save tag-b");
        save_object(&tag_a_cycle, &tag_a.id).expect("failed to save cyclic tag-a");

        let error = peel_to_non_tag_typed(tag_a.id, "tag-a").expect_err("tag cycle should fail");

        assert!(matches!(error, CommitBaseError::CorruptReference(_)));
    }

    /// Stage `path` with `content` (no commit) for index-path resolver tests.
    async fn stage_file_for_resolver_test(path: &str, content: &str) {
        test::ensure_file(path, Some(content));
        add::execute(AddArgs {
            intent_to_add: false,
            sparse: false,
            pathspec: vec![path.into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
            resolved: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        })
        .await;
    }

    /// Stage and commit `path` with `content` under `message`.
    async fn commit_file_for_resolver_test(path: &str, content: &str, message: &str) -> ObjectHash {
        stage_file_for_resolver_test(path, content).await;
        commit::execute(CommitArgs {
            message: Some(message.into()),
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;
        Head::current_commit().await.expect("committed HEAD")
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn index_path_spec_resolves_staged_blob() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        stage_file_for_resolver_test("foo.txt", "hello\n").await;

        let index = Index::load(path::index()).expect("load index");
        let expected = index.get_hash("foo.txt", 0).expect("staged entry");

        assert_eq!(
            resolve_object_spec_typed(":foo.txt").await.unwrap(),
            expected
        );
        assert_eq!(
            resolve_object_spec_typed(":0:foo.txt").await.unwrap(),
            expected
        );

        let err = resolve_object_spec_typed(":missing.txt").await.unwrap_err();
        assert!(
            err.to_string().contains("is not in the index"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn non_colon_spec_does_not_read_index() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());
        let head = commit_tracked_file_for_peel_test().await;

        // Corrupt the index: a non-`:` spec must resolve without touching it.
        fs::write(path::index(), b"not an index").expect("corrupt index");
        assert_eq!(resolve_object_spec_typed("HEAD").await.unwrap(), head);
        assert_eq!(resolve_object_spec_typed("main").await.unwrap(), head);

        // A `:` spec DOES read the index, so it fails against the corrupt file.
        assert!(resolve_object_spec_typed(":tracked.txt").await.is_err());
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn message_search_favors_newer_matching_commit() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        // Two commits whose messages both match `common`, with controlled
        // increasing committer dates: the newer one must win for both the
        // `:/` and the `^{/}` syntax (t1500-rev-parse.sh shape).
        // SAFETY: this test holds the `serial(env)` key, so no other test
        // reads these variables while they are set.
        unsafe {
            env::set_var("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z");
            env::set_var("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z");
        }
        commit_file_for_resolver_test("old.txt", "old\n", "common-old").await;
        unsafe {
            env::set_var("GIT_COMMITTER_DATE", "2020-01-02T00:00:00Z");
            env::set_var("GIT_AUTHOR_DATE", "2020-01-02T00:00:00Z");
        }
        let newer = commit_file_for_resolver_test("new.txt", "new\n", "common-new").await;
        unsafe {
            env::remove_var("GIT_COMMITTER_DATE");
            env::remove_var("GIT_AUTHOR_DATE");
        }

        assert_eq!(
            resolve_object_spec_typed("HEAD").await.unwrap(),
            newer,
            "fixture: HEAD must be the newer commit"
        );
        assert_eq!(resolve_object_spec_typed(":/common").await.unwrap(), newer);
        assert_eq!(
            resolve_object_spec_typed("HEAD^{/common}").await.unwrap(),
            newer
        );
        // Empty pattern with an explicit start matches the start commit itself
        // (Git's `^{/}` quirk), while the `:/` empty-pattern form resolves
        // nothing (asserted in the invalid-reference test below).
        assert_eq!(resolve_object_spec_typed("HEAD^{/}").await.unwrap(), newer);
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn message_search_does_not_load_trees_or_blobs() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());
        let head =
            commit_file_for_resolver_test("tracked.txt", "tracked\n", "searchable base").await;

        // Delete the commit's tree and blob objects. The message search only
        // reads commit objects, so it must still resolve.
        let commit: Commit = load_object(&head).expect("load commit");
        let tree: Tree = load_object(&commit.tree_id).expect("load tree");
        let blob_id = tree.tree_items[0].id;

        let blob_hex = blob_id.to_string();
        let blob_path = path::objects().join(&blob_hex[..2]).join(&blob_hex[2..]);
        fs::remove_file(&blob_path).expect("delete blob object");

        let tree_hex = commit.tree_id.to_string();
        let tree_path = path::objects().join(&tree_hex[..2]).join(&tree_hex[2..]);
        fs::remove_file(&tree_path).expect("delete tree object");

        assert_eq!(
            resolve_object_spec_typed(":/searchable").await.unwrap(),
            head
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn message_search_invalid_regex_and_no_match_are_invalid_reference() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());
        commit_file_for_resolver_test("tracked.txt", "tracked\n", "searchable base").await;

        let err = resolve_object_spec_typed(":/[invalid").await.unwrap_err();
        assert!(matches!(err, CommitBaseError::InvalidReference(_)));

        let err = resolve_object_spec_typed(":/no-such-message")
            .await
            .unwrap_err();
        assert!(matches!(err, CommitBaseError::InvalidReference(_)));

        // Git's `get_oid_oneline` skips empty patterns: `:/` resolves nothing.
        let err = resolve_object_spec_typed(":/").await.unwrap_err();
        assert!(matches!(err, CommitBaseError::InvalidReference(_)));
    }
}
