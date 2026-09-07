//! Tests for remote subcommands validating add/list/show behavior and URL mutation scenarios.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use libra::{
    command::{
        fetch,
        remote::{self, RemoteCmds},
    },
    internal::{
        branch::Branch,
        config::{ConfigKv, RemoteConfig},
    },
    utils::{error::StableErrorCode, output::OutputConfig},
};

use super::*;

#[tokio::test]
#[serial(cwd)]
async fn test_remote_add_creates_entry() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    remote::execute(RemoteCmds::Add {
        name: "origin".into(),
        url: "https://example.com/repo.git".into(),
        fetch: false,
        track: vec![],
        master: None,
        tags: false,
        no_tags: false,
        mirror: false,
    })
    .await;

    // Verify the remote URL is stored as expected.
    let remote = ConfigKv::remote_config("origin").await.ok().flatten();
    assert!(remote.is_some(), "remote should exist after add");
    assert_eq!(remote.unwrap().url, "https://example.com/repo.git");
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_add_duplicate_name_returns_error() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    remote::execute_safe(
        RemoteCmds::Add {
            name: "origin".into(),
            url: "https://example.com/repo.git".into(),
            fetch: false,
            track: vec![],
            master: None,
            tags: false,
            no_tags: false,
            mirror: false,
        },
        &OutputConfig::default(),
    )
    .await
    .expect("first add should succeed");

    let result = remote::execute_safe(
        RemoteCmds::Add {
            name: "origin".into(),
            url: "https://example.com/another.git".into(),
            fetch: false,
            track: vec![],
            master: None,
            tags: false,
            no_tags: false,
            mirror: false,
        },
        &OutputConfig::default(),
    )
    .await;

    assert!(result.is_err(), "adding existing remote should fail");
    let err = result.unwrap_err();
    assert!(
        err.render()
            .contains("fatal: remote 'origin' already exists"),
        "unexpected error: {}",
        err.render()
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_add_cold_config_flags() {
    use libra::internal::{db::get_db_conn_instance, model::reference};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    let p = repo_dir.path();

    // -t (repeatable), --tags, and -m together.
    assert_cli_success(
        &run_libra_command(
            &[
                "remote",
                "add",
                "-t",
                "main",
                "-t",
                "dev",
                "--tags",
                "-m",
                "main",
                "origin",
                "https://example.com/r.git",
            ],
            p,
        ),
        "remote add with cold-config flags",
    );

    // -t writes one specific fetch refspec per branch (instead of a wildcard).
    let fetch = ConfigKv::get_all("remote.origin.fetch")
        .await
        .expect("read fetch")
        .into_iter()
        .map(|e| e.value)
        .collect::<Vec<_>>();
    assert_eq!(
        fetch,
        vec![
            "+refs/heads/main:refs/remotes/origin/main".to_string(),
            "+refs/heads/dev:refs/remotes/origin/dev".to_string(),
        ],
        "-t writes a specific refspec per branch: {fetch:?}"
    );

    // --tags records the tag-fetch preference under the exact key `libra fetch`
    // reads (`remote.<name>.tagOpt`, camelCase — config keys are case-sensitive).
    let tagopt = ConfigKv::get("remote.origin.tagOpt")
        .await
        .expect("read tagOpt")
        .map(|e| e.value);
    assert_eq!(tagopt.as_deref(), Some("--tags"));

    // -m writes the remote HEAD (a Head row keyed by the remote name), even
    // though the tracking ref does not exist yet.
    let db = get_db_conn_instance().await;
    let head_row = reference::Entity::find()
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
        .filter(reference::Column::Remote.eq("origin".to_string()))
        .one(&db)
        .await
        .expect("query remote HEAD");
    assert!(head_row.is_some(), "-m writes refs/remotes/origin/HEAD");

    // --no-tags records the opposite preference; with no -t, no fetch refspec
    // is written (the default wildcard remains implicit, as for a plain add).
    assert_cli_success(
        &run_libra_command(
            &[
                "remote",
                "add",
                "--no-tags",
                "up",
                "https://example.com/u.git",
            ],
            p,
        ),
        "remote add --no-tags",
    );
    let up_tagopt = ConfigKv::get("remote.up.tagOpt")
        .await
        .expect("read up tagOpt")
        .map(|e| e.value);
    assert_eq!(up_tagopt.as_deref(), Some("--no-tags"));
    assert!(
        ConfigKv::get_all("remote.up.fetch")
            .await
            .expect("read up fetch")
            .is_empty(),
        "plain add (no -t) writes no fetch refspec"
    );

    // --tags and --no-tags are mutually exclusive (clap usage error, exit 129).
    let conflict = run_libra_command(
        &[
            "remote",
            "add",
            "--tags",
            "--no-tags",
            "x",
            "https://e.com/x.git",
        ],
        p,
    );
    assert_eq!(conflict.status.code(), Some(129));

    // An invalid -t branch name is rejected (usage error, exit 129) and the
    // remote is NOT persisted (validation runs before any config write).
    let bad_track = run_libra_command(
        &[
            "remote",
            "add",
            "-t",
            "bad name",
            "bt",
            "https://e.com/bt.git",
        ],
        p,
    );
    assert_eq!(bad_track.status.code(), Some(129));
    assert!(
        ConfigKv::get("remote.bt.url")
            .await
            .expect("read bt url")
            .is_none(),
        "remote with an invalid -t branch must not be persisted"
    );

    // Likewise an invalid -m branch name.
    let bad_master = run_libra_command(
        &[
            "remote",
            "add",
            "-m",
            "refs/heads/x",
            "bm",
            "https://e.com/bm.git",
        ],
        p,
    );
    assert_eq!(bad_master.status.code(), Some(129));
    assert!(
        ConfigKv::get("remote.bm.url")
            .await
            .expect("read bm url")
            .is_none(),
        "remote with an invalid -m branch must not be persisted"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_remove_deletes_entry() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    remote::execute(RemoteCmds::Add {
        name: "origin".into(),
        url: "https://example.com/repo.git".into(),
        fetch: false,
        track: vec![],
        master: None,
        tags: false,
        no_tags: false,
        mirror: false,
    })
    .await;

    remote::execute(RemoteCmds::Remove {
        name: "origin".into(),
    })
    .await;

    // Ensure the entry is gone from configuration.
    let remote = ConfigKv::remote_config("origin").await.ok().flatten();
    assert!(remote.is_none(), "remote should be removed");
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_remove_deletes_vault_ssh_keys() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    remote::execute(RemoteCmds::Add {
        name: "origin".into(),
        url: "https://example.com/repo.git".into(),
        fetch: false,
        track: vec![],
        master: None,
        tags: false,
        no_tags: false,
        mirror: false,
    })
    .await;
    ConfigKv::set("vault.ssh.origin.pubkey", "ssh-rsa origin", false)
        .await
        .unwrap();
    ConfigKv::set("vault.ssh.origin.privkey", "origin-private", true)
        .await
        .unwrap();

    remote::execute_safe(
        RemoteCmds::Remove {
            name: "origin".into(),
        },
        &OutputConfig::default(),
    )
    .await
    .expect("remote remove should succeed");

    assert!(
        ConfigKv::get("vault.ssh.origin.pubkey")
            .await
            .expect("query pubkey")
            .is_none(),
        "remote remove must delete the origin SSH public key"
    );
    assert!(
        ConfigKv::get("vault.ssh.origin.privkey")
            .await
            .expect("query privkey")
            .is_none(),
        "remote remove must delete the origin SSH private key"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_rename_updates_branch_tracking() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    remote::execute(RemoteCmds::Add {
        name: "origin".into(),
        url: "https://example.com/repo.git".into(),
        fetch: false,
        track: vec![],
        master: None,
        tags: false,
        no_tags: false,
        mirror: false,
    })
    .await;

    // Mirror Git's tracking layout for the main branch.
    ConfigKv::set("branch.main.remote", "origin", false)
        .await
        .unwrap();
    ConfigKv::set("branch.main.merge", "refs/heads/main", false)
        .await
        .unwrap();

    remote::execute(RemoteCmds::Rename {
        old: "origin".into(),
        new: "upstream".into(),
    })
    .await;

    assert!(
        ConfigKv::remote_config("origin")
            .await
            .ok()
            .flatten()
            .is_none(),
        "old remote entry should be gone"
    );

    // The new remote name should retain the original URL.
    let renamed = ConfigKv::remote_config("upstream").await.ok().flatten();
    assert!(renamed.is_some(), "new remote entry should exist");
    assert_eq!(
        renamed.unwrap().url,
        "https://example.com/repo.git",
        "URL should be preserved after rename"
    );

    let branch_remote = ConfigKv::get("branch.main.remote")
        .await
        .ok()
        .flatten()
        .map(|e| e.value);
    assert_eq!(
        branch_remote.as_deref(),
        Some("upstream"),
        "tracking branch should reference the new remote name"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_rename_cascades_vault_ssh_keys() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    remote::execute(RemoteCmds::Add {
        name: "origin".into(),
        url: "https://example.com/repo.git".into(),
        fetch: false,
        track: vec![],
        master: None,
        tags: false,
        no_tags: false,
        mirror: false,
    })
    .await;
    ConfigKv::set("vault.ssh.origin.pubkey", "ssh-rsa origin", false)
        .await
        .unwrap();
    ConfigKv::set("vault.ssh.origin.privkey", "origin-private", true)
        .await
        .unwrap();

    remote::execute_safe(
        RemoteCmds::Rename {
            old: "origin".into(),
            new: "upstream".into(),
        },
        &OutputConfig::default(),
    )
    .await
    .expect("remote rename should succeed");

    assert!(
        ConfigKv::get("vault.ssh.origin.pubkey")
            .await
            .expect("query old pubkey")
            .is_none(),
        "old SSH public key namespace must be removed"
    );
    assert!(
        ConfigKv::get("vault.ssh.origin.privkey")
            .await
            .expect("query old privkey")
            .is_none(),
        "old SSH private key namespace must be removed"
    );
    assert_eq!(
        ConfigKv::get("vault.ssh.upstream.pubkey")
            .await
            .expect("query new pubkey")
            .map(|entry| entry.value)
            .as_deref(),
        Some("ssh-rsa origin")
    );
    assert_eq!(
        ConfigKv::get("vault.ssh.upstream.privkey")
            .await
            .expect("query new privkey")
            .map(|entry| entry.value)
            .as_deref(),
        Some("origin-private")
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_rename_refuses_existing_target_vault_ssh_namespace() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    remote::execute(RemoteCmds::Add {
        name: "origin".into(),
        url: "https://example.com/repo.git".into(),
        fetch: false,
        track: vec![],
        master: None,
        tags: false,
        no_tags: false,
        mirror: false,
    })
    .await;
    ConfigKv::set("vault.ssh.origin.pubkey", "ssh-rsa origin", false)
        .await
        .unwrap();
    ConfigKv::set("vault.ssh.upstream.pubkey", "ssh-rsa stale-upstream", false)
        .await
        .unwrap();

    let result = remote::execute_safe(
        RemoteCmds::Rename {
            old: "origin".into(),
            new: "upstream".into(),
        },
        &OutputConfig::default(),
    )
    .await;

    assert!(
        result.is_err(),
        "rename must refuse a target with existing vault SSH keys"
    );
    let err = result.unwrap_err();
    assert_eq!(err.stable_code(), StableErrorCode::ConflictOperationBlocked);
    assert!(
        err.render()
            .contains("SSH key namespace for remote 'upstream' already exists"),
        "error should explain the stale SSH key namespace: {}",
        err.render()
    );
    assert!(
        ConfigKv::remote_config("origin")
            .await
            .expect("query origin")
            .is_some(),
        "failed rename must keep the source remote"
    );
    assert!(
        ConfigKv::remote_config("upstream")
            .await
            .expect("query upstream")
            .is_none(),
        "failed rename must not create the target remote"
    );
    assert_eq!(
        ConfigKv::get("vault.ssh.origin.pubkey")
            .await
            .expect("query source ssh key")
            .map(|entry| entry.value)
            .as_deref(),
        Some("ssh-rsa origin")
    );
    assert_eq!(
        ConfigKv::get("vault.ssh.upstream.pubkey")
            .await
            .expect("query target ssh key")
            .map(|entry| entry.value)
            .as_deref(),
        Some("ssh-rsa stale-upstream")
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_configkv_rename_refuses_existing_target_vault_ssh_namespace_without_partial_mutation()
{
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    ConfigKv::set("remote.origin.url", "https://example.com/repo.git", false)
        .await
        .unwrap();
    ConfigKv::set("branch.main.remote", "origin", false)
        .await
        .unwrap();
    ConfigKv::set("vault.ssh.origin.pubkey", "ssh-rsa origin", false)
        .await
        .unwrap();
    ConfigKv::set("vault.ssh.upstream.pubkey", "ssh-rsa stale-upstream", false)
        .await
        .unwrap();

    let result = ConfigKv::rename_remote("origin", "upstream").await;

    assert!(
        result.is_err(),
        "ConfigKv rename must refuse a pre-existing target SSH namespace"
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("SSH key namespace for remote 'upstream' already exists")
    );
    assert_eq!(
        ConfigKv::get("remote.origin.url")
            .await
            .expect("query origin remote")
            .map(|entry| entry.value)
            .as_deref(),
        Some("https://example.com/repo.git"),
        "failed lower-level rename must keep the source remote"
    );
    assert!(
        ConfigKv::get("remote.upstream.url")
            .await
            .expect("query target remote")
            .is_none(),
        "failed lower-level rename must not create the target remote"
    );
    assert_eq!(
        ConfigKv::get("branch.main.remote")
            .await
            .expect("query branch remote")
            .map(|entry| entry.value)
            .as_deref(),
        Some("origin"),
        "failed lower-level rename must not repoint branch tracking"
    );
    assert_eq!(
        ConfigKv::get("vault.ssh.origin.pubkey")
            .await
            .expect("query source ssh")
            .map(|entry| entry.value)
            .as_deref(),
        Some("ssh-rsa origin"),
        "failed lower-level rename must keep source SSH keys"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_rename_conflict_returns_error() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    remote::execute(RemoteCmds::Add {
        name: "origin".into(),
        url: "https://example.com/repo.git".into(),
        fetch: false,
        track: vec![],
        master: None,
        tags: false,
        no_tags: false,
        mirror: false,
    })
    .await;
    remote::execute(RemoteCmds::Add {
        name: "upstream".into(),
        url: "https://example.com/upstream.git".into(),
        fetch: false,
        track: vec![],
        master: None,
        tags: false,
        no_tags: false,
        mirror: false,
    })
    .await;

    // Attempt to rename into the existing target and expect failure.
    let result = ConfigKv::rename_remote("origin", "upstream").await;
    assert!(result.is_err(), "rename into existing name should fail");
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_set_url_add_appends_fetch_url() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    // initial url
    remote::execute(RemoteCmds::Add {
        name: "origin".into(),
        url: "https://example.com/repo.git".into(),
        fetch: false,
        track: vec![],
        master: None,
        tags: false,
        no_tags: false,
        mirror: false,
    })
    .await;

    // append a second fetch URL with --add
    remote::execute(RemoteCmds::SetUrl {
        add: true,
        delete: false,
        push: false,
        all: false,
        name: "origin".into(),
        value: "https://mirror.example.com/repo.git".into(),
    })
    .await;

    let urls: Vec<String> = ConfigKv::get_all("remote.origin.url")
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.value)
        .collect();
    assert_eq!(urls.len(), 2, "should have two fetch urls after --add");
    assert!(urls.contains(&"https://example.com/repo.git".to_string()));
    assert!(urls.contains(&"https://mirror.example.com/repo.git".to_string()));
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_set_url_delete_removes_matching_url() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    remote::execute(RemoteCmds::Add {
        name: "origin".into(),
        url: "https://example.com/repo.git".into(),
        fetch: false,
        track: vec![],
        master: None,
        tags: false,
        no_tags: false,
        mirror: false,
    })
    .await;
    remote::execute(RemoteCmds::SetUrl {
        add: true,
        delete: false,
        push: false,
        all: false,
        name: "origin".into(),
        value: "https://mirror.example.com/repo.git".into(),
    })
    .await;

    // delete the mirror url using --delete
    remote::execute(RemoteCmds::SetUrl {
        add: false,
        delete: true,
        push: false,
        all: false,
        name: "origin".into(),
        value: "mirror.example.com".into(),
    })
    .await;

    let urls: Vec<String> = ConfigKv::get_all("remote.origin.url")
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.value)
        .collect();
    assert_eq!(urls.len(), 1, "should have one fetch url after --delete");
    assert_eq!(urls[0], "https://example.com/repo.git");
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_set_url_push_and_get_pushurl_entries() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    remote::execute(RemoteCmds::Add {
        name: "origin".into(),
        url: "https://example.com/repo.git".into(),
        fetch: false,
        track: vec![],
        master: None,
        tags: false,
        no_tags: false,
        mirror: false,
    })
    .await;

    // add a pushurl entry
    remote::execute(RemoteCmds::SetUrl {
        add: true,
        delete: false,
        push: true,
        all: false,
        name: "origin".into(),
        value: "ssh://git@example.com/repo.git".into(),
    })
    .await;

    let pushurls: Vec<String> = ConfigKv::get_all("remote.origin.pushurl")
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.value)
        .collect();
    assert_eq!(
        pushurls.len(),
        1,
        "should have one pushurl after --add --push"
    );
    assert_eq!(pushurls[0], "ssh://git@example.com/repo.git");

    // Calling get-url --push should prefer pushurl entries (we don't capture stdout here,
    // but ensure the command runs without panic)
    remote::execute(RemoteCmds::GetUrl {
        push: true,
        all: false,
        name: "origin".into(),
    })
    .await;
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_set_url_all_replaces_all_fetch_urls() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    remote::execute(RemoteCmds::Add {
        name: "origin".into(),
        url: "https://one.example/repo.git".into(),
        fetch: false,
        track: vec![],
        master: None,
        tags: false,
        no_tags: false,
        mirror: false,
    })
    .await;
    remote::execute(RemoteCmds::SetUrl {
        add: true,
        delete: false,
        push: false,
        all: false,
        name: "origin".into(),
        value: "https://two.example/repo.git".into(),
    })
    .await;

    // Replace all fetch urls with a single new one
    remote::execute(RemoteCmds::SetUrl {
        add: false,
        delete: false,
        push: false,
        all: true,
        name: "origin".into(),
        value: "https://replaced.example/repo.git".into(),
    })
    .await;

    let urls: Vec<String> = ConfigKv::get_all("remote.origin.url")
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.value)
        .collect();
    assert_eq!(urls.len(), 1, "--all should leave exactly one fetch url");
    assert_eq!(urls[0], "https://replaced.example/repo.git");

    // get-url --all should run without panicking even when printing multiple/single entries
    remote::execute(RemoteCmds::GetUrl {
        push: false,
        all: true,
        name: "origin".into(),
    })
    .await;
}

#[test]
fn test_remote_verbose_cli_lists_all_fetch_and_push_urls() {
    let repo = tempdir().expect("failed to create repo");
    init_repo_via_cli(repo.path());

    let add_output = run_libra_command(
        &["remote", "add", "origin", "https://one.example/repo.git"],
        repo.path(),
    );
    assert_cli_success(&add_output, "remote add origin");

    let add_fetch_url = run_libra_command(
        &[
            "remote",
            "set-url",
            "--add",
            "origin",
            "https://two.example/repo.git",
        ],
        repo.path(),
    );
    assert_cli_success(&add_fetch_url, "remote set-url --add origin");

    let add_push_url = run_libra_command(
        &[
            "remote",
            "set-url",
            "--add",
            "--push",
            "origin",
            "ssh://git@example.com/repo.git",
        ],
        repo.path(),
    );
    assert_cli_success(&add_push_url, "remote set-url --add --push origin");

    let output = run_libra_command(&["remote", "-v"], repo.path());
    assert_cli_success(&output, "remote -v");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("origin\thttps://one.example/repo.git (fetch)"),
        "missing first fetch URL: {stdout}"
    );
    assert!(
        stdout.contains("origin\thttps://two.example/repo.git (fetch)"),
        "missing second fetch URL: {stdout}"
    );
    assert!(
        stdout.contains("origin\tssh://git@example.com/repo.git (push)"),
        "missing push URL: {stdout}"
    );
    assert!(
        !stdout.contains("origin\thttps://one.example/repo.git (push)"),
        "verbose output should prefer explicit pushurl entries: {stdout}"
    );
}

#[test]
fn test_remote_get_url_json_output_is_structured() {
    let repo = tempdir().expect("failed to create repo");
    init_repo_via_cli(repo.path());

    let add_output = run_libra_command(
        &["remote", "add", "origin", "https://one.example/repo.git"],
        repo.path(),
    );
    assert_cli_success(&add_output, "remote add origin");

    let add_fetch_url = run_libra_command(
        &[
            "remote",
            "set-url",
            "--add",
            "origin",
            "https://two.example/repo.git",
        ],
        repo.path(),
    );
    assert_cli_success(&add_fetch_url, "remote set-url --add origin");

    let output = run_libra_command(
        &["--json", "remote", "get-url", "--all", "origin"],
        repo.path(),
    );
    assert_cli_success(&output, "remote get-url --json");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "remote");
    assert_eq!(json["data"]["action"], "urls");
    assert_eq!(json["data"]["name"], "origin");
    assert_eq!(json["data"]["push"], false);
    assert_eq!(json["data"]["all"], true);
    assert_eq!(
        json["data"]["urls"],
        serde_json::json!([
            "https://one.example/repo.git",
            "https://two.example/repo.git"
        ])
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_prune_removes_stale_branches() {
    let temp_root = tempdir().unwrap();
    let remote_dir = temp_root.path().join("remote.git");
    let work_dir = temp_root.path().join("workdir");

    // Create a bare Git repository as remote
    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .unwrap_or_else(|e| panic!("failed to init bare remote: {}", e))
            .success()
    );

    // Create a working Git repository to push branches from
    assert!(
        Command::new("git")
            .args(["init", work_dir.to_str().unwrap()])
            .status()
            .unwrap_or_else(|e| panic!("failed to init working repo: {}", e))
            .success()
    );

    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["config", "user.name", "Libra Tester"])
            .status()
            .unwrap_or_else(|e| panic!("failed to set user.name: {}", e))
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["config", "user.email", "tester@example.com"])
            .status()
            .unwrap_or_else(|e| panic!("failed to set user.email: {}", e))
            .success()
    );

    // Create initial commit
    fs::write(work_dir.join("README.md"), "hello libra")
        .unwrap_or_else(|e| panic!("failed to write README: {}", e));
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["add", "README.md"])
            .status()
            .unwrap_or_else(|e| panic!("failed to add README: {}", e))
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["commit", "-m", "initial commit"])
            .status()
            .unwrap_or_else(|e| panic!("failed to commit: {}", e))
            .success()
    );

    // Get current branch name
    let current_branch = String::from_utf8(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap_or_else(|e| panic!("failed to read current branch: {}", e))
            .stdout,
    )
    .unwrap_or_else(|e| panic!("branch name not utf8: {}", e))
    .trim()
    .to_string();

    // Add remote and push initial branch
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["remote", "add", "origin", remote_dir.to_str().unwrap()])
            .status()
            .unwrap_or_else(|e| panic!("failed to add origin remote: {}", e))
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args([
                "push",
                "origin",
                &format!("HEAD:refs/heads/{}", current_branch),
            ])
            .status()
            .unwrap_or_else(|e| panic!("failed to push to remote: {}", e))
            .success()
    );

    // Create and push additional branches
    let branches_to_create = vec!["feature1", "feature2", "feature3"];
    for branch_name in &branches_to_create {
        assert!(
            Command::new("git")
                .current_dir(&work_dir)
                .args(["checkout", "-b", branch_name])
                .status()
                .unwrap_or_else(|e| panic!("failed to create branch {}: {}", branch_name, e))
                .success()
        );
        assert!(
            Command::new("git")
                .current_dir(&work_dir)
                .args(["push", "origin", branch_name])
                .status()
                .unwrap_or_else(|e| panic!("failed to push branch {}: {}", branch_name, e))
                .success()
        );
    }

    // Initialize a fresh Libra repository to fetch into
    let repo_dir = temp_root.path().join("libra_repo");
    fs::create_dir_all(&repo_dir).unwrap_or_else(|e| panic!("failed to create repo dir: {}", e));
    test::setup_with_new_libra_in(&repo_dir).await;
    let _guard = test::ChangeDirGuard::new(&repo_dir);

    let remote_path = remote_dir.to_str().unwrap().to_string();
    ConfigKv::set("remote.origin.url", &remote_path, false)
        .await
        .unwrap();

    // Fetch all branches to create remote-tracking branches
    let quiet_output = OutputConfig::resolve(None, false, true, "auto", true, false, "none");

    fetch::fetch_repository_safe(
        RemoteConfig {
            name: "origin".to_string(),
            url: remote_path.clone(),
        },
        None,
        false,
        None,
        None,
        &quiet_output,
    )
    .await
    .expect("initial remote update fixture fetch should succeed");

    // Verify all remote-tracking branches exist
    for branch_name in &branches_to_create {
        let tracked_branch = format!("refs/remotes/origin/{}", branch_name);
        assert!(
            Branch::find_branch_result(&tracked_branch, Some("origin"))
                .await
                .expect("failed to query remote-tracking branch")
                .is_some(),
            "remote-tracking branch {} should exist after fetch",
            tracked_branch
        );
    }

    // Delete some branches from remote
    let branches_to_delete = vec!["feature1", "feature3"];
    for branch_name in &branches_to_delete {
        assert!(
            Command::new("git")
                .current_dir(remote_dir.to_str().unwrap())
                .args(["update-ref", "-d", &format!("refs/heads/{}", branch_name)])
                .status()
                .unwrap_or_else(|e| panic!("failed to delete branch {}: {}", branch_name, e))
                .success()
        );
    }

    // Run prune command
    remote::execute(RemoteCmds::Prune {
        name: "origin".into(),
        dry_run: false,
    })
    .await;

    // Verify stale branches are pruned
    for branch_name in &branches_to_delete {
        let tracked_branch = format!("refs/remotes/origin/{}", branch_name);
        assert!(
            Branch::find_branch_result(&tracked_branch, Some("origin"))
                .await
                .expect("failed to query remote-tracking branch")
                .is_none(),
            "stale remote-tracking branch {} should be pruned",
            tracked_branch
        );
    }

    // Verify remaining branches still exist
    assert!(
        Branch::find_branch_result("refs/remotes/origin/feature2", Some("origin"))
            .await
            .expect("failed to query remote-tracking branch")
            .is_some(),
        "non-stale remote-tracking branch should still exist"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_prune_dry_run_previews_changes() {
    let temp_root = tempdir().unwrap();
    let remote_dir = temp_root.path().join("remote.git");
    let work_dir = temp_root.path().join("workdir");

    // Create a bare Git repository as remote
    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .unwrap_or_else(|e| panic!("failed to init bare remote: {}", e))
            .success()
    );

    // Create a working Git repository to push branches from
    assert!(
        Command::new("git")
            .args(["init", work_dir.to_str().unwrap()])
            .status()
            .unwrap_or_else(|e| panic!("failed to init working repo: {}", e))
            .success()
    );

    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["config", "user.name", "Libra Tester"])
            .status()
            .unwrap_or_else(|e| panic!("failed to set user.name: {}", e))
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["config", "user.email", "tester@example.com"])
            .status()
            .unwrap_or_else(|e| panic!("failed to set user.email: {}", e))
            .success()
    );

    // Create initial commit
    fs::write(work_dir.join("README.md"), "hello libra")
        .unwrap_or_else(|e| panic!("failed to write README: {}", e));
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["add", "README.md"])
            .status()
            .unwrap_or_else(|e| panic!("failed to add README: {}", e))
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["commit", "-m", "initial commit"])
            .status()
            .unwrap_or_else(|e| panic!("failed to commit: {}", e))
            .success()
    );

    // Get current branch name
    let current_branch = String::from_utf8(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap_or_else(|e| panic!("failed to read current branch: {}", e))
            .stdout,
    )
    .unwrap_or_else(|e| panic!("branch name not utf8: {}", e))
    .trim()
    .to_string();

    // Add remote and push initial branch
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["remote", "add", "origin", remote_dir.to_str().unwrap()])
            .status()
            .unwrap_or_else(|e| panic!("failed to add origin remote: {}", e))
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args([
                "push",
                "origin",
                &format!("HEAD:refs/heads/{}", current_branch),
            ])
            .status()
            .unwrap_or_else(|e| panic!("failed to push to remote: {}", e))
            .success()
    );

    // Create and push a branch
    let branch_name = "stale_branch";
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["checkout", "-b", branch_name])
            .status()
            .unwrap_or_else(|e| panic!("failed to create branch: {}", e))
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["push", "origin", branch_name])
            .status()
            .unwrap_or_else(|e| panic!("failed to push branch: {}", e))
            .success()
    );

    // Initialize a fresh Libra repository to fetch into
    let repo_dir = temp_root.path().join("libra_repo");
    fs::create_dir_all(&repo_dir).unwrap_or_else(|e| panic!("failed to create repo dir: {}", e));
    test::setup_with_new_libra_in(&repo_dir).await;
    let _guard = test::ChangeDirGuard::new(&repo_dir);

    let remote_path = remote_dir.to_str().unwrap().to_string();
    ConfigKv::set("remote.origin.url", &remote_path, false)
        .await
        .unwrap();

    // Fetch to create remote-tracking branch.
    let quiet_output = OutputConfig::resolve(None, false, true, "auto", true, false, "none");
    fetch::fetch_repository_safe(
        RemoteConfig {
            name: "origin".to_string(),
            url: remote_path.clone(),
        },
        None,
        false,
        None,
        None,
        &quiet_output,
    )
    .await
    .expect("initial prune fixture fetch should succeed");

    // Verify remote-tracking branch exists
    let tracked_branch = format!("refs/remotes/origin/{}", branch_name);
    assert!(
        Branch::find_branch_result(&tracked_branch, Some("origin"))
            .await
            .expect("failed to query remote-tracking branch")
            .is_some(),
        "remote-tracking branch should exist after fetch"
    );

    // Delete branch from remote
    assert!(
        Command::new("git")
            .current_dir(remote_dir.to_str().unwrap())
            .args(["update-ref", "-d", &format!("refs/heads/{}", branch_name)])
            .status()
            .unwrap_or_else(|e| panic!("failed to delete branch {}: {}", branch_name, e))
            .success()
    );

    // Run prune with --dry-run
    remote::execute(RemoteCmds::Prune {
        name: "origin".into(),
        dry_run: true,
    })
    .await;

    // Verify branch still exists (dry-run should not delete)
    assert!(
        Branch::find_branch_result(&tracked_branch, Some("origin"))
            .await
            .expect("failed to query remote-tracking branch")
            .is_some(),
        "remote-tracking branch should still exist after dry-run prune"
    );

    // Now run actual prune
    remote::execute(RemoteCmds::Prune {
        name: "origin".into(),
        dry_run: false,
    })
    .await;

    // Verify branch is now deleted
    assert!(
        Branch::find_branch_result(&tracked_branch, Some("origin"))
            .await
            .expect("failed to query remote-tracking branch")
            .is_none(),
        "remote-tracking branch should be pruned after actual prune"
    );
}

#[test]
fn test_remote_add_duplicate_name_returns_conflict_error_code() {
    let repo = tempdir().expect("failed to create repo");
    init_repo_via_cli(repo.path());

    let first = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );
    assert_cli_success(&first, "initial remote add");

    let duplicate = run_libra_command(
        &["remote", "add", "origin", "https://example.com/other.git"],
        repo.path(),
    );
    let (_stderr, report) = parse_cli_error_stderr(&duplicate.stderr);
    assert_eq!(duplicate.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert_eq!(report.message, "remote 'origin' already exists");
}

#[cfg(unix)]
#[tokio::test]
#[serial(cwd)]
async fn test_remote_prune_does_not_report_success_when_delete_fails() {
    if skip_permission_denied_test_if_root(
        "test_remote_prune_does_not_report_success_when_delete_fails",
    ) {
        return;
    }

    let temp_root = tempdir().unwrap();
    let remote_dir = temp_root.path().join("remote.git");
    let work_dir = temp_root.path().join("workdir");
    let repo_dir = temp_root.path().join("libra_repo");

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["init", work_dir.to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["config", "user.name", "Libra Tester"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["config", "user.email", "tester@example.com"])
            .status()
            .unwrap()
            .success()
    );
    fs::write(work_dir.join("README.md"), "hello libra").unwrap();
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["add", "README.md"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["commit", "-m", "initial commit"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["remote", "add", "origin", remote_dir.to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["checkout", "-b", "stale_branch"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["push", "origin", "stale_branch"])
            .status()
            .unwrap()
            .success()
    );
    fs::create_dir_all(&repo_dir).unwrap();
    init_repo_via_cli(&repo_dir);

    let remote_path = remote_dir.to_str().unwrap().to_string();
    let add_remote = run_libra_command(&["remote", "add", "origin", &remote_path], &repo_dir);
    assert_cli_success(&add_remote, "remote add origin");

    let fetch_output = run_libra_command(&["fetch", "origin"], &repo_dir);
    assert_cli_success(&fetch_output, "fetch origin");

    let tracked_branch = "refs/remotes/origin/stale_branch";
    {
        let _guard = test::ChangeDirGuard::new(&repo_dir);
        assert!(
            Branch::find_branch_result(tracked_branch, Some("origin"))
                .await
                .expect("failed to query remote-tracking branch")
                .is_some(),
            "expected stale remote-tracking branch to exist before prune"
        );
    }

    assert!(
        Command::new("git")
            .current_dir(remote_dir.to_str().unwrap())
            .args(["update-ref", "-d", "refs/heads/stale_branch"])
            .status()
            .unwrap()
            .success()
    );

    let db_path = repo_dir.join(".libra").join("libra.db");
    let original_mode = fs::metadata(&db_path).unwrap().permissions().mode();
    fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let output = run_libra_command(&["remote", "prune", "origin"], &repo_dir);
    fs::set_permissions(&db_path, std::fs::Permissions::from_mode(original_mode)).unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-IO-002");
    assert!(
        !stdout.contains("[pruned] origin/stale_branch"),
        "prune should not report success when deletion fails: {stdout}"
    );
    assert!(
        stderr.contains("failed to prune remote-tracking branch"),
        "unexpected stderr: {stderr}"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_set_url_delete_no_match_returns_error() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    remote::execute_safe(
        RemoteCmds::Add {
            name: "origin".into(),
            url: "https://example.com/repo.git".into(),
            fetch: false,
            track: vec![],
            master: None,
            tags: false,
            no_tags: false,
            mirror: false,
        },
        &OutputConfig::default(),
    )
    .await
    .expect("add should succeed");

    let result = remote::execute_safe(
        RemoteCmds::SetUrl {
            add: false,
            delete: true,
            push: false,
            all: false,
            name: "origin".into(),
            value: "nonexistent-pattern".into(),
        },
        &OutputConfig::default(),
    )
    .await;

    assert!(result.is_err(), "delete with no matching URL should fail");
    let err = result.unwrap_err();
    assert_eq!(err.stable_code(), StableErrorCode::CliInvalidTarget);
    assert!(
        err.render().contains("no matching fetch URL"),
        "unexpected error: {}",
        err.render()
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_prune_nonexistent_remote_returns_structured_error() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    let result = remote::execute_safe(
        RemoteCmds::Prune {
            name: "nonexistent".into(),
            dry_run: false,
        },
        &OutputConfig::default(),
    )
    .await;

    assert!(result.is_err(), "prune nonexistent remote should fail");
    let err = result.unwrap_err();
    assert_eq!(err.stable_code(), StableErrorCode::CliInvalidTarget);
    assert!(
        err.render().contains("no such remote"),
        "unexpected error: {}",
        err.render()
    );
}

#[test]
fn test_remote_rename_json_output_is_structured() {
    let repo = tempdir().expect("failed to create repo");
    init_repo_via_cli(repo.path());

    run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );

    let output = run_libra_command(
        &["--json", "remote", "rename", "origin", "upstream"],
        repo.path(),
    );
    assert_cli_success(&output, "remote rename --json");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "remote");
    assert_eq!(json["data"]["action"], "rename");
    assert_eq!(json["data"]["old_name"], "origin");
    assert_eq!(json["data"]["new_name"], "upstream");
}

#[test]
fn test_remote_set_url_delete_no_match_returns_error_code_cli() {
    let repo = tempdir().expect("failed to create repo");
    init_repo_via_cli(repo.path());

    run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );

    let output = run_libra_command(
        &[
            "remote",
            "set-url",
            "--delete",
            "origin",
            "nonexistent-pattern",
        ],
        repo.path(),
    );

    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    // CliInvalidTarget (LBR-CLI-003) maps to Cli category → exit code 129 (usage)
    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        report.message.contains("no matching fetch URL"),
        "unexpected message: {}",
        report.message
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_remove_works_after_deleting_last_url() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    // Add a remote, then add a pushurl key
    remote::execute_safe(
        RemoteCmds::Add {
            name: "origin".into(),
            url: "https://example.com/repo.git".into(),
            fetch: false,
            track: vec![],
            master: None,
            tags: false,
            no_tags: false,
            mirror: false,
        },
        &OutputConfig::default(),
    )
    .await
    .unwrap();

    ConfigKv::set(
        "remote.origin.pushurl",
        "ssh://git@example.com/repo.git",
        false,
    )
    .await
    .unwrap();

    // Delete the fetch URL
    remote::execute_safe(
        RemoteCmds::SetUrl {
            add: false,
            delete: true,
            push: false,
            all: false,
            name: "origin".into(),
            value: "example.com".into(),
        },
        &OutputConfig::default(),
    )
    .await
    .unwrap();

    // The remote should still be removable even though url is gone
    let result = remote::execute_safe(
        RemoteCmds::Remove {
            name: "origin".into(),
        },
        &OutputConfig::default(),
    )
    .await;

    assert!(
        result.is_ok(),
        "remove should succeed when remote has pushurl but no url: {:?}",
        result.err()
    );
}

/// `libra remote --help` surfaces the EXAMPLES banner so users see the
/// most common invocation per sub-command (`add`, `remove`, `rename`,
/// `-v`, `get-url --all`, `set-url --push`, `prune --dry-run`, `--json`)
/// without reading the design doc. Cross-cutting `--help` EXAMPLES
/// rollout per `docs/development/commands/_general.md` item B.
#[test]
fn test_remote_help_lists_examples_banner() {
    let repo = tempdir().expect("tempdir for remote --help");
    let output = run_libra_command(&["remote", "--help"], repo.path());
    assert!(
        output.status.success(),
        "remote --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "remote --help should include EXAMPLES banner, stdout: {stdout}"
    );
    for invocation in [
        "libra remote -v",
        "libra remote add origin",
        "libra remote rename origin upstream",
        "libra remote remove upstream",
        "libra remote get-url --all origin",
        "libra remote set-url --push origin",
        "libra remote prune --dry-run origin",
        "libra remote --json -v",
    ] {
        assert!(
            stdout.contains(invocation),
            "remote --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}

// ── show <name> (detailed, offline) ───────────────────────────────────────

#[tokio::test]
async fn test_remote_show_no_args_lists_remotes() {
    let repo = tempdir().expect("failed to create repo");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["remote", "show"], repo.path());
    assert_cli_success(&output, "remote show (empty repo)");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "expected empty output when no remotes: got '{stdout}'"
    );

    let add = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );
    assert_cli_success(&add, "remote add origin");

    let output = run_libra_command(&["remote", "show"], repo.path());
    assert_cli_success(&output, "remote show (with origin)");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim() == "origin",
        "expected 'origin', got '{stdout}'"
    );

    let add2 = run_libra_command(
        &[
            "remote",
            "add",
            "upstream",
            "https://upstream.example/repo.git",
        ],
        repo.path(),
    );
    assert_cli_success(&add2, "remote add upstream");

    let output = run_libra_command(&["remote", "show"], repo.path());
    assert_cli_success(&output, "remote show (two remotes)");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 2, "expected 2 remotes, got: {lines:?}");
    assert!(
        lines.contains(&"origin"),
        "expected 'origin' in output: {lines:?}"
    );
    assert!(
        lines.contains(&"upstream"),
        "expected 'upstream' in output: {lines:?}"
    );
}

#[tokio::test]
async fn test_remote_show_detail_json_output_is_structured() {
    let repo = tempdir().expect("failed to create repo");
    init_repo_via_cli(repo.path());

    let add = run_libra_command(
        &["remote", "add", "origin", "https://one.example/repo.git"],
        repo.path(),
    );
    assert_cli_success(&add, "remote add origin");

    let add_push = run_libra_command(
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "ssh://git@example.com/repo.git",
        ],
        repo.path(),
    );
    assert_cli_success(&add_push, "remote set-url --push origin");

    let output = run_libra_command(
        &["--json", "remote", "show", "--no-query", "origin"],
        repo.path(),
    );
    assert_cli_success(&output, "remote show --no-query origin --json");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "remote");
    assert_eq!(json["data"]["action"], "show");
    assert_eq!(json["data"]["name"], "origin");
    assert_eq!(
        json["data"]["fetch_urls"],
        serde_json::json!(["https://one.example/repo.git"])
    );
    assert_eq!(
        json["data"]["push_urls"],
        serde_json::json!(["ssh://git@example.com/repo.git"])
    );
    assert!(json["data"]["head_branch"].is_null());
    assert!(json["data"]["remote_branches"].is_array());
    assert!(json["data"]["pull_config"].is_array());
    assert!(json["data"]["push_config"].is_array());
    assert_eq!(json["data"]["queried"], false);
}

#[tokio::test]
async fn test_remote_show_detail_human_output() {
    let repo = tempdir().expect("failed to create repo");
    init_repo_via_cli(repo.path());

    run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );

    let output = run_libra_command(&["remote", "show", "--no-query", "origin"], repo.path());
    assert_cli_success(&output, "remote show --no-query origin");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("* remote origin"),
        "expected '* remote origin' header: {stdout}"
    );
    assert!(
        stdout.contains("Fetch URL:"),
        "expected Fetch URL line: {stdout}"
    );
    assert!(
        stdout.contains("https://example.com/repo.git"),
        "expected URL in output: {stdout}"
    );
}

#[tokio::test]
async fn test_remote_show_nonexistent_remote_returns_error() {
    let repo = tempdir().expect("failed to create repo");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["remote", "show", "nonexistent"], repo.path());
    assert!(
        !output.status.success(),
        "expected failure for nonexistent remote"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nonexistent"),
        "expected error mentioning remote name: {stderr}"
    );
}

#[tokio::test]
async fn test_remote_show_detail_json_with_pushurl_fallback() {
    let repo = tempdir().expect("failed to create repo");
    init_repo_via_cli(repo.path());

    run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );

    let output = run_libra_command(
        &["--json", "remote", "show", "--no-query", "origin"],
        repo.path(),
    );
    assert_cli_success(&output, "remote show --no-query origin --json");

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["action"], "show");
    assert_eq!(json["data"]["push_urls"], json["data"]["fetch_urls"]);
}

#[tokio::test]
async fn test_remote_show_detail_redacts_credentials() {
    let repo = tempdir().expect("failed to create repo");
    init_repo_via_cli(repo.path());

    run_libra_command(
        &[
            "remote",
            "add",
            "origin",
            "https://user:pass@example.com/repo.git",
        ],
        repo.path(),
    );

    let output = run_libra_command(&["remote", "show", "--no-query", "origin"], repo.path());
    assert_cli_success(&output, "remote show --no-query origin");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("user:pass"),
        "human output leaked credentials: {stdout}"
    );
    assert!(
        !stdout.contains("@example.com"),
        "human output should have redacted userinfo from URL: {stdout}"
    );

    let json_output = run_libra_command(
        &["--json", "remote", "show", "--no-query", "origin"],
        repo.path(),
    );
    assert_cli_success(&json_output, "remote show --no-query origin --json");
    let json = parse_json_stdout(&json_output);
    let fetch_url = json["data"]["fetch_urls"][0].as_str().unwrap();
    assert!(
        !fetch_url.contains("user:pass"),
        "JSON output leaked credentials: {fetch_url}"
    );
    assert!(
        !fetch_url.contains('@'),
        "JSON output should strip userinfo entirely: {fetch_url}"
    );
}

// ── set-branches / set-head ───────────────────────────────────────────────

async fn add_origin() {
    remote::execute(RemoteCmds::Add {
        name: "origin".into(),
        url: "https://example.com/repo.git".into(),
        fetch: false,
        track: vec![],
        master: None,
        tags: false,
        no_tags: false,
        mirror: false,
    })
    .await;
}

async fn fetch_refspecs() -> Vec<String> {
    ConfigKv::get_all("remote.origin.fetch")
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.value)
        .collect()
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_set_branches_overwrites() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    add_origin().await;

    remote::execute_safe(
        RemoteCmds::SetBranches {
            add: false,
            name: "origin".into(),
            branches: vec!["main".into()],
        },
        &OutputConfig::default(),
    )
    .await
    .expect("set-branches main");

    assert_eq!(
        fetch_refspecs().await,
        vec!["+refs/heads/main:refs/remotes/origin/main".to_string()]
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_set_branches_add_appends() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    add_origin().await;

    let out = OutputConfig::default();
    remote::execute_safe(
        RemoteCmds::SetBranches {
            add: false,
            name: "origin".into(),
            branches: vec!["main".into()],
        },
        &out,
    )
    .await
    .expect("set-branches main");
    remote::execute_safe(
        RemoteCmds::SetBranches {
            add: true,
            name: "origin".into(),
            branches: vec!["dev".into()],
        },
        &out,
    )
    .await
    .expect("set-branches --add dev");

    let specs = fetch_refspecs().await;
    assert_eq!(specs.len(), 2, "specs: {specs:?}");
    assert!(specs.contains(&"+refs/heads/main:refs/remotes/origin/main".to_string()));
    assert!(specs.contains(&"+refs/heads/dev:refs/remotes/origin/dev".to_string()));
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_set_branches_unknown_remote_errors() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    let error = remote::execute_safe(
        RemoteCmds::SetBranches {
            add: false,
            name: "ghost".into(),
            branches: vec!["main".into()],
        },
        &OutputConfig::default(),
    )
    .await
    .expect_err("unknown remote should error");
    assert_eq!(error.stable_code(), StableErrorCode::CliInvalidTarget);
    assert_eq!(error.exit_code(), 129);
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_set_branches_invalid_branch_rejected() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    add_origin().await;

    let error = remote::execute_safe(
        RemoteCmds::SetBranches {
            add: false,
            name: "origin".into(),
            branches: vec!["a..b".into()],
        },
        &OutputConfig::default(),
    )
    .await
    .expect_err("invalid branch name should error");
    assert_eq!(error.exit_code(), 129);
    assert!(error.message().contains("invalid branch name"));
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_set_head_delete_idempotent() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    add_origin().await;

    // Deleting a non-existent remote HEAD is a successful no-op, twice.
    for _ in 0..2 {
        remote::execute_safe(
            RemoteCmds::SetHead {
                auto: false,
                delete: true,
                name: "origin".into(),
                branch: None,
            },
            &OutputConfig::default(),
        )
        .await
        .expect("set-head -d should be idempotent");
    }
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_set_head_missing_branch_errors() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    add_origin().await;

    let error = remote::execute_safe(
        RemoteCmds::SetHead {
            auto: false,
            delete: false,
            name: "origin".into(),
            branch: Some("nope".into()),
        },
        &OutputConfig::default(),
    )
    .await
    .expect_err("missing tracking branch should error");
    assert_eq!(error.stable_code(), StableErrorCode::CliInvalidTarget);
    assert_eq!(error.exit_code(), 129);
    assert!(error.message().contains("no such remote-tracking branch"));
}

/// Run `git` with `args` in `dir`, panicking on failure.
fn git_in(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"))
            .success(),
        "git {args:?} failed in {}",
        dir.display()
    );
}

/// Build a bare git remote at `<root>/remote.git` seeded with a `main` branch
/// (its own commit) plus each name in `extra_branches` (each on a *distinct*
/// commit so HEAD's OID is unique to `main`). The remote HEAD symref is pinned
/// to `main`. Returns the bare remote path. Reused by the online `remote show`
/// and `set-head --auto` tests.
fn setup_bare_git_remote(root: &Path, extra_branches: &[&str]) -> PathBuf {
    let remote_dir = root.join("remote.git");
    let work_dir = root.join("workdir");
    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("init bare remote")
            .success()
    );
    assert!(
        Command::new("git")
            .args(["init", work_dir.to_str().unwrap()])
            .status()
            .expect("init workdir")
            .success()
    );
    git_in(&work_dir, &["config", "user.name", "Libra Tester"]);
    git_in(&work_dir, &["config", "user.email", "tester@example.com"]);
    fs::write(work_dir.join("README.md"), "hello libra").expect("write README");
    git_in(&work_dir, &["add", "README.md"]);
    git_in(&work_dir, &["commit", "-m", "initial commit"]);
    // Normalise the default branch to `main` regardless of git's init default.
    git_in(&work_dir, &["branch", "-M", "main"]);
    git_in(
        &work_dir,
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
    );
    git_in(&work_dir, &["push", "origin", "main"]);
    for branch in extra_branches {
        git_in(&work_dir, &["checkout", "main"]);
        git_in(&work_dir, &["checkout", "-b", branch]);
        fs::write(work_dir.join(format!("{branch}.txt")), *branch).expect("write branch file");
        git_in(&work_dir, &["add", "."]);
        git_in(&work_dir, &["commit", "-m", branch]);
        git_in(&work_dir, &["push", "origin", branch]);
    }
    // Pin the remote HEAD to main so discovery resolves the default branch.
    git_in(&remote_dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    remote_dir
}

/// Online `remote show` (default) contacts the remote and classifies branches:
/// `tracked` (on both), `new` (remote-only, not yet fetched), `stale`
/// (locally tracked, gone from the remote). It also reports the live HEAD.
#[tokio::test]
#[serial(env)]
async fn test_remote_show_online_classifies_tracked_new_stale() {
    let temp_root = tempdir().unwrap();
    let remote_dir = setup_bare_git_remote(temp_root.path(), &["feature1"]);

    let repo = temp_root.path().join("libra_repo");
    fs::create_dir_all(&repo).unwrap();
    init_repo_via_cli(&repo);
    assert_cli_success(
        &run_libra_command(
            &["remote", "add", "origin", remote_dir.to_str().unwrap()],
            &repo,
        ),
        "remote add origin",
    );
    assert_cli_success(
        &run_libra_command(&["fetch", "origin"], &repo),
        "fetch origin",
    );

    // Mutate the remote so the three classes are all exercised:
    //  - feature2 is created remote-only (from feature1's commit) -> `new`,
    //  - feature1 is deleted from the remote -> `stale` locally,
    //  - main stays on both -> `tracked`.
    git_in(&remote_dir, &["branch", "feature2", "feature1"]);
    git_in(&remote_dir, &["update-ref", "-d", "refs/heads/feature1"]);

    let output = run_libra_command(&["--json", "remote", "show", "origin"], &repo);
    assert_cli_success(&output, "remote show origin (online)");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["queried"], true);
    assert_eq!(json["data"]["head_branch"], "main");

    let mut status = std::collections::HashMap::new();
    for branch in json["data"]["remote_branches"].as_array().unwrap() {
        status.insert(
            branch["branch"].as_str().unwrap().to_string(),
            branch["status"].as_str().unwrap().to_string(),
        );
    }
    assert_eq!(status.get("main").map(String::as_str), Some("tracked"));
    assert_eq!(status.get("feature2").map(String::as_str), Some("new"));
    assert_eq!(status.get("feature1").map(String::as_str), Some("stale"));
}

/// `--no-query` keeps `remote show` fully offline: no network contact, branches
/// reported with the `cached` status and `queried = false`. (Pins the offline
/// path against the new online default.)
#[tokio::test]
#[serial(env)]
async fn test_remote_show_no_query_stays_offline() {
    let temp_root = tempdir().unwrap();
    let remote_dir = setup_bare_git_remote(temp_root.path(), &["feature1"]);

    let repo = temp_root.path().join("libra_repo");
    fs::create_dir_all(&repo).unwrap();
    init_repo_via_cli(&repo);
    run_libra_command(
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
        &repo,
    );
    assert_cli_success(
        &run_libra_command(&["fetch", "origin"], &repo),
        "fetch origin",
    );

    // Delete a branch from the remote; --no-query must NOT notice (still cached).
    git_in(&remote_dir, &["update-ref", "-d", "refs/heads/feature1"]);

    let output = run_libra_command(&["--json", "remote", "show", "--no-query", "origin"], &repo);
    assert_cli_success(&output, "remote show --no-query origin");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["queried"], false);
    for branch in json["data"]["remote_branches"].as_array().unwrap() {
        assert_eq!(branch["status"], "cached");
    }
}

/// Online `remote show` against an unreachable remote fails with a hint to use
/// `--no-query`.
#[tokio::test]
async fn test_remote_show_online_unreachable_hints_no_query() {
    let temp_root = tempdir().unwrap();
    let repo = temp_root.path().join("libra_repo");
    fs::create_dir_all(&repo).unwrap();
    init_repo_via_cli(&repo);
    let missing = temp_root.path().join("nonexistent.git");
    run_libra_command(
        &["remote", "add", "origin", missing.to_str().unwrap()],
        &repo,
    );

    let output = run_libra_command(&["remote", "show", "origin"], &repo);
    assert!(
        !output.status.success(),
        "online show against an unreachable remote must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--no-query"),
        "error should hint at --no-query: {stderr}"
    );
}

/// `remote set-head --auto` queries the remote, resolves its HEAD branch, and
/// writes the cached remote HEAD (provided that branch has been fetched).
#[tokio::test]
#[serial(env)]
async fn test_remote_set_head_auto_resolves_from_remote() {
    let temp_root = tempdir().unwrap();
    let remote_dir = setup_bare_git_remote(temp_root.path(), &[]);

    let repo = temp_root.path().join("libra_repo");
    fs::create_dir_all(&repo).unwrap();
    init_repo_via_cli(&repo);
    run_libra_command(
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
        &repo,
    );
    assert_cli_success(
        &run_libra_command(&["fetch", "origin"], &repo),
        "fetch origin",
    );

    let output = run_libra_command(&["--json", "remote", "set-head", "origin", "--auto"], &repo);
    assert_cli_success(&output, "remote set-head origin --auto");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["action"], "set-head");
    assert_eq!(json["data"]["mode"], "set");
    assert_eq!(json["data"]["target"], "main");

    // The cached remote HEAD now resolves to main.
    let show = run_libra_command(&["--json", "remote", "show", "--no-query", "origin"], &repo);
    assert_cli_success(&show, "remote show --no-query origin");
    let show_json = parse_json_stdout(&show);
    assert_eq!(show_json["data"]["head_branch"], "main");
}

/// `set-head --auto` resolves the remote HEAD but fails if that branch has not
/// been fetched (no remote-tracking ref yet) — with the "fetch first" hint.
#[tokio::test]
#[serial(env)]
async fn test_remote_set_head_auto_requires_fetched_branch() {
    let temp_root = tempdir().unwrap();
    let remote_dir = setup_bare_git_remote(temp_root.path(), &[]);

    let repo = temp_root.path().join("libra_repo");
    fs::create_dir_all(&repo).unwrap();
    init_repo_via_cli(&repo);
    run_libra_command(
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
        &repo,
    );
    // Intentionally NOT fetching.

    let output = run_libra_command(&["remote", "set-head", "origin", "--auto"], &repo);
    assert!(
        !output.status.success(),
        "set-head --auto without a fetched branch must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no such remote-tracking branch"),
        "expected a tracking-branch error: {stderr}"
    );
}

#[test]
fn test_remote_set_branches_json_schema() {
    let repo = create_committed_repo_via_cli();
    let add = run_libra_command(
        &["remote", "add", "origin", "git@github.com:o/r.git"],
        repo.path(),
    );
    assert_cli_success(&add, "remote add origin");

    let output = run_libra_command(
        &["--json", "remote", "set-branches", "origin", "main"],
        repo.path(),
    );
    assert_cli_success(&output, "json set-branches");
    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "remote");
    assert_eq!(json["data"]["action"], "set-branches");
    assert_eq!(json["data"]["added"], false);
    let specs = json["data"]["fetch_refspecs"].as_array().expect("refspecs");
    assert_eq!(specs.len(), 1);
}

#[test]
fn remote_update_resolves_and_fetches_configured_remotes() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // No remotes configured: `remote update` succeeds with a notice.
    let none = run_libra_command(&["remote", "update"], p);
    assert_cli_success(&none, "remote update (no remotes)");
    assert!(
        String::from_utf8_lossy(&none.stdout).contains("No remotes to update"),
        "expected the empty notice: {}",
        String::from_utf8_lossy(&none.stdout)
    );

    // An unknown remote name is an error (ensure_remote_exists).
    let unknown = run_libra_command(&["remote", "update", "does-not-exist"], p);
    assert!(
        !unknown.status.success(),
        "updating an unknown remote must error"
    );

    // A configured but unreachable remote: `remote update` resolves it and
    // attempts the fetch, which fails — proving the resolved remote reaches the
    // fetch path. (Successful fetching is covered by the fetch command tests.)
    let bogus = p.join("nonexistent-remote.git");
    assert_cli_success(
        &run_libra_command(&["remote", "add", "origin", bogus.to_str().unwrap()], p),
        "remote add origin",
    );
    let attempt = run_libra_command(&["remote", "update"], p);
    assert!(
        !attempt.status.success(),
        "updating an unreachable remote must fail at the fetch step"
    );
}

#[test]
fn remote_add_fetch_flag_registers_then_attempts_fetch() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // `-f` registers the remote and then fetches. With an unreachable URL the
    // fetch fails fast, but the remote remains registered.
    let with_fetch =
        run_libra_command(&["remote", "add", "-f", "origin", "not-a-valid-url-xyz"], p);
    let listed = run_libra_command(&["remote", "-v"], p);
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("origin"),
        "remote is registered after `add -f` even when the fetch fails"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&with_fetch.stdout),
        String::from_utf8_lossy(&with_fetch.stderr)
    );
    assert!(
        combined.contains("not-a-valid-url-xyz") || combined.to_lowercase().contains("repository"),
        "`add -f` attempted a fetch from the new remote: {combined:?}"
    );

    // Without `-f`, `add` registers the remote with no fetch attempt (succeeds).
    let plain = run_libra_command(&["remote", "add", "other", "https://example.com/x.git"], p);
    assert_cli_success(&plain, "`remote add` without -f registers without fetching");
}

#[test]
fn remote_update_prune_flag_is_wired() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // `-p`/`--prune` parse for `remote update`. With no remotes configured the
    // command still succeeds with the empty notice (nothing to fetch or prune),
    // proving the flag is accepted and threaded without a clap conflict. The
    // prune-after-fetch path itself needs a reachable remote and is covered by
    // the L2 network/fetch tests.
    for variant in [["remote", "update", "-p"], ["remote", "update", "--prune"]] {
        let out = run_libra_command(&variant, p);
        assert_cli_success(&out, "remote update -p/--prune (no remotes)");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("No remotes to update"),
            "expected the empty notice for {variant:?}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    // A configured but unreachable remote with `-p`: still fails at the fetch
    // step (prune runs only after a successful fetch), proving `-p` does not
    // alter the fetch-failure behavior.
    let bogus = p.join("nonexistent-remote.git");
    assert_cli_success(
        &run_libra_command(&["remote", "add", "origin", bogus.to_str().unwrap()], p),
        "remote add origin",
    );
    let attempt = run_libra_command(&["remote", "update", "-p"], p);
    assert!(
        !attempt.status.success(),
        "remote update -p of an unreachable remote must still fail at the fetch step"
    );
}

/// End-to-end regression that `remote update -p` actually prunes: it fetches a
/// reachable local remote and then deletes the remote-tracking refs whose
/// upstream branch is gone. Mirrors `test_remote_prune_removes_stale_branches`
/// but drives the prune through `run_remote_update` (the `Update { prune }`
/// path) so the test cannot pass unless `update -p` reaches the prune logic and
/// deletes the stale refs. (The thin text/JSON renderer just iterates the same
/// pruned entries.)
#[tokio::test]
#[serial(cwd)]
async fn remote_update_prune_removes_stale_tracking_branches() {
    let temp_root = tempdir().unwrap();
    let remote_dir = temp_root.path().join("remote.git");
    let work_dir = temp_root.path().join("workdir");

    let git = |dir: &Path, args: &[&str]| {
        assert!(
            Command::new("git")
                .current_dir(dir)
                .args(args)
                .status()
                .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
                .success(),
            "git {args:?} should succeed"
        );
    };

    git(
        temp_root.path(),
        &["init", "--bare", remote_dir.to_str().unwrap()],
    );
    git(temp_root.path(), &["init", work_dir.to_str().unwrap()]);
    git(&work_dir, &["config", "user.name", "Libra Tester"]);
    git(&work_dir, &["config", "user.email", "tester@example.com"]);
    fs::write(work_dir.join("README.md"), "hello libra").unwrap();
    git(&work_dir, &["add", "README.md"]);
    git(&work_dir, &["commit", "-m", "initial commit"]);

    let current_branch = String::from_utf8(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("read current branch")
            .stdout,
    )
    .expect("branch name utf8")
    .trim()
    .to_string();

    git(
        &work_dir,
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
    );
    git(
        &work_dir,
        &[
            "push",
            "origin",
            &format!("HEAD:refs/heads/{current_branch}"),
        ],
    );
    let branches = ["feature1", "feature2", "feature3"];
    for b in &branches {
        git(&work_dir, &["checkout", "-b", b]);
        git(&work_dir, &["push", "origin", b]);
    }

    let repo_dir = temp_root.path().join("libra_repo");
    fs::create_dir_all(&repo_dir).unwrap();
    test::setup_with_new_libra_in(&repo_dir).await;
    let _guard = test::ChangeDirGuard::new(&repo_dir);

    let remote_path = remote_dir.to_str().unwrap().to_string();
    ConfigKv::set("remote.origin.url", &remote_path, false)
        .await
        .unwrap();
    // Default fetch refspec so `remote update` knows what to fetch.
    ConfigKv::set(
        "remote.origin.fetch",
        "+refs/heads/*:refs/remotes/origin/*",
        false,
    )
    .await
    .unwrap();

    let quiet_output = OutputConfig::resolve(None, false, true, "auto", true, false, "none");
    fetch::fetch_repository_safe(
        RemoteConfig {
            name: "origin".to_string(),
            url: remote_path.clone(),
        },
        None,
        false,
        None,
        None,
        &quiet_output,
    )
    .await
    .expect("initial remote update fixture fetch should succeed");

    for b in &branches {
        assert!(
            Branch::find_branch_result(&format!("refs/remotes/origin/{b}"), Some("origin"))
                .await
                .expect("query tracking branch")
                .is_some(),
            "remote-tracking branch origin/{b} should exist after fetch"
        );
    }

    // Delete two branches on the remote so their tracking refs become stale.
    for b in ["feature1", "feature3"] {
        git(
            &remote_dir,
            &["update-ref", "-d", &format!("refs/heads/{b}")],
        );
    }

    // `remote update -p`: fetch then prune. Drives the full Update { prune }
    // path, not the standalone `prune` subcommand.
    remote::execute_safe(
        RemoteCmds::Update {
            groups: vec![],
            prune: true,
        },
        &quiet_output,
    )
    .await
    .expect("remote update -p should fetch and prune");

    for b in ["feature1", "feature3"] {
        assert!(
            Branch::find_branch_result(&format!("refs/remotes/origin/{b}"), Some("origin"))
                .await
                .expect("query tracking branch")
                .is_none(),
            "stale tracking branch origin/{b} should be pruned by `update -p`"
        );
    }
    assert!(
        Branch::find_branch_result("refs/remotes/origin/feature2", Some("origin"))
            .await
            .expect("query tracking branch")
            .is_some(),
        "non-stale tracking branch origin/feature2 must survive `update -p`"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_remote_add_mirror_writes_marker_and_conflicts_with_track() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    let p = repo_dir.path();

    // `--mirror` records the informational marker `remote.<name>.mirror=true`.
    assert_cli_success(
        &run_libra_command(
            &[
                "remote",
                "add",
                "--mirror",
                "backup",
                "https://example.com/r.git",
            ],
            p,
        ),
        "remote add --mirror",
    );
    let marker = ConfigKv::get("remote.backup.mirror")
        .await
        .expect("read mirror marker")
        .map(|e| e.value);
    assert_eq!(marker.as_deref(), Some("true"), "mirror marker is written");

    // NARROWING: no `+refs/*:refs/*` fetch refspec is written (fetch is not
    // mirror-aware), matching `clone --mirror`.
    let fetch = ConfigKv::get_all("remote.backup.fetch")
        .await
        .expect("read fetch")
        .into_iter()
        .map(|e| e.value)
        .collect::<Vec<_>>();
    assert!(
        fetch.is_empty(),
        "remote add --mirror writes no fetch refspec: {fetch:?}"
    );

    // `--mirror` is incompatible with `-t`/`--track` (clap conflict → usage error).
    let conflict = run_libra_command(
        &[
            "remote",
            "add",
            "--mirror",
            "-t",
            "main",
            "m2",
            "https://example.com/r.git",
        ],
        p,
    );
    assert!(
        !conflict.status.success(),
        "--mirror with -t must be rejected: {}",
        String::from_utf8_lossy(&conflict.stderr)
    );
}
