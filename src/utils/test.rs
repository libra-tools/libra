//! Root test support utilities: re-exports all low-level helpers from
//! `libra-core::utils::test` and keeps the CLI `command::init::init`-backed
//! `setup_with_new_libra_in` wrapper for root unit and integration tests.

use std::path::Path;

pub use libra_core::utils::test::*;

use crate::{command, internal::config::ConfigKv, utils::util};

/// Switch to `temp_path` and create a new `.libra` via the full CLI `init` path.
pub async fn setup_with_new_libra_in(temp_path: impl AsRef<Path>) {
    setup_clean_testing_env_in(temp_path.as_ref());
    util::reset_objects_storage_cache_for_path(
        &temp_path.as_ref().join(util::ROOT_DIR).join("objects"),
    );
    let _guard = ChangeDirGuard::new(temp_path.as_ref());
    let args = command::init::InitArgs {
        bare: false,
        initial_branch: None,
        repo_directory: temp_path.as_ref().to_str().unwrap().to_string(),
        template: None,
        quiet: false,
        shared: None,
        object_format: None,
        ref_format: None,
        separate_git_dir: None,
        separate_libra_dir: None,
        recurse_submodules: false,
        from_git_repository: None,
        vault: false,
    };
    command::init::init(args).await.unwrap();
    util::reset_objects_storage_cache_for_path(
        &temp_path.as_ref().join(util::ROOT_DIR).join("objects"),
    );

    let _ = ConfigKv::set("user.name", "Libra Test User", false).await;
    let _ = ConfigKv::set("user.email", "libra-test@example.com", false).await;
}

#[cfg(test)]
mod bootstrap_equivalence_tests {
    use serial_test::serial;
    use tempfile::tempdir;

    use crate::{
        internal::{config::ConfigKv, head::Head},
        utils::test,
    };

    #[tokio::test]
    #[serial(cwd, env)]
    async fn test_core_bootstrap_matches_cli_init_contract() {
        let cli_dir = tempdir().expect("cli init tempdir");
        let core_dir = tempdir().expect("core bootstrap tempdir");

        test::setup_with_new_libra_in(cli_dir.path()).await;
        libra_core::utils::test::setup_with_new_libra_in(core_dir.path()).await;

        // Compare essential layout directories.
        for rel in [
            ".libra",
            ".libra/libra.db",
            ".libra/objects/info",
            ".libra/objects/pack",
            ".libra/info",
            ".libra/hooks",
        ] {
            assert!(cli_dir.path().join(rel).exists(), "cli init missing {rel}");
            assert!(
                core_dir.path().join(rel).exists(),
                "core bootstrap missing {rel}"
            );
        }

        // Compare HEAD and core config keys (excluding random libra.repoid value).
        let read_snapshot = |repo: &std::path::Path| {
            let repo = repo.to_path_buf();
            async move {
                let _cwd = test::ChangeDirGuard::new(&repo);
                let head = Head::current().await;
                let mut pairs = Vec::new();
                for key in [
                    "core.repositoryformatversion",
                    "core.bare",
                    "core.filemode",
                    "core.logallrefupdates",
                    "core.ignorecase",
                    "core.objectformat",
                    "core.initrefformat",
                    "vault.signing",
                    "user.name",
                    "user.email",
                ] {
                    let val = ConfigKv::get(key).await.ok().flatten().map(|e| e.value);
                    pairs.push((key, val));
                }
                let has_repoid = ConfigKv::get("libra.repoid").await.ok().flatten().is_some();
                (head, pairs, has_repoid)
            }
        };

        let cli_snap = read_snapshot(cli_dir.path()).await;
        let core_snap = read_snapshot(core_dir.path()).await;
        assert_eq!(
            format!("{:?}", cli_snap.0),
            format!("{:?}", core_snap.0),
            "HEAD must match"
        );
        assert_eq!(cli_snap.1, core_snap.1, "core config keys must match");
        assert!(cli_snap.2 && core_snap.2, "both must seed libra.repoid");
    }
}
