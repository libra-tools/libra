//! Isolated synthetic producer-format fixtures and bounded CLI children.
//! No ambient HOME/configuration or external binary downloads are consulted.
#![allow(dead_code)]

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};

pub const CANARY: &str = "MIG06_SYNTHETIC_SECRET_MUST_NOT_LEAK";

#[cfg(unix)]
fn set_private_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(not(unix))]
fn set_private_mode(_path: &Path, _mode: u32) {}

pub struct RepairFixture {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
    pub db: PathBuf,
    pub system: PathBuf,
}

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

pub async fn connect(path: &Path, read_only: bool) -> DatabaseConnection {
    let filename = path.to_path_buf();
    let mut options = ConnectOptions::new("sqlite://isolated-repair-fixture");
    options.sqlx_logging(false).max_connections(1);
    options.map_sqlx_sqlite_pool_opts(|pool| pool.idle_timeout(None).max_lifetime(None));
    options.map_sqlx_sqlite_opts(move |opts| {
        opts.filename(&filename)
            .create_if_missing(false)
            .read_only(read_only)
            .busy_timeout(Duration::from_millis(200))
    });
    Database::connect(options).await.unwrap()
}

impl RepairFixture {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        set_private_mode(&root, 0o700);
        let home = root.join("home");
        fs::create_dir_all(home.join(".libra")).unwrap();
        fs::create_dir_all(home.join(".config")).unwrap();
        // The repair path intentionally verifies private Unix permissions.
        // tempfile's directory is private, but its children inherit the
        // process umask and are not a stable fixture contract under CI.
        set_private_mode(&home, 0o700);
        set_private_mode(&home.join(".libra"), 0o700);
        set_private_mode(&home.join(".config"), 0o700);
        let db = home.join(".libra/config.db");
        let system = root.join("system.db");
        fs::write(&db, b"").unwrap();
        set_private_mode(&db, 0o600);
        runtime().block_on(async {
            let conn = connect(&db, false).await;
            conn.execute_unprepared(include_str!(
                "../fixtures/config_repair/v0.22.19-global-config.sql"
            ))
            .await
            .unwrap();
            conn.execute_raw(Statement::from_sql_and_values(conn.get_database_backend(),
                "INSERT INTO config_kv (key,value,encrypted) VALUES ('test.repair','preserved',0),('test.secret',?,1)",
                [CANARY.into()])).await.unwrap();
            conn.execute_unprepared("INSERT INTO config (configuration,name,key,value) VALUES ('legacy',NULL,'kept','legacy-preserved'); INSERT INTO config_kv (id,key,value) VALUES (10000,'deleted','tombstone'); DELETE FROM config_kv WHERE id=10000;")
                .await.unwrap();
            conn.close().await.unwrap();
        });
        fs::write(&system, b"system database must remain untouched").unwrap();
        Self {
            _temp: temp,
            root,
            db,
            system,
        }
    }

    pub fn command(&self, args: &[&str]) -> Command {
        self.command_for(Path::new(env!("CARGO_BIN_EXE_libra")), args)
    }

    pub fn command_for(&self, binary: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(binary);
        command
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", self.root.join("home"))
            .env("USERPROFILE", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home/.config"))
            .env("LIBRA_CONFIG_GLOBAL_DB", &self.db)
            .env("LIBRA_CONFIG_SYSTEM_DB", &self.system)
            .env("LIBRA_TEST", "1");
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        command
    }

    pub fn repair_command(&self) -> Command {
        self.command(&[
            "--json",
            "config",
            "doctor",
            "--global-schema",
            "--repair",
            "--confirm",
            self.db.to_str().unwrap(),
        ])
    }

    pub fn repair(&self) -> Output {
        for ending in ["-wal", "-shm", "-journal"] {
            let sidecar = suffix(&self.db, ending);
            if sidecar.exists() {
                set_private_mode(&sidecar, 0o600);
            }
        }
        run(self.repair_command())
    }

    pub fn backups(&self) -> Vec<PathBuf> {
        let mut paths: Vec<_> = fs::read_dir(self.db.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".libra-config-repair-")
            })
            .collect();
        paths.sort();
        paths
    }

    pub fn checkpoint_command(&self, stage: &str) -> (Command, PathBuf) {
        let checkpoint = self.root.join("checkpoint");
        let mut command = self.repair_command();
        command
            .env("LIBRA_TEST_CONFIG_REPAIR_PAUSE_AT", stage)
            .env("LIBRA_TEST_CONFIG_REPAIR_CHECKPOINT", &checkpoint);
        (command, checkpoint)
    }
}

pub fn suffix(path: &Path, ending: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(ending);
    PathBuf::from(name)
}

pub struct ChildGuard(Option<Child>);

impl ChildGuard {
    pub fn spawn(mut command: Command) -> Self {
        Self(Some(
            command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ))
    }

    pub fn wait_checkpoint(&mut self, path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !suffix(path, ".ready").is_file() {
            assert!(
                self.0.as_mut().unwrap().try_wait().unwrap().is_none(),
                "child exited before checkpoint"
            );
            assert!(
                Instant::now() < deadline,
                "checkpoint exceeded 20s estimate"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn finish(mut self) -> Output {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if self.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                return self.0.take().unwrap().wait_with_output().unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "CLI test exceeded 30s estimate; child is killed on drop"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn run(command: Command) -> Output {
    ChildGuard::spawn(command).finish()
}

pub fn resume(checkpoint: &Path, success: bool) {
    fs::write(
        suffix(checkpoint, ".continue"),
        if success {
            b"continue".as_slice()
        } else {
            b"fail".as_slice()
        },
    )
    .unwrap();
}

pub fn assert_secret_free(output: &Output) {
    assert!(!String::from_utf8_lossy(&output.stdout).contains(CANARY));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(CANARY));
}

pub fn data(output: &Output) -> serde_json::Value {
    assert_secret_free(output);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["data"].clone()
}

pub fn execute_sql(path: &Path, sql: &str) {
    runtime().block_on(async {
        let conn = connect(path, false).await;
        conn.execute_unprepared(sql).await.unwrap();
        conn.close().await.unwrap();
    });
}

pub fn integer(path: &Path, sql: &str) -> i64 {
    runtime().block_on(async {
        let conn = connect(path, true).await;
        let value = conn
            .query_one_raw(Statement::from_string(conn.get_database_backend(), sql))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap();
        conn.close().await.unwrap();
        value
    })
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Complete logical rows of our own synthetic database, including sequence
/// high-water marks. Never used with a user database or ambient path.
pub fn rowsets(path: &Path) -> BTreeMap<String, String> {
    runtime().block_on(async {
        let conn = connect(path, true).await;
        let tables = conn.query_all_raw(Statement::from_string(conn.get_database_backend(),
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")).await.unwrap();
        let mut result = BTreeMap::new();
        for table in tables {
            let name: String = table.try_get_by_index(0).unwrap();
            let columns = conn.query_all_raw(Statement::from_string(conn.get_database_backend(),
                format!("PRAGMA table_info({})", quote(&name)))).await.unwrap();
            let columns: Vec<String> = columns.into_iter().map(|column| quote(&column.try_get_by_index::<String>(1).unwrap())).collect();
            let list = columns.join(",");
            let sql = format!("SELECT json_group_array(json_array({list})) FROM (SELECT * FROM {} ORDER BY {list})", quote(&name));
            let rows: String = conn.query_one_raw(Statement::from_string(conn.get_database_backend(), sql))
                .await.unwrap().unwrap().try_get_by_index(0).unwrap();
            result.insert(name, rows);
        }
        conn.close().await.unwrap();
        result
    })
}
