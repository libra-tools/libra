//! Global config schema-newer guards for plan-20260708 P0-12.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use sea_orm::{ConnectionTrait, Statement};
use tempfile::{TempDir, tempdir};

const SECRET_VALUE: &str = "SECRET_SCHEMA_FUTURE_SHOULD_NOT_LEAK";
const ENV_SECRET_VALUE: &str = "ENV_STORAGE_SECRET_SHOULD_NOT_LEAK";
const INSTALL_COMMAND: &str =
    "curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh";

struct CliFixture {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    global_db: PathBuf,
    future_schema_version: i64,
    latest_schema_version: i64,
}

impl CliFixture {
    fn new() -> Self {
        let temp = tempdir().expect("create tempdir");
        let root = temp.path().to_path_buf();
        let home = root.join("home");
        let repo = root.join("repo");
        let global_db = home.join(".libra").join("config.db");
        fs::create_dir_all(&home).expect("create isolated home");
        let latest_schema_version = libra::internal::db::migration::latest_builtin_schema_version()
            .expect("read latest schema version")
            .expect("built-in migrations should have a latest schema version");
        Self {
            _temp: temp,
            root,
            home,
            repo,
            global_db,
            future_schema_version: latest_schema_version + 1,
            latest_schema_version,
        }
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let config_home = self.home.join(".config");
        fs::create_dir_all(&config_home).expect("create isolated config dir");
        fs::create_dir_all(self.global_db.parent().expect("global db parent"))
            .expect("create global config dir");

        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("LIBRA_CONFIG_GLOBAL_DB", &self.global_db)
            .env("LIBRA_TEST", "1")
            .env("LANG", "C")
            .env("LC_ALL", "C");
        if let Some(profile_file) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile_file);
        }
        command
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd, args).output().expect("spawn libra")
    }

    fn run_env(&self, cwd: &Path, args: &[&str], key: &str, value: &str) -> Output {
        self.command(cwd, args)
            .env(key, value)
            .output()
            .expect("spawn libra with env")
    }

    fn run_envs(&self, cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut command = self.command(cwd, args);
        for (key, value) in envs {
            command.env(key, value);
        }
        command.output().expect("spawn libra with envs")
    }

    fn run_with_complete_storage_env(&self, cwd: &Path, args: &[&str]) -> Output {
        self.run_envs(
            cwd,
            args,
            &[
                ("LIBRA_STORAGE_TYPE", "r2"),
                ("LIBRA_STORAGE_BUCKET", "schema-future-test"),
                ("LIBRA_STORAGE_ENDPOINT", "http://127.0.0.1:1"),
                ("LIBRA_STORAGE_REGION", "auto"),
                ("LIBRA_STORAGE_ACCESS_KEY", "test-access-key"),
                ("LIBRA_STORAGE_SECRET_KEY", ENV_SECRET_VALUE),
                ("LIBRA_STORAGE_ALLOW_HTTP", "true"),
                ("LIBRA_STORAGE_THRESHOLD", "1048576"),
                ("LIBRA_STORAGE_CACHE_SIZE", "2097152"),
            ],
        )
    }

    fn write_complete_local_storage_config(&self) {
        for (key, value) in [
            ("vault.env.LIBRA_STORAGE_TYPE", "r2"),
            ("vault.env.LIBRA_STORAGE_BUCKET", "schema-future-test"),
            ("vault.env.LIBRA_STORAGE_ENDPOINT", "http://127.0.0.1:1"),
            ("vault.env.LIBRA_STORAGE_REGION", "auto"),
            ("vault.env.LIBRA_STORAGE_ACCESS_KEY", "test-access-key"),
            ("vault.env.LIBRA_STORAGE_SECRET_KEY", ENV_SECRET_VALUE),
            ("vault.env.LIBRA_STORAGE_ALLOW_HTTP", "true"),
            ("vault.env.LIBRA_STORAGE_THRESHOLD", "1048576"),
            ("vault.env.LIBRA_STORAGE_CACHE_SIZE", "2097152"),
        ] {
            self.success(&self.repo, &["config", "set", key, value]);
        }
    }

    fn success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.run(cwd, args);
        assert_success(args, &output);
        output
    }

    fn init_repo(&self) {
        fs::create_dir_all(&self.repo).expect("create repo dir");
        self.success(
            &self.root,
            &[
                "init",
                "--vault",
                "false",
                self.repo.to_str().expect("utf8 repo"),
            ],
        );
    }

    fn write_future_global_config(&self) {
        if self.global_db.exists() {
            fs::remove_file(&self.global_db).expect("remove previous global config db");
        }
        fs::create_dir_all(self.global_db.parent().expect("global db parent"))
            .expect("create global config dir");
        let db_path = self.global_db.to_str().expect("utf8 global db");
        let runtime = tokio::runtime::Runtime::new().expect("create tokio runtime");
        runtime.block_on(async {
            let conn = libra::internal::db::create_database(db_path)
                .await
                .expect("create global config db");
            let backend = conn.get_database_backend();
            conn.execute(Statement::from_sql_and_values(
                backend,
                "DELETE FROM schema_versions",
                [],
            ))
            .await
            .expect("clear schema versions");
            conn.execute(Statement::from_sql_and_values(
                backend,
                "INSERT INTO schema_versions (version, name, applied_at) VALUES (?, ?, ?)",
                [
                    self.future_schema_version.into(),
                    "future_schema_for_test".into(),
                    "2026-07-09T00:00:00Z".into(),
                ],
            ))
            .await
            .expect("insert future schema version");
            conn.execute(Statement::from_sql_and_values(
                backend,
                "INSERT INTO config_kv (`key`, `value`, `encrypted`) VALUES (?, ?, 0)",
                [
                    "vault.env.LIBRA_STORAGE_SECRET_KEY".into(),
                    SECRET_VALUE.into(),
                    0.into(),
                ],
            ))
            .await
            .expect("insert secret-like value");
            conn.close().await.expect("close global config db");
        });
    }
}

fn assert_success(args: &[&str], output: &Output) {
    assert!(
        output.status.success(),
        "{} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_not_success(args: &[&str], output: &Output) {
    assert!(
        !output.status.success(),
        "{} unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is utf8")
}

fn assert_schema_future_diagnostic(fixture: &CliFixture, stderr: &str) {
    assert!(
        stderr.contains("LBR-CONFIG-001") || stderr.contains("warning:"),
        "expected config error code or warning, got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("global config database schema is newer"),
        "missing global config schema diagnostic:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!("version: {}", env!("CARGO_PKG_VERSION"))),
        "missing binary version:\n{stderr}"
    );
    assert!(
        stderr.contains(&fixture.global_db.display().to_string()),
        "missing global config db path:\n{stderr}"
    );
    assert!(
        stderr.contains(&fixture.future_schema_version.to_string()),
        "missing future schema version:\n{stderr}"
    );
    assert!(
        stderr.contains(&fixture.latest_schema_version.to_string()),
        "missing latest supported schema version:\n{stderr}"
    );
    assert!(
        stderr.contains(INSTALL_COMMAND),
        "missing install command:\n{stderr}"
    );
    assert!(
        !stderr.contains(SECRET_VALUE),
        "diagnostic leaked secret-like config value:\n{stderr}"
    );
}

#[test]
fn pull_fails_closed_when_global_config_schema_is_future() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.write_future_global_config();

    let output = fixture.run(&fixture.repo, &["pull"]);

    assert_not_success(&["pull"], &output);
    let stderr = stderr_text(&output);
    assert_schema_future_diagnostic(&fixture, &stderr);
    assert!(
        stderr.contains("`libra pull` requires global storage config"),
        "missing fail-closed command context:\n{stderr}"
    );
    assert!(
        stderr.contains("use --offline or LIBRA_READ_POLICY=offline/local"),
        "missing explicit downgrade escape hatch:\n{stderr}"
    );
}

#[test]
fn remote_and_cloud_commands_fail_closed_when_global_config_schema_is_future() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.write_future_global_config();

    let clone_source = "https://example.invalid/schema-future.git";
    let cases = [
        ("fetch", fixture.repo.as_path(), vec!["fetch"]),
        ("push", fixture.repo.as_path(), vec!["push"]),
        ("cloud", fixture.repo.as_path(), vec!["cloud", "status"]),
        (
            "clone",
            fixture.root.as_path(),
            vec!["clone", clone_source, "copy"],
        ),
    ];

    for (command_name, cwd, args) in cases {
        let output = fixture.run(cwd, &args);
        assert_not_success(&args, &output);
        let stderr = stderr_text(&output);
        assert_schema_future_diagnostic(&fixture, &stderr);
        assert!(
            stderr.contains(&format!(
                "`libra {command_name}` requires global storage config"
            )),
            "missing fail-closed context for {command_name}:\n{stderr}"
        );
    }
}

#[test]
fn offline_policy_warns_and_allows_pull_to_reach_command() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.write_future_global_config();

    let output = fixture.run(&fixture.repo, &["--offline", "pull"]);
    let stderr = stderr_text(&output);

    assert!(
        !stderr.contains("LBR-CONFIG-001"),
        "--offline should not fail at global config schema guard:\n{stderr}"
    );
    assert_schema_future_diagnostic(&fixture, &stderr);
}

#[test]
fn env_offline_policy_warns_and_allows_pull_to_reach_command() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.write_future_global_config();

    let output = fixture.run_env(&fixture.repo, &["pull"], "LIBRA_READ_POLICY", "offline");
    let stderr = stderr_text(&output);

    assert!(
        !stderr.contains("LBR-CONFIG-001"),
        "LIBRA_READ_POLICY=offline should not fail at schema guard:\n{stderr}"
    );
    assert_schema_future_diagnostic(&fixture, &stderr);
}

#[test]
fn env_local_policy_warns_and_allows_pull_to_reach_command() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.write_future_global_config();

    let output = fixture.run_env(&fixture.repo, &["pull"], "LIBRA_READ_POLICY", "local");
    let stderr = stderr_text(&output);

    assert!(
        !stderr.contains("LBR-CONFIG-001"),
        "LIBRA_READ_POLICY=local should not fail at schema guard:\n{stderr}"
    );
    assert_schema_future_diagnostic(&fixture, &stderr);
}

#[test]
fn complete_process_env_storage_config_does_not_fail_schema_guard() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.write_future_global_config();

    let output = fixture.run_with_complete_storage_env(&fixture.repo, &["pull"]);
    let stderr = stderr_text(&output);

    assert!(
        !stderr.contains("LBR-CONFIG-001"),
        "complete LIBRA_STORAGE_* env should make global config unnecessary:\n{stderr}"
    );
    assert!(
        stderr.contains(
            "process or repo-local configuration makes global storage config unnecessary"
        ),
        "missing env-override diagnostic:\n{stderr}"
    );
    assert_schema_future_diagnostic(&fixture, &stderr);
    assert!(
        !stderr.contains(ENV_SECRET_VALUE),
        "diagnostic leaked process env storage secret:\n{stderr}"
    );
}

#[test]
fn complete_repo_local_storage_config_does_not_fail_schema_guard() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.write_complete_local_storage_config();
    fixture.write_future_global_config();

    let output = fixture.run(&fixture.repo, &["pull"]);
    let stderr = stderr_text(&output);

    assert!(
        !stderr.contains("LBR-CONFIG-001"),
        "complete repo-local vault.env.LIBRA_STORAGE_* config should make global config unnecessary:\n{stderr}"
    );
    assert!(
        stderr.contains(
            "process or repo-local configuration makes global storage config unnecessary"
        ),
        "missing local-config override diagnostic:\n{stderr}"
    );
    assert_schema_future_diagnostic(&fixture, &stderr);
    assert!(
        !stderr.contains(ENV_SECRET_VALUE),
        "diagnostic leaked repo-local storage secret:\n{stderr}"
    );
}

#[test]
fn cloud_storage_env_still_fails_when_d1_config_would_read_global() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.write_future_global_config();

    let output = fixture.run_with_complete_storage_env(&fixture.repo, &["cloud", "status"]);
    let stderr = stderr_text(&output);

    assert_not_success(&["cloud", "status"], &output);
    assert_schema_future_diagnostic(&fixture, &stderr);
    assert!(
        stderr.contains("`libra cloud` requires global storage config"),
        "cloud must fail closed when D1 config would fall through to global:\n{stderr}"
    );
    assert!(
        !stderr.contains(ENV_SECRET_VALUE),
        "diagnostic leaked process env storage secret:\n{stderr}"
    );
}

#[test]
fn local_command_warns_once_and_continues() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.write_future_global_config();

    let output = fixture.success(&fixture.repo, &["status", "--short"]);
    let stderr = stderr_text(&output);

    assert_schema_future_diagnostic(&fixture, &stderr);
    let count = stderr
        .matches("global config database schema is newer")
        .count();
    assert_eq!(count, 1, "schema warning should be deduplicated:\n{stderr}");
}

#[test]
fn json_error_reports_config_schema_future_details() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.write_future_global_config();

    let output = fixture.run(&fixture.repo, &["--json", "pull"]);

    assert_not_success(&["--json", "pull"], &output);
    assert!(
        output.stdout.is_empty(),
        "JSON errors must stay on stderr, got stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = stderr_text(&output);
    assert!(
        !stderr.contains(SECRET_VALUE),
        "JSON diagnostic leaked secret-like config value:\n{stderr}"
    );
    let payload: serde_json::Value =
        serde_json::from_str(&stderr).expect("stderr should be a JSON error envelope");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error_code"], "LBR-CONFIG-001");
    assert_eq!(payload["category"], "config");
    assert_eq!(payload["exit_code"], 128);
    assert_eq!(payload["details"]["command"], "pull");
    assert_eq!(
        payload["details"]["config_database"],
        fixture.global_db.display().to_string()
    );
    assert_eq!(
        payload["details"]["config_schema_version"],
        fixture.future_schema_version
    );
    assert_eq!(
        payload["details"]["latest_supported_schema_version"],
        fixture.latest_schema_version.to_string()
    );
    assert_eq!(payload["details"]["install_command"], INSTALL_COMMAND);
}
