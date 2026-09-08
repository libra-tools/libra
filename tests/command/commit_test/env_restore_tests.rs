//! Supervised regressions for the original commit identity tests' environment lifetime.

use std::{
    ffi::OsString,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use libra::utils::test::ScopedEnvVar;

const CHILD_CASE: &str = "LIBRA_COMMIT_IDENTITY_RESTORE_CHILD";
const CHILD_READY: &str = "LIBRA_COMMIT_IDENTITY_RESTORE_READY";
const CHILD_RETURNED: &str = "LIBRA_COMMIT_IDENTITY_RESTORE_RETURNED";
const CHILD_TEST: &str =
    "command::commit_test::env_restore_tests::identity_tests_preserve_inherited_environment";
const GUARDED_KEYS: [&str; 10] = [
    "LIBRA_CONFIG_GLOBAL_DB",
    "LIBRA_CONFIG_SYSTEM_DB",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "EMAIL",
    "LIBRA_COMMITTER_NAME",
    "LIBRA_COMMITTER_EMAIL",
    "LIBRA_HOME",
];

struct ReapedChild(Child);

impl Drop for ReapedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run_child_case(case: &str) {
    let inherited: Vec<(&str, OsString)> = GUARDED_KEYS
        .into_iter()
        .map(|key| {
            (
                key,
                std::env::var_os(key).expect("supervisor supplies an isolated sentinel"),
            )
        })
        .collect();
    let _restore: Vec<ScopedEnvVar> = inherited
        .iter()
        .map(|(key, value)| ScopedEnvVar::set(*key, value))
        .collect();
    // Tokio's test macro supplies synchronous entries. Each original wrapper
    // owns its serial(env, cwd) locks; do not add another runtime or serial lock.
    match case {
        "strict" => super::test_commit_requires_configured_identity_in_strict_mode(),
        "default" => super::test_commit_without_identity_fails_by_default(),
        "amend" => super::test_commit_amend_preserves_author_unless_reset(),
        _ => panic!("unknown supervised identity test"),
    }
    std::fs::write(
        std::env::var_os(CHILD_RETURNED).expect("supervisor supplies returned path"),
        b"returned",
    )
    .expect("record that the original test returned");
    // A buggy test has removed the override by now. Until the guards restore it,
    // only inspect process environment: no configuration or database queries.
    let observed: Vec<(&str, Option<OsString>)> = inherited
        .iter()
        .map(|(key, _)| (*key, std::env::var_os(key)))
        .collect();
    let expected: Vec<(&str, Option<OsString>)> = inherited
        .into_iter()
        .map(|(key, value)| (key, Some(value)))
        .collect();
    assert_eq!(
        observed, expected,
        "{case} identity test must preserve the caller's exact environment"
    );
}

#[test]
fn identity_tests_preserve_inherited_environment() {
    if let Some(case) = std::env::var_os(CHILD_CASE) {
        std::fs::write(
            std::env::var_os(CHILD_READY).expect("supervisor supplies ready path"),
            b"ready",
        )
        .expect("record exact child entry");
        run_child_case(case.to_str().expect("supervised case is UTF-8"));
        return;
    }
    let mut failed = Vec::new();
    for case in ["strict", "default", "amend"] {
        let isolated = tempfile::tempdir().expect("supervised identity sandbox");
        let root = isolated.path();
        let libra_home = root.join("caller-libra-home");
        let ready = root.join("child-ready");
        let returned = root.join("original-returned");
        std::fs::create_dir_all(&libra_home).expect("isolated Libra home");
        let child = Command::new(std::env::current_exe().expect("command-test executable"))
            .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
            .env_clear()
            .env(CHILD_CASE, case)
            .env(CHILD_READY, &ready)
            .env(CHILD_RETURNED, &returned)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("LIBRA_HOME", &libra_home)
            .env("LIBRA_CONFIG_GLOBAL_DB", root.join("caller-global.db"))
            .env("LIBRA_CONFIG_SYSTEM_DB", root.join("caller-system.db"))
            .env("GIT_COMMITTER_NAME", "Inherited Committer")
            .env("GIT_COMMITTER_EMAIL", "inherited-committer@example.invalid")
            .env("GIT_AUTHOR_NAME", "Inherited Author")
            .env("GIT_AUTHOR_EMAIL", "inherited-author@example.invalid")
            .env("EMAIL", "inherited-email@example.invalid")
            .env("LIBRA_COMMITTER_NAME", "Inherited Libra Committer")
            .env("LIBRA_COMMITTER_EMAIL", "inherited-libra@example.invalid")
            .env("LIBRA_TEST", "1")
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn supervised identity test");
        let mut child = ReapedChild(child);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = child.0.try_wait().expect("poll supervised child") {
                assert!(ready.is_file(), "{case} exact child test did not execute");
                assert!(
                    returned.is_file(),
                    "{case} original test did not return; not an environment-restoration RED"
                );
                if !status.success() {
                    failed.push(case);
                }
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{case} supervised identity test exceeded its deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(
        failed.is_empty(),
        "original identity tests discarded inherited environment: {failed:?}"
    );
}
