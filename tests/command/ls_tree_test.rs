//! Integration tests for `ls-tree`.
//!
//! **Layer:** L1 — deterministic local repositories, no network.

use std::fs;

use git_internal::{
    hash::ObjectHash,
    internal::object::{
        blob::Blob,
        tree::{Tree, TreeItem, TreeItemMode},
        types::ObjectType,
    },
};

use super::*;

fn setup_ls_tree_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::create_dir_all(repo.path().join("src/nested")).expect("create nested dir");
    fs::write(repo.path().join("README.md"), "readme\n").expect("write README");
    fs::write(repo.path().join("src/lib.rs"), "pub fn lib() {}\n").expect("write lib");
    fs::write(repo.path().join("src/nested/deep.txt"), "deep\n").expect("write deep");
    fs::write(repo.path().join("space name.txt"), "space\n").expect("write space path");
    fs::write(repo.path().join("中文.txt"), "unicode\n").expect("write unicode path");
    fs::write(repo.path().join("script.sh"), "#!/bin/sh\nexit 0\n").expect("write script");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(repo.path().join("script.sh"))
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(repo.path().join("script.sh"), permissions)
            .expect("set executable bit");
    }

    let add_args = vec![
        "add",
        ".libraignore",
        "README.md",
        "src/lib.rs",
        "src/nested/deep.txt",
        "space name.txt",
        "中文.txt",
        "script.sh",
    ];
    let output = run_libra_command(&add_args, repo.path());
    assert_cli_success(&output, "failed to add ls-tree fixture files");

    let output = run_libra_command(
        &["commit", "-m", "tree fixture", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&output, "failed to commit ls-tree fixture");
    repo
}

fn stdout_string(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stdout_trimmed(output: &std::process::Output) -> String {
    stdout_string(output)
        .trim_end_matches(['\n', '\0'])
        .to_string()
}

fn root_tree_hash(repo: &std::path::Path) -> String {
    let output = run_libra_command(&["cat-file", "-p", "HEAD"], repo);
    assert_cli_success(&output, "cat-file -p HEAD should succeed");
    stdout_string(&output)
        .lines()
        .find_map(|line| line.strip_prefix("tree "))
        .expect("commit pretty output should contain tree line")
        .to_string()
}

fn blob_hash_for(repo: &std::path::Path, path: &str) -> String {
    let output = run_libra_command(&["hash-object", path], repo);
    assert_cli_success(&output, "hash-object should compute blob hash");
    stdout_trimmed(&output)
}

#[test]
fn ls_tree_default_lists_root_entries() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "HEAD"], repo.path());
    assert_cli_success(&output, "ls-tree HEAD should succeed");
    let stdout = stdout_string(&output);

    assert!(stdout.contains("100644 blob "));
    assert!(stdout.contains("\tREADME.md\n"));
    assert!(stdout.contains("040000 tree "));
    assert!(stdout.contains("\tsrc\n"));
}

#[test]
fn ls_tree_from_subdirectory_defaults_to_current_directory() {
    let repo = setup_ls_tree_repo();
    let src_dir = repo.path().join("src");

    let output = run_libra_command(&["ls-tree", "HEAD"], &src_dir);
    assert_cli_success(&output, "ls-tree HEAD from src should succeed");
    let stdout = stdout_string(&output);

    assert!(stdout.contains("\tlib.rs\n"));
    assert!(stdout.contains("\tnested\n"));
    assert!(!stdout.contains("\tREADME.md\n"));
    assert!(!stdout.contains("\tsrc/lib.rs\n"));
}

#[test]
fn ls_tree_full_name_from_subdirectory_keeps_repository_paths() {
    let repo = setup_ls_tree_repo();
    let src_dir = repo.path().join("src");

    let output = run_libra_command(&["ls-tree", "--full-name", "HEAD"], &src_dir);
    assert_cli_success(&output, "ls-tree --full-name from src should succeed");
    let stdout = stdout_string(&output);

    assert!(stdout.contains("\tsrc/lib.rs\n"));
    assert!(stdout.contains("\tsrc/nested\n"));
    assert!(!stdout.contains("\tlib.rs\n"));
}

#[test]
fn ls_tree_full_tree_from_subdirectory_lists_repository_root() {
    let repo = setup_ls_tree_repo();
    let src_dir = repo.path().join("src");

    let output = run_libra_command(&["ls-tree", "--full-tree", "HEAD"], &src_dir);
    assert_cli_success(&output, "ls-tree --full-tree from src should succeed");
    let stdout = stdout_string(&output);

    assert!(stdout.contains("\tREADME.md\n"));
    assert!(stdout.contains("\tsrc\n"));
    assert!(!stdout.contains("\tlib.rs\n"));
}

#[test]
fn ls_tree_subdirectory_path_filter_is_current_directory_relative() {
    let repo = setup_ls_tree_repo();
    let src_dir = repo.path().join("src");

    let output = run_libra_command(&["ls-tree", "HEAD", "lib.rs"], &src_dir);
    assert_cli_success(&output, "ls-tree HEAD lib.rs from src should succeed");
    assert!(stdout_string(&output).contains("\tlib.rs\n"));

    let full_name = run_libra_command(&["ls-tree", "--full-name", "HEAD", "lib.rs"], &src_dir);
    assert_cli_success(
        &full_name,
        "ls-tree --full-name HEAD lib.rs from src should succeed",
    );
    assert!(stdout_string(&full_name).contains("\tsrc/lib.rs\n"));
}

#[test]
fn ls_tree_accepts_branch_treeish() {
    let repo = setup_ls_tree_repo();
    let branch = run_libra_command(&["branch", "topic"], repo.path());
    assert_cli_success(&branch, "branch topic should succeed");

    let output = run_libra_command(&["ls-tree", "topic", "README.md"], repo.path());
    assert_cli_success(&output, "ls-tree branch path should succeed");

    assert!(stdout_string(&output).contains("\tREADME.md\n"));
}

#[test]
fn ls_tree_accepts_tag_treeish() {
    let repo = setup_ls_tree_repo();
    let tag = run_libra_command(&["tag", "v1.0"], repo.path());
    assert_cli_success(&tag, "tag v1.0 should succeed");

    let output = run_libra_command(&["ls-tree", "v1.0", "README.md"], repo.path());
    assert_cli_success(&output, "ls-tree tag path should succeed");

    assert!(stdout_string(&output).contains("\tREADME.md\n"));
}

#[test]
fn ls_tree_accepts_direct_tree_hash() {
    let repo = setup_ls_tree_repo();
    let tree = root_tree_hash(repo.path());

    let output = run_libra_command(&["ls-tree", &tree, "README.md"], repo.path());
    assert_cli_success(&output, "ls-tree tree hash should succeed");

    assert!(stdout_string(&output).contains("\tREADME.md\n"));
}

#[test]
fn ls_tree_recursive_lists_nested_entries_without_parent_trees() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "-r", "HEAD"], repo.path());
    assert_cli_success(&output, "ls-tree -r HEAD should succeed");
    let stdout = stdout_string(&output);

    assert!(stdout.contains("\tsrc/lib.rs\n"));
    assert!(stdout.contains("\tsrc/nested/deep.txt\n"));
    assert!(!stdout.contains("\tsrc\n"));
}

#[test]
fn ls_tree_recursive_t_includes_tree_entries() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "-r", "-t", "HEAD"], repo.path());
    assert_cli_success(&output, "ls-tree -r -t HEAD should succeed");
    let stdout = stdout_string(&output);

    assert!(stdout.contains("\tsrc\n"));
    assert!(stdout.contains("\tsrc/nested\n"));
    assert!(stdout.contains("\tsrc/nested/deep.txt\n"));
}

#[test]
fn ls_tree_directory_filter_lists_one_level_by_default() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "HEAD", "src"], repo.path());
    assert_cli_success(&output, "ls-tree HEAD src should succeed");
    let stdout = stdout_string(&output);

    assert!(stdout.contains("\tsrc/lib.rs\n"));
    assert!(stdout.contains("\tsrc/nested\n"));
    assert!(!stdout.contains("\tsrc/nested/deep.txt\n"));
}

#[test]
fn ls_tree_directory_filter_recurses_when_requested() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "-r", "HEAD", "src"], repo.path());
    assert_cli_success(&output, "ls-tree -r HEAD src should succeed");
    let stdout = stdout_string(&output);

    assert!(stdout.contains("\tsrc/lib.rs\n"));
    assert!(stdout.contains("\tsrc/nested/deep.txt\n"));
}

#[test]
fn ls_tree_d_on_directory_outputs_entry_only() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "-d", "HEAD", "src"], repo.path());
    assert_cli_success(&output, "ls-tree -d HEAD src should succeed");
    let stdout = stdout_string(&output);

    assert!(stdout.contains("\tsrc\n"));
    assert!(!stdout.contains("src/lib.rs"));
}

#[test]
fn ls_tree_d_recursive_path_lists_nested_tree_entries() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "-d", "-r", "HEAD", "src"], repo.path());
    assert_cli_success(&output, "ls-tree -d -r HEAD src should succeed");
    let stdout = stdout_string(&output);

    assert!(stdout.contains("\tsrc\n"));
    assert!(stdout.contains("\tsrc/nested\n"));
    assert!(!stdout.contains("src/lib.rs"));
    assert!(!stdout.contains("src/nested/deep.txt"));
}

#[test]
fn ls_tree_d_without_path_lists_tree_entries_only() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "-d", "HEAD"], repo.path());
    assert_cli_success(&output, "ls-tree -d HEAD should succeed");
    let stdout = stdout_string(&output);

    assert!(stdout.contains("\tsrc\n"));
    assert!(!stdout.contains("\tREADME.md\n"));
}

#[test]
fn ls_tree_long_includes_blob_size_and_tree_dash() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "-l", "HEAD"], repo.path());
    assert_cli_success(&output, "ls-tree -l HEAD should succeed");
    let stdout = stdout_string(&output);

    assert!(stdout.contains("      7\tREADME.md"));
    assert!(stdout.contains("      -\tsrc"));
}

#[test]
fn ls_tree_z_uses_nul_terminated_records() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "-z", "HEAD", "README.md"], repo.path());
    assert_cli_success(&output, "ls-tree -z should succeed");

    assert!(output.stdout.ends_with(&[0]));
    assert!(!output.stdout.ends_with(b"\n"));
}

#[test]
fn ls_tree_name_only_prints_paths() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(
        &["ls-tree", "--name-only", "HEAD", "README.md"],
        repo.path(),
    );
    assert_cli_success(&output, "ls-tree --name-only should succeed");

    assert_eq!(stdout_trimmed(&output), "README.md");
}

#[test]
fn ls_tree_name_status_is_path_only_alias() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(
        &["ls-tree", "--name-status", "HEAD", "README.md"],
        repo.path(),
    );
    assert_cli_success(&output, "ls-tree --name-status should succeed");

    assert_eq!(stdout_trimmed(&output), "README.md");
}

#[test]
fn ls_tree_object_only_honors_abbrev_width() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(
        &[
            "ls-tree",
            "--object-only",
            "--abbrev=10",
            "HEAD",
            "README.md",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "ls-tree --object-only --abbrev should succeed");

    assert_eq!(stdout_trimmed(&output).len(), 10);
}

#[test]
fn ls_tree_handles_space_and_unicode_paths() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "--name-only", "HEAD"], repo.path());
    assert_cli_success(&output, "ls-tree --name-only HEAD should succeed");
    let stdout = stdout_string(&output);

    assert!(stdout.contains("space name.txt\n"));
    assert!(stdout.contains("中文.txt\n"));
}

#[cfg(unix)]
#[test]
fn ls_tree_reports_executable_file_mode() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "HEAD", "script.sh"], repo.path());
    assert_cli_success(&output, "ls-tree executable should succeed");

    assert!(stdout_string(&output).starts_with("100755 blob "));
}

#[cfg(unix)]
#[test]
#[serial(cwd)]
fn ls_tree_reports_symlink_as_blob_mode() {
    let repo = setup_ls_tree_repo();
    let _guard = ChangeDirGuard::new(repo.path());
    let link_blob = Blob::from_content("README.md");
    save_object(&link_blob, &link_blob.id).expect("save symlink blob");
    let tree = Tree::from_tree_items(vec![TreeItem::new(
        TreeItemMode::Link,
        link_blob.id,
        "readme-link".to_string(),
    )])
    .expect("build symlink tree");
    save_object(&tree, &tree.id).expect("save symlink tree");

    let output = run_libra_command(
        &["ls-tree", &tree.id.to_string(), "readme-link"],
        repo.path(),
    );
    assert_cli_success(&output, "ls-tree symlink should succeed");

    assert!(stdout_string(&output).starts_with("120000 blob "));
}

#[test]
fn ls_tree_json_lists_entries() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["--json", "ls-tree", "HEAD"], repo.path());
    assert_cli_success(&output, "ls-tree --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "ls-tree");
    assert_eq!(json["data"]["treeish"], "HEAD");
    let entries = json["data"]["entries"].as_array().expect("entries array");
    assert!(entries.iter().any(|entry| entry["path"] == "README.md"));
}

#[test]
fn ls_tree_json_recursive_path_lists_nested_entry() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["--json", "ls-tree", "-r", "HEAD", "src"], repo.path());
    assert_cli_success(&output, "ls-tree --json -r should succeed");

    let json = parse_json_stdout(&output);
    let entries = json["data"]["entries"].as_array().expect("entries array");
    assert!(
        entries
            .iter()
            .any(|entry| entry["path"] == "src/nested/deep.txt")
    );
}

#[test]
fn ls_tree_invalid_treeish_fails() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "NO_SUCH_REF"], repo.path());

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not a valid tree-ish"));
}

#[test]
fn ls_tree_blob_treeish_fails() {
    let repo = setup_ls_tree_repo();
    let blob = blob_hash_for(repo.path(), "README.md");

    let output = run_libra_command(&["ls-tree", &blob], repo.path());

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("is not a tree-ish"));
}

#[test]
fn ls_tree_rev_path_blob_target_errors_as_not_a_tree() {
    let repo = setup_ls_tree_repo();

    // `REV:path` is supported, but it must name a tree; targeting a blob is a
    // clear "not a tree object" error (see the navigation test for the
    // supported subtree cases).
    let output = run_libra_command(&["ls-tree", "HEAD:README.md"], repo.path());

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not a tree object"),
        "a blob REV:path should report not-a-tree: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn ls_tree_missing_path_fails() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "HEAD", "missing.txt"], repo.path());

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not exist"));
}

#[test]
fn ls_tree_invalid_abbrev_width_fails() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(&["ls-tree", "--abbrev=3", "HEAD"], repo.path());

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--abbrev must be at least 4"));
}

#[test]
fn ls_tree_rejects_conflicting_output_modes() {
    let repo = setup_ls_tree_repo();

    let output = run_libra_command(
        &["ls-tree", "--name-only", "--object-only", "HEAD"],
        repo.path(),
    );

    assert!(!output.status.success());
}

#[test]
fn ls_tree_json_error_uses_structured_envelope() {
    let repo = setup_ls_tree_repo();

    // `REV:path` is supported; pointing it at a blob is a structured
    // not-a-tree error (LBR-CLI-003), exercising the JSON error envelope.
    let output = run_libra_command(&["--json", "ls-tree", "HEAD:README.md"], repo.path());

    assert!(!output.status.success());
    let (_human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
}

#[test]
#[serial(cwd)]
fn ls_tree_empty_tree_hash_outputs_nothing() {
    let repo = setup_ls_tree_repo();
    let _guard = ChangeDirGuard::new(repo.path());
    let empty_tree = Tree {
        id: ObjectHash::from_type_and_data(ObjectType::Tree, &[]),
        tree_items: Vec::new(),
    };
    save_object(&empty_tree, &empty_tree.id).expect("save empty tree");

    let output = run_libra_command(&["ls-tree", &empty_tree.id.to_string()], repo.path());
    assert_cli_success(&output, "ls-tree empty tree hash should succeed");

    assert!(output.stdout.is_empty());
}

#[test]
fn ls_tree_help_lists_examples_banner() {
    let cwd = tempdir().expect("create help cwd");
    let output = run_libra_command(&["ls-tree", "--help"], cwd.path());
    assert_cli_success(&output, "ls-tree --help should succeed");

    assert!(stdout_string(&output).contains("EXAMPLES:"));
}

#[test]
fn test_ls_tree_rev_path_navigates_into_subtree() {
    let repo = setup_ls_tree_repo();
    let p = repo.path();

    // HEAD:src lists src's children with names relative to src.
    let out = run_libra_command(&["ls-tree", "HEAD:src"], p);
    assert_cli_success(&out, "ls-tree HEAD:src");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(s.contains("\tlib.rs"), "src child lib.rs: {s:?}");
    assert!(s.contains("\tnested"), "src child nested: {s:?}");
    assert!(!s.contains("src/"), "paths must be relative to src: {s:?}");
    assert!(!s.contains("README.md"), "only src contents listed: {s:?}");

    // HEAD:src/nested lists the deeper subtree.
    let out2 = run_libra_command(&["ls-tree", "HEAD:src/nested"], p);
    assert_cli_success(&out2, "ls-tree HEAD:src/nested");
    assert!(
        String::from_utf8_lossy(&out2.stdout).contains("\tdeep.txt"),
        "nested child deep.txt"
    );

    // HEAD: lists the root tree.
    let out3 = run_libra_command(&["ls-tree", "HEAD:"], p);
    assert_cli_success(&out3, "ls-tree HEAD:");
    assert!(
        String::from_utf8_lossy(&out3.stdout).contains("\tREADME.md"),
        "root tree has README.md"
    );

    // REV:path pointing at a blob is an error (not a tree).
    assert!(
        !run_libra_command(&["ls-tree", "HEAD:README.md"], p)
            .status
            .success(),
        "a blob path is not a tree"
    );
    // REV:path pointing at a missing path is an error.
    assert!(
        !run_libra_command(&["ls-tree", "HEAD:nope"], p)
            .status
            .success(),
        "a missing path errors"
    );
}
