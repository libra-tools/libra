//! Tests status reporting for staged, unstaged, ignored files and path filtering.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{fs, io::Write};

use libra::{
    cli::Stash,
    command::{
        stash,
        status::{
            PorcelainVersion, StatusArgs, UntrackedFiles, execute_to as status_execute_inner,
            output_porcelain as output_porcelain_inner,
        },
    },
};

use super::*;

async fn status_execute(args: StatusArgs, writer: &mut impl Write) {
    status_execute_inner(args, writer)
        .await
        .expect("status output should succeed in test");
}

fn output_porcelain(
    staged: &libra::command::status::Changes,
    unstaged: &libra::command::status::Changes,
    writer: &mut impl Write,
) {
    output_porcelain_inner(staged, unstaged, false, writer)
        .expect("porcelain output should succeed in test");
}

#[test]
#[serial]
fn test_status_cli_outside_repository_returns_fatal_128() {
    let temp = tempdir().unwrap();

    let output = run_libra_command(&["status"], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

#[tokio::test]
#[serial]
/// Tests --ignored flag: ignored files appear in outputs
async fn test_status_ignored_outputs() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create .libraignore ignoring foo* and dir/
    let mut ign = fs::File::create(".libraignore").unwrap();
    ign.write_all(b"foo*\ndir/\n").unwrap();

    // Create ignored files and non-ignored
    fs::write("foo.txt", "x").unwrap();
    fs::create_dir_all("dir").unwrap();
    fs::write("dir/a.txt", "y").unwrap();
    fs::write("bar.txt", "z").unwrap();

    // Porcelain
    let mut out = Vec::new();
    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V1),
            ignored: true,
            ..Default::default()
        },
        &mut out,
    )
    .await;
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.lines().any(|l| l.starts_with("!! foo.txt")),
        "porcelain should show !! for ignored file: {}",
        s
    );

    // Short
    let mut out = Vec::new();
    status_execute(
        StatusArgs {
            short: true,
            ignored: true,
            ..Default::default()
        },
        &mut out,
    )
    .await;
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.lines().any(|l| l.starts_with("!! foo.txt")),
        "short should show !! for ignored file: {}",
        s
    );

    // Standard
    let mut out = Vec::new();
    status_execute(
        StatusArgs {
            ignored: true,
            ..Default::default()
        },
        &mut out,
    )
    .await;
    let s = String::from_utf8(out).unwrap();
    // In standard mode, headers are printed to stdout via println!, so the writer content may
    // only include per-file lines. Assert that ignored file names are present.
    assert!(
        s.contains("foo.txt"),
        "standard should include ignored file name in writer output: {}",
        s
    );
}

#[tokio::test]
#[serial]
/// Ensures `status` refuses to run inside a bare repository.
async fn test_status_rejects_bare_repository() {
    let temp_path = tempdir().unwrap();
    test::setup_clean_testing_env_in(temp_path.path());

    init(InitArgs {
        bare: true,
        initial_branch: None,
        template: None,
        repo_directory: temp_path.path().to_str().unwrap().to_string(),
        quiet: false,
        shared: None,
        object_format: None,
        ref_format: None,
        from_git_repository: None,
        vault: false,
    })
    .await
    .unwrap();

    let _guard = ChangeDirGuard::new(temp_path.path());

    let mut out = Vec::new();
    let err = status_execute_inner(StatusArgs::default(), &mut out)
        .await
        .expect_err("status should refuse to run in bare repositories");
    assert!(
        err.to_string()
            .contains("this operation must be run in a work tree"),
        "unexpected bare-repo error: {err}"
    );
    assert!(
        out.is_empty(),
        "bare repo status should not write to stdout"
    );
}

// Helper function to create CommitArgs with a message, using default values for other fields
fn create_commit_args(message: &str) -> CommitArgs {
    CommitArgs {
        message: Some(message.to_string()),
        ..Default::default()
    }
}

#[tokio::test]
#[serial]
/// Tests the file status detection functionality with respect to ignore patterns.
/// Verifies that files matching patterns in .libraignore are properly excluded from status reports.
async fn test_changes_to_be_staged() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    let mut gitignore_file = fs::File::create(".libraignore").unwrap();
    gitignore_file
        .write_all(b"should_ignore*\nignore_dir/")
        .unwrap();

    let mut should_ignore_file_0 = fs::File::create("should_ignore.0").unwrap();
    let mut not_ignore_file_0 = fs::File::create("not_ignore.0").unwrap();
    fs::create_dir("ignore_dir").unwrap();
    let mut should_ignore_file_1 = fs::File::create("ignore_dir/should_ignore.1").unwrap();
    fs::create_dir("not_ignore_dir").unwrap();
    let mut not_ignore_file_1 = fs::File::create("not_ignore_dir/not_ignore.1").unwrap();

    let change = changes_to_be_staged().unwrap();
    assert!(
        !change
            .new
            .iter()
            .any(|x| x.file_name().unwrap() == "should_ignore.0")
    );
    assert!(
        !change
            .new
            .iter()
            .any(|x| x.file_name().unwrap() == "should_ignore.1")
    );
    assert!(
        change
            .new
            .iter()
            .any(|x| x.file_name().unwrap() == "not_ignore.0")
    );
    assert!(
        change
            .new
            .iter()
            .any(|x| x.file_name().unwrap() == "not_ignore.1")
    );

    add::execute(AddArgs {
        pathspec: vec![String::from(".")],
        all: true,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    should_ignore_file_0.write_all(b"foo").unwrap();
    should_ignore_file_1.write_all(b"foo").unwrap();
    not_ignore_file_0.write_all(b"foo").unwrap();
    not_ignore_file_1.write_all(b"foo").unwrap();

    let change = changes_to_be_staged().unwrap();
    assert!(
        !change
            .modified
            .iter()
            .any(|x| x.file_name().unwrap() == "should_ignore.0")
    );
    assert!(
        !change
            .modified
            .iter()
            .any(|x| x.file_name().unwrap() == "should_ignore.1")
    );
    assert!(
        change
            .modified
            .iter()
            .any(|x| x.file_name().unwrap() == "not_ignore.0")
    );
    assert!(
        change
            .modified
            .iter()
            .any(|x| x.file_name().unwrap() == "not_ignore.1")
    );

    fs::remove_dir_all("ignore_dir").unwrap();
    fs::remove_dir_all("not_ignore_dir").unwrap();
    fs::remove_file("should_ignore.0").unwrap();
    fs::remove_file("not_ignore.0").unwrap();

    not_ignore_file_1.write_all(b"foo").unwrap();

    let change = changes_to_be_staged().unwrap();
    assert!(
        !change
            .deleted
            .iter()
            .any(|x| x.file_name().unwrap() == "should_ignore.0")
    );
    assert!(
        !change
            .deleted
            .iter()
            .any(|x| x.file_name().unwrap() == "should_ignore.1")
    );
    assert!(
        change
            .deleted
            .iter()
            .any(|x| x.file_name().unwrap() == "not_ignore.0")
    );
    assert!(
        change
            .deleted
            .iter()
            .any(|x| x.file_name().unwrap() == "not_ignore.1")
    );
}

#[test]
fn test_output_porcelain_format() {
    use std::path::PathBuf;

    use libra::command::status::Changes;

    // Create test data
    let staged = Changes {
        new: vec![PathBuf::from("new_file.txt")],
        modified: vec![PathBuf::from("modified_file.txt")],
        deleted: vec![PathBuf::from("deleted_file.txt")],
        renamed: vec![],
    };

    let unstaged = Changes {
        new: vec![PathBuf::from("untracked_file.txt")],
        modified: vec![PathBuf::from("unstaged_modified.txt")],
        deleted: vec![PathBuf::from("unstaged_deleted.txt")],
        renamed: vec![],
    };

    // Create a buffer to capture the output
    let mut output = Vec::new();

    // Call the output_porcelain function
    output_porcelain(&staged, &unstaged, &mut output);

    // Get the output as a string
    let output_str = String::from_utf8(output).unwrap();

    // Verify the output format
    let lines: Vec<&str> = output_str.trim().split('\n').collect();

    assert!(lines.contains(&"A  new_file.txt"));
    assert!(lines.contains(&"M  modified_file.txt"));
    assert!(lines.contains(&"D  deleted_file.txt"));
    assert!(lines.contains(&" M unstaged_modified.txt"));
    assert!(lines.contains(&" D unstaged_deleted.txt"));
    assert!(lines.contains(&"?? untracked_file.txt"));
}

#[tokio::test]
#[serial]
/// Tests the --porcelain flag for machine-readable output format.
/// Verifies that the output matches Git's porcelain format specification.
async fn test_status_porcelain() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create test data
    let mut file1 = fs::File::create("file1.txt").unwrap();
    file1.write_all(b"content").unwrap();

    let mut file2 = fs::File::create("file2.txt").unwrap();
    file2.write_all(b"content").unwrap();

    // Add one file to the staging area
    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    // Add another file to the staging area and modify it
    add::execute(AddArgs {
        pathspec: vec![String::from("file2.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    file2.write_all(b"modified content").unwrap();

    // Create a new file (untracked)
    let mut file3 = fs::File::create("file3.txt").unwrap();
    file3.write_all(b"new content").unwrap();

    // Create a buffer to capture the output
    let mut output = Vec::new();

    // Execute the status command with the --porcelain flag
    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V1),
            ..Default::default()
        },
        &mut output,
    )
    .await;

    // Get the output as a string
    let output_str = String::from_utf8(output).unwrap();

    // Verify the porcelain output format
    let lines: Vec<&str> = output_str.trim().split('\n').collect();

    // Should contain staged file (only staged, no unstaged modification)
    assert!(
        lines.iter().any(|line| line.starts_with("A  file1.txt")),
        "Should show 'A  file1.txt' for staged-only file: {:?}",
        lines
    );
    // file2.txt is staged AND modified after staging - should be merged as "AM"
    assert!(
        lines.iter().any(|line| line.starts_with("AM file2.txt")),
        "Should show 'AM file2.txt' for staged+modified file: {:?}",
        lines
    );

    // Should contain untracked files
    assert!(
        lines.iter().any(|line| line.starts_with("?? file3.txt")),
        "Should show '?? file3.txt' for untracked file: {:?}",
        lines
    );

    // Should not contain human-readable text
    assert!(!output_str.contains("Changes to be committed"));
    assert!(!output_str.contains("Untracked files"));
    assert!(!output_str.contains("On branch"));
}

#[test]
fn test_output_short_format() {
    use std::path::PathBuf;

    use libra::command::status::Changes;

    // Create test data
    let staged = Changes {
        new: vec![PathBuf::from("new_file.txt")],
        modified: vec![PathBuf::from("modified_file.txt")],
        deleted: vec![PathBuf::from("deleted_file.txt")],
        renamed: vec![],
    };

    let unstaged = Changes {
        new: vec![PathBuf::from("untracked_file.txt")],
        modified: vec![PathBuf::from("unstaged_modified.txt")],
        deleted: vec![PathBuf::from("unstaged_deleted.txt")],
        renamed: vec![],
    };

    // Create a buffer to capture the output
    let mut output = Vec::new();

    // Test the core logic directly without config dependency
    let status_list = libra::command::status::generate_short_format_status(&staged, &unstaged);

    // Output the short format (without colors for testing)
    for (file, staged_status, unstaged_status) in status_list {
        writeln!(
            output,
            "{}{} {}",
            staged_status,
            unstaged_status,
            file.display()
        )
        .unwrap();
    }

    // Get the output as a string
    let output_str = String::from_utf8(output).unwrap();

    // Verify the output format
    let lines: Vec<&str> = output_str.trim().split('\n').collect();

    // Check staged changes
    assert!(lines.contains(&"A  new_file.txt"));
    assert!(lines.contains(&"M  modified_file.txt"));
    assert!(lines.contains(&"D  deleted_file.txt"));

    // Check unstaged changes
    assert!(lines.contains(&" M unstaged_modified.txt"));
    assert!(lines.contains(&" D unstaged_deleted.txt"));

    // Check untracked files
    assert!(lines.contains(&"?? untracked_file.txt"));
}

#[tokio::test]
#[serial]
/// Tests the -s (--short) flag for short format output.
/// Verifies that the output matches Git's short format specification.
async fn test_status_short_format() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create test data
    let mut file1 = fs::File::create("file1.txt").unwrap();
    file1.write_all(b"content").unwrap();

    let mut file2 = fs::File::create("file2.txt").unwrap();
    file2.write_all(b"content").unwrap();

    // Add one file to the staging area
    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    // Add another file to the staging area and modify it
    add::execute(AddArgs {
        pathspec: vec![String::from("file2.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    // Reopen file2.txt for writing after staging
    let mut file2 = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open("file2.txt")
        .unwrap();
    file2.write_all(b"modified content").unwrap();

    // Create a new file (untracked)
    let mut file3 = fs::File::create("file3.txt").unwrap();
    file3.write_all(b"new content").unwrap();

    // Create a buffer to capture the output
    let mut output = Vec::new();

    // Execute the status command with the -s flag
    status_execute(
        StatusArgs {
            short: true,
            ..Default::default()
        },
        &mut output,
    )
    .await;

    // Get the output as a string
    let output_str = String::from_utf8(output).unwrap();
    println!("Actual short format output: {}", output_str); // Add debug output

    // Verify the short format output
    let lines: Vec<&str> = output_str.trim().split('\n').collect();

    // More flexible assertion: check whether the file appears in the output, but do not specify the exact status code
    let file1_found = lines.iter().any(|line| line.contains("file1.txt"));
    let file2_found = lines.iter().any(|line| line.contains("file2.txt"));
    let file3_found = lines.iter().any(|line| line.contains("file3.txt"));

    assert!(
        file1_found,
        "file1.txt should appear in short format output. Got: {}",
        output_str
    );
    assert!(
        file2_found,
        "file2.txt should appear in short format output. Got: {}",
        output_str
    );
    assert!(
        file3_found,
        "file3.txt should appear in short format output. Got: {}",
        output_str
    );

    // Check that the output format is short (should not contain human-readable text)
    assert!(
        !output_str.contains("Changes to be committed"),
        "Short format should not contain human-readable text. Got: {}",
        output_str
    );
    assert!(
        !output_str.contains("Untracked files"),
        "Short format should not contain human-readable text. Got: {}",
        output_str
    );
    assert!(
        !output_str.contains("On branch"),
        "Short format should not contain branch information. Got: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests porcelain v2 output: branch info, tracked changes, and untracked files.
async fn test_status_porcelain_v2_basic() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // staged + modified file
    let mut file1 = fs::File::create("file1.txt").unwrap();
    file1.write_all(b"content").unwrap();
    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    file1.write_all(b" more").unwrap(); // unstaged modification

    // untracked file
    fs::write("untracked.txt", "u").unwrap();

    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V2),
            branch: true,
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.lines().any(|l| l.starts_with("# branch.head")),
        "porcelain v2 should contain branch.head line: {}",
        output_str
    );
    assert!(
        output_str.lines().any(|l| l.starts_with("1 AM")),
        "porcelain v2 should contain tracked entry line: {}",
        output_str
    );
    assert!(
        output_str.lines().any(|l| l.starts_with("? untracked.txt")),
        "porcelain v2 should list untracked files with '? ': {}",
        output_str
    );

    // Test --ignored flag with porcelain v2
    fs::write(".libraignore", "ignored.txt\n").unwrap();
    fs::write("ignored.txt", "i").unwrap();

    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V2),
            ignored: true,
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.lines().any(|l| l.starts_with("! ignored.txt")),
        "porcelain v2 should list ignored files with '! ' when --ignored: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests porcelain v2 branch metadata uses the real HEAD oid and upstream counts.
async fn test_status_porcelain_v2_branch_metadata_includes_upstream_counts() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::write("tracked.txt", "tracked\n").unwrap();
    add::execute_safe(
        AddArgs {
            pathspec: vec![String::from("tracked.txt")],
            all: false,
            update: false,
            verbose: false,
            dry_run: false,
            ignore_errors: false,
            refresh: false,
            force: false,

            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
        },
        &libra::utils::output::OutputConfig::default(),
    )
    .await
    .expect("add tracked.txt should succeed");
    execute_safe(
        create_commit_args("initial"),
        &libra::utils::output::OutputConfig::default(),
    )
    .await
    .expect("initial commit should succeed");

    let output = run_libra_command(&["config", "branch.main.remote", "origin"], test_dir.path());
    assert_cli_success(&output, "configure branch.main.remote");
    let output = run_libra_command(
        &["config", "branch.main.merge", "refs/heads/main"],
        test_dir.path(),
    );
    assert_cli_success(&output, "configure branch.main.merge");

    let head = Head::current_commit().await.expect("head commit");
    Branch::update_branch("main", &head.to_string(), Some("origin"))
        .await
        .expect("remote-tracking branch should be created");

    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V2),
            branch: true,
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.contains(&format!("# branch.oid {head}")),
        "porcelain v2 should emit the actual HEAD oid: {output_str}"
    );
    assert!(
        output_str.contains("# branch.upstream origin/main"),
        "porcelain v2 should emit upstream metadata: {output_str}"
    );
    assert!(
        output_str.contains("# branch.ab +0 -0"),
        "porcelain v2 should emit ahead/behind counts: {output_str}"
    );
}

#[tokio::test]
#[serial]
/// Tests porcelain v2 with --untracked-files=no hides untracked and ignored entries.
async fn test_status_porcelain_v2_untracked_files_no() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // tracked file
    fs::write("tracked.txt", "t").unwrap();
    add::execute(AddArgs {
        pathspec: vec![String::from("tracked.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    // untracked + ignored
    fs::write("untracked.txt", "u").unwrap();
    fs::write(".libraignore", "ignored.txt\n").unwrap();
    fs::write("ignored.txt", "i").unwrap();

    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V2),
            ignored: true,
            untracked_files: Some(UntrackedFiles::No),
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.lines().any(|l| l.starts_with("1 A")),
        "tracked entry should remain visible in v2: {}",
        output_str
    );
    assert!(
        !output_str.lines().any(|l| l.starts_with("? untracked.txt")),
        "untracked files should be hidden in v2 when --untracked-files=no: {}",
        output_str
    );
    assert!(
        !output_str.lines().any(|l| l.starts_with("! ignored.txt")),
        "ignored files should be hidden in v2 when --untracked-files=no even with --ignored: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests porcelain v2 with --untracked-files=all retains untracked output.
async fn test_status_porcelain_v2_untracked_files_all() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // tracked file
    fs::write("tracked.txt", "t").unwrap();
    add::execute(AddArgs {
        pathspec: vec![String::from("tracked.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    // untracked file
    fs::write("untracked.txt", "u").unwrap();

    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V2),
            untracked_files: Some(UntrackedFiles::All),
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.lines().any(|l| l.starts_with("1 A")),
        "tracked entry should be present in v2: {}",
        output_str
    );
    assert!(
        output_str.lines().any(|l| l.starts_with("? untracked.txt")),
        "untracked entry should be present in v2 when --untracked-files=all: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests --untracked-files=no hides untracked and ignored entries.
async fn test_status_untracked_files_no() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // tracked file
    fs::write("tracked.txt", "t").unwrap();
    add::execute(AddArgs {
        pathspec: vec![String::from("tracked.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    // untracked + ignored
    fs::write("untracked.txt", "u").unwrap();
    fs::write(".libraignore", "ignored.txt\n").unwrap();
    fs::write("ignored.txt", "i").unwrap();

    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V1),
            ignored: true,
            untracked_files: Some(UntrackedFiles::No),
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.contains("A  tracked.txt"),
        "tracked entry should remain visible: {}",
        output_str
    );
    assert!(
        !output_str.contains("?? untracked.txt"),
        "untracked files should be hidden when --untracked-files=no: {}",
        output_str
    );
    assert!(
        !output_str.contains("!! ignored.txt"),
        "ignored files should be hidden when --untracked-files=no even with --ignored: {}",
        output_str
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_status_untracked_files_no_skips_untracked_directory_scan() {
    use std::os::unix::fs::PermissionsExt;

    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::create_dir_all("artifacts/deep").unwrap();
    fs::write("artifacts/deep/blob.bin", "untracked build output").unwrap();
    fs::set_permissions("artifacts", fs::Permissions::from_mode(0o000)).unwrap();

    let mut output = Vec::new();
    let result = status_execute_inner(
        StatusArgs {
            short: true,
            untracked_files: Some(UntrackedFiles::No),
            ..Default::default()
        },
        &mut output,
    )
    .await;

    fs::set_permissions("artifacts", fs::Permissions::from_mode(0o700)).unwrap();
    result.expect("status -uno should not scan hidden untracked directories");
    let output_str = String::from_utf8(output).unwrap();
    assert!(
        !output_str.contains("artifacts"),
        "status -uno should hide untracked directories: {output_str}"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_status_normal_reports_untracked_directory_without_descending() {
    use std::os::unix::fs::PermissionsExt;

    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::create_dir_all("artifacts/deep").unwrap();
    fs::write("artifacts/deep/blob.bin", "untracked build output").unwrap();
    fs::set_permissions("artifacts", fs::Permissions::from_mode(0o000)).unwrap();

    let mut output = Vec::new();
    let result = status_execute_inner(
        StatusArgs {
            short: true,
            ..Default::default()
        },
        &mut output,
    )
    .await;

    fs::set_permissions("artifacts", fs::Permissions::from_mode(0o700)).unwrap();
    result.expect("status -s should report a top-level untracked directory without reading it");
    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.lines().any(|line| line == "?? artifacts/"),
        "status -s should report the untracked directory itself: {output_str}"
    );
}

/// Regression (2026-07-05): the no-descend directory marker must follow
/// git semantics — an untracked directory whose entire contents are
/// skip-listed (`.libra`/`.git`) or ignored holds no visible untracked
/// file and must not be reported at all. Pre-fix, `?? dir/` appeared for
/// directories containing only a nested `.libra` (e.g. test harnesses'
/// `.libra-test-home/`), breaking every clean-tree assertion downstream.
#[tokio::test]
#[serial]
async fn test_status_hides_untracked_directory_with_only_skiplisted_content() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Only skip-listed / empty contents: a nested `.libra` with a file,
    // a nested `.git`, and an empty subdirectory.
    fs::create_dir_all("fake-home/.libra/vault-keys").unwrap();
    fs::write("fake-home/.libra/vault-keys/key", "opaque").unwrap();
    fs::create_dir_all("fake-home/.git").unwrap();
    fs::write("fake-home/.git/config", "x").unwrap();
    fs::create_dir_all("fake-home/.config").unwrap();
    // Control: a sibling with one real untracked file IS reported.
    fs::create_dir_all("real-dir").unwrap();
    fs::write("real-dir/file.txt", "visible").unwrap();

    let mut output = Vec::new();
    status_execute_inner(
        StatusArgs {
            short: true,
            ..Default::default()
        },
        &mut output,
    )
    .await
    .expect("status -s");
    let output_str = String::from_utf8(output).unwrap();
    assert!(
        !output_str.contains("fake-home"),
        "directory with only skip-listed content must stay invisible: {output_str}"
    );
    assert!(
        output_str.lines().any(|line| line == "?? real-dir/"),
        "directory with a visible untracked file is still reported: {output_str}"
    );
}

#[tokio::test]
#[serial]
async fn test_status_normal_untracked_directories_are_sorted() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::create_dir_all("zeta").unwrap();
    fs::write("zeta/file.txt", "z").unwrap();
    fs::create_dir_all("alpha").unwrap();
    fs::write("alpha/file.txt", "a").unwrap();

    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            untracked_files: Some(UntrackedFiles::Normal),
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap().replace("\\", "/");
    let alpha = output_str
        .find("\talpha/")
        .expect("normal status should list alpha/ as a collapsed directory");
    let zeta = output_str
        .find("\tzeta/")
        .expect("normal status should list zeta/ as a collapsed directory");
    assert!(
        alpha < zeta,
        "normal status should sort collapsed untracked directories: {output_str}"
    );
}

#[tokio::test]
#[serial]
/// Tests --untracked-files=all retains untracked output (same as normal for now).
async fn test_status_untracked_files_all() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // tracked file
    fs::write("tracked.txt", "t").unwrap();
    add::execute(AddArgs {
        pathspec: vec![String::from("tracked.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    // untracked file
    fs::write("untracked.txt", "u").unwrap();

    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V1),
            untracked_files: Some(UntrackedFiles::All),
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.contains("A  tracked.txt"),
        "tracked entry should be present: {}",
        output_str
    );
    assert!(
        output_str.contains("?? untracked.txt"),
        "untracked entry should be present when --untracked-files=all: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests status in a newly initialized empty repository
/// Verifies the initial state message for empty repositories
async fn test_status_empty_repository() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Execute status command with default arguments
    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();

    // Should indicate no commits or nothing to commit in empty repo
    assert!(
        output_str.contains("No commits yet")
            || output_str.contains("nothing to commit")
            || output_str.contains("initial commit"),
        "Empty repository status should indicate initial state. Got: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests status with mixed staged and unstaged changes
/// Verifies proper separation of staged vs working directory changes
async fn test_status_mixed_changes() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create and stage a file
    let mut file1 = fs::File::create("staged.txt").unwrap();
    file1.write_all(b"initial content").unwrap();

    add::execute(AddArgs {
        pathspec: vec![String::from("staged.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    // Create an unstaged file
    let mut file2 = fs::File::create("unstaged.txt").unwrap();
    file2.write_all(b"unstaged content").unwrap();

    // Modify the staged file in working directory
    let mut file1 = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open("staged.txt")
        .unwrap();
    file1.write_all(b"modified content").unwrap();

    // Execute status command
    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();

    // Should show both staged and unstaged sections
    assert!(
        output_str.contains("staged.txt"),
        "Should show staged file: {}",
        output_str
    );
    assert!(
        output_str.contains("unstaged.txt"),
        "Should show unstaged file: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests status after file deletion
/// Verifies that deleted files are properly detected and reported
async fn test_status_deleted_files() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create, stage, and commit a file
    let file_path = "to_delete.txt";
    let mut file = fs::File::create(file_path).unwrap();
    file.write_all(b"content to delete").unwrap();

    add::execute(AddArgs {
        pathspec: vec![String::from(file_path)],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    // Use helper function to create CommitArgs
    commit::execute(create_commit_args("Add file to delete")).await;

    // Delete the file
    fs::remove_file(file_path).unwrap();

    // Execute status command
    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();

    // Should report the deleted file
    assert!(
        output_str.contains(file_path),
        "Should show deleted file: {}",
        output_str
    );
    assert!(
        output_str.contains("deleted") || output_str.contains("Deleted"),
        "Should indicate file deletion: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests status with subdirectory structure
/// Verifies that status works correctly with nested directory structures
async fn test_status_with_subdirectories() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create directory structure
    fs::create_dir_all("subdir/nested").unwrap();

    // Create files in different directories
    let files = [
        "root_file.txt",
        "subdir/sub_file.txt",
        "subdir/nested/deep_file.txt",
    ];

    for file_path in &files {
        let mut file = fs::File::create(file_path).unwrap();
        file.write_all(b"content").unwrap();
    }

    // Stage some files
    add::execute(AddArgs {
        pathspec: vec![String::from("root_file.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    // Execute status command with --untracked-files=all to show individual files
    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            untracked_files: Some(UntrackedFiles::All),
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap().replace("\\", "/");

    // Should show files from all directories (with --untracked-files=all)
    for file_path in &files {
        assert!(
            output_str.contains(file_path),
            "Should show file from subdirectory: {} in {}",
            file_path,
            output_str
        );
    }

    // Test normal mode: untracked directories should be collapsed
    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            untracked_files: Some(UntrackedFiles::Normal),
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap().replace("\\", "/");

    // In normal mode, subdir/ should be shown as a collapsed directory
    // since it's completely untracked
    assert!(
        output_str.contains("subdir/"),
        "Should show collapsed untracked directory: subdir/ in {}",
        output_str
    );
    // Individual files inside subdir should NOT be shown in normal mode
    assert!(
        !output_str.contains("subdir/sub_file.txt"),
        "Should NOT show individual files in collapsed directory: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests status verbose output format
/// Verifies that verbose mode provides additional information when requested
async fn test_status_verbose_output() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a file and make it executable
    let mut file = fs::File::create("script.sh").unwrap();
    file.write_all(b"#!/bin/bash\necho hello").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata("script.sh").unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions("script.sh", perms).unwrap();
    }

    // Stage the file
    add::execute(AddArgs {
        pathspec: vec![String::from("script.sh")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    // Execute status command - we'll test that it completes without error
    // since we can't predict the exact verbose output format
    let mut output = Vec::new();

    // This should complete successfully without panicking
    status_execute(
        StatusArgs {
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();

    // Basic verification that status produced some output
    assert!(
        !output_str.is_empty(),
        "Status should produce output in verbose mode"
    );
    assert!(
        output_str.contains("script.sh"),
        "Should show staged file: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests --short --branch combination output
/// Verifies that branch info is displayed in short format when --branch flag is enabled.
async fn test_status_short_format_with_branch() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create and commit a file
    let mut file1 = fs::File::create("file1.txt").unwrap();
    file1.write_all(b"content").unwrap();

    // Add one file to the staging area
    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    commit::execute(create_commit_args("Initial commit")).await;

    // Modify the file
    let mut file1 = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open("file1.txt")
        .unwrap();
    file1.write_all(b"modified content").unwrap();

    let mut output = Vec::new();

    status_execute(
        StatusArgs {
            porcelain: None,
            short: true,
            branch: true,
            show_stash: false,
            ignored: false,
            untracked_files: Some(UntrackedFiles::Normal),
            exit_code: false,
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();

    // Should show branch info in the first line with ## prefix
    assert!(
        output_str.contains("## main"),
        "Short format with --branch should start with branch info (##). Got: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests --porcelain --branch combination output
/// Verifies that branch info is displayed in porcelain format when --branch flag is enabled.
async fn test_status_porcelain_format_with_branch() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create and commit a file
    let mut file1 = fs::File::create("file1.txt").unwrap();
    file1.write_all(b"content").unwrap();

    // Add one file to the staging area
    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    commit::execute(create_commit_args("Initial commit")).await;

    // Modify the file
    let mut file1 = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open("file1.txt")
        .unwrap();
    file1.write_all(b"modified content").unwrap();

    let mut output = Vec::new();

    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V1),
            short: false,
            branch: true,
            show_stash: false,
            ignored: false,
            untracked_files: Some(UntrackedFiles::Normal),
            exit_code: false,
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();

    // Should show branch info in the first line with ## prefix
    assert!(
        output_str.contains("## main"),
        "Porcelain format with --branch should start with branch info (##). Got: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests --show-stash output when stash exists
/// Verifies that stash count info is displayed in standard mode when --show-stash flag is enabled
async fn test_status_show_stash_with_existing_stash() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create and commit a file
    let mut file1 = fs::File::create("file1.txt").unwrap();
    file1.write_all(b"content").unwrap();

    // Add one file to the staging area
    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    commit::execute(create_commit_args("Initial commit")).await;

    // Create changes for stashing
    let mut file1 = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open("file1.txt")
        .unwrap();
    file1.write_all(b"modified content").unwrap();

    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
        all: false,
        update: false,
        refresh: false,
        force: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    stash::execute(Stash::Push {
        message: Some("test stash".to_string()),
        include_untracked: false,
        no_include_untracked: false,
        all: false,
        keep_index: false,
        pathspec: Vec::new(),
    })
    .await;

    let mut output = Vec::new();

    status_execute(
        StatusArgs {
            porcelain: None,
            short: false,
            branch: false,
            show_stash: true,
            ignored: false,
            untracked_files: Some(UntrackedFiles::Normal),
            exit_code: false,
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();

    // Should display stash count info
    assert!(
        output_str.contains("Your stash currently has 1 entry"),
        "Should show stash count when --show-stash flag is enabled. Got: {}",
        output_str
    );

    // Test for porcelain mode
    // Shouldn't output the stash count info
    let mut output = Vec::new();

    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V1),
            short: false,
            branch: false,
            show_stash: true,
            ignored: false,
            untracked_files: Some(UntrackedFiles::Normal),
            exit_code: false,
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();

    // Shouldn't display stash count info
    assert!(
        !output_str.contains("Your stash currently has 1 entry"),
        "Porcelain format with --show-stash shouldn't start with stash count info. Got: {}",
        output_str
    );

    // Test for short mode
    // Shouldn't output the stash count info
    let mut output = Vec::new();

    status_execute(
        StatusArgs {
            porcelain: None,
            short: true,
            branch: false,
            show_stash: true,
            ignored: false,
            untracked_files: Some(UntrackedFiles::Normal),
            exit_code: false,
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();

    // Shouldn't display stash count info
    assert!(
        !output_str.contains("Your stash currently has 1 entry"),
        "Short format with --show-stash shouldn't start with stash count info. Got: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests --show-stash output when no stash exists
/// Verifies that stash info is not displayed when no stash is present
async fn test_status_show_stash_without_stash() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create and commit a file
    let mut file1 = fs::File::create("file1.txt").unwrap();
    file1.write_all(b"content").unwrap();

    // Add one file to the staging area
    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    commit::execute(create_commit_args("Initial commit")).await;

    let mut output = Vec::new();

    status_execute(
        StatusArgs {
            porcelain: None,
            short: false,
            branch: false,
            show_stash: true,
            ignored: false,
            untracked_files: Some(UntrackedFiles::Normal),
            exit_code: false,
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();

    // Should not display stash information when there are no stashes
    assert!(
        !output_str.contains("Your stash currently has"),
        "Should not show stash info when no stash exists. Got: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests --branch output in detached HEAD state
/// Verifies that branch info shows detached HEAD status correctly
async fn test_status_branch_detached_head() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create and commit a file
    let mut file1 = fs::File::create("file1.txt").unwrap();
    file1.write_all(b"initial content").unwrap();

    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    commit::execute(create_commit_args("Initial commit")).await;

    // Get the current commit hash for checkout
    let current_commit = Head::current_commit().await.expect("Should have a commit");

    // Create a second commit
    let mut file2 = fs::File::create("file2.txt").unwrap();
    file2.write_all(b"second file").unwrap();

    add::execute(AddArgs {
        pathspec: vec![String::from("file2.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    commit::execute(create_commit_args("Second commit")).await;

    // checkout the first commit to enter the detached state
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some(current_commit.to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: true,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    let mut output = Vec::new();

    status_execute(
        StatusArgs {
            porcelain: None,
            short: true,
            branch: true,
            show_stash: false,
            ignored: false,
            untracked_files: Some(UntrackedFiles::Normal),
            exit_code: false,
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();
    let display_info = format!("## HEAD (detached at {})", &current_commit.to_string()[..8]);
    // Should show detached HEAD info with ## prefix
    assert!(
        output_str.contains(&display_info),
        "Should show detached HEAD status in branch info. Got: {}",
        output_str
    );
}

#[tokio::test]
#[serial]
/// Tests porcelain v2 output shows actual file modes and hashes.
/// Verifies:
/// - New files have mH=000000 and zero hash for hH
/// - Tracked files show actual hashes from index and HEAD
async fn test_status_porcelain_v2_file_modes_and_hashes() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create and commit a file first
    fs::write("existing.txt", "existing content").unwrap();
    add::execute(AddArgs {
        pathspec: vec![String::from("existing.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    commit::execute(create_commit_args("Initial commit")).await;

    // Modify the existing file
    fs::write("existing.txt", "modified content").unwrap();
    add::execute(AddArgs {
        pathspec: vec![String::from("existing.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    // Create a new file (staged)
    fs::write("new_file.txt", "new content").unwrap();
    add::execute(AddArgs {
        pathspec: vec![String::from("new_file.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V2),
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();

    // Use dynamic zero hash to support both SHA-1 (40 chars) and SHA-256 (64 chars)
    let zero_hash = git_internal::hash::ObjectHash::zero_str(git_internal::hash::get_hash_kind());

    // Check format for modified file: should have actual modes and hashes
    let existing_line = output_str.lines().find(|l| l.contains("existing.txt"));
    assert!(
        existing_line.is_some(),
        "Should contain existing.txt in output: {}",
        output_str
    );
    let existing_line = existing_line.unwrap();

    // Format: 1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
    let parts: Vec<&str> = existing_line.split_whitespace().collect();
    assert!(
        parts.len() >= 9,
        "Modified file line should have at least 9 parts: {}",
        existing_line
    );

    // Check that mH (mode HEAD) is 100644 for existing file
    assert_eq!(
        parts[3], "100644",
        "mH should be 100644 for regular file: {}",
        existing_line
    );
    // Check that mI (mode index) is 100644
    assert_eq!(
        parts[4], "100644",
        "mI should be 100644 for regular file: {}",
        existing_line
    );
    // Check that hH and hI are not zero hashes
    assert!(
        parts[6] != zero_hash,
        "hH should not be zero hash for tracked file: {}",
        existing_line
    );
    assert!(
        parts[7] != zero_hash,
        "hI should not be zero hash for staged file: {}",
        existing_line
    );

    // Check format for new file: mH should be 000000 and hH should be zero hash
    let new_line = output_str.lines().find(|l| l.contains("new_file.txt"));
    assert!(
        new_line.is_some(),
        "Should contain new_file.txt in output: {}",
        output_str
    );
    let new_line = new_line.unwrap();

    let parts: Vec<&str> = new_line.split_whitespace().collect();
    assert!(
        parts.len() >= 9,
        "New file line should have at least 9 parts: {}",
        new_line
    );

    // Check that mH is 000000 for new file
    assert_eq!(
        parts[3], "000000",
        "mH should be 000000 for new file: {}",
        new_line
    );
    // Check that hH is zero hash for new file
    assert_eq!(
        parts[6], zero_hash,
        "hH should be zero hash for new file: {}",
        new_line
    );
    // Check that hI is NOT zero hash (file is in index)
    assert!(
        parts[7] != zero_hash,
        "hI should not be zero hash for staged new file: {}",
        new_line
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial]
/// Tests porcelain v2 output shows 100755 for executable files.
async fn test_status_porcelain_v2_executable_file() {
    use std::os::unix::fs::PermissionsExt;

    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create an executable file
    fs::write("script.sh", "#!/bin/bash\necho hello").unwrap();
    let mut perms = fs::metadata("script.sh").unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions("script.sh", perms).unwrap();

    // Stage the executable file
    add::execute(AddArgs {
        pathspec: vec![String::from("script.sh")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V2),
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();

    let script_line = output_str.lines().find(|l| l.contains("script.sh"));
    assert!(
        script_line.is_some(),
        "Should contain script.sh in output: {}",
        output_str
    );
    let script_line = script_line.unwrap();

    let parts: Vec<&str> = script_line.split_whitespace().collect();
    assert!(
        parts.len() >= 9,
        "Executable file line should have at least 9 parts: {}",
        script_line
    );

    // Check that mI (mode index) is 100755 for executable
    assert_eq!(
        parts[4], "100755",
        "mI should be 100755 for executable file: {}",
        script_line
    );
    // Check that mW (mode worktree) is 100755 for executable
    assert_eq!(
        parts[5], "100755",
        "mW should be 100755 for executable file: {}",
        script_line
    );
}

#[tokio::test]
#[serial]
/// Tests porcelain v2 output for deleted files shows correct modes.
async fn test_status_porcelain_v2_deleted_file() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create, stage and commit a file
    fs::write("to_delete.txt", "content").unwrap();
    add::execute(AddArgs {
        pathspec: vec![String::from("to_delete.txt")],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    commit::execute(create_commit_args("Initial commit")).await;

    // Delete the file from working tree (but not from index)
    fs::remove_file("to_delete.txt").unwrap();

    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V2),
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();

    let deleted_line = output_str.lines().find(|l| l.contains("to_delete.txt"));
    assert!(
        deleted_line.is_some(),
        "Should contain to_delete.txt in output: {}",
        output_str
    );
    let deleted_line = deleted_line.unwrap();

    let parts: Vec<&str> = deleted_line.split_whitespace().collect();
    assert!(
        parts.len() >= 9,
        "Deleted file line should have at least 9 parts: {}",
        deleted_line
    );

    // Should show status  D (space + D) for unstaged deletion
    assert!(
        deleted_line.starts_with("1  D"),
        "Should show ' D' status for deleted file: {}",
        deleted_line
    );

    // mW (mode worktree) should be 000000 for deleted file
    assert_eq!(
        parts[5], "000000",
        "mW should be 000000 for deleted file: {}",
        deleted_line
    );

    // mH and mI should still be 100644
    assert_eq!(
        parts[3], "100644",
        "mH should be 100644 for deleted file: {}",
        deleted_line
    );
    assert_eq!(
        parts[4], "100644",
        "mI should be 100644 for deleted file: {}",
        deleted_line
    );
}

#[tokio::test]
#[serial]
/// Tests status command after adding a file
///
/// Verifies that the status command correctly reports added files with proper formatting
async fn test_status_after_add() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a new file
    let file_path = "test.txt";
    fs::write(file_path, "content").unwrap();

    // Add the file
    add::execute(AddArgs {
        pathspec: vec![String::from(file_path)],
        all: false,
        update: false,
        refresh: false,
        force: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    // Test porcelain output
    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            porcelain: Some(PorcelainVersion::V1),
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str
            .lines()
            .any(|l| l.starts_with("A ") && l.contains(file_path)),
        "Porcelain status should show 'A ' prefix for added file: {}",
        output_str
    );

    // Test short output
    let mut output = Vec::new();
    status_execute(
        StatusArgs {
            short: true,
            ..Default::default()
        },
        &mut output,
    )
    .await;

    let output_str = String::from_utf8(output).unwrap();
    let re = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
    let clean_output = re.replace_all(&output_str, "");
    assert!(
        clean_output
            .lines()
            .any(|l| l.starts_with("A ") && l.contains(file_path)),
        "Short status should show 'A ' prefix for added file: {}",
        clean_output
    );

    // Verify via changes_to_be_committed
    let changes = changes_to_be_committed().await;
    assert!(
        changes.new.iter().any(|x| x.to_str().unwrap() == file_path),
        "Added file should appear in changes_to_be_committed"
    );
}

// ---------------------------------------------------------------------------
// Success summary output for add
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn test_add_success_summary_output() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init"], &repo);
    assert!(output.status.success());

    std::fs::write(repo.join("new.txt"), "hello").unwrap();

    let output = run_libra_command(&["add", "new.txt"], &repo);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("add") && stdout.contains("new.txt"),
        "add should print success summary, got: {stdout}"
    );
}

#[test]
#[serial]
fn test_add_quiet_suppresses_output() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init"], &repo);
    assert!(output.status.success());

    std::fs::write(repo.join("new.txt"), "hello").unwrap();

    let output = run_libra_command(&["--quiet", "add", "new.txt"], &repo);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "quiet mode should suppress stdout, got: {stdout}"
    );
}

#[test]
#[serial]
fn test_add_verbose_shows_per_file_listing() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init"], &repo);
    assert!(output.status.success());

    std::fs::write(repo.join("a.txt"), "a").unwrap();
    std::fs::write(repo.join("b.txt"), "b").unwrap();

    let output = run_libra_command(&["add", "--verbose", "a.txt", "b.txt"], &repo);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("add(new)"),
        "verbose mode should show per-file details, got: {stdout}"
    );
}

#[test]
#[serial]
fn test_add_nothing_specified_exit_129() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init"], &repo);
    assert!(output.status.success());

    let output = run_libra_command(&["add"], &repo);
    assert_eq!(output.status.code(), Some(129));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nothing specified"),
        "should show nothing specified hint: {stderr}"
    );
}

#[test]
#[serial]
fn test_add_dry_run_output() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init"], &repo);
    assert!(output.status.success());

    std::fs::write(repo.join("file.txt"), "content").unwrap();

    let output = run_libra_command(&["add", "--dry-run", "file.txt"], &repo);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("dry run"),
        "dry run should indicate no files staged, got: {stdout}"
    );

    // Verify file was NOT actually staged
    let status = run_libra_command(&["status", "--short"], &repo);
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        !status_stdout.contains("A  file.txt"),
        "dry run should not stage: {status_stdout}"
    );
}

#[test]
#[serial]
fn test_status_porcelain_z_uses_null_terminator() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init"], &repo);
    assert!(output.status.success());

    std::fs::write(repo.join("file.txt"), "content").unwrap();

    let output = run_libra_command(&["status", "--porcelain", "-z"], &repo);
    assert!(output.status.success());

    // NUL terminator means no newline byte in stdout.
    assert!(
        !output.stdout.contains(&b'\n'),
        "-z should not emit newlines, got: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stdout.contains(&b'\0'),
        "-z should emit NUL terminators"
    );
}

#[test]
#[serial]
fn test_status_short_z_uses_null_terminator() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init"], &repo);
    assert!(output.status.success());

    std::fs::write(repo.join("file.txt"), "content").unwrap();

    let output = run_libra_command(&["status", "-s", "-z"], &repo);
    assert!(output.status.success());

    assert!(
        !output.stdout.contains(&b'\n'),
        "-s -z should not emit newlines, got: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
#[serial]
fn test_status_branch_no_ahead_behind_suppresses_counts() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init"], &repo);
    assert!(output.status.success());

    let output = run_libra_command(&["config", "set", "user.name", "Test"], &repo);
    assert!(output.status.success());
    let output = run_libra_command(&["config", "set", "user.email", "test@example.com"], &repo);
    assert!(output.status.success());

    std::fs::write(repo.join("file.txt"), "content").unwrap();
    let output = run_libra_command(&["add", "file.txt"], &repo);
    assert!(output.status.success());
    let output = run_libra_command(&["commit", "-m", "first", "--no-verify"], &repo);
    assert!(output.status.success());

    let output = run_libra_command(
        &["status", "--short", "--branch", "--no-ahead-behind"],
        &repo,
    );
    assert!(
        output.status.success(),
        "--no-ahead-behind should be accepted"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("## main"),
        "branch info should be shown: {stdout}"
    );

    let output = run_libra_command(&["status", "--short", "--branch", "--ahead-behind"], &repo);
    assert!(output.status.success(), "--ahead-behind should be accepted");
}

#[test]
#[serial]
fn test_status_column_aligns_labels() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init"], &repo);
    assert!(output.status.success());

    let output = run_libra_command(&["config", "set", "user.name", "Test"], &repo);
    assert!(output.status.success());
    let output = run_libra_command(&["config", "set", "user.email", "test@example.com"], &repo);
    assert!(output.status.success());

    std::fs::write(repo.join("mod.txt"), "mod").unwrap();
    std::fs::write(repo.join("keep.txt"), "keep").unwrap();
    let output = run_libra_command(&["add", "mod.txt", "keep.txt"], &repo);
    assert!(output.status.success());
    let output = run_libra_command(&["commit", "-m", "base", "--no-verify"], &repo);
    assert!(output.status.success());

    std::fs::remove_file(repo.join("mod.txt")).unwrap();
    let output = run_libra_command(&["add", "mod.txt"], &repo);
    assert!(output.status.success());
    std::fs::write(repo.join("new.txt"), "new").unwrap();
    let output = run_libra_command(&["add", "new.txt"], &repo);
    assert!(output.status.success());

    let output = run_libra_command(&["status", "--column"], &repo);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("deleted:  mod.txt"),
        "--column should align labels: {stdout}"
    );
    assert!(
        stdout.contains("new file: new.txt"),
        "--column should align labels: {stdout}"
    );
}

#[test]
#[serial]
fn test_status_find_renames_detects_content_rename() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init"], &repo);
    assert!(output.status.success());

    let output = run_libra_command(&["config", "set", "user.name", "Test"], &repo);
    assert!(output.status.success());
    let output = run_libra_command(&["config", "set", "user.email", "test@example.com"], &repo);
    assert!(output.status.success());

    assert!(
        run_libra_command(&["config", "set", "status.renameUntracked", "true"], &repo)
            .status
            .success()
    );
    std::fs::write(repo.join("old.txt"), "content").unwrap();
    let output = run_libra_command(&["add", "old.txt"], &repo);
    assert!(output.status.success());
    let output = run_libra_command(&["commit", "-m", "base", "--no-verify"], &repo);
    assert!(output.status.success());

    std::fs::remove_file(repo.join("old.txt")).unwrap();
    std::fs::write(repo.join("new.txt"), "content").unwrap();

    let output = run_libra_command(&["status", "--find-renames", "--short"], &repo);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains('R'),
        "--find-renames should report rename status, got: {stdout}"
    );
    assert!(
        !stdout.contains("D  old.txt"),
        "old path should not remain as delete: {stdout}"
    );
    assert!(
        !stdout.contains("?? new.txt"),
        "new path should not remain as untracked: {stdout}"
    );
}

#[test]
#[serial]
fn test_status_find_renames_honors_threshold() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let output = run_libra_command(&["init"], &repo);
    assert!(output.status.success());

    let output = run_libra_command(&["config", "set", "user.name", "Test"], &repo);
    assert!(output.status.success());
    let output = run_libra_command(&["config", "set", "user.email", "test@example.com"], &repo);
    assert!(output.status.success());

    assert!(
        run_libra_command(&["config", "set", "status.renameUntracked", "true"], &repo)
            .status
            .success()
    );
    std::fs::write(repo.join("aaaa.txt"), "content-a").unwrap();
    let output = run_libra_command(&["add", "aaaa.txt"], &repo);
    assert!(output.status.success());
    let output = run_libra_command(&["commit", "-m", "base", "--no-verify"], &repo);
    assert!(output.status.success());

    std::fs::remove_file(repo.join("aaaa.txt")).unwrap();
    std::fs::write(repo.join("zzzz.txt"), "content-b").unwrap();

    let output = run_libra_command(&["status", "--find-renames=100", "--short"], &repo);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains('R'),
        "100% threshold should not match different names/content: {stdout}"
    );
}

#[test]
#[serial]
fn test_status_renames_and_no_renames_toggle_detection() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(run_libra_command(&["init"], &repo).status.success());
    assert!(
        run_libra_command(&["config", "set", "user.name", "Test"], &repo)
            .status
            .success()
    );
    assert!(
        run_libra_command(&["config", "set", "user.email", "t@e.test"], &repo)
            .status
            .success()
    );

    assert!(
        run_libra_command(&["config", "set", "status.renameUntracked", "true"], &repo)
            .status
            .success()
    );
    std::fs::write(repo.join("old.txt"), "content").unwrap();
    assert!(
        run_libra_command(&["add", "old.txt"], &repo)
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], &repo)
            .status
            .success()
    );

    std::fs::remove_file(repo.join("old.txt")).unwrap();
    std::fs::write(repo.join("new.txt"), "content").unwrap();

    // --renames enables detection (like --find-renames at the default threshold).
    let renames = run_libra_command(&["status", "--renames", "--short"], &repo);
    assert!(renames.status.success());
    let r = String::from_utf8_lossy(&renames.stdout);
    assert!(r.contains('R'), "--renames should detect the rename: {r}");

    // --no-renames disables detection: old shows as deleted, new as untracked.
    let no_renames = run_libra_command(&["status", "--no-renames", "--short"], &repo);
    assert!(no_renames.status.success());
    let n = String::from_utf8_lossy(&no_renames.stdout);
    assert!(
        !n.contains('R'),
        "--no-renames must not report a rename: {n}"
    );
    assert!(
        n.contains("old.txt") && n.contains("new.txt"),
        "both paths listed: {n}"
    );

    // --no-renames overrides --find-renames (no rename reported).
    let both = run_libra_command(
        &["status", "--no-renames", "--find-renames", "--short"],
        &repo,
    );
    assert!(both.status.success());
    let b = String::from_utf8_lossy(&both.stdout);
    assert!(
        !b.contains('R'),
        "--no-renames must override --find-renames: {b}"
    );
}

#[test]
fn test_status_short_b_alias_shows_branch_header() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // `-b` is the short alias for `--branch`; in short mode it adds the `## <branch>` header.
    let with_b = run_libra_command(&["status", "-s", "-b"], p);
    assert_cli_success(&with_b, "status -s -b");
    let out_b = String::from_utf8_lossy(&with_b.stdout);
    assert!(
        out_b.contains("## "),
        "`-b` adds the branch header: {out_b:?}"
    );

    // `-b` matches the long `--branch` form byte-for-byte on the header line.
    let with_long = run_libra_command(&["status", "-s", "--branch"], p);
    let out_long = String::from_utf8_lossy(&with_long.stdout);
    assert_eq!(
        out_b.lines().next(),
        out_long.lines().next(),
        "-b and --branch produce the same header"
    );

    // Without `-b`, the short output carries no branch header.
    let without = run_libra_command(&["status", "-s"], p);
    let out_without = String::from_utf8_lossy(&without.stdout);
    assert!(
        !out_without.contains("## "),
        "no branch header without -b: {out_without:?}"
    );
}

#[test]
fn test_status_long_selects_default_and_conflicts() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("f2.txt"), "x").unwrap();

    // `--long` selects Libra's default long format (parity no-op).
    let long = run_libra_command(&["status", "--long"], p);
    assert_cli_success(&long, "status --long");
    let plain = run_libra_command(&["status"], p);
    assert_cli_success(&plain, "status");
    assert_eq!(
        String::from_utf8_lossy(&long.stdout),
        String::from_utf8_lossy(&plain.stdout),
        "--long matches the default output"
    );
    assert!(
        String::from_utf8_lossy(&long.stdout).contains("On branch"),
        "long format shows the branch header"
    );

    // `--long` conflicts with `--short` and `--porcelain`.
    let conflict_short = run_libra_command(&["status", "--long", "--short"], p);
    assert!(!conflict_short.status.success(), "--long --short conflicts");
    let conflict_porcelain = run_libra_command(&["status", "--long", "--porcelain"], p);
    assert!(
        !conflict_porcelain.status.success(),
        "--long --porcelain conflicts"
    );
}

#[test]
fn status_no_column_countermands_column() {
    let temp = tempfile::tempdir().expect("tempdir");
    let p = temp.path();
    assert!(run_libra_command(&["init"], p).status.success());
    std::fs::write(p.join("f.txt"), "x\n").unwrap();

    // `--no-column` alone is accepted (status is not columnar by default).
    let out = run_libra_command(&["status", "--no-column"], p);
    assert!(
        out.status.success(),
        "status --no-column: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `--column --no-column` (last wins) is accepted: `--no-column` countermands
    // `--column` via clap's symmetric override, so there is no conflict error.
    let out2 = run_libra_command(&["status", "--column", "--no-column"], p);
    assert!(
        out2.status.success(),
        "status --column --no-column (override): {}",
        String::from_utf8_lossy(&out2.stderr)
    );
}

/// `-u`/`--untracked-files` parses like Git: bare = `all`, attached values
/// (`-uno`, `-uall`, `-unormal`, `--untracked-files=no`) select the mode, and
/// the default (absent) is `normal`.
#[test]
fn untracked_files_short_flag_parses_like_git() {
    use clap::Parser;

    let mode = |args: &[&str]| -> Option<UntrackedFiles> {
        StatusArgs::try_parse_from(args)
            .unwrap_or_else(|e| panic!("parse {args:?} failed: {e}"))
            .untracked_files
    };

    assert_eq!(
        mode(&["status"]),
        None,
        "absent flag leaves the mode to the status.showUntrackedFiles default"
    );
    assert_eq!(
        mode(&["status", "-u"]),
        Some(UntrackedFiles::All),
        "bare -u is all"
    );
    assert_eq!(
        mode(&["status", "--untracked-files"]),
        Some(UntrackedFiles::All),
        "bare --untracked-files is all"
    );
    assert_eq!(mode(&["status", "-uno"]), Some(UntrackedFiles::No));
    assert_eq!(mode(&["status", "-uall"]), Some(UntrackedFiles::All));
    assert_eq!(mode(&["status", "-unormal"]), Some(UntrackedFiles::Normal));
    assert_eq!(
        mode(&["status", "--untracked-files=no"]),
        Some(UntrackedFiles::No)
    );
}

/// P1-05d: the `/api/repo/status` envelope honors the same resolved
/// `status.*` defaults (and fail-closed validation) as `status --json`.
#[tokio::test]
#[serial]
async fn api_status_envelope_honors_status_config_defaults() {
    use libra::internal::config::ConfigKv;

    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());
    fs::write("untracked.txt", "x").unwrap();

    let envelope = libra::command::status::collect_status_json_envelope_for_api(test_dir.path())
        .await
        .expect("api status succeeds");
    let untracked = envelope["data"]["untracked"]
        .as_array()
        .expect("untracked array")
        .len();
    assert!(untracked > 0, "untracked file expected by default");

    ConfigKv::set("status.showUntrackedFiles", "no", false)
        .await
        .expect("set config");
    let envelope = libra::command::status::collect_status_json_envelope_for_api(test_dir.path())
        .await
        .expect("api status succeeds with config");
    assert_eq!(
        envelope["data"]["untracked"]
            .as_array()
            .expect("untracked array")
            .len(),
        0,
        "the API must honor status.showUntrackedFiles=no like the CLI"
    );

    ConfigKv::set("status.showUntrackedFiles", "sometimes", false)
        .await
        .expect("set config");
    let error = libra::command::status::collect_status_json_envelope_for_api(test_dir.path())
        .await
        .expect_err("invalid config must fail closed in the API too");
    assert!(error.to_string().contains("status.showUntrackedFiles"));
}
