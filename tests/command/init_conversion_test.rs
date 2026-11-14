//! Issue #487: an interrupted `init --from-git-repository` must either roll
//! back or stay recoverable, and must never be presented as an ordinary empty
//! repository.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network). The
//! interruption is injected through the conversion failpoints (plan
//! `issues/487.md`, ADR-IG-05), which are inert unless `LIBRA_TEST=1` — the
//! harness always sets it for child processes.

use std::{fs, path::Path, process::Command};

use super::{assert_cli_success, run_libra_command, run_libra_command_with_env};

/// Create a Git source repository with one commit; returns the source dir.
fn git_source(root: &Path) -> std::path::PathBuf {
    let source = root.join("src");
    fs::create_dir_all(&source).expect("create source");
    let run = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(&source)
            .args(args)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&["init", "-q", "."]);
    fs::write(source.join("a.txt"), "a\n").expect("write a.txt");
    run(&["add", "a.txt"]);
    run(&["commit", "-q", "-m", "c1"]);
    source
}

/// A target directory with a user file and (optionally) a Git repository, i.e.
/// the realistic "convert in place" shape.
fn target_dir(root: &Path, with_git: bool) -> std::path::PathBuf {
    let target = root.join("target");
    fs::create_dir_all(&target).expect("create target");
    fs::write(target.join("keep.txt"), "keep\n").expect("write keep.txt");
    if with_git {
        let output = Command::new("git")
            .current_dir(&target)
            .args(["init", "-q", "."])
            .output()
            .expect("git init");
        assert!(output.status.success(), "git init target");
    }
    target
}

fn convert(target: &Path, source: &Path, extra_env: &[(&str, &str)]) -> std::process::Output {
    let source = source.to_string_lossy().to_string();
    run_libra_command_with_env(
        &[
            "init",
            "--vault",
            "false",
            "--from-git-repository",
            &source,
            ".",
        ],
        target,
        extra_env,
    )
}

/// M-FAIL: an ordinary conversion error rolls the run's target state back, so
/// the target is immediately retryable and the user file survives.
#[test]
fn init_conversion_failure_rolls_back_and_is_retryable() {
    let root = tempfile::tempdir().expect("tempdir");
    let source = git_source(root.path());
    let target = target_dir(root.path(), true);

    let failed = convert(
        &target,
        &source,
        &[("LIBRA_TEST_INIT_CONVERT_FAIL_AT", "after-database")],
    );
    assert!(
        !failed.status.success(),
        "the injected failure must fail the conversion: {}",
        String::from_utf8_lossy(&failed.stderr)
    );
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("injected Git conversion failure"),
        "the injected failure must be reported: {}",
        String::from_utf8_lossy(&failed.stderr)
    );
    assert!(
        !target.join(".libra").exists(),
        "a rolled-back conversion must not leave a storage root behind"
    );
    assert!(
        target.join("keep.txt").exists(),
        "the user file must survive the rollback"
    );
    assert!(
        target.join(".git").exists(),
        "the source history must survive the rollback"
    );

    // The target is retryable immediately.
    let retried = convert(&target, &source, &[]);
    assert_cli_success(&retried, "retry after rollback");
    let log = run_libra_command(&["log", "--oneline"], &target);
    assert!(
        log.status.success(),
        "the retry must import history: {}",
        String::from_utf8_lossy(&log.stderr)
    );
}

/// A missing/.. spelling can name an existing target. Rollback must never
/// classify that target as newly created and remove its user-owned files.
#[test]
fn init_conversion_failure_preserves_existing_target_through_dotdot_alias() {
    let root = tempfile::tempdir().expect("tempdir");
    let source = git_source(root.path());
    let target = target_dir(root.path(), true);
    let alias = root.path().join("missing").join("..").join("target");
    let source_arg = source.to_string_lossy().to_string();
    let alias_arg = alias.to_string_lossy().to_string();

    let failed = run_libra_command_with_env(
        &[
            "init",
            "--vault",
            "false",
            "--from-git-repository",
            &source_arg,
            &alias_arg,
        ],
        root.path(),
        &[("LIBRA_TEST_INIT_CONVERT_FAIL_AT", "after-database")],
    );
    assert!(
        !failed.status.success(),
        "the injected failure must fail the conversion"
    );
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("injected Git conversion failure"),
        "the failure must reach the rollback path: {}",
        String::from_utf8_lossy(&failed.stderr)
    );
    assert_eq!(
        fs::read_to_string(target.join("keep.txt")).unwrap(),
        "keep\n"
    );
    assert!(
        target.join(".git").exists(),
        "Git history must survive rollback"
    );
    assert!(
        !target.join(".libra").exists(),
        "the newly created storage root must be rolled back"
    );

    let retried = convert(&target, &source, &[]);
    assert_cli_success(&retried, "retry after alias rollback");
}

/// Bare conversion writes metadata directly into the target. Its rollback
/// must leave an existing target and unrelated user files in place.
#[test]
fn init_bare_conversion_failure_preserves_existing_target_contents() {
    for stage in ["after-database", "after-setup"] {
        let root = tempfile::tempdir().expect("tempdir");
        let source = git_source(root.path());
        let target = target_dir(root.path(), false);
        let source_arg = source.to_string_lossy().to_string();
        let target_arg = target.to_string_lossy().to_string();
        let args = [
            "init",
            "--bare",
            "--vault",
            "false",
            "--from-git-repository",
            &source_arg,
            &target_arg,
        ];

        let failed = run_libra_command_with_env(
            &args,
            root.path(),
            &[("LIBRA_TEST_INIT_CONVERT_FAIL_AT", stage)],
        );
        assert!(
            !failed.status.success(),
            "the injected {stage} failure must fail bare conversion"
        );
        assert!(
            String::from_utf8_lossy(&failed.stderr).contains("injected Git conversion failure"),
            "the {stage} failure must reach the rollback path: {}",
            String::from_utf8_lossy(&failed.stderr)
        );
        assert_eq!(
            fs::read_to_string(target.join("keep.txt")).unwrap(),
            "keep\n"
        );
        for name in [
            "libra.db",
            "objects",
            "info",
            "hooks",
            "CONVERSION_IN_PROGRESS",
        ] {
            assert!(
                !target.join(name).exists(),
                "{stage} rollback must remove only the new bare metadata entry {name}"
            );
        }

        let retried = run_libra_command(&args, root.path());
        assert_cli_success(&retried, "retry after bare rollback");
        assert_eq!(
            fs::read_to_string(target.join("keep.txt")).unwrap(),
            "keep\n"
        );
    }
}

#[cfg(windows)]
#[test]
fn init_bare_conversion_close_failure_keeps_recovery_marker() {
    let root = tempfile::tempdir().expect("tempdir");
    let source = git_source(root.path());
    let target = target_dir(root.path(), false);
    let source_arg = source.to_string_lossy().to_string();
    let target_arg = target.to_string_lossy().to_string();
    let failed = run_libra_command_with_env(
        &[
            "init",
            "--bare",
            "--vault",
            "false",
            "--from-git-repository",
            &source_arg,
            &target_arg,
        ],
        root.path(),
        &[
            ("LIBRA_TEST_INIT_CONVERT_FAIL_AT", "after-database"),
            ("LIBRA_TEST_INIT_CONVERT_CLOSE_FAIL", "1"),
        ],
    );
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(!failed.status.success(), "close failure must be reported");
    assert!(stderr.contains("LBR-REPO-003"), "{stderr}");
    assert!(
        stderr.contains("injected Git conversion failure"),
        "{stderr}"
    );
    assert!(
        stderr.contains("injected database close failure"),
        "{stderr}"
    );
    for name in [
        "keep.txt",
        "libra.db",
        "objects",
        "info",
        "hooks",
        "CONVERSION_IN_PROGRESS",
    ] {
        assert!(
            target.join(name).exists(),
            "a close failure must not partially delete {name}"
        );
    }
}

#[test]
fn init_bare_conversion_layout_failure_cleans_only_new_entries() {
    let root = tempfile::tempdir().expect("tempdir");
    let source = git_source(root.path());
    let target = target_dir(root.path(), false);
    let template = root.path().join("template");
    fs::create_dir(&template).expect("create template");
    fs::write(template.join("objects"), "not a directory\n").expect("write bad layout");
    let template_arg = format!("--template={}", template.display());
    let source_arg = source.to_string_lossy().to_string();
    let target_arg = target.to_string_lossy().to_string();

    let failed = run_libra_command(
        &[
            "init",
            "--bare",
            "--vault",
            "false",
            "--from-git-repository",
            &source_arg,
            &template_arg,
            &target_arg,
        ],
        root.path(),
    );
    assert!(!failed.status.success(), "invalid layout must fail");
    assert_eq!(
        fs::read_to_string(target.join("keep.txt")).unwrap(),
        "keep\n"
    );
    assert!(
        !target.join("objects").exists(),
        "a failed layout must remove its new top-level entry"
    );
    assert!(
        !target.join("CONVERSION_IN_PROGRESS").exists(),
        "layout failure must not leave a conversion marker"
    );

    let retried = run_libra_command(
        &[
            "init",
            "--bare",
            "--vault",
            "false",
            "--from-git-repository",
            &source_arg,
            &target_arg,
        ],
        root.path(),
    );
    assert_cli_success(&retried, "retry after bare layout failure");
}

#[test]
fn init_bare_conversion_refuses_existing_sqlite_journal() {
    let root = tempfile::tempdir().expect("tempdir");
    let source = git_source(root.path());
    let target = target_dir(root.path(), false);
    let journal = target.join("libra.db-journal");
    fs::write(&journal, "user data\n").expect("write pre-existing journal name");
    let source_arg = source.to_string_lossy().to_string();
    let target_arg = target.to_string_lossy().to_string();

    let refused = run_libra_command(
        &[
            "init",
            "--bare",
            "--vault",
            "false",
            "--from-git-repository",
            &source_arg,
            &target_arg,
        ],
        root.path(),
    );
    assert!(
        !refused.status.success(),
        "conflicting journal must be refused"
    );
    assert_eq!(fs::read_to_string(&journal).unwrap(), "user data\n");
    assert!(
        !target.join("objects").exists(),
        "conflict must be detected before layout"
    );
}

#[test]
fn init_bare_conversion_preserves_case_variant_user_file_on_rollback() {
    let root = tempfile::tempdir().expect("tempdir");
    let source = git_source(root.path());
    let target = target_dir(root.path(), false);
    let user_file = target.join("readme");
    fs::write(&user_file, "user data\n").expect("write user file");
    let user_dir = target.join("docs");
    fs::create_dir(&user_dir).expect("create user directory");
    fs::write(user_dir.join("keep.txt"), "keep\n").expect("write nested user file");
    let template = root.path().join("template");
    fs::create_dir(&template).expect("create template");
    fs::write(template.join("README"), "template data\n").expect("write template file");
    fs::create_dir(template.join("Docs")).expect("create template directory");
    fs::write(template.join("Docs/template.txt"), "template\n")
        .expect("write nested template file");
    let template_arg = format!("--template={}", template.display());
    let source_arg = source.to_string_lossy().to_string();
    let target_arg = target.to_string_lossy().to_string();

    let failed = run_libra_command_with_env(
        &[
            "init",
            "--bare",
            "--vault",
            "false",
            "--from-git-repository",
            &source_arg,
            &template_arg,
            &target_arg,
        ],
        root.path(),
        &[("LIBRA_TEST_INIT_CONVERT_FAIL_AT", "after-database")],
    );
    assert!(
        !failed.status.success(),
        "injected failure must fail conversion"
    );
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("injected Git conversion failure"),
        "the failure must reach rollback: {}",
        String::from_utf8_lossy(&failed.stderr)
    );
    assert_eq!(fs::read_to_string(&user_file).unwrap(), "user data\n");
    assert_eq!(
        fs::read_to_string(user_dir.join("keep.txt")).unwrap(),
        "keep\n"
    );
    assert!(target.exists(), "rollback must retain the user's directory");
    for name in ["libra.db", "objects", "CONVERSION_IN_PROGRESS"] {
        assert!(
            !target.join(name).exists(),
            "rollback must remove new metadata entry {name}"
        );
    }
}

#[cfg(unix)]
#[test]
fn init_bare_conversion_shared_does_not_widen_existing_user_file() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    let source = git_source(root.path());
    let target = target_dir(root.path(), false);
    let user_file = target.join("keep.txt");
    fs::set_permissions(&user_file, fs::Permissions::from_mode(0o600))
        .expect("make user file private");
    let source_arg = source.to_string_lossy().to_string();
    let target_arg = target.to_string_lossy().to_string();

    let converted = run_libra_command(
        &[
            "init",
            "--bare",
            "--vault",
            "false",
            "--shared=all",
            "--from-git-repository",
            &source_arg,
            &target_arg,
        ],
        root.path(),
    );
    assert_cli_success(&converted, "bare conversion with shared storage");
    let mode = fs::metadata(&user_file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "existing user file must remain private");
    for (path, expected) in [
        (&target, 0o775),
        (&target.join("objects"), 0o775),
        (&target.join("libra.db"), 0o664),
    ] {
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode,
            expected,
            "new repository entry must use shared mode: {}",
            path.display()
        );
    }
}

/// M-FAIL: a hard kill leaves the conversion marker; every other command
/// refuses to treat the residue as an ordinary empty repository, and a retry
/// moves the residue aside (never deleting it) and completes.
#[test]
fn init_conversion_hard_kill_is_detected_and_recovered() {
    let root = tempfile::tempdir().expect("tempdir");
    let source = git_source(root.path());
    let target = target_dir(root.path(), true);

    // The abort failpoint kills the process with SIGABRT, like an OOM kill.
    let killed = convert(
        &target,
        &source,
        &[("LIBRA_TEST_INIT_CONVERT_ABORT_AT", "after-database")],
    );
    assert!(
        !killed.status.success(),
        "the abort failpoint must kill the conversion"
    );
    let marker = target.join(".libra").join("CONVERSION_IN_PROGRESS");
    assert!(
        marker.exists(),
        "a hard kill must leave the conversion marker behind"
    );

    // Ordinary commands refuse with the recovery guidance.
    let status = run_libra_command(&["status"], &target);
    assert_eq!(
        status.status.code(),
        Some(128),
        "status on a half-converted repository must fail: {}",
        String::from_utf8_lossy(&status.stdout)
    );
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(
        stderr.contains("incomplete Git conversion") || stderr.contains("LBR-REPO-003"),
        "the refusal must name the incomplete conversion: {stderr}"
    );

    // A retry moves the residue aside and converts.
    let retried = convert(&target, &source, &[]);
    assert_cli_success(&retried, "retry after a hard kill");
    assert!(!marker.exists(), "a completed retry must clear the marker");
    let residue: Vec<_> = fs::read_dir(&target)
        .expect("read target")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".libra.incomplete-"))
        .collect();
    assert_eq!(
        residue.len(),
        1,
        "the interrupted storage root must be preserved under one aside name: {residue:?}"
    );
    let log = run_libra_command(&["log", "--oneline"], &target);
    assert!(
        log.status.success(),
        "the recovered repository must have history: {}",
        String::from_utf8_lossy(&log.stderr)
    );
}

/// M-FAIL: a marker owned by a LIVE process blocks the retry instead of
/// racing it (ADR-IG-03 item 2).
#[test]
fn init_conversion_retry_refuses_a_live_owner() {
    use libra::command::init_conversion::{CONVERSION_MARKER, ConversionGuard};

    let root = tempfile::tempdir().expect("tempdir");
    let source = git_source(root.path());
    let target = target_dir(root.path(), true);
    fs::create_dir_all(target.join(".libra")).expect("mkdir .libra");
    // Use the production writer so the current process is provably alive in
    // both the legacy Linux/Windows and compatible macOS marker formats.
    let canonical_source = source.canonicalize().expect("source path").join(".git");
    let _guard = ConversionGuard::begin(&target.join(".libra"), &canonical_source)
        .expect("write live marker");
    assert!(target.join(".libra").join(CONVERSION_MARKER).exists());

    let refused = convert(&target, &source, &[]);
    assert!(
        !refused.status.success(),
        "a live conversion must refuse the retry"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("already running") || stderr.contains("LBR-CONFLICT-002"),
        "the refusal must name the live conversion: {stderr}"
    );
    assert!(
        target.join(".libra").join(CONVERSION_MARKER).exists(),
        "a refused retry must leave the marker untouched"
    );
}

#[test]
fn init_conversion_unstamped_marker_explains_manual_recovery() {
    use libra::command::init_conversion::{
        CONVERSION_MARKER, ConversionMarker, ConversionOwner, MARKER_VERSION, write_marker,
    };

    let root = tempfile::tempdir().expect("tempdir");
    let source = git_source(root.path());
    let target = target_dir(root.path(), false);
    let initialized = run_libra_command(&["init", "--vault", "false", "."], &target);
    assert_cli_success(&initialized, "initialize marker fixture");
    let storage = target.join(".libra");
    let marker = ConversionMarker {
        version: MARKER_VERSION,
        source: source
            .join(".git")
            .canonicalize()
            .expect("source .git")
            .display()
            .to_string(),
        started_at: 0,
        owner: ConversionOwner {
            pid: u32::MAX,
            start_time: None,
        },
        owner_start_time_v2: None,
        owner_boot_session: None,
        created: Vec::new(),
    };
    write_marker(&storage, &marker).expect("write marker");

    let status = run_libra_command(&["status"], &target);
    let status_error = String::from_utf8_lossy(&status.stderr);
    assert_eq!(status.status.code(), Some(128));
    assert!(status_error.contains("LBR-REPO-003"), "{status_error}");
    assert!(
        status_error.contains("confirm no conversion is running on any host"),
        "{status_error}"
    );

    let retry = convert(&target, &source, &[]);
    let retry_error = String::from_utf8_lossy(&retry.stderr);
    assert_eq!(retry.status.code(), Some(128));
    assert!(retry_error.contains("LBR-REPO-003"), "{retry_error}");
    assert!(
        retry_error.contains("confirm no conversion is running on any host"),
        "{retry_error}"
    );
    assert!(storage.join(CONVERSION_MARKER).exists());
}

/// M-FAIL: a marker recorded for a DIFFERENT source is refused rather than
/// silently re-using another conversion's residue.
#[test]
fn init_conversion_retry_refuses_a_source_mismatch() {
    use libra::command::init_conversion::{ConversionGuard, read_marker, write_marker};

    let root = tempfile::tempdir().expect("tempdir");
    let source = git_source(root.path());
    let target = target_dir(root.path(), true);
    fs::create_dir_all(target.join(".libra")).expect("mkdir .libra");
    let storage = target.join(".libra");
    let _guard = ConversionGuard::begin(&storage, Path::new("/somewhere/else/.git"))
        .expect("write source-mismatch marker");
    let mut marker = read_marker(&storage)
        .expect("marker exists")
        .expect("parse marker");
    marker.owner.pid = 999_999;
    write_marker(&storage, &marker).expect("write dead-owner marker");

    let refused = convert(&target, &source, &[]);
    assert!(
        !refused.status.success(),
        "a source mismatch must refuse the retry"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("interrupted conversion") || stderr.contains("LBR-REPO-003"),
        "the refusal must explain the mismatch: {stderr}"
    );
}
