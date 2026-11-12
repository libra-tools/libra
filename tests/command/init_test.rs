//! Integration tests for the `init` command core behavior.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use libra::internal::{config::ConfigKv, db::get_db_conn_instance_for_path, model::config};
use pgp::composed::{Deserializable, SignedPublicKey};
use sea_orm::EntityTrait;
use tempfile::tempdir;

use super::{assert_cli_success, run_libra_command};

#[path = "init_home_test.rs"]
mod home;

async fn open_repo_conn(repo: &std::path::Path, bare: bool) -> sea_orm::DatabaseConnection {
    let db_path = if bare {
        repo.join("libra.db")
    } else {
        repo.join(".libra").join("libra.db")
    };
    get_db_conn_instance_for_path(&db_path)
        .await
        .expect("failed to open repository database")
}

async fn config_value(conn: &sea_orm::DatabaseConnection, key: &str) -> Option<String> {
    ConfigKv::get_with_conn(conn, key)
        .await
        .expect("failed to query config_kv")
        .map(|entry| entry.value)
}

fn public_key_user_ids(public_key: &str) -> Vec<String> {
    let (signed_key, _headers) =
        SignedPublicKey::from_string(public_key).expect("failed to parse armored public key");
    signed_key
        .details
        .users
        .into_iter()
        .map(|user| {
            user.id
                .as_str()
                .expect("public key user id should be valid UTF-8")
                .to_string()
        })
        .collect()
}

fn run_libra_command_with_env(args: &[&str], cwd: &Path, envs: &[(&str, &str)]) -> Output {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    fs::create_dir_all(&config_home).expect("failed to create isolated config directory");

    let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("LIBRA_TEST", "1");

    for (key, value) in envs {
        command.env(key, value);
    }

    command
        .output()
        .expect("failed to execute libra command with extra env")
}

#[tokio::test]
async fn init_vault_false_writes_seed_keys_and_human_summary() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    let output = run_libra_command(&["init", "--vault", "false"], &repo);
    assert_cli_success(&output, "init --vault false");
    assert!(
        !repo.join(".libraignore").exists(),
        "WT-03: non-bare init must not create a default .libraignore"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("Initialized empty Libra repository in"),
        "expected past-tense success summary, got: {stdout}"
    );
    assert!(
        stderr.contains("Creating repository layout ..."),
        "expected human progress on stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("Initializing database ..."),
        "expected database progress on stderr, got: {stderr}"
    );

    let conn = open_repo_conn(&repo, false).await;
    assert_eq!(
        config_value(&conn, "core.repositoryformatversion")
            .await
            .as_deref(),
        Some("0")
    );
    assert_eq!(
        config_value(&conn, "core.filemode").await.as_deref(),
        Some(if cfg!(windows) { "false" } else { "true" })
    );
    assert_eq!(
        config_value(&conn, "core.bare").await.as_deref(),
        Some("false")
    );
    assert_eq!(
        config_value(&conn, "core.logallrefupdates")
            .await
            .as_deref(),
        Some("true")
    );
    assert_eq!(
        config_value(&conn, "core.objectformat").await.as_deref(),
        Some("sha1")
    );
    assert_eq!(
        config_value(&conn, "core.initrefformat").await.as_deref(),
        Some("strict")
    );
    assert_eq!(
        config_value(&conn, "vault.signing").await.as_deref(),
        Some("false")
    );

    let repo_id = config_value(&conn, "libra.repoid")
        .await
        .expect("libra.repoid should exist");
    uuid::Uuid::parse_str(&repo_id).expect("libra.repoid should be a valid UUID");

    let legacy_rows = config::Entity::find()
        .all(&conn)
        .await
        .expect("failed to inspect legacy config table");
    assert!(
        legacy_rows.is_empty(),
        "init should not seed the legacy config table"
    );
}

#[test]
fn init_leaves_status_clean_without_default_libraignore() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    let output = run_libra_command(&["init", "--vault", "false"], &repo);
    assert_cli_success(&output, "init --vault false");

    assert!(
        !repo.join(".libraignore").exists(),
        "WT-03: init must not create a default .libraignore"
    );
    let status = run_libra_command(&["status", "--porcelain"], &repo);
    assert_cli_success(&status, "status --porcelain");
    assert!(
        status.stdout.is_empty(),
        "WT-03: a fresh repository must be clean, got: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn init_preserves_existing_root_libraignore() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join(".libraignore"), "custom-cache/\n").unwrap();

    let output = run_libra_command(&["init", "--vault", "false"], &repo);
    assert_cli_success(&output, "init --vault false");

    let content = fs::read_to_string(repo.join(".libraignore")).unwrap();
    assert_eq!(
        content, "custom-cache/\n",
        "init must not overwrite a user-provided .libraignore"
    );
}

#[test]
fn init_bare_does_not_create_root_libraignore() {
    let temp = tempdir().unwrap();
    let bare_repo = temp.path().join("repo.git");

    let output = run_libra_command(
        &[
            "init",
            "--bare",
            "--vault",
            "false",
            bare_repo.to_str().unwrap(),
        ],
        temp.path(),
    );
    assert_cli_success(&output, "bare init");

    assert!(
        !bare_repo.join(".libraignore").exists(),
        "bare init should not create a worktree .libraignore"
    );
}

#[tokio::test]
async fn init_vault_true_records_signing_state_and_uses_global_identity_fallback() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    let set_name = run_libra_command(&["config", "--global", "user.name", "Global Name"], &repo);
    assert_cli_success(&set_name, "set global user.name");
    let set_email = run_libra_command(
        &["config", "--global", "user.email", "global@example.com"],
        &repo,
    );
    assert_cli_success(&set_email, "set global user.email");

    let output = run_libra_command(&["init"], &repo);
    assert_cli_success(&output, "init with global identity fallback");

    let conn = open_repo_conn(&repo, false).await;
    assert_eq!(
        config_value(&conn, "vault.signing").await.as_deref(),
        Some("true")
    );

    let pubkey = config_value(&conn, "vault.gpg.pubkey")
        .await
        .expect("vault.gpg.pubkey should exist after init");
    let user_ids = public_key_user_ids(&pubkey);
    assert!(
        user_ids
            .iter()
            .any(|user_id| user_id == "Global Name <global@example.com>"),
        "expected PGP public key to use global identity, got user IDs: {user_ids:?}"
    );
}

#[tokio::test]
async fn init_vault_true_uses_env_identity_fallback_when_config_is_missing() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    let output = run_libra_command_with_env(
        &["init"],
        &repo,
        &[
            ("GIT_COMMITTER_NAME", "Env Committer"),
            ("EMAIL", "env@example.com"),
        ],
    );
    assert_cli_success(&output, "init with env identity fallback");

    let conn = open_repo_conn(&repo, false).await;
    let pubkey = config_value(&conn, "vault.gpg.pubkey")
        .await
        .expect("vault.gpg.pubkey should exist after init");
    let user_ids = public_key_user_ids(&pubkey);
    assert!(
        user_ids
            .iter()
            .any(|user_id| user_id == "Env Committer <env@example.com>"),
        "expected PGP public key to use env fallback identity, got user IDs: {user_ids:?}"
    );
}

#[tokio::test]
async fn init_target_repo_does_not_inherit_local_identity_from_current_repo() {
    let temp = tempdir().unwrap();
    let repo_a = temp.path().join("repo-a");
    let repo_b = temp.path().join("repo-b");
    fs::create_dir_all(&repo_a).unwrap();

    let init_a = run_libra_command(&["init", "--vault", "false"], &repo_a);
    assert_cli_success(&init_a, "init repo-a");

    let set_name = run_libra_command(&["config", "user.name", "Repo A Name"], &repo_a);
    assert_cli_success(&set_name, "set repo-a local user.name");
    let set_email = run_libra_command(&["config", "user.email", "repo-a@example.com"], &repo_a);
    assert_cli_success(&set_email, "set repo-a local user.email");

    let init_b = run_libra_command_with_env(
        &["init", "../repo-b"],
        &repo_a,
        &[
            ("GIT_COMMITTER_NAME", "Repo B Env"),
            ("EMAIL", "repo-b@example.com"),
        ],
    );
    assert_cli_success(&init_b, "init repo-b from inside repo-a");

    let conn_b = open_repo_conn(&repo_b, false).await;
    let pubkey_b = config_value(&conn_b, "vault.gpg.pubkey")
        .await
        .expect("vault.gpg.pubkey should exist in repo-b");
    let user_ids = public_key_user_ids(&pubkey_b);
    assert!(
        user_ids
            .iter()
            .any(|user_id| user_id == "Repo B Env <repo-b@example.com>"),
        "repo-b should use env/global/default fallback for its own target, got user IDs: {user_ids:?}"
    );
    assert!(
        user_ids
            .iter()
            .all(|user_id| user_id != "Repo A Name <repo-a@example.com>"),
        "repo-b should not inherit repo-a local identity, got user IDs: {user_ids:?}"
    );
}

fn snapshot_json_object(repo: &Path, oid: &str) -> serde_json::Value {
    let output = run_libra_command(&["cat-file", "-p", oid], repo);
    assert_cli_success(&output, "read operation snapshot object");
    serde_json::from_slice(&output.stdout).expect("operation snapshot JSON")
}

fn snapshot_oid(value: &serde_json::Value) -> String {
    serde_json::from_value::<git_internal::hash::ObjectHash>(value.clone())
        .expect("snapshot object hash")
        .to_string()
}

fn latest_snapshot(repo: &Path, bare: bool) -> serde_json::Value {
    let gitdir = if bare {
        repo.to_path_buf()
    } else {
        repo.join(".libra")
    };
    let pointer: serde_json::Value = serde_json::from_slice(
        &fs::read(gitdir.join("info/operation-pointer.json")).expect("operation pointer"),
    )
    .expect("pointer JSON");
    snapshot_json_object(
        repo,
        &snapshot_oid(&pointer["pointer"]["last_snapshot_oid"]),
    )
}

#[test]
fn bare_reinit_snapshot_excludes_repository_metadata() {
    for (format, name) in [
        ("sha1", "repo.git"),
        ("sha256", "repo.git"),
        ("sha1", ".libra"),
        ("sha256", ".libra"),
    ] {
        let temp = tempdir().expect("tempdir");
        let init = run_libra_command(
            &[
                "init",
                "--bare",
                name,
                "--object-format",
                format,
                "--vault",
                "false",
            ],
            temp.path(),
        );
        assert_cli_success(&init, "initial bare init");
        let repo = temp.path().join(name);
        let reinit = run_libra_command(&["init", "--bare", "--vault", "false"], &repo);
        assert_cli_success(&reinit, "bare reinit");
        let snapshot = latest_snapshot(&repo, true);
        let untracked =
            snapshot_json_object(&repo, &snapshot_oid(&snapshot["untracked_manifest_oid"]));
        let files = untracked["files"].as_object().expect("untracked files");
        assert!(
            files.is_empty(),
            "BARE_SNAPSHOT_METADATA: {format} has no working tree, but snapshot retained {:?}",
            files.keys().collect::<Vec<_>>()
        );
        let tree = run_libra_command(
            &[
                "cat-file",
                "-p",
                &snapshot_oid(&snapshot["working_copy_tree_oid"]),
            ],
            &repo,
        );
        assert_cli_success(&tree, "read bare working-copy tree");
        assert!(
            tree.stdout.is_empty(),
            "bare working-copy tree must be empty"
        );
        assert!(
            repo.join("libra.db").is_file(),
            "metadata must remain on disk"
        );
    }
}

#[test]
fn worktree_reinit_snapshot_keeps_visible_files() {
    for format in ["sha1", "sha256"] {
        let repo = tempdir().expect("tempdir");
        let init = run_libra_command(
            &["init", "--object-format", format, "--vault", "false"],
            repo.path(),
        );
        assert_cli_success(&init, "initial worktree init");
        fs::write(repo.path().join("visible.txt"), b"working-copy payload\n")
            .expect("visible file");
        let reinit = run_libra_command(&["init", "--vault", "false"], repo.path());
        assert_cli_success(&reinit, "worktree reinit");
        let snapshot = latest_snapshot(repo.path(), false);
        let untracked = snapshot_json_object(
            repo.path(),
            &snapshot_oid(&snapshot["untracked_manifest_oid"]),
        );
        let files = untracked["files"].as_object().expect("untracked files");
        assert_eq!(
            files.keys().map(String::as_str).collect::<Vec<_>>(),
            ["visible.txt"]
        );
        let payload = run_libra_command(
            &["cat-file", "-p", &snapshot_oid(&files["visible.txt"])],
            repo.path(),
        );
        assert_cli_success(&payload, "read captured working-copy payload");
        assert_eq!(payload.stdout, b"working-copy payload\n");
    }
}

#[test]
fn init_bare_reinit_tops_up_and_preserves_state() {
    let temp = tempdir().unwrap();

    let first = run_libra_command(
        &["init", "--bare", "repo.git", "--vault", "false"],
        temp.path(),
    );
    assert_cli_success(&first, "initial bare init");

    let bare_repo = temp.path().join("repo.git");
    let repo_id = |dir: &std::path::Path| {
        String::from_utf8_lossy(&run_libra_command(&["config", "get", "libra.repoid"], dir).stdout)
            .trim()
            .to_string()
    };
    let id_before = repo_id(&bare_repo);
    assert!(!id_before.is_empty(), "bare repo id should be set");

    // Git-style re-initialization succeeds, prints the "Reinitialized existing"
    // banner, and preserves the existing repository identity.
    let second = run_libra_command(&["init", "--bare", "--vault", "false"], &bare_repo);
    assert_cli_success(&second, "bare reinit should succeed");
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        stdout.contains("Reinitialized existing") && stdout.contains("bare"),
        "expected bare reinit banner, got: {stdout}"
    );
    assert_eq!(
        repo_id(&bare_repo),
        id_before,
        "bare repo id preserved across reinit"
    );
}

#[test]
fn init_worktree_reinit_tops_up_and_preserves_state() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    let first = run_libra_command(&["init", "--vault", "false"], &repo);
    assert_cli_success(&first, "initial worktree init");

    let config_value = |key: &str| {
        String::from_utf8_lossy(&run_libra_command(&["config", "get", key], &repo).stdout)
            .trim()
            .to_string()
    };
    let id_before = config_value("libra.repoid");
    assert!(!id_before.is_empty(), "repo id should be set");
    // A user config value must survive re-initialization.
    assert_cli_success(
        &run_libra_command(&["config", "set", "user.name", "Reinit Tester"], &repo),
        "set user.name",
    );
    // Customize a standard template file: re-init must NOT clobber it (Git-style
    // top-up only writes MISSING files).
    let exclude = repo.join(".libra/info/exclude");
    fs::write(&exclude, "# my custom excludes\ncustom-cache/\n").unwrap();
    // Remove a different standard template file to prove re-init tops it back up.
    let pre_commit = repo.join(".libra/hooks/pre-commit.sh");
    if pre_commit.exists() {
        fs::remove_file(&pre_commit).unwrap();
    }

    let second = run_libra_command(&["init", "--vault", "false"], &repo);
    assert_cli_success(&second, "worktree reinit should succeed");
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        stdout.contains("Reinitialized existing"),
        "expected worktree reinit banner, got: {stdout}"
    );

    // Database state (repo id + user config) is preserved.
    assert_eq!(config_value("libra.repoid"), id_before, "repo id preserved");
    assert_eq!(
        config_value("user.name"),
        "Reinit Tester",
        "user config preserved"
    );
    // The customized template is preserved verbatim; the missing one is re-created.
    assert_eq!(
        fs::read_to_string(&exclude).unwrap(),
        "# my custom excludes\ncustom-cache/\n",
        "reinit must not clobber a customized info/exclude"
    );
    assert!(
        pre_commit.exists(),
        "reinit re-created the missing pre-commit.sh template"
    );
}

#[test]
fn init_reinit_ignored_flag_warning_triggers_exit_code_on_warning() {
    // Re-initializing with a differing --initial-branch emits an "ignored" warning;
    // under --exit-code-on-warning that warning must change the exit code (the
    // warning is recorded in every output mode).
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false", "-b", "main"], &repo),
        "initial init",
    );

    let warned = run_libra_command(
        &[
            "--exit-code-on-warning",
            "init",
            "--vault",
            "false",
            "-b",
            "different",
        ],
        &repo,
    );
    let stderr = String::from_utf8_lossy(&warned.stderr);
    assert!(
        stderr.contains("ignoring --initial-branch"),
        "expected an ignored-flag warning, got: {stderr}"
    );
    assert_ne!(
        warned.status.code(),
        Some(0),
        "--exit-code-on-warning should make the ignored-flag warning non-zero"
    );
}

#[test]
fn init_reinit_rejects_invalid_flag_values() {
    // Re-init still VALIDATES supplied `--object-format`/`--initial-branch`: an
    // invalid value is rejected (non-zero) rather than silently warned/ignored.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "initial init",
    );

    // Remove a standard template so we can prove an INVALID reinit performs no
    // filesystem side effects before failing (validation precedes the top-up).
    let pre_commit = repo.join(".libra/hooks/pre-commit.sh");
    if pre_commit.exists() {
        fs::remove_file(&pre_commit).unwrap();
    }

    let bad_format = run_libra_command(
        &["init", "--vault", "false", "--object-format", "sha265"],
        &repo,
    );
    assert_ne!(
        bad_format.status.code(),
        Some(0),
        "reinit must reject an invalid --object-format"
    );
    assert!(
        !pre_commit.exists(),
        "an invalid reinit must not top up the layout before failing"
    );

    let bad_branch = run_libra_command(&["init", "--vault", "false", "-b", "bad..branch"], &repo);
    assert_ne!(
        bad_branch.status.code(),
        Some(0),
        "reinit must reject an invalid --initial-branch"
    );

    // A subsequent VALID reinit does top up the missing template.
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "valid reinit",
    );
    assert!(
        pre_commit.exists(),
        "a valid reinit re-creates the missing template"
    );
}

#[cfg(unix)]
#[test]
fn init_reinit_refuses_symlinked_layout_dir() {
    use std::os::unix::fs::symlink;

    // A re-init must not follow a symlinked layout directory and write templates
    // outside the repository.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "initial init",
    );

    // Replace .libra/hooks with a symlink pointing at an outside directory.
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let hooks = repo.join(".libra/hooks");
    fs::remove_dir_all(&hooks).unwrap();
    symlink(&outside, &hooks).unwrap();

    let reinit = run_libra_command(&["init", "--vault", "false"], &repo);
    assert_ne!(
        reinit.status.code(),
        Some(0),
        "reinit must refuse a symlinked layout directory"
    );
    assert!(
        !outside.join("pre-commit.sh").exists(),
        "reinit must not write a template through the symlink into the outside directory"
    );
}

#[cfg(unix)]
#[test]
fn init_reinit_template_refuses_symlinked_layout_dir() {
    use std::os::unix::fs::symlink;

    // The `--template` copy path must also refuse to follow a symlinked layout
    // directory out of the repository during re-init.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "initial init",
    );

    // A template directory with a hooks/ entry to copy.
    let template = temp.path().join("template");
    fs::create_dir_all(template.join("hooks")).unwrap();
    fs::write(template.join("hooks").join("post-checkout"), "echo hi\n").unwrap();

    // Symlink .libra/hooks to an outside directory.
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let hooks = repo.join(".libra/hooks");
    fs::remove_dir_all(&hooks).unwrap();
    symlink(&outside, &hooks).unwrap();

    let reinit = run_libra_command(
        &[
            "init",
            "--vault",
            "false",
            "--template",
            template.to_str().unwrap(),
        ],
        &repo,
    );
    assert_ne!(
        reinit.status.code(),
        Some(0),
        "reinit --template must refuse a symlinked hooks directory"
    );
    assert!(
        !outside.join("post-checkout").exists(),
        "reinit --template must not copy a template file through the symlink"
    );
}

#[cfg(unix)]
#[test]
fn init_reinit_template_does_not_write_through_symlinked_file() {
    use std::os::unix::fs::symlink;

    // The `--template` copy must not follow a symlinked destination FILE (the case the
    // old `exists()` check would have followed): a broken symlink pointing outside the
    // repo must be left untouched, and its target must never be created.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "initial init",
    );

    let template = temp.path().join("template");
    fs::create_dir_all(template.join("hooks")).unwrap();
    fs::write(template.join("hooks").join("post-checkout"), "echo hi\n").unwrap();

    // `.libra/hooks` stays a REAL directory; only the destination FILE is a (broken)
    // symlink to an outside, not-yet-existing target.
    let outside_target = temp.path().join("outside-target");
    let dest_file = repo.join(".libra/hooks/post-checkout");
    symlink(&outside_target, &dest_file).unwrap();

    let reinit = run_libra_command(
        &[
            "init",
            "--vault",
            "false",
            "--template",
            template.to_str().unwrap(),
        ],
        &repo,
    );
    assert_cli_success(
        &reinit,
        "reinit --template with a symlinked destination file should succeed (file skipped)",
    );
    assert!(
        !outside_target.exists(),
        "reinit must not write through the symlinked file to create the outside target"
    );
    assert!(
        fs::symlink_metadata(&dest_file)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the symlinked destination file is left untouched"
    );
}

#[cfg(unix)]
#[test]
fn init_reinit_shared_preserves_vault_permissions() {
    use std::os::unix::fs::PermissionsExt;

    // `--shared` chmods the whole tree on re-init; the existing vault database must
    // stay owner-only (0o600) so private signing material is never exposed.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    // A vaulted repo (default) so vault.db exists.
    assert_cli_success(&run_libra_command(&["init"], &repo), "vaulted init");
    let vault_db = repo.join(".libra/vault.db");
    assert!(vault_db.exists(), "vaulted init should create vault.db");
    let mode_before = fs::metadata(&vault_db).unwrap().permissions().mode() & 0o777;

    let second = run_libra_command(&["init", "--shared=all"], &repo);
    assert_cli_success(&second, "shared reinit should succeed");

    // `--shared all` must NOT widen the private vault database (it is skipped by the
    // chmod sweep), so its mode is unchanged — in particular it does not gain the
    // group/world WRITE+EXEC bits the sweep applies elsewhere.
    let mode_after = fs::metadata(&vault_db).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode_after, mode_before,
        "vault.db mode must be unchanged by --shared reinit (not widened), got {mode_after:o}"
    );
    // ...while a non-vault file IS widened, proving the --shared sweep actually ran.
    let exclude_mode = fs::metadata(repo.join(".libra/info/exclude"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        exclude_mode, 0o777,
        "a non-vault file should be widened by --shared all, got {exclude_mode:o}"
    );
}

#[test]
fn init_invalid_object_format_suggests_sha256() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    let output = run_libra_command(&["init", "--object-format", "sha265"], &repo);
    assert_eq!(output.status.code(), Some(129));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported object format 'sha265'"),
        "expected object-format error, got: {stderr}"
    );
    assert!(
        stderr.contains("did you mean 'sha256'?"),
        "expected fuzzy-match hint, got: {stderr}"
    );
    assert!(
        stderr.contains("LBR-CLI-002"),
        "expected CLI invalid-arguments code, got: {stderr}"
    );
}

#[test]
fn init_vault_true_ignores_commit_use_config_only_strictness() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    let set = run_libra_command(&["config", "--global", "user.useConfigOnly", "true"], &repo);
    assert_cli_success(&set, "set user.useConfigOnly");

    let output = run_libra_command(&["init"], &repo);
    assert_cli_success(
        &output,
        "init should still succeed even when user.useConfigOnly=true and identity is missing",
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Generating PGP signing key ..."),
        "expected vault key generation progress, got: {stderr}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// #472: the Libra home (`$LIBRA_HOME`, default `~/.libra`) is per-user state,
// never repository storage. Global config lives in `~/.libra/config.db`; repo
// data lives in a project's `.libra/libra.db`.
// ─────────────────────────────────────────────────────────────────────────────

/// Seed the "stray artifact" database shape found in a real user's `~/.libra`
/// (#472): schema bookkeeping at the current registry tip, `config_kv` and
/// `object_index` present, legacy `config` table missing. Opening it must not
/// be necessary for any command — repository discovery skips the home.
async fn seed_libra_home_artifact_db(db_path: &std::path::Path) {
    use sea_orm::{ConnectionTrait, Database, Statement};

    assert!(!db_path.exists(), "artifact db must not pre-exist");
    // sqlx refuses to CREATE a missing SQLite file (create_if_missing=false);
    // an empty file is a valid empty database, so seed it before connecting.
    std::fs::File::create(db_path).expect("create empty artifact db file");
    let conn = Database::connect(format!("sqlite://{}", db_path.display()))
        .await
        .expect("connect artifact db");
    for ddl in [
        "CREATE TABLE `config_kv` (\
             `id` INTEGER PRIMARY KEY AUTOINCREMENT, \
             `key` TEXT NOT NULL, \
             `value` TEXT NOT NULL, \
             `encrypted` INTEGER NOT NULL DEFAULT 0)",
        "CREATE TABLE `object_index` (\
             `id` INTEGER PRIMARY KEY AUTOINCREMENT, \
             `o_id` TEXT NOT NULL, \
             `o_type` TEXT NOT NULL, \
             `o_size` INTEGER NOT NULL, \
             `repo_id` TEXT NOT NULL, \
             `created_at` INTEGER NOT NULL, \
             `is_synced` INTEGER DEFAULT 0)",
        "CREATE TABLE `schema_versions` (\
             `version` INTEGER PRIMARY KEY, \
             `name` TEXT NOT NULL, \
             `applied_at` TEXT NOT NULL)",
    ] {
        conn.execute_unprepared(ddl)
            .await
            .expect("seed artifact table");
    }
    let latest = libra::internal::db::migration::latest_builtin_schema_version()
        .expect("read latest schema version")
        .expect("built-in migrations have a latest version");
    conn.execute_raw(Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Sqlite,
        "INSERT INTO `schema_versions` (`version`, `name`, `applied_at`) VALUES (?, ?, ?)",
        [
            latest.into(),
            "artifact-seed".into(),
            "2026-09-06T00:00:00Z".into(),
        ],
    ))
    .await
    .expect("seed artifact schema version");
    conn.close().await.expect("close artifact db");
}

/// `libra init <dir>` run from `$HOME` must NOT read the stray
/// `~/.libra/libra.db` artifact as repo-local config — the failure mode was
/// `fatal: ... no such table: config`. Discovery skips the Libra home, the
/// config cascade falls through to the global scope (`~/.libra/config.db`),
/// and the initial branch comes from the GLOBAL `init.defaultBranch`.
#[tokio::test]
async fn init_from_home_ignores_libra_home_artifact_and_honors_global_config() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("fake-home");
    fs::create_dir_all(&home).unwrap();
    let libra_home = home.join(".libra");
    fs::create_dir_all(&libra_home).unwrap();
    seed_libra_home_artifact_db(&libra_home.join("libra.db")).await;

    let envs = [
        ("HOME", home.to_str().unwrap()),
        ("USERPROFILE", home.to_str().unwrap()),
    ];
    let artifact_before = fs::read(libra_home.join("libra.db")).unwrap();

    // A distinct branch name proves the value came from the global scope (the
    // built-in default is `main`).
    let set = run_libra_command_with_env(
        &["config", "--global", "init.defaultBranch", "trunk"],
        &home,
        &envs,
    );
    assert_cli_success(&set, "seed global init.defaultBranch");

    let output = run_libra_command_with_env(&["init", "linked"], &home, &envs);
    assert_cli_success(
        &output,
        "init must succeed from a directory whose .libra is the Libra home",
    );
    assert!(
        home.join("linked").join(".libra").join("libra.db").exists(),
        "the repository must be created at the requested target"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("branch: trunk"),
        "the initial branch must come from the global scope: {stdout}"
    );
    assert_eq!(
        fs::read(libra_home.join("libra.db")).unwrap(),
        artifact_before
    );
}

/// `libra init` whose storage root IS the Libra home is refused: repo data
/// must live in a project's `.libra/`, never mixed into the per-user state
/// directory (global config, vault keys, binaries).
#[tokio::test]
async fn init_refuses_to_initialize_inside_the_libra_home() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("fake-home");
    fs::create_dir_all(&home).unwrap();
    let libra_home = home.join(".libra");
    fs::create_dir_all(&libra_home).unwrap();
    seed_libra_home_artifact_db(&libra_home.join("libra.db")).await;

    let envs = [("LIBRA_HOME", libra_home.to_str().unwrap())];
    let output = run_libra_command_with_env(&["init"], &home, &envs);
    assert!(
        !output.status.success(),
        "initializing with the Libra home as storage root must be refused"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Libra home"),
        "the refusal must name the Libra home: {stderr}"
    );
    assert!(
        !libra_home.join("objects").exists(),
        "no repository layout may be written into the Libra home"
    );
}

/// ADR-GCX-01: the XDG global configuration directory is reserved per-user
/// state; `libra init` must refuse it just like the Libra home.
#[tokio::test]
async fn init_refuses_to_initialize_inside_the_global_config_dir() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("fake-home");
    let config_dir = home.join(".config").join("libra");
    fs::create_dir_all(&config_dir).unwrap();

    let xdg_config = home.join(".config");
    let envs = [
        ("HOME", home.to_str().unwrap()),
        ("USERPROFILE", home.to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg_config.to_str().unwrap()),
    ];
    let output = run_libra_command_with_env(&["init"], &config_dir, &envs);
    assert!(
        !output.status.success(),
        "initializing inside the global config directory must be refused"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Libra home") && stderr.contains("global configuration"),
        "the refusal must name the per-user state directory: {stderr}"
    );
    assert!(
        !config_dir.join("objects").exists(),
        "no repository layout may be written into the global config directory"
    );
}

#[test]
fn init_object_format_blake3_succeeds() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    let output = run_libra_command(
        &["init", "--vault", "false", "--object-format", "blake3"],
        &repo,
    );
    assert_cli_success(&output, "init --object-format blake3");
    let got = run_libra_command(&["config", "--local", "--get", "core.objectformat"], &repo);
    assert_cli_success(&got, "get core.objectformat");
    assert_eq!(
        String::from_utf8_lossy(&got.stdout).trim(),
        "blake3",
        "blake3 init must persist core.objectformat=blake3"
    );
}

#[test]
fn init_object_format_sha256_succeeds() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    let output = run_libra_command(
        &["init", "--vault", "false", "--object-format", "sha256"],
        &repo,
    );
    assert_cli_success(&output, "init --object-format sha256");
    let got = run_libra_command(&["config", "--local", "--get", "core.objectformat"], &repo);
    assert_cli_success(&got, "get core.objectformat");
    assert_eq!(String::from_utf8_lossy(&got.stdout).trim(), "sha256");
}

#[test]
fn reinit_rejects_object_format_change() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "initial sha1 init",
    );
    let pre_commit = repo.join(".libra/hooks/pre-commit.sh");
    if pre_commit.exists() {
        fs::remove_file(&pre_commit).unwrap();
    }

    let output = run_libra_command(
        &["init", "--vault", "false", "--object-format", "sha256"],
        &repo,
    );
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LBR-REPO-003")
            && stderr.contains("attempt to reinitialize repository with different hash"),
        "reinit format change must be LBR-REPO-003: {stderr}"
    );
    assert!(
        !pre_commit.exists(),
        "reinit format change must not top up layout"
    );
    let got = run_libra_command(&["config", "--local", "--get", "core.objectformat"], &repo);
    assert_eq!(String::from_utf8_lossy(&got.stdout).trim(), "sha1");
}

#[test]
fn reinit_same_object_format_tops_up() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(
            &["init", "--vault", "false", "--object-format", "sha256"],
            &repo,
        ),
        "initial sha256 init",
    );
    let pre_commit = repo.join(".libra/hooks/pre-commit.sh");
    if pre_commit.exists() {
        fs::remove_file(&pre_commit).unwrap();
    }
    let output = run_libra_command(
        &["init", "--vault", "false", "--object-format", "sha256"],
        &repo,
    );
    assert_cli_success(&output, "same-format reinit");
    assert!(
        pre_commit.exists(),
        "same-format reinit must top up missing templates"
    );
}

#[test]
fn reinit_unknown_stored_format_errors() {
    use sea_orm::{ConnectionTrait, Database};

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "initial init",
    );

    let db_path = repo.join(".libra/libra.db");
    let url = format!("sqlite:{}?mode=rwc", db_path.to_string_lossy());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = Database::connect(&url).await.expect("connect");
        db.execute_unprepared(
            "UPDATE config_kv SET value='not-a-real-format' WHERE key='core.objectformat'",
        )
        .await
        .expect("poison objectformat");
        db.close().await.ok();
    });

    let pre_commit = repo.join(".libra/hooks/pre-commit.sh");
    if pre_commit.exists() {
        fs::remove_file(&pre_commit).unwrap();
    }
    // Invoke from the parent so CLI preflight does not open the poisoned
    // repository before `init`'s reinit inspect path can return LBR-REPO-002.
    let output = run_libra_command(&["init", "--vault", "false", "repo"], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LBR-REPO-002") && stderr.contains("unknown core.objectformat"),
        "unknown stored format must be LBR-REPO-002: {stderr}"
    );
    assert!(
        !pre_commit.exists(),
        "unknown format must not top up layout"
    );
}

#[test]
fn init_machine_blake3_object_format() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    let output = run_libra_command(
        &[
            "--machine",
            "init",
            "--vault",
            "false",
            "--object-format",
            "blake3",
        ],
        &repo,
    );
    assert_cli_success(&output, "machine init blake3");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|l| !l.trim().is_empty())
        .expect("machine output line");
    let parsed: serde_json::Value = serde_json::from_str(line).expect("machine JSON");
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["data"]["object_format"].as_str(), Some("blake3"));
}

// ---------------------------------------------------------------------------
// Issue #473 IN-01/IN-02/IN-03/IN-08/IN-09
// ---------------------------------------------------------------------------

/// IN-01 (M-TPL T1/T5): an empty `--template=` creates only the required
/// structure (no built-in hooks/exclude, no current-dir recursion) and a
/// reinit with it changes nothing existing.
#[test]
fn test_init_blank_template_matrix() {
    let root = tempdir().unwrap();
    let repo = root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    // A stray file in the target: with empty template it must not be copied
    // into the storage dir.
    fs::write(repo.join("stray.txt"), "x").unwrap();

    let out = run_libra_command(&["init", "--template="], &repo);
    assert_cli_success(&out, "blank template init");
    assert!(
        !repo.join(".libra/info/exclude").exists(),
        "no builtin exclude"
    );
    assert!(
        !repo.join(".libra/hooks/pre-commit.sh").exists(),
        "no builtin hook"
    );
    assert!(!repo.join(".libra/stray.txt").exists(), "no self-recursion");
    assert!(
        repo.join(".libra/objects/pack").exists(),
        "required objects/pack"
    );
    assert!(
        repo.join(".libra/objects/info").exists(),
        "required objects/info"
    );

    let out = run_libra_command(&["init", "--template="], &repo);
    assert_cli_success(&out, "blank template reinit");
    assert!(repo.join(".libra/libra.db").exists(), "db preserved");
    assert!(
        !repo.join(".libra/stray.txt").exists(),
        "still no recursion"
    );
    assert!(
        !repo.join(".libra/info/exclude").exists(),
        "no builtin exclude after reinit"
    );
}

/// IN-01 (M-TPL T2): a template equal to the storage dir is refused, zero write.
#[test]
fn test_init_template_self_reference_guard() {
    let root = tempdir().unwrap();
    let repo = root.path().join("repo");
    super::init_repo_via_cli(&repo);

    let out = run_libra_command(&["init", "--template=.libra"], &repo);
    assert_eq!(out.status.code(), Some(129));
    let (_h, report) = super::parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(repo.join(".libra/libra.db").exists(), "db untouched");
}

/// IN-02 (M-REINIT R1/R2/R3/R7): a different `--object-format` on reinit is a
/// 128 repo-state failure with the Git wording, zero side effects; same format
/// and bare reinit stay a top-up.
#[test]
fn test_init_reinit_object_format_mismatch_matrix() {
    let root = tempdir().unwrap();
    let sha1 = root.path().join("sha1");
    super::init_repo_via_cli(&sha1);

    let out = run_libra_command(&["init", "--object-format=sha256"], &sha1);
    assert_eq!(out.status.code(), Some(128));
    let (_h, report) = super::parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert!(
        report
            .message
            .contains("attempt to reinitialize repository with different hash")
    );
    let stored = run_libra_command(&["config", "--local", "--get", "core.objectformat"], &sha1);
    assert_eq!(String::from_utf8_lossy(&stored.stdout).trim(), "sha1");

    let sha256 = root.path().join("sha256");
    assert_cli_success(
        &run_libra_command(&["init", "--object-format=sha256"], &sha256),
        "sha256 init",
    );
    let out = run_libra_command(&["init", "--object-format=sha1"], &sha256);
    assert_eq!(out.status.code(), Some(128));

    // R3: same format and no-arg reinit succeed.
    assert_cli_success(
        &run_libra_command(&["init", "--object-format=sha1"], &sha1),
        "same format reinit",
    );
    assert_cli_success(&run_libra_command(&["init"], &sha1), "no-arg reinit");

    // R7: JSON envelope carries the stable code.
    let out = run_libra_command(&["--json", "init", "--object-format=sha256"], &sha1);
    assert_eq!(out.status.code(), Some(128));
    let (_h, report) = super::parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-REPO-003");
}

/// IN-02 (M-REINIT R4): an unknown stored `core.objectformat` is a 128
/// repo-corrupt failure, not silently sha1.
#[tokio::test]
async fn test_init_reinit_unknown_stored_format() {
    let root = tempdir().unwrap();
    let repo = root.path().join("repo");
    super::init_repo_via_cli(&repo);
    // Overwrite the stored format with an unknown value.
    let conn = open_repo_conn(&repo, false).await;
    ConfigKv::set_with_conn(&conn, "core.objectformat", "sha999", false)
        .await
        .unwrap();
    drop(conn);

    let out = run_libra_command(&["init", "--object-format=sha256"], &repo);
    assert_eq!(out.status.code(), Some(128));
    let (_h, report) = super::parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-REPO-002");
}

/// IN-03 (M-EXIST E1/E2/E6): a pre-existing FILE where storage is required
/// fails with 128 LBR-REPO-002 before the reinit banner.
#[test]
fn test_init_storage_path_not_directory_matrix() {
    let root = tempdir().unwrap();
    let repo = root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join(".libra"), "x").unwrap();

    let out = run_libra_command(&["init"], &repo);
    assert_eq!(out.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("Reinitializing"),
        "no reinit banner: {stderr}"
    );
    let (_h, report) = super::parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert_eq!(fs::read(repo.join(".libra")).unwrap(), b"x");

    // E2: bare `objects` is a file.
    let bare = root.path().join("bare");
    fs::create_dir_all(&bare).unwrap();
    fs::write(bare.join("objects"), "x").unwrap();
    let out = run_libra_command(&["init", "--bare"], &bare);
    assert_eq!(out.status.code(), Some(128));

    // E6: JSON envelope.
    let out = run_libra_command(&["--json", "init"], &repo);
    assert_eq!(out.status.code(), Some(128));
    let (_human, report) = super::parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-REPO-002");
}

/// IN-08 (M-TPLSRC S1/S2/S4): template sources cascade env > config, and
/// `--template` wins. The test harness isolates the global config DB per cwd,
/// so the config source is set from the same directory the init runs in.
#[test]
fn test_init_template_sources_matrix() {
    let root = tempdir().unwrap();
    let env_tpl = root.path().join("envtpl");
    fs::create_dir_all(env_tpl.join("hooks")).unwrap();
    fs::write(env_tpl.join("hooks/env.sh"), "x").unwrap();
    let cfg_tpl = root.path().join("cfgtpl");
    fs::create_dir_all(cfg_tpl.join("hooks")).unwrap();
    fs::write(cfg_tpl.join("hooks/cfg.sh"), "x").unwrap();

    // S1: environment source.
    let repo = root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let env = env_tpl.to_str().unwrap();
    let out = super::run_libra_command_with_env(&["init"], &repo, &[("GIT_TEMPLATE_DIR", env)]);
    assert_cli_success(&out, "env template");
    assert!(repo.join(".libra/hooks/env.sh").exists());

    // S2: config source (same cwd so the isolated global DB matches).
    let repo2 = root.path().join("repo2");
    fs::create_dir_all(&repo2).unwrap();
    let out = run_libra_command(
        &[
            "config",
            "--global",
            "init.templateDir",
            cfg_tpl.to_str().unwrap(),
        ],
        &repo2,
    );
    assert_cli_success(&out, "set init.templateDir");
    let out = run_libra_command(&["init"], &repo2);
    assert_cli_success(&out, "config template");
    assert!(repo2.join(".libra/hooks/cfg.sh").exists());

    // S4: `--template` beats the environment source.
    let cli_tpl = root.path().join("clitpl");
    fs::create_dir_all(cli_tpl.join("hooks")).unwrap();
    fs::write(cli_tpl.join("hooks/cli.sh"), "x").unwrap();
    let repo3 = root.path().join("repo3");
    fs::create_dir_all(&repo3).unwrap();
    let out = super::run_libra_command_with_env(
        &["init", "--template", cli_tpl.to_str().unwrap()],
        &repo3,
        &[("GIT_TEMPLATE_DIR", env)],
    );
    assert_cli_success(&out, "cli template");
    assert!(repo3.join(".libra/hooks/cli.sh").exists());
    assert!(!repo3.join(".libra/hooks/env.sh").exists());
}

/// IN-09 (M-DEFHASH H1/H2/H4/H5): object-format sources cascade env > config;
/// invalid env is 129, invalid config warns and falls back to sha1.
#[test]
fn test_init_default_object_format_sources_matrix() {
    let root = tempdir().unwrap();

    // H2: environment source.
    let env_repo = root.path().join("env");
    fs::create_dir_all(&env_repo).unwrap();
    let out =
        super::run_libra_command_with_env(&["init"], &env_repo, &[("GIT_DEFAULT_HASH", "sha256")]);
    assert_cli_success(&out, "env hash");

    // H3 (cli wins): explicit `--object-format` overrides the environment.
    let cli_repo = root.path().join("cli");
    fs::create_dir_all(&cli_repo).unwrap();
    let out = super::run_libra_command_with_env(
        &["init", "--object-format=sha256"],
        &cli_repo,
        &[("GIT_DEFAULT_HASH", "sha1")],
    );
    assert_cli_success(&out, "cli hash wins");

    // H1: config source (same cwd so the isolated global DB matches).
    let cfg_repo = root.path().join("cfg");
    fs::create_dir_all(&cfg_repo).unwrap();
    assert_cli_success(
        &run_libra_command(
            &["config", "--global", "init.defaultObjectFormat", "sha256"],
            &cfg_repo,
        ),
        "set init.defaultObjectFormat",
    );
    let out = run_libra_command(&["init"], &cfg_repo);
    assert_cli_success(&out, "config hash");

    // H5: invalid env is a usage error with zero writes.
    let bad = root.path().join("bad");
    fs::create_dir_all(&bad).unwrap();
    let out = super::run_libra_command_with_env(&["init"], &bad, &[("GIT_DEFAULT_HASH", "sha999")]);
    assert_eq!(out.status.code(), Some(129));
    assert!(!bad.join(".libra").exists());

    // H4: invalid config warns and falls back to sha1.
    let fallback = root.path().join("fallback");
    fs::create_dir_all(&fallback).unwrap();
    assert_cli_success(
        &run_libra_command(
            &["config", "--global", "init.defaultObjectFormat", "garbage"],
            &fallback,
        ),
        "set invalid",
    );
    let out = run_libra_command(&["init"], &fallback);
    assert_cli_success(&out, "fallback init");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown hash algorithm"),
        "warning: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stored = run_libra_command(
        &["config", "--local", "--get", "core.objectformat"],
        &fallback,
    );
    assert_eq!(String::from_utf8_lossy(&stored.stdout).trim(), "sha1");
}

/// Issue #501 AC 2/3: a template that CONTAINS the target must never copy the
/// target into itself. `--template=.` (the parent directory of the new
/// repository) used to nest `<dir>/.libra/<dir>/.libra/...` until
/// `ENAMETOOLONG`; the guard now skips the whole target worktree, so a single
/// invocation leaves no nested copy and repeated invocations stay constant.
#[test]
fn test_init_template_cwd_never_copies_the_target_into_itself() {
    let root = tempdir().unwrap();
    let workdir = root.path();
    fs::write(workdir.join("sibling.txt"), "sibling\n").unwrap();

    let first = run_libra_command(&["init", "--template=.", "repo"], workdir);
    assert_cli_success(&first, "init --template=. repo");
    let nested: Vec<_> = fs::read_dir(workdir.join("repo").join(".libra"))
        .expect("read storage root")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name == "repo")
        .collect();
    assert!(
        nested.is_empty(),
        "the target must not be copied into its own storage root: {nested:?}"
    );

    // AC 3: repeated invocations leave NO `<dir>/.libra/<dir>` residue. (The
    // harness keeps an isolated HOME inside the working directory, so a
    // template of `.` legitimately copies those config files once; the
    // regression this guards is the recursive copy of the TARGET itself, which
    // is what grew one level per invocation and ended in ENAMETOOLONG.)
    for _ in 0..4 {
        let again = run_libra_command(&["init", "--template=.", "repo"], workdir);
        assert_cli_success(&again, "repeated init --template=. repo");
    }
    assert_eq!(
        count_directories_named(&workdir.join("repo"), "repo"),
        0,
        "repeated --template=. inits must not nest copies of the target"
    );

    // A sibling file in the template is still copied (the skip is targeted).
    assert!(
        workdir.join("repo/.libra/sibling.txt").exists(),
        "non-target template content is still copied"
    );
}

/// Issue #501 AC 1/2: `--template=.` where the target IS the current directory
/// is refused up front with a CLI error instead of self-copying.
#[test]
fn test_init_template_equal_to_target_is_refused() {
    let root = tempdir().unwrap();
    let workdir = root.path();

    let out = run_libra_command(&["init", "--template=.", "."], workdir);
    assert_eq!(
        out.status.code(),
        Some(129),
        "self template must be refused"
    );
    let (_human, report) = super::parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        report.message.contains("is the target repository itself"),
        "diagnostic must name the self reference: {}",
        report.message
    );
    assert!(
        !workdir.join(".libra").exists(),
        "a refused template must write nothing"
    );
}

#[cfg(unix)]
#[test]
fn test_init_template_symlinked_target_skips_self_copy_on_init_and_reinit() {
    let root = tempdir().unwrap();
    let template = root.path().join("source");
    fs::create_dir(&template).unwrap();
    fs::write(template.join("sibling.txt"), "sibling\n").unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&template, &alias).unwrap();
    let target = alias.join("repo");
    let template_arg = format!("--template={}", template.display());
    let target_arg = target.to_string_lossy().into_owned();

    for attempt in 0..2 {
        let result = run_libra_command(
            &["init", "--vault", "false", &template_arg, &target_arg],
            root.path(),
        );
        assert_cli_success(&result, &format!("symlinked target attempt {attempt}"));
        assert!(template.join("repo/.libra/sibling.txt").exists());
        assert!(
            !template.join("repo/.libra/repo").exists(),
            "a target reached through a symlink must not copy itself into .libra"
        );
    }
}

#[test]
fn test_init_template_dotdot_target_skips_self_copy() {
    let root = tempdir().unwrap();
    let template = root.path().join("source");
    fs::create_dir(&template).unwrap();
    fs::create_dir(template.join("carrier")).unwrap();
    fs::write(template.join("sibling.txt"), "sibling\n").unwrap();
    let target = template.join("carrier/../repo");
    let template_arg = format!("--template={}", template.display());
    let target_arg = target.to_string_lossy().into_owned();

    let result = run_libra_command(
        &["init", "--vault", "false", &template_arg, &target_arg],
        root.path(),
    );
    assert_cli_success(&result, "target with parent traversal");
    assert!(template.join("repo/.libra/sibling.txt").exists());
    assert!(
        !template.join("repo/.libra/repo").exists(),
        "a target reached through .. must not copy itself into .libra"
    );
}

#[cfg(unix)]
#[test]
fn test_init_non_directory_parent_component_refuses_before_writing() {
    for bare in [false, true] {
        for target in ["regular-file/..", "regular-file/child", "link-to-file/.."] {
            let root = tempdir().unwrap();
            let file = root.path().join("regular-file");
            fs::write(&file, "user data\n").unwrap();
            std::os::unix::fs::symlink(&file, root.path().join("link-to-file")).unwrap();

            let mut args = vec!["--json", "init", "--vault", "false", "-q"];
            if bare {
                args.push("--bare");
            }
            args.push(target);
            let result = run_libra_command(&args, root.path());
            assert_eq!(result.status.code(), Some(128), "{target}: {result:?}");
            let (_human, report) = super::parse_cli_error_stderr(&result.stderr);
            assert_eq!(report.error_code, "LBR-REPO-002", "{target}: {result:?}");
            assert!(report.message.contains("not a directory"));
            for path in [".libra", "libra.db", "HEAD", "objects"] {
                assert!(
                    !root.path().join(path).exists(),
                    "{path} was created for invalid target {target}"
                );
            }
            assert_eq!(fs::read_to_string(&file).unwrap(), "user data\n");
        }
    }
}

#[test]
fn test_init_missing_parent_alias_uses_actual_storage_for_postcheck() {
    let root = tempdir().unwrap();
    let result = run_libra_command(
        &[
            "init",
            "--vault",
            "false",
            "--object-format",
            "sha256",
            "-q",
            "missing/..",
        ],
        root.path(),
    );
    assert_cli_success(&result, "normalized init target");
    assert!(root.path().join(".libra/libra.db").exists());
    assert!(!root.path().join("missing").exists());
    let status = run_libra_command(&["status", "--short"], root.path());
    assert_cli_success(&status, "status after normalized init target");
    let object_format = run_libra_command(
        &["config", "--local", "--get", "core.objectformat"],
        root.path(),
    );
    assert_cli_success(&object_format, "object format after normalized init target");
    assert_eq!(
        String::from_utf8_lossy(&object_format.stdout).trim(),
        "sha256"
    );
}

#[test]
fn test_init_template_inside_storage_is_refused_before_reinit_writes() {
    let root = tempdir().unwrap();
    let repo = root.path().join("repo");
    let target_arg = repo.to_string_lossy().into_owned();
    let first = run_libra_command(&["init", "--vault", "false", &target_arg], root.path());
    assert_cli_success(&first, "initial repository");

    let template = repo.join(".libra/template");
    fs::create_dir_all(template.join("template")).unwrap();
    fs::write(template.join("sibling.txt"), "sibling\n").unwrap();
    let template_arg = format!("--template={}", template.display());
    let result = run_libra_command(&["init", &template_arg, &target_arg], root.path());
    assert_eq!(result.status.code(), Some(129));
    let (_human, report) = super::parse_cli_error_stderr(&result.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        report
            .message
            .contains("inside the target storage directory")
    );
    assert!(
        !template.join("template/template").exists(),
        "rejected template must not start a recursive copy"
    );
}

#[cfg(unix)]
#[test]
fn test_init_bare_template_alias_with_missing_parent_keeps_existing_data() {
    let root = tempdir().unwrap();
    let template = root.path().join("template");
    fs::create_dir(&template).unwrap();
    let sentinel = template.join("must-survive.txt");
    fs::write(&sentinel, "owned data\n").unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(root.path(), &alias).unwrap();
    let target = alias.join("missing/../template");
    let template_arg = format!("--template={}", template.display());
    let target_arg = target.to_string_lossy().into_owned();

    let result = run_libra_command(
        &[
            "init",
            "--bare",
            "--vault",
            "false",
            &template_arg,
            &target_arg,
        ],
        root.path(),
    );
    assert_eq!(result.status.code(), Some(129));
    let (_human, report) = super::parse_cli_error_stderr(&result.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        sentinel.exists(),
        "the rejected target must remain untouched"
    );
    assert!(
        !root.path().join("missing").exists(),
        "the self-template refusal must happen before parent creation"
    );
}

#[cfg(unix)]
#[test]
fn test_init_template_rejects_symlinked_storage_before_copy() {
    let root = tempdir().unwrap();
    let template = root.path().join("source");
    fs::create_dir(&template).unwrap();
    fs::write(template.join("sibling.txt"), "sibling\n").unwrap();
    let repo = template.join("repo");
    let repo_arg = repo.to_string_lossy().into_owned();
    let first = run_libra_command(&["init", "--vault", "false", &repo_arg], root.path());
    assert_cli_success(&first, "initial repository");
    let store = template.join("store");
    fs::rename(repo.join(".libra"), &store).unwrap();
    std::os::unix::fs::symlink(&store, repo.join(".libra")).unwrap();
    let template_arg = format!("--template={}", template.display());

    let result = run_libra_command(&["init", &template_arg, &repo_arg], root.path());
    assert_eq!(result.status.code(), Some(128));
    let (_human, report) = super::parse_cli_error_stderr(&result.stderr);
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        store.join("libra.db").exists(),
        "existing storage is untouched"
    );
    assert!(
        !store.join("store").exists(),
        "storage must not copy itself"
    );
    assert!(!store.join("repo").exists(), "target must not be copied");
}

#[cfg(unix)]
#[test]
fn test_init_bare_layout_failure_does_not_delete_preexisting_target() {
    let root = tempdir().unwrap();
    let target = root.path().join("existing-bare");
    fs::create_dir(&target).unwrap();
    let sentinel = target.join("must-survive.txt");
    fs::write(&sentinel, "owned data\n").unwrap();
    let template = root.path().join("template");
    fs::create_dir(&template).unwrap();
    let directory = root.path().join("source-directory");
    fs::create_dir(&directory).unwrap();
    std::os::unix::fs::symlink(&directory, template.join("directory-link")).unwrap();
    let template_arg = format!("--template={}", template.display());
    let target_arg = target.to_string_lossy().into_owned();

    let result = run_libra_command(
        &[
            "init",
            "--bare",
            "--vault",
            "false",
            &template_arg,
            &target_arg,
        ],
        root.path(),
    );
    assert!(!result.status.success(), "directory symlink copy must fail");
    assert!(
        sentinel.exists(),
        "failed init must not delete an existing target"
    );
}

/// Count directories named `name` below `root` (not counting `root` itself) —
/// used to prove the target is never copied into its own storage root.
fn count_directories_named(root: &std::path::Path, name: &str) -> usize {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            if entry.file_name().to_string_lossy() == name {
                total += 1;
            }
            stack.push(entry.path());
        }
    }
    total
}

// ─────────────────────────────────────────────────────────────────────────────
// Issue #473 IN-04: --shared parsing and persistence (M-SPARSE P1..P8)
// ─────────────────────────────────────────────────────────────────────────────

/// IN-04 (M-SPARSE P1..P8): comprehensive matrix for --shared parsing,
/// persistence, legacy value compatibility, and cascading config.
#[test]
fn test_init_shared_mode_parse_matrix() {
    let root = tempdir().unwrap();

    // P1: `init --shared repo` without value -> group mode, repo directory parameter, persists "1".
    let p1_dir = root.path().join("p1_dir");
    fs::create_dir_all(&p1_dir).unwrap();
    let out = run_libra_command(
        &["init", "--vault", "false", "--shared", "repo_p1"],
        &p1_dir,
    );
    assert_cli_success(&out, "init --shared repo_p1");
    let repo_p1 = p1_dir.join("repo_p1");
    assert!(repo_p1.join(".libra/libra.db").exists());
    let cfg = run_libra_command(
        &["config", "--local", "--get", "core.sharedRepository"],
        &repo_p1,
    );
    assert_eq!(String::from_utf8_lossy(&cfg.stdout).trim(), "1");

    // P2: --shared=umask, --shared=false, --shared=0 -> no chmod, does not persist core.sharedRepository;
    // reinit removes existing value.
    for val in ["umask", "false", "0"] {
        let repo = root.path().join(format!("repo_p2_{val}"));
        fs::create_dir_all(&repo).unwrap();
        let out = run_libra_command(
            &["init", "--vault", "false", &format!("--shared={val}")],
            &repo,
        );
        assert_cli_success(&out, &format!("init --shared={val}"));
        let cfg = run_libra_command(
            &["config", "--local", "--get", "core.sharedRepository"],
            &repo,
        );
        assert_eq!(String::from_utf8_lossy(&cfg.stdout).trim(), "");

        // Seed core.sharedRepository=1, then reinit with --shared={val} -> removes it.
        let set = run_libra_command(&["config", "--local", "core.sharedRepository", "1"], &repo);
        assert_cli_success(&set, "seed core.sharedRepository");
        let reinit = run_libra_command(
            &["init", "--vault", "false", &format!("--shared={val}")],
            &repo,
        );
        assert_cli_success(&reinit, &format!("reinit --shared={val}"));
        let cfg2 = run_libra_command(
            &["config", "--local", "--get", "core.sharedRepository"],
            &repo,
        );
        assert_eq!(String::from_utf8_lossy(&cfg2.stdout).trim(), "");
    }

    // P3: group / true / 1 -> persists "1"; all / world / everybody / 2 -> persists "2"
    for val in ["group", "true", "1"] {
        let repo = root.path().join(format!("repo_p3_{val}"));
        fs::create_dir_all(&repo).unwrap();
        let out = run_libra_command(
            &["init", "--vault", "false", &format!("--shared={val}")],
            &repo,
        );
        assert_cli_success(&out, &format!("init --shared={val}"));
        let cfg = run_libra_command(
            &["config", "--local", "--get", "core.sharedRepository"],
            &repo,
        );
        assert_eq!(String::from_utf8_lossy(&cfg.stdout).trim(), "1");
    }
    for val in ["all", "world", "everybody", "2"] {
        let repo = root.path().join(format!("repo_p3_{val}"));
        fs::create_dir_all(&repo).unwrap();
        let out = run_libra_command(
            &["init", "--vault", "false", &format!("--shared={val}")],
            &repo,
        );
        assert_cli_success(&out, &format!("init --shared={val}"));
        let cfg = run_libra_command(
            &["config", "--local", "--get", "core.sharedRepository"],
            &repo,
        );
        assert_eq!(String::from_utf8_lossy(&cfg.stdout).trim(), "2");
    }

    // P4: 0660, 0640, 0600, 0664, 0775 -> saves masked 0660, 0640, 0600, 0664, 0664
    let octal_cases = [
        ("0660", "0660"),
        ("0640", "0640"),
        ("0600", "0600"),
        ("0664", "0664"),
        ("0775", "0664"),
    ];
    for (input, expected) in octal_cases {
        let repo = root.path().join(format!("repo_p4_{input}"));
        fs::create_dir_all(&repo).unwrap();
        let out = run_libra_command(
            &["init", "--vault", "false", &format!("--shared={input}")],
            &repo,
        );
        assert_cli_success(&out, &format!("init --shared={input}"));
        let cfg = run_libra_command(
            &["config", "--local", "--get", "core.sharedRepository"],
            &repo,
        );
        assert_eq!(String::from_utf8_lossy(&cfg.stdout).trim(), expected);
    }

    // P5: 0400, 0200 -> 129 LBR-CLI-002, file owner must have read and write permissions, zero writes
    for input in ["0400", "0200"] {
        let repo = root.path().join(format!("repo_p5_{input}"));
        let out = run_libra_command(
            &["init", "--vault", "false", &format!("--shared={input}")],
            &repo,
        );
        assert_eq!(out.status.code(), Some(129));
        let (_h, report) = super::parse_cli_error_stderr(&out.stderr);
        assert_eq!(report.error_code, "LBR-CLI-002");
        assert!(
            report
                .message
                .contains("file owner must have read and write permissions"),
            "expected owner permission message: {}",
            report.message
        );
        assert!(!repo.join(".libra").exists(), "zero write on refusal");
    }

    // P6: bogus -> 129 LBR-CLI-002
    let repo_bogus = root.path().join("repo_p6_bogus");
    let out = run_libra_command(&["init", "--vault", "false", "--shared=bogus"], &repo_bogus);
    assert_eq!(out.status.code(), Some(129));
    let (_h, report) = super::parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        !repo_bogus.join(".libra").exists(),
        "zero write on bogus shared mode"
    );

    // P7: Legacy values in core.sharedRepository (group, all, 0775) rewritten on reinit
    let legacy_cases = [("group", "1"), ("all", "2"), ("0775", "0664")];
    for (legacy_val, rewritten_val) in legacy_cases {
        let repo = root.path().join(format!("repo_p7_{legacy_val}"));
        fs::create_dir_all(&repo).unwrap();
        assert_cli_success(
            &run_libra_command(&["init", "--vault", "false"], &repo),
            "initial init",
        );
        assert_cli_success(
            &run_libra_command(
                &["config", "--local", "core.sharedRepository", legacy_val],
                &repo,
            ),
            "seed legacy config",
        );
        // Reinit without explicit --shared rewrites legacy value to Git format
        let reinit = run_libra_command(&["init", "--vault", "false"], &repo);
        assert_cli_success(&reinit, "reinit legacy value");
        let cfg = run_libra_command(
            &["config", "--local", "--get", "core.sharedRepository"],
            &repo,
        );
        assert_eq!(String::from_utf8_lossy(&cfg.stdout).trim(), rewritten_val);
    }

    // P8: cascaded global core.sharedRepository=group when --shared is omitted
    let p8_dir = root.path().join("p8_dir");
    fs::create_dir_all(&p8_dir).unwrap();
    assert_cli_success(
        &run_libra_command(
            &["config", "--global", "core.sharedRepository", "group"],
            &p8_dir,
        ),
        "set global core.sharedRepository",
    );
    let p8_repo = p8_dir.join("repo_p8");
    let out = run_libra_command(&["init", "--vault", "false", "repo_p8"], &p8_dir);
    assert_cli_success(&out, "init with cascaded global sharedRepository");
    let cfg = run_libra_command(
        &["config", "--local", "--get", "core.sharedRepository"],
        &p8_repo,
    );
    assert_eq!(String::from_utf8_lossy(&cfg.stdout).trim(), "1");
}

/// IN-04: Git t1301 test case: git init --shared=all persists 2.
#[test]
fn test_t1301_shared_all_persists_2() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let out = run_libra_command(&["init", "--vault", "false", "--shared=all"], &repo);
    assert_cli_success(&out, "init --shared=all");
    let cfg = run_libra_command(
        &["config", "--local", "--get", "core.sharedRepository"],
        &repo,
    );
    assert_eq!(String::from_utf8_lossy(&cfg.stdout).trim(), "2");
}

/// IN-04: Git t1301 test case: git init --shared=0400 fails because file owner must have read and write permissions.
#[test]
fn test_t1301_shared_0400_rejected() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let out = run_libra_command(&["init", "--vault", "false", "--shared=0400"], &repo);
    assert_eq!(out.status.code(), Some(129));
    let (_h, report) = super::parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        report
            .message
            .contains("file owner must have read and write permissions")
    );
    assert!(!repo.join(".libra").exists());
}
