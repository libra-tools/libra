//! Integration tests for `ls-tree`.
//!
//! **Layer:** L1 — deterministic local repositories, no network.

use std::fs;

#[cfg(unix)]
use git_internal::internal::object::{
    blob::Blob,
    tree::{TreeItem, TreeItemMode},
};
use git_internal::{
    hash::ObjectHash,
    internal::object::{tree::Tree, types::ObjectType},
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
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "HEAD", "dir"],
        issue1057_golden("root"),
    );
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

// Recorded from Git 6de20f6092dcf9bdb1c8efe03db4b70c82b423dd; the plan
// binds every sample to its byte count and SHA-256. Never derive these from Libra.
const ISSUE1057_GOLDEN: &[(&str, usize, &str)] = &[
    (
        "root",
        57,
        "c9de48a7d325ca3fda206a1342e841a9a4d3940adf0b69de6e2fe9f0e9c21450",
    ),
    (
        "slash",
        127,
        "0cfa1f84cf536f15a0b8f44babaeb5625eb27db6b538d72fa23644ca1b580ff5",
    ),
    (
        "mixed",
        127,
        "0cfa1f84cf536f15a0b8f44babaeb5625eb27db6b538d72fa23644ca1b580ff5",
    ),
    (
        "mixed-reverse",
        127,
        "0cfa1f84cf536f15a0b8f44babaeb5625eb27db6b538d72fa23644ca1b580ff5",
    ),
    (
        "gitlink-slash",
        60,
        "02a1b9beec221b1c393c3bc3f69744c60e1ae2239da4d0e71fc64c152f09d66d",
    ),
    (
        "recursive",
        132,
        "a90aa272fe774245babd86bfa98b266b0560893a473e08b5803bdad05c953de1",
    ),
    (
        "t",
        57,
        "c9de48a7d325ca3fda206a1342e841a9a4d3940adf0b69de6e2fe9f0e9c21450",
    ),
    (
        "recursive-t",
        253,
        "7802185b8dc9040c6945d238357309513981268356dfd8a0113ee696ec0c5bea",
    ),
    (
        "dir-dot",
        127,
        "0cfa1f84cf536f15a0b8f44babaeb5625eb27db6b538d72fa23644ca1b580ff5",
    ),
    (
        "dir-dot-slash",
        127,
        "0cfa1f84cf536f15a0b8f44babaeb5625eb27db6b538d72fa23644ca1b580ff5",
    ),
    (
        "root-dot",
        235,
        "9506223ded550c9170133b5162178854d1e29cf363755365517ebcd9f54345a7",
    ),
    (
        "root-dot-slash",
        235,
        "9506223ded550c9170133b5162178854d1e29cf363755365517ebcd9f54345a7",
    ),
    (
        "nested-t",
        121,
        "25333f9abfb440a4112826caeb4d5b385f7df56ad8a96f09c8702e471ce85b58",
    ),
    (
        "nested-t-slash",
        190,
        "625d390dc5b1ecc215a0d13a22b3c533d6bda53d08caea859a2f0cda7265a5bd",
    ),
    (
        "nested-r-t",
        190,
        "625d390dc5b1ecc215a0d13a22b3c533d6bda53d08caea859a2f0cda7265a5bd",
    ),
    (
        "t-slash",
        184,
        "9f90c078fe0d1542d835c897011121cd38e401ee84478fc720a0d5d3d8cb966a",
    ),
    (
        "t-blob",
        120,
        "8c45524bf7ad201828547bb06bd3488505e516fbecd551b5eb05586c01021918",
    ),
    (
        "t-deep-blob",
        190,
        "625d390dc5b1ecc215a0d13a22b3c533d6bda53d08caea859a2f0cda7265a5bd",
    ),
    (
        "full-name",
        64,
        "f260adea835b2b85a89962f59ded7361d8f0128363ee353488a5afef3e1f41b0",
    ),
    (
        "dot",
        119,
        "69b3b664e57c1fd52fbc176ce518306c354f57ca01e9d40fc8d50cc5cbd6cded",
    ),
    (
        "subdir-plain",
        60,
        "c0f8fc162e2a2bcf25d5faf497a6ba3cc5dba81fc4e667d614918310274cbfd4",
    ),
    (
        "subdir-slash",
        65,
        "43654d95e3202ad5c98e7fee1393560198574cf3a3c87a02bc8439844f71beb5",
    ),
    (
        "subdir-full-tree",
        57,
        "c9de48a7d325ca3fda206a1342e841a9a4d3940adf0b69de6e2fe9f0e9c21450",
    ),
    (
        "dot-dir",
        57,
        "c9de48a7d325ca3fda206a1342e841a9a4d3940adf0b69de6e2fe9f0e9c21450",
    ),
    (
        "overlap-child",
        127,
        "0cfa1f84cf536f15a0b8f44babaeb5625eb27db6b538d72fa23644ca1b580ff5",
    ),
    (
        "slash-overlap-child",
        127,
        "0cfa1f84cf536f15a0b8f44babaeb5625eb27db6b538d72fa23644ca1b580ff5",
    ),
    (
        "recursive-slash",
        132,
        "a90aa272fe774245babd86bfa98b266b0560893a473e08b5803bdad05c953de1",
    ),
    (
        "recursive-t-slash",
        253,
        "7802185b8dc9040c6945d238357309513981268356dfd8a0113ee696ec0c5bea",
    ),
    (
        "t-overlap-child",
        184,
        "9f90c078fe0d1542d835c897011121cd38e401ee84478fc720a0d5d3d8cb966a",
    ),
];

fn issue1057_golden(name: &str) -> &'static [u8] {
    use sha2::{Digest, Sha256};
    let bytes: &'static [u8] = match name {
        "root" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n",
        "slash" => b"040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tdir/other\n",
        "mixed" => b"040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tdir/other\n",
        "mixed-reverse" => b"040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tdir/other\n",
        "gitlink-slash" => b"160000 commit 1111111111111111111111111111111111111111\tlink\n",
        "recursive" => b"100644 blob ce013625030ba8dba906f756967f9e9ca394464a\tdir/nested/file\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tdir/other\n",
        "t" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n",
        "recursive-t" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n100644 blob ce013625030ba8dba906f756967f9e9ca394464a\tdir/nested/file\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tdir/other\n",
        "dir-dot" => b"040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tdir/other\n",
        "dir-dot-slash" => b"040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tdir/other\n",
        "root-dot" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tempty\n100644 blob 5630165e4a6c588e6657164f014b0afc169f29e4\tlarge\n160000 commit 1111111111111111111111111111111111111111\tlink\n",
        "root-dot-slash" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tempty\n100644 blob 5630165e4a6c588e6657164f014b0afc169f29e4\tlarge\n160000 commit 1111111111111111111111111111111111111111\tlink\n",
        "nested-t" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n",
        "nested-t-slash" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n100644 blob ce013625030ba8dba906f756967f9e9ca394464a\tdir/nested/file\n",
        "nested-r-t" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n100644 blob ce013625030ba8dba906f756967f9e9ca394464a\tdir/nested/file\n",
        "t-slash" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tdir/other\n",
        "t-blob" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tdir/other\n",
        "t-deep-blob" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n100644 blob ce013625030ba8dba906f756967f9e9ca394464a\tdir/nested/file\n",
        "full-name" => b"040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n",
        "dot" => b"040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tnested\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tother\n",
        "subdir-plain" => b"040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tnested\n",
        "subdir-slash" => b"100644 blob ce013625030ba8dba906f756967f9e9ca394464a\tnested/file\n",
        "subdir-full-tree" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n",
        "dot-dir" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n",
        "overlap-child" => b"040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tdir/other\n",
        "slash-overlap-child" => b"040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tdir/other\n",
        "recursive-slash" => b"100644 blob ce013625030ba8dba906f756967f9e9ca394464a\tdir/nested/file\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tdir/other\n",
        "recursive-t-slash" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n100644 blob ce013625030ba8dba906f756967f9e9ca394464a\tdir/nested/file\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tdir/other\n",
        "t-overlap-child" => b"040000 tree 0057ceb4b6bcb4f4166bcd617df7b4477a116273\tdir\n040000 tree fb5a86199f63243160ee5b463d2cd5c36fafeb6d\tdir/nested\n100644 blob c1b0730e0133447badcfd47fd144e254807b06e1\tdir/other\n",
        _ => panic!("unknown issue1057 golden: {name}"),
    };
    let (_, len, sha) = ISSUE1057_GOLDEN
        .iter()
        .find(|row| row.0 == name)
        .expect("registered golden");
    assert_eq!(bytes.len(), *len, "golden {name} length");
    assert_eq!(
        format!("{:x}", Sha256::digest(bytes)),
        *sha,
        "golden {name} digest"
    );
    bytes
}

fn setup_issue1057_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("create issue1057 repo");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::create_dir_all(p.join("dir/nested")).expect("create directories");
    fs::write(p.join("dir/nested/file"), b"hello\n").expect("write six-byte blob");
    fs::write(p.join("dir/other"), b"x").expect("write other blob");
    fs::write(p.join("empty"), b"").expect("write empty blob");
    fs::File::create(p.join("large"))
        .expect("create large blob")
        .set_len(10_000_000)
        .expect("size large blob");
    assert_cli_success(
        &run_libra_command(&["add", "dir", "empty", "large"], p),
        "add fixture",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,1111111111111111111111111111111111111111,link",
            ],
            p,
        ),
        "add gitlink",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "fixture", "--no-verify"], p),
        "commit fixture",
    );
    repo
}

fn assert_issue1057_listing(cwd: &std::path::Path, args: &[&str], expected: &[u8]) {
    let output = run_libra_command(args, cwd);
    assert_cli_success(&output, &format!("issue1057 {args:?}"));
    assert_eq!(output.stdout, expected, "issue1057 {args:?}");
}

#[test]
fn ls_tree_issue1057_full_name_directory() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        &repo.path().join("dir"),
        &["ls-tree", "--full-name", "HEAD", "nested"],
        issue1057_golden("full-name"),
    );
}

#[test]
fn ls_tree_issue1057_trailing_slash() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "HEAD", "dir/"],
        issue1057_golden("slash"),
    );
}

#[test]
fn ls_tree_issue1057_mixed_directory() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "HEAD", "dir", "dir//"],
        issue1057_golden("mixed"),
    );
}

#[test]
fn ls_tree_issue1057_recursive() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "-r", "HEAD", "dir"],
        issue1057_golden("recursive"),
    );
}

#[test]
fn ls_tree_issue1057_t_without_recursive() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "-t", "HEAD", "dir"],
        issue1057_golden("t"),
    );
}

#[test]
fn ls_tree_issue1057_recursive_t() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "-r", "-t", "HEAD", "dir"],
        issue1057_golden("recursive-t"),
    );
}

#[test]
fn ls_tree_issue1057_root_directory() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "HEAD", "dir"],
        issue1057_golden("root"),
    );
}

#[test]
fn ls_tree_issue1057_subdir_dot() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        &repo.path().join("dir"),
        &["ls-tree", "HEAD", "."],
        issue1057_golden("dot"),
    );
}

#[test]
fn ls_tree_issue1057_mixed_reverse() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "HEAD", "dir//", "dir"],
        issue1057_golden("mixed-reverse"),
    );
}

#[test]
fn ls_tree_issue1057_gitlink_slash() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "HEAD", "link/"],
        issue1057_golden("gitlink-slash"),
    );
}

#[test]
fn ls_tree_issue1057_trailing_dot() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "HEAD", "dir/."],
        issue1057_golden("dir-dot"),
    );
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "HEAD", "dir/./"],
        issue1057_golden("dir-dot-slash"),
    );
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "HEAD", "."],
        issue1057_golden("root-dot"),
    );
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "HEAD", "./"],
        issue1057_golden("root-dot-slash"),
    );
}

#[test]
fn ls_tree_issue1057_nested_t() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "-t", "HEAD", "dir/nested"],
        issue1057_golden("nested-t"),
    );
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "-t", "HEAD", "dir/nested/"],
        issue1057_golden("nested-t-slash"),
    );
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "-r", "-t", "HEAD", "dir/nested"],
        issue1057_golden("nested-r-t"),
    );
}

#[test]
fn ls_tree_issue1057_t_slash_and_blob() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "-t", "HEAD", "dir/"],
        issue1057_golden("t-slash"),
    );
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "-t", "HEAD", "dir/other"],
        issue1057_golden("t-blob"),
    );
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "-t", "HEAD", "dir/nested/file"],
        issue1057_golden("t-deep-blob"),
    );
}

#[test]
fn ls_tree_issue1057_subdir_paths() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        &repo.path().join("dir"),
        &["ls-tree", "HEAD", "nested"],
        issue1057_golden("subdir-plain"),
    );
    assert_issue1057_listing(
        &repo.path().join("dir"),
        &["ls-tree", "HEAD", "nested/"],
        issue1057_golden("subdir-slash"),
    );
    assert_issue1057_listing(
        &repo.path().join("dir"),
        &["ls-tree", "--full-tree", "HEAD", "dir"],
        issue1057_golden("subdir-full-tree"),
    );
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "HEAD", "./dir"],
        issue1057_golden("dot-dir"),
    );
}

#[test]
fn ls_tree_issue1057_overlap_paths() {
    let repo = setup_issue1057_repo();
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "HEAD", "dir", "dir/other"],
        issue1057_golden("overlap-child"),
    );
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "HEAD", "dir/", "dir/other"],
        issue1057_golden("slash-overlap-child"),
    );
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "-r", "HEAD", "dir/"],
        issue1057_golden("recursive-slash"),
    );
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "-r", "-t", "HEAD", "dir/"],
        issue1057_golden("recursive-t-slash"),
    );
    assert_issue1057_listing(
        repo.path(),
        &["ls-tree", "-t", "HEAD", "dir", "dir/other"],
        issue1057_golden("t-overlap-child"),
    );
}

#[test]
fn ls_tree_issue1057_json() {
    let repo = setup_issue1057_repo();
    let output = run_libra_command(&["--json", "ls-tree", "HEAD", "dir"], repo.path());
    assert_cli_success(&output, "issue1057 JSON directory");
    let golden = std::str::from_utf8(issue1057_golden("root")).expect("UTF-8 golden");
    let expected: Vec<_> = golden.lines().map(|line| {
        let (metadata, path) = line.split_once('\t').expect("tree record");
        let fields: Vec<_> = metadata.split_whitespace().collect();
        serde_json::json!({"mode": fields[0], "object_type": fields[1], "object": fields[2], "path": path})
    }).collect();
    let json = parse_json_stdout(&output);
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "ls-tree");
    assert_eq!(json["data"]["entries"], serde_json::json!(expected));
}
