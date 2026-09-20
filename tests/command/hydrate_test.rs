//! Integration tests for on-demand hydration (lore.md 3.3).
//!
//! Verifies: whole-object materialization; transitive dep pull-in; the
//! atomic/verified failure-recovery contract (a missing object never writes a
//! corrupt file); sparse gating (out-of-view paths refused, never written);
//! already-present skip; --json; and honest scope (no FUSE, whole-object only).
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::fs;

use super::{assert_cli_success, run_libra_command};

/// A committed repo: scene.usd depends on tex/wood.png; other.txt is unrelated.
fn dep_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init"], p), "init");
    assert_cli_success(&run_libra_command(&["config", "user.name", "t"], p), "name");
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "t@t"], p),
        "email",
    );
    fs::create_dir_all(p.join("tex")).unwrap();
    fs::write(p.join("scene.usd"), "scene-content\n").unwrap();
    fs::write(p.join("tex/wood.png"), "wood-content\n").unwrap();
    fs::write(p.join("other.txt"), "other\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "-A"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    assert_cli_success(
        &run_libra_command(&["deps", "add", "scene.usd", "tex/wood.png"], p),
        "dep",
    );
    repo
}

#[test]
fn hydrate_materializes_path_and_its_deps() {
    let repo = dep_repo();
    let p = repo.path();
    // Delete the worktree files, then hydrate scene.usd → pulls tex/wood.png too.
    fs::remove_file(p.join("scene.usd")).unwrap();
    fs::remove_file(p.join("tex/wood.png")).unwrap();
    fs::remove_file(p.join("other.txt")).unwrap();

    let out = run_libra_command(&["hydrate", "scene.usd"], p);
    assert_cli_success(&out, "hydrate");
    assert_eq!(
        fs::read_to_string(p.join("scene.usd")).unwrap(),
        "scene-content\n"
    );
    assert_eq!(
        fs::read_to_string(p.join("tex/wood.png")).unwrap(),
        "wood-content\n",
        "dep pulled in"
    );
    // other.txt was not a target → not hydrated.
    assert!(!p.join("other.txt").exists(), "unrelated file not hydrated");

    // Re-running reports already-present (byte-identical, no rewrite).
    let again = run_libra_command(&["--json", "hydrate", "scene.usd"], p);
    let js: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(js["data"]["summary"]["hydrated"].as_u64(), Some(0));
    assert_eq!(
        js["data"]["summary"]["skipped"].as_u64(),
        Some(2),
        "both already present"
    );
}

#[test]
fn no_deps_hydrates_only_the_root() {
    let repo = dep_repo();
    let p = repo.path();
    fs::remove_file(p.join("scene.usd")).unwrap();
    fs::remove_file(p.join("tex/wood.png")).unwrap();
    assert_cli_success(
        &run_libra_command(&["hydrate", "scene.usd", "--no-deps"], p),
        "hydrate",
    );
    assert!(p.join("scene.usd").exists());
    assert!(
        !p.join("tex/wood.png").exists(),
        "--no-deps skips the dependency"
    );
}

#[test]
fn missing_object_fails_without_a_corrupt_file() {
    let repo = dep_repo();
    let p = repo.path();
    // A path absent at the revision fails cleanly (non-zero) and writes nothing.
    let out = run_libra_command(&["hydrate", "does-not-exist.txt"], p);
    assert_ne!(out.status.code(), Some(0), "missing path fails");
    assert!(
        !p.join("does-not-exist.txt").exists(),
        "no partial/corrupt file written"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("does not exist"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn sparse_view_gates_hydration_including_deps() {
    let repo = dep_repo();
    let p = repo.path();
    fs::remove_file(p.join("scene.usd")).unwrap();
    // Sparse view scoped to tex/** — scene.usd is out of view.
    assert_cli_success(
        &run_libra_command(&["sparse-view", "set", "tex/**"], p),
        "set view",
    );

    // Hydrating an out-of-view root is refused (skipped) and writes nothing.
    let refused = run_libra_command(&["hydrate", "scene.usd", "--no-deps"], p);
    assert_cli_success(&refused, "out-of-view skip is not an error");
    assert!(
        !p.join("scene.usd").exists(),
        "out-of-view path never materialized"
    );
    assert!(
        String::from_utf8_lossy(&refused.stdout).contains("out of sparse view"),
        "{}",
        String::from_utf8_lossy(&refused.stdout)
    );

    // --ignore-sparse overrides.
    assert_cli_success(
        &run_libra_command(&["hydrate", "scene.usd", "--no-deps", "--ignore-sparse"], p),
        "ignore-sparse",
    );
    assert!(
        p.join("scene.usd").exists(),
        "--ignore-sparse hydrates out-of-view"
    );
}

#[test]
fn dry_run_writes_nothing() {
    let repo = dep_repo();
    let p = repo.path();
    fs::remove_file(p.join("scene.usd")).unwrap();
    let out = run_libra_command(&["hydrate", "scene.usd", "--no-deps", "--dry-run"], p);
    assert_cli_success(&out, "dry-run");
    assert!(!p.join("scene.usd").exists(), "dry-run makes no change");
    assert!(String::from_utf8_lossy(&out.stdout).contains("would hydrate"));
}

/// FM-02 (M-MAT2 U7): hydrate creates an executable entry with `0777 & umask`.
#[cfg(unix)]
#[test]
fn hydrate_honors_process_umask_for_executable_entry() {
    use std::{os::unix::fs::PermissionsExt, process::Command};

    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init"], p), "init");
    assert_cli_success(&run_libra_command(&["config", "user.name", "t"], p), "name");
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "t@t"], p),
        "email",
    );
    let tool = p.join("tool");
    fs::write(&tool, "#!/bin/sh\necho hi\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    assert_cli_success(&run_libra_command(&["add", "tool"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "exec", "--no-verify"], p),
        "commit",
    );
    fs::remove_file(&tool).unwrap();

    let home = p.join(".libra-test-home");
    fs::create_dir_all(home.join(".config")).unwrap();
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "umask 077; exec {} hydrate tool",
            env!("CARGO_BIN_EXE_libra")
        ))
        .current_dir(p)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env(libra::utils::pager::LIBRA_TEST_ENV, "1")
        .output()
        .expect("hydrate under umask 077");
    assert!(
        output.status.success(),
        "hydrate under umask 077 failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::symlink_metadata(&tool)
            .expect("tool metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "hydrate under umask 077 must materialize 700"
    );
}
