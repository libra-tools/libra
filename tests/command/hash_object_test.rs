//! Tests for `hash-object`, covering Git-compatible blob hashing, stdin input,
//! object writes, and structured output.

use std::{fs, path::Path};

use sea_orm::{ConnectionTrait, Database, Statement};

use super::{
    assert_cli_success, init_repo_via_cli, parse_cli_error_stderr, parse_json_stdout,
    run_libra_command, run_libra_command_with_stdin,
};

const OBJECT_INDEX_REJECTION_TRIGGER: &str = "test_reject_object_index_write";

async fn set_object_index_write_rejection(repo: &Path, enabled: bool) {
    let db_path = repo.join(".libra/libra.db");
    let conn = Database::connect(format!("sqlite://{}", db_path.display()))
        .await
        .expect("open repository database for object-index fixture");
    let sql = if enabled {
        format!(
            "CREATE TRIGGER {OBJECT_INDEX_REJECTION_TRIGGER} \
             BEFORE INSERT ON object_index BEGIN \
             SELECT RAISE(ABORT, 'test object-index write rejection'); END"
        )
    } else {
        format!("DROP TRIGGER IF EXISTS {OBJECT_INDEX_REJECTION_TRIGGER}")
    };
    conn.execute_raw(Statement::from_string(conn.get_database_backend(), sql))
        .await
        .expect("install object-index rejection fixture");
    conn.close()
        .await
        .expect("close object-index fixture connection");
}

#[tokio::test]
async fn hash_object_file_matches_git_blob_hash() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());
    fs::write(repo.path().join("hello.txt"), b"hello world\n").expect("write fixture");

    let output = run_libra_command(&["hash-object", "hello.txt"], repo.path());
    assert_cli_success(&output, "hash-object file should succeed");

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "3b18e512dba79e4c8300dd08aeb37f8e728b8dad"
    );
    assert!(
        output.stderr.is_empty(),
        "stderr should stay clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn hash_object_stdin_matches_git_blob_hash() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());

    let output = run_libra_command_with_stdin(&["hash-object", "--stdin"], repo.path(), "hello");
    assert_cli_success(&output, "hash-object --stdin should succeed");

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0"
    );
}

#[tokio::test]
async fn hash_object_read_only_uses_sha256_repository_format() {
    let repo = tempfile::tempdir().expect("create temp repo");
    let init = run_libra_command(&["init", "--object-format", "sha256"], repo.path());
    assert_cli_success(&init, "failed to initialize sha256 repository");
    fs::write(repo.path().join("hello.txt"), b"hello world\n").expect("write fixture");

    let output = run_libra_command(&["hash-object", "hello.txt"], repo.path());
    assert_cli_success(
        &output,
        "read-only hash-object should use repository object format",
    );

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "0bd69098bd9b9cc5934a610ab65da429b525361147faa7b5b922919e9a23143d"
    );
}

#[tokio::test]
async fn hash_object_file_works_outside_repository() {
    let dir = tempfile::tempdir().expect("create temp dir");
    fs::write(dir.path().join("hello.txt"), b"hello world\n").expect("write fixture");

    let output = run_libra_command(&["hash-object", "hello.txt"], dir.path());
    assert_cli_success(&output, "read-only hash-object should not require repo");

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "3b18e512dba79e4c8300dd08aeb37f8e728b8dad"
    );
}

#[tokio::test]
async fn hash_object_stdin_works_outside_repository() {
    let dir = tempfile::tempdir().expect("create temp dir");

    let output = run_libra_command_with_stdin(&["hash-object", "--stdin"], dir.path(), "hello");
    assert_cli_success(
        &output,
        "read-only hash-object --stdin should not require repo",
    );

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0"
    );
}

#[tokio::test]
async fn hash_object_no_filters_matches_default_hash() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());
    fs::write(repo.path().join("hello.txt"), b"hello").expect("write fixture");

    let output = run_libra_command(&["hash-object", "--no-filters", "hello.txt"], repo.path());
    assert_cli_success(&output, "hash-object --no-filters should succeed");

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0"
    );
}

#[tokio::test]
async fn hash_object_stdin_path_matches_raw_hash_and_reports_source_label() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());

    let output = run_libra_command_with_stdin(
        &[
            "hash-object",
            "--stdin",
            "--path=virtual/input.txt",
            "--json",
        ],
        repo.path(),
        "hello",
    );
    assert_cli_success(&output, "hash-object --stdin --path should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["objects"][0]["source"], "virtual/input.txt");
    assert_eq!(
        json["data"]["objects"][0]["oid"],
        "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0"
    );
}

#[tokio::test]
async fn hash_object_path_conflicts_with_no_filters() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());

    let output = run_libra_command_with_stdin(
        &[
            "hash-object",
            "--stdin",
            "--path=virtual/input.txt",
            "--no-filters",
        ],
        repo.path(),
        "hello",
    );

    assert_eq!(output.status.code(), Some(129));
    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        human.contains("cannot be used with"),
        "expected clap conflict message, got: {human}"
    );
}

#[tokio::test]
async fn hash_object_write_still_requires_repository() {
    let dir = tempfile::tempdir().expect("create temp dir");
    fs::write(dir.path().join("persist.txt"), b"persist me").expect("write fixture");

    let output = run_libra_command(&["hash-object", "-w", "persist.txt"], dir.path());
    assert!(
        !output.status.success(),
        "hash-object -w outside repo should fail"
    );

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-REPO-001");
    assert!(
        human.contains("not a libra repository"),
        "error should explain repo requirement: {human}"
    );
}

#[tokio::test]
async fn hash_object_write_persists_blob_for_cat_file() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());
    fs::write(repo.path().join("persist.txt"), b"persist me").expect("write fixture");

    let output = run_libra_command(&["hash-object", "-w", "persist.txt"], repo.path());
    assert_cli_success(&output, "hash-object -w should succeed");
    let oid = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let type_output = run_libra_command(&["cat-file", "-t", &oid], repo.path());
    assert_cli_success(&type_output, "cat-file should find written blob");
    assert_eq!(String::from_utf8_lossy(&type_output.stdout).trim(), "blob");

    let pretty_output = run_libra_command(&["cat-file", "-p", &oid], repo.path());
    assert_cli_success(&pretty_output, "cat-file -p should print written blob");
    assert_eq!(String::from_utf8_lossy(&pretty_output.stdout), "persist me");
}

#[tokio::test]
async fn hash_object_write_retains_terminal_index_failure_for_next_command_repair() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());
    set_object_index_write_rejection(repo.path(), true).await;

    let output = run_libra_command_with_stdin(
        &["hash-object", "--stdin", "-w", "--json"],
        repo.path(),
        "persist despite index failure",
    );
    assert_cli_success(
        &output,
        "local write should succeed with durable cloud-index repair pending",
    );
    let json = parse_json_stdout(&output);
    assert_eq!(json["ok"], true);
    let oid = json["data"]["objects"][0]["oid"]
        .as_str()
        .expect("hash-object JSON should contain an oid")
        .to_string();
    assert_eq!(oid.len(), 40);
    let warning = String::from_utf8_lossy(&output.stderr);
    assert!(
        warning.contains("durable repair queue"),
        "warning should describe automatic repair: {warning}"
    );
    let marker = repo
        .path()
        .join(".libra/object-index-repair")
        .join(format!("{oid}.blob.json"));
    assert!(
        marker.is_file(),
        "terminal failure must retain its identity"
    );

    let blocked_sync = run_libra_command(&["cloud", "sync"], repo.path());
    assert_eq!(blocked_sync.status.code(), Some(128));
    let (human, report) = parse_cli_error_stderr(&blocked_sync.stderr);
    assert_eq!(report.error_code, "LBR-IO-002");
    assert!(
        human.contains(
            "cannot run this operation while durable local object-index repair is pending"
        ),
        "cloud sync must fail before credentials or uploads: {human}"
    );
    assert!(marker.is_file(), "failed repair must retain its marker");

    set_object_index_write_rejection(repo.path(), false).await;

    let persisted = run_libra_command(&["cat-file", "-p", &oid], repo.path());
    assert_cli_success(&persisted, "object payload should remain readable");
    assert_eq!(
        String::from_utf8_lossy(&persisted.stdout),
        "persist despite index failure"
    );
    assert!(
        !marker.exists(),
        "the next schema-aware repository command should repair and retire the marker"
    );
}

#[tokio::test]
async fn hash_object_pending_index_honors_exit_code_on_warning_and_keeps_success_payload() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());
    set_object_index_write_rejection(repo.path(), true).await;

    let output = run_libra_command_with_stdin(
        &[
            "--json",
            "--exit-code-on-warning",
            "hash-object",
            "--stdin",
            "-w",
        ],
        repo.path(),
        "warning exit payload",
    );
    assert_eq!(output.status.code(), Some(9));
    assert_eq!(parse_json_stdout(&output)["ok"], true);
    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-WARN-001");
    assert!(human.contains("durable repair queue"));

    set_object_index_write_rejection(repo.path(), false).await;

    assert_cli_success(
        &run_libra_command(&["status", "--short"], repo.path()),
        "a later command should repair the warning-exit write",
    );
}

#[tokio::test]
async fn hash_object_batch_prints_successes_before_later_failure() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());
    fs::write(repo.path().join("first.txt"), b"first").expect("write fixture");

    let output = run_libra_command(&["hash-object", "first.txt", "missing.txt"], repo.path());

    assert!(
        !output.status.success(),
        "missing trailing input should fail the command"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "fe4f02ad058b43f6ed467fdf65b935107529564b"
    );

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        human.contains("failed to read 'missing.txt'"),
        "human stderr should explain unreadable input: {human}"
    );
    assert_eq!(report.error_code, "LBR-IO-001");
}

#[tokio::test]
async fn hash_object_partial_write_failure_still_reports_pending_index_repair() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());
    fs::write(repo.path().join("first.txt"), b"first").expect("write fixture");
    set_object_index_write_rejection(repo.path(), true).await;

    let output = run_libra_command_with_stdin(
        &["hash-object", "-w", "first.txt", "missing.txt"],
        repo.path(),
        "",
    );

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "fe4f02ad058b43f6ed467fdf65b935107529564b"
    );
    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-IO-001");
    assert!(human.contains("failed to read 'missing.txt'"));
    assert!(
        human.contains("durable repair queue"),
        "the primary error must retain the repair warning: {human}"
    );

    let marker = repo
        .path()
        .join(".libra/object-index-repair")
        .join("fe4f02ad058b43f6ed467fdf65b935107529564b.blob.json");
    assert!(marker.is_file(), "failed indexing must remain repairable");
    set_object_index_write_rejection(repo.path(), false).await;
    assert_cli_success(
        &run_libra_command(&["status", "--short"], repo.path()),
        "a later command should repair the partial write",
    );
    assert!(!marker.exists(), "repair should retire the durable marker");
}

#[tokio::test]
async fn hash_object_json_reports_source_size_and_write_mode() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());

    let output =
        run_libra_command_with_stdin(&["hash-object", "--stdin", "--json"], repo.path(), "hello");
    assert_cli_success(&output, "hash-object --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "hash-object");
    assert_eq!(json["data"]["object_type"], "blob");
    assert_eq!(json["data"]["write"], false);
    assert_eq!(json["data"]["objects"][0]["source"], "-");
    assert_eq!(json["data"]["objects"][0]["size"], 5);
    assert_eq!(
        json["data"]["objects"][0]["oid"],
        "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0"
    );
    assert_eq!(json["data"]["objects"][0]["written"], false);
}

#[tokio::test]
async fn hash_object_rejects_non_git_object_type() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());
    fs::write(repo.path().join("hello.txt"), b"hello").expect("write fixture");

    // A type outside the four Git object types is rejected outright.
    let output = run_libra_command(&["hash-object", "-t", "widget", "hello.txt"], repo.path());
    assert!(
        !output.status.success(),
        "a non-Git object type should fail"
    );

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        human.contains("unsupported object type 'widget'"),
        "human stderr should explain the unsupported type: {human}"
    );
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        report
            .hints
            .iter()
            .any(|hint| hint.contains("blob, commit, tree, and tag")),
        "hint should list the supported types: {:?}",
        report.hints
    );
}

#[tokio::test]
async fn hash_object_validates_typed_content_and_honors_literally() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());
    fs::write(repo.path().join("junk.txt"), b"not a commit\n").expect("write fixture");

    // `-t commit` on non-commit content fails validation (cleanly, never panicking).
    let invalid = run_libra_command(&["hash-object", "-t", "commit", "junk.txt"], repo.path());
    assert!(
        !invalid.status.success(),
        "invalid commit content should fail"
    );
    let (human, report) = parse_cli_error_stderr(&invalid.stderr);
    assert!(
        human.contains("invalid commit object"),
        "human stderr should report invalid object: {human}"
    );
    assert!(
        !human.contains("panic") && !human.contains("unwrap"),
        "validation must not surface a panic: {human}"
    );
    assert_eq!(report.error_code, "LBR-CLI-002");

    // `--literally` skips validation and still produces a deterministic id.
    let literal = run_libra_command(
        &["hash-object", "-t", "commit", "--literally", "junk.txt"],
        repo.path(),
    );
    assert_cli_success(&literal, "--literally should hash without validation");
    let oid = String::from_utf8_lossy(&literal.stdout).trim().to_string();
    assert_eq!(oid.len(), 40, "expected a 40-hex SHA-1 oid, got: {oid}");
    assert!(oid.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn hash_object_commit_header_byte_handling_matches_git() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());
    const T: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    // A non-UTF-8 byte in the author name is accepted (Git does too).
    let mut non_utf8 = format!("tree {T}\nauthor A").into_bytes();
    non_utf8.push(0xff);
    non_utf8.extend_from_slice(b" <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\nm\n");
    fs::write(repo.path().join("non_utf8"), &non_utf8).expect("write non-utf8 fixture");
    assert_cli_success(
        &run_libra_command(&["hash-object", "-t", "commit", "non_utf8"], repo.path()),
        "non-UTF-8 author byte should be accepted",
    );

    // A NUL byte in the header block is rejected (Git's nulInHeader).
    let mut with_nul = format!("tree {T}\nauthor A").into_bytes();
    with_nul.push(0);
    with_nul.extend_from_slice(b"B <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\nm\n");
    fs::write(repo.path().join("with_nul"), &with_nul).expect("write NUL fixture");
    let nul = run_libra_command(&["hash-object", "-t", "commit", "with_nul"], repo.path());
    assert!(
        !nul.status.success(),
        "a NUL byte in the commit header must be rejected"
    );
}

#[tokio::test]
async fn hash_object_typed_oids_match_git_and_write_persists() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());

    // Empty tree → Git's well-known empty-tree SHA-1.
    let empty_tree =
        run_libra_command_with_stdin(&["hash-object", "-t", "tree", "--stdin"], repo.path(), "");
    assert_cli_success(&empty_tree, "empty tree hash");
    assert_eq!(
        String::from_utf8_lossy(&empty_tree.stdout).trim(),
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    );

    // A fixed, valid commit payload → the same id as `git hash-object -t commit`.
    let commit_payload = "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
         author A U Thor <author@example.com> 0 +0000\n\
         committer A U Thor <author@example.com> 0 +0000\n\nmsg\n";
    fs::write(repo.path().join("commit_payload"), commit_payload).expect("write commit payload");
    let commit = run_libra_command(
        &["hash-object", "-t", "commit", "commit_payload"],
        repo.path(),
    );
    assert_cli_success(&commit, "commit hash");
    assert_eq!(
        String::from_utf8_lossy(&commit.stdout).trim(),
        "28ce33cd971c75ff0050d386244c35d7dfaa80ab"
    );

    // A fixed, valid tag payload → the same id as `git hash-object -t tag`.
    let tag_payload = "object 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
         type tree\ntag v1\ntagger A U Thor <author@example.com> 0 +0000\n\nmsg\n";
    fs::write(repo.path().join("tag_payload"), tag_payload).expect("write tag payload");
    let tag = run_libra_command(&["hash-object", "-t", "tag", "tag_payload"], repo.path());
    assert_cli_success(&tag, "tag hash");
    assert_eq!(
        String::from_utf8_lossy(&tag.stdout).trim(),
        "df80052480428e6e44da5fa8ae6dc6be7dbb7cab"
    );

    // `-w -t commit` persists a loose commit object that cat-file reads back as a commit.
    let written = run_libra_command(
        &["hash-object", "-w", "-t", "commit", "commit_payload"],
        repo.path(),
    );
    assert_cli_success(&written, "write typed commit object");
    let oid = String::from_utf8_lossy(&written.stdout).trim().to_string();
    assert_eq!(oid, "28ce33cd971c75ff0050d386244c35d7dfaa80ab");
    let type_output = run_libra_command(&["cat-file", "-t", &oid], repo.path());
    assert_cli_success(&type_output, "cat-file -t on written commit");
    assert_eq!(
        String::from_utf8_lossy(&type_output.stdout).trim(),
        "commit"
    );
}

#[tokio::test]
async fn hash_object_stdin_paths_hashes_each_path_in_order() {
    let repo = tempfile::tempdir().expect("create temp repo");
    init_repo_via_cli(repo.path());
    let p = repo.path();
    fs::write(p.join("f1.txt"), b"A\n").expect("write f1");
    fs::write(p.join("f2.txt"), b"BB\n").expect("write f2");

    // Reference hashes computed one path at a time.
    let oid1 = String::from_utf8_lossy(&run_libra_command(&["hash-object", "f1.txt"], p).stdout)
        .trim()
        .to_string();
    let oid2 = String::from_utf8_lossy(&run_libra_command(&["hash-object", "f2.txt"], p).stdout)
        .trim()
        .to_string();

    // --stdin-paths hashes each newline-separated path, one hash per line in order.
    let out =
        run_libra_command_with_stdin(&["hash-object", "--stdin-paths"], p, "f1.txt\nf2.txt\n");
    assert_cli_success(&out, "hash-object --stdin-paths should succeed");
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        lines,
        vec![oid1, oid2],
        "one hash per stdin path, in input order"
    );

    // Records are taken verbatim (no whitespace trimming): a trailing space
    // names a different (missing) file, so hashing must fail rather than
    // silently hashing "f1.txt".
    let bad = run_libra_command_with_stdin(&["hash-object", "--stdin-paths"], p, "f1.txt \n");
    assert!(
        !bad.status.success(),
        "trailing-space path must not be trimmed to an existing file: {}",
        String::from_utf8_lossy(&bad.stdout)
    );
}

#[tokio::test]
async fn hash_object_blake3_roundtrip() {
    let repo = tempfile::tempdir().expect("create temp repo");
    let init = run_libra_command(
        &["init", "--vault", "false", "--object-format", "blake3"],
        repo.path(),
    );
    assert_cli_success(&init, "failed to initialize blake3 repository");
    fs::write(repo.path().join("hello.txt"), b"hello world\n").expect("write fixture");

    let output = run_libra_command(&["hash-object", "-w", "hello.txt"], repo.path());
    assert_cli_success(&output, "hash-object -w blake3");
    let oid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(oid.len(), 64, "blake3 blob OID must be 64 hex: {oid}");
    assert!(
        oid.chars().all(|c| c.is_ascii_hexdigit()),
        "blake3 OID must be hex: {oid}"
    );

    let typed = run_libra_command(&["cat-file", "-t", &oid], repo.path());
    assert_cli_success(&typed, "cat-file -t");
    assert_eq!(String::from_utf8_lossy(&typed.stdout).trim(), "blob");

    let printed = run_libra_command(&["cat-file", "-p", &oid], repo.path());
    assert_cli_success(&printed, "cat-file -p");
    assert_eq!(printed.stdout, b"hello world\n");

    let hashed = run_libra_command(&["hash-object", "hello.txt"], repo.path());
    assert_cli_success(&hashed, "hash-object read-only");
    assert_eq!(String::from_utf8_lossy(&hashed.stdout).trim(), oid);
}
