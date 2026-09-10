//! Tests merge command scenarios including fast-forward handling and conflict reporting.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use git_internal::internal::object::commit::Commit;
use libra::{
    command::load_object,
    internal::{branch::Branch, head::Head},
    utils::test::ChangeDirGuard,
};
use serial_test::serial;

use super::{
    assert_cli_success, configure_identity_via_cli, create_committed_repo_via_cli,
    init_repo_via_cli, parse_cli_error_stderr, parse_json_stdout, run_libra_command,
    run_libra_command_with_stdin, run_libra_command_with_stdin_and_env,
};

fn commit_file(repo: &Path, file: &str, content: &str, message: &str) {
    let path = repo.join(file);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create parent directory");
    }
    std::fs::write(path, content).expect("failed to write file");
    assert_cli_success(&run_libra_command(&["add", file], repo), "add file");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", message, "--no-verify"], repo),
        "commit file",
    );
}

fn merge_driver_repo(attribute: Option<&str>, default_driver: Option<&str>) -> tempfile::TempDir {
    merge_driver_repo_for_path("driver.txt", attribute, default_driver)
}

fn merge_driver_repo_for_path(
    file: &str,
    attribute: Option<&str>,
    default_driver: Option<&str>,
) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    let file_path = root.join(file);
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create merge-driver parent");
    }
    std::fs::write(&file_path, "top\nbase\nbottom\n").expect("failed to write merge-driver base");
    let mut paths = vec![file];
    if let Some(attribute) = attribute {
        std::fs::write(root.join(".gitattributes"), format!("*.txt {attribute}\n"))
            .expect("failed to write merge attributes");
        paths.push(".gitattributes");
    }
    assert_cli_success(
        &run_libra_command(&["add", paths[0]], root),
        "add driver base",
    );
    if paths.len() == 2 {
        assert_cli_success(
            &run_libra_command(&["add", paths[1]], root),
            "add merge attributes",
        );
    }
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "driver base", "--no-verify"], root),
        "commit driver base",
    );
    if let Some(driver) = default_driver {
        assert_cli_success(
            &run_libra_command(&["config", "merge.default", driver], root),
            "configure default merge driver",
        );
    }
    assert_cli_success(
        &run_libra_command(&["branch", "driver-side"], root),
        "create side",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "driver-side"], root),
        "checkout side",
    );
    commit_file(root, file, "top\ntheirs\nbottom\n", "driver theirs");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], root),
        "checkout main",
    );
    commit_file(root, file, "top\nours\nbottom\n", "driver ours");
    repo
}

#[cfg(unix)]
fn shell_quote_test(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn external_driver_fixture_command(root: &Path, mode: &str) -> (String, std::path::PathBuf) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/merge-driver.sh");
    let log = root.join(format!("driver-{mode}.log"));
    let command = format!(
        "sh {} {} {} %O %A %B %L %P %S %X %Y",
        shell_quote_test(&fixture.to_string_lossy()),
        shell_quote_test(mode),
        shell_quote_test(&log.to_string_lossy())
    );
    (command, log)
}

#[cfg(unix)]
fn configure_external_driver(root: &Path, mode: &str) -> std::path::PathBuf {
    let (command, log) = external_driver_fixture_command(root, mode);
    assert_cli_success(
        &run_libra_command(&["config", "merge.custom.driver", command.as_str()], root),
        "configure external merge driver",
    );
    log
}

#[test]
fn merge_driver_dispatches_builtin_attributes_and_defaults() {
    struct Case {
        attribute: Option<&'static str>,
        default_driver: Option<&'static str>,
        clean: bool,
        marker: bool,
        label: &'static str,
    }

    for case in [
        Case {
            attribute: Some("merge"),
            default_driver: None,
            clean: false,
            marker: true,
            label: "set means text",
        },
        Case {
            attribute: Some("merge=text"),
            default_driver: None,
            clean: false,
            marker: true,
            label: "named text",
        },
        Case {
            attribute: Some("-merge"),
            default_driver: None,
            clean: false,
            marker: false,
            label: "unset means binary",
        },
        Case {
            attribute: Some("merge=binary"),
            default_driver: None,
            clean: false,
            marker: false,
            label: "named binary",
        },
        Case {
            attribute: Some("merge=union"),
            default_driver: None,
            clean: true,
            marker: false,
            label: "named union",
        },
        Case {
            attribute: Some("merge=unknown"),
            default_driver: Some("union"),
            clean: false,
            marker: true,
            label: "unknown attribute falls back to text",
        },
        Case {
            attribute: None,
            default_driver: Some("union"),
            clean: true,
            marker: false,
            label: "configured default",
        },
        Case {
            attribute: None,
            default_driver: Some("unknown"),
            clean: false,
            marker: true,
            label: "unknown default falls back to text",
        },
        Case {
            attribute: None,
            default_driver: None,
            clean: false,
            marker: true,
            label: "implicit text default",
        },
    ] {
        let repo = merge_driver_repo(case.attribute, case.default_driver);
        let output = run_libra_command(&["merge", "driver-side", "--no-verify"], repo.path());
        assert_eq!(
            output.status.success(),
            case.clean,
            "{}: unexpected status {:?}: {}",
            case.label,
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        let content = std::fs::read_to_string(repo.path().join("driver.txt"))
            .expect("failed to read merge-driver result");
        assert_eq!(
            content.contains("<<<<<<<"),
            case.marker,
            "{}: unexpected merge result: {content:?}",
            case.label
        );
        if case.clean {
            let ours = content.find("ours").expect("clean union must retain ours");
            let theirs = content
                .find("theirs")
                .expect("clean union must retain theirs");
            assert!(
                ours < theirs,
                "{}: union order must be ours then theirs",
                case.label
            );
        }
        if case
            .attribute
            .is_some_and(|value| value == "-merge" || value == "merge=binary")
        {
            assert_eq!(
                content, "top\nours\nbottom\n",
                "{}: binary keeps ours whole",
                case.label
            );
        }
    }
}

#[test]
fn merge_driver_union_binary_input_keeps_ours_without_markers() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    std::fs::write(root.join("driver.bin"), b"base\0bytes").unwrap();
    std::fs::write(root.join(".gitattributes"), "*.bin merge=union\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "driver.bin", ".gitattributes"], root),
        "add union-binary base",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "union-binary base", "--no-verify"], root),
        "commit union-binary base",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "driver-side"], root),
        "create union-binary side",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "driver-side"], root),
        "checkout union-binary side",
    );
    std::fs::write(root.join("driver.bin"), b"theirs\0bytes").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "driver.bin"], root),
        "add union-binary theirs",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "union-binary theirs", "--no-verify"],
            root,
        ),
        "commit union-binary theirs",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], root),
        "checkout union-binary main",
    );
    std::fs::write(root.join("driver.bin"), b"ours\0bytes").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "driver.bin"], root),
        "add union-binary ours",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "union-binary ours", "--no-verify"], root),
        "commit union-binary ours",
    );

    let output = run_libra_command(&["merge", "driver-side", "--no-verify"], root);
    assert!(
        !output.status.success(),
        "binary union fallback must conflict"
    );
    assert_eq!(
        std::fs::read(root.join("driver.bin")).unwrap(),
        b"ours\0bytes"
    );
}

#[cfg(unix)]
#[test]
fn merge_ext_driver_clean_result_is_read_from_percent_a() {
    let repo = merge_driver_repo(Some("merge=custom"), None);
    let root = repo.path();
    let log = configure_external_driver(root, "clean");

    let output = run_libra_command(&["merge", "driver-side", "--no-verify"], root);
    assert_cli_success(&output, "external driver clean merge");
    assert_eq!(
        std::fs::read(root.join("driver.txt")).expect("read external merge result"),
        b"external result\n"
    );
    let invocation = std::fs::read_to_string(log).expect("read driver invocation");
    for expected in [
        "base=top\nbase\nbottom",
        "ours=top\nours\nbottom",
        "theirs=top\ntheirs\nbottom",
        "marker=7",
        "path=driver.txt",
        "ancestor=base",
        "ours-label=HEAD",
        "theirs-label=driver-side",
    ] {
        assert!(
            invocation.contains(expected),
            "missing {expected:?} in {invocation:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn merge_ext_driver_conflict_result_survives_strategy_options_and_exit_128() {
    for (mode, option, expected) in [
        ("conflict", Some("ours"), b"external conflict\n".as_slice()),
        (
            "conflict128",
            Some("theirs"),
            b"external conflict 128\n".as_slice(),
        ),
    ] {
        let repo = merge_driver_repo(Some("merge=custom"), None);
        let root = repo.path();
        configure_external_driver(root, mode);
        let mut args = vec!["merge", "driver-side", "--no-verify"];
        if let Some(option) = option {
            args.extend(["-X", option]);
        }

        let output = run_libra_command(&args, root);
        assert_eq!(
            output.status.code(),
            Some(128),
            "external status {mode} remains a content conflict: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(root.join("driver.txt")).expect("read external conflict"),
            expected,
            "-X must not replace an external driver's %A result"
        );
        let stages = run_libra_command(&["ls-files", "-s", "driver.txt"], root);
        assert_cli_success(&stages, "inspect conflict stages");
        let listing = String::from_utf8_lossy(&stages.stdout);
        for stage in [" 1\t", " 2\t", " 3\t"] {
            assert!(
                listing.contains(stage),
                "missing stage {stage:?}: {listing}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn merge_ext_driver_accepts_an_empty_percent_a_result() {
    let repo = merge_driver_repo(Some("merge=custom"), None);
    let root = repo.path();
    configure_external_driver(root, "empty");

    let output = run_libra_command(&["merge", "driver-side", "--no-verify"], root);
    assert_cli_success(&output, "empty external result is a clean merge");
    assert_eq!(
        std::fs::metadata(root.join("driver.txt"))
            .expect("empty merged file")
            .len(),
        0
    );
}

#[cfg(unix)]
#[test]
fn merge_ext_driver_result_survives_an_independent_add_add_mode_conflict() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    std::fs::write(root.join(".gitattributes"), "*.txt merge=custom\n")
        .expect("write external-driver attributes");
    assert_cli_success(
        &run_libra_command(&["add", ".gitattributes"], root),
        "add external-driver attributes",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "driver attributes", "--no-verify"], root),
        "commit external-driver attributes",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "driver-side"], root),
        "create driver side",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "driver-side"], root),
        "checkout driver side",
    );
    std::fs::write(root.join("driver.txt"), "theirs\n").expect("write executable side");
    std::fs::set_permissions(
        root.join("driver.txt"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("make side executable");
    assert_cli_success(
        &run_libra_command(&["add", "driver.txt"], root),
        "add executable side",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add executable", "--no-verify"], root),
        "commit executable side",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], root),
        "checkout main",
    );
    commit_file(root, "driver.txt", "ours\n", "add regular");
    configure_external_driver(root, "clean");

    let output = run_libra_command(&["merge", "driver-side", "--no-verify"], root);
    assert_eq!(
        output.status.code(),
        Some(128),
        "the independent mode conflict must remain unmerged: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(root.join("driver.txt")).expect("read mode-conflict result"),
        b"external result\n",
        "the external driver's clean content must survive the mode conflict"
    );
    let stages = run_libra_command(&["ls-files", "-s", "driver.txt"], root);
    assert_cli_success(&stages, "inspect mode-conflict stages");
    let listing = String::from_utf8_lossy(&stages.stdout);
    assert!(
        listing.contains("100644 ") && listing.contains(" 2\t"),
        "{listing}"
    );
    assert!(
        listing.contains("100755 ") && listing.contains(" 3\t"),
        "{listing}"
    );
}

#[cfg(unix)]
#[test]
fn merge_ext_driver_sq_quotes_a_hostile_path_without_executing_it() {
    let file = "odd '$(touch PWNED)'.txt";
    let repo = merge_driver_repo_for_path(file, Some("merge=custom"), None);
    let root = repo.path();
    let log = configure_external_driver(root, "clean");

    let output = run_libra_command(&["merge", "driver-side", "--no-verify"], root);
    assert_cli_success(&output, "hostile-looking path is data, not shell code");
    assert_eq!(
        std::fs::read(root.join(file)).expect("merged hostile-looking path"),
        b"external result\n"
    );
    assert!(
        !root.join("PWNED").exists(),
        "path command substitution ran"
    );
    let invocation = std::fs::read_to_string(log).expect("driver path log");
    assert!(invocation.contains(&format!("path={file}")), "{invocation}");
}

#[cfg(unix)]
#[test]
fn merge_ext_driver_protects_temporary_paths_under_a_hostile_worktree_name() {
    let parent = tempfile::tempdir().expect("hostile worktree parent");
    let root = parent.path().join("repo $(touch PWNED)");
    std::fs::create_dir(&root).expect("create hostile worktree path");
    init_repo_via_cli(&root);
    configure_identity_via_cli(&root);
    std::fs::write(root.join("seed.txt"), "seed\n").expect("write initial file");
    assert_cli_success(&run_libra_command(&["add", "seed.txt"], &root), "add seed");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "seed", "--no-verify"], &root),
        "commit seed",
    );
    std::fs::write(root.join("driver.txt"), "top\nbase\nbottom\n").expect("write driver base");
    std::fs::write(root.join(".gitattributes"), "*.txt merge=custom\n")
        .expect("write driver attribute");
    assert_cli_success(
        &run_libra_command(&["add", "driver.txt", ".gitattributes"], &root),
        "add driver base",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "driver base", "--no-verify"], &root),
        "commit driver base",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "driver-side"], &root),
        "create side",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "driver-side"], &root),
        "checkout side",
    );
    commit_file(&root, "driver.txt", "top\ntheirs\nbottom\n", "theirs");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], &root),
        "checkout main",
    );
    commit_file(&root, "driver.txt", "top\nours\nbottom\n", "ours");
    configure_external_driver(&root, "clean");

    let output = run_libra_command(&["merge", "driver-side", "--no-verify"], &root);
    assert_cli_success(&output, "external merge in hostile worktree path");
    assert_eq!(
        std::fs::read(root.join("driver.txt")).expect("read external result"),
        b"external result\n"
    );
    assert!(
        !root.join("PWNED").exists(),
        "temporary path executed shell code"
    );
}

#[cfg(unix)]
#[test]
fn merge_ext_driver_reads_a_global_driver_for_the_configured_default() {
    let repo = merge_driver_repo(None, Some("custom"));
    let root = repo.path();
    let global = tempfile::tempdir().expect("isolated global config");
    let global_db = global.path().join("config.db");
    let (command, _) = external_driver_fixture_command(root, "clean");
    let global_db_value = global_db.to_string_lossy().into_owned();
    let configured = run_libra_command_with_stdin_and_env(
        &[
            "config",
            "--global",
            "Merge.custom.Driver",
            command.as_str(),
        ],
        root,
        "",
        &[("LIBRA_CONFIG_GLOBAL_DB", global_db_value.as_str())],
    );
    assert_cli_success(&configured, "configure global external driver");

    let output = run_libra_command_with_stdin_and_env(
        &["merge", "driver-side", "--no-verify"],
        root,
        "",
        &[("LIBRA_CONFIG_GLOBAL_DB", global_db_value.as_str())],
    );
    assert_cli_success(&output, "global default external driver");
    assert_eq!(
        std::fs::read(root.join("driver.txt")).expect("global driver result"),
        b"external result\n"
    );
}

#[cfg(unix)]
#[test]
fn merge_ext_driver_errors_leave_head_index_and_worktree_unchanged() {
    for mode in ["error129", "signal"] {
        let repo = merge_driver_repo(Some("merge=custom"), None);
        let root = repo.path();
        let log = configure_external_driver(root, mode);
        let head_before = head_commit(root);
        let index_before = std::fs::read(root.join(".libra/index")).expect("read index");
        let worktree_before = std::fs::read(root.join("driver.txt")).expect("read worktree");

        let output = run_libra_command(&["merge", "driver-side", "--no-verify"], root);
        assert!(!output.status.success(), "{mode} must be fatal");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("external merge driver 'custom'"),
            "{stderr}"
        );
        assert!(
            !stderr.contains("tests/fixtures/merge-driver.sh"),
            "the configured command leaked: {stderr}"
        );
        assert_eq!(head_commit(root), head_before);
        assert_eq!(
            std::fs::read(root.join(".libra/index")).expect("read unchanged index"),
            index_before
        );
        assert_eq!(
            std::fs::read(root.join("driver.txt")).expect("read unchanged worktree"),
            worktree_before
        );
        if mode == "signal" {
            let temp_root = std::fs::read_to_string(log.with_extension("log.temp-root"))
                .expect("signal fixture records its protected temp root");
            assert!(
                !Path::new(temp_root.trim()).exists(),
                "parent RAII must remove the interrupted driver's temporary directory"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn merge_ext_driver_reports_an_unavailable_shell_without_leaking_the_command() {
    let repo = merge_driver_repo(Some("merge=custom"), None);
    let root = repo.path();
    configure_external_driver(root, "clean");
    let head_before = head_commit(root);
    let index_before = std::fs::read(root.join(".libra/index")).expect("read index");

    let output = run_libra_command_with_stdin_and_env(
        &["merge", "driver-side", "--no-verify"],
        root,
        "",
        &[("PATH", "/definitely-missing")],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("could not start"), "{stderr}");
    assert!(stderr.contains("merge.custom.driver"), "{stderr}");
    assert!(
        !stderr.contains("merge-driver.sh"),
        "command leaked: {stderr}"
    );
    assert_eq!(head_commit(root), head_before);
    assert_eq!(
        std::fs::read(root.join(".libra/index")).expect("unchanged index"),
        index_before
    );
}

#[cfg(unix)]
#[test]
fn merge_ext_driver_uses_recursive_and_top_level_labels_without_strategy_override() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    std::fs::write(root.join("p.txt"), "0\n").expect("write root");
    assert_cli_success(&run_libra_command(&["add", "p.txt"], root), "add root");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], root),
        "commit root",
    );

    for (branch, content) in [("a", "a\n"), ("b", "b\n")] {
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], root),
            "checkout main",
        );
        assert_cli_success(
            &run_libra_command(&["branch", branch], root),
            "create branch",
        );
        assert_cli_success(
            &run_libra_command(&["checkout", branch], root),
            "checkout branch",
        );
        commit_file(root, "p.txt", content, "side edit");
    }
    for (from, tip, other, resolution) in [("a", "x", "b", "x\n"), ("b", "y", "a", "y\n")] {
        assert_cli_success(
            &run_libra_command(&["checkout", from], root),
            "checkout side",
        );
        assert_cli_success(&run_libra_command(&["branch", tip], root), "create tip");
        assert_cli_success(&run_libra_command(&["checkout", tip], root), "checkout tip");
        assert_eq!(
            run_libra_command(&["merge", other], root).status.code(),
            Some(128)
        );
        std::fs::write(root.join("p.txt"), resolution).expect("resolve side merge");
        assert_cli_success(&run_libra_command(&["add", "p.txt"], root), "stage side");
        assert_cli_success(
            &run_libra_command(&["merge", "--continue", "--no-verify"], root),
            "finish side merge",
        );
    }

    assert_cli_success(
        &run_libra_command(&["checkout", "x"], root),
        "checkout final ours",
    );
    std::fs::write(root.join(".gitattributes"), "*.txt merge=custom\n")
        .expect("write external attribute");
    assert_cli_success(
        &run_libra_command(&["add", ".gitattributes"], root),
        "add external attribute",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "external attribute", "--no-verify"], root),
        "commit external attribute",
    );
    let log = configure_external_driver(root, "conflict");

    let output = run_libra_command(&["merge", "y", "-X", "ours", "--no-verify"], root);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(
        std::fs::read(root.join("p.txt")).expect("external recursive result"),
        b"external conflict\n"
    );
    let invocations = std::fs::read_to_string(log).expect("recursive driver log");
    for expected in [
        "ancestor=merged common ancestors",
        "ours-label=Temporary merge branch 1",
        "theirs-label=Temporary merge branch 2",
        "ancestor=base",
        "ours-label=HEAD",
        "theirs-label=y",
    ] {
        assert!(
            invocations.contains(expected),
            "missing {expected:?}: {invocations}"
        );
    }
}

#[cfg(unix)]
#[test]
fn merge_ext_driver_caches_the_rename_conflict_replay() {
    let ours = "line1\nours\nline3\nline4\nline5\nline6\nline7\nline8\n";
    let theirs = "line1\ntheirs\nline3\nline4\nline5\nline6\nline7\nline8\n";
    let repo = create_rename_repo(Some(ours), theirs);
    let root = repo.path();
    std::fs::write(root.join(".gitattributes"), "*.txt merge=custom\n")
        .expect("write external attribute");
    assert_cli_success(
        &run_libra_command(&["add", ".gitattributes"], root),
        "add external attribute",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "external attribute", "--no-verify"], root),
        "commit external attribute",
    );
    let log = configure_external_driver(root, "conflict");

    let output = run_libra_command(&["merge", "feature", "--no-verify"], root);
    assert_eq!(output.status.code(), Some(128));
    let invocations = std::fs::read_to_string(log).expect("rename driver log");
    assert_eq!(
        invocations.matches("mode=conflict").count(),
        1,
        "incremental conflict-state replay must reuse the external result: {invocations}"
    );
    assert_eq!(
        std::fs::read(root.join("new.txt")).expect("external rename result"),
        b"external conflict\n"
    );
}

#[test]
fn test_merge_cli_missing_branch_returns_error_1() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["merge", "no-such"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(stderr.contains("error: no-such - not something we can merge"));
}

#[test]
fn test_merge_json_fast_forward_outputs_summary() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );

    std::fs::write(temp_path.join("file.txt"), "Feature content").expect("failed to write file");
    assert_cli_success(&run_libra_command(&["add", "."], temp_path), "add file");
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "Add feature content", "--no-verify"],
            temp_path,
        ),
        "commit",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );

    let output = run_libra_command(&["--json", "merge", "feature"], temp_path);
    assert_cli_success(&output, "json merge feature");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "merge");
    assert_eq!(json["data"]["strategy"], "fast-forward");
    assert_eq!(json["data"]["up_to_date"], false);
    assert_eq!(json["data"]["files_changed"], 1);
    assert!(json["data"]["old_commit"].as_str().is_some());
    assert!(json["data"]["commit"].as_str().is_some());
    assert!(output.stderr.is_empty());
}

#[test]
fn test_merge_json_already_up_to_date_outputs_summary() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create branch",
    );

    let output = run_libra_command(&["--json", "merge", "feature"], temp_path);
    assert_cli_success(&output, "json merge up to date");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "merge");
    assert_eq!(json["data"]["strategy"], "already-up-to-date");
    assert_eq!(json["data"]["up_to_date"], true);
    assert_eq!(json["data"]["files_changed"], 0);
    assert!(json["data"]["old_commit"].as_str().is_some());
    assert!(json["data"]["commit"].is_null());
    assert!(output.stderr.is_empty());
}

#[test]
fn test_merge_machine_outputs_single_json_line() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create branch",
    );

    let output = run_libra_command(&["--machine", "merge", "feature"], temp_path);
    assert_cli_success(&output, "machine merge feature");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "expected one JSON line, got: {stdout}"
    );
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("expected JSON");
    assert_eq!(json["command"], "merge");
    assert_eq!(json["data"]["strategy"], "already-up-to-date");
    assert!(output.stderr.is_empty());
}

#[test]
fn test_merge_machine_fast_forward_outputs_single_json_line() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );

    std::fs::write(temp_path.join("file.txt"), "Feature content").expect("failed to write file");
    assert_cli_success(&run_libra_command(&["add", "."], temp_path), "add file");
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "Add feature content", "--no-verify"],
            temp_path,
        ),
        "commit",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );

    let output = run_libra_command(&["--machine", "merge", "feature"], temp_path);
    assert_cli_success(&output, "machine merge feature");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "expected one JSON line, got: {stdout}"
    );
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("expected JSON");
    assert_eq!(json["command"], "merge");
    assert_eq!(json["data"]["strategy"], "fast-forward");
    assert_eq!(json["data"]["up_to_date"], false);
    assert_eq!(json["data"]["files_changed"], 1);
    assert!(output.stderr.is_empty());
}

#[tokio::test]
/// Test fast-forward merge of local branches
async fn test_merge_fast_forward() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );

    // Commit changes on the feature branch
    std::fs::write(temp_path.join("file.txt"), "Feature content").expect("Failed to write file");
    assert_cli_success(&run_libra_command(&["add", "."], temp_path), "add file");
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "Add feature content", "--no-verify"],
            temp_path,
        ),
        "commit",
    );

    // Switch back to the main branch and perform fast-forward merge
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );

    let merge_output = run_libra_command(&["merge", "feature"], temp_path);
    assert!(
        merge_output.status.success(),
        "Fast-forward merge failed: {}",
        String::from_utf8_lossy(&merge_output.stderr)
    );
}

#[tokio::test]
#[serial(cwd)]
/// Test merging a remote branch
async fn test_merge_remote_branch() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );

    std::fs::write(temp_path.join("remote.txt"), "Remote content").expect("Failed to write file");
    assert_cli_success(&run_libra_command(&["add", "."], temp_path), "add file");
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "Add remote content", "--no-verify"],
            temp_path,
        ),
        "commit",
    );

    let _guard = ChangeDirGuard::new(temp_path);
    let feature_commit = Head::current_commit()
        .await
        .expect("feature branch should have a tip");
    Branch::update_branch("feature", &feature_commit.to_string(), Some("origin"))
        .await
        .unwrap();

    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );

    let merge_output = run_libra_command(&["merge", "origin/feature"], temp_path);
    assert!(
        merge_output.status.success(),
        "Merge remote branch failed: {}",
        String::from_utf8_lossy(&merge_output.stderr)
    );
}

#[tokio::test]
#[serial(cwd)]
/// Test JSON output when merging a remote branch reference.
async fn test_merge_json_remote_branch_outputs_summary() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );

    std::fs::write(temp_path.join("remote.txt"), "Remote content").expect("Failed to write file");
    assert_cli_success(&run_libra_command(&["add", "."], temp_path), "add file");
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "Add remote content", "--no-verify"],
            temp_path,
        ),
        "commit",
    );

    let _guard = ChangeDirGuard::new(temp_path);
    let feature_commit = Head::current_commit()
        .await
        .expect("feature branch should have a tip");
    Branch::update_branch("feature", &feature_commit.to_string(), Some("origin"))
        .await
        .unwrap();

    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );

    let output = run_libra_command(
        &["--json", "merge", "refs/remotes/origin/feature"],
        temp_path,
    );
    assert_cli_success(&output, "json merge remote branch");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "merge");
    assert_eq!(json["data"]["strategy"], "fast-forward");
    assert_eq!(json["data"]["up_to_date"], false);
    assert_eq!(json["data"]["files_changed"], 1);
    assert!(json["data"]["commit"].as_str().is_some());
    assert!(output.stderr.is_empty());
}

#[tokio::test]
#[serial(cwd)]
/// Test merging diverged branches with non-overlapping changes.
async fn test_merge_diverged_branch_creates_two_parent_commit() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    let output = run_libra_command(&["branch", "branch1"], temp_path);
    assert!(output.status.success(), "Failed to create branch1");

    let output = run_libra_command(&["checkout", "branch1"], temp_path);
    assert!(output.status.success(), "Failed to checkout branch1");

    commit_file(
        temp_path,
        "branch1.txt",
        "Branch1 content",
        "Add branch1 content",
    );

    let output = run_libra_command(&["checkout", "main"], temp_path);
    assert!(output.status.success(), "Failed to checkout main");

    let output = run_libra_command(&["branch", "branch2"], temp_path);
    assert!(output.status.success(), "Failed to create branch2");

    let output = run_libra_command(&["checkout", "branch2"], temp_path);
    assert!(output.status.success(), "Failed to checkout branch2");

    commit_file(
        temp_path,
        "branch2.txt",
        "Branch2 content",
        "Add branch2 content",
    );

    let output = run_libra_command(&["checkout", "branch1"], temp_path);
    assert!(output.status.success(), "Failed to checkout branch1");

    let merge_output = run_libra_command(&["merge", "branch2"], temp_path);
    assert_cli_success(&merge_output, "three-way merge");
    let stdout = String::from_utf8_lossy(&merge_output.stdout);
    assert!(
        stdout.contains("Merge made by the 'three-way' strategy."),
        "merge should report three-way strategy, stdout: {stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(temp_path.join("branch1.txt")).expect("read branch1"),
        "Branch1 content"
    );
    assert_eq!(
        std::fs::read_to_string(temp_path.join("branch2.txt")).expect("read branch2"),
        "Branch2 content"
    );

    let _guard = ChangeDirGuard::new(temp_path);
    let head = Head::current_commit()
        .await
        .expect("merge should create HEAD");
    let commit: Commit = load_object(&head).expect("load merge commit");
    assert_eq!(
        commit.parent_commit_ids.len(),
        2,
        "diverged merge should create a two-parent commit"
    );
    assert!(
        commit.message.starts_with('\n'),
        "merge commit body must retain Git's blank-line separator before the message"
    );
}

#[test]
fn test_merge_custom_message_via_dash_m() {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();

    assert!(
        run_libra_command(&["checkout", "-b", "feat"], p)
            .status
            .success(),
        "create+checkout feat"
    );
    commit_file(p, "feat.txt", "feat content", "feat commit");
    assert!(
        run_libra_command(&["checkout", "main"], p).status.success(),
        "checkout main"
    );
    commit_file(p, "main.txt", "main content", "main commit");

    let merge = run_libra_command(&["merge", "-m", "MY CUSTOM MERGE MSG", "feat"], p);
    assert_cli_success(&merge, "merge -m custom feat");

    // The merge commit (HEAD) should carry the custom subject.
    let log = run_libra_command(&["log", "-n", "1", "--pretty=%s"], p);
    assert_cli_success(&log, "log -n 1 --pretty=%s");
    let subject = String::from_utf8_lossy(&log.stdout);
    assert!(
        subject.contains("MY CUSTOM MERGE MSG"),
        "merge commit subject should be the -m message, got: {subject}"
    );
}

#[test]
fn test_merge_squash_stages_without_committing() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    assert!(
        run_libra_command(&["checkout", "-b", "feat"], p)
            .status
            .success(),
        "checkout -b feat"
    );
    commit_file(p, "feat.txt", "feat content", "feat commit");
    assert!(
        run_libra_command(&["checkout", "main"], p).status.success(),
        "checkout main"
    );
    commit_file(p, "main.txt", "main content", "main commit");

    let before = run_libra_command(&["rev-parse", "HEAD"], p);
    let before_head = String::from_utf8_lossy(&before.stdout).trim().to_string();

    let merge = run_libra_command(&["merge", "--squash", "feat"], p);
    assert_cli_success(&merge, "merge --squash feat");
    let merge_out = String::from_utf8_lossy(&merge.stdout);
    assert!(
        merge_out.contains("Squash commit"),
        "expected squash message, got: {merge_out}"
    );

    // --squash must NOT move HEAD, but the merged file must be in the worktree.
    let after = run_libra_command(&["rev-parse", "HEAD"], p);
    assert_eq!(
        String::from_utf8_lossy(&after.stdout).trim(),
        before_head,
        "--squash must not move HEAD"
    );
    assert!(
        p.join("feat.txt").exists(),
        "merged file should be staged into the worktree"
    );

    // The staged result is finalized with a normal commit, which advances HEAD.
    let commit = run_libra_command(&["commit", "-m", "squashed merge", "--no-verify"], p);
    assert_cli_success(&commit, "commit after squash");
    let final_head = run_libra_command(&["rev-parse", "HEAD"], p);
    assert_ne!(
        String::from_utf8_lossy(&final_head.stdout).trim(),
        before_head,
        "HEAD should advance after committing the squashed result"
    );
}

#[test]
fn test_merge_no_commit_then_continue() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    assert!(
        run_libra_command(&["checkout", "-b", "feat"], p)
            .status
            .success(),
        "checkout -b feat"
    );
    commit_file(p, "feat.txt", "feat content", "feat commit");
    assert!(
        run_libra_command(&["checkout", "main"], p).status.success(),
        "checkout main"
    );
    commit_file(p, "main.txt", "main content", "main commit");

    let before = run_libra_command(&["rev-parse", "HEAD"], p);
    let before_head = String::from_utf8_lossy(&before.stdout).trim().to_string();

    // --no-commit stages the merge but does not move HEAD.
    let merge = run_libra_command(&["merge", "--no-commit", "feat"], p);
    assert_cli_success(&merge, "merge --no-commit feat");
    assert!(
        String::from_utf8_lossy(&merge.stdout).contains("stopped before committing"),
        "expected the no-commit message, got: {}",
        String::from_utf8_lossy(&merge.stdout)
    );
    let mid = run_libra_command(&["rev-parse", "HEAD"], p);
    assert_eq!(
        String::from_utf8_lossy(&mid.stdout).trim(),
        before_head,
        "--no-commit must not move HEAD"
    );
    assert!(
        p.join("feat.txt").exists(),
        "merged file should be staged into the worktree"
    );

    // merge --continue finalizes the two-parent commit and advances HEAD.
    let cont = run_libra_command(&["merge", "--continue"], p);
    assert_cli_success(&cont, "merge --continue");
    let after = run_libra_command(&["rev-parse", "HEAD"], p);
    assert_ne!(
        String::from_utf8_lossy(&after.stdout).trim(),
        before_head,
        "HEAD should advance after merge --continue"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_merge_same_file_non_overlapping_edits_merges_without_conflict() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    commit_file(
        temp_path,
        "tracked.txt",
        "line 1\nline 2\nline 3\nline 4\nline 5\n",
        "Prepare shared merge fixture",
    );

    let output = run_libra_command(&["branch", "feature"], temp_path);
    assert_cli_success(&output, "create feature");

    let output = run_libra_command(&["checkout", "feature"], temp_path);
    assert_cli_success(&output, "checkout feature");

    commit_file(
        temp_path,
        "tracked.txt",
        "line 1\nline 2\nline 3\nline 4\nline 5 from feature\n",
        "Edit last line on feature",
    );

    let output = run_libra_command(&["checkout", "main"], temp_path);
    assert_cli_success(&output, "checkout main");

    commit_file(
        temp_path,
        "tracked.txt",
        "line 1 from main\nline 2\nline 3\nline 4\nline 5\n",
        "Edit first line on main",
    );

    let merge_output = run_libra_command(&["merge", "feature"], temp_path);
    assert_cli_success(&merge_output, "non-overlapping same-file merge");

    let merged = std::fs::read_to_string(temp_path.join("tracked.txt")).expect("read merged file");
    assert_eq!(
        merged, "line 1 from main\nline 2\nline 3\nline 4\nline 5 from feature\n",
        "non-overlapping same-file edits should merge without conflict markers"
    );
    assert!(
        !merged.contains("<<<<<<<") && !merged.contains("=======") && !merged.contains(">>>>>>>"),
        "clean same-file merge must not leave conflict markers: {merged}"
    );
    assert!(
        !temp_path.join(".libra").join("merge-state.json").exists(),
        "clean same-file merge must not leave merge state"
    );

    let _guard = ChangeDirGuard::new(temp_path);
    let head = Head::current_commit()
        .await
        .expect("merge should create HEAD");
    let commit: Commit = load_object(&head).expect("load merge commit");
    assert_eq!(
        commit.parent_commit_ids.len(),
        2,
        "clean same-file merge should create a two-parent commit"
    );
}

#[test]
fn test_merge_diverged_nested_directory_file_survives_three_way() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(
        temp_path,
        "nested/feature.txt",
        "feature nested\n",
        "feature nested",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "main.txt", "main\n", "main change");

    let output = run_libra_command(&["merge", "feature"], temp_path);
    assert_cli_success(&output, "nested three-way merge");
    assert_eq!(
        std::fs::read_to_string(temp_path.join("nested").join("feature.txt"))
            .expect("read nested feature file"),
        "feature nested\n"
    );
}

#[test]
/// Test JSON envelope for a clean three-way merge.
fn test_merge_json_diverged_branch_outputs_three_way_summary() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    let output = run_libra_command(&["branch", "branch1"], temp_path);
    assert!(output.status.success(), "Failed to create branch1");

    let output = run_libra_command(&["checkout", "branch1"], temp_path);
    assert!(output.status.success(), "Failed to checkout branch1");

    commit_file(
        temp_path,
        "branch1.txt",
        "Branch1 content",
        "Add branch1 content",
    );

    let output = run_libra_command(&["checkout", "main"], temp_path);
    assert!(output.status.success(), "Failed to checkout main");

    let output = run_libra_command(&["branch", "branch2"], temp_path);
    assert!(output.status.success(), "Failed to create branch2");

    let output = run_libra_command(&["checkout", "branch2"], temp_path);
    assert!(output.status.success(), "Failed to checkout branch2");

    commit_file(
        temp_path,
        "branch2.txt",
        "Branch2 content",
        "Add branch2 content",
    );

    let output = run_libra_command(&["checkout", "branch1"], temp_path);
    assert!(output.status.success(), "Failed to checkout branch1");

    let merge_output = run_libra_command(&["--json", "merge", "branch2"], temp_path);
    assert_cli_success(&merge_output, "json three-way merge");
    assert!(merge_output.stderr.is_empty());
    let json = parse_json_stdout(&merge_output);
    assert_eq!(json["command"], "merge");
    assert_eq!(json["data"]["strategy"], "three-way");
    assert_eq!(json["data"]["up_to_date"], false);
    assert_eq!(
        json["data"]["parents"].as_array().expect("parents").len(),
        2
    );
    assert!(
        json["data"]["commit"].as_str().is_some(),
        "json should report the merge commit: {json}"
    );
}

#[test]
fn test_merge_conflict_writes_markers_status_hints_and_abort_restores() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(
        temp_path,
        "tracked.txt",
        "feature change\n",
        "feature change",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "tracked.txt", "main change\n", "main change");

    let output = run_libra_command(&["merge", "feature"], temp_path);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(stderr.contains("merge has conflicts in tracked.txt"));
    assert!(
        report
            .hints
            .iter()
            .any(|hint| hint.contains("libra merge --continue")),
        "conflict error should hint continue: {:?}",
        report.hints
    );

    let conflicted = std::fs::read_to_string(temp_path.join("tracked.txt")).expect("read conflict");
    assert!(conflicted.contains("<<<<<<< HEAD"), "{conflicted}");
    assert!(conflicted.contains("======="), "{conflicted}");
    assert!(conflicted.contains(">>>>>>>"), "{conflicted}");

    let status = run_libra_command(&["status"], temp_path);
    assert_cli_success(&status, "status during merge");
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_stdout.contains("You are in the middle of a merge with 'feature'."),
        "status should mention merge state, stdout: {status_stdout}"
    );
    assert!(status_stdout.contains("libra merge --continue"));
    assert!(status_stdout.contains("libra merge --abort"));

    let abort = run_libra_command(&["merge", "--abort"], temp_path);
    assert_cli_success(&abort, "merge abort");
    assert_eq!(
        std::fs::read_to_string(temp_path.join("tracked.txt")).expect("read restored file"),
        "main change\n"
    );
    assert!(
        !temp_path.join(".libra").join("merge-state.json").exists(),
        "abort should remove merge state"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_merge_continue_after_resolving_conflict_creates_two_parent_commit() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(
        temp_path,
        "tracked.txt",
        "feature change\n",
        "feature change",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "tracked.txt", "main change\n", "main change");

    let output = run_libra_command(&["merge", "feature"], temp_path);
    assert_eq!(output.status.code(), Some(128));

    std::fs::write(temp_path.join("tracked.txt"), "resolved change\n").expect("write resolution");
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], temp_path),
        "stage resolution",
    );
    let status = run_libra_command(&["status"], temp_path);
    assert_cli_success(&status, "status after staged resolution");
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_stdout.contains("all conflicts fixed"),
        "status should acknowledge staged conflict resolution, stdout: {status_stdout}"
    );
    let continued = run_libra_command(&["merge", "--continue"], temp_path);
    assert_cli_success(&continued, "merge continue");
    let stdout = String::from_utf8_lossy(&continued.stdout);
    assert!(stdout.contains("Merge completed."), "stdout: {stdout}");

    let _guard = ChangeDirGuard::new(temp_path);
    let head = Head::current_commit()
        .await
        .expect("merge continue should create HEAD");
    let commit: Commit = load_object(&head).expect("load continued merge commit");
    assert_eq!(commit.parent_commit_ids.len(), 2);
    assert!(
        commit.message.starts_with('\n'),
        "merge --continue commit body must retain Git's blank-line separator before the message"
    );
    assert_eq!(
        std::fs::read_to_string(temp_path.join("tracked.txt")).expect("read resolved file"),
        "resolved change\n"
    );
    assert!(!temp_path.join(".libra").join("merge-state.json").exists());
}

#[test]
fn test_merge_continue_refuses_unstaged_resolution_edits() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(
        temp_path,
        "tracked.txt",
        "feature change\n",
        "feature change",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "tracked.txt", "main change\n", "main change");

    let output = run_libra_command(&["merge", "feature"], temp_path);
    assert_eq!(output.status.code(), Some(128));

    std::fs::write(temp_path.join("tracked.txt"), "staged resolution\n").expect("write resolution");
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], temp_path),
        "stage resolution",
    );
    std::fs::write(temp_path.join("tracked.txt"), "unstaged follow-up\n")
        .expect("write unstaged follow-up");

    let continued = run_libra_command(&["merge", "--continue"], temp_path);
    let (_stderr, report) = parse_cli_error_stderr(&continued.stderr);
    assert_eq!(continued.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(report.message.contains("uncommitted changes"));
    assert_eq!(
        std::fs::read_to_string(temp_path.join("tracked.txt")).expect("read follow-up"),
        "unstaged follow-up\n"
    );
}

#[test]
fn test_merge_dirty_worktree_refuses_before_state() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(temp_path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "main.txt", "main\n", "main change");
    std::fs::write(temp_path.join("tracked.txt"), "dirty\n").expect("write dirty file");

    let output = run_libra_command(&["merge", "feature"], temp_path);
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(report.message.contains("uncommitted changes"));
    assert!(
        !temp_path.join(".libra").join("merge-state.json").exists(),
        "dirty refusal should not create merge state"
    );
}

#[test]
fn test_merge_untracked_overwrite_refuses_before_head_update() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(
        temp_path,
        "clobber.txt",
        "from feature\n",
        "feature clobber",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    std::fs::write(temp_path.join("clobber.txt"), "untracked local\n")
        .expect("write untracked clobber");

    let output = run_libra_command(&["merge", "feature"], temp_path);
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        report
            .message
            .contains("untracked working tree file would be overwritten"),
        "message: {}",
        report.message
    );
    assert_eq!(
        std::fs::read_to_string(temp_path.join("clobber.txt")).expect("read untracked file"),
        "untracked local\n"
    );
    assert!(!temp_path.join(".libra").join("merge-state.json").exists());
}

/// `libra merge --help` surfaces the EXAMPLES banner so users see the
/// supported fast-forward / remote-ref / JSON forms before hitting the
/// `MergeNonFastForward` runtime error. Cross-cutting `--help` EXAMPLES
/// rollout per `docs/development/commands/_general.md` item B.
#[test]
fn test_merge_help_lists_examples_banner() {
    let repo = tempfile::tempdir().expect("tempdir for merge --help");
    let output = run_libra_command(&["merge", "--help"], repo.path());
    assert!(
        output.status.success(),
        "merge --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "merge --help should include EXAMPLES banner, stdout: {stdout}"
    );
    assert!(
        stdout.contains("NOTES:"),
        "merge --help should call out the non-fast-forward limitation, stdout: {stdout}"
    );
    for invocation in [
        "libra merge feature-x",
        "libra merge origin/main",
        "libra merge --json",
    ] {
        assert!(
            stdout.contains(invocation),
            "merge --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}

#[test]
fn test_merge_no_edit_accepts_default_message() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(temp_path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "main.txt", "main\n", "main change");

    // `--no-edit` accepts the auto-generated merge message without an editor
    // (Libra never opens one, so this behaves like a plain three-way merge).
    let output = run_libra_command(&["merge", "feature", "--no-edit"], temp_path);
    assert_cli_success(&output, "merge feature --no-edit");
    let log = run_libra_command(&["log", "--oneline", "-n", "1"], temp_path);
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("Merge feature into main"),
        "merge commit landed with the default message: {:?}",
        String::from_utf8_lossy(&log.stdout)
    );
}

#[test]
fn test_merge_no_stat_short_n_and_long_are_accepted() {
    // `-n`/`--no-stat` suppress Git's post-merge diffstat. Libra's merge never
    // prints a diffstat, so both are accepted no-ops that produce a normal merge.
    for flag in ["-n", "--no-stat"] {
        let temp_repo = create_committed_repo_via_cli();
        let temp_path = temp_repo.path();
        assert_cli_success(
            &run_libra_command(&["branch", "feature"], temp_path),
            "create feature",
        );
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], temp_path),
            "checkout feature",
        );
        commit_file(temp_path, "feature.txt", "feature\n", "feature change");
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], temp_path),
            "checkout main",
        );
        commit_file(temp_path, "main.txt", "main\n", "main change");

        let output = run_libra_command(&["merge", "feature", flag], temp_path);
        assert_cli_success(&output, &format!("merge feature {flag}"));
        // No diffstat is printed (Libra never shows one); the merge still happens.
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains(" | ")
                && !stdout.contains("file changed")
                && !stdout.contains("files changed"),
            "merge {flag} prints no diffstat: {stdout}"
        );
        let log = run_libra_command(&["log", "--oneline", "-n", "1"], temp_path);
        assert!(
            String::from_utf8_lossy(&log.stdout)
                .to_lowercase()
                .contains("merge"),
            "merge {flag} created a merge commit"
        );
    }
}

#[test]
fn test_merge_no_progress_is_accepted_noop() {
    // `--no-progress` suppresses a progress meter. Libra's merge never renders
    // one, so it is an accepted no-op that produces a normal merge.
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(temp_path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "main.txt", "main\n", "main change");

    let output = run_libra_command(&["merge", "feature", "--no-progress"], temp_path);
    assert_cli_success(&output, "merge feature --no-progress");
    let log = run_libra_command(&["log", "--oneline", "-n", "1"], temp_path);
    assert!(
        String::from_utf8_lossy(&log.stdout)
            .to_lowercase()
            .contains("merge"),
        "merge --no-progress created a merge commit"
    );
}

#[test]
fn test_merge_no_verify_signatures_is_accepted_noop() {
    // `--no-verify-signatures` skips GPG signature verification of the merged
    // commits. Libra's merge never verifies signatures, so it is an accepted
    // no-op that produces a normal merge.
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(temp_path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "main.txt", "main\n", "main change");

    let output = run_libra_command(&["merge", "feature", "--no-verify-signatures"], temp_path);
    assert_cli_success(&output, "merge feature --no-verify-signatures");
    let log = run_libra_command(&["log", "--oneline", "-n", "1"], temp_path);
    assert!(
        String::from_utf8_lossy(&log.stdout)
            .to_lowercase()
            .contains("merge"),
        "merge --no-verify-signatures created a merge commit"
    );
}

#[test]
fn test_merge_no_rerere_autoupdate_is_accepted_noop() {
    // `--no-rerere-autoupdate` skips updating the rerere index. Libra has no
    // rerere, so it is an accepted no-op that produces a normal merge.
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(temp_path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "main.txt", "main\n", "main change");

    let output = run_libra_command(&["merge", "feature", "--no-rerere-autoupdate"], temp_path);
    assert_cli_success(&output, "merge feature --no-rerere-autoupdate");
    let log = run_libra_command(&["log", "--oneline", "-n", "1"], temp_path);
    assert!(
        String::from_utf8_lossy(&log.stdout)
            .to_lowercase()
            .contains("merge"),
        "merge --no-rerere-autoupdate created a merge commit"
    );
}

#[test]
fn test_merge_no_gpg_sign_is_accepted_noop() {
    // `--no-gpg-sign` skips signing the merge commit. Libra's merge never signs,
    // so it is an accepted no-op that produces a normal merge.
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(temp_path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "main.txt", "main\n", "main change");

    let output = run_libra_command(&["merge", "feature", "--no-gpg-sign"], temp_path);
    assert_cli_success(&output, "merge feature --no-gpg-sign");
    let log = run_libra_command(&["log", "--oneline", "-n", "1"], temp_path);
    assert!(
        String::from_utf8_lossy(&log.stdout)
            .to_lowercase()
            .contains("merge"),
        "merge --no-gpg-sign created a merge commit"
    );
}

#[test]
fn test_merge_stat_prints_diffstat_for_three_way() {
    // `--stat` prints a diffstat of what the merge brought in. Three-way setup:
    // feature.txt on `feature`, main.txt on `main`, so merging `feature` adds
    // feature.txt relative to the pre-merge main tip.
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "branch feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "checkout feature",
    );
    commit_file(p, "feature.txt", "feature line\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );
    commit_file(p, "main.txt", "main line\n", "main change");

    let out = run_libra_command(&["merge", "--stat", "feature"], p);
    assert_cli_success(&out, "merge --stat feature");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("feature.txt"),
        "diffstat must name the merged-in file: {stdout}"
    );
    assert!(
        stdout.contains(" | "),
        "diffstat must have a per-file bar line: {stdout}"
    );
    assert!(
        stdout.contains("file changed") || stdout.contains("files changed"),
        "diffstat must have a summary line: {stdout}"
    );
}

#[test]
fn test_merge_stat_prints_diffstat_for_fast_forward() {
    // Fast-forward: `main` is strictly behind `feature`, so merging fast-forwards
    // and `--stat` reports the files feature added.
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "branch feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "checkout feature",
    );
    commit_file(p, "ff.txt", "ff line\n", "ff change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );

    let out = run_libra_command(&["merge", "--stat", "feature"], p);
    assert_cli_success(&out, "merge --stat feature (ff)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Fast-forward"),
        "expected a fast-forward: {stdout}"
    );
    assert!(
        stdout.contains("ff.txt") && stdout.contains(" | "),
        "fast-forward --stat must print the diffstat: {stdout}"
    );
}

#[test]
fn test_merge_stat_no_stat_toggle_last_wins() {
    // `--stat`/`--no-stat` is a last-one-wins toggle.
    let make = || -> tempfile::TempDir {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        assert_cli_success(
            &run_libra_command(&["branch", "feature"], p),
            "branch feature",
        );
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], p),
            "checkout feature",
        );
        commit_file(p, "feature.txt", "feature line\n", "feature change");
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], p),
            "checkout main",
        );
        commit_file(p, "main.txt", "main line\n", "main change");
        repo
    };

    // `--no-stat --stat` → stat wins → diffstat printed.
    let repo = make();
    let out = run_libra_command(&["merge", "--no-stat", "--stat", "feature"], repo.path());
    assert_cli_success(&out, "merge --no-stat --stat");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("file changed")
            || String::from_utf8_lossy(&out.stdout).contains("files changed"),
        "last --stat wins → diffstat printed"
    );

    // `--stat --no-stat` → no-stat wins → no diffstat.
    let repo = make();
    let out = run_libra_command(&["merge", "--stat", "--no-stat", "feature"], repo.path());
    assert_cli_success(&out, "merge --stat --no-stat");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains(" | ") && !stdout.contains("file changed"),
        "last --no-stat wins → no diffstat: {stdout}"
    );
}

#[test]
fn test_merge_stat_suppressed_in_json_machine_and_quiet_modes() {
    // `--stat` must never corrupt structured (`--json`/`--machine`) output or
    // break `--quiet` silence: the diffstat is human-only.
    let setup = || -> tempfile::TempDir {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        assert_cli_success(
            &run_libra_command(&["branch", "feature"], p),
            "branch feature",
        );
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], p),
            "checkout feature",
        );
        commit_file(p, "feature.txt", "feature line\n", "feature change");
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], p),
            "checkout main",
        );
        commit_file(p, "main.txt", "main line\n", "main change");
        repo
    };
    let no_stat_text =
        |s: &str| !s.contains(" | ") && !s.contains("file changed") && !s.contains("files changed");

    // `--json --stat`: stdout is a single parseable JSON envelope, no diffstat text.
    let repo = setup();
    let out = run_libra_command(&["--json", "merge", "--stat", "feature"], repo.path());
    assert_cli_success(&out, "--json merge --stat");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json stdout must be a single JSON record");
    assert_eq!(json["command"], "merge");
    assert!(
        no_stat_text(&stdout),
        "no diffstat text in JSON stdout: {stdout}"
    );

    // `--machine --stat`: NDJSON stays clean (machine implies json + quiet).
    let repo = setup();
    let out = run_libra_command(&["--machine", "merge", "--stat", "feature"], repo.path());
    assert_cli_success(&out, "--machine merge --stat");
    assert!(
        no_stat_text(&String::from_utf8_lossy(&out.stdout)),
        "no diffstat text in machine stdout"
    );

    // `--quiet --stat`: stdout stays empty.
    let repo = setup();
    let out = run_libra_command(&["--quiet", "merge", "--stat", "feature"], repo.path());
    assert_cli_success(&out, "--quiet merge --stat");
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "quiet must suppress the diffstat"
    );
}

#[test]
fn test_merge_verify_signatures_accepts_signed_rejects_unsigned() {
    // `merge --verify-signatures` validates the merged tip's PGP signature
    // against the local vault key, aborting if it is unsigned (or invalid).
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // dev: a branch whose tip is a SIGNED commit (vault PGP signing on; `libra
    // init` already provisioned the vault key, so enabling the config is enough).
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "true"], p),
        "enable vault signing",
    );
    assert_cli_success(&run_libra_command(&["branch", "dev"], p), "branch dev");
    assert_cli_success(&run_libra_command(&["checkout", "dev"], p), "checkout dev");
    std::fs::write(p.join("dev.txt"), "dev\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "dev.txt"], p), "add dev.txt");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "dev-signed", "--no-verify"], p),
        "signed dev commit",
    );

    // dev2: a branch (from the original base) whose tip is UNSIGNED.
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "false"], p),
        "disable vault signing",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );
    assert_cli_success(&run_libra_command(&["branch", "dev2"], p), "branch dev2");
    assert_cli_success(
        &run_libra_command(&["checkout", "dev2"], p),
        "checkout dev2",
    );
    std::fs::write(p.join("dev2.txt"), "dev2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "dev2.txt"], p), "add dev2.txt");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "dev2-unsigned", "--no-verify"], p),
        "unsigned dev2 commit",
    );

    // Signed tip → merge --verify-signatures succeeds (proves the signed-content
    // reconstruction round-trips against the vault key).
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main again",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--verify-signatures", "dev"], p),
        "merge of a signed tip",
    );

    // Unsigned tip → aborts before merging.
    let bad = run_libra_command(&["merge", "--verify-signatures", "dev2"], p);
    assert!(
        !bad.status.success(),
        "merge of an unsigned tip must abort: {}",
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("does not have a GPG signature"),
        "unsigned-merge error should name the missing signature: {}",
        String::from_utf8_lossy(&bad.stderr)
    );

    // Without verification, the unsigned tip merges fine.
    assert_cli_success(
        &run_libra_command(&["merge", "--no-verify-signatures", "dev2"], p),
        "unsigned tip merges without verification",
    );

    // A signed commit whose message starts with whitespace (preserved via
    // --cleanup=verbatim) must still verify: the signed-content reconstruction
    // takes the message verbatim, not trimmed.
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "true"], p),
        "re-enable vault signing",
    );
    assert_cli_success(&run_libra_command(&["branch", "dev3"], p), "branch dev3");
    assert_cli_success(
        &run_libra_command(&["checkout", "dev3"], p),
        "checkout dev3",
    );
    std::fs::write(p.join("dev3.txt"), "dev3\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "dev3.txt"], p), "add dev3.txt");
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--cleanup=verbatim",
                "-m",
                "  leading-space subject",
                "--no-verify",
            ],
            p,
        ),
        "signed commit with leading-whitespace message",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main for dev3",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--verify-signatures", "dev3"], p),
        "signed leading-whitespace-message tip verifies (message taken verbatim)",
    );

    // A signed message whose body itself contains the signature END-marker text
    // must still verify: the body is located by the signature block's offset, not
    // by searching for the marker (which would mis-select the body copy).
    assert_cli_success(&run_libra_command(&["branch", "dev4"], p), "branch dev4");
    assert_cli_success(
        &run_libra_command(&["checkout", "dev4"], p),
        "checkout dev4",
    );
    std::fs::write(p.join("dev4.txt"), "dev4\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "dev4.txt"], p), "add dev4.txt");
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--cleanup=verbatim",
                "-m",
                "body mentions -----END PGP SIGNATURE----- inline",
                "--no-verify",
            ],
            p,
        ),
        "signed commit whose body contains the END marker text",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main for dev4",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--verify-signatures", "dev4"], p),
        "signed tip whose message contains the END marker still verifies",
    );
}

/// A three-way `merge` conflict on one line of a multi-line file produces
/// LINE-LEVEL markers (matching Git): shared context lines stay OUTSIDE the
/// `<<<<<<< / ======= / >>>>>>>` region. Fails under the old whole-file
/// presentation (which enclosed every line of each side).
#[test]
fn test_merge_conflict_is_line_level() {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();

    commit_file(p, "shared.txt", "top\nl1\nl2\nl3\nbottom\n", "base shared");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(
        p,
        "shared.txt",
        "top\nl1\nFEATURE\nl3\nbottom\n",
        "feature edit",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    commit_file(p, "shared.txt", "top\nl1\nMAIN\nl3\nbottom\n", "main edit");

    let out = run_libra_command(&["merge", "feature"], p);
    assert_eq!(out.status.code(), Some(128), "merge conflict exits 128");
    let body = std::fs::read_to_string(p.join("shared.txt")).expect("read conflict");

    assert!(
        body.starts_with("top\nl1\n<<<<<<< HEAD\n"),
        "shared prefix precedes the markers: {body:?}"
    );
    assert!(
        body.ends_with("l3\nbottom\n"),
        "shared suffix follows the markers: {body:?}"
    );
    let ours = body
        .split_once("<<<<<<< HEAD\n")
        .and_then(|(_, rest)| rest.split_once("\n======="))
        .map(|(mid, _)| mid)
        .expect("conflict region present");
    assert_eq!(
        ours, "MAIN",
        "ours hunk is just the diverging line: {body:?}"
    );
    assert!(
        body.contains("\nFEATURE\n"),
        "theirs hunk present: {body:?}"
    );
    assert!(
        !ours.contains("top") && !ours.contains("bottom"),
        "shared lines must not be inside the conflict region: {body:?}"
    );
}

/// Build a one-line both-modified conflict repo (`shared.txt`) with `feature`
/// diverging from `main`, without running the merge yet.
fn create_diverged_repo_for_conflict() -> tempfile::TempDir {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    commit_file(
        p,
        "shared.txt",
        "top\nl1\nORIG\nl3\nbottom\n",
        "base shared",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(
        p,
        "shared.txt",
        "top\nl1\nFEATURE\nl3\nbottom\n",
        "feature edit",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    commit_file(p, "shared.txt", "top\nl1\nMAIN\nl3\nbottom\n", "main edit");
    temp_repo
}

/// `merge.conflictStyle = diff3` adds the `||||||| base` block with the
/// common-ancestor content between ours and the `=======` separator
/// (lore.md §1.3); the default two-marker style stays unchanged when unset.
#[test]
fn test_merge_conflict_diff3_markers() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "merge.conflictStyle", "diff3"], p),
        "set conflictStyle",
    );

    let out = run_libra_command(&["merge", "feature"], p);
    assert_eq!(out.status.code(), Some(128), "merge conflict exits 128");
    let body = std::fs::read_to_string(p.join("shared.txt")).expect("read conflict");
    assert!(
        body.contains("<<<<<<< HEAD\nMAIN\n||||||| base\nORIG\n=======\nFEATURE\n"),
        "diff3 emits the base block between ours and the separator: {body:?}"
    );
}

/// An unsupported `merge.conflictStyle` (e.g. the unimplemented `zdiff3`) is a
/// hard error when a conflict must be rendered — never a silent fall-back to
/// the default marker format — and nothing is written (no merge state).
#[test]
fn test_merge_conflict_style_invalid_rejected() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "merge.conflictStyle", "zdiff3"], p),
        "set conflictStyle",
    );

    let out = run_libra_command(&["merge", "feature"], p);
    assert_eq!(out.status.code(), Some(128), "invalid style is fatal");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unsupported merge.conflictStyle 'zdiff3'"),
        "actionable error names the bad value: {stderr}"
    );
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state is left behind when the style is rejected"
    );
    let body = std::fs::read_to_string(p.join("shared.txt")).expect("read file");
    assert!(
        !body.contains("<<<<<<<"),
        "no conflict markers were written: {body:?}"
    );
}

// ---------------------------------------------------------------------------
// `merge --dry-run` (Libra extension, lore.md §1.3): preview the outcome
// writing NOTHING — no HEAD/index/worktree/merge-state/object-store mutation.
// Exit 0 for ff/up-to-date/clean; exit 1 when the merge would conflict.
// ---------------------------------------------------------------------------

/// HEAD commit hash via `--json log -n1`-free plumbing: read `.libra` HEAD via
/// `rev-parse`-equivalent CLI (`libra rev-parse HEAD`).
fn head_commit(p: &Path) -> String {
    let out = run_libra_command(&["rev-parse", "HEAD"], p);
    assert_cli_success(&out, "rev-parse HEAD");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Count every file under `.libra/objects` (loose objects), recursively.
fn count_loose_objects(p: &Path) -> usize {
    fn walk(dir: &Path, total: &mut usize) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, total);
                } else {
                    *total += 1;
                }
            }
        }
    }
    let mut total = 0;
    walk(&p.join(".libra").join("objects"), &mut total);
    total
}

#[test]
fn test_merge_dry_run_fast_forward_writes_nothing() {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "co feat");
    commit_file(p, "file.txt", "feature content\n", "feature edit");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");

    let head_before = head_commit(p);
    let operations_before = operation_count(p);
    let out = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_cli_success(&out, "dry-run ff");
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["strategy"], "fast-forward");
    assert_eq!(json["data"]["dry_run"], true);
    assert!(json["data"].get("would_conflict").is_none());
    // Nothing was written: HEAD unchanged, worktree file absent, no state.
    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert!(
        !p.join("file.txt").exists(),
        "worktree must not receive the feature file"
    );
    assert!(!p.join(".libra").join("merge-state.json").exists());
    // And no OPERATION row (§C.9): the sequencer boundary persists one before
    // the handler runs, so mapping a dry run to a control action would write to
    // the operation log for a command documented to write nothing.
    assert_eq!(
        operation_count(p),
        operations_before,
        "a dry run must not record an operation"
    );
}

/// How many operations the log holds — the assertion a dry run needs, since
/// the control boundary writes its row before the handler is reached.
fn operation_count(repo: &Path) -> u64 {
    let out = run_libra_command(&["--json", "op", "log", "-n", "100"], repo);
    assert_cli_success(&out, "op log");
    parse_json_stdout(&out)["data"]["total"]
        .as_u64()
        .expect("op log total")
}

#[test]
fn test_merge_dry_run_already_up_to_date() {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    let out = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_cli_success(&out, "dry-run up-to-date");
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["up_to_date"], true);
    assert_eq!(json["data"]["dry_run"], true);
}

#[test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
fn test_merge_dry_run_clean_three_way_writes_no_objects() {
    // Divergent but non-overlapping edits: a clean three-way preview. The
    // auto-merged blob must be computed in memory only — the object store,
    // HEAD, index, worktree, and merge state all stay untouched.
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    commit_file(p, "shared.txt", "top\nl1\nl2\nl3\nbottom\n", "base shared");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "co feat");
    commit_file(
        p,
        "shared.txt",
        "top\nFEATURE\nl2\nl3\nbottom\n",
        "feature edit",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    commit_file(p, "shared.txt", "top\nl1\nl2\nMAIN\nbottom\n", "main edit");

    let head_before = head_commit(p);
    let objects_before = count_loose_objects(p);
    let out = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_cli_success(&out, "dry-run clean three-way");
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["strategy"], "three-way");
    assert_eq!(json["data"]["dry_run"], true);
    assert!(json["data"]["commit"].is_null(), "no merge commit created");
    assert!(json["data"].get("would_conflict").is_none());

    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert_eq!(
        count_loose_objects(p),
        objects_before,
        "a dry-run must not write objects (auto-merged blobs stay in memory)"
    );
    assert!(!p.join(".libra").join("merge-state.json").exists());
    assert_eq!(
        std::fs::read_to_string(p.join("shared.txt")).unwrap(),
        "top\nl1\nl2\nMAIN\nbottom\n",
        "worktree untouched"
    );
}

#[test]
fn test_merge_dry_run_conflict_exits_1_and_writes_nothing() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    let head_before = head_commit(p);

    let out = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_eq!(
        out.status.code(),
        Some(1),
        "would-conflict preview exits 1 (an outcome signal, not the 128 of a real conflict)"
    );
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["dry_run"], true);
    assert_eq!(json["data"]["would_conflict"], true);
    assert!(
        json["data"]["conflicted_paths"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("shared.txt"))),
        "conflicted_paths names the path: {json}"
    );

    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert!(!p.join(".libra").join("merge-state.json").exists());
    let body = std::fs::read_to_string(p.join("shared.txt")).unwrap();
    assert!(
        !body.contains("<<<<<<<"),
        "no conflict markers written by a preview: {body:?}"
    );
}

#[test]
fn test_merge_json_schema_freeze_no_dry_run_fields_on_real_merge() {
    // A REAL merge's JSON must not grow the dry_run/would_conflict keys.
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    let out = run_libra_command(&["--json", "merge", "feature"], p);
    assert_cli_success(&out, "real merge");
    let json = parse_json_stdout(&out);
    assert!(json["data"].get("dry_run").is_none());
    assert!(json["data"].get("would_conflict").is_none());
}

#[test]
fn test_merge_dry_run_clap_exclusions() {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    for argv in [
        &["merge", "--dry-run", "--continue"][..],
        &["merge", "--dry-run", "--abort"][..],
        &["merge", "--dry-run", "--squash", "feature"][..],
        &["merge", "--restart", "feature"][..],
        &["merge", "--restart", "--no-ff"][..],
        &["merge", "--restart", "--dry-run"][..],
    ] {
        let out = run_libra_command(argv, p);
        assert_eq!(
            out.status.code(),
            Some(129),
            "clap must reject {argv:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// ---------------------------------------------------------------------------
// `merge --restart` (Libra extension, lore.md §1.3): abort the in-progress
// conflicted merge (discarding resolution work, exactly like --abort) and
// re-run the SAME merge against the recorded target commit.
// ---------------------------------------------------------------------------

#[test]
fn test_merge_restart_regenerates_fresh_conflict() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    let head_before = head_commit(p);
    let out = run_libra_command(&["merge", "feature"], p);
    assert_eq!(out.status.code(), Some(128), "initial conflict");

    // Simulate partial resolution work that --restart must DISCARD.
    std::fs::write(p.join("shared.txt"), "half-resolved\n").unwrap();

    let out = run_libra_command(&["merge", "--restart"], p);
    assert_eq!(
        out.status.code(),
        Some(128),
        "the re-run reproduces the conflict (normal merge exit): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = std::fs::read_to_string(p.join("shared.txt")).unwrap();
    assert!(
        body.contains("<<<<<<< HEAD") && !body.contains("half-resolved"),
        "fresh markers regenerated, user edits discarded: {body:?}"
    );
    assert!(
        p.join(".libra").join("merge-state.json").exists(),
        "a fresh merge state exists after restart"
    );
    assert_eq!(head_commit(p), head_before, "HEAD is back at orig_head");
    // The restarted merge is resumable exactly like a normal conflicted merge.
    assert_cli_success(&run_libra_command(&["merge", "--abort"], p), "abort");
}

#[test]
fn test_merge_restart_without_merge_in_progress_errors() {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    let out = run_libra_command(&["merge", "--restart"], p);
    assert_eq!(out.status.code(), Some(128), "no merge in progress");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no merge in progress"),
        "actionable error: {stderr}"
    );
}

#[test]
fn test_merge_restart_refuses_staged_no_commit_merge() {
    // `--no-commit` persists MergeState with NO conflicts; --restart must
    // refuse (it would discard the staged result and could fast-forward),
    // leaving the staged merge fully intact.
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    commit_file(p, "shared.txt", "top\nl1\nl2\nl3\nbottom\n", "base shared");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "co feat");
    commit_file(
        p,
        "shared.txt",
        "top\nFEATURE\nl2\nl3\nbottom\n",
        "feature edit",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    commit_file(p, "shared.txt", "top\nl1\nl2\nMAIN\nbottom\n", "main edit");

    assert_cli_success(
        &run_libra_command(&["merge", "--no-commit", "feature"], p),
        "clean --no-commit merge",
    );
    assert!(p.join(".libra").join("merge-state.json").exists());
    let head_before = head_commit(p);

    let out = run_libra_command(&["merge", "--restart"], p);
    assert_eq!(out.status.code(), Some(128), "restart refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no conflicted merge to restart"),
        "actionable refusal: {stderr}"
    );
    // The staged no-commit merge is untouched and still finishable.
    assert_eq!(head_commit(p), head_before, "HEAD untouched");
    assert!(
        p.join(".libra").join("merge-state.json").exists(),
        "staged merge state preserved"
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--continue"], p),
        "staged merge still finishable",
    );
}

// ── merge --autostash (lore.md §1.8) ────────────────────────────────────────

/// Diverged repo WITHOUT conflicts: feature edits its own file.
fn create_diverged_repo_clean() -> tempfile::TempDir {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    commit_file(p, "base.txt", "base\n", "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(p, "feature.txt", "feature\n", "feature edit");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    commit_file(p, "main.txt", "main\n", "main edit");
    temp_repo
}

fn stash_list_len(p: &Path) -> usize {
    let out = run_libra_command(&["stash", "list"], p);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}

#[test]
fn test_merge_autostash_clean_merge_reapplies() {
    let temp_repo = create_diverged_repo_clean();
    let p = temp_repo.path();
    // Clean tree: strict no-op (no stash, normal merge).
    let out = run_libra_command(&["--json", "merge", "feature", "--autostash"], p);
    assert_cli_success(&out, "clean-tree autostash merge");
    let json = parse_json_stdout(&out);
    assert!(
        json["data"].get("autostash").is_none(),
        "clean tree adds no autostash marker: {json}"
    );
    // Re-merge with a dirty tree in a fresh repo.
    let temp_repo = create_diverged_repo_clean();
    let p = temp_repo.path();
    std::fs::write(p.join("base.txt"), "dirty edit\n").unwrap();
    let out = run_libra_command(&["--json", "merge", "feature", "--autostash"], p);
    assert_cli_success(&out, "dirty autostash merge");
    let json = parse_json_stdout(&out);
    assert_eq!(
        json["data"]["autostash"].as_str(),
        Some("applied"),
        "{json}"
    );
    // The dirty edit is back, and the stash list is empty (never entered).
    assert_eq!(
        std::fs::read_to_string(p.join("base.txt")).unwrap(),
        "dirty edit\n"
    );
    assert_eq!(stash_list_len(p), 0, "autostash never enters stash list");
    // The merge result is present too.
    assert!(p.join("feature.txt").exists());
}

#[test]
fn test_merge_autostash_restores_staged_and_worktree_layers() {
    let temp_repo = create_diverged_repo_clean();
    let p = temp_repo.path();
    std::fs::write(p.join("base.txt"), "staged only\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "base.txt"], p), "stage edit");
    std::fs::write(p.join("base.txt"), "base\n").unwrap();

    let out = run_libra_command(&["merge", "feature", "--autostash"], p);
    assert_cli_success(&out, "layered autostash merge");
    assert_eq!(
        std::fs::read_to_string(p.join("base.txt")).unwrap(),
        "base\n"
    );

    let staged = run_libra_command(&["ls-files", "--stage", "base.txt"], p);
    assert_cli_success(&staged, "inspect restored staged entry");
    let staged = String::from_utf8(staged.stdout).unwrap();
    let staged_oid = staged
        .split_whitespace()
        .nth(1)
        .expect("stage row has object id");
    let blob = run_libra_command(&["cat-file", "-p", staged_oid], p);
    assert_cli_success(&blob, "read restored staged blob");
    assert_eq!(blob.stdout, b"staged only\n");
}

#[test]
fn test_merge_autostash_conflict_holds_then_abort_restores() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    std::fs::write(p.join("unrelated.txt"), "precious\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "unrelated.txt"], p), "add");
    let out = run_libra_command(&["merge", "feature", "--autostash"], p);
    assert_eq!(out.status.code(), Some(128), "conflict exits 128");
    // Held: dirty changes absent from the conflicted tree, stash list empty.
    assert!(
        !p.join("unrelated.txt").exists(),
        "held autostash removes the dirty file from the conflict worktree"
    );
    assert_eq!(stash_list_len(p), 0, "held autostash not in stash list");
    assert!(
        p.join(".libra/merge-autostash.json").exists(),
        "sidecar holds the stash"
    );
    assert_cli_success(
        &run_libra_command(&["maintenance", "run", "--task", "gc"], p),
        "gc preserves held merge autostash",
    );
    // --abort restores the pre-merge tree AND re-applies the autostash.
    let abort = run_libra_command(&["--json", "merge", "--abort"], p);
    assert_cli_success(&abort, "abort");
    let json = parse_json_stdout(&abort);
    assert_eq!(
        json["data"]["autostash"].as_str(),
        Some("applied"),
        "{json}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("unrelated.txt")).unwrap(),
        "precious\n"
    );
    assert!(!p.join(".libra/merge-autostash.json").exists());
}

#[test]
fn test_merge_autostash_conflict_resolve_continue_reapplies() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    std::fs::write(p.join("unrelated.txt"), "precious\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "unrelated.txt"], p), "add");
    let out = run_libra_command(&["merge", "feature", "--autostash"], p);
    assert_eq!(out.status.code(), Some(128));
    // Resolve and continue; the autostash comes back after the merge commit.
    std::fs::write(p.join("shared.txt"), "top\nl1\nRESOLVED\nl3\nbottom\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add");
    let cont = run_libra_command(&["--json", "merge", "--continue"], p);
    assert_cli_success(&cont, "continue");
    let json = parse_json_stdout(&cont);
    assert_eq!(
        json["data"]["autostash"].as_str(),
        Some("applied"),
        "{json}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("unrelated.txt")).unwrap(),
        "precious\n"
    );
    assert_eq!(stash_list_len(p), 0);
}

#[test]
fn test_merge_autostash_restart_preserves_held_stash() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    std::fs::write(p.join("unrelated.txt"), "precious\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "unrelated.txt"], p), "add");
    let out = run_libra_command(&["merge", "feature", "--autostash"], p);
    assert_eq!(out.status.code(), Some(128));
    // Restart re-conflicts; the held stash must survive (not demoted).
    let restart = run_libra_command(&["merge", "--restart"], p);
    assert_eq!(restart.status.code(), Some(128), "re-conflicts");
    assert_eq!(stash_list_len(p), 0, "held stash NOT demoted by restart");
    assert!(
        p.join(".libra/merge-autostash.json").exists(),
        "sidecar survives restart"
    );
    // Abort finally restores everything.
    assert_cli_success(&run_libra_command(&["merge", "--abort"], p), "abort");
    assert_eq!(
        std::fs::read_to_string(p.join("unrelated.txt")).unwrap(),
        "precious\n"
    );
}

#[test]
fn test_merge_autostash_start_failure_restores_immediately() {
    let temp_repo = create_diverged_repo_clean();
    let p = temp_repo.path();
    std::fs::write(p.join("base.txt"), "dirty edit\n").unwrap();
    // --ff-only on diverged branches is refused AFTER the stash was taken:
    // the dirty tree must be restored before the error propagates.
    let out = run_libra_command(&["merge", "feature", "--ff-only", "--autostash"], p);
    assert!(!out.status.success(), "ff-only diverged refused");
    assert_eq!(
        std::fs::read_to_string(p.join("base.txt")).unwrap(),
        "dirty edit\n",
        "start failure restores the dirty tree"
    );
    assert!(!p.join(".libra/merge-autostash.json").exists());
    assert_eq!(stash_list_len(p), 0);
}

#[test]
fn test_merge_autostash_config_and_validation() {
    let temp_repo = create_diverged_repo_clean();
    let p = temp_repo.path();
    // merge.autostash=true enables without the flag.
    assert_cli_success(
        &run_libra_command(&["config", "merge.autostash", "true"], p),
        "set config",
    );
    std::fs::write(p.join("base.txt"), "dirty edit\n").unwrap();
    let out = run_libra_command(&["--json", "merge", "feature"], p);
    assert_cli_success(&out, "config-enabled autostash");
    let json = parse_json_stdout(&out);
    assert_eq!(
        json["data"]["autostash"].as_str(),
        Some("applied"),
        "{json}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("base.txt")).unwrap(),
        "dirty edit\n"
    );
    // Invalid config value is a HARD error (not silently off).
    let temp_repo = create_diverged_repo_clean();
    let p = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "merge.autostash", "sometimes"], p),
        "set bad config",
    );
    std::fs::write(p.join("base.txt"), "dirty edit\n").unwrap();
    let out = run_libra_command(&["merge", "feature"], p);
    assert!(!out.status.success(), "invalid merge.autostash is fatal");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("merge.autostash"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // --no-autostash overrides the (invalid) config: merge refuses the dirty
    // tree via the normal guard instead (dirty worktree blocks a three-way).
    let out = run_libra_command(&["merge", "feature", "--no-autostash"], p);
    assert!(!out.status.success());
    assert!(
        std::fs::read_to_string(p.join("base.txt")).unwrap() == "dirty edit\n",
        "no-autostash leaves the tree alone"
    );
    // clap exclusions.
    let out = run_libra_command(&["merge", "--continue", "--autostash"], p);
    assert_eq!(out.status.code(), Some(129));
    let out = run_libra_command(&["merge", "feature", "--dry-run", "--autostash"], p);
    assert_eq!(out.status.code(), Some(129));
}

/// Regression: `Commit::from_tree_id` hardcodes `mega <admin@mega.org>` as both
/// author and committer. Every merge-commit path used it, so merge commits
/// silently discarded `user.name` / `user.email`. All three paths must now carry
/// the configured identity.
#[tokio::test]
#[serial(cwd)]
async fn test_merge_commit_carries_configured_identity() {
    for (label, extra_args) in [
        ("three-way", Vec::new()),
        ("no-ff", vec!["--no-ff"]),
        ("ours-strategy", vec!["-s", "ours"]),
    ] {
        let temp_repo = create_committed_repo_via_cli();
        let temp_path = temp_repo.path();

        assert_cli_success(
            &run_libra_command(&["branch", "feature"], temp_path),
            "create feature",
        );
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], temp_path),
            "checkout feature",
        );
        commit_file(temp_path, "feature.txt", "feature\n", "feature commit");
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], temp_path),
            "checkout main",
        );
        commit_file(temp_path, "main.txt", "main\n", "main commit");

        let mut args = vec!["merge", "feature"];
        args.extend_from_slice(&extra_args);
        assert_cli_success(&run_libra_command(&args, temp_path), label);

        let _guard = ChangeDirGuard::new(temp_path);
        let head = Head::current_commit().await.expect("merge moved HEAD");
        let commit: Commit = load_object(&head).expect("load merge commit");
        assert_eq!(
            commit.parent_commit_ids.len(),
            2,
            "{label} should record two parents"
        );
        assert_eq!(
            (
                commit.author.name.as_str(),
                commit.author.email.as_str(),
                commit.committer.name.as_str(),
                commit.committer.email.as_str(),
            ),
            (
                "Test User",
                "test@example.com",
                "Test User",
                "test@example.com",
            ),
            "{label} merge commit must use the configured identity, not the hardcoded default"
        );
    }
}

/// `--continue` finalizes without an editor, so `-m` is the only way to set the
/// message of a conflicted merge. It must also carry the configured identity.
#[tokio::test]
#[serial(cwd)]
async fn test_merge_continue_accepts_message_override_and_configured_identity() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(temp_path, "tracked.txt", "feature change\n", "feature");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "tracked.txt", "main change\n", "main");

    // Conflict, then resolve.
    assert_eq!(
        run_libra_command(&["merge", "feature"], temp_path)
            .status
            .code(),
        Some(128)
    );
    std::fs::write(temp_path.join("tracked.txt"), "resolved\n").expect("write resolution");
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], temp_path),
        "stage resolution",
    );

    assert_cli_success(
        &run_libra_command(
            &["merge", "--continue", "-m", "custom merge subject"],
            temp_path,
        ),
        "merge continue with -m",
    );

    let _guard = ChangeDirGuard::new(temp_path);
    let head = Head::current_commit().await.expect("continue moved HEAD");
    let commit: Commit = load_object(&head).expect("load continued merge commit");
    assert_eq!(commit.parent_commit_ids.len(), 2);
    // Commit messages are stored with a leading newline (`format_commit_msg`).
    assert!(
        commit
            .message
            .trim_start()
            .starts_with("custom merge subject"),
        "-m must override the message stored at merge start, got: {}",
        commit.message
    );
    assert!(
        !commit.message.contains("Merge feature into main"),
        "the stored default must not survive the override, got: {}",
        commit.message
    );
    assert_eq!(
        (commit.author.name.as_str(), commit.author.email.as_str()),
        ("Test User", "test@example.com"),
        "continued merge commit must use the configured identity"
    );
}

// ── ADR-MG-01 gitlink (submodule) fail-closed ────────────────────────────────
//
// Libra is a monorepo client and never merges submodule content. A three-way
// merge that would have to ARBITRATE a `160000` gitlink is refused before
// anything is written; a gitlink all three sides already agree on is carried
// through untouched. `merge`, `rebase` and `cherry-pick` share one guard, so
// the refusal text is identical apart from the operation name.

/// The submodule pointer the fixtures start from. A gitlink names a commit of
/// ANOTHER repository, so nothing requires it to exist here — which is exactly
/// why merging one cannot be resolved locally.
const GITLINK_BASE: &str = "0123456789abcdef0123456789abcdef01234567";
/// A different pointer, used to make one side of the merge move the submodule.
const GITLINK_MOVED: &str = "89abcdef0123456789abcdef0123456789abcdef";

fn stdout_trimmed(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Build a repository whose base commit tracks a `vendor` gitlink, with a
/// `feature` branch that adds `side.txt` and sets the gitlink to
/// `feature_gitlink`, and a `main` tip that adds `ours.txt`.
///
/// Everything is composed with plumbing (`update-index --cacheinfo` /
/// `write-tree` / `commit-tree` / `update-ref`) so no checkout ever has to
/// materialize the submodule, and `main`'s index is restored to the base tree
/// before the final commit — the fixture therefore starts from a clean status.
fn create_gitlink_repo(feature_gitlink: &str) -> tempfile::TempDir {
    create_gitlink_repo_with(feature_gitlink, false)
}

/// [`create_gitlink_repo`], optionally making both sides edit `tracked.txt` so
/// the merge stops on a real content conflict while the submodule pointer stays
/// untouched.
fn create_gitlink_repo_with(feature_gitlink: &str, conflicting: bool) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},vendor"),
            ],
            p,
        ),
        "stage the base gitlink",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add submodule", "--no-verify"], p),
        "commit the base gitlink",
    );
    let base = head_commit(p);

    let side_blob = {
        let out = run_libra_command(&["hash-object", "-w", "--stdin"], p);
        assert!(out.status.success(), "hash-object must succeed");
        stdout_trimmed(&out)
    };
    // `hash-object --stdin` with no stdin body hashes the empty blob, which is
    // all this fixture needs: the point is that `feature` touches a file.
    let mut stage_feature = vec![
        "update-index".to_string(),
        "--cacheinfo".to_string(),
        format!("100644,{side_blob},side.txt"),
        "--cacheinfo".to_string(),
        format!("160000,{feature_gitlink},vendor"),
    ];
    if conflicting {
        let their_tracked = {
            let out =
                run_libra_command_with_stdin(&["hash-object", "-w", "--stdin"], p, "theirs edit\n");
            assert!(out.status.success(), "hash-object must succeed");
            stdout_trimmed(&out)
        };
        stage_feature.push("--cacheinfo".to_string());
        stage_feature.push(format!("100644,{their_tracked},tracked.txt"));
    }
    let stage_feature: Vec<&str> = stage_feature.iter().map(String::as_str).collect();
    assert_cli_success(
        &run_libra_command(&stage_feature, p),
        "stage the feature tree",
    );
    let tree = {
        let out = run_libra_command(&["write-tree"], p);
        assert_cli_success(&out, "write-tree");
        stdout_trimmed(&out)
    };
    let feature = {
        let out = run_libra_command(&["commit-tree", &tree, "-p", &base, "-m", "feature"], p);
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/feature", &feature], p),
        "create refs/heads/feature",
    );

    // Put main's index back to the base tree: `side.txt` was only ever an index
    // entry, and the gitlink goes back to the pointer the base commit records.
    assert_cli_success(
        &run_libra_command(&["update-index", "--remove", "side.txt"], p),
        "unstage side.txt",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},vendor"),
            ],
            p,
        ),
        "restore the base gitlink",
    );

    std::fs::write(p.join("ours.txt"), "ours\n").expect("write ours.txt");
    assert_cli_success(&run_libra_command(&["add", "ours.txt"], p), "add ours.txt");
    if conflicting {
        std::fs::write(p.join("tracked.txt"), "ours edit\n").expect("write tracked.txt");
        assert_cli_success(
            &run_libra_command(&["add", "tracked.txt"], p),
            "add tracked.txt",
        );
    }
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ours", "--no-verify"], p),
        "commit ours",
    );

    repo
}

/// The `vendor` line of `libra ls-tree <rev>`, or `None` when the tree has no
/// such entry (which is what the silent-drop bug used to produce).
fn gitlink_tree_line(p: &Path, rev: &str) -> Option<String> {
    let out = run_libra_command(&["ls-tree", rev], p);
    assert_cli_success(&out, "ls-tree");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|line| line.ends_with("\tvendor"))
        .map(|line| line.to_string())
}

#[test]
fn merge_gitlink_divergent_pointer_is_refused_before_any_write() {
    let repo = create_gitlink_repo(GITLINK_MOVED);
    let p = repo.path();
    let head_before = head_commit(p);

    let output = run_libra_command(&["merge", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
    assert!(
        stderr.contains("'vendor'"),
        "the refusal must name the gitlink path, got: {stderr}"
    );
    assert!(
        stderr.contains("Libra does not support submodules"),
        "the refusal must say why it cannot be resolved, got: {stderr}"
    );
    // Fail-closed means fail BEFORE writing: no merge state, no moved HEAD, and
    // nothing from the other side staged or materialized.
    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "a refused merge must not record merge state"
    );
    assert!(
        !p.join("side.txt").exists(),
        "a refused merge must not write the other side's files"
    );
}

#[test]
fn merge_gitlink_agreed_pointer_passes_through_untouched() {
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();

    let output = run_libra_command(&["merge", "feature"], p);
    assert_cli_success(&output, "a submodule no side moved needs no decision");

    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "the merge result must keep the submodule pointer verbatim"
    );
    // Regression guard for the silent-drop this card removed: the merge must
    // also leave the repository clean, i.e. the index still records the gitlink.
    let status = run_libra_command(&["status", "--short"], p);
    assert_cli_success(&status, "status after merge");
    assert!(
        !String::from_utf8_lossy(&status.stdout).contains("vendor"),
        "the carried-through submodule must not show up as a change"
    );
}

#[test]
fn merge_gitlink_rebase_consumer_refuses_divergent_pointer() {
    let repo = create_gitlink_repo(GITLINK_MOVED);
    let p = repo.path();

    let head_before = head_commit(p);
    let output = run_libra_command(&["rebase", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
    // Same shared guard as merge: identical wording apart from the operation.
    assert!(
        stderr.contains(
            "rebase would have to merge the submodule (gitlink) entry 'vendor': Libra does not support submodules"
        ),
        "rebase must refuse through the shared gitlink guard, got: {stderr}"
    );
    // The refusal must land before the start path's writes: the aux sidecar,
    // the HEAD detach, and the rebase state row.
    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert!(
        !p.join(".libra").join("rebase-aux.json").exists(),
        "a refused rebase must not write the aux sidecar"
    );
    assert!(
        !p.join("side.txt").exists(),
        "a refused rebase must not materialize the replayed side"
    );
    let status = run_libra_command(&["status", "--short", "--branch"], p);
    assert_cli_success(&status, "status after a refused rebase");
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("## main"),
        "a refused rebase must leave the branch checked out, not a detached HEAD"
    );
}

#[test]
fn merge_gitlink_rebase_consumer_passes_through_agreed_pointer() {
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();

    let output = run_libra_command(&["rebase", "feature"], p);
    assert_cli_success(&output, "rebase over an unchanged submodule");

    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "the replayed commit must keep the submodule pointer verbatim"
    );
}

#[test]
fn merge_gitlink_cherry_pick_consumer_refuses_divergent_pointer() {
    let repo = create_gitlink_repo(GITLINK_MOVED);
    let p = repo.path();
    let feature = {
        let out = run_libra_command(&["rev-parse", "feature"], p);
        assert_cli_success(&out, "rev-parse feature");
        stdout_trimmed(&out)
    };

    let head_before = head_commit(p);
    let output = run_libra_command(&["cherry-pick", &feature], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
    assert!(
        stderr.contains(
            "cherry-pick would have to merge the submodule (gitlink) entry 'vendor': Libra does not support submodules"
        ),
        "cherry-pick must refuse through the shared gitlink guard, got: {stderr}"
    );
    // Refused before the first index/worktree/state write.
    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert!(
        !p.join("side.txt").exists(),
        "a refused pick must not materialize the picked side"
    );
    let status = run_libra_command(&["status", "--short"], p);
    assert_cli_success(&status, "status after a refused cherry-pick");
    assert!(
        !String::from_utf8_lossy(&status.stdout).contains("side.txt"),
        "a refused pick must not stage the picked side"
    );
}

#[test]
fn merge_gitlink_cherry_pick_consumer_passes_through_agreed_pointer() {
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    let feature = {
        let out = run_libra_command(&["rev-parse", "feature"], p);
        assert_cli_success(&out, "rev-parse feature");
        stdout_trimmed(&out)
    };

    let output = run_libra_command(&["cherry-pick", &feature], p);
    assert_cli_success(&output, "cherry-pick over an unchanged submodule");

    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "the picked commit must keep the submodule pointer verbatim"
    );
}

#[test]
fn merge_gitlink_agreed_pointer_survives_conflict_and_continue() {
    let repo = create_gitlink_repo_with(GITLINK_BASE, true);
    let p = repo.path();

    let conflicted = run_libra_command(&["merge", "feature"], p);
    let (_, report) = parse_cli_error_stderr(&conflicted.stderr);
    assert_eq!(
        report.error_code, "LBR-CONFLICT-002",
        "the fixture must stop on a real content conflict, not on the submodule"
    );

    std::fs::write(p.join("tracked.txt"), "resolved\n").expect("resolve the conflict");
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], p),
        "stage the resolution",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--continue", "--no-verify"], p),
        "finish the conflicted merge",
    );

    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "a submodule carried across a CONFLICTED merge must survive --continue"
    );
}

#[test]
fn merge_gitlink_divergent_pointer_refused_before_autostash_writes() {
    // The three-way engine's own gate runs after `--autostash` has created a
    // stash commit, fsynced its sidecar, and reset the working tree — so the
    // refusal has to happen in the wrapper, ahead of all three (ADR-MG-01 G1).
    let repo = create_gitlink_repo(GITLINK_MOVED);
    let p = repo.path();
    std::fs::write(p.join("tracked.txt"), "dirty\n").expect("dirty the worktree");

    let output = run_libra_command(&["merge", "--autostash", "feature"], p);
    let (_, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
    assert_eq!(
        std::fs::read_to_string(p.join("tracked.txt")).expect("read tracked.txt"),
        "dirty\n",
        "the refused merge must not have stashed and reset the working tree"
    );
    assert!(
        !p.join(".libra").join("merge-autostash.json").exists(),
        "a refused merge must not leave an autostash sidecar"
    );
    let stash = run_libra_command(&["stash", "list"], p);
    assert_cli_success(&stash, "stash list after a refused merge");
    assert!(
        String::from_utf8_lossy(&stash.stdout).trim().is_empty(),
        "a refused merge must not create a stash entry"
    );
}

#[test]
fn merge_gitlink_agreed_pointer_survives_a_conflicted_rebase_replay() {
    // A replay that BOTH carries a pass-through gitlink and stops on a real
    // content conflict: the conflict path stages and materializes the merged
    // entries, and a submodule pointer has no blob to write.
    let repo = create_gitlink_repo_with(GITLINK_BASE, true);
    let p = repo.path();

    let conflicted = run_libra_command(&["rebase", "feature"], p);
    let (_, report) = parse_cli_error_stderr(&conflicted.stderr);
    assert_eq!(
        report.error_code, "LBR-CONFLICT-001",
        "the fixture must stop on a content conflict, not on the submodule"
    );

    std::fs::write(p.join("tracked.txt"), "resolved\n").expect("resolve the conflict");
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], p),
        "stage the resolution",
    );
    assert_cli_success(
        &run_libra_command(&["rebase", "--continue"], p),
        "finish the conflicted rebase",
    );

    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "a submodule carried across a CONFLICTED replay must survive --continue"
    );
}

#[test]
fn merge_gitlink_hard_reset_restores_a_tree_carrying_a_pointer() {
    // `reset --hard` rebuilds the index from the tree and restores the working
    // tree from it; a gitlink names a SUBMODULE's commit, which is not an
    // object of this repository, so neither step may ask for it as a blob.
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    std::fs::write(p.join("ours.txt"), "dirty\n").expect("dirty a tracked file");
    // A user who checked the submodule out by hand: the gitlink path is a real
    // DIRECTORY, so the removal loop must not try to unlink it and the
    // untracked-overwrite check must not count its contents.
    std::fs::create_dir_all(p.join("vendor")).expect("materialize the submodule directory");
    std::fs::write(p.join("vendor").join("inner.txt"), "submodule\n").expect("submodule content");

    let output = run_libra_command(&["reset", "--hard", "HEAD"], p);
    assert_cli_success(&output, "hard reset in a repository carrying a gitlink");
    assert!(
        p.join("vendor").join("inner.txt").exists(),
        "a checked-out submodule directory is not Libra's to delete"
    );

    assert_eq!(
        std::fs::read_to_string(p.join("ours.txt")).expect("read ours.txt"),
        "ours\n",
        "the hard reset must still restore ordinary files"
    );
    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "the submodule pointer stays in the tree"
    );
    let files = run_libra_command(&["ls-files", "-s"], p);
    assert_cli_success(&files, "ls-files after a hard reset");
    assert!(
        String::from_utf8_lossy(&files.stdout)
            .lines()
            .any(|line| line.starts_with("160000") && line.ends_with("vendor")),
        "the rebuilt index keeps the gitlink entry"
    );
}

/// `main` carries `vendor` at [`GITLINK_BASE`]; `feature` is a DIRECT CHILD of
/// it that moves the pointer to [`GITLINK_MOVED`]. HEAD stays on `main`, so the
/// branch is an ancestor of `feature` — the shape `rebase` fast-forwards and
/// `cherry-pick --ff` fast-forwards, neither of which arbitrates anything.
fn create_gitlink_fast_forward_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},vendor"),
            ],
            p,
        ),
        "stage the base gitlink",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add submodule", "--no-verify"], p),
        "commit the base gitlink",
    );
    let base = head_commit(p);

    let side_blob = {
        let out = run_libra_command(&["hash-object", "-w", "--stdin"], p);
        assert!(out.status.success(), "hash-object must succeed");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("100644,{side_blob},side.txt"),
                "--cacheinfo",
                &format!("160000,{GITLINK_MOVED},vendor"),
            ],
            p,
        ),
        "stage the child tree",
    );
    let tree = {
        let out = run_libra_command(&["write-tree"], p);
        assert_cli_success(&out, "write-tree");
        stdout_trimmed(&out)
    };
    let child = {
        let out = run_libra_command(
            &["commit-tree", &tree, "-p", &base, "-m", "move submodule"],
            p,
        );
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/feature", &child], p),
        "create refs/heads/feature",
    );
    assert_cli_success(
        &run_libra_command(&["update-index", "--remove", "side.txt"], p),
        "unstage side.txt",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},vendor"),
            ],
            p,
        ),
        "restore the base gitlink",
    );

    repo
}

#[test]
fn merge_gitlink_fast_forward_rebase_adopts_a_moved_pointer() {
    // A rebase whose merge base IS the branch tip fast-forwards onto the
    // upstream tree wholesale: nothing is arbitrated, so a MOVED submodule
    // pointer must be adopted rather than refused.
    let repo = create_gitlink_fast_forward_repo();
    let p = repo.path();

    let output = run_libra_command(&["rebase", "feature"], p);
    assert_cli_success(&output, "fast-forward rebase over a moved submodule");

    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_MOVED}\tvendor").as_str()),
        "the fast-forward adopts the upstream pointer"
    );
}

#[test]
fn merge_gitlink_fast_forward_pick_adopts_a_moved_pointer_only_with_ff() {
    // `cherry-pick --ff` on a direct child of HEAD advances HEAD without
    // replaying, so it decides nothing and a moved pointer is adopted. The same
    // pick WITHOUT `--ff` performs a three-way apply and must be refused — the
    // preflight has to tell the two apart.
    let repo = create_gitlink_fast_forward_repo();
    let p = repo.path();
    let child = {
        let out = run_libra_command(&["rev-parse", "feature"], p);
        assert_cli_success(&out, "rev-parse feature");
        stdout_trimmed(&out)
    };

    let replayed = run_libra_command(&["cherry-pick", &child], p);
    let (_, report) = parse_cli_error_stderr(&replayed.stderr);
    assert_eq!(
        report.error_code, "LBR-UNSUPPORTED-001",
        "a replaying pick still has to arbitrate the moved pointer"
    );

    let output = run_libra_command(&["cherry-pick", "--ff", &child], p);
    assert_cli_success(&output, "fast-forward pick over a moved submodule");
    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_MOVED}\tvendor").as_str()),
        "the fast-forward pick adopts the moved pointer"
    );
}

/// Run `git` in `cwd`, asserting success. Used only by the `pull --rebase`
/// fixture below: `pull` needs a real remote, and git is the one tool that can
/// author a gitlink-bearing history on the far side.
fn git_in(args: &[&str], cwd: &Path) -> String {
    let output = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("failed to execute git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is utf8")
        .trim()
        .to_string()
}

#[test]
fn merge_gitlink_pull_rebase_refuses_before_its_autostash() {
    // `pull --rebase` is a SECOND entry into the replay and pushes its own
    // autostash before handing off, so the refusal has to come from `pull`
    // itself — the gate inside the rebase start path would already be too late.
    let temp_root = tempfile::tempdir().expect("temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let work_dir = temp_root.path().join("workdir");
    git_in(
        &["init", "--bare", remote_dir.to_str().unwrap()],
        temp_root.path(),
    );
    git_in(&["init", work_dir.to_str().unwrap()], temp_root.path());
    git_in(&["config", "user.name", "Libra Tester"], &work_dir);
    git_in(&["config", "user.email", "tester@example.com"], &work_dir);

    std::fs::write(work_dir.join("README.md"), "hello\n").expect("write README");
    git_in(&["add", "README.md"], &work_dir);
    git_in(
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{GITLINK_BASE},vendor"),
        ],
        &work_dir,
    );
    git_in(&["commit", "-m", "initial commit"], &work_dir);
    let branch = git_in(&["rev-parse", "--abbrev-ref", "HEAD"], &work_dir);
    git_in(
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
        &work_dir,
    );
    git_in(
        &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
        &work_dir,
    );

    let local = tempfile::tempdir().expect("local repo");
    let p = local.path();
    assert_cli_success(&run_libra_command(&["init"], p), "libra init");
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "Libra Tester"], p),
        "set user.name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "tester@example.com"], p),
        "set user.email",
    );
    assert_cli_success(
        &run_libra_command(
            &["remote", "add", "origin", remote_dir.to_str().unwrap()],
            p,
        ),
        "remote add",
    );
    assert_cli_success(
        &run_libra_command(&["config", "branch.main.remote", "origin"], p),
        "set branch.main.remote",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "config",
                "branch.main.merge",
                &format!("refs/heads/{branch}"),
            ],
            p,
        ),
        "set branch.main.merge",
    );
    assert_cli_success(&run_libra_command(&["pull"], p), "initial pull");
    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "the fetched history carries the submodule pointer"
    );

    // Diverge: a local commit, and a remote commit that MOVES the pointer.
    std::fs::write(p.join("local.txt"), "local\n").expect("write local.txt");
    assert_cli_success(
        &run_libra_command(&["add", "local.txt"], p),
        "add local.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "local work", "--no-verify"], p),
        "local commit",
    );
    let head_before = head_commit(p);

    git_in(
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{GITLINK_MOVED},vendor"),
        ],
        &work_dir,
    );
    std::fs::write(work_dir.join("remote.txt"), "remote\n").expect("write remote.txt");
    git_in(&["add", "remote.txt"], &work_dir);
    git_in(&["commit", "-m", "move submodule"], &work_dir);
    git_in(
        &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
        &work_dir,
    );

    std::fs::write(p.join("local.txt"), "dirty\n").expect("dirty the worktree");
    let output = run_libra_command(&["pull", "--rebase", "--autostash"], p);
    let (_, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert_eq!(
        std::fs::read_to_string(p.join("local.txt")).expect("read local.txt"),
        "dirty\n",
        "the refused pull must not have stashed and reset the working tree"
    );
    let stash = run_libra_command(&["stash", "list"], p);
    assert_cli_success(&stash, "stash list after a refused pull --rebase");
    assert!(
        String::from_utf8_lossy(&stash.stdout).trim().is_empty(),
        "a refused pull must not create a stash entry"
    );
}

/// `main` has no submodule; `feature` is a direct child that ADDS `vendor` at
/// [`GITLINK_BASE`]. HEAD stays on `main`, so the merge fast-forwards.
fn create_gitlink_adding_fast_forward_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let base = head_commit(p);

    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},vendor"),
            ],
            p,
        ),
        "stage the gitlink",
    );
    let tree = {
        let out = run_libra_command(&["write-tree"], p);
        assert_cli_success(&out, "write-tree");
        stdout_trimmed(&out)
    };
    let child = {
        let out = run_libra_command(
            &["commit-tree", &tree, "-p", &base, "-m", "add submodule"],
            p,
        );
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/feature", &child], p),
        "create refs/heads/feature",
    );
    assert_cli_success(
        &run_libra_command(&["update-index", "--remove", "vendor"], p),
        "unstage the gitlink",
    );

    repo
}

#[test]
fn merge_gitlink_refuses_to_replace_an_untracked_file_at_the_pointer_path() {
    // Materializing a `160000` entry creates a DIRECTORY placeholder, which
    // would delete a plain file sitting exactly there. That path is matched
    // exactly (files UNDER a submodule directory belong to the submodule and
    // are not overwritten), and the refusal lands before HEAD moves.
    let repo = create_gitlink_adding_fast_forward_repo();
    let p = repo.path();
    std::fs::write(p.join("vendor"), "not a submodule\n").expect("write untracked file");
    let head_before = head_commit(p);

    let output = run_libra_command(&["merge", "feature"], p);
    let (_, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert_eq!(
        std::fs::read_to_string(p.join("vendor")).expect("read the untracked file"),
        "not a submodule\n",
        "the untracked file must survive the refusal"
    );
}

#[test]
fn merge_gitlink_leaves_a_checked_out_submodule_directory_untouched() {
    // The same fast-forward, but the path is a real submodule checkout: its
    // files are untracked and live UNDER the pointer, so they neither block the
    // merge nor get deleted by it.
    let repo = create_gitlink_adding_fast_forward_repo();
    let p = repo.path();
    std::fs::create_dir_all(p.join("vendor")).expect("materialize the submodule");
    std::fs::write(p.join("vendor").join("inner.txt"), "submodule\n").expect("submodule content");

    let output = run_libra_command(&["merge", "feature"], p);
    assert_cli_success(&output, "fast-forward adopting a checked-out submodule");

    assert_eq!(
        std::fs::read_to_string(p.join("vendor").join("inner.txt")).expect("read submodule file"),
        "submodule\n",
        "the submodule checkout is not Libra's to touch"
    );
    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "the pointer is adopted"
    );
}

/// [`create_gitlink_repo`] plus a `dropped` branch: a direct child of HEAD whose
/// tree no longer declares `vendor`.
fn create_gitlink_dropping_repo() -> tempfile::TempDir {
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    let base = head_commit(p);
    assert_cli_success(
        &run_libra_command(&["update-index", "--remove", "vendor"], p),
        "stage the removal",
    );
    let tree = {
        let out = run_libra_command(&["write-tree"], p);
        assert_cli_success(&out, "write-tree");
        stdout_trimmed(&out)
    };
    let child = {
        let out = run_libra_command(
            &["commit-tree", &tree, "-p", &base, "-m", "drop submodule"],
            p,
        );
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/dropped", &child], p),
        "create refs/heads/dropped",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},vendor"),
            ],
            p,
        ),
        "restore the gitlink",
    );
    repo
}

#[test]
fn merge_gitlink_fast_forward_dropping_a_pointer_refuses_before_moving_head() {
    // `restore` refuses to replace a NON-EMPTY materialized submodule directory
    // (its own long-standing contract). The fast-forward restores AFTER moving
    // the ref, so that refusal has to be raised beforehand — otherwise the
    // branch ends up ahead of the index and the working tree.
    let repo = create_gitlink_dropping_repo();
    let p = repo.path();
    std::fs::create_dir_all(p.join("vendor")).expect("materialize the submodule");
    std::fs::write(p.join("vendor").join("inner.txt"), "submodule\n").expect("submodule content");
    let head_before = head_commit(p);

    let output = run_libra_command(&["merge", "dropped"], p);
    let (stderr, _) = parse_cli_error_stderr(&output.stderr);

    assert!(
        stderr.contains("refusing to replace non-empty worktree directory 'vendor'"),
        "the refusal must name the submodule directory, got: {stderr}"
    );
    assert_eq!(
        head_commit(p),
        head_before,
        "the refusal must land BEFORE the ref moves"
    );
    assert!(
        p.join("vendor").join("inner.txt").exists(),
        "the submodule checkout survives"
    );
    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "HEAD still declares the pointer"
    );
}

#[test]
fn merge_gitlink_fast_forward_dropping_a_pointer_clears_an_empty_placeholder() {
    // With only Libra's own empty placeholder there, the same fast-forward
    // completes and HEAD, the index and the working tree all agree.
    let repo = create_gitlink_dropping_repo();
    let p = repo.path();
    std::fs::create_dir_all(p.join("vendor")).expect("materialize the placeholder");

    let output = run_libra_command(&["merge", "dropped"], p);
    assert_cli_success(&output, "fast-forward dropping an unmaterialized submodule");

    assert_eq!(
        gitlink_tree_line(p, "HEAD"),
        None,
        "the tree drops the pointer"
    );
    let files = run_libra_command(&["ls-files", "-s"], p);
    assert_cli_success(&files, "ls-files after the drop");
    assert!(
        !String::from_utf8_lossy(&files.stdout).contains("vendor"),
        "the index drops the pointer too"
    );
}

#[test]
fn merge_gitlink_restore_refuses_to_delete_an_untracked_file_at_the_pointer_path() {
    // `restore --source` materializes a `160000` entry as a directory
    // placeholder, which would remove a plain file sitting exactly there. An
    // UNTRACKED file is the user's, so the whole restore is refused before any
    // write; files BENEATH a checked-out submodule are untouched either way.
    let repo = create_gitlink_adding_fast_forward_repo();
    let p = repo.path();
    std::fs::write(p.join("vendor"), "not a submodule\n").expect("write untracked file");

    let output = run_libra_command(&["restore", "--source", "feature", "--worktree", "."], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("refusing to replace worktree path 'vendor'"),
        "the refusal must name the path, got: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("vendor")).expect("read the untracked file"),
        "not a submodule\n",
        "the untracked file must survive"
    );
}

#[test]
fn merge_gitlink_cleanliness_gate_ignores_a_submodule_but_not_a_plain_file() {
    // The gate (`status::changes_to_be_staged`, read by
    // `switch::ensure_clean_status`) ignores the two shapes Libra expects for a
    // `160000` entry — absent (never materialized) and a directory (checked out
    // by the user) — otherwise no repository containing a submodule could merge
    // at all. A plain file at that path is neither, and must NOT be waved
    // through silently.
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], p),
        "an unmaterialized submodule must not read as a dirty worktree",
    );

    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    std::fs::create_dir_all(p.join("vendor")).expect("materialize the submodule");
    std::fs::write(p.join("vendor").join("inner.txt"), "submodule\n").expect("submodule content");
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], p),
        "a checked-out submodule directory must not read as a dirty worktree",
    );

    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    std::fs::write(p.join("vendor"), "not a submodule\n").expect("write a file at the path");
    let refused = run_libra_command(&["merge", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&refused.stderr);
    assert_eq!(
        report.error_code, "LBR-CONFLICT-002",
        "a plain file where a submodule belongs is a dirty worktree, not a no-op"
    );
    assert!(
        stderr.contains("uncommitted changes"),
        "the gate must name the dirty worktree, got: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("vendor")).expect("read the file"),
        "not a submodule\n"
    );
}

#[test]
fn merge_gitlink_multi_pick_refusal_survives_a_later_empty_commit() {
    // The whole-sequence preflight stops modelling at a pick the sequencer
    // would reject for its own reason (here: an empty commit without
    // `--allow-empty`). It must still decide on what it modelled so far —
    // otherwise the divergent pointer in the FIRST pick escapes the gate and is
    // applied before the per-pick guard refuses it.
    let repo = create_gitlink_repo(GITLINK_MOVED);
    let p = repo.path();
    let diverging = {
        let out = run_libra_command(&["rev-parse", "feature"], p);
        assert_cli_success(&out, "rev-parse feature");
        stdout_trimmed(&out)
    };
    // An empty commit on top of `feature`: same tree, so the pick would stop
    // with `EmptyCommit` rather than a gitlink verdict.
    let feature_tree = {
        let out = run_libra_command(&["rev-parse", "feature^{tree}"], p);
        assert_cli_success(&out, "rev-parse feature tree");
        stdout_trimmed(&out)
    };
    let empty = {
        let out = run_libra_command(
            &[
                "commit-tree",
                &feature_tree,
                "-p",
                &diverging,
                "-m",
                "empty",
            ],
            p,
        );
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    let head_before = head_commit(p);

    let output = run_libra_command(&["cherry-pick", &diverging, &empty], p);
    let (_, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(
        report.error_code, "LBR-UNSUPPORTED-001",
        "the first pick's divergent pointer must still be refused"
    );
    assert_eq!(head_commit(p), head_before, "no pick may be applied");
}

#[test]
fn merge_gitlink_restore_refuses_a_file_left_where_the_index_records_a_pointer() {
    // "Tracked" is not enough to make a path replaceable: the index entry here
    // IS the gitlink, so the file at that path was never written by Libra and
    // is not recoverable from the object store. Only ordinary tracked content
    // may be replaced by the directory placeholder.
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    std::fs::write(p.join("vendor"), "left behind\n").expect("write a file at the path");

    let output = run_libra_command(&["restore", "--source", "HEAD", "--worktree", "."], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("refusing to replace worktree path 'vendor'"),
        "the refusal must name the path, got: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("vendor")).expect("read the file"),
        "left behind\n"
    );
}

#[test]
fn merge_gitlink_hard_reset_keeps_a_file_left_where_the_index_records_a_pointer() {
    // `reset --hard` to a tree WITHOUT the pointer must not delete that file
    // either: Libra never materialized the submodule, so nothing at that path
    // is its to remove. (`cherry-pick --ff` delegates here.)
    let repo = create_gitlink_dropping_repo();
    let p = repo.path();
    std::fs::write(p.join("vendor"), "left behind\n").expect("write a file at the path");
    let dropped = {
        let out = run_libra_command(&["rev-parse", "dropped"], p);
        assert_cli_success(&out, "rev-parse dropped");
        stdout_trimmed(&out)
    };

    let output = run_libra_command(&["reset", "--hard", &dropped], p);
    assert_cli_success(&output, "hard reset dropping the pointer");

    assert_eq!(
        std::fs::read_to_string(p.join("vendor")).expect("read the file"),
        "left behind\n",
        "a file at a former submodule path is not Libra's to delete"
    );
}

#[test]
fn merge_gitlink_restore_refuses_to_drop_a_pointer_over_user_content() {
    // The other direction across the same path: the source no longer declares
    // the submodule, so a plain restore would REMOVE whatever is there. That
    // content is the user's — Libra never wrote it — so the restore refuses
    // before touching anything.
    let repo = create_gitlink_dropping_repo();
    let p = repo.path();
    std::fs::write(p.join("vendor"), "left behind\n").expect("write a file at the path");

    let output = run_libra_command(&["restore", "--source", "dropped", "--worktree", "."], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("refusing to replace worktree path 'vendor'"),
        "the refusal must name the path, got: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("vendor")).expect("read the file"),
        "left behind\n"
    );
}

#[test]
fn merge_gitlink_overlay_restore_still_refuses_a_source_present_replacement() {
    // `--overlay` only suppresses DELETION of paths the source omits. A source
    // that REPLACES the submodule with an ordinary blob still writes, so the
    // guard must stay armed for it.
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    let base = head_commit(p);
    let blob = {
        let out = run_libra_command_with_stdin(&["hash-object", "-w", "--stdin"], p, "replaced\n");
        assert!(out.status.success(), "hash-object must succeed");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("100644,{blob},vendor"),
            ],
            p,
        ),
        "stage a blob at the submodule path",
    );
    let tree = {
        let out = run_libra_command(&["write-tree"], p);
        assert_cli_success(&out, "write-tree");
        stdout_trimmed(&out)
    };
    let replaced = {
        let out = run_libra_command(
            &["commit-tree", &tree, "-p", &base, "-m", "submodule to file"],
            p,
        );
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/replaced", &replaced], p),
        "create refs/heads/replaced",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},vendor"),
            ],
            p,
        ),
        "restore the gitlink",
    );
    std::fs::write(p.join("vendor"), "left behind\n").expect("write a file at the path");

    let output = run_libra_command(
        &[
            "restore",
            "--overlay",
            "--source",
            "replaced",
            "--worktree",
            ".",
        ],
        p,
    );
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("refusing to replace worktree path 'vendor'"),
        "the refusal must name the path, got: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("vendor")).expect("read the file"),
        "left behind\n"
    );
}

#[test]
fn merge_gitlink_fast_forward_rebase_refuses_before_moving_the_ref() {
    // `rebase`'s fast-forward branch materializes the worktree AFTER updating
    // the ref and the index, so a transition the materialization would refuse
    // (here: a non-empty submodule directory the upstream tree drops) has to be
    // caught first — otherwise the branch runs ahead of the working tree.
    let repo = create_gitlink_dropping_repo();
    let p = repo.path();
    std::fs::create_dir_all(p.join("vendor")).expect("materialize the submodule");
    std::fs::write(p.join("vendor").join("inner.txt"), "submodule\n").expect("submodule content");
    let head_before = head_commit(p);

    let output = run_libra_command(&["rebase", "dropped"], p);
    let (stderr, _) = parse_cli_error_stderr(&output.stderr);

    assert!(
        stderr.contains("refusing to replace non-empty worktree directory 'vendor'"),
        "the refusal must name the submodule directory, got: {stderr}"
    );
    assert_eq!(
        head_commit(p),
        head_before,
        "the refusal must land BEFORE the ref moves"
    );
    assert!(
        p.join("vendor").join("inner.txt").exists(),
        "the submodule checkout survives"
    );
}

// ---------------------------------------------------------------------------
// MG-02: criss-cross histories (several merge bases) and the recursive virtual
// ancestor they are folded into.
// ---------------------------------------------------------------------------

/// A criss-cross history, the shape Git's `t6024-recursive-merge.sh`
/// (git@`3cb9185f6`) is built around: two branches that merged each other, so
/// the two tips below have TWO merge bases and neither dominates the other.
///
/// ```text
///            ┌─ a(f=1) ─┐    x = merge(a, b) ── ours (adds t)
///   main(o) ─┤          ├────
///            └─ b(g=1) ─┘    y = merge(b, a) ── theirs (f=2, g=2)
/// ```
///
/// `merge_bases(ours, theirs) == {a, b}`, and the fixture is chosen so that
/// EITHER single base gives the wrong answer: relative to `a` the `g` edits on
/// both sides look divergent, relative to `b` the `f` edits do. Only the
/// recursive ancestor (`f=1, g=1`) explains both sides' history.
fn create_crisscross_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("f.txt"), "0\n").expect("write f");
    std::fs::write(p.join("g.txt"), "0\n").expect("write g");
    assert_cli_success(
        &run_libra_command(&["add", "f.txt", "g.txt"], p),
        "add roots",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "commit root",
    );

    for (branch, file, content) in [("a", "f.txt", "1\n"), ("b", "g.txt", "1\n")] {
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], p),
            "checkout main",
        );
        assert_cli_success(&run_libra_command(&["branch", branch], p), "create branch");
        assert_cli_success(
            &run_libra_command(&["checkout", branch], p),
            "checkout branch",
        );
        commit_file(p, file, content, "side edit");
    }

    // The two merges have to be made from THROWAWAY branches: merging `b` into
    // `a` itself would leave `a` an ancestor of the result, and the second
    // merge would fast-forward instead of criss-crossing.
    for (from, tip, other) in [("a", "x", "b"), ("b", "y", "a")] {
        assert_cli_success(&run_libra_command(&["checkout", from], p), "checkout side");
        assert_cli_success(&run_libra_command(&["branch", tip], p), "create tip branch");
        assert_cli_success(&run_libra_command(&["checkout", tip], p), "checkout tip");
        assert_cli_success(
            &run_libra_command(&["merge", other], p),
            "criss-cross merge",
        );
    }

    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "checkout x");
    commit_file(p, "t.txt", "ours\n", "ours-only file");

    assert_cli_success(&run_libra_command(&["checkout", "y"], p), "checkout y");
    std::fs::write(p.join("f.txt"), "2\n").expect("write f");
    std::fs::write(p.join("g.txt"), "2\n").expect("write g");
    assert_cli_success(
        &run_libra_command(&["add", "f.txt", "g.txt"], p),
        "add edits",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "theirs edits", "--no-verify"], p),
        "commit theirs",
    );

    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "checkout x");
    repo
}

fn read_merge_state(p: &Path) -> serde_json::Value {
    let raw = std::fs::read(p.join(".libra").join("merge-state.json"))
        .expect("merge-state.json written by a conflicted merge");
    serde_json::from_slice(&raw).expect("merge-state.json is json")
}

/// G6: with two merge bases, folding them into a virtual ancestor merges
/// cleanly where picking either single base reports a conflict.
#[test]
fn merge_crisscross_folds_both_bases_and_merges_cleanly() {
    let repo = create_crisscross_repo();
    let p = repo.path();

    let output = run_libra_command(&["--json", "merge", "y"], p);
    assert_cli_success(&output, "criss-cross merge");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["strategy"], "three-way");
    assert!(
        json["data"]["conflicted_paths"].is_null(),
        "a clean merge omits the key entirely (frozen schema): {json}"
    );
    assert_eq!(
        json["data"]["files_changed"], 2,
        "only the two files `theirs` re-edited change"
    );

    assert_eq!(
        std::fs::read_to_string(p.join("f.txt")).expect("f"),
        "2\n",
        "the virtual ancestor already carries f=1, so theirs' edit applies cleanly"
    );
    assert_eq!(std::fs::read_to_string(p.join("g.txt")).expect("g"), "2\n");
    assert_eq!(
        std::fs::read_to_string(p.join("t.txt")).expect("t"),
        "ours\n",
        "our side's own file survives"
    );

    let raw = run_libra_command(&["cat-file", "-p", "HEAD"], p);
    assert_cli_success(&raw, "cat-file the merge commit");
    assert_eq!(
        String::from_utf8_lossy(&raw.stdout)
            .lines()
            .filter(|line| line.starts_with("parent "))
            .count(),
        2,
        "a criss-cross merge still records the two REAL parents, not the virtual ancestor"
    );
}

/// G6 (conflict half) + G7: when the sides really do diverge relative to the
/// virtual ancestor the merge conflicts as usual — and the state it writes
/// records no `base`, because a virtual ancestor is a one-shot object that must
/// not become a GC root (ADR-MG-04).
#[test]
fn merge_crisscross_conflict_records_no_virtual_base_in_the_state() {
    let repo = create_crisscross_repo();
    let p = repo.path();
    // Diverge from the virtual ancestor (f=1) on OUR side too, so f is a real
    // both-modified conflict; g stays clean.
    commit_file(p, "f.txt", "ours-2\n", "ours re-edits f");

    let output = run_libra_command(&["merge", "y"], p);
    assert_eq!(
        output.status.code(),
        Some(128),
        "the merge conflicts (LBR-CONFLICT-002)"
    );
    let conflicted = std::fs::read_to_string(p.join("f.txt")).expect("f");
    assert!(
        conflicted.contains("<<<<<<< HEAD") && conflicted.contains("ours-2"),
        "the OUTER conflict keeps the default seven-character markers: {conflicted}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("g.txt")).expect("g"),
        "2\n",
        "the path the virtual ancestor explains still merges cleanly"
    );

    let state = read_merge_state(p);
    assert!(
        state.get("base").is_none_or(serde_json::Value::is_null),
        "the virtual ancestor is never recorded as the merge base: {state}"
    );
    assert!(
        state["conflicted_paths"]
            .as_array()
            .expect("conflicted_paths")
            .iter()
            .any(|path| path == "f.txt"),
        "the conflicted path is recorded: {state}"
    );
}

/// The single-base path is untouched by MG-02: an ordinary diverged merge still
/// records its real base and merges exactly as before.
#[test]
fn merge_crisscross_single_base_merge_is_unchanged() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "shared.txt", "base\n", "shared base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(p, "feature.txt", "feature\n", "feature file");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    commit_file(p, "main.txt", "main\n", "main file");
    let base = head_commit(p);

    let output = run_libra_command(&["--json", "merge", "feature"], p);
    assert_cli_success(&output, "single-base merge");
    assert_eq!(parse_json_stdout(&output)["data"]["strategy"], "three-way");
    assert_ne!(head_commit(p), base, "a merge commit was created");
    assert!(p.join("feature.txt").exists() && p.join("main.txt").exists());
}

/// `--allow-unrelated-histories` keeps its virtual EMPTY base: zero merge bases
/// is still zero, not something the fold is asked to build.
#[test]
fn merge_crisscross_unrelated_histories_keep_the_empty_base() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["checkout", "--orphan", "imported"], p),
        "orphan branch",
    );
    std::fs::write(p.join("imported.txt"), "imported\n").expect("write imported");
    assert_cli_success(&run_libra_command(&["add", "imported.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "imported root", "--no-verify"], p),
        "commit orphan",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");

    let refused = run_libra_command(&["merge", "imported"], p);
    assert_eq!(refused.status.code(), Some(128), "unrelated by default");

    let output = run_libra_command(
        &["--json", "merge", "--allow-unrelated-histories", "imported"],
        p,
    );
    assert_cli_success(&output, "unrelated merge with the empty base");
    assert_eq!(parse_json_stdout(&output)["data"]["strategy"], "three-way");
    assert!(p.join("imported.txt").exists() && p.join("tracked.txt").exists());
}

/// `--restart` recomputes the virtual ancestor from the REAL bases and lands on
/// the same conflict — the fold is deterministic (bases folded in hex order).
#[test]
fn merge_crisscross_restart_recomputes_the_virtual_ancestor() {
    let repo = create_crisscross_repo();
    let p = repo.path();
    commit_file(p, "f.txt", "ours-2\n", "ours re-edits f");
    let ours = head_commit(p);

    assert_eq!(
        run_libra_command(&["merge", "y"], p).status.code(),
        Some(128),
        "the merge conflicts"
    );
    let first = std::fs::read_to_string(p.join("f.txt")).expect("f");
    // Overwrite the conflict resolution: --restart must discard it.
    std::fs::write(p.join("f.txt"), "hand-resolved\n").expect("resolve");

    let restarted = run_libra_command(&["merge", "--restart"], p);
    assert_eq!(
        restarted.status.code(),
        Some(128),
        "the restart re-conflicts"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("f.txt")).expect("f"),
        first,
        "the recomputed ancestor reproduces the same conflict byte for byte"
    );
    assert_eq!(head_commit(p), ours, "HEAD is still the pre-merge commit");
}

/// Every loose object currently on disk, as `<dir><file>` object ids.
fn loose_object_ids(p: &Path) -> std::collections::BTreeSet<String> {
    let mut ids = std::collections::BTreeSet::new();
    let objects = p.join(".libra").join("objects");
    let Ok(dirs) = std::fs::read_dir(&objects) else {
        return ids;
    };
    for dir in dirs.flatten() {
        let prefix = dir.file_name().to_string_lossy().to_string();
        if prefix.len() != 2 {
            continue;
        }
        if let Ok(files) = std::fs::read_dir(dir.path()) {
            for file in files.flatten() {
                ids.insert(format!("{prefix}{}", file.file_name().to_string_lossy()));
            }
        }
    }
    ids
}

/// Age every loose object past the prune grace window and drive the GC
/// quarantine's two phases explicitly, the way `maintenance_test` does, instead
/// of waiting an hour.
fn prune_unreachable_objects(p: &Path) -> String {
    let objects = p.join(".libra").join("objects");
    let aged = std::process::Command::new("find")
        .arg(&objects)
        .args([
            "-type",
            "f",
            "-exec",
            "touch",
            "-t",
            "200001010000",
            "{}",
            ";",
        ])
        .status()
        .expect("spawn find");
    assert!(aged.success(), "backdate the loose objects");

    let first = run_libra_command(&["maintenance", "run", "--task", "gc"], p);
    assert_cli_success(&first, "gc quarantines the unreachable objects");
    let ledger_path = p.join(".libra").join("gc-prune-candidates.json");
    let ledger: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&ledger_path).expect("ledger")).expect("ledger json");
    let aged_ledger: serde_json::Map<String, serde_json::Value> = ledger
        .as_object()
        .expect("ledger object")
        .keys()
        .map(|oid| (oid.clone(), serde_json::json!(0)))
        .collect();
    assert!(
        !aged_ledger.is_empty(),
        "something unreachable must have been quarantined"
    );
    std::fs::write(
        &ledger_path,
        serde_json::to_vec(&serde_json::Value::Object(aged_ledger)).expect("serialize"),
    )
    .expect("age the ledger");

    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], p);
    assert_cli_success(&gc, "gc prunes without a dangling-reference error");
    String::from_utf8_lossy(&gc.stdout).to_string()
}

/// G8 + G9: the virtual ancestor is NOT a GC root. `maintenance gc` reclaims
/// the exact objects the fold created, mid-merge, without any
/// dangling-reference complaint — and `--restart` brings those same object ids
/// back, which is ADR-MG-04's whole recovery contract (the fold is
/// deterministic, so recomputation is bit-identical).
#[test]
fn merge_crisscross_gc_reclaims_the_virtual_ancestor_and_restart_recovers() {
    let repo = create_crisscross_repo();
    let p = repo.path();
    commit_file(p, "f.txt", "ours-2\n", "ours re-edits f");

    let before_merge = loose_object_ids(p);
    assert_eq!(
        run_libra_command(&["merge", "y"], p).status.code(),
        Some(128),
        "the merge conflicts and leaves state behind"
    );
    let conflicted = std::fs::read_to_string(p.join("f.txt")).expect("f");
    let created: std::collections::BTreeSet<String> = loose_object_ids(p)
        .difference(&before_merge)
        .cloned()
        .collect();
    assert!(
        !created.is_empty(),
        "the merge writes the virtual ancestor's objects"
    );

    let target_before = read_merge_state(p)["target"].clone();
    let gc_out = prune_unreachable_objects(p);
    let after_gc = loose_object_ids(p);
    let pruned: Vec<String> = created.difference(&after_gc).cloned().collect();
    // The synthetic COMMIT is always new; its tree usually is not, because
    // folding two bases reproduces a tree an earlier merge already wrote and
    // object storage is content-addressed. What matters is that whatever the
    // fold DID add is unrooted and reclaimable.
    assert!(
        !pruned.is_empty(),
        "the virtual ancestor is unrooted, so gc takes it (created={created:?}, \
         gc said: {gc_out})"
    );
    assert_eq!(
        read_merge_state(p)["target"],
        target_before,
        "the merge state survives the prune intact — it never named the virtual ancestor, \
         so the sidecar root check has nothing to fail closed on"
    );

    // Recovery does not depend on the reclaimed objects: --restart rebuilds the
    // ancestor from the real merge bases, byte for byte.
    let restarted = run_libra_command(&["merge", "--restart"], p);
    assert_eq!(
        restarted.status.code(),
        Some(128),
        "the restart re-runs the merge and re-conflicts"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("f.txt")).expect("f"),
        conflicted,
        "the ancestor recomputed after the prune is the same one"
    );
    let after_restart = loose_object_ids(p);
    let missing: Vec<&String> = pruned
        .iter()
        .filter(|oid| !after_restart.contains(*oid))
        .collect();
    assert!(
        missing.is_empty(),
        "every reclaimed object is recomputed by --restart: {missing:?}"
    );
}

/// G10: a recursive merge adds no field to `merge-state.json`, so a state file
/// in the pre-existing schema still drives `--abort` to completion.
#[test]
fn merge_crisscross_merge_state_keeps_the_older_schema_readable() {
    let repo = create_crisscross_repo();
    let p = repo.path();
    commit_file(p, "f.txt", "ours-2\n", "ours re-edits f");
    let ours = head_commit(p);

    assert_eq!(
        run_libra_command(&["merge", "y"], p).status.code(),
        Some(128),
        "the merge conflicts"
    );
    let state = read_merge_state(p);
    let known = [
        "head_name",
        "orig_head",
        "target",
        "target_ref",
        "base",
        "strategy",
        "allow_unrelated_histories",
        "skip_hooks",
        "conflicted_paths",
        "message",
        // Injected at the JSON layer by `MergeState::save` (W2 worktree
        // ownership), not part of the merge's own schema.
        "owner_scope",
    ];
    for key in state.as_object().expect("state object").keys() {
        assert!(
            known.contains(&key.as_str()),
            "a recursive merge must not grow the state schema; found '{key}'"
        );
    }

    // Rewrite it in the pre-P1-07b shape (no strategy / unrelated / hook flags,
    // no base) and confirm it is still a state this binary can finish.
    let old_schema = serde_json::json!({
        "owner_scope": state["owner_scope"],
        "head_name": state["head_name"],
        "orig_head": state["orig_head"],
        "target": state["target"],
        "target_ref": state["target_ref"],
        "conflicted_paths": state["conflicted_paths"],
        "message": state["message"],
    });
    std::fs::write(
        p.join(".libra").join("merge-state.json"),
        serde_json::to_vec(&old_schema).expect("serialize"),
    )
    .expect("write old-schema state");

    let aborted = run_libra_command(&["merge", "--abort"], p);
    assert_cli_success(&aborted, "abort reads the older state schema");
    assert_eq!(head_commit(p), ours);
    assert_eq!(
        std::fs::read_to_string(p.join("f.txt")).expect("f"),
        "ours-2\n"
    );
}

/// A criss-cross `--dry-run` previews the folded result and still writes
/// nothing: the fold keeps its blobs in memory and materializes no virtual
/// tree or commit when the merge is only being previewed.
#[test]
fn merge_crisscross_dry_run_previews_without_writing_objects() {
    let repo = create_crisscross_repo();
    let p = repo.path();
    let head_before = head_commit(p);
    let objects_before = count_loose_objects(p);

    let output = run_libra_command(&["--json", "merge", "--dry-run", "y"], p);
    assert_cli_success(&output, "criss-cross dry run");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["dry_run"], true);
    assert!(
        json["data"]["would_conflict"].is_null(),
        "a clean preview omits `would_conflict` (frozen schema): {json}"
    );
    assert_eq!(json["data"]["files_changed"], 2);

    assert_eq!(count_loose_objects(p), objects_before, "no objects written");
    assert_eq!(head_commit(p), head_before);
    assert!(!p.join(".libra").join("merge-state.json").exists());
    assert_eq!(std::fs::read_to_string(p.join("f.txt")).expect("f"), "1\n");
}

/// A history with THREE merge bases, which forces the fold to run more than one
/// step: `merge_bases_of_folded` has to answer for the ancestor already folded
/// from `a` and `b` before `c` can be folded in.
///
/// ```text
///            ┌─ a(f=1) ─┐
///   main(o) ─┼─ b(g=1) ─┼── x = ((a ⊕ b) ⊕ c) ── ours
///            └─ c(h=1) ─┘   y = ((b ⊕ c) ⊕ a) ── theirs
/// ```
///
/// `merge_bases(ours, theirs) == {a, b, c}`, and EVERY single base is wrong:
/// relative to `a` the `g`/`h` edits look divergent, relative to `b` the `f`/`h`
/// ones do, and relative to `c` the `f`/`g` ones do. Only the folded ancestor
/// (`f=1, g=1, h=1`) explains both sides.
fn create_three_base_crisscross_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for file in ["f.txt", "g.txt", "h.txt"] {
        std::fs::write(p.join(file), "0\n").expect("write root file");
    }
    assert_cli_success(
        &run_libra_command(&["add", "f.txt", "g.txt", "h.txt"], p),
        "add roots",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "commit root",
    );

    for (branch, file) in [("a", "f.txt"), ("b", "g.txt"), ("c", "h.txt")] {
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], p),
            "checkout main",
        );
        assert_cli_success(&run_libra_command(&["branch", branch], p), "create branch");
        assert_cli_success(
            &run_libra_command(&["checkout", branch], p),
            "checkout branch",
        );
        commit_file(p, file, "1\n", "side edit");
    }

    for (from, tip, others) in [("a", "x", ["b", "c"]), ("b", "y", ["c", "a"])] {
        assert_cli_success(&run_libra_command(&["checkout", from], p), "checkout side");
        assert_cli_success(&run_libra_command(&["branch", tip], p), "create tip branch");
        assert_cli_success(&run_libra_command(&["checkout", tip], p), "checkout tip");
        for other in others {
            assert_cli_success(
                &run_libra_command(&["merge", other], p),
                "criss-cross merge",
            );
        }
    }

    assert_cli_success(&run_libra_command(&["checkout", "y"], p), "checkout y");
    for file in ["f.txt", "g.txt", "h.txt"] {
        std::fs::write(p.join(file), "2\n").expect("write theirs");
    }
    assert_cli_success(
        &run_libra_command(&["add", "f.txt", "g.txt", "h.txt"], p),
        "add theirs",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "theirs edits", "--no-verify"], p),
        "commit theirs",
    );

    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "checkout x");
    repo
}

/// G1 + G2 + G6 at three bases: the two fold steps produce ONE ancestor that
/// resolves the paths no single base can, the one genuinely divergent path
/// still conflicts, and `--restart` recomputes the whole fold byte-identically
/// (the fold order is fixed, so a recompute cannot land anywhere else).
#[test]
fn merge_crisscross_three_merge_bases_fold_into_one_ancestor() {
    let repo = create_three_base_crisscross_repo();
    let p = repo.path();
    // Diverge from the folded ancestor (f=1) on our side too, so `f` is a real
    // both-modified conflict while `g` and `h` stay clean.
    commit_file(p, "f.txt", "ours-2\n", "ours re-edits f");
    let ours = head_commit(p);

    assert_eq!(
        run_libra_command(&["merge", "y"], p).status.code(),
        Some(128),
        "only the genuinely divergent path conflicts"
    );
    let conflicted = std::fs::read_to_string(p.join("f.txt")).expect("f");
    assert!(
        conflicted.contains("<<<<<<< HEAD") && conflicted.contains("ours-2"),
        "f.txt carries the outer conflict: {conflicted}"
    );
    for file in ["g.txt", "h.txt"] {
        assert_eq!(
            std::fs::read_to_string(p.join(file)).expect("clean path"),
            "2\n",
            "{file} merges cleanly ONLY through the ancestor folded from all three bases"
        );
    }
    let state = read_merge_state(p);
    assert!(
        state.get("base").is_none_or(serde_json::Value::is_null),
        "the folded ancestor is not recorded as the merge base: {state}"
    );

    std::fs::write(p.join("f.txt"), "hand-resolved\n").expect("resolve");
    let restarted = run_libra_command(&["merge", "--restart"], p);
    assert_eq!(
        restarted.status.code(),
        Some(128),
        "the restart re-conflicts"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("f.txt")).expect("f"),
        conflicted,
        "two fold steps recompute to the same ancestor, so the conflict is identical"
    );
    assert_eq!(head_commit(p), ours, "HEAD is still the pre-merge commit");
}

/// G5 end to end: a conflict recorded INSIDE the virtual ancestor keeps markers
/// two characters wider than the merge that reads it back. With
/// `merge.conflictStyle=diff3` the ancestor's content is printed in the
/// `|||||||` block of the outer conflict, so both widths are visible in one
/// file — and the nested ones carry Git's temporary-branch labels.
#[test]
fn merge_crisscross_nested_conflict_markers_are_wider_than_the_outer_ones() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "merge.conflictStyle", "diff3"], p),
        "configure diff3",
    );
    std::fs::write(p.join("p.txt"), "0\n").expect("write p");
    assert_cli_success(&run_libra_command(&["add", "p.txt"], p), "add p");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "commit root",
    );

    // Both sides change the SAME line, so folding the two bases conflicts.
    for (branch, content) in [("a", "a\n"), ("b", "b\n")] {
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], p),
            "checkout main",
        );
        assert_cli_success(&run_libra_command(&["branch", branch], p), "create branch");
        assert_cli_success(
            &run_libra_command(&["checkout", branch], p),
            "checkout branch",
        );
        commit_file(p, "p.txt", content, "side edit");
    }

    // The criss-cross merges themselves conflict; resolve each by hand so the
    // recorded trees do NOT contain what the fold will compute.
    for (from, tip, other, resolution) in [("a", "x", "b", "x\n"), ("b", "y", "a", "y\n")] {
        assert_cli_success(&run_libra_command(&["checkout", from], p), "checkout side");
        assert_cli_success(&run_libra_command(&["branch", tip], p), "create tip branch");
        assert_cli_success(&run_libra_command(&["checkout", tip], p), "checkout tip");
        assert_eq!(
            run_libra_command(&["merge", other], p).status.code(),
            Some(128),
            "the criss-cross merge conflicts"
        );
        std::fs::write(p.join("p.txt"), resolution).expect("resolve");
        assert_cli_success(&run_libra_command(&["add", "p.txt"], p), "stage resolution");
        assert_cli_success(
            &run_libra_command(&["merge", "--continue", "--no-verify"], p),
            "finish the criss-cross merge",
        );
    }

    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "checkout x");
    assert_eq!(
        run_libra_command(&["merge", "y"], p).status.code(),
        Some(128),
        "ours and theirs both differ from the folded ancestor"
    );

    let text = std::fs::read_to_string(p.join("p.txt")).expect("p");
    assert!(
        text.contains("<<<<<<<<< Temporary merge branch 1")
            && text.contains(">>>>>>>>> Temporary merge branch 2"),
        "the ancestor's own conflict is nine characters wide (7 + 2 x depth 1) and labelled \
         the way Git labels a virtual-ancestor merge: {text}"
    );
    assert!(
        text.contains("<<<<<<<<<< HEAD"),
        "the outer merge's markers are widened past the nested ones, so the two levels can \
         never be confused: {text}"
    );
}

/// `--dry-run` must not touch a leftover autostash sidecar either: recovering
/// one promotes it into the stash list and deletes the file, and both are
/// writes. A preview leaves it exactly where it found it, for the next REAL
/// merge to recover.
#[test]
fn merge_crisscross_dry_run_leaves_a_stale_autostash_sidecar_untouched() {
    let repo = create_crisscross_repo();
    let p = repo.path();
    // A syntactically valid sidecar naming an object that does not exist: a
    // real merge would refuse to proceed past it, a preview must not even read
    // it as something to act on.
    let sidecar = p.join(".libra").join("merge-autostash.json");
    let stale = r#"{"stash_commit":"0123456789abcdef0123456789abcdef01234567"}"#;
    std::fs::write(&sidecar, stale).expect("plant a stale sidecar");
    let stashes_before = stash_list_len(p);
    let objects_before = count_loose_objects(p);

    let output = run_libra_command(&["--json", "merge", "--dry-run", "y"], p);
    assert_cli_success(&output, "dry run with a stale sidecar present");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["dry_run"], true);
    assert!(
        json["data"]["autostash"].is_null(),
        "a preview reports no autostash outcome: {json}"
    );

    assert_eq!(
        std::fs::read_to_string(&sidecar).expect("sidecar still present"),
        stale,
        "the stale sidecar is neither recovered nor rewritten by a preview"
    );
    assert_eq!(
        stash_list_len(p),
        stashes_before,
        "nothing promoted to the stash list"
    );
    assert_eq!(count_loose_objects(p), objects_before, "no objects written");
}

/// The width ceiling on the COMMAND path: with more merge bases than Libra
/// folds, `merge` is refused with `LBR-UNSUPPORTED-001` before it loads a
/// single base commit or tree, and — like every refusal — writes nothing.
///
/// The fixture builds 33 mutually independent common ancestors (`a01`..`a33`
/// off `main`) and two tips that each reach all of them by different routes.
#[test]
fn merge_crisscross_more_bases_than_the_width_ceiling_is_refused_before_loading() {
    const WIDTH: usize = 33;
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let names: Vec<String> = (1..=WIDTH).map(|i| format!("a{i:02}")).collect();
    for name in &names {
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], p),
            "checkout main",
        );
        assert_cli_success(
            &run_libra_command(&["branch", name], p),
            "create base branch",
        );
        assert_cli_success(&run_libra_command(&["checkout", name], p), "checkout base");
        commit_file(p, &format!("{name}.txt"), "1\n", "independent ancestor");
    }
    // x folds a01..a33 in order; y folds a33..a01 in reverse. Both reach every
    // ancestor, neither reaches the other, and no ancestor dominates another.
    for (tip, order) in [
        ("x", names.clone()),
        ("y", names.iter().rev().cloned().collect::<Vec<_>>()),
    ] {
        assert_cli_success(
            &run_libra_command(&["checkout", &order[0]], p),
            "checkout first",
        );
        assert_cli_success(&run_libra_command(&["branch", tip], p), "create tip");
        assert_cli_success(&run_libra_command(&["checkout", tip], p), "checkout tip");
        for other in &order[1..] {
            assert_cli_success(&run_libra_command(&["merge", other], p), "fold ancestor in");
        }
    }
    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "checkout x");
    commit_file(p, "ours.txt", "ours\n", "diverge ours");
    assert_cli_success(&run_libra_command(&["checkout", "y"], p), "checkout y");
    commit_file(p, "theirs.txt", "theirs\n", "diverge theirs");
    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "checkout x");

    let head_before = head_commit(p);
    let objects_before = count_loose_objects(p);
    let output = run_libra_command(&["merge", "y"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128), "refused: {stderr}");
    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
    assert!(
        stderr.contains("folded from 33 merge bases, more than the 32 Libra folds"),
        "the refusal names the width: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "HEAD untouched");
    assert_eq!(count_loose_objects(p), objects_before, "no objects written");
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state written"
    );
    assert!(!p.join("theirs.txt").exists(), "worktree untouched");

    // `--ff-only` never folds either: a diverged history is refused as
    // non-fast-forward (LBR-CONFLICT-002), exactly as with one merge base —
    // the width ceiling must not pre-empt that verdict.
    let ff_only = run_libra_command(&["merge", "--ff-only", "y"], p);
    let (ff_stderr, ff_report) = parse_cli_error_stderr(&ff_only.stderr);
    assert_eq!(ff_only.status.code(), Some(128), "refused: {ff_stderr}");
    assert_eq!(
        ff_report.error_code, "LBR-CONFLICT-002",
        "--ff-only reports non-fast-forward, not the width ceiling: {ff_stderr}"
    );
    assert!(
        !ff_stderr.contains("merge bases"),
        "the width ceiling stays out of a --ff-only verdict: {ff_stderr}"
    );

    // `-s ours` never folds, so it is never refused for width.
    let ours = run_libra_command(&["--json", "merge", "-s", "ours", "y"], p);
    assert_cli_success(&ours, "-s ours ignores the width ceiling");
    assert_eq!(parse_json_stdout(&ours)["data"]["strategy"], "ours");
}

/// MG-03 G4 end to end: the flattening path is still selectable under the test
/// sentinel and produces the same merge — same tree, same files_changed — as
/// the default incremental walk.
#[test]
fn merge_tree_walk_flat_switch_matches_the_incremental_default() {
    let run = |flat: bool| -> (String, serde_json::Value) {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        std::fs::create_dir_all(p.join("deep/er/dir")).expect("dirs");
        std::fs::write(p.join("deep/er/dir/leaf.txt"), "0\n").expect("leaf");
        std::fs::write(p.join("top.txt"), "0\n").expect("top");
        assert_cli_success(&run_libra_command(&["add", "."], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
            "root",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
        commit_file(p, "top.txt", "ours\n", "ours edit");
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], p),
            "co feature",
        );
        commit_file(p, "deep/er/dir/leaf.txt", "theirs\n", "theirs deep edit");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
        let mut env: Vec<(&str, &str)> = vec![("LIBRA_TEST", "1")];
        if flat {
            env.push(("LIBRA_TEST_MERGE_TREE_WALK", "flat"));
        }
        let output =
            run_libra_command_with_stdin_and_env(&["--json", "merge", "feature"], p, "", &env);
        assert_cli_success(&output, "merge under the selected tree walk");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json summary");
        let tree = run_libra_command(&["rev-parse", "HEAD^{tree}"], p);
        assert_cli_success(&tree, "tree id");
        (
            String::from_utf8_lossy(&tree.stdout).trim().to_string(),
            json["data"].clone(),
        )
    };
    let (incremental_tree, incremental) = run(false);
    let (flat_tree, flat) = run(true);
    assert_eq!(
        incremental_tree, flat_tree,
        "both paths write the same merged tree"
    );
    assert_eq!(incremental["files_changed"], flat["files_changed"]);
    assert_eq!(
        incremental["files_changed"], 1,
        "only the deep leaf changed relative to ours"
    );
    assert_eq!(incremental["strategy"], "three-way");
}

/// MG-03 G1/G2/G5 at the PRODUCTION entry: the default `libra merge` takes the
/// incremental walk and reads only the trees along the changed paths — proven
/// through the `LIBRA_TEST_MERGE_TREE_STATS` seam rather than an in-memory
/// graph. A deep subtree all three sides share is never opened; the subtree
/// theirs changed is opened once per side per differing level.
#[test]
fn merge_tree_walk_default_is_incremental_and_reads_only_changed_paths() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for dir in ["shared/a/b/c", "moved/x/y"] {
        std::fs::create_dir_all(p.join(dir)).expect("dirs");
    }
    std::fs::write(p.join("shared/a/b/c/leaf.txt"), "0\n").expect("shared leaf");
    std::fs::write(p.join("moved/x/y/leaf.txt"), "0\n").expect("moved leaf");
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(p, "moved/x/y/leaf.txt", "theirs\n", "theirs deep edit");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    // An untracked file under the SHARED subtree: the untracked-collision check
    // must not expand `shared/` for it (ours already has that subtree; nothing
    // under it is written), so the read bound below still holds.
    std::fs::write(p.join("shared/a/b/untracked.txt"), "stray\n").expect("untracked");

    let stats_dir = tempfile::tempdir().expect("stats dir");
    let stats = stats_dir.path().join("stats.json");
    let stats_path = stats.to_string_lossy().to_string();
    let output = run_libra_command_with_stdin_and_env(
        &["--json", "merge", "feature"],
        p,
        "",
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_MERGE_TREE_STATS", &stats_path),
        ],
    );
    assert_cli_success(&output, "default merge");
    let recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&stats).expect("stats written")).expect("stats json");
    assert_eq!(
        recorded["walk"], "incremental",
        "the default merge takes the pruning walk"
    );
    let reads = recorded["tree_reads"].as_u64().expect("tree_reads") as usize;
    // Per pass: 3 distinct roots + `moved/x/y` (three levels, two distinct
    // sides: base == ours, theirs) = 9; `shared/…` is identical everywhere: 0.
    // Two passes read from the store — the preflight gate's and the engine's,
    // each with its own cache — so the whole merge is bounded by 2 × 9.
    assert!(
        reads <= 2 * (3 + 6),
        "tree reads are bounded by the changed path, not the tree: {reads} ({recorded})"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("moved/x/y/leaf.txt")).expect("merged leaf"),
        "theirs\n"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("top.txt")).expect("top"),
        "ours\n"
    );

    // The same repository shape under the flat switch records the flat walk.
    let repo2 = create_committed_repo_via_cli();
    let q = repo2.path();
    std::fs::write(q.join("a.txt"), "0\n").expect("a");
    assert_cli_success(&run_libra_command(&["add", "a.txt"], q), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], q),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], q), "branch");
    commit_file(q, "b.txt", "ours\n", "ours");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], q),
        "co feature",
    );
    commit_file(q, "a.txt", "theirs\n", "theirs");
    assert_cli_success(&run_libra_command(&["checkout", "main"], q), "co main");
    let output = run_libra_command_with_stdin_and_env(
        &["merge", "feature"],
        q,
        "",
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_MERGE_TREE_WALK", "flat"),
            ("LIBRA_TEST_MERGE_TREE_STATS", &stats_path),
        ],
    );
    assert_cli_success(&output, "flat merge");
    let recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&stats).expect("stats written")).expect("stats json");
    assert_eq!(recorded["walk"], "flat");
}

/// MG-03: an untracked FILE whose path is an ancestor of a subtree the merge
/// would adopt verbatim collides exactly as the flattening path's per-leaf
/// check says it does — refused before HEAD, index or worktree change.
#[test]
fn merge_tree_walk_refuses_untracked_ancestor_of_an_adopted_subtree() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "top.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(
        p,
        "newdir/sub/leaf.txt",
        "theirs\n",
        "theirs adds a subtree",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    // An untracked FILE at `newdir` blocks the adopted `newdir/…` subtree.
    std::fs::write(p.join("newdir"), "untracked file, not a directory\n").expect("untracked");
    let head_before = head_commit(p);
    let objects_before = count_loose_objects(p);

    let output = run_libra_command(&["merge", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128), "refused: {stderr}");
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("newdir"),
        "the colliding untracked path is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "HEAD untouched");
    assert_eq!(
        std::fs::read_to_string(p.join("newdir")).expect("untracked survives"),
        "untracked file, not a directory\n"
    );
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state"
    );
    assert!(
        count_loose_objects(p) <= objects_before + 1,
        "no tree or commit written (at most the auto-merge's blob-free walk): before \
         {objects_before}, after {}",
        count_loose_objects(p)
    );
}

/// MG-03: a `pre-merge-commit` hook that drops an untracked file UNDER a
/// subtree the merge adopts verbatim is caught by the post-hook recheck — the
/// collision set is recomputed after every hook, never reused — so the merge
/// is refused before HEAD moves, exactly as the flattening path refuses it.
#[cfg(unix)]
#[test]
fn merge_tree_walk_rechecks_hook_created_files_under_adopted_subtrees() {
    use std::os::unix::fs::PermissionsExt;
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "top.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(
        p,
        "newdir/sub/leaf.txt",
        "theirs\n",
        "theirs adds a subtree",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    // The hook creates a file at a path the adopted subtree will write.
    let hooks = p.join(".libra").join("hooks");
    std::fs::create_dir_all(&hooks).expect("hooks dir");
    let hook = hooks.join("pre-merge-commit");
    std::fs::write(
        &hook,
        "#!/bin/sh\nmkdir -p \"$LIBRA_WORK_TREE/newdir/sub\"\nprintf 'hook\\n' > \"$LIBRA_WORK_TREE/newdir/sub/leaf.txt\"\n",
    )
    .expect("write hook");
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let head_before = head_commit(p);

    let output = run_libra_command(&["merge", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128), "refused: {stderr}");
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("newdir/sub/leaf.txt"),
        "the hook-created colliding path is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "HEAD untouched");
    assert_eq!(
        std::fs::read_to_string(p.join("newdir/sub/leaf.txt")).expect("hook file survives"),
        "hook\n",
        "the untracked file the hook wrote is not overwritten"
    );
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state"
    );
}

/// MG-03: a subtree theirs ADDED is enumerated in full by the read-only gate
/// before anything is written (an added directory has no counterpart on the
/// other sides, so nothing about it can be skipped). With one of its nested
/// tree objects missing from the store the merge fails the way the flattening
/// path failed — while reading the trees — and HEAD, the index and the working
/// tree are untouched. (Trees the walk leaves unopened are HEAD's own; see the
/// unopened-tree invariant on `incremental_merge_trees`.)
#[test]
fn merge_tree_walk_refuses_an_added_subtree_with_a_missing_nested_tree() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "top.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(
        p,
        "newdir/sub/leaf.txt",
        "theirs\n",
        "theirs adds a nested subtree",
    );
    // Only the leaf's tree differs from the base at the top level; the adopted
    // `newdir/` subtree's nested `sub/` tree is what goes missing.
    let nested = run_libra_command(&["rev-parse", "feature:newdir/sub"], p);
    assert_cli_success(&nested, "resolve the nested tree");
    let nested_id = String::from_utf8_lossy(&nested.stdout).trim().to_string();
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    let object = p
        .join(".libra")
        .join("objects")
        .join(&nested_id[..2])
        .join(&nested_id[2..]);
    assert!(
        object.exists(),
        "the nested tree is a loose object: {}",
        object.display()
    );
    std::fs::remove_file(&object).expect("simulate a missing nested tree");
    let head_before = head_commit(p);

    let output = run_libra_command(&["merge", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128), "refused: {stderr}");
    assert_eq!(
        report.error_code, "LBR-REPO-002",
        "a missing tree is repository corruption"
    );
    assert!(
        stderr.contains(&nested_id),
        "the unreadable tree is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "HEAD never moved");
    assert!(
        !p.join("newdir").exists(),
        "nothing of the adopted subtree was written"
    );
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("top.txt")).expect("top"),
        "ours\n"
    );
}

/// MG-03: a `refs/replace` substitution on a ROOT tree is honoured identically
/// by both walks — the flattening path loads roots through the
/// replacement-aware loader and nested trees raw, and the incremental path
/// mirrors exactly that (roots via `replace::resolve`, nested via the raw
/// loader) — so default and flat merges agree, and both see the replacement.
#[test]
fn merge_tree_walk_root_tree_replacement_is_honoured_identically_by_both_walks() {
    let run = |flat: bool| -> (String, serde_json::Value, String) {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        std::fs::write(p.join("a.txt"), "0\n").expect("a");
        std::fs::write(p.join("b.txt"), "0\n").expect("b");
        assert_cli_success(&run_libra_command(&["add", "a.txt", "b.txt"], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
            "root",
        );
        assert_cli_success(
            &run_libra_command(&["branch", "feature"], p),
            "branch feature",
        );
        assert_cli_success(&run_libra_command(&["branch", "alt"], p), "branch alt");
        commit_file(p, "b.txt", "ours\n", "ours edit");
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], p),
            "co feature",
        );
        commit_file(p, "a.txt", "theirs\n", "theirs edit");
        let feature_tree = run_libra_command(&["rev-parse", "feature^{tree}"], p);
        assert_cli_success(&feature_tree, "feature tree");
        // The replacement: a root tree where a.txt says something else.
        assert_cli_success(&run_libra_command(&["checkout", "alt"], p), "co alt");
        commit_file(p, "a.txt", "replaced\n", "alt edit");
        let alt_tree = run_libra_command(&["rev-parse", "alt^{tree}"], p);
        assert_cli_success(&alt_tree, "alt tree");
        assert_cli_success(
            &run_libra_command(
                &[
                    "replace",
                    String::from_utf8_lossy(&feature_tree.stdout).trim(),
                    String::from_utf8_lossy(&alt_tree.stdout).trim(),
                ],
                p,
            ),
            "replace feature's root tree",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
        let mut env: Vec<(&str, &str)> = vec![("LIBRA_TEST", "1")];
        if flat {
            env.push(("LIBRA_TEST_MERGE_TREE_WALK", "flat"));
        }
        let output =
            run_libra_command_with_stdin_and_env(&["--json", "merge", "feature"], p, "", &env);
        assert_cli_success(&output, "merge with a replaced root tree");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json summary");
        let tree = run_libra_command(&["rev-parse", "HEAD^{tree}"], p);
        assert_cli_success(&tree, "merged tree");
        (
            String::from_utf8_lossy(&tree.stdout).trim().to_string(),
            json["data"].clone(),
            std::fs::read_to_string(p.join("a.txt")).expect("a"),
        )
    };
    let (incremental_tree, incremental, incremental_a) = run(false);
    let (flat_tree, flat, flat_a) = run(true);
    assert_eq!(
        incremental_a, "replaced\n",
        "the replacement root tree is what gets merged"
    );
    assert_eq!(
        flat_a, incremental_a,
        "both walks see the same replaced root"
    );
    assert_eq!(
        incremental_tree, flat_tree,
        "both walks write the same merged tree"
    );
    assert_eq!(incremental["files_changed"], flat["files_changed"]);
}

/// MG-03: a nested tree that all three sides SHARE and the walk therefore never
/// opens is still read by the checkout — which now runs before the commit and
/// HEAD are written — so a missing shared tree fails with HEAD, index and
/// working tree untouched, exactly as the flattening path's up-front read did.
#[test]
fn merge_tree_walk_missing_shared_tree_fails_before_head_moves() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::create_dir_all(p.join("shared/a/b")).expect("dirs");
    std::fs::write(p.join("shared/a/b/leaf.txt"), "0\n").expect("shared leaf");
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(p, "other.txt", "theirs\n", "theirs edit");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    let shared = run_libra_command(&["rev-parse", "HEAD:shared/a"], p);
    assert_cli_success(&shared, "resolve the shared nested tree");
    let shared_id = String::from_utf8_lossy(&shared.stdout).trim().to_string();
    let object = p
        .join(".libra")
        .join("objects")
        .join(&shared_id[..2])
        .join(&shared_id[2..]);
    assert!(object.exists(), "loose object: {}", object.display());
    std::fs::remove_file(&object).expect("simulate a missing shared tree");
    let head_before = head_commit(p);
    let index_before = std::fs::read(p.join(".libra/index")).expect("index bytes");

    let output = run_libra_command(&["merge", "feature"], p);
    // Both walks reach the missing tree through the same reader (`Tree::load`
    // inside the index rebuild here; inside flattening on the flat walk), which
    // aborts rather than returning a structured report — a pre-existing,
    // path-independent shape. What this test pins is WHEN: before HEAD, the
    // index or the working tree changed.
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        !output.status.success(),
        "the merge cannot complete: {stderr}"
    );
    assert!(
        stderr.contains(&shared_id),
        "the unreadable tree is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "HEAD never moved");
    assert_eq!(
        std::fs::read(p.join(".libra/index")).expect("index bytes"),
        index_before,
        "the index is untouched"
    );
    assert!(
        !p.join("other.txt").exists(),
        "nothing of the merge result was written"
    );
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state"
    );
}

/// MG-03: with a replaced root tree that CONFLICTS, the conflict state names
/// the replaced content on stage 3 — identically on both walks — so
/// `restore --theirs` and `--continue` operate on what was actually merged.
#[test]
fn merge_tree_walk_conflict_stages_follow_the_replaced_root_on_both_walks() {
    let run = |flat: bool| -> String {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        std::fs::write(p.join("a.txt"), "0\n").expect("a");
        assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
            "root",
        );
        assert_cli_success(
            &run_libra_command(&["branch", "feature"], p),
            "branch feature",
        );
        assert_cli_success(&run_libra_command(&["branch", "alt"], p), "branch alt");
        commit_file(p, "a.txt", "ours\n", "ours edit");
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], p),
            "co feature",
        );
        commit_file(p, "a.txt", "theirs\n", "theirs edit");
        let feature_tree = run_libra_command(&["rev-parse", "feature^{tree}"], p);
        assert_cli_success(&feature_tree, "feature tree");
        assert_cli_success(&run_libra_command(&["checkout", "alt"], p), "co alt");
        commit_file(p, "a.txt", "replaced\n", "alt edit");
        let alt_tree = run_libra_command(&["rev-parse", "alt^{tree}"], p);
        assert_cli_success(&alt_tree, "alt tree");
        assert_cli_success(
            &run_libra_command(
                &[
                    "replace",
                    String::from_utf8_lossy(&feature_tree.stdout).trim(),
                    String::from_utf8_lossy(&alt_tree.stdout).trim(),
                ],
                p,
            ),
            "replace feature's root tree",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
        let mut env: Vec<(&str, &str)> = vec![("LIBRA_TEST", "1")];
        if flat {
            env.push(("LIBRA_TEST_MERGE_TREE_WALK", "flat"));
        }
        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", &env);
        assert_eq!(
            output.status.code(),
            Some(128),
            "the replaced root conflicts with ours"
        );
        assert_cli_success(
            &run_libra_command(&["restore", "--theirs", "a.txt"], p),
            "restore theirs from stage 3",
        );
        std::fs::read_to_string(p.join("a.txt")).expect("a")
    };
    let incremental = run(false);
    let flat = run(true);
    assert_eq!(
        incremental, "replaced\n",
        "stage 3 is the REPLACED root's content"
    );
    assert_eq!(
        flat, incremental,
        "both walks record the same stage-3 entry"
    );
}

/// Build the nested-gitlink fixture shared by the two MG-03 G9/G10 CLI tests:
/// `main` (ours) carries `deps/vendor/lib` = [`GITLINK_BASE`] plus `deps/keep.txt`,
/// then edits only `top.txt`, so the whole `deps/` subtree still equals the base
/// on ours — the shape the incremental walk prunes past. `feature` (theirs) is
/// the base tree plus `side.txt`, with the gitlink set to `feature_gitlink`.
fn create_nested_gitlink_repo(feature_gitlink: &str) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    std::fs::create_dir_all(p.join("deps")).expect("deps");
    std::fs::write(p.join("deps/keep.txt"), "keep\n").expect("keep");
    assert_cli_success(
        &run_libra_command(&["add", "top.txt", "deps/keep.txt"], p),
        "add files",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},deps/vendor/lib"),
            ],
            p,
        ),
        "stage the nested base gitlink",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "base with nested submodule", "--no-verify"],
            p,
        ),
        "commit the base",
    );
    let base = head_commit(p);

    let side_blob = {
        let out = run_libra_command(&["hash-object", "-w", "--stdin"], p);
        assert!(out.status.success(), "hash-object must succeed");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("100644,{side_blob},side.txt"),
                "--cacheinfo",
                &format!("160000,{feature_gitlink},deps/vendor/lib"),
            ],
            p,
        ),
        "stage the feature tree",
    );
    let tree = {
        let out = run_libra_command(&["write-tree"], p);
        assert_cli_success(&out, "write-tree");
        stdout_trimmed(&out)
    };
    let feature = {
        let out = run_libra_command(&["commit-tree", &tree, "-p", &base, "-m", "feature"], p);
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/feature", &feature], p),
        "create refs/heads/feature",
    );
    // Put main's index back to the base tree, then make ours' own change at
    // the ROOT only, leaving `deps/` byte-identical to the base.
    assert_cli_success(
        &run_libra_command(&["update-index", "--remove", "side.txt"], p),
        "unstage side.txt",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},deps/vendor/lib"),
            ],
            p,
        ),
        "restore the base gitlink in main's index",
    );
    commit_file(p, "top.txt", "ours\n", "ours edits top only");
    repo
}

/// MG-03 G9 at the CLI: a gitlink buried two directories deep inside a subtree
/// that equals the base on OUR side — the walk would adopt `deps/` from theirs
/// without opening it — is still arbitration when theirs moved the pointer, and
/// is refused before anything is written, naming the nested path.
#[test]
fn merge_gitlink_nested_changed_pointer_inside_a_pruned_subtree_is_refused() {
    let repo = create_nested_gitlink_repo(GITLINK_MOVED);
    let p = repo.path();
    let head_before = head_commit(p);
    let index_before = std::fs::read(p.join(".libra/index")).expect("index bytes");
    let objects_before = count_loose_objects(p);

    let output = run_libra_command(&["merge", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128), "refused: {stderr}");
    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
    assert!(
        stderr.contains("deps/vendor/lib"),
        "the nested gitlink path is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "HEAD untouched");
    assert_eq!(
        std::fs::read(p.join(".libra/index")).expect("index bytes"),
        index_before,
        "index untouched"
    );
    assert_eq!(count_loose_objects(p), objects_before, "no objects written");
    assert!(!p.join("side.txt").exists(), "worktree untouched");
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state"
    );
}

/// MG-03 G10 at the CLI: the same nested gitlink, identical on all three sides,
/// passes through inside the pruned `deps/` subtree — the merge succeeds, the
/// result still carries the pointer, and the production read counter shows the
/// subtree was never opened (only the three distinct root trees, per pass).
#[test]
fn merge_gitlink_nested_identical_pointer_inside_a_pruned_subtree_passes_through() {
    let repo = create_nested_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    let stats_dir = tempfile::tempdir().expect("stats dir");
    let stats = stats_dir.path().join("stats.json");
    let stats_path = stats.to_string_lossy().to_string();

    let output = run_libra_command_with_stdin_and_env(
        &["--json", "merge", "feature"],
        p,
        "",
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_MERGE_TREE_STATS", &stats_path),
        ],
    );
    assert_cli_success(&output, "merge passes the nested gitlink through");
    // `ls-tree` prints the Git mode; (`cat-file -p` renders a gitlink item's
    // mode through the enum's display, which is not the octal form.)
    let vendor = run_libra_command(&["ls-tree", "HEAD", "deps/vendor/"], p);
    assert_cli_success(&vendor, "list the merged deps/vendor tree");
    let listing = String::from_utf8_lossy(&vendor.stdout).to_string();
    assert!(
        listing.contains("160000") && listing.contains(GITLINK_BASE) && listing.contains("lib"),
        "the pointer survives inside the pruned subtree verbatim: {listing}"
    );
    assert!(p.join("side.txt").exists(), "theirs' file merged");
    assert_eq!(
        std::fs::read_to_string(p.join("top.txt")).expect("top"),
        "ours\n"
    );

    let recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&stats).expect("stats written")).expect("stats json");
    assert_eq!(recorded["walk"], "incremental");
    let reads = recorded["tree_reads"].as_u64().expect("tree_reads") as usize;
    // Three distinct roots per pass (base / ours / theirs all differ at the
    // root), two passes; `deps/` is identical everywhere and never opened.
    assert!(
        reads <= 2 * 3,
        "the subtree holding the gitlink is pruned, not opened: {reads} ({recorded})"
    );
}

/// MG-03: `--dry-run` reaches the same verdict as the real merge when a tree
/// the walk never opens is missing — the preview probes the carried trees
/// (read-only), so it fails exactly where the real merge's checkout would,
/// instead of reporting a clean preview for a merge that cannot complete.
#[test]
fn merge_tree_walk_dry_run_matches_the_real_verdict_on_a_missing_shared_tree() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::create_dir_all(p.join("shared/a/b")).expect("dirs");
    std::fs::write(p.join("shared/a/b/leaf.txt"), "0\n").expect("shared leaf");
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(p, "other.txt", "theirs\n", "theirs edit");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    let shared = run_libra_command(&["rev-parse", "HEAD:shared/a"], p);
    assert_cli_success(&shared, "resolve the shared nested tree");
    let shared_id = String::from_utf8_lossy(&shared.stdout).trim().to_string();
    let object = p
        .join(".libra")
        .join("objects")
        .join(&shared_id[..2])
        .join(&shared_id[2..]);
    std::fs::remove_file(&object).expect("simulate a missing shared tree");
    let head_before = head_commit(p);

    let preview = run_libra_command(&["merge", "--dry-run", "feature"], p);
    let stderr = String::from_utf8_lossy(&preview.stderr).to_string();
    assert!(
        !preview.status.success(),
        "the preview must not promise a merge that cannot complete: {stderr}"
    );
    assert!(
        stderr.contains(&shared_id),
        "the unreadable tree is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "a preview never moves HEAD");
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state"
    );
}

/// Shared shape for the MG-03 G6–G8 CLI carriers. The base carries a plain
/// `tool.sh`, a `target.txt` and a symlink `link -> target.txt`; `ours` edits
/// `top.txt`; `theirs` is built by PLUMBING (`update-index --cacheinfo` +
/// `write-tree` + `commit-tree`) so that a mode-only change can be expressed —
/// Libra's porcelain `add` does not detect a bare `chmod`. Runs the merge under
/// the requested walk and returns `(repo, ls-tree -r HEAD, ls-files -s)`.
#[cfg(unix)]
fn merge_mode_scenario(
    flat: bool,
    theirs_entries: &[(&str, &str)],
) -> (tempfile::TempDir, String, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    std::fs::write(p.join("tool.sh"), "#!/bin/sh\necho hi\n").expect("tool");
    std::fs::write(p.join("target.txt"), "t\n").expect("target");
    std::os::unix::fs::symlink("target.txt", p.join("link")).expect("symlink");
    assert_cli_success(
        &run_libra_command(&["add", "top.txt", "tool.sh", "target.txt", "link"], p),
        "add",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    let base = head_commit(p);
    // theirs = base tree with the requested entries overridden.
    let mut stage: Vec<String> = vec!["update-index".to_string()];
    for (mode_and_path, content) in theirs_entries {
        let (mode, path) = mode_and_path.split_once(' ').expect("'<mode> <path>'");
        let blob = run_libra_command_with_stdin(&["hash-object", "-w", "--stdin"], p, content);
        assert!(blob.status.success(), "hash-object");
        stage.push("--cacheinfo".to_string());
        stage.push(format!("{mode},{},{path}", stdout_trimmed(&blob)));
    }
    let stage: Vec<&str> = stage.iter().map(String::as_str).collect();
    assert_cli_success(&run_libra_command(&stage, p), "stage theirs' entries");
    let tree = {
        let out = run_libra_command(&["write-tree"], p);
        assert_cli_success(&out, "write-tree");
        stdout_trimmed(&out)
    };
    let feature = {
        let out = run_libra_command(&["commit-tree", &tree, "-p", &base, "-m", "theirs"], p);
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/feature", &feature], p),
        "refs/heads/feature",
    );
    // Put main's index back to the base tree, then make ours' change.
    assert_cli_success(
        &run_libra_command(&["reset", "--hard", "HEAD"], p),
        "reset main's index to the base",
    );
    commit_file(p, "top.txt", "ours\n", "ours edit");
    let mut env: Vec<(&str, &str)> = vec![("LIBRA_TEST", "1")];
    if flat {
        env.push(("LIBRA_TEST_MERGE_TREE_WALK", "flat"));
    }
    let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", &env);
    assert_cli_success(&output, "merge");
    let tree_listing = run_libra_command(&["ls-tree", "-r", "HEAD"], p);
    assert_cli_success(&tree_listing, "ls-tree");
    let index_listing = run_libra_command(&["ls-files", "-s"], p);
    assert_cli_success(&index_listing, "ls-files -s");
    (
        repo,
        String::from_utf8_lossy(&tree_listing.stdout).to_string(),
        String::from_utf8_lossy(&index_listing.stdout).to_string(),
    )
}

/// What the merge checkout left in the working tree for a path: the file's
/// mode bits, or the symlink target — compared between the two walks.
#[cfg(unix)]
fn worktree_shape(p: &Path, path: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let full = p.join(path);
    match std::fs::read_link(&full) {
        Ok(target) => format!("link->{}", target.to_string_lossy()),
        Err(_) => format!(
            "mode={:o}",
            std::fs::metadata(&full).expect("meta").permissions().mode() & 0o777
        ),
    }
}

/// MG-03 G6 at the CLI: a mode-only change on theirs (`tool.sh` becomes
/// executable, content untouched) merges to a `100755` tree entry and index
/// entry on both walks, and the two walks leave the working tree in the same
/// state. MG-04 R7 later made the shared merge writer apply entry type and
/// mode; `merge_preserves_symlinks_and_executable_bits_when_it_writes` pins
/// that current write behavior on both walks.
#[cfg(unix)]
#[test]
fn merge_tree_walk_preserves_a_mode_only_change_on_both_walks() {
    let mut shapes = Vec::new();
    for flat in [false, true] {
        let (repo, tree, index) =
            merge_mode_scenario(flat, &[("100755 tool.sh", "#!/bin/sh\necho hi\n")]);
        assert!(
            tree.lines()
                .any(|l| l.starts_with("100755") && l.ends_with("\ttool.sh")),
            "flat={flat}: the executable bit is in the merged tree: {tree}"
        );
        assert!(
            index
                .lines()
                .any(|l| l.starts_with("100755") && l.ends_with("tool.sh")),
            "flat={flat}: the executable bit is in the merged index: {index}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("top.txt")).expect("top"),
            "ours\n"
        );
        shapes.push(worktree_shape(repo.path(), "tool.sh"));
    }
    assert_eq!(
        shapes[0], shapes[1],
        "both walks leave the same working tree"
    );
}

/// MG-03 G7 at the CLI: theirs re-points `link`; the merged tree and index keep
/// a `120000` entry with the new target's blob on both walks, and both walks
/// leave the same working tree.
#[cfg(unix)]
#[test]
fn merge_tree_walk_preserves_a_symlink_change_on_both_walks() {
    let mut shapes = Vec::new();
    for flat in [false, true] {
        let (repo, tree, index) = merge_mode_scenario(flat, &[("120000 link", "top.txt")]);
        let new_target_blob = {
            let out =
                run_libra_command_with_stdin(&["hash-object", "--stdin"], repo.path(), "top.txt");
            assert!(out.status.success(), "hash-object");
            stdout_trimmed(&out)
        };
        assert!(
            tree.lines().any(|l| l.starts_with("120000")
                && l.contains(&new_target_blob)
                && l.ends_with("\tlink")),
            "flat={flat}: the re-pointed symlink is in the merged tree: {tree}"
        );
        assert!(
            index
                .lines()
                .any(|l| l.starts_with("120000") && l.ends_with("link")),
            "flat={flat}: the symlink entry is in the merged index: {index}"
        );
        shapes.push(worktree_shape(repo.path(), "link"));
    }
    assert_eq!(
        shapes[0], shapes[1],
        "both walks leave the same working tree"
    );
}

/// MG-03 G8 at the CLI: an executable file ADDED by theirs arrives as `100755`
/// in the merged tree and index on both walks, with the same working tree.
#[cfg(unix)]
#[test]
fn merge_tree_walk_preserves_an_added_executable_on_both_walks() {
    let mut shapes = Vec::new();
    for flat in [false, true] {
        let (repo, tree, index) =
            merge_mode_scenario(flat, &[("100755 run.sh", "#!/bin/sh\nexit 0\n")]);
        assert!(
            tree.lines()
                .any(|l| l.starts_with("100755") && l.ends_with("\trun.sh")),
            "flat={flat}: the added executable keeps its mode in the tree: {tree}"
        );
        assert!(
            index
                .lines()
                .any(|l| l.starts_with("100755") && l.ends_with("run.sh")),
            "flat={flat}: …and in the index: {index}"
        );
        assert!(
            repo.path().join("run.sh").exists(),
            "flat={flat}: the file is checked out"
        );
        shapes.push(worktree_shape(repo.path(), "run.sh"));
    }
    assert_eq!(
        shapes[0], shapes[1],
        "both walks leave the same working tree"
    );
}

/// MG-03: a nested `refs/replace` whose replacement is MISSING makes the
/// checkout fail (it resolves replacements); the preview probe sees the trees
/// the way the checkout does, so `--dry-run` fails too — same verdict — and
/// the real merge fails before HEAD moves.
#[test]
fn merge_tree_walk_dry_run_follows_nested_replacements_like_the_checkout() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::create_dir_all(p.join("shared/a")).expect("dirs");
    std::fs::write(p.join("shared/a/leaf.txt"), "0\n").expect("leaf");
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(&run_libra_command(&["branch", "alt"], p), "branch alt");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(p, "other.txt", "theirs\n", "theirs edit");
    // A replacement tree for `shared/a`, then its object goes missing.
    assert_cli_success(&run_libra_command(&["checkout", "alt"], p), "co alt");
    commit_file(p, "shared/a/leaf.txt", "alt\n", "alt edit");
    let alt_sub = run_libra_command(&["rev-parse", "alt:shared/a"], p);
    assert_cli_success(&alt_sub, "alt nested tree");
    let alt_id = String::from_utf8_lossy(&alt_sub.stdout).trim().to_string();
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    let shared = run_libra_command(&["rev-parse", "HEAD:shared/a"], p);
    assert_cli_success(&shared, "shared nested tree");
    let shared_id = String::from_utf8_lossy(&shared.stdout).trim().to_string();
    assert_cli_success(
        &run_libra_command(&["replace", &shared_id, &alt_id], p),
        "replace the nested tree",
    );
    let object = p
        .join(".libra")
        .join("objects")
        .join(&alt_id[..2])
        .join(&alt_id[2..]);
    std::fs::remove_file(&object).expect("make the replacement dangle");
    let head_before = head_commit(p);

    let preview = run_libra_command(&["merge", "--dry-run", "feature"], p);
    assert!(
        !preview.status.success(),
        "the preview follows the replacement like the checkout and fails: {}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let real = run_libra_command(&["merge", "feature"], p);
    assert!(!real.status.success(), "the real merge fails at checkout");
    assert_eq!(head_commit(p), head_before, "HEAD never moved");
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state"
    );
}

/// MG-03: with a CONFLICT elsewhere, the real merge never checks out — it
/// writes conflict state through the raw tree view — so a dangling nested
/// `refs/replace` does not stop it. The preview mirrors that: it reports the
/// conflict (exit 1) instead of failing on the replacement the real merge
/// would never follow.
#[test]
fn merge_tree_walk_conflicted_dry_run_matches_the_real_conflict_path_under_a_dangling_replace() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::create_dir_all(p.join("shared/a")).expect("dirs");
    std::fs::write(p.join("shared/a/leaf.txt"), "0\n").expect("leaf");
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(&run_libra_command(&["branch", "alt"], p), "branch alt");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(p, "top.txt", "theirs\n", "theirs conflicting edit");
    assert_cli_success(&run_libra_command(&["checkout", "alt"], p), "co alt");
    commit_file(p, "shared/a/leaf.txt", "alt\n", "alt edit");
    let alt_sub = run_libra_command(&["rev-parse", "alt:shared/a"], p);
    assert_cli_success(&alt_sub, "alt nested tree");
    let alt_id = String::from_utf8_lossy(&alt_sub.stdout).trim().to_string();
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    let shared = run_libra_command(&["rev-parse", "HEAD:shared/a"], p);
    assert_cli_success(&shared, "shared nested tree");
    let shared_id = String::from_utf8_lossy(&shared.stdout).trim().to_string();
    assert_cli_success(
        &run_libra_command(&["replace", &shared_id, &alt_id], p),
        "replace the nested tree",
    );
    let object = p
        .join(".libra")
        .join("objects")
        .join(&alt_id[..2])
        .join(&alt_id[2..]);
    std::fs::remove_file(&object).expect("make the replacement dangle");

    let preview = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_eq!(
        preview.status.code(),
        Some(1),
        "the preview reports the conflict, not the replacement: {}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let json = parse_json_stdout(&preview);
    assert_eq!(json["data"]["would_conflict"], true);

    let real = run_libra_command(&["merge", "feature"], p);
    assert_eq!(
        real.status.code(),
        Some(128),
        "the real merge writes conflict state: {}",
        String::from_utf8_lossy(&real.stderr)
    );
    assert!(
        p.join(".libra").join("merge-state.json").exists(),
        "conflict state written"
    );
    assert!(
        std::fs::read_to_string(p.join("top.txt"))
            .expect("top")
            .contains("<<<<<<<"),
        "the conflict is in the working tree"
    );
}

// ---------------------------------------------------------------------------
// MG-04: directory/file (D/F) collisions.
// ---------------------------------------------------------------------------

/// The D/F fixture: one side keeps (edits, or adds) a FILE `foo`, the other
/// side replaces it with a DIRECTORY `foo/` with content. `dir_on_theirs`
/// picks which side grows the directory; `file_in_base` says whether the
/// merge base already tracked the file (modify/delete + D/F, Git's stages 1+2)
/// or the file is a one-sided add (pure D/F, stage 2 only). Returns the repo
/// on `main` with `feature` ready to merge.
///
/// Libra's `checkout` cannot flip a path between file and directory yet
/// (pre-existing, registered in the dev doc), so every switch between the two
/// sides goes through the `root` branch, which tracks neither.
fn create_df_conflict_repo(dir_on_theirs: bool, file_in_base: bool) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "other.txt", "0\n", "root");
    assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
    if file_in_base {
        commit_file(p, "foo", "base file\n", "base with file foo");
    }
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    let make_dir = |p: &Path| {
        if file_in_base {
            assert_cli_success(&run_libra_command(&["rm", "foo"], p), "drop the file");
        }
        std::fs::create_dir_all(p.join("foo")).expect("mkdir foo");
        std::fs::write(p.join("foo/bar.txt"), "inside the directory\n").expect("bar");
        assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add dir");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "foo becomes a directory", "--no-verify"],
                p,
            ),
            "dir commit",
        );
    };
    let edit_file = |p: &Path| commit_file(p, "foo", "edited file\n", "foo edited");
    let switch = |p: &Path, branch: &str| {
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", branch], p), "checkout");
    };
    if dir_on_theirs {
        edit_file(p);
        switch(p, "feature");
        make_dir(p);
    } else {
        make_dir(p);
        switch(p, "feature");
        edit_file(p);
    }
    switch(p, "main");
    repo
}

/// Run a merge and require Libra's CONFLICT exit — not the 128 an I/O failure
/// also carries — returning the output for further assertions.
fn merge_expecting_conflict(p: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let output = run_libra_command_with_stdin_and_env(args, p, "", env);
    assert_eq!(
        output.status.code(),
        Some(128),
        "a D/F collision conflicts.\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-002", "{stderr}");
    output
}

fn index_stage_lines(p: &Path, path: &str) -> Vec<String> {
    let out = run_libra_command(&["ls-files", "-s"], p);
    assert_cli_success(&out, "ls-files -s");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.ends_with(&format!("\t{path}")))
        .map(|l| l.to_string())
        .collect()
}

/// G1 + G3 + G5 + G6: ours EDITED the file, theirs made the DIRECTORY. The
/// directory keeps `foo`; our file is moved to `foo~HEAD` and recorded there
/// as Git records it — the D/F relocation runs first, then the modify/delete
/// branch, so `foo~HEAD` carries the base on stage 1 and ours on stage 2
/// (git@3cb9185f6 merge-ort.c:4100-4198 then :4374; verified against
/// `git merge`: `100644 … 1 foo~HEAD` / `100644 … 2 foo~HEAD`), and the merge
/// prints Git's `CONFLICT (file/directory)` line.
#[test]
fn merge_df_conflict_file_on_ours_is_moved_to_head_suffix() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    let output = run_libra_command(&["merge", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(128),
        "a D/F collision conflicts: {stderr}"
    );
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains(
            "CONFLICT (file/directory): directory in the way of foo from HEAD; moving it to foo~HEAD instead."
        ),
        "Git's message: {stdout}"
    );
    assert!(p.join("foo").is_dir(), "the directory keeps the path");
    assert_eq!(
        std::fs::read_to_string(p.join("foo/bar.txt")).expect("dir content"),
        "inside the directory\n"
    );
    assert!(
        stdout.contains(
            "CONFLICT (modify/delete): foo~HEAD deleted in feature and modified in HEAD.  Version HEAD of foo~HEAD left in tree."
        ),
        "Git's modify/delete line for the MOVED name: {stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD")).expect("moved file"),
        "edited file\n",
        "our version is left verbatim at the unique path, as Git's line says"
    );
    assert!(!p.join("foo").is_file() && index_stage_lines(p, "foo").is_empty());
    let stages = index_stage_lines(p, "foo~HEAD");
    assert!(
        stages.iter().any(|l| l.contains(" 2\t")) && stages.iter().any(|l| l.contains(" 1\t")),
        "stage 2 (ours) and stage 1 (the base's file), nothing at stage 0/3: {stages:?}"
    );
    assert!(
        !stages
            .iter()
            .any(|l| l.contains(" 0\t") || l.contains(" 3\t")),
        "no stage 0/3 for the moved file: {stages:?}"
    );
    let state = read_merge_state(p);
    assert_eq!(
        state["conflicted_paths"],
        serde_json::json!(["foo~HEAD"]),
        "the unmerged path is the moved file"
    );
}

/// G2 + G4: ours made the DIRECTORY, theirs edited the FILE — the file is moved
/// to `foo~<branch>` on stage 3.
#[test]
fn merge_df_conflict_file_on_theirs_is_moved_to_branch_suffix() {
    let repo = create_df_conflict_repo(false, true);
    let p = repo.path();
    let output = merge_expecting_conflict(p, &["merge", "feature"], &[]);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains(
            "CONFLICT (file/directory): directory in the way of foo from feature; moving it to foo~feature instead."
        ),
        "{stdout}"
    );
    assert!(p.join("foo").is_dir());
    assert!(
        stdout.contains(
            "CONFLICT (modify/delete): foo~feature deleted in HEAD and modified in feature.  Version feature of foo~feature left in tree."
        ),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo~feature")).expect("moved file"),
        "edited file\n"
    );
    let stages = index_stage_lines(p, "foo~feature");
    assert!(
        stages.iter().any(|l| l.contains(" 3\t")) && stages.iter().any(|l| l.contains(" 1\t")),
        "stage 3 (theirs) and stage 1 (base): {stages:?}"
    );
    assert_eq!(
        read_merge_state(p)["conflicted_paths"],
        serde_json::json!(["foo~feature"])
    );
}

/// G7 + G8: `--abort` restores the pre-merge file `foo`, removes the directory
/// the merge created and the moved `foo~HEAD`, and leaves no merge state.
#[test]
fn merge_df_conflict_abort_restores_the_file_and_removes_the_moved_copy() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    let head_before = head_commit(p);
    merge_expecting_conflict(p, &["merge", "feature"], &[]);
    assert!(p.join("foo~HEAD").exists() && p.join("foo").is_dir());

    assert_cli_success(&run_libra_command(&["merge", "--abort"], p), "abort");
    assert_eq!(head_commit(p), head_before);
    assert!(p.join("foo").is_file(), "foo is a file again");
    assert_eq!(
        std::fs::read_to_string(p.join("foo")).expect("foo"),
        "edited file\n"
    );
    assert!(!p.join("foo~HEAD").exists(), "the moved copy is cleaned up");
    assert!(!p.join(".libra").join("merge-state.json").exists());
    let status = run_libra_command(&["status", "--short"], p);
    assert_cli_success(&status, "status");
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "clean after abort: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

/// The moved file resolves like any unmerged path: staging it lets
/// `--continue` finish, and the merge commit carries BOTH the directory and
/// the moved file.
#[test]
fn merge_df_conflict_continue_after_staging_the_moved_file() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    merge_expecting_conflict(p, &["merge", "feature"], &[]);
    assert_cli_success(
        &run_libra_command(&["add", "foo~HEAD"], p),
        "stage the moved file",
    );
    assert_cli_success(&run_libra_command(&["merge", "--continue"], p), "continue");
    let listing = run_libra_command(&["ls-tree", "-r", "HEAD"], p);
    assert_cli_success(&listing, "ls-tree");
    let listing = String::from_utf8_lossy(&listing.stdout).to_string();
    assert!(
        listing.contains("\tfoo/bar.txt") && listing.contains("\tfoo~HEAD"),
        "{listing}"
    );
}

/// Both walks reach the same D/F verdict (the collision pass is shared).
#[test]
fn merge_df_conflict_is_identical_on_the_flat_walk() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    // The complete `--dry-run` summaries agree, `files_changed` included
    // (Codex R3: the incremental count is adjusted when the post-pass moves a
    // kept file out of the result).
    let summaries: Vec<serde_json::Value> = [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ]
    .iter()
    .map(|env| {
        let out = run_libra_command_with_stdin_and_env(
            &["--json", "merge", "--dry-run", "feature"],
            p,
            "",
            env,
        );
        assert_eq!(out.status.code(), Some(1));
        parse_json_stdout(&out)["data"].clone()
    })
    .collect();
    assert_eq!(
        summaries[0], summaries[1],
        "both walks preview the same summary"
    );
    assert_eq!(
        summaries[0]["files_changed"], 2,
        "foo gone from the result + foo/bar.txt"
    );
    let output = merge_expecting_conflict(
        p,
        &["merge", "feature"],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")],
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("moving it to foo~HEAD instead."),
        "the flat walk prints the same line"
    );
    assert!(p.join("foo").is_dir() && p.join("foo~HEAD").is_file());
    let stages = index_stage_lines(p, "foo~HEAD");
    assert!(stages.iter().any(|l| l.contains(" 1	")) && stages.iter().any(|l| l.contains(" 2	")));
    assert_eq!(
        read_merge_state(p)["conflicted_paths"],
        serde_json::json!(["foo~HEAD"])
    );
}

/// Pure D/F (no file in the base): ours ADDED `foo`, theirs added `foo/`. Git
/// records only our side at `foo~HEAD` (stage 2, no stage 1) and prints just
/// the file/directory line — verified against `git merge`: `AU foo~HEAD`.
#[test]
fn merge_df_conflict_added_file_is_moved_with_only_its_own_stage() {
    let repo = create_df_conflict_repo(true, false);
    let p = repo.path();
    let output = merge_expecting_conflict(p, &["merge", "feature"], &[]);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains(
            "CONFLICT (file/directory): directory in the way of foo from HEAD; moving it to foo~HEAD instead."
        ),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD")).expect("moved file"),
        "edited file\n",
        "a one-sided add is written verbatim"
    );
    let stages = index_stage_lines(p, "foo~HEAD");
    assert_eq!(stages.len(), 1, "{stages:?}");
    assert!(stages[0].contains(" 2\t"), "stage 2 only: {stages:?}");
    assert!(p.join("foo").is_dir());
}

/// An UNCHANGED file replaced by a directory on the other side is a clean
/// deletion, with no D/F message — verified against `git merge` on a divergent
/// history (`Merge made by the 'ort' strategy`, `delete mode 100644 foo`).
#[test]
fn merge_df_unchanged_file_replaced_by_a_directory_merges_cleanly() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "other.txt", "0\n", "root");
    assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
    commit_file(p, "foo", "base file\n", "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    commit_file(p, "other.txt", "ours\n", "unrelated");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    assert_cli_success(&run_libra_command(&["rm", "foo"], p), "rm");
    std::fs::create_dir_all(p.join("foo")).expect("dir");
    std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
    assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "dir", "--no-verify"], p),
        "dir",
    );
    // `checkout` cannot flip foo/ back into a file: go through the hub.
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    let out = run_libra_command(&["merge", "feature"], p);
    assert_cli_success(&out, "clean");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("CONFLICT"),
        "no D/F message on a clean deletion"
    );
    assert!(p.join("foo").is_dir() && p.join("foo/bar.txt").is_file());
    assert!(index_stage_lines(p, "foo").is_empty());
}

/// A directory that merges to NOTHING is not in the way: the file stays put
/// (Git: "directory no longer in the way"). Ours deletes `foo/` entirely,
/// theirs adds file `foo` — a plain one-sided add, no conflict.
#[test]
fn merge_df_conflict_is_not_raised_when_the_directory_merges_to_nothing() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::create_dir_all(p.join("foo")).expect("dir");
    std::fs::write(p.join("foo/bar.txt"), "x\n").expect("bar");
    assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base with dir", "--no-verify"], p),
        "base",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    // ours: delete the directory
    std::fs::remove_dir_all(p.join("foo")).expect("rm dir");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage removal");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "drop foo/", "--no-verify"], p),
        "ours",
    );
    // theirs: also delete the directory and add file foo
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    std::fs::remove_dir_all(p.join("foo")).expect("rm dir");
    std::fs::write(p.join("foo"), "now a file\n").expect("file");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "foo is a file", "--no-verify"], p),
        "theirs",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    assert_cli_success(&run_libra_command(&["merge", "feature"], p), "clean merge");
    assert_eq!(
        std::fs::read_to_string(p.join("foo")).expect("foo"),
        "now a file\n"
    );
}

/// Codex MG-04 R1 (occupied name + nested directory), verified against
/// `git merge`: a tracked `foo~HEAD/` directory occupies the name, so the moved
/// file becomes `foo~HEAD_0`; the directory in the way is nested
/// (`foo/a/bar.txt`), and both `--restart` and `--abort` must put the file
/// `foo` back where `foo/a/` stood — the emptied directories are pruned rather
/// than left to block the write.
#[test]
fn merge_df_conflict_occupied_name_and_nested_directory_restart_and_abort() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "other.txt", "0\n", "root");
    assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
    std::fs::create_dir_all(p.join("foo~HEAD")).expect("dir");
    std::fs::write(p.join("foo~HEAD/bar.txt"), "taken\n").expect("taken");
    std::fs::write(p.join("foo"), "base file\n").expect("foo");
    assert_cli_success(
        &run_libra_command(&["add", "foo", "foo~HEAD/bar.txt"], p),
        "add",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "base",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    commit_file(p, "foo", "edited file\n", "ours edit");
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    assert_cli_success(&run_libra_command(&["rm", "foo"], p), "rm");
    std::fs::create_dir_all(p.join("foo/a")).expect("nested");
    std::fs::write(p.join("foo/a/bar.txt"), "nested\n").expect("nested file");
    assert_cli_success(
        &run_libra_command(&["add", "foo/a/bar.txt"], p),
        "add nested",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "nested dir", "--no-verify"], p),
        "dir",
    );
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    let head_before = head_commit(p);

    let out = merge_expecting_conflict(p, &["merge", "feature"], &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains(
            "CONFLICT (file/directory): directory in the way of foo from HEAD; moving it to foo~HEAD_0 instead."
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "CONFLICT (modify/delete): foo~HEAD_0 deleted in feature and modified in HEAD.  Version HEAD of foo~HEAD_0 left in tree."
        ),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD_0")).expect("moved"),
        "edited file\n"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD/bar.txt")).expect("occupying dir"),
        "taken\n"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo/a/bar.txt")).expect("nested"),
        "nested\n"
    );
    let stages = index_stage_lines(p, "foo~HEAD_0");
    assert!(
        stages.iter().any(|l| l.contains(" 1\t")) && stages.iter().any(|l| l.contains(" 2\t")),
        "{stages:?}"
    );
    assert_eq!(
        read_merge_state(p)["conflicted_paths"],
        serde_json::json!(["foo~HEAD_0"])
    );

    // `--restart` re-runs from a restored tree and hits the same collision.
    let restart = merge_expecting_conflict(p, &["merge", "--restart"], &[]);
    assert!(String::from_utf8_lossy(&restart.stdout).contains("moving it to foo~HEAD_0 instead."));
    assert!(p.join("foo~HEAD_0").is_file() && p.join("foo/a/bar.txt").is_file());
    assert_eq!(
        read_merge_state(p)["conflicted_paths"],
        serde_json::json!(["foo~HEAD_0"])
    );

    assert_cli_success(&run_libra_command(&["merge", "--abort"], p), "abort");
    assert_eq!(head_commit(p), head_before);
    assert!(
        p.join("foo").is_file(),
        "foo is the file again (foo/a/ was pruned)"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo")).expect("foo"),
        "edited file\n"
    );
    assert!(!p.join("foo~HEAD_0").exists());
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD/bar.txt")).expect("kept"),
        "taken\n"
    );
    let status = run_libra_command(&["status", "--short"], p);
    assert_cli_success(&status, "status");
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "{}",
        String::from_utf8_lossy(&status.stdout)
    );
}

/// Codex MG-04 R1: under `--json` the D/F announcement must not reach stdout —
/// the conflict travels in the error envelope on stderr, and the merge state is
/// still written.
#[test]
fn merge_df_conflict_json_keeps_stdout_machine_clean() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    let output = merge_expecting_conflict(p, &["--json", "merge", "feature"], &[]);
    assert!(
        String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "stdout must stay machine-clean: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(p.join("foo~HEAD").is_file() && p.join("foo").is_dir());
    assert_eq!(
        read_merge_state(p)["conflicted_paths"],
        serde_json::json!(["foo~HEAD"])
    );
}

/// MG-04, verified against real `git merge` (git@3cb9185f6 + `git mktree`):
/// a directory holding nothing but an EMPTY tree is "in the way" of the file
/// at the same path only when the merge base had nothing there.
///
/// * base has `foo` as a file and ours edits it → Git traverses the new
///   directory, finds no file, and reports a plain
///   `CONFLICT (modify/delete): foo` with stages 1 + 2 at `foo` itself;
/// * base has nothing at `foo` and ours adds it → Git defers the new
///   directory and adopts its tree verbatim, so the file moves to `foo~HEAD`.
///
/// Both walks must agree on both shapes.
#[test]
fn merge_df_conflict_empty_subtree_follows_gits_base_presence_rule() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        // (1) the base tracks the file: no relocation.
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        commit_file(p, "foo", "base file\n", "base with file foo");
        let theirs = craft_commit_with_empty_dir(p, &head_commit(p), true, Some("bar"));
        assert_cli_success(
            &run_libra_command(&["update-ref", "refs/heads/feature", &theirs], p),
            "refs/heads/feature",
        );
        commit_file(p, "foo", "edited file\n", "ours edits foo");

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        assert!(
            !stdout.contains("file/directory"),
            "walk {env:?}: an empty-only subtree the base already had is not in the way: {stdout}"
        );
        assert!(
            !p.join("foo~HEAD").exists(),
            "walk {env:?}: nothing was moved"
        );
        let stages = index_stage_lines(p, "foo");
        assert!(
            stages.iter().any(|l| l.contains(" 1\t")) && stages.iter().any(|l| l.contains(" 2\t")),
            "walk {env:?}: the modify/delete stays at `foo`: {stages:?}"
        );
        assert_eq!(
            read_merge_state(p)["conflicted_paths"],
            serde_json::json!(["foo"])
        );

        // (2) the base has nothing at `foo`: Git relocates the added file.
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        let theirs = craft_commit_with_empty_dir(p, &head_commit(p), false, Some("bar"));
        assert_cli_success(
            &run_libra_command(&["update-ref", "refs/heads/feature", &theirs], p),
            "refs/heads/feature",
        );
        commit_file(p, "foo", "added file\n", "ours adds foo");

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        assert!(
            stdout.contains(
                "CONFLICT (file/directory): directory in the way of foo from HEAD; moving it to foo~HEAD instead."
            ),
            "walk {env:?}: {stdout}"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("foo~HEAD")).expect("moved file"),
            "added file\n"
        );
        let stages = index_stage_lines(p, "foo~HEAD");
        assert_eq!(stages.len(), 1, "walk {env:?}: stage 2 only: {stages:?}");
        assert!(stages[0].contains(" 2\t"), "walk {env:?}: {stages:?}");
        assert!(index_stage_lines(p, "foo").is_empty(), "walk {env:?}");
    }
}

/// Codex MG-04 R2, verified against `git merge`: a `foo~HEAD` only the merge
/// base had — deleted on both sides, absent from the result — still occupies
/// its name (Git's `unique_path` consults every input path), so the moved file
/// becomes `foo~HEAD_0`. Both walks.
#[test]
fn merge_df_conflict_deleted_input_path_still_occupies_its_name() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        std::fs::write(p.join("foo"), "base file\n").expect("foo");
        std::fs::write(p.join("foo~HEAD"), "gone\n").expect("foo~HEAD");
        assert_cli_success(&run_libra_command(&["add", "foo", "foo~HEAD"], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "foo~HEAD"], p),
            "ours drops foo~HEAD",
        );
        commit_file(p, "foo", "edited file\n", "ours edit + drop");
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "foo", "foo~HEAD"], p),
            "theirs drops both",
        );
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add dir");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "dir", "--no-verify"], p),
            "dir",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        // An UNTRACKED `foo~HEAD` recreated since is not the merge's to delete
        // (Codex R3): only paths tracked NOW and absent from the result go.
        std::fs::write(p.join("foo~HEAD"), "untracked\n").expect("untracked");

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("moving it to foo~HEAD_0 instead."),
            "walk {env:?}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(
            std::fs::read_to_string(p.join("foo~HEAD_0")).expect("moved"),
            "edited file\n"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("foo~HEAD")).expect("untracked survives"),
            "untracked\n",
            "walk {env:?}: a path the merge never tracked is left alone"
        );
        assert_eq!(
            read_merge_state(p)["conflicted_paths"],
            serde_json::json!(["foo~HEAD_0"])
        );
    }
}

/// Codex MG-04 R2, verified against `git merge -X theirs` / `-X ours`: a
/// strategy option settles content hunks, not a modify/delete under a
/// directory — the edited file still moves to `foo~HEAD` with stages 1 + 2
/// and both lines, whichever side `-X` favours.
#[test]
fn merge_df_conflict_strategy_option_keeps_the_modify_delete_conflict() {
    for favour in ["theirs", "ours"] {
        let repo = create_df_conflict_repo(true, true);
        let p = repo.path();
        let output = merge_expecting_conflict(p, &["merge", "-X", favour, "feature"], &[]);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        assert!(
            stdout.contains("moving it to foo~HEAD instead.")
                && stdout.contains(
                    "CONFLICT (modify/delete): foo~HEAD deleted in feature and modified in HEAD."
                ),
            "-X {favour}: {stdout}"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("foo~HEAD")).expect("moved file"),
            "edited file\n",
            "-X {favour}: the edited version is kept, not discarded"
        );
        let stages = index_stage_lines(p, "foo~HEAD");
        assert!(
            stages.iter().any(|l| l.contains(" 1\t")) && stages.iter().any(|l| l.contains(" 2\t")),
            "-X {favour}: {stages:?}"
        );
        assert!(p.join("foo").is_dir());
    }
}

/// Codex MG-04 R2: an IGNORED symlink sitting where the moved file goes is
/// invisible to the untracked scan; the write must replace the link itself,
/// never follow it out of the working tree.
#[cfg(unix)]
#[test]
fn merge_df_conflict_replaces_an_ignored_symlink_at_the_moved_name() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    let outside = tempfile::tempdir().expect("outside");
    std::fs::write(outside.path().join("target.txt"), "outside\n").expect("target");
    // The ignore rule is committed on main (the fixture tracks `.libraignore`,
    // so a bare edit would count as a dirty worktree).
    std::fs::write(p.join(".libraignore"), "foo~HEAD\n").expect("ignore");
    assert_cli_success(
        &run_libra_command(&["add", ".libraignore"], p),
        "add ignore",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ignore foo~HEAD", "--no-verify"], p),
        "commit ignore",
    );
    std::os::unix::fs::symlink(outside.path().join("target.txt"), p.join("foo~HEAD"))
        .expect("symlink");

    merge_expecting_conflict(p, &["merge", "feature"], &[]);
    let meta = std::fs::symlink_metadata(p.join("foo~HEAD")).expect("moved file");
    assert!(
        meta.file_type().is_file(),
        "the link was replaced by the file"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD")).expect("moved file"),
        "edited file\n"
    );
    assert_eq!(
        std::fs::read_to_string(outside.path().join("target.txt")).expect("outside"),
        "outside\n",
        "nothing was written through the link"
    );
}

/// Codex MG-04 R2: a merge never writes THROUGH a symlinked directory (Git:
/// "beyond a symbolic link"). An ignored `foo -> <outside>` would redirect
/// `foo/bar.txt`; the merge is refused before HEAD, index or the outside
/// directory change — on both walks.
#[cfg(unix)]
#[test]
fn merge_refuses_to_write_through_an_ignored_symlinked_directory() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(p, "other.txt", "ours\n", "unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "dir", "--no-verify"], p),
            "dir",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(p.join(".libraignore"), "foo\n").expect("ignore");
        assert_cli_success(
            &run_libra_command(&["add", ".libraignore"], p),
            "add ignore",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ignore foo", "--no-verify"], p),
            "commit ignore",
        );
        std::os::unix::fs::symlink(outside.path(), p.join("foo")).expect("symlink");
        let head_before = head_commit(p);

        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert!(
            !output.status.success(),
            "walk {env:?}: the merge must be refused"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("symbolic link"), "walk {env:?}: {stderr}");
        assert_eq!(head_commit(p), head_before, "walk {env:?}: HEAD untouched");
        assert!(
            !outside.path().join("bar.txt").exists(),
            "walk {env:?}: nothing was written outside the working tree"
        );
        assert!(
            index_stage_lines(p, "foo/bar.txt").is_empty(),
            "walk {env:?}: index untouched"
        );
        assert!(!p.join(".libra").join("merge-state.json").exists());
    }
}

/// Build a commit whose root tree holds an EMPTY directory `foo` next to the
/// blobs of `parent`'s tree — a shape no working tree can produce, crafted
/// through the plumbing. Returns the commit id.
fn craft_commit_with_empty_dir(
    p: &Path,
    parent: &str,
    drop_foo_blob: bool,
    nested: Option<&str>,
) -> String {
    let raw = |hex: &str| -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect()
    };
    let hash_tree = |bytes: &[u8], name: &str| -> String {
        let path = p.join(name);
        std::fs::write(&path, bytes).expect("tree bytes");
        let out = run_libra_command(&["hash-object", "-t", "tree", "-w", "--literally", name], p);
        assert_cli_success(&out, "hash-object tree");
        std::fs::remove_file(&path).expect("cleanup");
        stdout_trimmed(&out)
    };
    let empty = hash_tree(b"", ".empty-tree");
    // The parent's leaves (blobs at the root only in these fixtures).
    let listing = run_libra_command(&["ls-tree", parent], p);
    assert_cli_success(&listing, "ls-tree");
    let mode_of = |meta: &str| meta.split_whitespace().next().expect("mode").to_string();
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for line in String::from_utf8_lossy(&listing.stdout).lines() {
        let (meta, name) = line.split_once('\t').expect("ls-tree line");
        if name == "foo" {
            // The crafted tree provides `foo` itself (as the directory below);
            // whatever the parent had there — a blob or a directory — is
            // replaced.
            assert!(
                drop_foo_blob || mode_of(meta) == "040000",
                "`foo` is replaced"
            );
            continue;
        }
        let mut parts = meta.split_whitespace();
        let mode = parts.next().expect("mode");
        let _kind = parts.next();
        let id = parts.next().expect("id");
        assert_ne!(mode, "040000", "fixture roots hold blobs beside `foo` only");
        let mut entry = format!("{} {name}\0", mode.trim_start_matches('0')).into_bytes();
        entry.extend(raw(id));
        entries.push((name.to_string(), entry));
    }
    // `nested`: `foo/` holds one EMPTY subtree under that name (a directory
    // that contributes no file); `None` makes `foo` itself the empty tree.
    let foo_tree = if let Some(name) = nested {
        let mut child = format!("40000 {name}\0").into_bytes();
        child.extend(raw(&empty));
        hash_tree(&child, ".foo-tree")
    } else {
        empty.clone()
    };
    let mut foo_entry = b"40000 foo\0".to_vec();
    foo_entry.extend(raw(&foo_tree));
    // Git orders tree entries by name, a directory as `name/`.
    entries.push(("foo/".to_string(), foo_entry));
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let root: Vec<u8> = entries.into_iter().flat_map(|(_, bytes)| bytes).collect();
    let root = hash_tree(&root, ".root-tree");
    let out = run_libra_command(
        &[
            "commit-tree",
            &root,
            "-p",
            parent,
            "-m",
            "crafted empty dir",
        ],
        p,
    );
    assert_cli_success(&out, "commit-tree");
    stdout_trimmed(&out)
}

/// Codex MG-04 R3: an empty `foo/` in the BASE next to two different files
/// `foo` added on either side is an add/add conflict — stages 2 and 3, no
/// stage 1 (the base's directory marker is not a file) — on both walks.
#[test]
fn merge_add_add_conflict_over_an_empty_base_directory_has_no_stage_1() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        let base = craft_commit_with_empty_dir(p, &head_commit(p), false, None);
        for branch in ["main", "feature"] {
            assert_cli_success(
                &run_libra_command(&["update-ref", &format!("refs/heads/{branch}"), &base], p),
                "branch at the crafted base",
            );
        }
        commit_file(p, "foo", "a\n", "ours adds foo");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        commit_file(p, "foo", "b\n", "theirs adds foo");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        merge_expecting_conflict(p, &["merge", "feature"], env);
        let stages = index_stage_lines(p, "foo");
        assert!(
            stages.iter().any(|l| l.contains(" 2\t")) && stages.iter().any(|l| l.contains(" 3\t")),
            "walk {env:?}: {stages:?}"
        );
        assert!(
            !stages.iter().any(|l| l.contains(" 1\t")),
            "walk {env:?}: the base's empty directory is not a stage: {stages:?}"
        );
        assert_eq!(
            read_merge_state(p)["conflicted_paths"],
            serde_json::json!(["foo"])
        );
    }
}

/// Codex MG-04 R3 (P2): an empty base directory turning into a file on one
/// side is one added file — both walks count it, and the merge is clean.
#[test]
fn merge_empty_directory_turning_into_a_file_counts_one_change() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        let base = craft_commit_with_empty_dir(p, &head_commit(p), false, None);
        for branch in ["main", "feature"] {
            assert_cli_success(
                &run_libra_command(&["update-ref", &format!("refs/heads/{branch}"), &base], p),
                "branch at the crafted base",
            );
        }
        commit_file(p, "other.txt", "ours\n", "unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        commit_file(p, "foo", "now a file\n", "theirs adds foo");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let preview = run_libra_command_with_stdin_and_env(
            &["--json", "merge", "--dry-run", "feature"],
            p,
            "",
            env,
        );
        assert_cli_success(&preview, "dry-run");
        let data = parse_json_stdout(&preview)["data"].clone();
        assert_eq!(data["files_changed"], 1, "walk {env:?}: {data}");
        assert!(data["would_conflict"].is_null());
        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        assert_eq!(
            std::fs::read_to_string(p.join("foo")).expect("foo"),
            "now a file\n"
        );
    }
}

/// Codex MG-04 R3: a TRACKED symlink `foo` on ours giving way to theirs'
/// directory `foo/` is a legitimate transition — the link is one of the paths
/// the merge removes, so the write of `foo/bar.txt` is allowed, and the link
/// moves to `foo~HEAD` (stage 2, mode 120000) like any D/F file. Both walks.
#[cfg(unix)]
#[test]
fn merge_df_conflict_tracked_symlink_gives_way_to_a_directory() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        std::os::unix::fs::symlink("other.txt", p.join("foo")).expect("symlink");
        assert_cli_success(&run_libra_command(&["add", "foo"], p), "add link");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ours: symlink foo", "--no-verify"], p),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add dir");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs: dir foo", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("moving it to foo~HEAD instead."),
            "walk {env:?}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            p.join("foo").is_dir() && p.join("foo/bar.txt").is_file(),
            "walk {env:?}"
        );
        let stages = index_stage_lines(p, "foo~HEAD");
        assert_eq!(stages.len(), 1, "walk {env:?}: {stages:?}");
        assert!(
            stages[0].starts_with("120000") && stages[0].contains(" 2\t"),
            "walk {env:?}: the link itself is what moved: {stages:?}"
        );
        // Git checks out a real link at the moved name, not its target text.
        let moved = std::fs::symlink_metadata(p.join("foo~HEAD")).expect("moved link");
        assert!(
            moved.file_type().is_symlink(),
            "walk {env:?}: a symlink moved as a symlink"
        );
        assert_eq!(
            std::fs::read_link(p.join("foo~HEAD")).expect("link target"),
            std::path::PathBuf::from("other.txt"),
            "walk {env:?}"
        );
    }
}

/// Codex MG-04 R3: a tracked `gone/file` the merge removes, while the working
/// tree's `gone` is an IGNORED symlink to a directory outside the repository:
/// unlinking through the link would delete an external file, so the merge is
/// refused before anything changes — both walks.
#[cfg(unix)]
#[test]
fn merge_refuses_to_unlink_a_tracked_file_through_an_ignored_symlinked_directory() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        std::fs::write(p.join("other.txt"), "0\n").expect("other");
        std::fs::create_dir_all(p.join("gone")).expect("dir");
        std::fs::write(p.join("gone/file"), "keep\n").expect("file");
        assert_cli_success(
            &run_libra_command(&["add", "other.txt", "gone/file"], p),
            "add",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
            "root",
        );
        std::fs::write(p.join(".libraignore"), "gone\n").expect("ignore");
        assert_cli_success(
            &run_libra_command(&["add", ".libraignore"], p),
            "add ignore",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ignore gone", "--no-verify"], p),
            "ignore",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(p, "other.txt", "ours\n", "unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "gone/file"], p),
            "theirs drops it",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "drop gone/file", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("file"), "keep\n").expect("external file");
        std::fs::remove_dir_all(p.join("gone")).expect("drop the real dir");
        std::os::unix::fs::symlink(outside.path(), p.join("gone")).expect("symlink");
        let head_before = head_commit(p);

        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert!(!output.status.success(), "walk {env:?}: refused");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("symbolic link"), "walk {env:?}: {stderr}");
        assert_eq!(head_commit(p), head_before, "walk {env:?}: HEAD untouched");
        assert_eq!(
            std::fs::read_to_string(outside.path().join("file")).expect("external file"),
            "keep\n",
            "walk {env:?}: nothing outside the working tree was unlinked"
        );
        assert!(!p.join(".libra").join("merge-state.json").exists());
    }
}

/// Codex MG-04 R3: a hook that plants an IGNORED symlink where the merge will
/// write is caught by the post-hook traversal recheck — before HEAD moves on
/// the flat walk, and inherently before HEAD on the incremental walk. Both
/// hook checkpoints, both walks.
#[cfg(unix)]
#[test]
fn merge_refuses_a_hook_planted_ignored_symlink_before_head_moves() {
    use std::os::unix::fs::PermissionsExt;
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        for hook_name in ["pre-merge-commit", "commit-msg"] {
            let repo = create_committed_repo_via_cli();
            let p = repo.path();
            commit_file(p, "top.txt", "0\n", "root");
            assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
            // The ignore rule is ours only, so theirs can still stage the path.
            std::fs::write(p.join(".libraignore"), "newdir\n").expect("ignore");
            assert_cli_success(
                &run_libra_command(&["add", ".libraignore"], p),
                "add ignore",
            );
            assert_cli_success(
                &run_libra_command(&["commit", "-m", "ignore newdir", "--no-verify"], p),
                "ignore",
            );
            commit_file(p, "top.txt", "ours\n", "ours edit");
            assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
            commit_file(
                p,
                "newdir/sub/leaf.txt",
                "theirs\n",
                "theirs adds a subtree",
            );
            assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
            let outside = tempfile::tempdir().expect("outside");
            let hooks = p.join(".libra").join("hooks");
            std::fs::create_dir_all(&hooks).expect("hooks dir");
            let hook = hooks.join(hook_name);
            std::fs::write(
                &hook,
                format!(
                    "#!/bin/sh\nln -s '{}' \"$LIBRA_WORK_TREE/newdir\"\n",
                    outside.path().display()
                ),
            )
            .expect("write hook");
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            let head_before = head_commit(p);

            let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
            assert!(
                !output.status.success(),
                "walk {env:?} hook {hook_name}: refused"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("symbolic link"),
                "walk {env:?} hook {hook_name}: {stderr}"
            );
            assert_eq!(
                head_commit(p),
                head_before,
                "walk {env:?} hook {hook_name}: HEAD"
            );
            assert!(
                !outside.path().join("sub").exists(),
                "walk {env:?} hook {hook_name}: nothing written outside"
            );
            assert!(!p.join(".libra").join("merge-state.json").exists());
        }
    }
}

/// Codex MG-04 pre-review: a tracked DANGLING symlink `foo` giving way to
/// theirs' directory `foo/` must merge cleanly — the removal test may not
/// follow the link (`exists()` does), or the write of `foo/bar.txt` fails
/// after the flat engine has already moved HEAD. Verified against
/// `git merge`: `delete mode 120000 foo` / `create mode 100644 foo/bar.txt`.
#[cfg(unix)]
#[test]
fn merge_dangling_tracked_symlink_gives_way_to_a_directory_cleanly() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        // The hub must predate both shapes: `checkout` cannot flip a path
        // between a directory and a file/symlink (registered residual).
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        std::os::unix::fs::symlink("does-not-exist", p.join("foo")).expect("dangling link");
        assert_cli_success(&run_libra_command(&["add", "foo"], p), "add link");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base: dangling link", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(p, "other.txt", "ours\n", "ours unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "foo"], p),
            "theirs drops the link",
        );
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add dir");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs: dir foo", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        assert_eq!(
            std::fs::read_to_string(p.join("foo/bar.txt")).expect("dir content"),
            "bar\n",
            "walk {env:?}"
        );
        assert!(
            std::fs::symlink_metadata(p.join("foo"))
                .expect("foo")
                .is_dir(),
            "walk {env:?}: the dangling link gave way"
        );
        let status = run_libra_command(&["status", "--short"], p);
        assert_cli_success(&status, "status");
        assert!(
            String::from_utf8_lossy(&status.stdout).trim().is_empty(),
            "walk {env:?}: HEAD, index and worktree agree: {}",
            String::from_utf8_lossy(&status.stdout)
        );
    }
}

/// Codex MG-04 pre-review: when the merge is REFUSED at the moved name (an
/// untracked `foo~HEAD` would be overwritten) nothing was moved, so — as in
/// Git — no `CONFLICT (file/directory)` line is printed and no merge state is
/// left behind.
#[test]
fn merge_df_conflict_refused_at_the_moved_name_announces_nothing() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    let head_before = head_commit(p);
    std::fs::write(p.join("foo~HEAD"), "untracked\n").expect("untracked");

    let output = run_libra_command(&["merge", "feature"], p);
    assert!(!output.status.success(), "refused");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        !stdout.contains("CONFLICT"),
        "nothing was moved, so nothing is announced: {stdout}"
    );
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-002", "{stderr}");
    assert!(
        stderr.contains("foo~HEAD"),
        "the collision is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before);
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD")).expect("untracked survives"),
        "untracked\n"
    );
    assert!(p.join("foo").is_file(), "our file is untouched");
    assert!(!p.join(".libra").join("merge-state.json").exists());
}

/// Codex MG-04 pre-review: a directory that must become a FILE while IGNORED
/// content lives inside it is refused BEFORE the merge mutates anything —
/// the takeover only ever removes this merge's own tracked files.
#[test]
fn merge_refuses_to_replace_a_directory_holding_ignored_content() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        std::fs::write(p.join(".libraignore"), "foo/keep.log\n").expect("ignore");
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        assert_cli_success(
            &run_libra_command(&["add", ".libraignore", "foo/bar.txt"], p),
            "add",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base: dir foo", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(p, "other.txt", "ours\n", "ours unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "foo/bar.txt"], p),
            "drop the dir",
        );
        std::fs::write(p.join("foo"), "now a file\n").expect("file");
        assert_cli_success(&run_libra_command(&["add", "foo"], p), "add file");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs: foo is a file", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        // An ignored build artefact inside the directory the merge must replace.
        std::fs::write(p.join("foo/keep.log"), "ignored\n").expect("ignored file");
        let head_before = head_commit(p);

        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert!(!output.status.success(), "walk {env:?}: refused");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("foo/keep.log"),
            "walk {env:?}: the blocker is named: {stderr}"
        );
        assert_eq!(head_commit(p), head_before, "walk {env:?}: HEAD untouched");
        assert_eq!(
            std::fs::read_to_string(p.join("foo/keep.log")).expect("ignored file survives"),
            "ignored\n",
            "walk {env:?}"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("foo/bar.txt")).expect("tracked file survives"),
            "bar\n",
            "walk {env:?}: refused before any removal"
        );
    }
}

/// Codex MG-04 pre-review: with more than one conflict the unmerged paths,
/// the announcement and the `--dry-run` report follow path order, as Git's
/// sorted `process_entries` output does — the relocated path included.
#[test]
fn merge_df_conflict_paths_are_reported_in_path_order() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    // A second, ordinary content conflict on a path sorting BEFORE `foo`.
    // (Every switch goes through the `root` hub: `checkout` cannot flip `foo`
    // between a file and a directory.)
    commit_file(p, "a-shared.txt", "ours\n", "ours edits a-shared");
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    commit_file(p, "a-shared.txt", "theirs\n", "theirs edits a-shared");
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

    let preview = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_eq!(preview.status.code(), Some(1));
    let json = parse_json_stdout(&preview);
    assert_eq!(
        json["data"]["conflicted_paths"],
        serde_json::json!(["a-shared.txt", "foo~HEAD"]),
        "{json}"
    );
    merge_expecting_conflict(p, &["merge", "feature"], &[]);
    assert_eq!(
        read_merge_state(p)["conflicted_paths"],
        serde_json::json!(["a-shared.txt", "foo~HEAD"])
    );
}

/// Codex MG-04 R4: a tracked symlink whose target is not valid UTF-8 relocates
/// byte-for-byte (link targets are raw bytes), and an EMPTY directory standing
/// at the moved name is taken over rather than failing after the merge state
/// was written. Both walks.
#[cfg(unix)]
#[test]
fn merge_df_conflict_moves_a_non_utf8_symlink_over_an_empty_directory() {
    use std::os::unix::ffi::OsStrExt;
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        let target = std::ffi::OsStr::from_bytes(&[0xff, 0xfe, b'A']);
        std::os::unix::fs::symlink(target, p.join("foo")).expect("weird link");
        assert_cli_success(&run_libra_command(&["add", "foo"], p), "add link");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base: non-utf8 link", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(p, "other.txt", "ours\n", "ours unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "foo"], p),
            "theirs drops the link",
        );
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add dir");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs: dir foo", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        // Ours also EDITED the link, so it must move instead of being deleted.
        std::fs::remove_file(p.join("foo")).expect("drop the link");
        let edited = std::ffi::OsStr::from_bytes(&[0xff, 0xfe, b'B']);
        std::os::unix::fs::symlink(edited, p.join("foo")).expect("edited link");
        assert_cli_success(&run_libra_command(&["add", "foo"], p), "stage the edit");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "ours: retarget the link", "--no-verify"],
                p,
            ),
            "ours",
        );
        // An EMPTY directory already sits where the link will move.
        std::fs::create_dir_all(p.join("foo~HEAD")).expect("empty dir at the moved name");

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("moving it to foo~HEAD instead."),
            "walk {env:?}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let moved = std::fs::read_link(p.join("foo~HEAD")).expect("moved link");
        assert_eq!(
            moved.as_os_str().as_bytes(),
            &[0xff, 0xfe, b'B'],
            "walk {env:?}: the raw target survives"
        );
        let stages = index_stage_lines(p, "foo~HEAD");
        assert!(
            stages.iter().any(|line| line.starts_with("120000")),
            "walk {env:?}: {stages:?}"
        );
    }
}

/// Codex MG-04 R4, verified against `git merge`: an IGNORED file standing at
/// the moved name is expendable — Git replaces it (only *untracked,
/// non-ignored* files refuse the merge), and Libra follows.
#[test]
fn merge_df_conflict_replaces_an_ignored_file_at_the_moved_name_like_git() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    std::fs::write(p.join(".libraignore"), "foo~HEAD\n").expect("ignore");
    assert_cli_success(
        &run_libra_command(&["add", ".libraignore"], p),
        "add ignore",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ignore the moved name", "--no-verify"], p),
        "commit ignore",
    );
    std::fs::write(p.join("foo~HEAD"), "expendable\n").expect("ignored file");

    let output = merge_expecting_conflict(p, &["merge", "feature"], &[]);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("moving it to foo~HEAD instead."),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD")).expect("moved file"),
        "edited file\n",
        "as in Git, an ignored file at the moved name is replaced"
    );
}

/// Codex MG-04 R5: `--abort` removes a moved DANGLING symlink. The restored
/// index does not track the moved name, so nothing else would ever clean it
/// up, and `is_file()` follows the link and reports false for a dangling one.
#[cfg(unix)]
#[test]
fn merge_df_conflict_abort_removes_a_moved_dangling_symlink() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        std::os::unix::fs::symlink("does-not-exist", p.join("foo")).expect("dangling link");
        assert_cli_success(&run_libra_command(&["add", "foo"], p), "add link");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base: dangling link", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        // Ours retargets the link, so it must MOVE rather than be deleted.
        std::fs::remove_file(p.join("foo")).expect("drop");
        std::os::unix::fs::symlink("still-missing", p.join("foo")).expect("retarget");
        assert_cli_success(&run_libra_command(&["add", "foo"], p), "stage retarget");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ours: retarget", "--no-verify"], p),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "foo"], p),
            "theirs drops the link",
        );
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add dir");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs: dir foo", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        let head_before = head_commit(p);

        merge_expecting_conflict(p, &["merge", "feature"], env);
        assert!(
            std::fs::symlink_metadata(p.join("foo~HEAD"))
                .expect("moved link")
                .file_type()
                .is_symlink(),
            "walk {env:?}"
        );
        assert_cli_success(&run_libra_command(&["merge", "--abort"], p), "abort");
        assert_eq!(head_commit(p), head_before, "walk {env:?}");
        assert!(
            std::fs::symlink_metadata(p.join("foo~HEAD")).is_err(),
            "walk {env:?}: the moved dangling link is cleaned up"
        );
        let status = run_libra_command(&["status", "--short"], p);
        assert_cli_success(&status, "status");
        assert!(
            String::from_utf8_lossy(&status.stdout).trim().is_empty(),
            "walk {env:?}: clean after abort: {}",
            String::from_utf8_lossy(&status.stdout)
        );
    }
}

/// Codex MG-04 R5, verified against `git merge`: rewriting a tracked file
/// REPLACES the directory entry (a new inode), so a hard link to the old
/// content elsewhere keeps its own bytes instead of being truncated in place.
#[cfg(unix)]
#[test]
fn merge_replaces_the_directory_entry_of_a_rewritten_file() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "tracked.txt", "base\n", "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    commit_file(p, "other.txt", "ours\n", "ours unrelated");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    commit_file(p, "tracked.txt", "theirs\n", "theirs rewrites it");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    let alias_dir = tempfile::tempdir().expect("alias dir");
    let alias = alias_dir.path().join("alias.txt");
    std::fs::hard_link(p.join("tracked.txt"), &alias).expect("hard link");

    assert_cli_success(&run_libra_command(&["merge", "feature"], p), "clean merge");
    assert_eq!(
        std::fs::read_to_string(p.join("tracked.txt")).expect("merged file"),
        "theirs\n"
    );
    assert_eq!(
        std::fs::read_to_string(&alias).expect("alias"),
        "base\n",
        "the merge replaced the entry instead of truncating the shared inode"
    );
}

/// Codex MG-04 R6: the flat engine's own flattener (which keeps empty
/// subtrees as markers) must report a missing nested tree as repository
/// corruption instead of panicking, and refuse before HEAD moves.
#[test]
fn merge_flat_walk_reports_a_missing_nested_tree_instead_of_panicking() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "top.txt", "0\n", "root");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    commit_file(p, "ours.txt", "ours\n", "ours edit");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    commit_file(
        p,
        "newdir/sub/leaf.txt",
        "theirs\n",
        "theirs adds a subtree",
    );
    let nested = run_libra_command(&["rev-parse", "HEAD:newdir/sub"], p);
    assert_cli_success(&nested, "rev-parse the nested tree");
    let nested_id = stdout_trimmed(&nested);
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    let object = p
        .join(".libra")
        .join("objects")
        .join(&nested_id[..2])
        .join(&nested_id[2..]);
    assert!(object.exists(), "loose object: {}", object.display());
    std::fs::remove_file(&object).expect("simulate corruption");
    let head_before = head_commit(p);

    let output = run_libra_command_with_stdin_and_env(
        &["merge", "feature"],
        p,
        "",
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")],
    );
    assert!(!output.status.success(), "the merge is refused");
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(
        report.error_code, "LBR-REPO-002",
        "a missing tree is repository corruption, not a panic: {stderr}"
    );
    assert!(
        stderr.contains(&nested_id),
        "the unreadable tree is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "HEAD never moved");
    assert!(!p.join("newdir").exists());
}

/// Codex MG-04 R7: every merge write carries the entry's TYPE and MODE — a
/// tracked symlink stays a symlink and an executable keeps `0755` — on the
/// clean path, through `--abort`, and on both walks. (This also closes the
/// residual MG-03 registered: merge's checkout writer used to leave the
/// executable bit and symlink targets to whatever was already on disk.)
#[cfg(unix)]
#[test]
fn merge_preserves_symlinks_and_executable_bits_when_it_writes() {
    use std::os::unix::fs::PermissionsExt;
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "target.txt", "target\n", "root");
        std::fs::write(p.join("tool.sh"), "#!/bin/sh\necho ours\n").expect("tool");
        std::fs::set_permissions(p.join("tool.sh"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        std::os::unix::fs::symlink("target.txt", p.join("link")).expect("symlink");
        assert_cli_success(&run_libra_command(&["add", "tool.sh", "link"], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base: link + script", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(p, "ours.txt", "ours\n", "ours unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        // Theirs rewrites the script (same mode) so the merge must write it.
        std::fs::write(p.join("tool.sh"), "#!/bin/sh\necho theirs\n").expect("tool");
        assert_cli_success(&run_libra_command(&["add", "tool.sh"], p), "add tool");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "theirs rewrites the script", "--no-verify"],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        // A mode-only change is invisible to `status` (a pre-existing gap), so
        // the merge still runs — and its write must restore `0755` from the
        // entry rather than inherit whatever is on disk.
        std::fs::set_permissions(p.join("tool.sh"), std::fs::Permissions::from_mode(0o600))
            .expect("chmod down");

        assert_cli_success(
            &run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env),
            "clean merge",
        );
        let script = std::fs::metadata(p.join("tool.sh")).expect("script");
        assert_eq!(
            script.permissions().mode() & 0o777,
            0o755,
            "walk {env:?}: the executable bit is written, not inherited"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("tool.sh")).expect("script"),
            "#!/bin/sh\necho theirs\n"
        );
        let link = std::fs::symlink_metadata(p.join("link")).expect("link");
        assert!(
            link.file_type().is_symlink(),
            "walk {env:?}: a tracked symlink is written as a symlink"
        );
        assert_eq!(
            std::fs::read_link(p.join("link")).expect("target"),
            std::path::PathBuf::from("target.txt"),
            "walk {env:?}"
        );
        let listing = run_libra_command(&["ls-files", "-s"], p);
        assert_cli_success(&listing, "ls-files");
        let listing = String::from_utf8_lossy(&listing.stdout).to_string();
        assert!(
            listing.contains("100755") && listing.contains("120000"),
            "walk {env:?}: the index keeps both modes: {listing}"
        );
    }
}

/// Codex MG-04 R8: when the directory is NOT in the way (an empty-only
/// subtree the base already had) the surviving file must not end up beside a
/// leftover subtree entry — the result tree would carry a blob and a directory
/// under one name. Theirs REPLACES the base's empty `foo/bar` with an equally
/// empty `foo/baz`, so the incremental walk records a fresh subtree beneath
/// the file ours keeps. Both walks must produce the same tree: only `foo`.
#[test]
fn merge_keeps_only_the_file_when_an_empty_subtree_is_not_in_the_way() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        let root = head_commit(p);
        // base: `foo/` holding one empty subtree; ours: a file at `foo`.
        let base = craft_commit_with_empty_dir(p, &root, false, Some("bar"));
        for branch in ["main", "feature"] {
            assert_cli_success(
                &run_libra_command(&["update-ref", &format!("refs/heads/{branch}"), &base], p),
                "branch at the crafted base",
            );
        }
        commit_file(p, "foo", "ours file\n", "ours puts a file at foo");
        // theirs: the same shape with a DIFFERENT empty subtree name.
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        let theirs = craft_commit_with_empty_dir(p, &base, false, Some("baz"));
        assert_cli_success(
            &run_libra_command(&["update-ref", "refs/heads/feature", &theirs], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        let listing = run_libra_command(&["ls-tree", "-r", "-t", "HEAD"], p);
        assert_cli_success(&listing, "ls-tree");
        let listing = String::from_utf8_lossy(&listing.stdout).to_string();
        let foo_entries: Vec<&str> = listing
            .lines()
            .filter(|line| line.ends_with("\tfoo") || line.contains("\tfoo/"))
            .collect();
        assert_eq!(
            foo_entries.len(),
            1,
            "walk {env:?}: exactly one entry at `foo`, and it is the file: {foo_entries:?}"
        );
        assert!(
            foo_entries[0].starts_with("100644") && foo_entries[0].ends_with("\tfoo"),
            "walk {env:?}: {foo_entries:?}"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("foo")).expect("foo"),
            "ours file\n",
            "walk {env:?}"
        );
    }
}

/// Codex MG-04 R9, verified against `git merge`: an IGNORED regular file
/// standing where a merged directory must go is replaced by that directory
/// (git: `create mode 100644 foo/bar.txt`, `foo` becomes a directory). Without
/// clearing it, `create_dir_all` fails mid-write — and on the flat engine that
/// happens after HEAD has already moved.
#[test]
fn merge_replaces_an_ignored_file_standing_where_a_directory_must_go() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        std::fs::write(p.join(".libraignore"), "foo\n").expect("ignore");
        assert_cli_success(
            &run_libra_command(&["add", ".libraignore"], p),
            "add ignore",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ignore foo", "--no-verify"], p),
            "ignore",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(p, "other.txt", "ours\n", "ours unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        // A tracked SYMLINK beneath the same directory: its writer takes the
        // other branch and needs the same ancestor rule (Codex R10).
        #[cfg(unix)]
        std::os::unix::fs::symlink("bar.txt", p.join("foo/link")).expect("link");
        #[cfg(unix)]
        let staged = ["add", "-f", "foo/bar.txt", "foo/link"];
        #[cfg(not(unix))]
        let staged = ["add", "-f", "foo/bar.txt"];
        assert_cli_success(&run_libra_command(&staged, p), "add the ignored path");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "theirs adds foo/bar.txt", "--no-verify"],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        // `main` never had `foo/`, so checkout already removed it; put an
        // IGNORED file exactly where the merge must create the directory.
        let _ = std::fs::remove_dir_all(p.join("foo"));
        std::fs::write(p.join("foo"), "expendable\n").expect("ignored file in the way");
        let head_before = head_commit(p);

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        assert_ne!(
            head_commit(p),
            head_before,
            "walk {env:?}: the merge completed"
        );
        assert!(
            p.join("foo").is_dir(),
            "walk {env:?}: the ignored file gave way to the directory"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("foo/bar.txt")).expect("merged file"),
            "bar\n",
            "walk {env:?}"
        );
        let status = run_libra_command(&["status", "--short"], p);
        assert_cli_success(&status, "status");
        assert!(
            String::from_utf8_lossy(&status.stdout).trim().is_empty(),
            "walk {env:?}: HEAD, index and worktree agree: {}",
            String::from_utf8_lossy(&status.stdout)
        );
    }
}

/// MG-05 fixture: the base tracks `old.txt`; ours renames it to `new.txt`
/// (optionally editing it), theirs edits it in place. Returns the repo on
/// `main` with `feature` ready to merge.
fn create_rename_repo(our_edit: Option<&str>, their_edit: &str) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let base = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\n";
    commit_file(p, "old.txt", base, "base tracks old.txt");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    std::fs::rename(p.join("old.txt"), p.join("new.txt")).expect("rename");
    if let Some(content) = our_edit {
        std::fs::write(p.join("new.txt"), content).expect("our edit");
    }
    assert_cli_success(
        &run_libra_command(&["add", "-A", "."], p),
        "stage the rename",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ours renames old.txt", "--no-verify"], p),
        "ours",
    );
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    commit_file(p, "old.txt", their_edit, "theirs edits old.txt");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    repo
}

/// G1 + G2: one side renames, the other edits the same file — the merge is
/// clean and the result carries the other side's edit at the NEW path, on
/// both walks (Git: `detect_regular_renames` + `process_renames`).
#[test]
fn merge_rename_with_other_side_edit_merges_at_the_new_path() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let their = "line1\nline2 edited\nline3\nline4\nline5\nline6\nline7\nline8\n";
        let repo = create_rename_repo(None, their);
        let p = repo.path();
        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        assert_eq!(
            std::fs::read_to_string(p.join("new.txt")).expect("merged file"),
            their,
            "walk {env:?}: the other side's edit followed the rename"
        );
        assert!(
            !p.join("old.txt").exists(),
            "walk {env:?}: the source is gone"
        );
        let listing = run_libra_command(&["ls-files", "-s"], p);
        assert_cli_success(&listing, "ls-files");
        let listing = String::from_utf8_lossy(&listing.stdout).to_string();
        assert!(
            listing.contains("\tnew.txt") && !listing.contains("\told.txt"),
            "walk {env:?}: {listing}"
        );
    }
}

/// G3 + G4 + G5: both sides changed the renamed file in conflicting ways —
/// the conflict is presented at the NEW path, with stage 1 holding the base's
/// content from the ORIGINAL path and stages 2/3 the two sides.
#[test]
fn merge_rename_conflict_is_presented_at_the_new_path() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let ours = "line1\nours\nline3\nline4\nline5\nline6\nline7\nline8\n";
        let theirs = "line1\ntheirs\nline3\nline4\nline5\nline6\nline7\nline8\n";
        let repo = create_rename_repo(Some(ours), theirs);
        let p = repo.path();
        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        let (stderr, report) = parse_cli_error_stderr(&output.stderr);
        assert_eq!(output.status.code(), Some(128), "walk {env:?}: {stderr}");
        assert_eq!(report.error_code, "LBR-CONFLICT-002");
        let markers = std::fs::read_to_string(p.join("new.txt")).expect("conflicted file");
        assert!(
            markers.contains("<<<<<<<") && markers.contains("ours") && markers.contains("theirs"),
            "walk {env:?}: markers at the new path: {markers}"
        );
        let stages = index_stage_lines(p, "new.txt");
        assert_eq!(stages.len(), 3, "walk {env:?}: {stages:?}");
        assert!(index_stage_lines(p, "old.txt").is_empty(), "walk {env:?}");
        // Stage 1 is the base blob — the content the ORIGINAL path had.
        let base_blob = {
            let out = run_libra_command(&["rev-parse", "main~1:old.txt"], p);
            assert_cli_success(&out, "rev-parse the base blob");
            stdout_trimmed(&out)
        };
        assert!(
            stages
                .iter()
                .any(|line| line.contains(&base_blob) && line.contains(" 1\t")),
            "walk {env:?}: stage 1 keeps the base content: {stages:?}"
        );
        assert_eq!(
            read_merge_state(p)["conflicted_paths"],
            serde_json::json!(["new.txt"]),
            "walk {env:?}"
        );
        // `--abort` puts the pre-merge state back.
        assert_cli_success(&run_libra_command(&["merge", "--abort"], p), "abort");
        assert!(p.join("new.txt").is_file() && !p.join("old.txt").exists());
        assert_eq!(
            std::fs::read_to_string(p.join("new.txt")).expect("restored"),
            ours,
            "walk {env:?}"
        );
    }
}

/// Both sides rename the same file to DIFFERENT paths. MG-05 left this
/// unarbitrated — a notice, then a merge that behaved as if neither rename had
/// been detected — and MG-06 turns it into Git's rename/rename conflict. The
/// fixture is kept as the regression for that transition; the shape's stages,
/// merged blob and markers are pinned by `merge_rename_conflict_1to2_*`.
#[test]
fn merge_divergent_renames_conflict_as_rename_rename() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let base = "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\n";
    commit_file(p, "old.txt", base, "base tracks old.txt");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    std::fs::rename(p.join("old.txt"), p.join("ours.txt")).expect("our rename");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ours renames", "--no-verify"], p),
        "ours",
    );
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    std::fs::rename(p.join("old.txt"), p.join("theirs.txt")).expect("their rename");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "theirs renames", "--no-verify"], p),
        "theirs",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

    let out = merge_expecting_conflict(p, &["merge", "feature"], &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains(
            "CONFLICT (rename/rename): old.txt renamed to ours.txt in HEAD and to theirs.txt in feature."
        ),
        "the divergent renames are reported in Git's words: {stdout}"
    );
    // Both destinations are kept, and the source keeps neither a working-tree
    // file nor an index stage. What changed from MG-05 is that the destination
    // pair is now an explicit path-level conflict.
    assert!(p.join("ours.txt").is_file() && p.join("theirs.txt").is_file());
    assert!(!p.join("old.txt").exists());
    assert!(
        index_stage_lines(p, "old.txt").is_empty(),
        "the source is resolved by removal (deviation documented in apply_renames)"
    );
}

/// The notices are human output only: `--json` keeps stdout machine-clean.
#[test]
fn merge_rename_notices_stay_out_of_json_output() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let base = "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\n";
    commit_file(p, "old.txt", base, "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    std::fs::rename(p.join("old.txt"), p.join("ours.txt")).expect("rename");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ours renames", "--no-verify"], p),
        "ours",
    );
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    std::fs::rename(p.join("old.txt"), p.join("theirs.txt")).expect("rename");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "theirs renames", "--no-verify"], p),
        "theirs",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

    // MG-06: this shape is a rename/rename(1to2) conflict now, so the envelope
    // reports the failure — but stdout must still be EXACTLY that envelope,
    // with none of the human-readable CONFLICT prose leaking into it.
    let out = run_libra_command(&["--json", "merge", "feature"], p);
    assert_eq!(
        out.status.code(),
        Some(128),
        "rename/rename(1to2) conflicts"
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !stdout.contains("CONFLICT (") && !stdout.contains("notice:"),
        "the human-readable rename lines stay out of machine output: {stdout}"
    );
    // A conflicted merge reports through the error envelope on stderr, so
    // stdout carries nothing at all here.
    let (stderr, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-002", "{stderr}");
    assert!(
        !stderr.contains("renamed to"),
        "the envelope carries the code, not the prose: {stderr}"
    );
}

/// `merge.renames=false` turns detection off: the rename becomes a delete
/// plus an add again, so the other side's edit conflicts as a modify/delete.
#[test]
fn merge_renames_config_false_disables_detection() {
    let their = "line1\nline2 edited\nline3\nline4\nline5\nline6\nline7\nline8\n";
    let repo = create_rename_repo(None, their);
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "merge.renames", "false"], p),
        "merge.renames=false",
    );
    let output = run_libra_command(&["merge", "feature"], p);
    assert_eq!(output.status.code(), Some(128), "modify/delete conflict");
    assert!(
        !index_stage_lines(p, "old.txt").is_empty(),
        "the conflict stays at the original path"
    );
}

/// A rename whose destination lands inside a subtree the pruned walk adopts
/// wholesale is still detected: the candidates come from the diffs the walk
/// already runs for `files_changed`, so both walks agree.
#[test]
fn merge_rename_into_an_adopted_subtree_is_detected_on_both_walks() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        let base = "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\n";
        commit_file(p, "old.txt", base, "base tracks old.txt");
        commit_file(
            p,
            "shared/keep.txt",
            "keep\n",
            "a subtree ours never touches",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        // Ours only edits the file, so `shared/` stays identical to the base
        // and theirs' version of it is adopted verbatim by the pruned walk.
        commit_file(
            p,
            "old.txt",
            "alpha\nbeta edited\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\n",
            "ours edits old.txt",
        );
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::create_dir_all(p.join("shared")).expect("dir");
        std::fs::rename(p.join("old.txt"), p.join("shared/moved.txt")).expect("rename");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &[
                    "commit",
                    "-m",
                    "theirs moves it into shared/",
                    "--no-verify",
                ],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        assert_eq!(
            std::fs::read_to_string(p.join("shared/moved.txt")).expect("moved file"),
            "alpha\nbeta edited\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\n",
            "walk {env:?}: ours' edit followed the rename into the adopted subtree"
        );
        assert!(!p.join("old.txt").exists(), "walk {env:?}");
    }
}

/// G6 at the production entry: past `merge.renameLimit` the inexact stage is
/// skipped, the merge still completes, the notice says so in human output —
/// and `--json` stays machine-clean.
#[test]
fn merge_rename_limit_reports_a_notice_and_still_merges() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // One exact rename plus two inexact candidates per side, so that after the
    // exact stage each side is still over a limit of 1.
    commit_file(p, "exact.txt", "exactly the same\n", "base: exact source");
    for index in 0..2 {
        commit_file(
            p,
            &format!("similar-{index}.txt"),
            &format!("aaaa\nbbbb\ncccc\ndddd{index}\n"),
            "base: inexact source",
        );
    }
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    std::fs::rename(p.join("exact.txt"), p.join("exact-moved.txt")).expect("rename");
    for index in 0..2 {
        std::fs::rename(
            p.join(format!("similar-{index}.txt")),
            p.join(format!("similar-moved-{index}.txt")),
        )
        .expect("rename");
        std::fs::write(
            p.join(format!("similar-moved-{index}.txt")),
            format!("aaaa\nbbbb\ncccc\nchanged{index}\n"),
        )
        .expect("edit");
    }
    assert_cli_success(
        &run_libra_command(&["add", "-A", "."], p),
        "stage the renames",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "ours renames three files", "--no-verify"],
            p,
        ),
        "ours",
    );
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    // Theirs EDITS the exactly-renamed file (Codex R14 P2): if the exact pair
    // were dropped along with the inexact stage, this would be a modify/delete
    // conflict rather than a clean merge, so the assertions below actually
    // establish that the exact rename survived the limit.
    commit_file(
        p,
        "exact.txt",
        "exactly the same, edited\n",
        "theirs edits the exactly-renamed file",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    assert_cli_success(
        &run_libra_command(&["config", "merge.renameLimit", "1"], p),
        "merge.renameLimit=1",
    );

    let out = run_libra_command(&["merge", "feature"], p);
    assert_cli_success(&out, "the merge completes despite the limit");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("skipped inexact rename detection") && stdout.contains("merge.renameLimit"),
        "the degradation is reported: {stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("exact-moved.txt")).expect("the exactly-renamed file"),
        "exactly the same, edited\n",
        "the exact pair survived the limit and the edit followed it: {stdout}"
    );
    assert!(
        index_stage_lines(p, "exact.txt").is_empty(),
        "nothing is left at the old path"
    );

    // The same shape under `--json`: nothing but the envelope on stdout.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "exact.txt", "exactly the same\n", "base");
    for index in 0..2 {
        commit_file(
            p,
            &format!("similar-{index}.txt"),
            &format!("aaaa\nbbbb\ncccc\ndddd{index}\n"),
            "base",
        );
    }
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    std::fs::rename(p.join("exact.txt"), p.join("exact-moved.txt")).expect("rename");
    for index in 0..2 {
        std::fs::rename(
            p.join(format!("similar-{index}.txt")),
            p.join(format!("similar-moved-{index}.txt")),
        )
        .expect("rename");
    }
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ours renames", "--no-verify"], p),
        "ours",
    );
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    commit_file(p, "unrelated.txt", "theirs\n", "theirs");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    assert_cli_success(
        &run_libra_command(&["config", "merge.renameLimit", "1"], p),
        "merge.renameLimit=1",
    );
    let out = run_libra_command(&["--json", "merge", "feature"], p);
    assert_cli_success(&out, "merge");
    let json = parse_json_stdout(&out);
    assert_eq!(
        json["ok"], true,
        "stdout is exactly the JSON envelope: {json}"
    );
}

/// A config value Git rejects is an error here too, before anything is merged.
#[test]
fn merge_rejects_an_invalid_rename_config() {
    let repo = create_rename_repo(
        None,
        "line1\nline2 edited\nline3\nline4\nline5\nline6\nline7\nline8\n",
    );
    let p = repo.path();
    let head_before = head_commit(p);
    assert_cli_success(
        &run_libra_command(&["config", "merge.renames", "wat"], p),
        "merge.renames=wat",
    );
    let output = run_libra_command(&["merge", "feature"], p);
    assert!(
        !output.status.success(),
        "an unparseable value fails closed"
    );
    let (stderr, _report) = parse_cli_error_stderr(&output.stderr);
    assert!(stderr.contains("merge.renames"), "{stderr}");
    assert_eq!(head_commit(p), head_before);

    assert_cli_success(
        &run_libra_command(&["config", "merge.renames", "true"], p),
        "merge.renames=true",
    );
    assert_cli_success(
        &run_libra_command(&["config", "merge.renameLimit", "not-a-number"], p),
        "merge.renameLimit=not-a-number",
    );
    let output = run_libra_command(&["merge", "feature"], p);
    assert!(
        !output.status.success(),
        "a non-integer limit fails closed, as Git's `bad numeric config value` does"
    );
    let (stderr, _report) = parse_cli_error_stderr(&output.stderr);
    assert!(stderr.contains("merge.renameLimit"), "{stderr}");
    assert_eq!(head_commit(p), head_before);

    // Git starts `merge.renameLimit` at -1 and maps every value <= 0 onto its
    // 7000 default (merge-ort.c:3452-3453, :5504), so a negative value is not
    // an error — measured: `git -c merge.renameLimit=0` still detects the
    // renames a 30-path merge needs.
    assert_cli_success(
        &run_libra_command(&["config", "merge.renameLimit", "--", "-3"], p),
        "merge.renameLimit=-3",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], p),
        "a negative limit means Git's default, not a refusal",
    );
}

/// The two walks report the SAME `files_changed` for a rename, in both
/// directions (Codex MG-05 R1: the pruned walk used to add one unconditionally).
#[test]
fn merge_rename_files_changed_agrees_across_walks_in_both_directions() {
    let their_edit = "line1\nline2 edited\nline3\nline4\nline5\nline6\nline7\nline8\n";
    for ours_renames in [true, false] {
        let mut summaries = Vec::new();
        for env in [
            &[][..],
            &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
        ] {
            let repo = if ours_renames {
                create_rename_repo(None, their_edit)
            } else {
                // Mirror image: theirs renames, ours edits in place.
                let repo = create_committed_repo_via_cli();
                let p = repo.path();
                let base = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\n";
                commit_file(p, "old.txt", base, "base");
                assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
                commit_file(p, "old.txt", their_edit, "ours edits in place");
                assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
                std::fs::rename(p.join("old.txt"), p.join("new.txt")).expect("rename");
                assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
                assert_cli_success(
                    &run_libra_command(&["commit", "-m", "theirs renames", "--no-verify"], p),
                    "theirs",
                );
                assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
                repo
            };
            let p = repo.path();
            let out = run_libra_command_with_stdin_and_env(
                &["--json", "merge", "--dry-run", "feature"],
                p,
                "",
                env,
            );
            assert_cli_success(&out, "dry-run");
            let mut data = parse_json_stdout(&out)["data"].clone();
            // Each walk runs in its own fixture, so the commit ids differ by
            // construction; everything the merge DECIDED must not.
            if let Some(object) = data.as_object_mut() {
                object.remove("old_commit");
                object.remove("commit");
            }
            summaries.push(data);
        }
        assert_eq!(
            summaries[0], summaries[1],
            "ours_renames={ours_renames}: both walks preview the same summary"
        );
        assert_eq!(
            summaries[0]["files_changed"], 1,
            "ours_renames={ours_renames}: a rename is one changed path, as Git's diffstat renders it"
        );
    }
}

/// Codex MG-05 R2, verified against `git merge`: a rename plus a mode change
/// on one side and a content edit on the other merges cleanly and keeps
/// `100755` — Git resolves content and mode independently.
#[cfg(unix)]
#[test]
fn merge_rename_with_mode_change_merges_cleanly_and_keeps_the_mode() {
    use std::os::unix::fs::PermissionsExt;
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "old.txt", "a\nb\nc\nd\ne\nf\ng\nh\n", "base");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        std::fs::rename(p.join("old.txt"), p.join("new.txt")).expect("rename");
        std::fs::set_permissions(p.join("new.txt"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "ours renames and chmods", "--no-verify"],
                p,
            ),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        commit_file(
            p,
            "old.txt",
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "theirs edits",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        assert_eq!(
            std::fs::read_to_string(p.join("new.txt")).expect("merged file"),
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "walk {env:?}: the edit followed the rename"
        );
        let stages = index_stage_lines(p, "new.txt");
        assert!(
            stages.iter().any(|line| line.starts_with("100755")),
            "walk {env:?}: the executable bit survives: {stages:?}"
        );
    }
}

/// Codex MG-05 R2, verified against `git merge`: when the rename's
/// destination is a DIRECTORY on the other side, the rename is not used —
/// the file/directory collision takes over and the file moves to `new~HEAD`.
#[test]
fn merge_rename_into_a_directory_falls_back_to_the_df_collision() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        commit_file(p, "old.txt", "a\nb\nc\nd\ne\nf\ng\nh\n", "base");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        std::fs::rename(p.join("old.txt"), p.join("new")).expect("rename to `new`");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "ours renames old.txt to new", "--no-verify"],
                p,
            ),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::create_dir_all(p.join("new")).expect("dir");
        std::fs::write(p.join("new/child.txt"), "child\n").expect("child");
        assert_cli_success(&run_libra_command(&["add", "new/child.txt"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "theirs adds new/child.txt", "--no-verify"],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        assert!(
            stdout.contains(
                "CONFLICT (file/directory): directory in the way of new from HEAD; moving it to new~HEAD instead."
            ),
            "walk {env:?}: {stdout}"
        );
        assert!(p.join("new").is_dir(), "walk {env:?}");
        let stages = index_stage_lines(p, "new~HEAD");
        assert_eq!(stages.len(), 1, "walk {env:?}: {stages:?}");
        assert!(stages[0].contains(" 2\t"), "walk {env:?}: {stages:?}");
    }
}

/// Codex MG-05 R6 claimed that a rename whose destination nests *below* its own
/// former path must raise Git's file/directory conflict and leave the other
/// side's edit at `old~HEAD`. Measured against upstream Git — both
/// `git merge-tree --write-tree --messages` on plumbing-built commits and a real
/// `git merge` in a worktree — that shape merges cleanly, using the rename and
/// carrying the edit to `old/new`. The finding was recorded as not reproducible;
/// this test pins the measured parity on both walks.
#[test]
fn merge_rename_into_a_subdirectory_of_its_own_path_merges_cleanly() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        commit_file(p, "old", "a\nb\nc\nd\ne\nf\ng\nh\n", "base");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        assert_cli_success(&run_libra_command(&["rm", "old"], p), "drop the file");
        std::fs::create_dir_all(p.join("old")).expect("dir");
        std::fs::write(p.join("old/new"), "a\nb\nc\nd\ne\nf\ng\nh\n").expect("write");
        assert_cli_success(&run_libra_command(&["add", "old/new"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "ours renames old to old/new", "--no-verify"],
                p,
            ),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        commit_file(
            p,
            "old",
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "theirs edits old",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        assert_eq!(
            std::fs::read_to_string(p.join("old/new")).expect("merged file"),
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "walk {env:?}: the edit followed the rename"
        );
        assert!(
            p.join("old").is_dir(),
            "walk {env:?}: the destination directory stands"
        );
        let stages = index_stage_lines(p, "old/new");
        assert_eq!(stages.len(), 1, "walk {env:?}: {stages:?}");
        assert!(stages[0].contains(" 0\t"), "walk {env:?}: {stages:?}");
        assert!(
            index_stage_lines(p, "old").is_empty(),
            "walk {env:?}: nothing is left at the old path"
        );
    }
}

/// Codex MG-05 R2: a rename whose destination the other side ALSO added (an
/// add/add conflict there) must not be used — the other side's content must
/// not vanish. Both walks.
#[test]
fn merge_rename_onto_a_path_the_other_side_added_keeps_both_sides() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "old.txt", "a\nb\nc\nd\ne\nf\ng\nh\n", "base");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        std::fs::rename(p.join("old.txt"), p.join("new.txt")).expect("rename");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ours renames", "--no-verify"], p),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        commit_file(p, "new.txt", "theirs own file\n", "theirs adds new.txt");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        // MG-06: Git prints NO rename line here — `merge-ort.c` has no
        // `CONFLICT (rename/add)` at all, and the collision branch only speaks
        // up when the rename's OWN merge was unclean (it is clean here: only
        // ours touched the file). Measured on git 2.50.1
        // (`/Volumes/Data/tmp/mg06-git/radd`): the sole message is
        // `CONFLICT (add/add): Merge conflict in new`.
        assert!(
            !stdout.contains("rename/") && !stdout.contains("notice:"),
            "walk {env:?}: no rename line is printed for a clean collision: {stdout}"
        );
        let markers = std::fs::read_to_string(p.join("new.txt")).expect("conflicted file");
        assert!(
            markers.contains("theirs own file"),
            "walk {env:?}: theirs' content is not discarded: {markers}"
        );
        let stages = index_stage_lines(p, "new.txt");
        assert!(
            stages.iter().any(|line| line.contains(" 2\t"))
                && stages.iter().any(|line| line.contains(" 3\t")),
            "walk {env:?}: an add/add conflict keeps both sides: {stages:?}"
        );
        // Git records NO merge base at the destination for a collision
        // (`merge-ort.c:3137-3179` never copies `base->stages[0]` there), so
        // the conflict is a genuine add/add. Measured: stages 2 and 3 only.
        assert!(
            !stages.iter().any(|line| line.contains(" 1\t")),
            "walk {env:?}: a collision records no base stage: {stages:?}"
        );
    }
}

/// Codex MG-05 R2: a rename INTO a subtree the pruned walk carries whole must
/// not leave that subtree's tree entry beside the new leaf — the written tree
/// has exactly one entry per name.
#[test]
fn merge_rename_into_a_carried_subtree_writes_one_tree_entry() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "old.txt", "a\nb\nc\nd\ne\nf\ng\nh\n", "base file");
        commit_file(
            p,
            "shared/keep.txt",
            "keep\n",
            "a subtree ours never touches",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(
            p,
            "old.txt",
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "ours edits the file",
        );
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::rename(p.join("old.txt"), p.join("shared/moved.txt")).expect("rename");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &[
                    "commit",
                    "-m",
                    "theirs moves it into shared/",
                    "--no-verify",
                ],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        assert_cli_success(
            &run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env),
            "clean merge",
        );
        let listing = run_libra_command(&["ls-tree", "HEAD"], p);
        assert_cli_success(&listing, "ls-tree");
        let listing = String::from_utf8_lossy(&listing.stdout).to_string();
        let shared_rows = listing
            .lines()
            .filter(|line| line.ends_with("\tshared"))
            .count();
        assert_eq!(
            shared_rows, 1,
            "walk {env:?}: one entry per name: {listing}"
        );
        let recursive = run_libra_command(&["ls-tree", "-r", "HEAD"], p);
        assert_cli_success(&recursive, "ls-tree -r");
        let recursive = String::from_utf8_lossy(&recursive.stdout).to_string();
        assert!(
            recursive.contains("\tshared/keep.txt") && recursive.contains("\tshared/moved.txt"),
            "walk {env:?}: {recursive}"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("shared/moved.txt")).expect("moved file"),
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "walk {env:?}: ours' edit followed the rename"
        );
    }
}

/// Codex MG-05 R3, verified against `git merge` on a crafted tree: an EMPTY
/// directory at the rename's destination is not in the way — Git uses the
/// rename and lands the other side's edit at the new path. Both walks.
#[test]
fn merge_rename_destination_with_an_empty_directory_still_uses_the_rename() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        commit_file(p, "old.txt", "a\nb\nc\nd\ne\nf\ng\nh\n", "base");
        let base = head_commit(p);
        // Theirs edits `old.txt` AND adds an empty tree at `new` — a shape only
        // the plumbing can build.
        let edited = {
            let out = run_libra_command_with_stdin(
                &["hash-object", "-w", "--stdin"],
                p,
                "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            );
            assert_cli_success(&out, "hash-object");
            stdout_trimmed(&out)
        };
        let theirs = {
            assert_cli_success(
                &run_libra_command(
                    &[
                        "update-index",
                        "--cacheinfo",
                        &format!("100644,{edited},old.txt"),
                    ],
                    p,
                ),
                "stage theirs' edit",
            );
            let tree = run_libra_command(&["write-tree"], p);
            assert_cli_success(&tree, "write-tree");
            let tree = stdout_trimmed(&tree);
            // Splice an empty `new` tree into that root.
            let empty = {
                std::fs::write(p.join(".empty"), b"").expect("empty");
                let out = run_libra_command(
                    &["hash-object", "-t", "tree", "-w", "--literally", ".empty"],
                    p,
                );
                assert_cli_success(&out, "empty tree");
                std::fs::remove_file(p.join(".empty")).expect("cleanup");
                stdout_trimmed(&out)
            };
            let listing = run_libra_command(&["ls-tree", &tree], p);
            assert_cli_success(&listing, "ls-tree");
            let mut bytes: Vec<(String, Vec<u8>)> = Vec::new();
            for line in String::from_utf8_lossy(&listing.stdout).lines() {
                let (meta, name) = line.split_once('\t').expect("ls-tree line");
                let mut parts = meta.split_whitespace();
                let mode = parts.next().expect("mode");
                let _kind = parts.next();
                let id = parts.next().expect("id");
                let mut entry = format!("{} {name}\0", mode.trim_start_matches('0')).into_bytes();
                entry.extend(
                    (0..id.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&id[i..i + 2], 16).expect("hex")),
                );
                bytes.push((name.to_string(), entry));
            }
            let mut new_entry = b"40000 new\0".to_vec();
            new_entry.extend(
                (0..empty.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&empty[i..i + 2], 16).expect("hex")),
            );
            bytes.push(("new/".to_string(), new_entry));
            bytes.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            let root: Vec<u8> = bytes.into_iter().flat_map(|(_, entry)| entry).collect();
            std::fs::write(p.join(".root"), &root).expect("root bytes");
            let out = run_libra_command(
                &["hash-object", "-t", "tree", "-w", "--literally", ".root"],
                p,
            );
            assert_cli_success(&out, "root tree");
            std::fs::remove_file(p.join(".root")).expect("cleanup");
            let root = stdout_trimmed(&out);
            let out = run_libra_command(
                &[
                    "commit-tree",
                    &root,
                    "-p",
                    &base,
                    "-m",
                    "theirs edits and adds an empty new/",
                ],
                p,
            );
            assert_cli_success(&out, "commit-tree");
            stdout_trimmed(&out)
        };
        assert_cli_success(
            &run_libra_command(&["update-ref", "refs/heads/feature", &theirs], p),
            "refs/heads/feature",
        );
        // Put the index back and make ours' rename.
        assert_cli_success(&run_libra_command(&["reset", "--hard", &base], p), "reset");
        std::fs::rename(p.join("old.txt"), p.join("new")).expect("rename");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "ours renames old.txt to new", "--no-verify"],
                p,
            ),
            "ours",
        );

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge — an empty directory is not in the way");
        assert_eq!(
            std::fs::read_to_string(p.join("new")).expect("merged file"),
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "walk {env:?}: the rename was used and the edit followed it"
        );
        assert!(index_stage_lines(p, "old.txt").is_empty(), "walk {env:?}");
    }
}

/// Codex MG-05 R4, verified with `git merge-tree`: a rename whose destination
/// REPLACES an empty-tree marker inside a subtree is still detected — Git
/// merges cleanly and the other side's edit follows the file to its new path.
/// Both walks.
#[test]
fn merge_rename_onto_an_empty_marker_inside_a_subtree_is_detected() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        commit_file(p, "old.txt", "a\nb\nc\nd\ne\nf\ng\nh\n", "base file");
        // The base also holds `shared/sub` as an EMPTY tree — plumbing only.
        let base_parent = head_commit(p);
        let raw = |hex: &str| -> Vec<u8> {
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
                .collect()
        };
        let hash_tree = |bytes: &[u8], name: &str| -> String {
            std::fs::write(p.join(name), bytes).expect("tree bytes");
            let out =
                run_libra_command(&["hash-object", "-t", "tree", "-w", "--literally", name], p);
            assert_cli_success(&out, "hash-object tree");
            std::fs::remove_file(p.join(name)).expect("cleanup");
            stdout_trimmed(&out)
        };
        let empty = hash_tree(b"", ".empty");
        let mut shared_empty = b"40000 sub\0".to_vec();
        shared_empty.extend(raw(&empty));
        let shared_empty = hash_tree(&shared_empty, ".shared-empty");
        let listing = run_libra_command(&["ls-tree", &base_parent], p);
        assert_cli_success(&listing, "ls-tree");
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        let mut old_blob = String::new();
        for line in String::from_utf8_lossy(&listing.stdout).lines() {
            let (meta, name) = line.split_once('\t').expect("ls-tree line");
            let mut parts = meta.split_whitespace();
            let mode = parts.next().expect("mode");
            let _kind = parts.next();
            let id = parts.next().expect("id");
            if name == "old.txt" {
                old_blob = id.to_string();
            }
            let mut entry = format!("{} {name}\0", mode.trim_start_matches('0')).into_bytes();
            entry.extend(raw(id));
            entries.push((name.to_string(), entry));
        }
        let with_shared = |entries: &[(String, Vec<u8>)], shared: &str, drop_old: bool| -> String {
            let mut all: Vec<(String, Vec<u8>)> = entries
                .iter()
                .filter(|(name, _)| !(drop_old && name == "old.txt"))
                .cloned()
                .collect();
            let mut shared_entry = b"40000 shared\0".to_vec();
            shared_entry.extend(raw(shared));
            all.push(("shared/".to_string(), shared_entry));
            all.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            let bytes: Vec<u8> = all.into_iter().flat_map(|(_, entry)| entry).collect();
            hash_tree(&bytes, ".root")
        };
        let base_tree = with_shared(&entries, &shared_empty, false);
        let base = {
            let out = run_libra_command(
                &[
                    "commit-tree",
                    &base_tree,
                    "-m",
                    "base with empty shared/sub",
                ],
                p,
            );
            assert_cli_success(&out, "commit-tree");
            stdout_trimmed(&out)
        };
        // Theirs: `old.txt` becomes the FILE `shared/sub`.
        let mut shared_file = b"100644 sub\0".to_vec();
        shared_file.extend(raw(&old_blob));
        let shared_file = hash_tree(&shared_file, ".shared-file");
        let theirs_tree = with_shared(&entries, &shared_file, true);
        let theirs = {
            let out = run_libra_command(
                &[
                    "commit-tree",
                    &theirs_tree,
                    "-p",
                    &base,
                    "-m",
                    "theirs moves old.txt to shared/sub",
                ],
                p,
            );
            assert_cli_success(&out, "commit-tree");
            stdout_trimmed(&out)
        };
        for (branch, commit) in [("main", &base), ("feature", &theirs)] {
            assert_cli_success(
                &run_libra_command(&["update-ref", &format!("refs/heads/{branch}"), commit], p),
                "update-ref",
            );
        }
        assert_cli_success(&run_libra_command(&["reset", "--hard", &base], p), "reset");
        // Ours edits the file in place.
        commit_file(
            p,
            "old.txt",
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "ours edits old.txt",
        );

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        assert_eq!(
            std::fs::read_to_string(p.join("shared/sub")).expect("moved file"),
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "walk {env:?}: the edit followed the rename onto the empty marker"
        );
        assert!(!p.join("old.txt").exists(), "walk {env:?}");
    }
}

/// Codex MG-05 R5: a rename whose destination has an UNMERGED path beneath it
/// must be declined — the conflicted file has to survive. Base has `new/child`
/// and `old.txt`; ours deletes the child and renames `old.txt` to `new`;
/// theirs edits the child. Both walks.
#[test]
fn merge_rename_onto_a_path_with_a_conflict_beneath_is_declined() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        commit_file(p, "old.txt", "a\nb\nc\nd\ne\nf\ng\nh\n", "base file");
        commit_file(p, "new/child.txt", "child base\n", "base directory");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        // Ours: delete the child, then rename the file onto `new`.
        assert_cli_success(
            &run_libra_command(&["rm", "new/child.txt"], p),
            "drop the child",
        );
        std::fs::rename(p.join("old.txt"), p.join("new")).expect("rename onto new");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ours renames onto new", "--no-verify"], p),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        commit_file(
            p,
            "new/child.txt",
            "child edited\n",
            "theirs edits the child",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        assert!(
            stdout.contains("CONFLICT (file/directory)"),
            "walk {env:?}: the D/F pass takes over: {stdout}"
        );
        // The conflicted child must still be there — the rename must not have
        // been applied over it.
        assert!(
            p.join("new/child.txt").exists() || !index_stage_lines(p, "new/child.txt").is_empty(),
            "walk {env:?}: the unmerged child survives"
        );
        assert!(
            p.join("new~HEAD").exists(),
            "walk {env:?}: ours' file moved out of the directory's way"
        );
    }
}

/// Codex MG-05 R5: an unparseable rename config is rejected BEFORE a
/// criss-cross merge folds its virtual ancestor, so the refused command
/// leaves no objects behind.
#[test]
fn merge_rejects_rename_config_before_folding_a_virtual_ancestor() {
    let repo = create_crisscross_repo();
    let p = repo.path();
    let head_before = head_commit(p);
    assert_cli_success(
        &run_libra_command(&["config", "merge.renames", "wat"], p),
        "merge.renames=wat",
    );
    let before = loose_object_ids(p);
    let output = run_libra_command(&["merge", "y"], p);
    assert!(!output.status.success(), "the merge is refused");
    let (stderr, _report) = parse_cli_error_stderr(&output.stderr);
    assert!(stderr.contains("merge.renames"), "{stderr}");
    assert_eq!(head_commit(p), head_before);
    assert_eq!(
        loose_object_ids(p),
        before,
        "no virtual-ancestor objects were written before the refusal"
    );
}

/// Codex R7 P1: the strict rename config must be refused before `--autostash`
/// writes a stash commit, a durable sidecar and resets the worktree — the
/// mutation Codex R5's virtual-ancestor fix did not cover. The refusal leaves
/// the object store, the gitdir and the dirty worktree exactly as they were.
#[test]
fn merge_rejects_rename_config_before_autostash_touches_the_repository() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "f.txt", "a\nb\nc\n", "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    commit_file(p, "f.txt", "a\nb\nc\nours\n", "ours");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    commit_file(p, "f.txt", "theirs\na\nb\nc\n", "theirs");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    assert_cli_success(
        &run_libra_command(&["config", "merge.renames", "not-a-bool"], p),
        "merge.renames=not-a-bool",
    );
    std::fs::write(p.join("f.txt"), "a\nb\nc\nours\ndirty\n").expect("dirty the tree");

    let head_before = head_commit(p);
    let before = loose_object_ids(p);
    let output = run_libra_command(&["merge", "--autostash", "feature"], p);
    assert!(!output.status.success(), "the merge is refused");
    let (stderr, _report) = parse_cli_error_stderr(&output.stderr);
    assert!(stderr.contains("merge.renames"), "{stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        !stdout.contains("Created autostash") && !stderr.contains("Created autostash"),
        "no autostash was created: {stdout} / {stderr}"
    );
    assert_eq!(head_commit(p), head_before);
    assert_eq!(
        loose_object_ids(p),
        before,
        "no autostash objects were written before the refusal"
    );
    assert!(
        !p.join(".libra/merge-autostash.json").exists(),
        "no held-autostash sidecar was left behind"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("f.txt")).expect("worktree"),
        "a\nb\nc\nours\ndirty\n",
        "the dirty worktree was not reset"
    );
}

/// Codex R8 P1: the refusal must also precede the STALE-sidecar recovery,
/// which promotes a leftover autostash into the shared stash list and deletes
/// its sidecar — a mutation that ran before the config was validated.
#[test]
fn merge_rejects_rename_config_before_recovering_a_stale_autostash() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "f.txt", "a\nb\nc\n", "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    commit_file(p, "f.txt", "a\nOURS\nc\n", "ours");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    commit_file(p, "f.txt", "a\nTHEIRS\nc\n", "theirs");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

    // A leftover held-autostash sidecar with no merge in progress: exactly what
    // the recovery promotes. Any commit object serves as the held commit; the
    // point is that nothing may touch it while the config is unusable.
    let held = head_commit(p);
    let sidecar = p.join(".libra/merge-autostash.json");
    std::fs::write(
        &sidecar,
        format!("{{\n  \"owner_scope\": \"\",\n  \"stash_commit\": \"{held}\"\n}}\n"),
    )
    .expect("write the stale sidecar");
    let before = std::fs::read_to_string(&sidecar).expect("sidecar");
    assert_cli_success(
        &run_libra_command(&["config", "merge.renames", "not-a-bool"], p),
        "merge.renames=not-a-bool",
    );

    let output = run_libra_command(&["merge", "feature"], p);
    assert!(!output.status.success(), "the merge is refused");
    let (stderr, _report) = parse_cli_error_stderr(&output.stderr);
    assert!(stderr.contains("merge.renames"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(&sidecar).expect("sidecar"),
        before,
        "the stale sidecar was neither promoted nor deleted"
    );
    let stash = run_libra_command(&["stash", "list"], p);
    assert!(
        String::from_utf8_lossy(&stash.stdout).trim().is_empty(),
        "nothing was promoted into the stash list: {:?}",
        String::from_utf8_lossy(&stash.stdout)
    );
}

/// Codex R8 P1: `--squash` and `--no-commit` over a FAST-FORWARDABLE history
/// only reach the three-way engine because they skip the fast-forward branch;
/// Git takes its own fast-forward path there and never parses the rename
/// config. Measured on git 2.50.1 with `merge.renames=not-a-bool`:
/// `git merge --squash` and `git merge --no-commit` print "Updating ..
/// Fast-forward" and succeed, while `git merge --no-ff` on the same history
/// fails with `fatal: bad boolean config value`.
#[test]
fn merge_squash_over_a_fast_forwardable_history_ignores_the_rename_config() {
    for flag in ["--squash", "--no-commit"] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "f.txt", "a\n", "base");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        commit_file(p, "f.txt", "a\nb\n", "ahead");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        assert_cli_success(
            &run_libra_command(&["config", "merge.renames", "not-a-bool"], p),
            "merge.renames=not-a-bool",
        );
        assert_cli_success(
            &run_libra_command(&["merge", flag, "feature"], p),
            &format!("{flag} over a fast-forwardable history is not refused"),
        );
    }

    // `--no-ff` on the same history IS a real merge, and Git fails there.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "f.txt", "a\n", "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    commit_file(p, "f.txt", "a\nb\n", "ahead");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    assert_cli_success(
        &run_libra_command(&["config", "merge.renames", "not-a-bool"], p),
        "merge.renames=not-a-bool",
    );
    let output = run_libra_command(&["merge", "--no-ff", "-m", "m", "feature"], p);
    assert!(
        !output.status.success(),
        "--no-ff is a real merge and is refused, as Git refuses it"
    );
    let (stderr, _report) = parse_cli_error_stderr(&output.stderr);
    assert!(stderr.contains("merge.renames"), "{stderr}");
}

/// The same strict config must NOT be read where Git never parses it: measured
/// on git 2.50.1 with `merge.renames = not-a-bool`, a fast-forward merge
/// succeeds (as does an already-up-to-date one, and `-s ours`), because only a
/// real three-way merge reaches the rename machinery.
#[test]
fn merge_ignores_an_invalid_rename_config_on_a_fast_forward() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "f.txt", "a\nb\nc\n", "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    commit_file(p, "t.txt", "t\n", "theirs");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    assert_cli_success(
        &run_libra_command(&["config", "merge.renames", "not-a-bool"], p),
        "merge.renames=not-a-bool",
    );

    assert_cli_success(
        &run_libra_command(&["merge", "feature"], p),
        "the fast-forward is not refused",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], p),
        "already up to date is not refused",
    );
}

/// Codex R15: an empty tree beneath the destination is in the way only where
/// MG-04's base-presence rule puts it there. When the merge base HAD something
/// at that path, Git traverses the directory, finds no file, and merges
/// cleanly. Measured on git 2.50.1 with base holding `old` and `new/gone`, ours
/// deleting `new/gone` and renaming `old` to `new`, and theirs editing `old`
/// and replacing `new/gone` with an empty `new/sub`: `git merge-tree
/// --write-tree --messages` exits 0 with no messages. Contrast
/// `merge_rename_onto_a_nested_empty_directory_is_declined`, where the base had
/// nothing at `new` and Git raises the collision.
#[test]
fn merge_rename_onto_an_emptied_directory_the_base_had_is_used() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        commit_file(p, "old", "a\nb\nc\nd\ne\nf\ng\nh\n", "base file");
        std::fs::create_dir_all(p.join("new")).expect("dir");
        std::fs::write(p.join("new/gone"), "gone\n").expect("gone");
        assert_cli_success(&run_libra_command(&["add", "new/gone"], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base adds new/gone", "--no-verify"], p),
            "base",
        );
        let base = head_commit(p);
        let hex_bytes = |id: &str| -> Vec<u8> {
            (0..id.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&id[i..i + 2], 16).expect("hex"))
                .collect()
        };
        let write_tree_bytes = |bytes: &[u8], label: &str| -> String {
            std::fs::write(p.join(".tree"), bytes).expect("tree bytes");
            let out = run_libra_command(
                &["hash-object", "-t", "tree", "-w", "--literally", ".tree"],
                p,
            );
            assert_cli_success(&out, label);
            std::fs::remove_file(p.join(".tree")).expect("cleanup");
            stdout_trimmed(&out)
        };
        let edited = {
            let out = run_libra_command_with_stdin(
                &["hash-object", "-w", "--stdin"],
                p,
                "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            );
            assert_cli_success(&out, "hash-object");
            stdout_trimmed(&out)
        };
        // Theirs: `old` edited, and `new` now holds ONLY an empty `sub` tree.
        let empty = write_tree_bytes(&[], "empty tree");
        let mut sub = b"40000 sub\0".to_vec();
        sub.extend(hex_bytes(&empty));
        let nested = write_tree_bytes(&sub, "new/ tree");
        let other_id = {
            let out = run_libra_command(&["rev-parse", &format!("{base}:other.txt")], p);
            assert_cli_success(&out, "other.txt id");
            stdout_trimmed(&out)
        };
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        let push = |name: &str, mode: &str, id: &str, entries: &mut Vec<(String, Vec<u8>)>| {
            let mut entry = format!("{mode} {name}\0").into_bytes();
            entry.extend(hex_bytes(id));
            let key = if mode == "40000" {
                format!("{name}/")
            } else {
                name.to_string()
            };
            entries.push((key, entry));
        };
        push("new", "40000", &nested, &mut entries);
        push("old", "100644", &edited, &mut entries);
        push("other.txt", "100644", &other_id, &mut entries);
        entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let root: Vec<u8> = entries.into_iter().flat_map(|(_, entry)| entry).collect();
        let root = write_tree_bytes(&root, "root tree");
        let theirs = {
            let out = run_libra_command(
                &[
                    "commit-tree",
                    &root,
                    "-p",
                    &base,
                    "-m",
                    "theirs edits old and empties new/",
                ],
                p,
            );
            assert_cli_success(&out, "commit-tree");
            stdout_trimmed(&out)
        };
        assert_cli_success(
            &run_libra_command(&["update-ref", "refs/heads/feature", &theirs], p),
            "refs/heads/feature",
        );
        assert_cli_success(&run_libra_command(&["reset", "--hard", &base], p), "reset");
        assert_cli_success(&run_libra_command(&["rm", "new/gone"], p), "drop new/gone");
        std::fs::rename(p.join("old"), p.join("new")).expect("rename");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &[
                    "commit",
                    "-m",
                    "ours deletes new/gone and renames old to new",
                    "--no-verify",
                ],
                p,
            ),
            "ours",
        );

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge — the base had that directory");
        assert_eq!(
            std::fs::read_to_string(p.join("new")).expect("merged file"),
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "walk {env:?}: the rename was used and the edit followed it"
        );
        assert!(index_stage_lines(p, "old").is_empty(), "walk {env:?}");
    }
}

/// Codex R16/R17: after the rename fix-up resolves a destination, a collision
/// candidate the WALK recorded for that same path is stale — settling it puts
/// the pre-rename blob back and drops the resolved content from every stage.
/// Base holds files `old` and `d`; ours replaces both with `d/new` (the rename);
/// theirs edits `old` and replaces `d` with an empty `d/new/sub`. Git merges
/// cleanly with the edit at `d/new`.
#[test]
fn merge_rename_destination_is_not_re_settled_as_a_collision() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        commit_file(p, "old", "a\nb\nc\nd\ne\nf\ng\nh\n", "base old");
        commit_file(p, "d", "a file at d\n", "base d");
        let base = head_commit(p);
        let hex_bytes = |id: &str| -> Vec<u8> {
            (0..id.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&id[i..i + 2], 16).expect("hex"))
                .collect()
        };
        let write_tree_bytes = |bytes: &[u8], label: &str| -> String {
            std::fs::write(p.join(".tree"), bytes).expect("tree bytes");
            let out = run_libra_command(
                &["hash-object", "-t", "tree", "-w", "--literally", ".tree"],
                p,
            );
            assert_cli_success(&out, label);
            std::fs::remove_file(p.join(".tree")).expect("cleanup");
            stdout_trimmed(&out)
        };
        let edited = {
            let out = run_libra_command_with_stdin(
                &["hash-object", "-w", "--stdin"],
                p,
                "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            );
            assert_cli_success(&out, "hash-object");
            stdout_trimmed(&out)
        };
        // theirs: `d` becomes a directory holding only an empty `new/sub`.
        let empty = write_tree_bytes(&[], "empty tree");
        let mut sub = b"40000 sub\0".to_vec();
        sub.extend(hex_bytes(&empty));
        let new_tree = write_tree_bytes(&sub, "d/new tree");
        let mut new_entry = b"40000 new\0".to_vec();
        new_entry.extend(hex_bytes(&new_tree));
        let d_tree = write_tree_bytes(&new_entry, "d tree");
        let other_id = {
            let out = run_libra_command(&["rev-parse", &format!("{base}:other.txt")], p);
            assert_cli_success(&out, "other.txt id");
            stdout_trimmed(&out)
        };
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        for (name, mode, id) in [
            ("d", "40000", d_tree.as_str()),
            ("old", "100644", edited.as_str()),
            ("other.txt", "100644", other_id.as_str()),
        ] {
            let mut entry = format!("{mode} {name}\0").into_bytes();
            entry.extend(hex_bytes(id));
            let key = if mode == "40000" {
                format!("{name}/")
            } else {
                name.to_string()
            };
            entries.push((key, entry));
        }
        entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let root: Vec<u8> = entries.into_iter().flat_map(|(_, entry)| entry).collect();
        let root = write_tree_bytes(&root, "root tree");
        let theirs = {
            let out = run_libra_command(&["commit-tree", &root, "-p", &base, "-m", "theirs"], p);
            assert_cli_success(&out, "commit-tree");
            stdout_trimmed(&out)
        };
        assert_cli_success(
            &run_libra_command(&["update-ref", "refs/heads/feature", &theirs], p),
            "refs/heads/feature",
        );
        assert_cli_success(&run_libra_command(&["reset", "--hard", &base], p), "reset");
        assert_cli_success(&run_libra_command(&["rm", "d"], p), "drop the file d");
        std::fs::create_dir_all(p.join("d")).expect("dir");
        std::fs::rename(p.join("old"), p.join("d/new")).expect("rename");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "ours renames old to d/new", "--no-verify"],
                p,
            ),
            "ours",
        );

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        // Git merges this cleanly with the edit at `d/new`, and both walks must
        // agree with it — a weaker "the blob is on SOME stage" assertion let the
        // two engines disagree here for a whole round (Codex R18).
        assert_cli_success(
            &out,
            &format!("walk {env:?}: clean merge, as git merges it: {stdout} / {stderr}"),
        );
        assert_eq!(
            std::fs::read_to_string(p.join("d/new")).expect("merged file"),
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "walk {env:?}: the rename was used and the edit followed it"
        );
        let stages = index_stage_lines(p, "d/new");
        assert_eq!(stages.len(), 1, "walk {env:?}: {stages:?}");
        assert!(
            stages[0].contains(" 0\t") && stages[0].contains(&edited),
            "walk {env:?}: the edit is at stage 0: {stages:?}"
        );
        assert!(
            index_stage_lines(p, "old").is_empty(),
            "walk {env:?}: nothing is left at the old path"
        );
    }
}

/// Codex R20: `files_changed` for an INCOMING rename, counted the way Git's
/// diffstat counts it, and identical across preview, `--no-commit` and
/// `--continue` on both walks. Two shapes, both measured on git 2.50.1:
///
///  * theirs renames `old` to `new` AND edits a line while ours touches an
///    unrelated file — `1 file changed`, rendered `old => new`, because the pair
///    still reads as a rename;
///  * ours first deletes most of `old` so HEAD-to-result similarity drops below
///    the threshold — `2 files changed`, a delete and a create, because the pair
///    no longer reads as a rename at all.
///
/// The earlier carriers put the rename on OURS (already in HEAD) or made theirs
/// perform a pure move, so neither exercised this accounting.
#[test]
fn merge_incoming_rename_files_changed_matches_git_across_modes() {
    for (label, ours_truncates, expected) in [("edited", false, 1), ("low-similarity", true, 2)] {
        for env in [
            &[][..],
            &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
        ] {
            let full: String = (0..12).map(|n| format!("line {n}\n")).collect();
            let build = |p: &std::path::Path| {
                std::fs::write(p.join("old"), &full).expect("old");
                std::fs::write(p.join("unrelated.txt"), "u\n").expect("unrelated");
                assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
                assert_cli_success(
                    &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
                    "base",
                );
                assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
                if ours_truncates {
                    // Drop most of the file so the surviving pair is no longer
                    // similar enough to read as a rename.
                    let tail: String = (8..12).map(|n| format!("line {n}\n")).collect();
                    std::fs::write(p.join("old"), tail).expect("ours truncates");
                } else {
                    std::fs::write(p.join("unrelated.txt"), "ours change\n").expect("ours");
                }
                assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
                assert_cli_success(
                    &run_libra_command(&["commit", "-m", "ours", "--no-verify"], p),
                    "ours",
                );
                assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
                std::fs::rename(p.join("old"), p.join("new")).expect("theirs renames");
                let edited: String = (0..12)
                    .map(|n| {
                        if (9..12).contains(&n) {
                            format!("EDITED {n}\n")
                        } else {
                            format!("line {n}\n")
                        }
                    })
                    .collect();
                std::fs::write(p.join("new"), edited).expect("theirs edits");
                assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
                assert_cli_success(
                    &run_libra_command(&["commit", "-m", "theirs", "--no-verify"], p),
                    "theirs",
                );
                assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
            };

            // Preview.
            let repo = create_committed_repo_via_cli();
            let p = repo.path();
            build(p);
            let preview = run_libra_command_with_stdin_and_env(
                &["--json", "merge", "--dry-run", "feature"],
                p,
                "",
                env,
            );
            assert_cli_success(&preview, "dry-run");
            let json = parse_json_stdout(&preview);
            assert_eq!(
                json["data"]["files_changed"], expected,
                "{label} walk {env:?}: preview counts it as git does: {json}"
            );

            // `--no-commit`, then `--continue`: the same number, twice.
            let repo = create_committed_repo_via_cli();
            let p = repo.path();
            build(p);
            let staged = run_libra_command_with_stdin_and_env(
                &["--json", "merge", "--no-commit", "feature"],
                p,
                "",
                env,
            );
            assert_cli_success(&staged, "no-commit");
            let json = parse_json_stdout(&staged);
            assert_eq!(
                json["data"]["files_changed"], expected,
                "{label} walk {env:?}: --no-commit agrees: {json}"
            );
            let finished = run_libra_command_with_stdin_and_env(
                &["--json", "merge", "--continue"],
                p,
                "",
                env,
            );
            assert_cli_success(&finished, "continue");
            let json = parse_json_stdout(&finished);
            assert_eq!(
                json["data"]["files_changed"], expected,
                "{label} walk {env:?}: --continue agrees: {json}"
            );
        }
    }
}

/// Codex R21: the empty-blob exclusion belongs to DETECTION, not to reporting.
/// Git turns rename detection off for empty blobs while merging
/// (`rename_empty = 0`), but its diffstat is an ordinary diff and pairs them as
/// usual — so with ours emptying `old` and theirs purely renaming it to `new`,
/// `git merge` still reports `1 file changed`. Reusing the merge's snapshot at
/// finalize time made `--continue` report 2 where the preview reported 1.
#[test]
fn merge_files_changed_pairs_an_emptied_rename_when_reporting() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let build = |p: &std::path::Path| {
            commit_file(p, "old", "a\nb\nc\nd\ne\nf\ng\nh\n", "base");
            commit_file(p, "unrelated.txt", "u\n", "base unrelated");
            assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
            std::fs::write(p.join("old"), "").expect("ours empties old");
            assert_cli_success(&run_libra_command(&["add", "old"], p), "stage");
            assert_cli_success(
                &run_libra_command(&["commit", "-m", "ours empties old", "--no-verify"], p),
                "ours",
            );
            assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
            std::fs::rename(p.join("old"), p.join("new")).expect("theirs renames");
            assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
            assert_cli_success(
                &run_libra_command(&["commit", "-m", "theirs renames", "--no-verify"], p),
                "theirs",
            );
            assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        };

        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        build(p);
        let preview = run_libra_command_with_stdin_and_env(
            &["--json", "merge", "--dry-run", "feature"],
            p,
            "",
            env,
        );
        assert_cli_success(&preview, "dry-run");
        assert_eq!(
            parse_json_stdout(&preview)["data"]["files_changed"],
            1,
            "walk {env:?}: the preview counts the rename once, as git does"
        );

        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        build(p);
        assert_cli_success(
            &run_libra_command_with_stdin_and_env(&["merge", "--no-commit", "feature"], p, "", env),
            "no-commit",
        );
        let finished =
            run_libra_command_with_stdin_and_env(&["--json", "merge", "--continue"], p, "", env);
        assert_cli_success(&finished, "continue");
        assert_eq!(
            parse_json_stdout(&finished)["data"]["files_changed"],
            1,
            "walk {env:?}: --continue agrees with the preview"
        );
    }
}

/// Codex R20: finalizing a merge must never fail because of the rename
/// configuration. `merge -s ours` bypasses detection entirely, and Git's
/// `--continue` (a plain commit) never parses `merge.renames` at all — yet the
/// count added for R18 parsed it, so staging a move before `--continue` failed
/// the command with LBR-REPO-003. The count is a report: an unusable value now
/// falls back to the plain number.
#[test]
fn merge_continue_survives_an_unusable_rename_config() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "old", "a\nb\nc\n", "base");
    commit_file(p, "f.txt", "x\n", "base f");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    commit_file(p, "o.txt", "o\n", "ours");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    commit_file(p, "t.txt", "t\n", "theirs");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    assert_cli_success(
        &run_libra_command(&["config", "merge.renames", "not-a-bool"], p),
        "merge.renames=not-a-bool",
    );

    assert_cli_success(
        &run_libra_command(&["merge", "-s", "ours", "--no-commit", "feature"], p),
        "-s ours ignores the rename config, as git does",
    );
    // The user stages a move before finishing — the staged result no longer
    // equals HEAD, which is what defeated the first version of the guard.
    std::fs::rename(p.join("old"), p.join("moved")).expect("move a tracked file");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage the move");
    assert_cli_success(
        &run_libra_command(&["merge", "--continue"], p),
        "the merge finalizes; the count never fails the command",
    );
}

/// Codex R19: an empty-directory marker at a PREFIX of the destination does not
/// make the base "hold something" there. Base holds `old` and an EMPTY tree `d`;
/// ours renames `old` to `d/new`; theirs edits `old` and adds an empty
/// `d/new/sub`. Git 2.50.1 conflicts, and so must both walks — accepting
/// markers through the prefix test let the flattening walk commit a rename git
/// refuses. The companion case, where `d` is a FILE, is
/// `merge_rename_destination_is_not_re_settled_as_a_collision`.
#[test]
fn merge_rename_under_an_empty_base_directory_is_declined() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        commit_file(p, "old", "a\nb\nc\nd\ne\nf\ng\nh\n", "base old");
        let seed = head_commit(p);
        let hex_bytes = |id: &str| -> Vec<u8> {
            (0..id.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&id[i..i + 2], 16).expect("hex"))
                .collect()
        };
        let write_tree_bytes = |bytes: &[u8], label: &str| -> String {
            std::fs::write(p.join(".tree"), bytes).expect("tree bytes");
            let out = run_libra_command(
                &["hash-object", "-t", "tree", "-w", "--literally", ".tree"],
                p,
            );
            assert_cli_success(&out, label);
            std::fs::remove_file(p.join(".tree")).expect("cleanup");
            stdout_trimmed(&out)
        };
        let id_of = |rev: &str| -> String {
            let out = run_libra_command(&["rev-parse", rev], p);
            assert_cli_success(&out, "rev-parse");
            stdout_trimmed(&out)
        };
        let empty = write_tree_bytes(&[], "empty tree");
        let old_id = id_of(&format!("{seed}:old"));
        let other_id = id_of(&format!("{seed}:other.txt"));
        let root_bytes = |entries: Vec<(&str, &str, String)>| -> Vec<u8> {
            let mut rows: Vec<(String, Vec<u8>)> = entries
                .into_iter()
                .map(|(name, mode, id)| {
                    let mut entry = format!("{mode} {name}\0").into_bytes();
                    entry.extend(hex_bytes(&id));
                    let key = if mode == "40000" {
                        format!("{name}/")
                    } else {
                        name.to_string()
                    };
                    (key, entry)
                })
                .collect();
            rows.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            rows.into_iter().flat_map(|(_, entry)| entry).collect()
        };
        // BASE: `old`, an EMPTY tree at `d`, and `other.txt`.
        let base_root = write_tree_bytes(
            &root_bytes(vec![
                ("d", "40000", empty.clone()),
                ("old", "100644", old_id.clone()),
                ("other.txt", "100644", other_id.clone()),
            ]),
            "base root",
        );
        let base = {
            let out = run_libra_command(&["commit-tree", &base_root, "-p", &seed, "-m", "base"], p);
            assert_cli_success(&out, "commit-tree base");
            stdout_trimmed(&out)
        };
        // THEIRS: `old` edited, and `d` holding only an empty `new/sub`.
        let edited = {
            let out = run_libra_command_with_stdin(
                &["hash-object", "-w", "--stdin"],
                p,
                "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            );
            assert_cli_success(&out, "hash-object");
            stdout_trimmed(&out)
        };
        let mut sub = b"40000 sub\0".to_vec();
        sub.extend(hex_bytes(&empty));
        let new_tree = write_tree_bytes(&sub, "d/new tree");
        let mut new_entry = b"40000 new\0".to_vec();
        new_entry.extend(hex_bytes(&new_tree));
        let d_tree = write_tree_bytes(&new_entry, "d tree");
        let theirs_root = write_tree_bytes(
            &root_bytes(vec![
                ("d", "40000", d_tree),
                ("old", "100644", edited),
                ("other.txt", "100644", other_id.clone()),
            ]),
            "theirs root",
        );
        let theirs = {
            let out = run_libra_command(
                &["commit-tree", &theirs_root, "-p", &base, "-m", "theirs"],
                p,
            );
            assert_cli_success(&out, "commit-tree theirs");
            stdout_trimmed(&out)
        };
        assert_cli_success(
            &run_libra_command(&["update-ref", "refs/heads/feature", &theirs], p),
            "refs/heads/feature",
        );
        // OURS: `old` renamed to `d/new`.
        let ours_d = write_tree_bytes(
            &{
                let mut bytes = b"100644 new\0".to_vec();
                bytes.extend(hex_bytes(&old_id));
                bytes
            },
            "ours d tree",
        );
        let ours_root = write_tree_bytes(
            &root_bytes(vec![
                ("d", "40000", ours_d),
                ("other.txt", "100644", other_id),
            ]),
            "ours root",
        );
        let ours = {
            let out = run_libra_command(&["commit-tree", &ours_root, "-p", &base, "-m", "ours"], p);
            assert_cli_success(&out, "commit-tree ours");
            stdout_trimmed(&out)
        };
        assert_cli_success(
            &run_libra_command(&["update-ref", "refs/heads/main", &ours], p),
            "refs/heads/main",
        );
        assert_cli_success(&run_libra_command(&["reset", "--hard", &ours], p), "reset");

        // `merge_expecting_conflict` already pins exit 128 + LBR-CONFLICT-002.
        // What matters here is that the rename was NOT used: the edited content
        // must not have followed it to the destination.
        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stages = index_stage_lines(p, "d/new");
        assert!(
            !stages.iter().any(|line| line.contains(" 0\t")),
            "walk {env:?}: the rename was refused, as git refuses it: \
             {stages:?} / {stdout}"
        );
    }
}

/// Codex R16: SEVERAL departing siblings must empty their directory together.
/// With `new/child` and `new/second` both renamed away, neither one alone makes
/// `new` empty, so a "does any other entry remain" test answers yes for both
/// and neither release fires. Counting occupants fixes it. Git merges this
/// cleanly with all three edits; both walks must too.
#[test]
fn merge_three_renames_emptying_one_directory_all_apply() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "o\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        let old_body: String = (1..=8).map(|n| format!("a{n}\n")).collect();
        let child_body: String = (1..=8).map(|n| format!("p{n}\n")).collect();
        let second_body: String = (1..=8).map(|n| format!("q{n}\n")).collect();
        std::fs::write(p.join("old"), &old_body).expect("old");
        std::fs::create_dir_all(p.join("new")).expect("dir");
        std::fs::write(p.join("new/child"), &child_body).expect("child");
        std::fs::write(p.join("new/second"), &second_body).expect("second");
        assert_cli_success(
            &run_libra_command(&["add", "old", "new/child", "new/second"], p),
            "stage",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "old", "new/child", "new/second"], p),
            "drop",
        );
        let _ = std::fs::remove_dir(p.join("new"));
        std::fs::write(p.join("new"), &old_body).expect("new");
        std::fs::write(p.join("z"), &child_body).expect("z");
        std::fs::write(p.join("w"), &second_body).expect("w");
        assert_cli_success(&run_libra_command(&["add", "new", "z", "w"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "ours renames all three", "--no-verify"],
                p,
            ),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::write(p.join("old"), old_body.replace("a2\n", "a2 EDIT\n")).expect("edit old");
        std::fs::write(p.join("new/child"), child_body.replace("p2\n", "p2 EDIT\n"))
            .expect("edit child");
        std::fs::write(
            p.join("new/second"),
            second_body.replace("q2\n", "q2 EDIT\n"),
        )
        .expect("edit second");
        assert_cli_success(
            &run_libra_command(&["add", "old", "new/child", "new/second"], p),
            "stage",
        );
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "theirs edits all three", "--no-verify"],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge — the three renames empty the directory");
        for (file, body, marker) in [
            ("new", &old_body, "a2 EDIT"),
            ("z", &child_body, "p2 EDIT"),
            ("w", &second_body, "q2 EDIT"),
        ] {
            let got = std::fs::read_to_string(p.join(file)).expect("merged file");
            assert!(
                got.contains(marker),
                "walk {env:?}: {file} carries theirs' edit: {got:?} (base {body:?})"
            );
        }
        assert!(index_stage_lines(p, "old").is_empty(), "walk {env:?}");
    }
}

/// Codex R16: a rename BLOCKED at its destination never happens, so its source
/// stays — and that can block a further rename that was counting on it moving.
/// Releasing every source once and deciding once accepted both, which left a
/// file and a directory sharing the name `new` in the index (`ls-files` failed
/// with EISDIR); the decision is a fixed point, and both walks still agree
/// exactly on it.
///
/// MG-06 then took the outcome to Git's: the first rename is used and merges
/// cleanly, the second collides with theirs' added `z` as a base-less add/add,
/// and both sources are resolved by removal — every blob id asserted below is
/// Git's own, measured on git 2.50.1.
#[test]
fn merge_a_blocked_rename_keeps_its_source_occupied() {
    let mut results = Vec::new();
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "o\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        let old_body: String = (1..=8).map(|n| format!("a{n}\n")).collect();
        let child_body: String = (1..=8).map(|n| format!("p{n}\n")).collect();
        std::fs::write(p.join("old"), &old_body).expect("old");
        std::fs::create_dir_all(p.join("new")).expect("dir");
        std::fs::write(p.join("new/child"), &child_body).expect("child");
        assert_cli_success(&run_libra_command(&["add", "old", "new/child"], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        assert_cli_success(&run_libra_command(&["rm", "old", "new/child"], p), "drop");
        let _ = std::fs::remove_dir(p.join("new"));
        std::fs::write(p.join("new"), &old_body).expect("new");
        std::fs::write(p.join("z"), &child_body).expect("z");
        assert_cli_success(&run_libra_command(&["add", "new", "z"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &[
                    "commit",
                    "-m",
                    "ours renames old to new and new/child to z",
                    "--no-verify",
                ],
                p,
            ),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::write(p.join("old"), old_body.replace("a2\n", "a2 EDIT\n")).expect("edit old");
        std::fs::write(p.join("new/child"), child_body.replace("p2\n", "p2 EDIT\n"))
            .expect("edit child");
        // Theirs occupies `z`, so `new/child` cannot move — and `old` therefore
        // cannot move onto `new` either.
        std::fs::write(p.join("z"), "theirs own z\n").expect("z");
        assert_cli_success(
            &run_libra_command(&["add", "old", "new/child", "z"], p),
            "stage",
        );
        assert_cli_success(
            &run_libra_command(
                &[
                    "commit",
                    "-m",
                    "theirs edits both and adds z",
                    "--no-verify",
                ],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let _ = merge_expecting_conflict(p, &["merge", "feature"], env);
        // The index is well formed: no file and directory share `new`.
        let listing = run_libra_command(&["ls-files", "-s"], p);
        assert_cli_success(&listing, "ls-files still works");
        let mut stages: Vec<String> = String::from_utf8_lossy(&listing.stdout)
            .lines()
            .filter(|line| !line.contains("libraignore"))
            .map(|line| line.to_string())
            .collect();
        stages.sort();
        // MG-06 brings this chain to Git's own answer. Measured on git 2.50.1
        // (`/Volumes/Data/tmp/mg06-git/blocked`, `git merge-tree --messages`):
        // `old` -> `new` is USED and merges cleanly, carrying theirs' edit
        // (blob `5a094330…`), while `new/child` -> `z` collides with theirs'
        // added `z` and comes out as a base-less add/add (stages `a8aa0f7b…`
        // and `b49573fd…`); the sole message is
        // `CONFLICT (add/add): Merge conflict in z`. Every blob id below is
        // Git's, byte for byte. Before MG-06 Libra declined BOTH renames and
        // left `new` absent — safe (Codex R16 fixed an index corruption that
        // way) but not what Git does.
        assert!(
            stages.iter().any(|line| line.contains(" 0\tnew")
                && line.contains("5a094330b268dbf633b76f4ebd1e61d2aad066e1")),
            "walk {env:?}: the rename is used and merges cleanly, as Git's does: {stages:?}"
        );
        let z_stages = index_stage_lines(p, "z");
        assert!(
            z_stages.iter().any(|line| line.contains(" 2\t")
                && line.contains("a8aa0f7b7e73a0c2b690f9fde090dad0985e1399"))
                && z_stages.iter().any(|line| line.contains(" 3\t")
                    && line.contains("b49573fdc4779b84118727665b110636da2463f0")),
            "walk {env:?}: the second rename collides as a base-less add/add: {z_stages:?}"
        );
        assert!(
            !z_stages.iter().any(|line| line.contains(" 1\t")),
            "walk {env:?}: a collision records no merge base: {z_stages:?}"
        );
        // Both rename sources are resolved by removal, as Git resolves them.
        assert!(
            index_stage_lines(p, "old").is_empty() && index_stage_lines(p, "new/child").is_empty(),
            "walk {env:?}: neither source survives: {stages:?}"
        );
        results.push(stages);
    }
    assert_eq!(
        results[0], results[1],
        "the two walks agree exactly on the blocked chain"
    );
}

/// MG-06 collision/structural-block split: the independent `x` is an exact
/// destination collision for `a -> x`, so that rename consumes source `a`
/// rather than reoccupying it. The now-valid `b -> a/child` must remain clean,
/// and stale pre-rename D/F bookkeeping must not resurrect `a` under a suffix.
#[test]
fn merge_collision_consumed_source_is_not_restored_as_a_df_conflict() {
    let mut results = Vec::new();
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "o\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        let a_body: String = (1..=8).map(|n| format!("a{n}\n")).collect();
        let b_body: String = (1..=8).map(|n| format!("b{n}\n")).collect();
        std::fs::write(p.join("a"), &a_body).expect("a");
        std::fs::write(p.join("b"), &b_body).expect("b");
        assert_cli_success(&run_libra_command(&["add", "a", "b"], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");

        assert_cli_success(&run_libra_command(&["rm", "a", "b"], p), "drop");
        std::fs::create_dir(p.join("a")).expect("a directory");
        std::fs::write(p.join("a/child"), &b_body).expect("a/child");
        std::fs::write(p.join("x"), &a_body).expect("x");
        assert_cli_success(&run_libra_command(&["add", "a/child", "x"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &[
                    "commit",
                    "-m",
                    "ours renames a to x and b to a/child",
                    "--no-verify",
                ],
                p,
            ),
            "ours",
        );

        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        let edited_a = a_body.replace("a2\n", "a2 EDIT\n");
        let edited_b = b_body.replace("b2\n", "b2 EDIT\n");
        std::fs::write(p.join("a"), &edited_a).expect("edit a");
        std::fs::write(p.join("b"), &edited_b).expect("edit b");
        // `a -> x` collides with this independent add. Path-level rename
        // arbitration consumes source `a`; that makes room for `b -> a/child`.
        // Git 2.54.0 keeps the latter as a clean stage-0 path and does not
        // resurrect `a` as a stale file/directory conflict afterwards.
        std::fs::write(p.join("x"), "theirs own x\n").expect("x");
        assert_cli_success(&run_libra_command(&["add", "a", "b", "x"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &[
                    "commit",
                    "-m",
                    "theirs edits both and adds x",
                    "--no-verify",
                ],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let _ = merge_expecting_conflict(p, &["merge", "feature"], env);
        let listing = run_libra_command(&["ls-files", "-s"], p);
        assert_cli_success(&listing, "ls-files still works");
        let mut stages: Vec<String> = String::from_utf8_lossy(&listing.stdout)
            .lines()
            .filter(|line| !line.contains("libraignore"))
            .map(|line| line.to_string())
            .collect();
        stages.sort();
        let child_stages = index_stage_lines(p, "a/child");
        assert_eq!(child_stages.len(), 1, "walk {env:?}: {stages:?}");
        assert!(
            child_stages[0].contains(" 0\ta/child"),
            "walk {env:?}: the moved `b` is resolved at stage 0: {stages:?}"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("a/child")).expect("a/child result"),
            edited_b,
            "walk {env:?}: the moved path keeps the other side's source edit"
        );
        assert!(
            index_stage_lines(p, "b").is_empty() && !p.join("b").exists(),
            "walk {env:?}: the consumed source `b` stays absent: {stages:?}"
        );
        assert!(
            index_stage_lines(p, "a~feature").is_empty() && !p.join("a~feature").exists(),
            "walk {env:?}: the consumed source `a` must not return as a D/F conflict: {stages:?}"
        );
        results.push(stages);
    }
    assert_eq!(
        results[0], results[1],
        "the two walks agree after both colliding renames consume their sources"
    );
}

/// Codex R15: two renames can free each other's destination. Ours renames both
/// `old` to `new` and `new/child` to `z`, so nothing is left under `new` for
/// the first rename to collide with. Measured on git 2.50.1 with theirs editing
/// BOTH sources: a clean merge that keeps both edits. Treating a path a rename
/// moves away as occupancy left a spurious modify/delete conflict at `old`.
#[test]
fn merge_two_renames_that_free_each_other_both_apply() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "o\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        let old_body = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let child_body = "p1\np2\np3\np4\np5\np6\np7\np8\n";
        std::fs::write(p.join("old"), old_body).expect("old");
        std::fs::create_dir_all(p.join("new")).expect("dir");
        std::fs::write(p.join("new/child"), child_body).expect("child");
        assert_cli_success(&run_libra_command(&["add", "old", "new/child"], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        assert_cli_success(&run_libra_command(&["rm", "old", "new/child"], p), "drop");
        let _ = std::fs::remove_dir(p.join("new"));
        std::fs::write(p.join("new"), old_body).expect("new");
        std::fs::write(p.join("z"), child_body).expect("z");
        assert_cli_success(&run_libra_command(&["add", "new", "z"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &[
                    "commit",
                    "-m",
                    "ours renames old to new and new/child to z",
                    "--no-verify",
                ],
                p,
            ),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::write(p.join("old"), old_body.replace("b\n", "b EDIT\n")).expect("edit old");
        std::fs::write(p.join("new/child"), child_body.replace("p2\n", "p2 EDIT\n"))
            .expect("edit child");
        assert_cli_success(&run_libra_command(&["add", "old", "new/child"], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs edits both", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge — each rename frees the other");
        assert_eq!(
            std::fs::read_to_string(p.join("new")).expect("new"),
            old_body.replace("b\n", "b EDIT\n"),
            "walk {env:?}: the first rename carried theirs' edit"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("z")).expect("z"),
            child_body.replace("p2\n", "p2 EDIT\n"),
            "walk {env:?}: the second rename carried theirs' edit"
        );
        assert!(index_stage_lines(p, "old").is_empty(), "walk {env:?}");
    }
}

/// Codex R14: an empty tree AT the rename destination is not in the way, but
/// one BENEATH it is — it makes the destination a real directory, and MG-04's
/// rule adopts a directory the base did not have verbatim. Measured on
/// git 2.50.1 with theirs editing `old` and adding `new/sub` as an empty tree:
/// `git merge-tree --messages` reports
/// `CONFLICT (file/directory): directory in the way of new` and relocates the
/// file to `new~<branch>`. The occupancy set had excluded every empty marker,
/// so the flattening walk used the rename and committed cleanly.
#[test]
fn merge_rename_onto_a_nested_empty_directory_is_declined() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        commit_file(p, "old.txt", "a\nb\nc\nd\ne\nf\ng\nh\n", "base");
        let base = head_commit(p);
        let hex_bytes = |id: &str| -> Vec<u8> {
            (0..id.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&id[i..i + 2], 16).expect("hex"))
                .collect()
        };
        let write_tree_bytes = |bytes: &[u8], label: &str| -> String {
            std::fs::write(p.join(".tree"), bytes).expect("tree bytes");
            let out = run_libra_command(
                &["hash-object", "-t", "tree", "-w", "--literally", ".tree"],
                p,
            );
            assert_cli_success(&out, label);
            std::fs::remove_file(p.join(".tree")).expect("cleanup");
            stdout_trimmed(&out)
        };
        let edited = {
            let out = run_libra_command_with_stdin(
                &["hash-object", "-w", "--stdin"],
                p,
                "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            );
            assert_cli_success(&out, "hash-object");
            stdout_trimmed(&out)
        };
        // `new` is a tree holding one entry, `sub`, which is the EMPTY tree.
        let empty = write_tree_bytes(&[], "empty tree");
        let mut sub = b"40000 sub\0".to_vec();
        sub.extend(hex_bytes(&empty));
        let nested = write_tree_bytes(&sub, "new/ tree");

        assert_cli_success(
            &run_libra_command(
                &[
                    "update-index",
                    "--cacheinfo",
                    &format!("100644,{edited},old.txt"),
                ],
                p,
            ),
            "stage theirs' edit",
        );
        let tree = run_libra_command(&["write-tree"], p);
        assert_cli_success(&tree, "write-tree");
        let tree = stdout_trimmed(&tree);
        let listing = run_libra_command(&["ls-tree", &tree], p);
        assert_cli_success(&listing, "ls-tree");
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        for line in String::from_utf8_lossy(&listing.stdout).lines() {
            let (meta, name) = line.split_once('\t').expect("ls-tree line");
            let mut parts = meta.split_whitespace();
            let mode = parts.next().expect("mode");
            let _kind = parts.next();
            let id = parts.next().expect("id");
            let mut entry = format!("{} {name}\0", mode.trim_start_matches('0')).into_bytes();
            entry.extend(hex_bytes(id));
            entries.push((name.to_string(), entry));
        }
        let mut new_entry = b"40000 new\0".to_vec();
        new_entry.extend(hex_bytes(&nested));
        entries.push(("new/".to_string(), new_entry));
        entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let root: Vec<u8> = entries.into_iter().flat_map(|(_, entry)| entry).collect();
        let root = write_tree_bytes(&root, "root tree");
        let theirs = {
            let out = run_libra_command(
                &[
                    "commit-tree",
                    &root,
                    "-p",
                    &base,
                    "-m",
                    "theirs edits old.txt and adds an empty new/sub",
                ],
                p,
            );
            assert_cli_success(&out, "commit-tree");
            stdout_trimmed(&out)
        };
        assert_cli_success(
            &run_libra_command(&["update-ref", "refs/heads/feature", &theirs], p),
            "refs/heads/feature",
        );
        assert_cli_success(&run_libra_command(&["reset", "--hard", &base], p), "reset");
        std::fs::rename(p.join("old.txt"), p.join("new")).expect("rename");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "ours renames old.txt to new", "--no-verify"],
                p,
            ),
            "ours",
        );

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        assert!(
            stdout.contains("CONFLICT (file/directory)"),
            "walk {env:?}: the nested empty directory is in the way: {stdout}"
        );
        assert!(
            !index_stage_lines(p, "new~HEAD").is_empty(),
            "walk {env:?}: the renamed file was relocated, as git relocates it"
        );
    }
}

/// FIX-MG05-02 (Codex R13, pre-existing — the released v0.22.15 reproduces it):
/// the criss-cross fold is a merge, so it has to detect renames like any other.
/// Without that the virtual ancestor keeps the OLD path while both sides carry
/// the new one, and the outer merge compares each side against a base that has
/// nothing at the renamed path — silently resurrecting content one side had
/// deliberately reverted. Measured on git 2.50.1: with bases `A` (renames `old`
/// to `new`) and `B` (edits line 2), ours merging both and reverting B's edit
/// and theirs merging both and editing line 7, Git keeps the revert.
#[test]
fn merge_criss_cross_fold_detects_renames_and_keeps_a_revert() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let original: String = (1..=8).map(|n| format!("line{n}\n")).collect();
    let b_edit = original.replace("line2", "B edit");
    let reverted = original.clone();
    let d_edit = b_edit.replace("line7", "D edit");

    std::fs::write(p.join("old"), &original).expect("write old");
    assert_cli_success(&run_libra_command(&["add", "old"], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    for branch in ["a", "b"] {
        assert_cli_success(&run_libra_command(&["branch", branch], p), "branch");
    }
    // A renames the file.
    assert_cli_success(&run_libra_command(&["checkout", "a"], p), "a");
    std::fs::rename(p.join("old"), p.join("new")).expect("rename");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "A renames old to new", "--no-verify"], p),
        "A",
    );
    // B edits line 2 at the old path.
    assert_cli_success(&run_libra_command(&["checkout", "b"], p), "b");
    commit_file(p, "old", &b_edit, "B edits line 2");

    // C = merge(A, B), then revert B's edit.
    assert_cli_success(&run_libra_command(&["checkout", "a"], p), "a");
    assert_cli_success(&run_libra_command(&["branch", "c"], p), "branch c");
    assert_cli_success(&run_libra_command(&["checkout", "c"], p), "c");
    assert_cli_success(
        &run_libra_command(&["merge", "b", "-m", "C merges", "--no-verify"], p),
        "C merges",
    );
    commit_file(p, "new", &reverted, "C reverts B's edit");

    // D = merge(B, A), then edit line 7.
    assert_cli_success(&run_libra_command(&["checkout", "b"], p), "b");
    assert_cli_success(&run_libra_command(&["branch", "d"], p), "branch d");
    assert_cli_success(&run_libra_command(&["checkout", "d"], p), "d");
    assert_cli_success(
        &run_libra_command(&["merge", "a", "-m", "D merges", "--no-verify"], p),
        "D merges",
    );
    commit_file(p, "new", &d_edit, "D edits line 7");

    assert_cli_success(&run_libra_command(&["checkout", "c"], p), "c");
    assert_cli_success(
        &run_libra_command(&["merge", "d", "-m", "final", "--no-verify"], p),
        "the criss-cross merge is clean",
    );
    let merged = std::fs::read_to_string(p.join("new")).expect("merged file");
    assert!(
        merged.contains("line2") && !merged.contains("B edit"),
        "the revert survives the fold, as it does in git: {merged:?}"
    );
    assert!(
        merged.contains("D edit"),
        "the other side's edit is still applied: {merged:?}"
    );
}

/// Codex R13: Git turns rename detection OFF for EMPTY blobs when it merges
/// (`merge-ort.c:3449`, `rename_empty = 0`) — every empty file matches every
/// other, so an emptied placeholder would pair with anything. Measured on
/// git 2.50.1: base holds an empty `old`, ours moves it to an empty `new`,
/// theirs fills `old`; Git stops at `CONFLICT (modify/delete)` and keeps
/// theirs' content on `old`. Pairing them carried that content to `new`,
/// deleted `old` and exited 0. `diff` and `status` keep Git's own default.
#[test]
fn merge_does_not_pair_empty_blobs_as_renames() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "o\n", "other");
        std::fs::write(p.join("old"), "").expect("empty old");
        assert_cli_success(&run_libra_command(&["add", "old"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "base adds an empty old", "--no-verify"],
                p,
            ),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        std::fs::rename(p.join("old"), p.join("new")).expect("move the empty file");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ours moves it to new", "--no-verify"], p),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        commit_file(p, "old", "now it has content\n", "theirs fills old");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let _ = merge_expecting_conflict(p, &["merge", "feature"], env);
        let old_stages = index_stage_lines(p, "old");
        assert_eq!(old_stages.len(), 2, "walk {env:?}: {old_stages:?}");
        assert!(
            old_stages.iter().any(|line| line.contains(" 3\t")),
            "walk {env:?}: theirs' content survives at the old path: {old_stages:?}"
        );
        let new_stages = index_stage_lines(p, "new");
        assert_eq!(new_stages.len(), 1, "walk {env:?}: {new_stages:?}");
        assert!(
            new_stages[0].contains("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"),
            "walk {env:?}: the moved file is still empty: {new_stages:?}"
        );
    }
}

/// Codex R13 P2: holding the rename announcements until the write preflight
/// must not silence `--dry-run`, which writes nothing and so has no preflight
/// to wait for. A preview reports the same rename decisions the real merge
/// would make; `--json` stays machine-clean as always. MG-06 turned this
/// shape's notice into Git's rename/rename CONFLICT line, so the preview now
/// carries that line — and, like every would-conflict preview, exits 1.
#[test]
fn merge_dry_run_reports_rename_decisions() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "o\n", "other");
        commit_file(p, "old.txt", "a\nb\nc\nd\ne\nf\ng\nh\n", "base");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        std::fs::rename(p.join("old.txt"), p.join("ours.txt")).expect("ours renames");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ours renames", "--no-verify"], p),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::rename(p.join("old.txt"), p.join("theirs.txt")).expect("theirs renames");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "theirs renames elsewhere", "--no-verify"],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let preview =
            run_libra_command_with_stdin_and_env(&["merge", "--dry-run", "feature"], p, "", env);
        assert_eq!(
            preview.status.code(),
            Some(1),
            "walk {env:?}: a would-conflict preview exits 1"
        );
        let stdout = String::from_utf8_lossy(&preview.stdout).to_string();
        assert!(
            stdout.contains("CONFLICT (rename/rename):"),
            "walk {env:?}: the preview reports the rename decision: {stdout}"
        );

        let json = run_libra_command_with_stdin_and_env(
            &["merge", "--dry-run", "--json", "feature"],
            p,
            "",
            env,
        );
        assert_eq!(json.status.code(), Some(1), "walk {env:?}: same verdict");
        let json_out = String::from_utf8_lossy(&json.stdout).to_string();
        assert!(
            !json_out.contains("notice:") && !json_out.contains("CONFLICT ("),
            "walk {env:?}: json stdout stays machine-clean: {json_out}"
        );
    }
}

/// Codex R12 P2: a merge the writer's preflight refuses prints NO rename
/// decision, exactly as it prints no file/directory line (MG-04 set that rule).
/// The notices are held until the preflight has passed.
#[test]
fn a_refused_merge_prints_no_rename_notice() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "o\n", "other");
        commit_file(p, "old.txt", "a\nb\nc\nd\ne\nf\ng\nh\n", "base");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        std::fs::rename(p.join("old.txt"), p.join("ours.txt")).expect("ours renames");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ours renames", "--no-verify"], p),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::rename(p.join("old.txt"), p.join("theirs.txt")).expect("theirs renames");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "theirs renames elsewhere", "--no-verify"],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        // An untracked file the merge would overwrite: the preflight refuses.
        std::fs::write(p.join("theirs.txt"), "untracked precious\n").expect("untracked");

        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert!(
            !output.status.success(),
            "walk {env:?}: the merge is refused"
        );
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            !stdout.contains("notice:") && !stderr.contains("was renamed to"),
            "walk {env:?}: a refused merge announces no rename decision: {stdout} / {stderr}"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("theirs.txt")).expect("untracked file"),
            "untracked precious\n",
            "walk {env:?}: the untracked file is untouched"
        );
    }
}

/// FIX-MG05-01 (Codex R12, pre-existing — reproduced on the released v0.22.15
/// binary): `-X ours` / `-X theirs` settle CONTENT hunks only. Applying them to
/// a modify/delete resolved the pair in favour of the DELETION, so the other
/// side's edit was destroyed by a merge that exited 0 and recorded nothing.
/// Measured on git 2.50.1 in both directions and with both options: the merge
/// stops at `CONFLICT (modify/delete)` and keeps the modified content on its
/// stage. Both walks, both options, both directions.
#[test]
fn strategy_option_never_resolves_a_modify_delete() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        for option in ["ours", "theirs"] {
            // Both directions: whichever side holds the edit, that edit must
            // survive — as content, not merely as a stage number.
            for ours_deletes in [true, false] {
                let repo = create_committed_repo_via_cli();
                let p = repo.path();
                commit_file(p, "other.txt", "o\n", "other");
                commit_file(p, "f.txt", "a\nb\nc\n", "base");
                assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
                if ours_deletes {
                    assert_cli_success(&run_libra_command(&["rm", "f.txt"], p), "ours deletes");
                    assert_cli_success(
                        &run_libra_command(&["commit", "-m", "ours deletes", "--no-verify"], p),
                        "ours",
                    );
                    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
                    commit_file(p, "f.txt", "a\nEDITED\nc\n", "theirs edits");
                } else {
                    commit_file(p, "f.txt", "a\nEDITED\nc\n", "ours edits");
                    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
                    assert_cli_success(&run_libra_command(&["rm", "f.txt"], p), "theirs deletes");
                    assert_cli_success(
                        &run_libra_command(&["commit", "-m", "theirs deletes", "--no-verify"], p),
                        "theirs",
                    );
                }
                assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

                let args = ["merge", "-X", option, "feature"];
                let _ = merge_expecting_conflict(p, &args, env);
                let stages = index_stage_lines(p, "f.txt");
                let where_edit = if ours_deletes { " 3\t" } else { " 2\t" };
                assert_eq!(
                    stages.len(),
                    2,
                    "walk {env:?} -X {option} ours_deletes={ours_deletes}: {stages:?}"
                );
                assert!(
                    stages.iter().any(|line| line.contains(" 1\t")),
                    "walk {env:?} -X {option} ours_deletes={ours_deletes}: base stage: {stages:?}"
                );
                let edit = stages
                    .iter()
                    .find(|line| line.contains(where_edit))
                    .unwrap_or_else(|| {
                        panic!(
                            "walk {env:?} -X {option} ours_deletes={ours_deletes}: \
                             the modified side survives: {stages:?}"
                        )
                    });
                // The surviving stage really carries the EDIT, and the worktree
                // keeps it too — a stage number alone would not have caught the
                // deletion this test exists for.
                assert!(
                    !edit.contains("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"),
                    "walk {env:?} -X {option}: the surviving stage is not empty: {edit}"
                );
                // The edit survives in the worktree too. Libra frames a
                // modify/delete with conflict markers where Git leaves the
                // modified file verbatim ("Version <side> of f.txt left in
                // tree"); that framing predates this card — the released
                // v0.22.15 does the same — and belongs to the conflict
                // presentation axis, so the assertion is on the content being
                // RETAINED, which is what the data-loss fix is about.
                let worktree = std::fs::read_to_string(p.join("f.txt")).expect("worktree file");
                assert!(
                    worktree.contains("EDITED"),
                    "walk {env:?} -X {option} ours_deletes={ours_deletes}: \
                     the edited content is retained in the worktree: {worktree:?}"
                );
            }
        }
    }
}

/// The same rule under MG-04's directory/file relocation, which is the shape
/// Codex R12 reported: base `old` + `new/child`, ours renaming `old` to `new`
/// and deleting `new/child`, theirs modifying `new/child`. With `-X ours` the
/// modify/delete used to be resolved away, which also emptied the directory and
/// so removed the collision — the merge committed only `new` and both the
/// other side's edit and the conflict vanished. Git keeps `new/child` on
/// stages 1 and 3 and relocates the file to `new~HEAD`.
#[test]
fn strategy_option_keeps_a_modify_delete_under_a_relocated_directory() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "o\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        commit_file(p, "old", "a\nb\nc\nd\ne\nf\ng\nh\n", "base file");
        std::fs::create_dir_all(p.join("new")).expect("dir");
        std::fs::write(p.join("new/child"), "payload\n").expect("child");
        assert_cli_success(&run_libra_command(&["add", "new/child"], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base adds new/child", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        assert_cli_success(&run_libra_command(&["rm", "old", "new/child"], p), "drop");
        std::fs::write(p.join("new"), "a\nb\nc\nd\ne\nf\ng\nh\n").expect("write new");
        assert_cli_success(&run_libra_command(&["add", "new"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &[
                    "commit",
                    "-m",
                    "ours renames old to new and deletes new/child",
                    "--no-verify",
                ],
                p,
            ),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::write(p.join("new/child"), "payload edited\n").expect("edit child");
        assert_cli_success(&run_libra_command(&["add", "new/child"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "theirs edits new/child", "--no-verify"],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let output = merge_expecting_conflict(p, &["merge", "-X", "ours", "feature"], env);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        assert!(
            stdout.contains("CONFLICT (file/directory)"),
            "walk {env:?}: the collision is still reported: {stdout}"
        );
        let child = index_stage_lines(p, "new/child");
        assert_eq!(child.len(), 2, "walk {env:?}: {child:?}");
        assert!(
            child.iter().any(|line| line.contains(" 3\t")),
            "walk {env:?}: theirs' edit survives: {child:?}"
        );
        let moved = index_stage_lines(p, "new~HEAD");
        assert_eq!(moved.len(), 1, "walk {env:?}: {moved:?}");
        assert!(moved[0].contains(" 2\t"), "walk {env:?}: {moved:?}");
    }
}

/// Adversarial pre-review: inside a subtree BOTH sides changed identically,
/// the walk recorded the BASE entry as "the other side's entry", so a rename
/// whose source the other side ALSO deleted escaped the rename/delete check and
/// the pruned walk accepted it silently while the flattening engine declined it
/// with a notice. Git reports `CONFLICT (rename/delete)` for this shape, which
/// MG-06 owns; MG-05 must at least decline it the same way on both walks.
#[test]
fn merge_rename_whose_source_both_sides_deleted_conflicts_as_rename_delete() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        let body: String = (0..40).map(|n| format!("src line {n}\n")).collect();
        std::fs::create_dir_all(p.join("sub")).expect("sub");
        std::fs::write(p.join("sub/src"), &body).expect("sub/src");
        std::fs::write(p.join("sub/keep"), "keep\n").expect("sub/keep");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        assert_cli_success(&run_libra_command(&["rm", "sub/src"], p), "ours drops it");
        std::fs::write(p.join("dest"), &body).expect("dest");
        assert_cli_success(&run_libra_command(&["add", "dest"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &[
                    "commit",
                    "-m",
                    "ours renames sub/src to dest",
                    "--no-verify",
                ],
                p,
            ),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(&run_libra_command(&["rm", "sub/src"], p), "theirs drops it");
        std::fs::write(p.join("unrelated.txt"), "u\n").expect("unrelated");
        assert_cli_success(&run_libra_command(&["add", "unrelated.txt"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "theirs deletes sub/src too", "--no-verify"],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        // MG-06: this IS Git's rename/delete — ours renamed the file, theirs
        // deleted it, and both having dropped the source path changes nothing.
        // Measured on git 2.50.1 (`/Volumes/Data/tmp/mg06-git/jboth`):
        // `CONFLICT (rename/delete): sub/src renamed to dest in main, but
        // deleted in theirs.`, exit 1, stages 1 and 2 at `dest`.
        assert!(
            stdout.contains(
                "CONFLICT (rename/delete): sub/src renamed to dest in HEAD, but deleted in feature."
            ),
            "walk {env:?}: Git's rename/delete wording, verbatim: {stdout}"
        );
        let stages = index_stage_lines(p, "dest");
        assert!(
            stages.iter().any(|line| line.contains(" 1\t"))
                && stages.iter().any(|line| line.contains(" 2\t"))
                && !stages.iter().any(|line| line.contains(" 3\t")),
            "walk {env:?}: the base follows the rename, the deleting side has no stage: {stages:?}"
        );
    }
}

/// Adversarial pre-review, shape A: a destination directory the OTHER side
/// merely carries UNCHANGED from the base is not in the way — the renaming
/// side necessarily deleted everything under it, so the merge deletes it.
/// Measured on git 2.50.1 (`git merge-tree --write-tree --messages` and a real
/// `git merge`) with base `old` + `new/child`, ours renaming `old` to `new`
/// and deleting `new/child`, theirs editing `old`: a clean merge holding only
/// `new` with the edit. The flattening engine used to decline the rename here
/// and conflict at `old` while the pruned walk merged.
#[test]
fn merge_rename_onto_a_directory_the_merge_deletes_is_used() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        commit_file(p, "old", "a\nb\nc\nd\ne\nf\ng\nh\n", "base file");
        std::fs::create_dir_all(p.join("new")).expect("dir");
        std::fs::write(p.join("new/child"), "payload\n").expect("child");
        assert_cli_success(&run_libra_command(&["add", "new/child"], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base adds new/child", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        assert_cli_success(&run_libra_command(&["rm", "old", "new/child"], p), "drop");
        std::fs::write(p.join("new"), "a\nb\nc\nd\ne\nf\ng\nh\n").expect("write new");
        assert_cli_success(&run_libra_command(&["add", "new"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &[
                    "commit",
                    "-m",
                    "ours renames old to new and deletes new/child",
                    "--no-verify",
                ],
                p,
            ),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        commit_file(
            p,
            "old",
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "theirs edits old and leaves new/child alone",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        assert_eq!(
            std::fs::read_to_string(p.join("new")).expect("merged file"),
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "walk {env:?}: the edit followed the rename"
        );
        let stages = index_stage_lines(p, "new");
        assert_eq!(stages.len(), 1, "walk {env:?}: {stages:?}");
        assert!(stages[0].contains(" 0\t"), "walk {env:?}: {stages:?}");
        assert!(
            index_stage_lines(p, "old").is_empty(),
            "walk {env:?}: nothing is left at the old path"
        );
    }
}

/// Adversarial pre-review: a rename whose source the other side replaced with a
/// SYMLINK is a type change, which Git treats as a delete — it reports
/// `CONFLICT (modify/delete)` and keeps the renamed file's content. Accepting
/// the pair instead let the symlink be remapped onto the new path, where the
/// three-way match took it as the only change and the file's content vanished
/// from a merge that exited 0. Both walks lost it; both must now keep it.
#[test]
fn merge_rename_whose_source_became_a_symlink_keeps_the_content() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "o\n", "other");
        commit_file(p, "old", "a\nb\nc\nd\ne\nf\ng\nh\n", "base");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        std::fs::rename(p.join("old"), p.join("new")).expect("rename");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "ours renames old to new", "--no-verify"],
                p,
            ),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::remove_file(p.join("old")).expect("remove old");
        std::os::unix::fs::symlink("other.txt", p.join("old")).expect("symlink");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &[
                    "commit",
                    "-m",
                    "theirs replaces old with a symlink",
                    "--no-verify",
                ],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        // MG-06: Git takes its `type_changed` branch (`merge-ort.c:3205-3212`)
        // and never says rename/delete — the destination's own modify/delete
        // is the whole report. Measured on git 2.50.1
        // (`/Volumes/Data/tmp/mg06-git/ktype`).
        assert!(
            !stdout.contains("rename/delete"),
            "walk {env:?}: a type change is not a rename/delete: {stdout}"
        );
        // ... and the type-changed entry SURVIVES under the old name: git's
        // result tree holds `120000 old` beside the conflicted `new`.
        assert!(
            std::fs::symlink_metadata(p.join("old"))
                .expect("the type-changed entry survives")
                .file_type()
                .is_symlink(),
            "walk {env:?}: the symlink the other side put at the old name survives"
        );
        // MG-06: the merge base still follows the rename, so the new path is
        // an unmerged modify/delete with stages 1 and 2 — exactly the shape
        // measured on git 2.50.1 (`/Volumes/Data/tmp/mg06-git/ktype`), where
        // the only message is `CONFLICT (modify/delete)`. Before MG-06 the
        // rename was declined outright and `new` was a clean one-sided add.
        let stages = index_stage_lines(p, "new");
        assert!(
            stages.iter().any(|line| line.contains(" 1\t"))
                && stages.iter().any(|line| line.contains(" 2\t"))
                && !stages.iter().any(|line| line.contains(" 3\t")),
            "walk {env:?}: base and ours only: {stages:?}"
        );
        assert!(
            stages.iter().all(|line| line.contains("100644")),
            "walk {env:?}: it is still a regular file: {stages:?}"
        );
        // The renamed content survives. Git leaves the surviving version
        // VERBATIM here ("Version HEAD of new left in tree") while Libra marks
        // every modify/delete up — a presentation difference on the conflict
        // axis that predates this card (MG-05 Codex R13 recorded it), not a
        // loss of content.
        let body = std::fs::read_to_string(p.join("new")).expect("the renamed file");
        assert!(
            body.contains("a\nb\nc\nd\ne\nf\ng\nh\n"),
            "walk {env:?}: the renamed file's content survives: {body}"
        );
    }
}

/// Adversarial pre-review: an accepted rename can move the last file out of a
/// directory the other side replaced with a file, and the collision then does
/// not exist. Measured on git 2.50.1: base `dir/y` + `new/child`, ours editing
/// `new/child`, theirs renaming `dir/y` to `new` and `new/child` to `a` — a
/// clean merge holding `a` and a plain file `new`. The pruned walk settled its
/// D/F collisions at the end of the walk, before the rename fix-up, and
/// reported `new~theirs`; the flattening engine and Git did not.
#[test]
fn merge_rename_that_empties_a_directory_leaves_no_df_conflict() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "z\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        std::fs::create_dir_all(p.join("dir")).expect("dir");
        std::fs::create_dir_all(p.join("new")).expect("new");
        let long: String = (0..14).map(|n| format!("y line {n}\n")).collect();
        let child: String = (0..8).map(|n| format!("child line {n}\n")).collect();
        std::fs::write(p.join("dir/y"), &long).expect("dir/y");
        std::fs::write(p.join("new/child"), &child).expect("new/child");
        assert_cli_success(
            &run_libra_command(&["add", "dir/y", "new/child"], p),
            "stage",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        std::fs::write(p.join("new/child"), child.replace("child line 5", "OURS")).expect("edit");
        assert_cli_success(&run_libra_command(&["add", "new/child"], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ours edits new/child", "--no-verify"], p),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(&run_libra_command(&["rm", "dir/y", "new/child"], p), "drop");
        std::fs::write(p.join("new"), long.replace("y line 5", "THEIRS")).expect("new file");
        std::fs::write(p.join("a"), &child).expect("a");
        assert_cli_success(&run_libra_command(&["add", "new", "a"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &[
                    "commit",
                    "-m",
                    "theirs renames dir/y to new and new/child to a",
                    "--no-verify",
                ],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            !stdout.contains("file/directory"),
            "walk {env:?}: the emptied directory is not in the way: {stdout}"
        );
        assert!(
            p.join("new").is_file(),
            "walk {env:?}: the plain file stands at new"
        );
        let stages = index_stage_lines(p, "new");
        assert_eq!(stages.len(), 1, "walk {env:?}: {stages:?}");
        assert!(stages[0].contains(" 0\t"), "walk {env:?}: {stages:?}");
        assert!(
            index_stage_lines(p, "a").len() == 1,
            "walk {env:?}: the other rename landed too"
        );
    }
}

/// Codex R7 P1: a destination the merge BASE occupied but that neither side
/// kept is not in the way — only what SURVIVES the merge is. Measured on
/// git 2.50.1 (`git merge-tree --write-tree --messages` and a real `git merge`)
/// with base `old` + `dir/other`, both sides deleting `dir/other`, ours
/// renaming `old` to `dir` and theirs editing `old`: a clean merge holding only
/// `dir` with the edit. The flattening engine consulted the base as well and
/// conflicted here while the pruned walk merged; both walks now agree with Git.
#[test]
fn merge_rename_onto_a_path_only_the_base_occupied_is_used() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        commit_file(p, "old", "a\nb\nc\nd\ne\nf\ng\nh\n", "base file");
        std::fs::create_dir_all(p.join("dir")).expect("dir");
        std::fs::write(p.join("dir/other"), "other\n").expect("dir/other");
        assert_cli_success(&run_libra_command(&["add", "dir/other"], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base adds dir/other", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        assert_cli_success(&run_libra_command(&["rm", "old", "dir/other"], p), "drop");
        std::fs::write(p.join("dir"), "a\nb\nc\nd\ne\nf\ng\nh\n").expect("write dir");
        assert_cli_success(&run_libra_command(&["add", "dir"], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &[
                    "commit",
                    "-m",
                    "ours deletes dir/other and renames old to dir",
                    "--no-verify",
                ],
                p,
            ),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "dir/other"], p),
            "drop dir/other",
        );
        commit_file(
            p,
            "old",
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "theirs deletes dir/other and edits old",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        assert_eq!(
            std::fs::read_to_string(p.join("dir")).expect("merged file"),
            "a\nb edited\nc\nd\ne\nf\ng\nh\n",
            "walk {env:?}: the edit followed the rename"
        );
        let stages = index_stage_lines(p, "dir");
        assert_eq!(stages.len(), 1, "walk {env:?}: {stages:?}");
        assert!(stages[0].contains(" 0\t"), "walk {env:?}: {stages:?}");
        assert!(
            index_stage_lines(p, "old").is_empty(),
            "walk {env:?}: nothing is left at the old path"
        );
    }
}

/// The rename result is the same through every merge mode: the preview says
/// what the real merge does, `--squash` and `--no-commit` stage it, and
/// `--abort` after a conflicting rename restores the pre-merge state.
#[test]
fn merge_rename_is_consistent_across_dry_run_squash_and_no_commit() {
    let their = "line1\nline2 edited\nline3\nline4\nline5\nline6\nline7\nline8\n";

    // --dry-run: clean preview, nothing written.
    let repo = create_rename_repo(None, their);
    let p = repo.path();
    let head_before = head_commit(p);
    let preview = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_cli_success(&preview, "dry-run");
    let json = parse_json_stdout(&preview);
    assert_eq!(json["data"]["dry_run"], true);
    assert!(json["data"]["would_conflict"].is_null(), "{json}");
    assert_eq!(json["data"]["files_changed"], 1, "one path changed: {json}");
    assert_eq!(head_commit(p), head_before);
    assert!(!p.join(".libra").join("merge-state.json").exists());

    // --squash: the renamed result is staged, HEAD stays put.
    let repo = create_rename_repo(None, their);
    let p = repo.path();
    let head_before = head_commit(p);
    assert_cli_success(
        &run_libra_command(&["merge", "--squash", "feature"], p),
        "squash",
    );
    assert_eq!(head_commit(p), head_before, "squash never moves HEAD");
    assert_eq!(
        std::fs::read_to_string(p.join("new.txt")).expect("merged file"),
        their
    );

    // --no-commit: same result staged, finished with `--continue`.
    let repo = create_rename_repo(None, their);
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["merge", "--no-commit", "feature"], p),
        "no-commit",
    );
    assert_eq!(
        std::fs::read_to_string(p.join("new.txt")).expect("merged file"),
        their
    );
    // `--continue` reports the SAME `files_changed` the preview did: it compares
    // the pre-merge tree with the index, where a rename looks like a delete plus
    // an add, so without rename-aware counting it said 2 where the preview said
    // 1 (Codex R18).
    let finish = run_libra_command(&["--json", "merge", "--continue"], p);
    assert_cli_success(&finish, "continue");
    let json = parse_json_stdout(&finish);
    assert_eq!(
        json["data"]["files_changed"], 1,
        "the finalized merge counts the rename once, as the preview did: {json}"
    );
    let listing = run_libra_command(&["ls-tree", "-r", "HEAD"], p);
    assert_cli_success(&listing, "ls-tree");
    let listing = String::from_utf8_lossy(&listing.stdout).to_string();
    assert!(
        listing.contains("\tnew.txt") && !listing.contains("\told.txt"),
        "{listing}"
    );
}

/// G9 + G10: `--dry-run` reports a D/F collision as a conflict (exit 1) and the
/// JSON names its category, the path the file would move to, and the original.
#[test]
fn merge_dry_run_reports_a_df_conflict_with_its_kind() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    let head_before = head_commit(p);
    let out = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_eq!(out.status.code(), Some(1), "would-conflict preview exits 1");
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["dry_run"], true);
    assert_eq!(json["data"]["would_conflict"], true);
    assert_eq!(
        json["data"]["conflicted_paths"],
        serde_json::json!(["foo~HEAD"])
    );
    assert_eq!(
        json["data"]["conflict_kinds"],
        serde_json::json!([{"path": "foo~HEAD", "kind": "modify-delete", "original_path": "foo"}]),
        "an edited file under a directory is Git's modify/delete at the moved path: {json}"
    );
    assert_eq!(head_commit(p), head_before);
    assert!(!p.join("foo~HEAD").exists(), "a preview writes nothing");
    assert!(p.join("foo").is_file(), "the working tree is untouched");
    assert!(!p.join(".libra").join("merge-state.json").exists());

    // A one-sided add under a directory is the pure file/directory kind.
    let repo = create_df_conflict_repo(true, false);
    let p = repo.path();
    let out = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_eq!(out.status.code(), Some(1));
    let json = parse_json_stdout(&out);
    assert_eq!(
        json["data"]["conflict_kinds"],
        serde_json::json!([{"path": "foo~HEAD", "kind": "file-directory", "original_path": "foo"}]),
        "{json}"
    );
    assert!(!p.join("foo~HEAD").exists() && p.join("foo").is_file());
}

/// The category field also distinguishes ordinary conflicts.
#[test]
fn merge_dry_run_reports_content_conflict_kind() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    let out = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_eq!(out.status.code(), Some(1));
    let json = parse_json_stdout(&out);
    assert_eq!(
        json["data"]["conflict_kinds"],
        serde_json::json!([{"path": "shared.txt", "kind": "content"}]),
        "{json}"
    );
}

/// A D/F collision INSIDE the recursive fold (criss-cross whose two bases
/// disagree about `foo` being a file or a directory) is settled the way Git
/// settles it at `call_depth > 0`: the file moves to
/// `foo~Temporary merge branch N` in the virtual ancestor, so the outer merge
/// sees an ancestor that never had a file at `foo` — our re-created `foo`
/// survives as a one-sided add instead of being "deleted by theirs".
#[test]
fn merge_crisscross_df_collision_inside_the_fold_moves_the_file_like_git() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "base.txt", "root\n", "root");
    // Codex R3: names only the ROOT had — deleted by both folded sides — still
    // occupy the fold's temporary-branch names (Git's `unique_path` consults
    // every input), so the relocated file must take `…_0` in the ancestor.
    // Both labels are planted because the fold's ours/theirs order follows the
    // bases' ids.
    for label in ["1", "2"] {
        std::fs::write(
            p.join(format!("foo~Temporary merge branch {label}")),
            "gone\n",
        )
        .expect("planted name");
    }
    assert_cli_success(
        &run_libra_command(
            &[
                "add",
                "foo~Temporary merge branch 1",
                "foo~Temporary merge branch 2",
            ],
            p,
        ),
        "add planted",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "plant temporary names", "--no-verify"], p),
        "plant",
    );
    assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
    // a: file `foo`; b: directory `foo/`. (Switching between them goes through
    // `root`: `checkout` cannot flip a path between file and directory yet.)
    assert_cli_success(&run_libra_command(&["checkout", "-b", "a"], p), "a");
    assert_cli_success(
        &run_libra_command(
            &[
                "rm",
                "foo~Temporary merge branch 1",
                "foo~Temporary merge branch 2",
            ],
            p,
        ),
        "a drops the planted names",
    );
    commit_file(p, "foo", "file\n", "a: file foo");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    assert_cli_success(&run_libra_command(&["checkout", "-b", "b"], p), "b");
    assert_cli_success(
        &run_libra_command(
            &[
                "rm",
                "foo~Temporary merge branch 1",
                "foo~Temporary merge branch 2",
            ],
            p,
        ),
        "b drops the planted names",
    );
    std::fs::create_dir_all(p.join("foo")).expect("dir");
    std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
    assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "b: dir foo", "--no-verify"], p),
        "b commit",
    );
    // Criss-cross: x = a + b (D/F resolved by keeping foo~HEAD), y = b + a.
    for (from, tip, other, moved) in [("a", "x", "b", "foo~HEAD"), ("b", "y", "a", "foo~a")] {
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", from], p), "from");
        assert_cli_success(&run_libra_command(&["checkout", "-b", tip], p), "tip");
        // The criss-cross merges themselves hit the D/F collision.
        merge_expecting_conflict(p, &["merge", other], &[]);
        assert_cli_success(&run_libra_command(&["add", moved], p), "stage moved");
        assert_cli_success(&run_libra_command(&["merge", "--continue"], p), "continue");
    }
    // x': drop the directory and put the file back at `foo`.
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "x");
    assert_cli_success(&run_libra_command(&["rm", "foo/bar.txt"], p), "rm dir file");
    assert_cli_success(&run_libra_command(&["rm", "foo~HEAD"], p), "rm moved");
    let _ = std::fs::remove_dir(p.join("foo"));
    std::fs::write(p.join("foo"), "file\n").expect("file again");
    // x' also re-adds the planted names: against a correct ancestor (which
    // holds `…_0`, not these names) they are one-sided adds; against a wrong
    // one they would be modify/delete conflicts.
    for label in ["1", "2"] {
        std::fs::write(p.join(format!("foo~Temporary merge branch {label}")), "x\n")
            .expect("re-add");
    }
    assert_cli_success(
        &run_libra_command(
            &[
                "add",
                "foo",
                "foo~Temporary merge branch 1",
                "foo~Temporary merge branch 2",
            ],
            p,
        ),
        "stage",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "x: file foo again", "--no-verify"], p),
        "x commit",
    );
    // y': an unrelated edit so y is not an ancestor of x.
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "y"], p), "y");
    commit_file(p, "base.txt", "y\n", "y edit");
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "x");

    // MG-06: the ancestor holds the relocated file at `…_0` and BOTH sides
    // renamed it — ours back to `foo`, theirs to `foo~a` — which is Git's
    // rename/rename(1to2). MG-05 degraded that shape to "no rename detected"
    // and the outer merge came out clean; MG-06 raises the conflict Git raises
    // for it (measured on git 2.50.1, `/Volumes/Data/tmp/mg06-git/r1to2`).
    //
    // The conflict line NAMES the ancestor's path, so it is now the most
    // direct evidence of what this case has always guarded: that the fold took
    // `…_0` for its relocation rather than one of the planted names.
    let out = merge_expecting_conflict(p, &["merge", "y"], &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    // Which of the two planted labels the fold relocates from follows the
    // bases' ids, so only the `…_0` SUFFIX is deterministic — that suffix is
    // the whole point: the ancestor took a name neither planted path holds.
    assert!(
        stdout.contains("CONFLICT (rename/rename): foo~Temporary merge branch ")
            && stdout.contains("_0 renamed to foo in HEAD and to foo~a in y."),
        "the fold relocated to `…_0`, and both sides renamed it: {stdout}"
    );
    // Resolve it the way the pre-MG-06 result already looked — ours' `foo`,
    // theirs' `foo~a` — so the rest of the case still checks the merge the
    // fold produced.
    assert_cli_success(
        &run_libra_command(&["add", "foo", "foo~a"], p),
        "stage the rename/rename resolution",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--continue"], p),
        "the merge concludes",
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo")).expect("foo"),
        "file\n",
        "the file we re-created is a one-sided add against the virtual ancestor"
    );
    assert!(!p.join("foo").is_dir());
    assert_eq!(
        std::fs::read_to_string(p.join("foo~a")).expect("foo~a"),
        "file\n"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("base.txt")).expect("base"),
        "y\n"
    );
    for label in ["1", "2"] {
        assert_eq!(
            std::fs::read_to_string(p.join(format!("foo~Temporary merge branch {label}")))
                .expect("re-added name"),
            "x\n",
            "the fold took `…_0` for its relocation, leaving this name to x'"
        );
    }
    let parents = run_libra_command(&["cat-file", "-p", "HEAD"], p);
    assert_cli_success(&parents, "cat-file");
    assert_eq!(
        String::from_utf8_lossy(&parents.stdout)
            .lines()
            .filter(|l| l.starts_with("parent "))
            .count(),
        2
    );
}

// ---------------------------------------------------------------------------
// MG-06: path-level rename conflicts (git@3cb9185f6 `process_renames`,
// `merge-ort.c:2913-3232`). Every gate below is anchored to a measurement on
// git 2.50.1; the fixtures live under `/Volumes/Data/tmp/mg06-git/`.
// ---------------------------------------------------------------------------

/// Build the 1to2 fixture: `old` on the base, renamed to `a` by ours and to
/// `b` by theirs, each side editing the line it is given.
fn rename_1to2_repo(ours_line: usize, theirs_line: usize) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let base: String = (1..=8).map(|n| format!("l{n}\n")).collect();
    commit_file(p, "old", &base, "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    let edited = |line: usize, text: &str| -> String {
        (1..=8)
            .map(|n| {
                if n == line {
                    format!("{text}\n")
                } else {
                    format!("l{n}\n")
                }
            })
            .collect()
    };
    std::fs::remove_file(p.join("old")).expect("drop old");
    std::fs::write(p.join("a"), edited(ours_line, "OURS")).expect("a");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ours renames to a", "--no-verify"], p),
        "ours",
    );
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    std::fs::remove_file(p.join("old")).expect("drop old");
    std::fs::write(p.join("b"), edited(theirs_line, "THEIRS")).expect("b");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "theirs renames to b", "--no-verify"], p),
        "theirs",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    repo
}

/// The blob id `ls-files -s` printed for one stage line.
fn stage_blob(line: &str) -> String {
    line.split_whitespace().nth(1).expect("blob id").to_string()
}

/// G1/G2/G10: rename/rename(1to2). Git runs ONE content merge and copies the
/// result into BOTH destinations, leaves the merge base unmerged under the
/// ORIGINAL name — `merge-ort.c:3057-3068` spells out that keeping it there is
/// deliberate — and reports its own wording. Measured on git 2.50.1
/// (`/Volumes/Data/tmp/mg06-git/r1to2b`): `1 old`, `2 a`, `3 b` with `a` and
/// `b` holding the SAME blob, and a working tree holding only `a` and `b`.
#[test]
fn merge_rename_conflict_1to2_keeps_both_destinations_and_drops_the_source() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = rename_1to2_repo(2, 3);
        let p = repo.path();
        let out = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            stdout.contains(
                "CONFLICT (rename/rename): old renamed to a in HEAD and to b in feature."
            ),
            "walk {env:?}: Git's rename/rename wording, verbatim: {stdout}"
        );

        // INTENTIONAL DEVIATION from Git, documented in `apply_renames`: the
        // source is resolved by REMOVAL. Git leaves it unmerged at stage 1 and
        // its own comment (`merge-ort.c:3057-3068`) calls that legacy; keeping
        // it in Libra would put the source outside the ordinary documented
        // `libra add <path>` / `libra rm <path>` flow because those commands
        // match through stage 0. Plumbing or an index rebuild can resolve the
        // shape, but would discard or bypass ordinary staged resolutions.
        assert!(
            index_stage_lines(p, "old").is_empty(),
            "walk {env:?}: the source is resolved by removal, not left unmerged"
        );
        assert!(
            !p.join("old").exists(),
            "walk {env:?}: and no working-tree file is left at the source"
        );

        let a_stages = index_stage_lines(p, "a");
        let b_stages = index_stage_lines(p, "b");
        assert!(
            a_stages.len() == 1 && a_stages[0].contains(" 2\t"),
            "walk {env:?}: ours' destination is stage 2 alone: {a_stages:?}"
        );
        assert!(
            b_stages.len() == 1 && b_stages[0].contains(" 3\t"),
            "walk {env:?}: theirs' destination is stage 3 alone: {b_stages:?}"
        );
        assert_eq!(
            stage_blob(&a_stages[0]),
            stage_blob(&b_stages[0]),
            "walk {env:?}: ONE merge result is recorded at both destinations"
        );

        let merged = std::fs::read_to_string(p.join("a")).expect("a");
        assert_eq!(
            merged,
            std::fs::read_to_string(p.join("b")).expect("b"),
            "walk {env:?}: both working-tree files hold that same result"
        );
        assert!(
            merged.contains("OURS") && merged.contains("THEIRS"),
            "walk {env:?}: edits on different lines merge into it: {merged}"
        );
    }
}

/// G16: a rename-involved content merge widens Git's conflict markers by one
/// (`handle_content_merge` is called with `extra_marker_size = 1 + 2 *
/// call_depth`, `merge-ort.c:3027`) and labels them `<branch>:<path>` rather
/// than by branch alone. Measured on git 2.50.1
/// (`/Volumes/Data/tmp/mg06-git/r1to2c`, both sides editing line 2):
/// `<<<<<<<< HEAD:a` / `========` / `>>>>>>>> theirs:b`.
#[test]
fn merge_rename_conflict_1to2_marks_up_with_wide_labelled_markers() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = rename_1to2_repo(2, 2);
        let p = repo.path();
        merge_expecting_conflict(p, &["merge", "feature"], env);
        let merged = std::fs::read_to_string(p.join("a")).expect("a");
        // Compared as WHOLE LINES: an eight-character run contains a
        // seven-character one, so `contains` cannot tell the widened marker
        // from the ordinary one.
        let marker_line = |lead: char| -> String {
            merged
                .lines()
                .find(|line| line.starts_with(lead))
                .unwrap_or_else(|| panic!("walk {env:?}: no {lead} marker in {merged}"))
                .to_string()
        };
        assert_eq!(
            marker_line('<'),
            "<<<<<<<< HEAD:a",
            "walk {env:?}: eight characters, labelled <branch>:<path>: {merged}"
        );
        assert_eq!(
            marker_line('='),
            "========",
            "walk {env:?}: the separator is widened too: {merged}"
        );
        assert_eq!(
            marker_line('>'),
            ">>>>>>>> feature:b",
            "walk {env:?}: the other side is labelled by its own path: {merged}"
        );
        assert_eq!(
            merged,
            std::fs::read_to_string(p.join("b")).expect("b"),
            "walk {env:?}: the conflicted result is copied to both destinations"
        );
    }
}

/// G7/G8: rename/delete, in BOTH directions, for a PURE rename — Git keeps it
/// a conflict even though the content never changed, moving the merge base to
/// the NEW path's stage 1 and leaving the renaming side's content beside it
/// while the deleting side contributes no stage (`merge-ort.c:3202-3221`).
/// Measured on git 2.50.1: `/Volumes/Data/tmp/mg06-git/rdel2` gives `1 new` +
/// `2 new`, `rdel3` gives `1 new` + `3 new`, with the wording flipped.
#[test]
fn merge_rename_conflict_rename_delete_reports_git_wording_both_directions() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        for ours_renames in [true, false] {
            let repo = create_committed_repo_via_cli();
            let p = repo.path();
            let base: String = (1..=8).map(|n| format!("l{n}\n")).collect();
            commit_file(p, "old", &base, "base");
            assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
            let rename_here = |p: &Path| {
                std::fs::rename(p.join("old"), p.join("new")).expect("rename");
            };
            let delete_here = |p: &Path| {
                std::fs::remove_file(p.join("old")).expect("delete");
            };
            if ours_renames {
                rename_here(p);
            } else {
                delete_here(p);
            }
            assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
            assert_cli_success(
                &run_libra_command(&["commit", "-m", "ours", "--no-verify"], p),
                "ours",
            );
            assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
            if ours_renames {
                delete_here(p);
            } else {
                rename_here(p);
            }
            assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
            assert_cli_success(
                &run_libra_command(&["commit", "-m", "theirs", "--no-verify"], p),
                "theirs",
            );
            assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

            let out = merge_expecting_conflict(p, &["merge", "feature"], env);
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let (renamer, deleter) = if ours_renames {
                ("HEAD", "feature")
            } else {
                ("feature", "HEAD")
            };
            assert!(
                stdout.contains(&format!(
                    "CONFLICT (rename/delete): old renamed to new in {renamer}, but deleted in {deleter}."
                )),
                "walk {env:?} ours_renames={ours_renames}: Git's wording, verbatim: {stdout}"
            );
            let stages = index_stage_lines(p, "new");
            let side_stage = if ours_renames { " 2\t" } else { " 3\t" };
            let absent_stage = if ours_renames { " 3\t" } else { " 2\t" };
            assert!(
                stages.iter().any(|line| line.contains(" 1\t")),
                "walk {env:?} ours_renames={ours_renames}: the base follows the rename: {stages:?}"
            );
            assert!(
                stages.iter().any(|line| line.contains(side_stage)),
                "walk {env:?} ours_renames={ours_renames}: the renaming side keeps its content: {stages:?}"
            );
            assert!(
                !stages.iter().any(|line| line.contains(absent_stage)),
                "walk {env:?} ours_renames={ours_renames}: the deleting side has no stage: {stages:?}"
            );
            assert!(
                index_stage_lines(p, "old").is_empty(),
                "walk {env:?} ours_renames={ours_renames}: the source is resolved by removal"
            );
        }
    }
}

/// G5/G6: rename/add. `merge-ort.c` has NO `CONFLICT (rename/add)` string at
/// all — the collision branch (`:3137-3179`) merges the rename itself first,
/// parks that result at the renaming side's stage of the destination, leaves
/// the other side's add at its own stage, and records NO base there, so the
/// destination lands as a base-less add/add. Measured on git 2.50.1
/// (`/Volumes/Data/tmp/mg06-git/radd2`, where theirs ALSO edits the source):
/// stage 2 carries that edit, stage 3 is theirs' independent add.
#[test]
fn merge_rename_conflict_rename_add_carries_the_rename_merge_into_its_stage() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        let base: String = (1..=8).map(|n| format!("l{n}\n")).collect();
        commit_file(p, "old", &base, "base");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        std::fs::rename(p.join("old"), p.join("new")).expect("rename");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ours renames", "--no-verify"], p),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        // theirs edits the SOURCE and independently adds the destination.
        let edited: String = (1..=8)
            .map(|n| {
                if n == 4 {
                    "THEIRS\n".to_string()
                } else {
                    format!("l{n}\n")
                }
            })
            .collect();
        std::fs::write(p.join("old"), &edited).expect("old");
        std::fs::write(p.join("new"), "theirs own file\n").expect("new");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs edits and adds", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            !stdout.contains("rename/add"),
            "walk {env:?}: Git has no such conflict name: {stdout}"
        );
        let stages = index_stage_lines(p, "new");
        assert!(
            !stages.iter().any(|line| line.contains(" 1\t")),
            "walk {env:?}: a collision records no merge base: {stages:?}"
        );
        assert!(
            stages.iter().any(|line| line.contains(" 2\t"))
                && stages.iter().any(|line| line.contains(" 3\t")),
            "walk {env:?}: both sides are kept: {stages:?}"
        );
        // Stage 2 is the rename's own merge, so it carries THEIRS' edit of the
        // source even though ours only moved the file.
        let stage2 = stages
            .iter()
            .find(|line| line.contains(" 2\t"))
            .expect("stage 2");
        let out = run_libra_command(&["cat-file", "-p", &stage_blob(stage2)], p);
        assert_cli_success(&out, "cat-file");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("THEIRS"),
            "walk {env:?}: the rename carried the other side's edit to the new path"
        );
        assert!(
            index_stage_lines(p, "old").is_empty() && !p.join("old").exists(),
            "walk {env:?}: Git resolves the rename source by removal"
        );
    }
}

/// G15: the collision branch speaks up ONLY when the rename's OWN content
/// merge came out unclean — `merge-ort.c:3169-3178`. Measured on git 2.50.1
/// (`/Volumes/Data/tmp/mg06-git/rcoll`): both the collision line and the
/// destination's add/add are printed.
#[test]
fn merge_rename_conflict_collision_announces_when_the_rename_merge_is_dirty() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        let base: String = (1..=8).map(|n| format!("l{n}\n")).collect();
        commit_file(p, "old", &base, "base");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        let edited = |text: &str| -> String {
            (1..=8)
                .map(|n| {
                    if n == 2 {
                        format!("{text}\n")
                    } else {
                        format!("l{n}\n")
                    }
                })
                .collect()
        };
        std::fs::remove_file(p.join("old")).expect("drop");
        std::fs::write(p.join("new"), edited("OURS")).expect("new");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "ours renames and edits", "--no-verify"],
                p,
            ),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::write(p.join("old"), edited("THEIRS")).expect("old");
        std::fs::write(p.join("new"), "theirs own file\n").expect("new");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs edits and adds", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            stdout.contains(
                "CONFLICT (rename involved in collision): rename of old -> new has content conflicts AND collides with another path; this may result in nested conflict markers."
            ),
            "walk {env:?}: Git's collision wording, verbatim: {stdout}"
        );
    }
}

/// G3/G4: rename/rename(2to1) — two DIFFERENT sources renamed onto one name.
/// Git treats each rename as its own collision (`merge-ort.c:3137-3179`), so
/// each side's stage at the destination holds that side's rename merged
/// against the other side's copy of ITS source, and no merge base is recorded.
/// Measured on git 2.50.1 (`/Volumes/Data/tmp/mg06-git/r2to1b`): stages 2 and
/// 3 only, each carrying the other side's edit of the matching source.
#[test]
fn merge_rename_conflict_2to1_merges_each_source_into_its_own_stage() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        let body = |tag: &str| -> String { (1..=8).map(|n| format!("{tag}{n}\n")).collect() };
        let edited = |tag: &str, text: &str| -> String {
            (1..=8)
                .map(|n| {
                    if n == 3 {
                        format!("{text}\n")
                    } else {
                        format!("{tag}{n}\n")
                    }
                })
                .collect()
        };
        std::fs::write(p.join("o1"), body("a")).expect("o1");
        std::fs::write(p.join("o2"), body("b")).expect("o2");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        // ours: o1 -> new, and an edit to o2 (the source theirs will move).
        std::fs::rename(p.join("o1"), p.join("new")).expect("rename o1");
        std::fs::write(p.join("o2"), edited("b", "OURS")).expect("o2");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ours moves o1", "--no-verify"], p),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        // theirs: o2 -> new, and an edit to o1 (the source ours moved).
        std::fs::rename(p.join("o2"), p.join("new")).expect("rename o2");
        std::fs::write(p.join("o1"), edited("a", "THEIRS")).expect("o1");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs moves o2", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            !stdout.contains("rename/rename"),
            "walk {env:?}: 2to1 is a collision, not Git's rename/rename line: {stdout}"
        );
        let stages = index_stage_lines(p, "new");
        assert!(
            !stages.iter().any(|line| line.contains(" 1\t")),
            "walk {env:?}: a collision records no merge base: {stages:?}"
        );
        let read_stage = |needle: &str| -> String {
            let line = stages
                .iter()
                .find(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("walk {env:?}: stage {needle} missing: {stages:?}"));
            let out = run_libra_command(&["cat-file", "-p", &stage_blob(line)], p);
            assert_cli_success(&out, "cat-file");
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        assert!(
            read_stage(" 2\t").contains("THEIRS"),
            "walk {env:?}: ours' stage merged theirs' edit of o1 into the destination"
        );
        assert!(
            read_stage(" 3\t").contains("OURS"),
            "walk {env:?}: theirs' stage merged ours' edit of o2 into the destination"
        );
        assert!(
            index_stage_lines(p, "o1").is_empty() && index_stage_lines(p, "o2").is_empty(),
            "walk {env:?}: both sources are resolved by removal"
        );
    }
}

/// G14: rename/rename(1to1) is NOT a conflict. Both sides moving the same file
/// to the same name lets Git carry the merge base to that name
/// (`merge-ort.c:2991-3018`) and merge the two contents there normally — so
/// edits on different lines come out CLEAN. Before MG-06 Libra declined the
/// pair and the destination degraded to a base-less add/add, which turned a
/// clean merge into a conflict and lost the base. Measured on git 2.50.1
/// (`/Volumes/Data/tmp/mg06-git/r1to1`).
#[test]
fn merge_rename_conflict_1to1_merges_cleanly_with_the_base_carried_over() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        for same_line in [false, true] {
            let repo = create_committed_repo_via_cli();
            let p = repo.path();
            let base: String = (1..=8).map(|n| format!("l{n}\n")).collect();
            commit_file(p, "old", &base, "base");
            assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
            let edited = |line: usize, text: &str| -> String {
                (1..=8)
                    .map(|n| {
                        if n == line {
                            format!("{text}\n")
                        } else {
                            format!("l{n}\n")
                        }
                    })
                    .collect()
            };
            std::fs::remove_file(p.join("old")).expect("drop");
            std::fs::write(p.join("new"), edited(2, "OURS")).expect("new");
            assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
            assert_cli_success(
                &run_libra_command(&["commit", "-m", "ours moves and edits", "--no-verify"], p),
                "ours",
            );
            assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
            std::fs::remove_file(p.join("old")).expect("drop");
            let their_line = if same_line { 2 } else { 6 };
            std::fs::write(p.join("new"), edited(their_line, "THEIRS")).expect("new");
            assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
            assert_cli_success(
                &run_libra_command(
                    &["commit", "-m", "theirs moves and edits", "--no-verify"],
                    p,
                ),
                "theirs",
            );
            assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

            if same_line {
                // The base IS carried over, so this is an ordinary content
                // conflict at the new path — stage 1 proves the base arrived.
                merge_expecting_conflict(p, &["merge", "feature"], env);
                let stages = index_stage_lines(p, "new");
                assert!(
                    stages.iter().any(|line| line.contains(" 1\t")),
                    "walk {env:?}: the base followed the agreed rename: {stages:?}"
                );
                assert!(
                    stages.iter().any(|line| line.contains(" 2\t"))
                        && stages.iter().any(|line| line.contains(" 3\t")),
                    "walk {env:?}: both sides are staged: {stages:?}"
                );
            } else {
                let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
                assert_cli_success(&out, "walk {env:?}: an agreed rename merges cleanly");
                let merged = std::fs::read_to_string(p.join("new")).expect("new");
                assert!(
                    merged.contains("OURS") && merged.contains("THEIRS"),
                    "walk {env:?}: both edits survive on the shared destination: {merged}"
                );
                assert!(!p.join("old").exists(), "walk {env:?}: the source is gone");
            }
        }
    }
}

/// G12: `--abort` restores the pre-merge state for every rename shape — the
/// two destinations of a 1to2 go back to what each side had, the source
/// reappears where HEAD had it (nowhere: ours renamed it away), and no merge
/// state survives.
#[test]
fn merge_rename_conflict_abort_restores_the_pre_merge_state() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        // Codex R1 P2-4: every conflicting shape, not just 1to2.
        for (label, build, conflicts) in rename_shape_table() {
            if !conflicts {
                continue;
            }
            let repo = build();
            let p = repo.path();
            let head_before = run_libra_command(&["rev-parse", "HEAD"], p);
            assert_cli_success(&head_before, "rev-parse");
            merge_expecting_conflict(p, &["merge", "feature"], env);
            assert_cli_success(
                &run_libra_command_with_stdin_and_env(&["merge", "--abort"], p, "", env),
                "abort",
            );
            let left = unmerged_stage_lines(p);
            assert!(
                left.is_empty(),
                "walk {env:?} / {label}: no unmerged entry survives the abort: {left:?}"
            );
            let head_after = run_libra_command(&["rev-parse", "HEAD"], p);
            assert_cli_success(&head_after, "rev-parse");
            assert_eq!(
                head_before.stdout, head_after.stdout,
                "walk {env:?} / {label}: HEAD is back where it was"
            );
        }
        let repo = rename_1to2_repo(2, 3);
        let p = repo.path();
        let before = std::fs::read_to_string(p.join("a")).expect("a before");
        merge_expecting_conflict(p, &["merge", "feature"], env);
        assert_cli_success(
            &run_libra_command_with_stdin_and_env(&["merge", "--abort"], p, "", env),
            "abort",
        );
        assert_eq!(
            std::fs::read_to_string(p.join("a")).expect("a after"),
            before,
            "walk {env:?}: ours' destination is back to HEAD's content"
        );
        assert!(
            !p.join("b").exists(),
            "walk {env:?}: theirs' destination is gone again"
        );
        assert!(
            !p.join("old").exists(),
            "walk {env:?}: the source stays where HEAD had it — nowhere"
        );
        // `index_stage_lines` returns EVERY stage line for the path, stage 0
        // included — ours' own entry legitimately survives the abort.
        let a_stages = index_stage_lines(p, "a");
        assert!(
            a_stages.len() == 1 && a_stages[0].contains(" 0\t"),
            "walk {env:?}: ours' file is back as an ordinary entry: {a_stages:?}"
        );
        assert!(
            index_stage_lines(p, "b").is_empty() && index_stage_lines(p, "old").is_empty(),
            "walk {env:?}: nothing the merge introduced survives the abort"
        );
    }
}

/// G11: `--continue` finishes a rename conflict once the paths are staged —
/// including the source Git leaves unmerged under its old name, which has to
/// stop being unmerged before the merge can conclude.
#[test]
fn merge_rename_conflict_continue_finishes_the_1to2() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        // Codex R1 P2-4: every conflicting shape, not just 1to2.
        for (label, build, conflicts) in rename_shape_table() {
            if !conflicts {
                continue;
            }
            let repo = build();
            let p = repo.path();
            merge_expecting_conflict(p, &["merge", "feature"], env);
            // Every conflicted path of every shape exists on disk, so `add -A`
            // is the whole resolution — which is exactly why no shape may leave
            // an unmerged entry at a path that exists nowhere.
            assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
            let out = run_libra_command_with_stdin_and_env(&["merge", "--continue"], p, "", env);
            assert_cli_success(&out, "walk {env:?} / {label}: the merge concludes");
            let left = unmerged_stage_lines(p);
            assert!(
                left.is_empty(),
                "walk {env:?} / {label}: nothing is left unmerged: {left:?}"
            );
            let parents = run_libra_command(&["cat-file", "-p", "HEAD"], p);
            assert_cli_success(&parents, "cat-file");
            assert_eq!(
                String::from_utf8_lossy(&parents.stdout)
                    .lines()
                    .filter(|line| line.starts_with("parent "))
                    .count(),
                2,
                "walk {env:?} / {label}: a two-parent merge commit was recorded"
            );
        }
        let repo = rename_1to2_repo(2, 3);
        let p = repo.path();
        merge_expecting_conflict(p, &["merge", "feature"], env);
        // Both destinations exist on disk, so staging them is the whole
        // resolution — which is exactly why the source must not be left
        // unmerged: nothing could stage a path that exists nowhere.
        assert_cli_success(
            &run_libra_command(&["add", "-A", "."], p),
            "stage the resolution",
        );
        let out = run_libra_command_with_stdin_and_env(&["merge", "--continue"], p, "", env);
        assert_cli_success(&out, "walk {env:?}: the merge concludes");
        assert!(
            index_stage_lines(p, "a")
                .iter()
                .all(|line| line.contains(" 0\t")),
            "walk {env:?}: nothing is left unmerged"
        );
        let log = run_libra_command(&["log", "--oneline", "-1"], p);
        assert_cli_success(&log, "log");
        assert!(
            !String::from_utf8_lossy(&log.stdout).is_empty(),
            "walk {env:?}: a merge commit was recorded"
        );
    }
}

/// G13: `--dry-run` names the conflict kinds without writing, and `--json`
/// keeps stdout machine-clean while doing so.
#[test]
fn merge_rename_conflict_dry_run_reports_the_kind_without_writing() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        // Every shape previews the verdict it would really reach, and writes
        // nothing doing it (Codex R1 P2-4: these gates used to test 1to2 only).
        for (label, build, conflicts) in rename_shape_table() {
            let repo = build();
            let p = repo.path();
            let head_before = run_libra_command(&["rev-parse", "HEAD"], p);
            assert_cli_success(&head_before, "rev-parse");
            let preview = run_libra_command_with_stdin_and_env(
                &["merge", "--dry-run", "--json", "feature"],
                p,
                "",
                env,
            );
            assert_eq!(
                preview.status.code(),
                Some(if conflicts { 1 } else { 0 }),
                "walk {env:?} / {label}: the preview reaches the real verdict"
            );
            let parsed = parse_json_stdout(&preview);
            assert_eq!(parsed["data"]["dry_run"], true, "{label}: {parsed}");
            if conflicts {
                assert_eq!(parsed["data"]["would_conflict"], true, "{label}: {parsed}");
                assert_eq!(
                    parsed["data"]["conflict_kinds"],
                    rename_matrix::expected_conflict_kinds(label),
                    "walk {env:?} / {label}: exact conflicted paths and kinds: {parsed}"
                );
            } else {
                // A clean preview carries NEITHER key — the documented frozen
                // schema, which this now pins for a shape that merges.
                assert!(
                    parsed["data"]["would_conflict"].is_null()
                        && parsed["data"]["conflict_kinds"].is_null(),
                    "{label}: a clean preview omits the conflict keys: {parsed}"
                );
            }
            // A preview writes nothing: HEAD, the index and the merge state
            // are all untouched.
            let head_after = run_libra_command(&["rev-parse", "HEAD"], p);
            assert_cli_success(&head_after, "rev-parse");
            assert_eq!(
                head_before.stdout, head_after.stdout,
                "{label}: a preview does not move HEAD"
            );
            assert!(
                unmerged_stage_lines(p).is_empty(),
                "{label}: a preview leaves no unmerged entry"
            );
        }
        let repo = rename_1to2_repo(2, 3);
        let p = repo.path();
        let before = std::fs::read_to_string(p.join("a")).expect("a before");
        let preview =
            run_libra_command_with_stdin_and_env(&["merge", "--dry-run", "feature"], p, "", env);
        assert_eq!(
            preview.status.code(),
            Some(1),
            "walk {env:?}: a would-conflict preview exits 1"
        );
        let stdout = String::from_utf8_lossy(&preview.stdout).to_string();
        assert!(
            stdout.contains(
                "CONFLICT (rename/rename): old renamed to a in HEAD and to b in feature."
            ),
            "walk {env:?}: the preview says what the merge would say: {stdout}"
        );
        assert!(
            stdout.contains("Would conflict in: a, b"),
            "walk {env:?}: both destinations are previewed: {stdout}"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("a")).expect("a after"),
            before,
            "walk {env:?}: a preview writes nothing"
        );
        assert!(
            !p.join("b").exists(),
            "walk {env:?}: a preview creates no destination"
        );

        let json = run_libra_command_with_stdin_and_env(
            &["merge", "--dry-run", "--json", "feature"],
            p,
            "",
            env,
        );
        assert_eq!(json.status.code(), Some(1), "walk {env:?}: same verdict");
        let json_out = String::from_utf8_lossy(&json.stdout).to_string();
        assert!(
            !json_out.contains("CONFLICT (") && !json_out.contains("notice:"),
            "walk {env:?}: json stdout stays machine-clean: {json_out}"
        );
        let parsed = parse_json_stdout(&json);
        assert_eq!(parsed["data"]["dry_run"], true, "walk {env:?}: {parsed}");
        assert_eq!(
            parsed["data"]["would_conflict"], true,
            "walk {env:?}: {parsed}"
        );
        // The machine surface carries the kind too, not just the human line —
        // `rename-rename` is a NEW enum value and has to be pinned where
        // clients actually read it (Codex R1 P1-5).
        assert_eq!(
            parsed["data"]["conflict_kinds"],
            serde_json::json!([
                {"path": "a", "kind": "rename-rename"},
                {"path": "b", "kind": "rename-rename"},
            ]),
            "walk {env:?}: {parsed}"
        );
    }
}

/// G18: a collision whose rename merge is ALSO unclean produces NESTED markers
/// — the rename's own conflicted result becomes one side of the destination's
/// add/add, so the file carries an 8-character region inside a 7-character one.
/// That is the literal meaning of Git's warning "this may result in nested
/// conflict markers", which MG-06 quotes verbatim; measured on git 2.50.1 under
/// `merge.conflictStyle=diff3` (`/Volumes/Data/tmp/mg06-sweep.sh`):
///
/// ```text
/// <<<<<<< HEAD              (7, the outer add/add)
/// <<<<<<<< HEAD:new         (8, the rename's own merge)
/// |||||||| <ancestor>:old
/// ========
/// >>>>>>>> feature:old
/// ||||||| <ancestor>
/// =======
/// >>>>>>> feature
/// ```
///
/// Libra nests the same way, with two documented differences: it writes the
/// ancestor label as `base:<path>` (its own diff3 convention, where Git names
/// the ancestor commit), and the OUTER markers come out longer than Git's seven
/// — Libra additionally requires a marker to be longer than any marker-like run
/// in its inputs (`unambiguous_conflict_marker_length`, an extension MG-02
/// registered), and the nested eight-character run is such an input. Measured:
/// Git 7 outside / 8 inside, Libra 9 outside / 8 inside. The STRUCTURE — which
/// side carries which path, and the inner region sitting wholly inside the
/// outer one — is identical, and that is what this gate asserts.
#[test]
fn merge_rename_conflict_collision_nests_the_rename_merge_markers() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        let base: String = (1..=8).map(|n| format!("l{n}\n")).collect();
        commit_file(p, "old", &base, "base");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        let edited = |text: &str| -> String {
            (1..=8)
                .map(|n| {
                    if n == 2 {
                        format!("{text}\n")
                    } else {
                        format!("l{n}\n")
                    }
                })
                .collect()
        };
        // ours renames AND edits; theirs edits the source differently AND adds
        // the destination — so the rename's own merge conflicts and then
        // collides.
        std::fs::remove_file(p.join("old")).expect("drop");
        std::fs::write(p.join("new"), edited("OURS")).expect("new");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "ours renames and edits", "--no-verify"],
                p,
            ),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::write(p.join("old"), edited("THEIRS")).expect("edit source");
        std::fs::write(p.join("new"), "theirs own file\n").expect("add destination");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs edits and adds", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        assert_cli_success(
            &run_libra_command(&["config", "merge.conflictStyle", "diff3"], p),
            "diff3",
        );

        merge_expecting_conflict(p, &["merge", "feature"], env);
        let body = std::fs::read_to_string(p.join("new")).expect("the conflicted destination");
        let target = run_libra_command(&["rev-parse", "feature"], p);
        assert_cli_success(&target, "read the outer conflict's target label");
        let target_id = String::from_utf8(target.stdout).expect("target object id is ASCII");
        let target_abbrev: String = target_id.trim().chars().take(7).collect();
        // The whole rename result, including every context line, belongs to
        // the outer ours arm. Exact bytes pin both complete diff3 regions:
        // matching open/base/separator/close widths, labels, and one outer
        // block. Git uses seven outside; MG-02's documented rule requires
        // Libra's outer markers to be longer than the nested eight. The outer
        // writer uses the target commit's abbreviation, as for plain conflicts.
        let renamed = concat!(
            "l1\n",
            "<<<<<<<< HEAD:new\n",
            "OURS\n",
            "|||||||| base:old\n",
            "l2\n",
            "========\n",
            "THEIRS\n",
            ">>>>>>>> feature:old\n",
            "l3\nl4\nl5\nl6\nl7\nl8\n",
        );
        assert_eq!(
            body,
            format!(
                "<<<<<<<<< HEAD\n{renamed}||||||||| base\n=========\ntheirs own file\n>>>>>>>>> {target_abbrev}\n"
            ),
            "walk {env:?}: one complete outer add/add contains the complete rename conflict"
        );
    }
}

/// G17 (Codex R2 P2-1): `-X ours` / `-X theirs` reach the rename's OWN content
/// merge. Git maps the variant onto `ll_opts.variant` (`merge-ort.c:2129-2143`)
/// so the favoured side settles every hunk — while the PATH-level rename
/// conflict itself survives, because `-X` only settles content. Measured on
/// git 2.50.1: `git merge -X ours` on this shape prints
/// `CONFLICT (rename/rename)` and leaves the OURS line at both destinations.
///
/// This gate exists because the first attempt at the `-X` support passed the
/// user's configured (two-marker) conflict style to the favoured resolver,
/// which looks for `|||||||` — so `-X` on any text rename conflict died with
/// `LBR-IO-002 ... malformed conflict markers`, and no gate noticed.
#[test]
fn merge_rename_conflict_strategy_option_settles_the_rename_content_merge() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        for (favor, expected) in [("ours", "OURS"), ("theirs", "THEIRS")] {
            // rename/rename(1to2) with BOTH sides editing the same line, so the
            // rename's own content merge really does conflict.
            let repo = rename_1to2_repo(2, 2);
            let p = repo.path();
            let out = merge_expecting_conflict(p, &["merge", "-X", favor, "feature"], env);
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            assert!(
                stdout.contains("CONFLICT (rename/rename):"),
                "walk {env:?} / -X {favor}: the path-level conflict survives `-X`: {stdout}"
            );
            for destination in ["a", "b"] {
                let body = std::fs::read_to_string(p.join(destination))
                    .unwrap_or_else(|_| panic!("walk {env:?} / -X {favor}: {destination}"));
                assert!(
                    body.contains(expected),
                    "walk {env:?} / -X {favor}: {destination} takes the favoured side: {body}"
                );
                assert!(
                    !body.contains("<<<<<<<") && !body.contains("|||||||"),
                    "walk {env:?} / -X {favor}: {destination} keeps no marker of any style: {body}"
                );
            }
        }

        // The collision branch runs the same content merge, so `-X` reaches it
        // too — and there `-X` settles the destination's add/add as well, so
        // the whole merge comes out CLEAN. Measured on git 2.50.1
        // (`/Volumes/Data/tmp/mg06-xc.sh`): `Merge made by the 'ort' strategy`,
        // no unmerged entries, and the blob ids asserted below are Git's own.
        for (favor, expected_blob) in [
            ("ours", "5088f08d9902bc42a1cbf78a7a40e4362f8c4e5f"),
            ("theirs", "0deff81a0a4001538d3f973b2cf8cf012b591f1c"),
        ] {
            let repo = create_committed_repo_via_cli();
            let p = repo.path();
            let base: String = (1..=8).map(|n| format!("l{n}\n")).collect();
            commit_file(p, "old", &base, "base");
            assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
            std::fs::rename(p.join("old"), p.join("new")).expect("rename");
            assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
            assert_cli_success(
                &run_libra_command(&["commit", "-m", "ours renames", "--no-verify"], p),
                "ours",
            );
            assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
            let edited: String = (1..=8)
                .map(|n| {
                    if n == 2 {
                        "THEIRS\n".to_string()
                    } else {
                        format!("l{n}\n")
                    }
                })
                .collect();
            std::fs::write(p.join("old"), &edited).expect("edit source");
            std::fs::write(p.join("new"), "theirs own file\n").expect("add destination");
            assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
            assert_cli_success(
                &run_libra_command(&["commit", "-m", "theirs edits and adds", "--no-verify"], p),
                "theirs",
            );
            assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

            let out = run_libra_command_with_stdin_and_env(
                &["merge", "-X", favor, "feature"],
                p,
                "",
                env,
            );
            assert_cli_success(
                &out,
                "walk {env:?} / -X {favor}: `-X` settles the collision outright, as Git's does",
            );
            let stages = index_stage_lines(p, "new");
            assert_eq!(
                stages.len(),
                1,
                "walk {env:?} / -X {favor}: nothing is left unmerged: {stages:?}"
            );
            assert!(
                stages[0].contains(" 0\t") && stages[0].contains(expected_blob),
                "walk {env:?} / -X {favor}: the result is Git's own blob: {stages:?}"
            );
        }
    }
}

/// Codex R1 P2-5: the virtual-ancestor fold runs MG-06's shapes too, and the
/// fold has no conflicts of its own — every shape has to settle as CONTENT
/// there. A criss-cross whose two merge bases disagree about a renamed file is
/// the case that exercises it: the fold must produce ONE ancestor both walks
/// then merge against identically, on the default (pruned) walk and the
/// flattening one alike.
#[test]
fn merge_rename_conflict_inside_the_fold_agrees_across_both_walks() {
    let mut results = Vec::new();
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        let base: String = (1..=8).map(|n| format!("l{n}\n")).collect();
        commit_file(p, "old", &base, "base file");
        assert_cli_success(&run_libra_command(&["branch", "a"], p), "a");
        assert_cli_success(&run_libra_command(&["branch", "b"], p), "b");
        // a renames the file; b edits it in place — the two merge bases of the
        // criss-cross below therefore disagree about where it lives.
        assert_cli_success(&run_libra_command(&["checkout", "a"], p), "a");
        std::fs::rename(p.join("old"), p.join("moved")).expect("rename");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "a renames", "--no-verify"], p),
            "a",
        );
        assert_cli_success(&run_libra_command(&["checkout", "b"], p), "b");
        let edited: String = (1..=8)
            .map(|n| {
                if n == 7 {
                    "b edit\n".to_string()
                } else {
                    format!("l{n}\n")
                }
            })
            .collect();
        std::fs::write(p.join("old"), &edited).expect("edit");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "b edits", "--no-verify"], p),
            "b",
        );
        // Two tips, each merging both bases — the fold has two bases to settle.
        for (from, other, tip) in [("a", "b", "x"), ("b", "a", "y")] {
            assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
            assert_cli_success(&run_libra_command(&["checkout", from], p), "from");
            assert_cli_success(&run_libra_command(&["checkout", "-b", tip], p), "tip");
            let merged = run_libra_command_with_stdin_and_env(&["merge", other], p, "", env);
            assert_cli_success(&merged, "the criss-cross arm merges");
        }
        assert_cli_success(&run_libra_command(&["checkout", "x"], p), "x");
        let out = run_libra_command_with_stdin_and_env(
            &["merge", "--dry-run", "--json", "y"],
            p,
            "",
            env,
        );
        let mut parsed = parse_json_stdout(&out);
        let data = parsed["data"]
            .as_object_mut()
            .expect("the envelope carries a data object");
        data.remove("old_commit");
        data.remove("commit");
        results.push(parsed);
    }
    assert_eq!(
        results[0], results[1],
        "the fold's ancestor makes both walks reach the same verdict"
    );
}

/// Every shape, with whether the merge it produces CONFLICTS. `1to1 clean` is
/// the one that does not — MG-06 turned it from a degraded add/add into a real
/// three-way that merges. Drives the lifecycle gates (Codex R1 P2-4).
#[allow(clippy::type_complexity)]
fn rename_shape_table() -> Vec<(&'static str, Box<dyn Fn() -> tempfile::TempDir>, bool)> {
    vec![
        (
            "1to2 clean content",
            Box::new(|| rename_1to2_repo(2, 3)) as Box<dyn Fn() -> tempfile::TempDir>,
            true,
        ),
        (
            "1to2 conflicting content",
            Box::new(|| rename_1to2_repo(2, 2)),
            true,
        ),
        (
            "rename/delete, ours renames",
            Box::new(|| rename_delete_repo(true)),
            true,
        ),
        (
            "rename/delete, theirs renames",
            Box::new(|| rename_delete_repo(false)),
            true,
        ),
        ("rename/add", Box::new(rename_add_repo), true),
        ("2to1", Box::new(rename_matrix::rename_2to1_repo), true),
        ("1to1 clean", Box::new(|| rename_1to1_repo(2, 6)), false),
        (
            "1to1 conflicting",
            Box::new(|| rename_1to1_repo(2, 2)),
            true,
        ),
    ]
}

mod dir_rename;
mod rename_binary;
mod rename_fold;
mod rename_fold_binary;
mod rename_interactions;
mod rename_matrix;
mod rename_path_collisions;
mod squash;

/// Every unmerged stage line in the index, for the "nothing is left unmerged"
/// assertions the lifecycle gates share.
fn unmerged_stage_lines(p: &Path) -> Vec<String> {
    let out = run_libra_command(&["ls-files", "-s"], p);
    assert_cli_success(&out, "ls-files -s");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.contains(" 0\t"))
        .map(|line| line.to_string())
        .collect()
}

/// Every path-level rename shape MG-06 settles, as `(label, builder)`. Used by
/// the cross-engine parity case so each shape is compared field by field.
#[allow(clippy::type_complexity)]
fn rename_conflict_shapes() -> Vec<(&'static str, Box<dyn Fn() -> tempfile::TempDir>)> {
    vec![
        (
            "1to2 clean content",
            Box::new(|| rename_1to2_repo(2, 3)) as Box<dyn Fn() -> tempfile::TempDir>,
        ),
        (
            "1to2 conflicting content",
            Box::new(|| rename_1to2_repo(2, 2)),
        ),
        (
            "rename/delete, ours renames",
            Box::new(|| rename_delete_repo(true)),
        ),
        (
            "rename/delete, theirs renames",
            Box::new(|| rename_delete_repo(false)),
        ),
        ("rename/add", Box::new(rename_add_repo)),
        ("2to1", Box::new(rename_matrix::rename_2to1_repo)),
        ("1to1 clean", Box::new(|| rename_1to1_repo(2, 6))),
        ("1to1 conflicting", Box::new(|| rename_1to1_repo(2, 2))),
    ]
}

/// base `old`; one side renames it to `new` (pure rename), the other deletes it.
fn rename_delete_repo(ours_renames: bool) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let base: String = (1..=8).map(|n| format!("l{n}\n")).collect();
    commit_file(p, "old", &base, "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    let act = |p: &Path, rename: bool| {
        if rename {
            std::fs::rename(p.join("old"), p.join("new")).expect("rename");
        } else {
            std::fs::remove_file(p.join("old")).expect("delete");
        }
    };
    act(p, ours_renames);
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ours", "--no-verify"], p),
        "ours",
    );
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    act(p, !ours_renames);
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "theirs", "--no-verify"], p),
        "theirs",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    repo
}

/// base `old`; ours renames it to `new`, theirs independently ADDS `new`.
fn rename_add_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let base: String = (1..=8).map(|n| format!("l{n}\n")).collect();
    commit_file(p, "old", &base, "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    std::fs::rename(p.join("old"), p.join("new")).expect("rename");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ours renames", "--no-verify"], p),
        "ours",
    );
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    commit_file(p, "new", "theirs own file\n", "theirs adds new");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    repo
}

/// base `old`; BOTH sides rename it to `new`, each editing the given line.
fn rename_1to1_repo(ours_line: usize, theirs_line: usize) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let base: String = (1..=8).map(|n| format!("l{n}\n")).collect();
    commit_file(p, "old", &base, "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    let edited = |line: usize, text: &str| -> String {
        (1..=8)
            .map(|n| {
                if n == line {
                    format!("{text}\n")
                } else {
                    format!("l{n}\n")
                }
            })
            .collect()
    };
    for (branch, line, text, message) in [
        ("main", ours_line, "OURS", "ours moves and edits"),
        ("feature", theirs_line, "THEIRS", "theirs moves and edits"),
    ] {
        if branch == "feature" {
            assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        }
        std::fs::remove_file(p.join("old")).expect("drop");
        std::fs::write(p.join("new"), edited(line, text)).expect("new");
        assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", message, "--no-verify"], p),
            message,
        );
    }
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    repo
}

/// Both engines must reach the SAME verdict for every MG-06 shape — MG-04 set
/// this rule with `merge_df_conflict_is_identical_on_the_flat_walk`, and the
/// rename shapes need it more: the pruned walk decides each path on its own
/// and the rename pass then overrides those decisions, while the flattening
/// engine rewrites its maps before deciding anything. The `--dry-run` summary
/// is compared field by field, `files_changed` included.
#[test]
fn merge_rename_conflict_summaries_match_across_both_walks() {
    // Every MG-06 shape, not just 1to2 — Codex R1 P1-6 found the two engines
    // disagreeing on `files_changed` for rename/delete, which this had not
    // been wide enough to catch.
    for (label, build) in rename_conflict_shapes() {
        let mut summaries = Vec::new();
        for env in [
            &[][..],
            &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
        ] {
            let repo = build();
            let p = repo.path();
            let out = run_libra_command_with_stdin_and_env(
                &["merge", "--dry-run", "--json", "feature"],
                p,
                "",
                env,
            );
            // A would-conflict preview exits 1 and still prints its envelope.
            // Each iteration builds its OWN repository, so the commit ids
            // differ by construction — only the merge OUTCOME is comparable.
            let mut parsed = parse_json_stdout(&out);
            let data = parsed["data"]
                .as_object_mut()
                .expect("the envelope carries a data object");
            data.remove("old_commit");
            data.remove("commit");
            summaries.push(parsed);
        }
        assert_eq!(
            summaries[0], summaries[1],
            "{label}: the pruned walk and the flattening engine must agree"
        );
    }
}
