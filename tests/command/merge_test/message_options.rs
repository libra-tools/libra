//! End-to-end coverage for merge-message source, editing, and cleanup options.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::{fs, path::Path};

use libra::common_utils::parse_commit_msg;
use tempfile::{TempDir, tempdir};

use super::{
    assert_cli_success, commit_file, configure_identity_via_cli, run_libra_command,
    run_libra_command_with_stdin_and_env,
};

const SIGNOFF: &str = "Signed-off-by: Test User <test@example.com>";

fn divergent_merge_repo() -> TempDir {
    let repo = tempdir().expect("create merge message repository");
    let path = repo.path();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], path),
        "initialize merge message repository",
    );
    configure_identity_via_cli(path);
    commit_file(path, "base.txt", "base\n", "base");
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], path),
        "create feature branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], path),
        "checkout feature branch",
    );
    commit_file(path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], path),
        "return to main branch",
    );
    commit_file(path, "main.txt", "main\n", "main change");
    repo
}

fn conflicted_merge_repo() -> TempDir {
    let repo = tempdir().expect("create conflicted merge message repository");
    let path = repo.path();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], path),
        "initialize conflicted merge message repository",
    );
    configure_identity_via_cli(path);
    commit_file(path, "shared.txt", "base\n", "base");
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], path),
        "create feature branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], path),
        "checkout feature branch",
    );
    commit_file(path, "shared.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], path),
        "return to main branch",
    );
    commit_file(path, "shared.txt", "main\n", "main change");
    repo
}

fn head_message(repo: &Path) -> String {
    let output = run_libra_command(&["cat-file", "-p", "HEAD"], repo);
    assert_cli_success(&output, "read merge commit");
    let raw = String::from_utf8(output.stdout).expect("commit object must be utf-8");
    let (_, message) = raw
        .split_once("\n\n")
        .expect("commit object must contain a message separator");
    parse_commit_msg(message).0.trim_end().to_string()
}

#[cfg(unix)]
fn write_editor(repo: &Path, name: &str, body: &str) -> String {
    let path = repo.join(name);
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s' '{}' > \"$1\"\n",
            body.replace('\'', "'\\''")
        ),
    )
    .expect("write merge editor script");
    let mut permissions = fs::metadata(&path)
        .expect("read merge editor script metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("make merge editor script executable");
    path.to_string_lossy().into_owned()
}

#[test]
fn merge_message_options_file_reads_contents_and_rejects_dash_m() {
    let repo = divergent_merge_repo();
    let path = repo.path();
    let message_file = path.join("merge-message.txt");
    fs::write(&message_file, "message from file\n\nwith a body\n")
        .expect("write merge message file");
    let message_file = message_file.to_string_lossy().into_owned();

    assert_cli_success(
        &run_libra_command(&["merge", "-F", &message_file, "feature"], path),
        "merge with message file",
    );
    assert_eq!(
        head_message(path),
        "message from file\n\nwith a body",
        "-F must use the file contents as the merge message"
    );

    let repo = divergent_merge_repo();
    let output = run_libra_command(
        &["merge", "-m", "inline", "-F", &message_file, "feature"],
        repo.path(),
    );
    assert_eq!(
        output.status.code(),
        Some(129),
        "-m and -F must be mutually exclusive: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn merge_message_options_into_name_changes_only_the_generated_target_name() {
    let repo = divergent_merge_repo();
    assert_cli_success(
        &run_libra_command(&["merge", "--into-name", "release", "feature"], repo.path()),
        "merge with a custom destination name",
    );
    assert!(
        head_message(repo.path()).starts_with("Merge feature into release"),
        "--into-name must change the generated destination name"
    );
}

#[test]
fn merge_message_options_cleanup_modes_use_the_shared_cleanup_contract() {
    for (mode, raw, expected, absent) in [
        (
            "strip",
            "subject   \n\n# comment\n\nbody   \n\n\ntrailing   \n",
            "subject\n\nbody\n\ntrailing",
            "# comment",
        ),
        (
            "whitespace",
            "subject   \n\n# comment\n\nbody   \n",
            "subject\n\n# comment\n\nbody",
            "this text is absent",
        ),
        (
            "default",
            "subject   \n\n# comment\n\nbody   \n",
            "subject\n\n# comment\n\nbody",
            "this text is absent",
        ),
    ] {
        let repo = divergent_merge_repo();
        let path = repo.path();
        let message_file = path.join(format!("{mode}.txt"));
        fs::write(&message_file, raw).expect("write cleanup fixture");
        let message_file = message_file.to_string_lossy().into_owned();
        let cleanup = format!("--cleanup={mode}");
        assert_cli_success(
            &run_libra_command(&["merge", "-F", &message_file, &cleanup, "feature"], path),
            "merge with cleanup mode",
        );
        let message = head_message(path);
        assert_eq!(
            message, expected,
            "--cleanup={mode} must match shared cleanup"
        );
        assert!(
            !message.contains(absent),
            "--cleanup={mode} unexpectedly retained {absent:?}: {message}"
        );
    }

    let repo = divergent_merge_repo();
    let path = repo.path();
    let message_file = path.join("verbatim.txt");
    let raw = "subject   \n\n# comment\n\nbody   \n";
    fs::write(&message_file, raw).expect("write verbatim fixture");
    let message_file = message_file.to_string_lossy().into_owned();
    assert_cli_success(
        &run_libra_command(
            &[
                "merge",
                "-F",
                &message_file,
                "--cleanup=verbatim",
                "feature",
            ],
            path,
        ),
        "merge with verbatim cleanup",
    );
    assert_eq!(
        head_message(path),
        raw.trim_end(),
        "verbatim cleanup must preserve whitespace and comment lines"
    );
}

#[cfg(unix)]
#[test]
fn merge_message_options_scissors_truncates_an_edited_message() {
    let repo = divergent_merge_repo();
    let path = repo.path();
    let message_file = path.join("scissors.txt");
    fs::write(
        &message_file,
        "subject   \n# ------------------------ >8 ------------------------\nafter scissors\n",
    )
    .expect("write scissors fixture");
    let message_file = message_file.to_string_lossy().into_owned();
    let editor = path.join("no-op-merge-editor.sh");
    fs::write(&editor, "#!/bin/sh\nexit 0\n").expect("write no-op merge editor");
    let mut permissions = fs::metadata(&editor)
        .expect("read no-op merge editor metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&editor, permissions).expect("make no-op merge editor executable");
    let editor = editor.to_string_lossy().into_owned();
    assert_cli_success(
        &run_libra_command_with_stdin_and_env(
            &[
                "merge",
                "--edit",
                "-F",
                &message_file,
                "--cleanup=scissors",
                "feature",
            ],
            path,
            "",
            &[("GIT_EDITOR", editor.as_str())],
        ),
        "merge with edited scissors cleanup",
    );
    assert_eq!(
        head_message(path),
        "subject",
        "scissors cleanup must truncate at the marker when an editor is used"
    );
}

#[cfg(unix)]
#[test]
fn merge_message_options_edit_uses_editor_and_rejects_an_empty_result() {
    let repo = divergent_merge_repo();
    let path = repo.path();
    let editor = write_editor(
        path,
        "merge-editor.sh",
        "edited subject\n# hidden\n\nedited body\n",
    );
    assert_cli_success(
        &run_libra_command_with_stdin_and_env(
            &["merge", "--edit", "feature"],
            path,
            "",
            &[("GIT_EDITOR", editor.as_str())],
        ),
        "merge with editor",
    );
    assert_eq!(
        head_message(path),
        "edited subject\n\nedited body",
        "--edit must use the edited, comment-cleaned message"
    );

    let repo = divergent_merge_repo();
    let path = repo.path();
    let editor = write_editor(path, "empty-merge-editor.sh", "\n# no message\n");
    let output = run_libra_command_with_stdin_and_env(
        &["merge", "--edit", "feature"],
        path,
        "",
        &[("GIT_EDITOR", editor.as_str())],
    );
    assert_eq!(
        output.status.code(),
        Some(128),
        "an empty editor result must abort the merge: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn merge_message_options_continue_replays_file_message_and_allows_edit() {
    let repo = conflicted_merge_repo();
    let path = repo.path();
    let message_file = path.join("conflict-message.txt");
    fs::write(
        &message_file,
        "stored subject\n\n# stripped comment\n\nstored body\n",
    )
    .expect("write conflict message file");
    let message_file = message_file.to_string_lossy().into_owned();
    let conflict = run_libra_command(
        &[
            "merge",
            "--signoff",
            "-F",
            &message_file,
            "--cleanup=strip",
            "feature",
        ],
        path,
    );
    assert_eq!(
        conflict.status.code(),
        Some(128),
        "merge must stop on conflict"
    );
    fs::write(path.join("shared.txt"), "resolved\n").expect("write conflict resolution");
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], path),
        "stage conflict resolution",
    );
    let editor = write_editor(
        path,
        "continue-merge-editor.sh",
        "continued subject\n# stripped editor comment\n\ncontinued body\n",
    );
    assert_cli_success(
        &run_libra_command_with_stdin_and_env(
            &["merge", "--continue", "--edit"],
            path,
            "",
            &[("GIT_EDITOR", editor.as_str())],
        ),
        "continue merge with editor",
    );
    assert_eq!(
        head_message(path),
        format!("continued subject\n\ncontinued body\n\n{SIGNOFF}"),
        "--continue must edit the saved message and append signoff after cleanup"
    );
}

#[test]
fn merge_message_options_continue_accepts_a_file_override() {
    let repo = conflicted_merge_repo();
    let path = repo.path();
    assert_eq!(
        run_libra_command(&["merge", "feature"], path).status.code(),
        Some(128),
        "merge must stop on conflict"
    );
    fs::write(path.join("shared.txt"), "resolved\n").expect("write conflict resolution");
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], path),
        "stage conflict resolution",
    );
    let message_file = path.join("continue-message.txt");
    fs::write(
        &message_file,
        "continued from file\n\n# comment removed\n\nbody\n",
    )
    .expect("write continuation message file");
    let message_file = message_file.to_string_lossy().into_owned();
    assert_cli_success(
        &run_libra_command(
            &[
                "merge",
                "--continue",
                "-F",
                &message_file,
                "--cleanup=strip",
            ],
            path,
        ),
        "continue merge with a message file",
    );
    assert_eq!(
        head_message(path),
        "continued from file\n\nbody",
        "-F must override the saved message while continuing a merge"
    );
}
