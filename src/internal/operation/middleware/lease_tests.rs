//! Supervised regressions for the production scope-lease boundary.
#![cfg(test)]

use std::{
    fs,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use super::{MutationClass, OperationError, OperationMetaV2, ScopeLease, run_with_operation};
use crate::internal::operation::PinnedRequestScope;

#[path = "tests/lease_tests/cases.rs"]
mod cases;
#[path = "tests/lease_tests/operations.rs"]
mod operations;
#[path = "tests/lease_tests/support.rs"]
mod support;

const CHILD_ENV: &str = "LIBRA_TEST_SCOPE_LEASE_CHILD";
const READY_ENV: &str = "LIBRA_TEST_SCOPE_LEASE_READY";
const CHILD_TEST: &str = "internal::operation::middleware::lease_tests::supervised_lease_child";

struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn run_case(name: &str, hold_external_lock: bool) {
    // Given an isolated process and, where requested, a lock owned by its parent.
    let directory = tempfile::tempdir().expect("lease sandbox");
    let root = directory.path().canonicalize().expect("canonical sandbox");
    let repo = root.join("main");
    let info = repo.join(".libra/info");
    fs::create_dir_all(&info).expect("lease directory");
    let held = hold_external_lock.then(|| {
        let file = fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(info.join("operation-v2.lock"))
            .expect("parent lock file");
        file.try_lock()
            .expect("parent exclusively owns fixture lock");
        file
    });
    let stdout_path = root.join("stdout");
    let stderr_path = root.join("stderr");
    let ready_path = root.join("ready");
    let mut command = Command::new(std::env::current_exe().expect("libtest executable"));
    command
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env_clear()
        .env(CHILD_ENV, name)
        .env(READY_ENV, &ready_path)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LIBRA_HOME", root.join("libra-home"))
        .env("XDG_CONFIG_HOME", root.join("home/.config"))
        .env("LIBRA_CONFIG_GLOBAL_DB", root.join("home/global.db"))
        .env("LIBRA_CONFIG_SYSTEM_DB", root.join("home/system.db"))
        .env("LIBRA_TEST", "1")
        .env("RUST_MIN_STACK", "16777216")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .current_dir(&repo)
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stdout_path).expect("stdout"))
        .stderr(fs::File::create(&stderr_path).expect("stderr"));
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    let mut child = ChildGuard(Some(command.spawn().expect("supervised lease child")));

    // When the existing production boundary runs, only this child may block.
    let startup_deadline = Instant::now() + Duration::from_secs(30);
    let mut execution_deadline = None;
    let status = loop {
        if execution_deadline.is_none() && ready_path.exists() {
            execution_deadline = Some(Instant::now() + Duration::from_secs(5));
        }
        let running = child.0.as_mut().expect("owned child");
        if let Some(status) = running.try_wait().expect("poll child") {
            break status;
        }
        if Instant::now() >= execution_deadline.unwrap_or(startup_deadline) {
            running.kill().expect("kill stalled child");
            running.wait().expect("reap stalled child");
            child.0.take();
            panic!(
                "{name} exceeded startup/5-second execution watchdog; ready={}\nstdout:\n{}\nstderr:\n{}",
                ready_path.exists(),
                fs::read_to_string(&stdout_path).expect("stdout"),
                fs::read_to_string(&stderr_path).expect("stderr"),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    child.0.take();
    drop(held);
    let stdout = fs::read_to_string(stdout_path).expect("stdout");
    let stderr = fs::read_to_string(stderr_path).expect("stderr");
    // Then execution, not a zero-test filter or fixture-only exit, must have succeeded.
    assert!(
        status.success(),
        "{name}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        ready_path.exists(),
        "fixture never reached the tested boundary"
    );
    assert!(stdout.contains("running 1 test"), "{stdout}");
    assert!(
        stdout.contains(&format!("RESULT:{name}:PASS")),
        "{stdout}\n{stderr}"
    );
}

#[test]
fn supervised_lease_child() {
    let Ok(name) = std::env::var(CHILD_ENV) else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(if name == "cancelled_waiter" { 1 } else { 16 })
        .build()
        .expect("child runtime");
    runtime.block_on(cases::run(&name));
    drop(runtime);
    println!("RESULT:{name}:PASS");
}

#[test]
fn same_scope_second_operation_refuses_before_callback() {
    run_case("same_scope", false);
}

#[test]
fn linked_scope_operation_runs_while_main_scope_is_held() {
    run_case("different_scope", false);
}

#[test]
fn another_process_holding_scope_lock_is_a_prompt_refusal() {
    run_case("external_holder", true);
}

#[test]
fn cancelled_scope_waiter_does_not_strand_blocking_work() {
    run_case("cancelled_waiter", true);
}

#[test]
fn cancelling_a_started_operation_releases_its_scope_lease() {
    run_case("cancelled_operation", false);
}

#[test]
fn released_scope_lease_can_be_acquired_again() {
    run_case("released", false);
}

#[test]
fn lease_open_error_preserves_resource_and_os_cause() {
    run_case("open_error", false);
}

#[test]
fn acquiring_a_lease_does_not_append_to_existing_lock_contents() {
    run_case("unchanged_contents", false);
}

#[cfg(unix)]
#[test]
fn operation_scope_lock_honors_shared_group_permissions() {
    run_case("shared_group_permissions", false);
}

#[cfg(unix)]
#[test]
fn symlink_lock_leaf_is_rejected_without_writing_its_target() {
    run_case("symlink_leaf", false);
}

#[cfg(unix)]
#[test]
fn symlink_info_directory_is_rejected_without_creating_external_lock() {
    run_case("symlink_parent", false);
}

#[cfg(unix)]
#[test]
fn fifo_lock_leaf_is_rejected_without_waiting_for_a_reader() {
    run_case("fifo_leaf", false);
}
