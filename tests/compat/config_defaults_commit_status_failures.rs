#[cfg(unix)]
use super::commit_status_defaults::write_capturing_editor;
use super::*;

#[cfg(unix)]
#[test]
fn status_template_errors_precede_hook_editor_and_history_and_no_status_bypasses_them() {
    let fixture = Fixture::new();
    let repo = fixture.path("status-template-error");
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "base.txt", "base\n", "base");
    fs::write(repo.join("next.txt"), "next\n").expect("write staged file");
    fixture.success(&repo, &["add", "next.txt"]);
    fixture.success(&repo, &["config", "status.showUntrackedFiles", "invalid"]);

    let hook_sentinel = repo.join(".hook-ran");
    let hook = repo.join(".libra").join("hooks").join("pre-commit.sh");
    fs::write(
        &hook,
        format!("#!/bin/sh\ntouch \"{}\"\n", hook_sentinel.display()),
    )
    .expect("write pre-commit hook");
    let mut hook_permissions = fs::metadata(&hook)
        .expect("read hook metadata")
        .permissions();
    hook_permissions.set_mode(0o755);
    fs::set_permissions(&hook, hook_permissions).expect("make hook executable");

    let editor_sentinel = fixture.path("editor-ran");
    let editor = fixture.path("status-error-editor.sh");
    fs::write(
        &editor,
        format!(
            "#!/bin/sh\ntest -f \"{}\" || exit 42\ntouch \"{}\"\nprintf '%s\\n' 'status error subject' > \"$1\"\n",
            hook_sentinel.display(),
            editor_sentinel.display(),
        ),
    )
    .expect("write editor");
    let mut editor_permissions = fs::metadata(&editor)
        .expect("read editor metadata")
        .permissions();
    editor_permissions.set_mode(0o755);
    fs::set_permissions(&editor, editor_permissions).expect("make editor executable");

    let before = stdout_trim(&fixture.success(&repo, &["rev-parse", "HEAD"]));
    let rejected = fixture
        .libra_command(&repo, &["commit", "--no-gpg-sign"])
        .env("EDITOR", &editor)
        .output()
        .expect("run commit with invalid status template config");
    assert_eq!(rejected.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
    assert!(stderr.contains("status.showUntrackedFiles"), "{stderr}");
    assert!(!hook_sentinel.exists(), "pre-commit hook must not run");
    assert!(!editor_sentinel.exists(), "editor must not run");
    assert_eq!(
        stdout_trim(&fixture.success(&repo, &["rev-parse", "HEAD"])),
        before,
        "status-template failure must not move HEAD"
    );

    let committed = fixture
        .libra_command(&repo, &["commit", "--no-status", "--no-gpg-sign"])
        .env("EDITOR", &editor)
        .output()
        .expect("run commit with --no-status bypass");
    assert_success("libra", &["commit", "--no-status"], &committed);
    assert!(
        hook_sentinel.exists(),
        "bypass should reach pre-commit hook"
    );
    assert!(editor_sentinel.exists(), "bypass should open the editor");
}

#[cfg(unix)]
#[test]
fn stash_status_read_failure_precedes_hook_editor_and_ref_write() {
    let fixture = Fixture::new();
    let repo = fixture.path("stash-status-read-error");
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "base.txt", "base\n", "base");
    fs::write(repo.join("next.txt"), "next\n").expect("write staged file");
    fixture.success(&repo, &["add", "next.txt"]);
    fixture.success(&repo, &["config", "status.showStash", "true"]);

    let libra_dir = repo.join(".libra");
    fs::create_dir_all(libra_dir.join("refs")).expect("create refs directory");
    fs::write(libra_dir.join("refs/stash"), "invalid-ref\n").expect("seed stash ref");
    fs::create_dir_all(libra_dir.join("logs/refs")).expect("create stash log directory");
    fs::write(libra_dir.join("logs/refs/stash"), "corrupt stash log\n")
        .expect("seed corrupt stash log");

    let hook_sentinel = repo.join(".stash-hook-ran");
    let hook = libra_dir.join("hooks/pre-commit.sh");
    fs::write(
        &hook,
        format!("#!/bin/sh\ntouch \"{}\"\n", hook_sentinel.display()),
    )
    .expect("write pre-commit hook");
    let mut hook_permissions = fs::metadata(&hook)
        .expect("read hook metadata")
        .permissions();
    hook_permissions.set_mode(0o755);
    fs::set_permissions(&hook, hook_permissions).expect("make hook executable");

    let editor_sentinel = fixture.path("stash-editor-ran");
    let editor = fixture.path("stash-status-editor.sh");
    fs::write(
        &editor,
        format!(
            "#!/bin/sh\ntouch \"{}\"\nprintf '%s\\n' 'must not commit' > \"$1\"\n",
            editor_sentinel.display()
        ),
    )
    .expect("write editor");
    let mut editor_permissions = fs::metadata(&editor)
        .expect("read editor metadata")
        .permissions();
    editor_permissions.set_mode(0o755);
    fs::set_permissions(&editor, editor_permissions).expect("make editor executable");

    let before = stdout_trim(&fixture.success(&repo, &["rev-parse", "HEAD"]));
    let rejected = fixture
        .libra_command(&repo, &["commit", "--no-gpg-sign"])
        .env("EDITOR", &editor)
        .output()
        .expect("run commit with corrupt stash status");
    assert_eq!(rejected.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("stash"), "{stderr}");
    assert!(!hook_sentinel.exists(), "pre-commit hook must not run");
    assert!(!editor_sentinel.exists(), "editor must not run");
    assert_eq!(
        stdout_trim(&fixture.success(&repo, &["rev-parse", "HEAD"])),
        before,
        "stash status failure must not move HEAD"
    );

    fs::remove_file(libra_dir.join("logs/refs/stash")).expect("remove corrupt stash log");
    fs::remove_file(libra_dir.join("refs/stash")).expect("remove regular stash ref");
    std::os::unix::fs::symlink("stash", libra_dir.join("refs/stash"))
        .expect("create self-referential stash ref");
    let rejected = fixture
        .libra_command(&repo, &["commit", "--no-gpg-sign"])
        .env("EDITOR", &editor)
        .output()
        .expect("run commit with unreadable stash ref metadata");
    assert_eq!(rejected.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("stash"), "{stderr}");
    assert!(!hook_sentinel.exists(), "pre-commit hook must not run");
    assert!(!editor_sentinel.exists(), "editor must not run");
    assert_eq!(
        stdout_trim(&fixture.success(&repo, &["rev-parse", "HEAD"])),
        before,
        "stash-ref metadata failure must not move HEAD"
    );

    fs::remove_file(libra_dir.join("refs/stash")).expect("remove self-referential stash ref");
    fs::write(libra_dir.join("refs/stash-target"), "invalid-ref\n")
        .expect("create regular symlink target");
    std::os::unix::fs::symlink("stash-target", libra_dir.join("refs/stash"))
        .expect("create stash ref symlink to a regular file");
    let rejected = fixture
        .libra_command(&repo, &["commit", "--no-gpg-sign"])
        .env("EDITOR", &editor)
        .output()
        .expect("run commit with symlink at stash ref path");
    assert_eq!(rejected.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("stash"), "{stderr}");
    assert!(!hook_sentinel.exists(), "pre-commit hook must not run");
    assert!(!editor_sentinel.exists(), "editor must not run");
    assert_eq!(
        stdout_trim(&fixture.success(&repo, &["rev-parse", "HEAD"])),
        before,
        "stash-ref symlink must not move HEAD"
    );

    fs::remove_file(libra_dir.join("refs/stash")).expect("remove stash ref symlink");
    fs::remove_file(libra_dir.join("refs/stash-target")).expect("remove regular symlink target");
    fs::create_dir(libra_dir.join("refs/stash")).expect("create invalid stash ref directory");
    let rejected = fixture
        .libra_command(&repo, &["commit", "--no-gpg-sign"])
        .env("EDITOR", &editor)
        .output()
        .expect("run commit with directory at stash ref path");
    assert_eq!(rejected.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("stash"), "{stderr}");
    assert!(!hook_sentinel.exists(), "pre-commit hook must not run");
    assert!(!editor_sentinel.exists(), "editor must not run");
    assert_eq!(
        stdout_trim(&fixture.success(&repo, &["rev-parse", "HEAD"])),
        before,
        "invalid stash-ref file type must not move HEAD"
    );
}

#[cfg(unix)]
#[test]
fn invalid_status_defaults_are_bypassed_without_editor_or_with_non_stripping_cleanup() {
    let message_fixture = Fixture::new();
    let message_repo = message_fixture.path("status-no-editor");
    message_fixture.init_repo(&message_repo);
    fs::write(message_repo.join("message.txt"), "message\n").expect("write message file");
    message_fixture.success(&message_repo, &["add", "message.txt"]);
    message_fixture.success(
        &message_repo,
        &["config", "status.showUntrackedFiles", "invalid"],
    );
    message_fixture.success(&message_repo, &["config", "commit.status", "invalid"]);
    message_fixture.success(
        &message_repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "no editor"],
    );

    for mode in ["verbatim", "whitespace", "scissors"] {
        let fixture = Fixture::new();
        let repo = fixture.path(&format!("status-cleanup-{mode}"));
        fixture.init_repo(&repo);
        fs::write(repo.join("tracked.txt"), "staged\n").expect("write staged file");
        fixture.success(&repo, &["add", "tracked.txt"]);
        fixture.success(&repo, &["config", "status.showUntrackedFiles", "invalid"]);
        fixture.success(&repo, &["config", "commit.status", "invalid"]);
        let capture = fixture.path(&format!("cleanup-{mode}-template.txt"));
        let editor =
            write_capturing_editor(&fixture, &format!("cleanup-{mode}-editor.sh"), &capture);
        let cleanup = format!("--cleanup={mode}");
        let args = ["commit", "--no-gpg-sign", "--no-verify", &cleanup];
        let output = fixture
            .libra_command(&repo, &args)
            .env("EDITOR", editor)
            .output()
            .expect("run non-stripping cleanup commit");
        assert_success("libra", &args, &output);
        let template = fs::read_to_string(capture).expect("read captured cleanup template");
        assert!(!template.contains("tracked.txt"), "mode={mode}: {template}");
    }
}

#[derive(Clone, Copy)]
enum CommitStatusFailure {
    InvalidValue,
    UnreadableGlobalStore,
}

fn assert_commit_status_bypass(
    failure: CommitStatusFailure,
    case_name: &str,
    args: &[&str],
    needs_editor: bool,
) {
    let fixture = Fixture::new();
    let repo = fixture.path(case_name);
    fixture.init_repo(&repo);
    fs::write(repo.join("tracked.txt"), "staged\n").expect("write staged file");
    fixture.success(&repo, &["add", "tracked.txt"]);
    match failure {
        CommitStatusFailure::InvalidValue => {
            fixture.success(&repo, &["config", "commit.status", "invalid"]);
        }
        CommitStatusFailure::UnreadableGlobalStore => {
            fs::create_dir_all(fixture.home.join(".libra").join("config.db"))
                .expect("replace global config database with a directory");
        }
    }

    let mut command = fixture.libra_command(&repo, args);
    if needs_editor {
        let capture = fixture.path(&format!("{case_name}-template.txt"));
        let editor = write_capturing_editor(&fixture, &format!("{case_name}-editor.sh"), &capture);
        command.env("EDITOR", editor);
    }
    let output = command.output().expect("run commit.status bypass case");
    assert_success("libra", args, &output);
    if args.first() == Some(&"--json") {
        let payload: Value = serde_json::from_slice(&output.stdout).expect("parse commit JSON");
        assert_eq!(payload["ok"], true, "case={case_name}");
    }
}

#[test]
fn invalid_and_unreadable_commit_status_are_bypassed_on_inapplicable_paths() {
    let common_cases: &[(&str, &[&str], bool)] = &[
        (
            "message-bypass",
            &["commit", "--no-gpg-sign", "--no-verify", "-m", "message"],
            false,
        ),
        (
            "dry-run-bypass",
            &["commit", "--dry-run", "--no-gpg-sign"],
            false,
        ),
        (
            "porcelain-bypass",
            &["commit", "--porcelain", "--no-gpg-sign"],
            false,
        ),
        (
            "json-bypass",
            &[
                "--json",
                "commit",
                "--no-gpg-sign",
                "--no-verify",
                "-m",
                "json",
            ],
            false,
        ),
        (
            "cleanup-bypass",
            &[
                "commit",
                "--cleanup=whitespace",
                "--no-gpg-sign",
                "--no-verify",
            ],
            true,
        ),
        (
            "no-status-bypass",
            &["commit", "--no-status", "--no-gpg-sign", "--no-verify"],
            true,
        ),
    ];

    for failure in [
        CommitStatusFailure::InvalidValue,
        CommitStatusFailure::UnreadableGlobalStore,
    ] {
        let prefix = match failure {
            CommitStatusFailure::InvalidValue => "invalid",
            CommitStatusFailure::UnreadableGlobalStore => "unreadable",
        };
        for (name, args, needs_editor) in common_cases {
            assert_commit_status_bypass(failure, &format!("{prefix}-{name}"), args, *needs_editor);
        }
    }

    assert_commit_status_bypass(
        CommitStatusFailure::InvalidValue,
        "invalid-explicit-status-bypass",
        &["commit", "--status", "--no-gpg-sign", "--no-verify"],
        true,
    );
}

#[test]
fn unreadable_store_with_explicit_status_skips_commit_status_then_reports_status_key() {
    let fixture = Fixture::new();
    let repo = fixture.path("unreadable-explicit-status");
    fixture.init_repo(&repo);
    fs::write(repo.join("tracked.txt"), "staged\n").expect("write staged file");
    fixture.success(&repo, &["add", "tracked.txt"]);
    fs::create_dir_all(fixture.home.join(".libra/config.db"))
        .expect("replace global config database with a directory");
    let output = fixture
        .libra_command(&repo, &["commit", "--status", "--no-gpg-sign"])
        .env("EDITOR", "true")
        .output()
        .expect("run explicit status with unreadable config store");
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("status.showUntrackedFiles"), "{stderr}");
    assert!(!stderr.contains("commit.status"), "{stderr}");
}
