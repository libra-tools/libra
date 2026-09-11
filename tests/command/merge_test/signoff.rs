//! End-to-end coverage for `merge --signoff`.

use std::path::Path;

use libra::common_utils::parse_commit_msg;
use tempfile::{TempDir, tempdir};

use super::{assert_cli_success, commit_file, configure_identity_via_cli, run_libra_command};

const SIGNOFF: &str = "Signed-off-by: Test User <test@example.com>";

fn divergent_merge_repo() -> TempDir {
    let repo = tempdir().expect("create merge signoff repository");
    let path = repo.path();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], path),
        "initialize merge signoff repository",
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
    commit_file(path, "feature-first.txt", "first\n", "feature first");
    commit_file(path, "feature-second.txt", "second\n", "feature second");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], path),
        "return to main branch",
    );
    commit_file(path, "main.txt", "main\n", "main change");
    repo
}

fn conflicted_merge_repo() -> TempDir {
    let repo = tempdir().expect("create conflicted merge signoff repository");
    let path = repo.path();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], path),
        "initialize conflicted merge signoff repository",
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
    // Object serialization terminates the commit message with a newline; that
    // transport terminator is not part of the logical trailer ordering.
    parse_commit_msg(message).0.trim_end().to_string()
}

fn assert_one_final_signoff(repo: &Path, context: &str) -> String {
    let message = head_message(repo);
    assert_eq!(
        message.matches(SIGNOFF).count(),
        1,
        "{context} must contain exactly one signoff: {message}"
    );
    assert!(
        message.ends_with(SIGNOFF),
        "{context} must append the signoff after the final message body: {message}"
    );
    message
}

#[test]
fn merge_signoff_automatic_merge_appends_trailer() {
    let repo = divergent_merge_repo();
    assert_cli_success(
        &run_libra_command(&["merge", "--signoff", "feature"], repo.path()),
        "merge with --signoff",
    );
    assert_one_final_signoff(repo.path(), "automatic merge");
}

#[test]
fn merge_signoff_continue_preserves_the_original_request() {
    let repo = conflicted_merge_repo();
    let path = repo.path();
    let conflict = run_libra_command(&["merge", "--signoff", "feature"], path);
    assert_eq!(
        conflict.status.code(),
        Some(128),
        "merge must stop on conflict"
    );
    std::fs::write(path.join("shared.txt"), "resolved\n").expect("write conflict resolution");
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], path),
        "stage conflict resolution",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--continue"], path),
        "continue signed-off merge",
    );
    assert_one_final_signoff(path, "continued merge");
}

#[test]
fn merge_signoff_deduplicates_an_existing_matching_trailer() {
    let repo = divergent_merge_repo();
    assert_cli_success(
        &run_libra_command(
            &[
                "merge",
                "--signoff",
                "-m",
                "custom subject\n\nSigned-off-by: Test User <test@example.com>",
                "feature",
            ],
            repo.path(),
        ),
        "merge with a preexisting signoff",
    );
    assert_one_final_signoff(repo.path(), "deduplicated merge");
}

#[test]
fn merge_signoff_does_not_treat_body_text_as_a_trailer() {
    let repo = divergent_merge_repo();
    assert_cli_success(
        &run_libra_command(
            &[
                "merge",
                "--signoff",
                "-m",
                "custom subject mentions Signed-off-by: Test User <test@example.com>",
                "feature",
            ],
            repo.path(),
        ),
        "merge with signoff-shaped body text",
    );
    let message = head_message(repo.path());
    assert_eq!(
        message.matches(SIGNOFF).count(),
        2,
        "body text must not suppress the generated trailer: {message}"
    );
    assert!(
        message.ends_with(SIGNOFF),
        "the generated trailer must remain final: {message}"
    );
}

#[test]
fn merge_signoff_appends_after_a_custom_message() {
    let repo = divergent_merge_repo();
    assert_cli_success(
        &run_libra_command(
            &[
                "merge",
                "--signoff",
                "-m",
                "custom subject\n\ncustom body",
                "feature",
            ],
            repo.path(),
        ),
        "merge with custom message and signoff",
    );
    let message = assert_one_final_signoff(repo.path(), "custom-message merge");
    assert_eq!(
        message,
        format!("custom subject\n\ncustom body\n\n{SIGNOFF}")
    );
}

#[test]
fn merge_signoff_appends_after_the_generated_shortlog() {
    let repo = divergent_merge_repo();
    assert_cli_success(
        &run_libra_command(&["merge", "--signoff", "--log=2", "feature"], repo.path()),
        "merge with shortlog and signoff",
    );
    let message = assert_one_final_signoff(repo.path(), "shortlog merge");
    let signoff_offset = message.find(SIGNOFF).expect("signoff exists");
    assert!(
        message[..signoff_offset].contains("feature first")
            && message[..signoff_offset].contains("feature second"),
        "the complete shortlog must remain before the signoff: {message}"
    );
}
