//! Utilities module aggregator exposing storage, path, object, LFS, D1 client, and testing helpers.
//!
//! Core utility modules live in `crates/libra-core` and are re-exported here
//! with identical visibility so root `libra` exposes zero new `pub` APIs.

#[cfg(feature = "fastcdc")]
pub use libra_core::utils::media;
#[cfg(feature = "otlp")]
pub use libra_core::utils::telemetry;
pub use libra_core::utils::{
    approxidate, atomic_write, attributes, backoff, client_storage, config_color, d1_client,
    diff_format, error, funcname, fuse, grep_pattern, ignore, index_ext, json, lfs, log_config,
    object, object_ext, output, pager, path, path_case, path_ext, pathspec, read_policy, redact,
    resource_limits, shared_perm, stat_diff, storage, storage_ext, text, thin_pack, tree,
    tree_attributes, worktree, worktree_blob, worktree_layout,
};
pub(crate) use libra_core::utils::{
    atomic_stream, beneath, preview_object, preview_scratch, worktree_conversion,
};
#[cfg(windows)]
pub(crate) use libra_core::utils::{windows_fs, windows_process};

pub mod convert;
pub mod test;
pub mod util;

#[cfg(test)]
mod ignore_root_tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use git_internal::internal::index::Index;
    use serial_test::serial;
    use tempfile::tempdir;

    use crate::{
        command::{
            add::{self, AddArgs},
            status::{changes_to_be_committed_safe, changes_to_be_staged},
        },
        utils::{ignore::*, test},
    };

    fn build_index() -> Index {
        Index::load(crate::utils::path::index()).unwrap()
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn respect_policy_ignores_untracked_files() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        fs::write(".libraignore", "ignored.txt\n").unwrap();
        fs::write("ignored.txt", "ignored").unwrap();
        fs::write("tracked.txt", "tracked").unwrap();

        add::execute(AddArgs {
            intent_to_add: false,
            sparse: false,
            pathspec: vec!["tracked.txt".into()],
            all: false,
            update: false,
            refresh: false,
            force: false,
            verbose: false,
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

        let index = build_index();
        assert!(should_ignore(
            Path::new("ignored.txt"),
            IgnorePolicy::Respect,
            &index
        ));
        assert!(!should_ignore(
            Path::new("tracked.txt"),
            IgnorePolicy::Respect,
            &index
        ));
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn include_ignored_policy_keeps_untracked_files() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        fs::write(".libraignore", "ignored.txt\n").unwrap();
        fs::write("ignored.txt", "ignored").unwrap();
        fs::write("visible.txt", "visible").unwrap();

        let index = build_index();
        assert!(!should_ignore(
            Path::new("ignored.txt"),
            IgnorePolicy::IncludeIgnored,
            &index
        ));

        let filtered = filter_workdir_paths(
            vec![PathBuf::from("ignored.txt"), PathBuf::from("visible.txt")],
            IgnorePolicy::IncludeIgnored,
            &index,
        );
        assert_eq!(
            filtered,
            vec![PathBuf::from("ignored.txt"), PathBuf::from("visible.txt")]
        );

        let unstaged =
            crate::command::status::changes_to_be_staged_with_policy(IgnorePolicy::IncludeIgnored)
                .unwrap();
        assert!(
            unstaged.new.iter().any(|p| p == Path::new("ignored.txt")),
            "IncludeIgnored policy should surface ignored entries for staging workflows"
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn only_ignored_policy_returns_only_ignored_paths() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        fs::write(".libraignore", "ignored.txt\n").unwrap();
        fs::write("ignored.txt", "ignored").unwrap();
        fs::write("tracked.txt", "tracked").unwrap();

        add::execute(AddArgs {
            intent_to_add: false,
            sparse: false,
            pathspec: vec!["tracked.txt".into()],
            all: false,
            update: false,
            refresh: false,
            force: false,
            verbose: false,
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

        let index = build_index();
        let workdir_files = vec![PathBuf::from("ignored.txt"), PathBuf::from("tracked.txt")];
        let filtered =
            filter_workdir_paths(workdir_files.into_iter(), IgnorePolicy::OnlyIgnored, &index);
        assert_eq!(filtered, vec![PathBuf::from("ignored.txt")]);

        let staged = changes_to_be_committed_safe().await.unwrap();
        assert!(staged.new.iter().any(|p| p == Path::new("tracked.txt")));

        let unstaged = changes_to_be_staged().unwrap();
        assert!(!unstaged.new.iter().any(|p| p == Path::new("ignored.txt")));
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn only_ignored_policy_excludes_tracked_entries() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        fs::write(".libraignore", "ignored.txt\n").unwrap();
        fs::write("ignored.txt", "initial").unwrap();

        add::execute(AddArgs {
            intent_to_add: false,
            sparse: false,
            pathspec: vec!["ignored.txt".into()],
            all: false,
            update: false,
            refresh: false,
            force: true,
            verbose: false,
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

        let index = build_index();
        assert!(
            index.tracked("ignored.txt", 0),
            "sanity check: ignored file should now be tracked"
        );

        let filtered = filter_workdir_paths(
            vec![PathBuf::from("ignored.txt")],
            IgnorePolicy::OnlyIgnored,
            &index,
        );
        assert!(
            filtered.is_empty(),
            "tracked entries must be removed when requesting only ignored files"
        );

        let only_ignored =
            crate::command::status::changes_to_be_staged_with_policy(IgnorePolicy::OnlyIgnored)
                .unwrap();
        assert!(
            !only_ignored
                .new
                .iter()
                .any(|p| p == Path::new("ignored.txt")),
            "OnlyIgnored policy should hide tracked files even if they match ignore patterns"
        );
    }

    /// Regression test for issue #387: a nested `.git` directory must be
    /// force-ignored like Git. It must not appear as untracked in status and
    /// must not be staged by `add .` or `add --force`, even when a
    /// `.libraignore` whitelist rule tries to un-ignore it.
    #[tokio::test]
    #[serial(cwd, env)]
    async fn git_directory_is_force_ignored_like_git() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        // Simulate a user-initialised nested git repository plus a real file.
        fs::create_dir_all(".git/refs/heads").unwrap();
        fs::write(".git/config", "[core]\n").unwrap();
        fs::write(".git/HEAD", "ref: refs/heads/main\n").unwrap();
        fs::write("real.txt", "hello").unwrap();
        // A whitelist rule must NOT be able to un-ignore `.git`.
        fs::write(".libraignore", "!.git\n!.git/**\n").unwrap();

        let index = build_index();
        assert!(should_ignore(
            Path::new(".git"),
            IgnorePolicy::Respect,
            &index
        ));
        assert!(should_ignore(
            Path::new(".git/config"),
            IgnorePolicy::Respect,
            &index
        ));

        // status must not surface `.git` contents as untracked.
        let unstaged = crate::command::status::changes_to_be_staged().unwrap();
        assert!(
            !unstaged
                .new
                .iter()
                .any(|p| p.starts_with(".git") && p != Path::new(".gitignore")),
            ".git must never be reported as untracked"
        );

        // `add --force .` must not stage anything under `.git`.
        add::execute(AddArgs {
            intent_to_add: false,
            sparse: false,
            pathspec: vec![".".into()],
            all: false,
            update: false,
            refresh: false,
            force: true,
            verbose: false,
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

        let index = build_index();
        assert!(
            !index.tracked(".git/config", 0),
            ".git/config must not be staged even with --force"
        );
        assert!(
            !index.tracked(".git/HEAD", 0),
            ".git/HEAD must not be staged even with --force"
        );
        assert!(
            index.tracked("real.txt", 0),
            "a normal file should still be staged"
        );
    }
}
