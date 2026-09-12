//! `libra mergetool` end-to-end coverage: a temporary resolver receives Git's
//! BASE/LOCAL/REMOTE/MERGED contract and only a positively resolved path is
//! written back and staged.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::{
    assert_cli_success, create_committed_repo_via_cli, run_libra_command,
    run_libra_command_with_stdin_and_env,
};

fn commit_file(repo: &Path, file: &str, content: &str, message: &str) {
    std::fs::write(repo.join(file), content).expect("write test file");
    assert_cli_success(&run_libra_command(&["add", file], repo), "stage test file");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", message, "--no-verify"], repo),
        "commit test file",
    );
}

fn conflicted_repo() -> tempfile::TempDir {
    conflicted_repo_at("conflict.txt")
}

fn conflicted_repo_at(file: &str) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    if let Some(parent) = root.join(file).parent() {
        std::fs::create_dir_all(parent).expect("create conflict parent directory");
    }
    commit_file(root, file, "base\n", "add conflict base");
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], root),
        "create feature branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], root),
        "checkout feature branch",
    );
    commit_file(root, file, "theirs\n", "feature changes conflict");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], root),
        "return to main branch",
    );
    commit_file(root, file, "ours\n", "main changes conflict");
    let merge = run_libra_command(&["merge", "feature", "--no-verify"], root);
    assert_eq!(
        merge.status.code(),
        Some(128),
        "fixture merge must conflict: {}",
        String::from_utf8_lossy(&merge.stderr)
    );
    repo
}

fn configure_cmd(repo: &Path, name: &str, command: &str) {
    assert_cli_success(
        &run_libra_command(&["config", "merge.tool", name], repo),
        "configure default mergetool",
    );
    let key = format!("mergetool.{name}.cmd");
    assert_cli_success(
        &run_libra_command(&["config", key.as_str(), command], repo),
        "configure custom mergetool command",
    );
}

fn index_stages(repo: &Path, path: &str) -> Vec<String> {
    let output = run_libra_command(&["ls-files", "-s"], repo);
    assert_cli_success(&output, "inspect index stages");
    String::from_utf8(output.stdout)
        .expect("ls-files output is utf-8")
        .lines()
        .filter(|line| line.ends_with(&format!("\t{path}")))
        .map(str::to_string)
        .collect()
}

#[test]
fn mergetool_shell_command_gets_all_files_and_stages_its_result() {
    let repo = conflicted_repo();
    let root = repo.path();
    configure_cmd(
        root,
        "capture",
        "test -f \"$BASE\" && test -f \"$LOCAL\" && test -f \"$REMOTE\" && test -f \"$MERGED\" && printf 'resolved by tool\\n' > \"$MERGED\"",
    );

    assert_cli_success(
        &run_libra_command(&["mergetool"], root),
        "custom mergetool resolves the conflict",
    );
    assert_eq!(
        std::fs::read_to_string(root.join("conflict.txt")).expect("resolved file"),
        "resolved by tool\n"
    );
    assert!(
        std::fs::read_to_string(root.join("conflict.txt.orig"))
            .expect("default backup")
            .contains("<<<<<<<"),
        "the default backup retains conflict markers"
    );
    let stages = index_stages(root, "conflict.txt");
    assert_eq!(
        stages.len(),
        1,
        "all conflict stages are replaced: {stages:?}"
    );
    assert!(
        stages[0].contains(" 0\t"),
        "stage 0 is recorded: {stages:?}"
    );
}

#[test]
fn mergetool_uses_mtime_when_exit_code_is_not_trusted() {
    let repo = conflicted_repo();
    let root = repo.path();
    configure_cmd(
        root,
        "mtime",
        "printf 'mtime result\\n' > \"$MERGED\"; exit 9",
    );

    assert_cli_success(
        &run_libra_command(&["mergetool"], root),
        "a changed MERGED file resolves despite a non-zero tool exit",
    );
    assert_eq!(
        std::fs::read_to_string(root.join("conflict.txt")).expect("resolved file"),
        "mtime result\n"
    );
}

#[test]
fn mergetool_accepts_an_older_merged_mtime_when_exit_code_is_not_trusted() {
    let repo = conflicted_repo();
    let root = repo.path();
    configure_cmd(
        root,
        "older-mtime",
        "printf 'older mtime result\\n' > \"$MERGED\"; touch -t 200001010000 \"$MERGED\"; exit 9",
    );

    assert_cli_success(
        &run_libra_command(&["mergetool"], root),
        "any changed MERGED mtime resolves despite a non-zero tool exit",
    );
    assert_eq!(
        std::fs::read_to_string(root.join("conflict.txt")).expect("resolved file"),
        "older mtime result\n"
    );
}

#[test]
fn mergetool_trust_exit_code_uses_only_the_exit_status() {
    let repo = conflicted_repo();
    let root = repo.path();
    configure_cmd(
        root,
        "trusted",
        "printf 'ignored result\\n' > \"$MERGED\"; exit 9",
    );
    assert_cli_success(
        &run_libra_command(&["config", "mergetool.trusted.trustExitCode", "true"], root),
        "trust exit code",
    );

    let output = run_libra_command(&["mergetool"], root);
    assert!(
        !output.status.success(),
        "a trusted non-zero exit is unresolved"
    );
    assert!(
        std::fs::read_to_string(root.join("conflict.txt"))
            .expect("working tree")
            .contains("<<<<<<<"),
        "untrusted temporary output must not replace the worktree"
    );
    assert!(
        index_stages(root, "conflict.txt")
            .iter()
            .any(|line| line.contains(" 2\t")),
        "unresolved conflict stays in the index"
    );
}

#[test]
fn mergetool_noninteractive_unchanged_output_is_unresolved() {
    let repo = conflicted_repo();
    let root = repo.path();
    configure_cmd(root, "unchanged", "exit 0");

    let output = run_libra_command_with_stdin_and_env(&["mergetool"], root, "", &[]);
    assert!(
        !output.status.success(),
        "EOF cannot silently resolve a conflict"
    );
    assert!(
        std::fs::read_to_string(root.join("conflict.txt"))
            .expect("working tree")
            .contains("<<<<<<<"),
        "unchanged output leaves the worktree unresolved"
    );
    assert!(
        index_stages(root, "conflict.txt")
            .iter()
            .any(|line| line.contains(" 3\t")),
        "unchanged output leaves conflict stages"
    );
}

#[cfg(unix)]
#[test]
fn mergetool_path_overrides_path_lookup_for_a_builtin_tool() {
    let repo = conflicted_repo();
    let root = repo.path();
    let tool = root.join("configured-vimdiff");
    std::fs::write(
        &tool,
        "#!/bin/sh\nprintf 'path override\\n' > \"$MERGED\"\n",
    )
    .expect("write fake vimdiff");
    let mut permissions = std::fs::metadata(&tool)
        .expect("fake tool metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&tool, permissions).expect("mark fake tool executable");
    assert_cli_success(
        &run_libra_command(&["config", "merge.tool", "vimdiff"], root),
        "select vimdiff",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "config",
                "mergetool.vimdiff.path",
                tool.to_str().expect("tool path is utf-8"),
            ],
            root,
        ),
        "configure vimdiff path",
    );

    assert_cli_success(
        &run_libra_command(&["mergetool"], root),
        "configured path is selected before PATH lookup",
    );
    assert_eq!(
        std::fs::read_to_string(root.join("conflict.txt")).expect("resolved file"),
        "path override\n"
    );
}

#[test]
fn mergetool_keep_backup_false_does_not_create_orig() {
    let repo = conflicted_repo();
    let root = repo.path();
    configure_cmd(root, "no-backup", "printf 'no backup\\n' > \"$MERGED\"");
    assert_cli_success(
        &run_libra_command(&["config", "mergetool.keepBackup", "false"], root),
        "disable backup",
    );

    assert_cli_success(&run_libra_command(&["mergetool"], root), "resolve conflict");
    assert!(
        !root.join("conflict.txt.orig").exists(),
        "keepBackup=false does not add a new .orig file"
    );
}

#[test]
fn mergetool_explicit_tool_overrides_merge_tool() {
    let repo = conflicted_repo();
    let root = repo.path();
    configure_cmd(root, "default-tool", "exit 0");
    assert_cli_success(
        &run_libra_command(
            &[
                "config",
                "mergetool.explicit-tool.cmd",
                "printf 'explicit tool\\n' > \"$MERGED\"",
            ],
            root,
        ),
        "configure explicit tool",
    );

    assert_cli_success(
        &run_libra_command(&["mergetool", "--tool", "explicit-tool"], root),
        "explicit tool overrides merge.tool",
    );
    assert_eq!(
        std::fs::read_to_string(root.join("conflict.txt")).expect("resolved file"),
        "explicit tool\n"
    );
}

#[test]
fn mergetool_rejects_deferred_configuration_instead_of_ignoring_it() {
    let repo = conflicted_repo();
    let root = repo.path();
    configure_cmd(root, "deferred", "printf 'must not run\\n' > \"$MERGED\"");
    assert_cli_success(
        &run_libra_command(&["config", "mergetool.futureToolPreference", "true"], root),
        "configure deferred mergetool option",
    );

    let output = run_libra_command(&["mergetool"], root);
    assert!(
        !output.status.success(),
        "deferred setting must fail closed"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("mergetool.futureToolPreference' is not supported"),
        "the error explains why the command did not run"
    );
    assert!(
        std::fs::read_to_string(root.join("conflict.txt"))
            .expect("working tree")
            .contains("<<<<<<<"),
        "the configured tool was not invoked"
    );
}

#[test]
fn mergetool_reports_no_work_and_unknown_tools_actionably() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    let no_work = run_libra_command(&["mergetool"], root);
    assert!(!no_work.status.success(), "no conflict must fail");
    assert!(
        String::from_utf8_lossy(&no_work.stderr).contains("no files need merging"),
        "no-work diagnostic is actionable"
    );

    let conflicted = conflicted_repo();
    let unknown = run_libra_command(
        &["mergetool", "--tool", "not-configured"],
        conflicted.path(),
    );
    assert!(!unknown.status.success(), "unknown tool must fail");
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("unknown merge tool 'not-configured'"),
        "unknown-tool diagnostic explains how to configure it"
    );
}

#[test]
fn mergetool_tool_help_lists_the_builtins() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["mergetool", "--tool-help"], repo.path());
    assert_cli_success(&output, "tool help");
    let text = String::from_utf8(output.stdout).expect("tool help is utf-8");
    for tool in ["vimdiff", "nvimdiff", "meld", "vscode", "opendiff"] {
        assert!(text.contains(tool), "tool-help must list {tool}: {text}");
    }
}

#[test]
fn mergetool_reports_modify_delete_conflicts_instead_of_skipping_them() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    commit_file(
        root,
        "modify-delete.txt",
        "base\n",
        "add modify-delete base",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "delete-side"], root),
        "create delete branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "delete-side"], root),
        "checkout delete branch",
    );
    assert_cli_success(
        &run_libra_command(&["rm", "-f", "modify-delete.txt"], root),
        "stage deletion",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "delete file", "--no-verify"], root),
        "commit deletion",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], root),
        "return to main",
    );
    commit_file(root, "modify-delete.txt", "ours changed\n", "modify file");
    let merge = run_libra_command(&["merge", "delete-side", "--no-verify"], root);
    assert_eq!(merge.status.code(), Some(128), "fixture merge conflicts");
    configure_cmd(root, "manual", "printf ignored > \"$MERGED\"");

    let output = run_libra_command(&["mergetool"], root);
    assert!(
        !output.status.success(),
        "modify/delete requires manual handling"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("modify/delete conflict"),
        "the diagnostic names the unsupported conflict shape"
    );
}

#[cfg(unix)]
#[test]
fn mergetool_reports_symlink_or_mode_conflicts_instead_of_skipping_them() {
    use std::os::unix::fs::symlink;

    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    commit_file(
        root,
        "type-conflict.txt",
        "base\n",
        "add type conflict base",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "link-side"], root),
        "create link branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "link-side"], root),
        "checkout link branch",
    );
    std::fs::remove_file(root.join("type-conflict.txt")).expect("remove regular file");
    symlink("feature-target", root.join("type-conflict.txt")).expect("create feature symlink");
    assert_cli_success(
        &run_libra_command(&["add", "type-conflict.txt"], root),
        "stage feature symlink",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "make link", "--no-verify"], root),
        "commit feature symlink",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], root),
        "return to main",
    );
    commit_file(
        root,
        "type-conflict.txt",
        "ours changed\n",
        "modify regular file",
    );
    let merge = run_libra_command(&["merge", "link-side", "--no-verify"], root);
    assert_eq!(merge.status.code(), Some(128), "fixture merge conflicts");
    configure_cmd(root, "manual-link", "printf ignored > \"$MERGED\"");

    let output = run_libra_command(&["mergetool"], root);
    assert!(
        !output.status.success(),
        "symlink/mode requires manual handling"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("symlink or mode conflict"),
        "the diagnostic names the unsupported conflict shape"
    );
}

#[cfg(unix)]
#[test]
fn mergetool_refuses_a_regular_conflict_replaced_by_a_worktree_symlink() {
    let repo = conflicted_repo();
    let root = repo.path();
    std::fs::write(root.join("outside.txt"), "must not be exposed\n").expect("write target");
    std::fs::remove_file(root.join("conflict.txt")).expect("remove conflict file");
    std::os::unix::fs::symlink("outside.txt", root.join("conflict.txt"))
        .expect("replace conflict file with symlink");
    configure_cmd(
        root,
        "manual-link",
        "touch resolver-started; printf 'must not run\\n' > \"$MERGED\"",
    );

    let output = run_libra_command(&["mergetool"], root);
    assert!(
        !output.status.success(),
        "a worktree symlink must not be followed into a tool"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("working-tree path is a symbolic link"),
        "the refusal identifies the untrusted worktree shape"
    );
    assert!(
        !root.join("resolver-started").exists(),
        "the resolver command is not started before the symlink refusal"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("outside.txt")).expect("outside target"),
        "must not be exposed\n",
        "the resolver command never reads or writes the symlink target"
    );
}

#[cfg(unix)]
#[test]
fn mergetool_refuses_a_conflict_below_a_symlinked_ancestor_before_starting() {
    let repo = conflicted_repo_at("nested/conflict.txt");
    let root = repo.path();
    let outside = tempfile::tempdir().expect("create outside directory");
    std::fs::write(outside.path().join("conflict.txt"), "must not be exposed\n")
        .expect("write outside target");
    std::fs::remove_file(root.join("nested/conflict.txt")).expect("remove conflict file");
    std::fs::remove_dir(root.join("nested")).expect("remove conflict parent");
    std::os::unix::fs::symlink(outside.path(), root.join("nested"))
        .expect("replace conflict parent with symlink");
    configure_cmd(
        root,
        "ancestor-link",
        "touch resolver-started; printf 'must not run\\n' > \"$MERGED\"",
    );

    let output = run_libra_command(&["mergetool"], root);
    assert!(
        !output.status.success(),
        "a symlinked ancestor must be refused before the resolver starts"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unsafe ancestor"),
        "the refusal identifies the ancestor boundary"
    );
    assert!(
        !root.join("resolver-started").exists(),
        "the resolver command is not started before the ancestor refusal"
    );
    assert_eq!(
        std::fs::read_to_string(outside.path().join("conflict.txt")).expect("outside target"),
        "must not be exposed\n",
        "the resolver never reads or writes through the symlinked parent"
    );
}

#[cfg(unix)]
#[test]
fn mergetool_refuses_a_parent_swapped_to_a_symlink_while_the_tool_runs() {
    let repo = conflicted_repo_at("nested/conflict.txt");
    let root = repo.path();
    let outside = tempfile::tempdir().expect("create outside directory");
    std::fs::write(
        outside.path().join("conflict.txt"),
        "must not be overwritten\n",
    )
    .expect("write outside target");
    let outside_path = outside
        .path()
        .to_str()
        .expect("temporary directory path is utf-8");
    configure_cmd(
        root,
        "swap-parent",
        &format!(
            "rm nested/conflict.txt && rmdir nested && ln -s '{outside_path}' nested && printf 'resolved by tool\\n' > \"$MERGED\""
        ),
    );

    let output = run_libra_command(&["mergetool"], root);
    assert!(
        !output.status.success(),
        "a parent swapped while the resolver runs must not be followed on write-back"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("without following a symbolic link"),
        "the write-back failure names its no-follow boundary"
    );
    assert_eq!(
        std::fs::read_to_string(outside.path().join("conflict.txt")).expect("outside target"),
        "must not be overwritten\n",
        "the post-tool write never follows the swapped parent"
    );
    assert!(
        index_stages(root, "nested/conflict.txt")
            .iter()
            .any(|line| line.contains(" 2\t")),
        "a refused write-back leaves the conflict stages untouched"
    );
}
