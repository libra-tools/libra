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
        repo.join(".libraignore").exists(),
        "non-bare init should create a visible root .libraignore"
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
fn init_status_shows_root_libraignore_as_untracked() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    let output = run_libra_command(&["init", "--vault", "false"], &repo);
    assert_cli_success(&output, "init --vault false");

    let status = run_libra_command(&["status", "--short"], &repo);
    assert_cli_success(&status, "status --short");
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        stdout.contains("?? .libraignore"),
        "new repository should show .libraignore as an untracked project file, got: {stdout}"
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

    let second = run_libra_command(&["init", "--shared", "all"], &repo);
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
