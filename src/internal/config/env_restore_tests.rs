// Run the legacy test bodies only in a fully isolated, supervised libtest child.

use std::{
    ffi::OsString,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use crate::utils::test::ScopedEnvVar;

const CHILD_CASE: &str = "LIBRA_CONFIG_RESTORE_REGRESSION_CHILD";
const CHILD_READY: &str = "LIBRA_CONFIG_RESTORE_REGRESSION_READY";
const CHILD_TEST: &str =
    "internal::config::tests::env_restore_tests::resolver_tests_preserve_caller_env";
const GUARDED_KEYS: [&str; 7] = [
    "LIBRA_CONFIG_GLOBAL_DB",
    "LIBRA_CONFIG_SYSTEM_DB",
    "LIBRA_HOME",
    "HOME",
    "USERPROFILE",
    "LIBRA_PS06_LOCATE_TEST_KEY",
    "LIBRA_PS06_AB_FACTORY_KEY",
];

struct ReapedChild(Child);

impl Drop for ReapedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run_child_case(case: &str) {
    // Preserve every inherited synthetic value even if the legacy body or
    // the immediate post-return assertion panics. Never inspect a DB here.
    let inherited: Vec<(&str, OsString)> = GUARDED_KEYS
        .into_iter()
        .map(|key| {
            (
                key,
                std::env::var_os(key).expect("supervisor supplies isolated sentinel"),
            )
        })
        .collect();
    let _restore: Vec<ScopedEnvVar> = inherited
        .iter()
        .map(|(key, value)| ScopedEnvVar::set(*key, value))
        .collect();
    match case {
        // Tokio's test macro produces a synchronous test entry; do not
        // create an outer Tokio runtime or reacquire their serial(env) lock.
        "locate" => super::locate_env_reports_the_hit_layer_and_resolver_delegates(),
        "sync" => super::resolve_env_sync_for_dir_reads_the_target_repos_vault(),
        _ => panic!("unknown supervised configuration test"),
    }
    // A buggy legacy body may have just removed GLOBAL_DB. Until the guards
    // restore it, only read process env and assert: no config/DB calls.
    for (key, expected) in inherited {
        assert_eq!(
            std::env::var_os(key),
            Some(expected),
            "{case} test must preserve caller environment variable {key}"
        );
    }
}

#[test]
fn resolver_tests_preserve_caller_env() {
    if let Some(case) = std::env::var_os(CHILD_CASE) {
        let ready = std::env::var_os(CHILD_READY).expect("supervisor supplies ready path");
        std::fs::write(ready, b"ready").expect("record exact child entry");
        run_child_case(case.to_str().expect("supervisor case is UTF-8"));
        return;
    }
    // The parent never changes its own env. Each exact child executes only
    // this test, so the two existing serial(env) wrappers own the sole lock.
    let mut failed = Vec::new();
    for case in ["locate", "sync"] {
        let isolated = tempfile::tempdir().expect("supervised child sandbox");
        let root = isolated.path();
        let home = root.join("home");
        let libra_home = home.join(".libra");
        let ready = root.join("child-ready");
        std::fs::create_dir_all(&libra_home).expect("isolated home");
        let child = Command::new(std::env::current_exe().expect("libtest executable"))
            .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
            .env_clear()
            .env(CHILD_CASE, case)
            .env(CHILD_READY, &ready)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("LIBRA_HOME", &libra_home)
            .env("LIBRA_CONFIG_GLOBAL_DB", root.join("caller-global.db"))
            .env("LIBRA_CONFIG_SYSTEM_DB", root.join("caller-system.db"))
            .env("LIBRA_PS06_LOCATE_TEST_KEY", "inherited-locate-sentinel")
            .env("LIBRA_PS06_AB_FACTORY_KEY", "inherited-factory-sentinel")
            .env("LIBRA_TEST", "1")
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn supervised config test");
        let mut child = ReapedChild(child);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = child.0.try_wait().expect("poll supervised child") {
                assert!(ready.is_file(), "{case} exact child test did not execute");
                if !status.success() {
                    failed.push(case);
                }
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{case} supervised config test exceeded its deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(
        failed.is_empty(),
        "configuration test bodies discarded inherited environment: {failed:?}"
    );
}
