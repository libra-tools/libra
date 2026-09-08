//! Ignore failures must not authorize snapshot payload persistence.

use std::{
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use sea_orm::{ActiveModelTrait, ActiveValue::Set};

use super::{Completeness, Index, PinnedRequestScope, WorkspaceSnapshotter};
use crate::{
    internal::{
        db::create_database, model::reference, operation::WorkspaceStatePointer,
        worktree_scope::WorktreeScope,
    },
    utils::client_storage::ClientStorage,
};

#[path = "tests/ignore_tests/cases.rs"]
mod cases;
#[path = "tests/ignore_tests/epoch.rs"]
mod epoch;
#[path = "tests/ignore_tests/listing.rs"]
mod listing;
#[path = "tests/ignore_tests/policy.rs"]
mod policy;

const CHILD_ENV: &str = "LIBRA_TEST_SNAPSHOT_IGNORE_CHILD";
const CHILD_TEST: &str = "internal::operation::snapshot::ignore_tests::supervised_ignore_child";
const READY_ENV: &str = "LIBRA_TEST_SNAPSHOT_IGNORE_READY";

struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Fixture {
    snapshotter: WorkspaceSnapshotter,
    storage: ClientStorage,
}

impl Fixture {
    fn open() -> Self {
        let root = std::env::current_dir().expect("isolated child worktree");
        let storage = root.join(".libra");
        let scope = PinnedRequestScope {
            scope: WorktreeScope::Main,
            workdir: root.clone(),
            worktree_root: root,
            gitdir: storage.clone(),
            storage: storage.clone(),
        };
        let pointer = WorkspaceStatePointer::new("ignore-fixture", listing::blob_oid(b"seed"), 0);
        Self {
            snapshotter: WorkspaceSnapshotter::new(scope, pointer).with_io(std::sync::Arc::new(
                crate::internal::worktree_io::executor::WorktreeIo::with_test_handler(
                    listing::ordered_listing,
                ),
            )),
            storage: ClientStorage::init_local(storage.join("objects")),
        }
    }
}

fn run_case(name: &str) {
    // Given: setup happens before the child deadline and never changes parent CWD/env.
    let directory = tempfile::tempdir().expect("isolated child directory");
    let root = directory
        .path()
        .canonicalize()
        .expect("canonical test root");
    let repo = root.join("repo");
    let home = root.join("home");
    fs::create_dir_all(repo.join(".libra")).expect("child gitdir");
    fs::create_dir_all(&home).expect("isolated home");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("fixture runtime");
    runtime.block_on(async {
        let connection = create_database(
            repo.join(".libra/libra.db")
                .to_str()
                .expect("database path"),
        )
        .await
        .expect("repository schema");
        reference::ActiveModel {
            name: Set(Some("main".to_string())),
            kind: Set(reference::ConfigKind::Head),
            commit: Set(None),
            remote: Set(None),
            worktree_id: Set(None),
            ..Default::default()
        }
        .insert(&connection)
        .await
        .expect("main unborn HEAD");
        connection.close().await.expect("close setup connection");
    });
    fs::write(repo.join(".libra/HEAD"), b"ref: refs/heads/main\n").expect("HEAD");
    Index::new()
        .save(repo.join(".libra/index"))
        .expect("empty valid index");
    let stdout_path = root.join("stdout");
    let stderr_path = root.join("stderr");
    let ready_path = root.join("ready");
    let stdout = fs::File::create(&stdout_path).expect("stdout log");
    let stderr = fs::File::create(&stderr_path).expect("stderr log");
    let executable = std::env::current_exe().expect("libtest executable");
    let child = Command::new(executable)
        .arg("--exact")
        .arg(CHILD_TEST)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env_clear()
        .env(CHILD_ENV, name)
        .env(READY_ENV, &ready_path)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LIBRA_HOME", root.join("libra-home"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("LIBRA_CONFIG_GLOBAL_DB", home.join("global.db"))
        .env("LIBRA_CONFIG_SYSTEM_DB", home.join("system.db"))
        .env("LIBRA_TEST", "1")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .current_dir(&repo)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .expect("spawn exact child test");
    let mut child = ChildGuard(Some(child));

    // When: potentially blocking ignore reads execute only in the supervised child.
    let startup_deadline = Instant::now() + Duration::from_secs(30);
    let mut execution_deadline = None;
    let status = loop {
        if execution_deadline.is_none() && ready_path.exists() {
            execution_deadline = Some(Instant::now() + Duration::from_secs(5));
        }
        let deadline = execution_deadline.unwrap_or(startup_deadline);
        let running = child.0.as_mut().expect("owned child");
        if let Some(status) = running.try_wait().expect("poll child") {
            break status;
        }
        if Instant::now() >= deadline {
            running.kill().expect("kill stalled child");
            running.wait().expect("reap stalled child");
            child.0.take();
            panic!(
                "{name} exceeded startup/5-second execution deadline\nstdout:\n{}\nstderr:\n{}",
                fs::read_to_string(&stdout_path).expect("stdout"),
                fs::read_to_string(&stderr_path).expect("stderr"),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    child.0.take();
    let stdout = fs::read_to_string(stdout_path).expect("stdout");
    let stderr = fs::read_to_string(stderr_path).expect("stderr");

    // Then: a missing/incorrect exact filter cannot pass as zero executed tests.
    assert!(
        status.success(),
        "{name}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("running 1 test"),
        "wrong exact filter: {stdout}"
    );
    assert!(
        stdout.contains(&format!("RESULT:{name}:PASS")),
        "{stdout}\n{stderr}"
    );
    assert!(
        ready_path.exists(),
        "child never entered the behavior under test"
    );
}

#[test]
fn supervised_ignore_child() {
    let Ok(name) = std::env::var(CHILD_ENV) else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    runtime.block_on(cases::run(&name));
    println!("RESULT:{name}:PASS");
}

#[test]
fn ordinary_ignore_rules_preserve_tracked_paths() {
    run_case("ordinary_ignore_rules_preserve_tracked_paths");
}

#[test]
fn invalid_utf8_per_directory_ignore_stays_partial_until_repaired() {
    run_case("invalid_utf8_per_directory_ignore_stays_partial_until_repaired");
}

#[test]
fn invalid_utf8_core_excludes_file_stays_partial_until_repaired() {
    run_case("invalid_utf8_core_excludes_file_stays_partial_until_repaired");
}

#[test]
fn listing_consumes_original_deadline_before_ignore_lookup() {
    run_case("listing_consumes_original_deadline_before_ignore_lookup");
}

#[cfg(unix)]
#[test]
fn fifo_per_directory_ignore_returns_partial_within_capture_budget() {
    run_case("fifo_per_directory");
}

#[cfg(unix)]
#[test]
fn fifo_core_excludes_file_returns_partial_within_capture_budget() {
    run_case("fifo_configured");
}
