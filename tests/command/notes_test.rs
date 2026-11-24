//! Tests for `libra notes` — add, list, show, and remove notes attached to commits.
//!
//! **Layer:** L1 — deterministic, no external dependencies.
//!
//! ## Test tiers
//!
//! | Tier | Section | Focus |
//! |------|---------|-------|
//! | 1 | Basic functionality | Happy-path for all 4 subcommands, JSON output, --quiet, --ref |
//! | 2 | Boundary conditions | Empty/long/unicode content, multi-object, cross-ref isolation |
//! | 3 | Error handling | Invalid args, missing objects, unborn HEAD, conflict, file errors |

use tempfile::tempdir;

use super::*;

// ===========================================================================
// Tier 1 — Basic functionality tests
// ===========================================================================

// ── add ────────────────────────────────────────────────────────────────

#[test]
fn basic_add_with_message() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["notes", "add", "-m", "Reviewed-by: Alice"], repo.path());
    assert_cli_success(&output, "notes add -m");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Added note to"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn basic_add_json_output() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(
        &["--json", "notes", "add", "-m", "Reviewed-by: Alice"],
        repo.path(),
    );
    assert_cli_success(&output, "notes add --json");
    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "notes");
    assert_eq!(json["data"]["action"], "add");
    assert!(
        json["data"]["ref"]
            .as_str()
            .unwrap()
            .contains("refs/notes/commits"),
        "expected notes ref, got: {json}"
    );
    assert!(json["data"]["object"].as_str().is_some());
    assert!(json["data"]["note_hash"].as_str().is_some());
}

#[test]
fn basic_add_with_file() {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("msg.txt"), "note from file\n").unwrap();
    let output = run_libra_command(&["notes", "add", "-F", "msg.txt"], repo.path());
    assert_cli_success(&output, "notes add -F");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Added note to"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn basic_add_with_stdin() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command_with_stdin(
        &["notes", "add", "-F", "-"],
        repo.path(),
        "note from stdin\n",
    );
    assert_cli_success(&output, "notes add -F -");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Added note to"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn basic_add_with_multiple_messages() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(
        &["notes", "add", "-m", "Line 1", "-m", "Line 2"],
        repo.path(),
    );
    assert_cli_success(&output, "notes add multiple -m");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Added note to"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn basic_add_force_overwrite() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(&["notes", "add", "-m", "First note"], repo.path());
    run_libra_command(&["notes", "add", "-m", "Second note", "-f"], repo.path());

    let output = run_libra_command(&["notes", "show"], repo.path());
    assert_cli_success(&output, "notes show");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Second note"),
        "expected updated note, got: {stdout}"
    );
}

#[test]
fn basic_add_to_specific_object() {
    let repo = create_committed_repo_via_cli();
    // Add a note to the initial commit by hash
    let log_output = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_cli_success(&log_output, "rev-parse to get commit hash");
    let commit_hash = String::from_utf8_lossy(&log_output.stdout)
        .trim()
        .to_string();

    let output = run_libra_command(
        &[
            "notes",
            "add",
            "-m",
            "Note on specific commit",
            &commit_hash,
        ],
        repo.path(),
    );
    assert_cli_success(&output, "notes add on specific object");
}

// ── list ───────────────────────────────────────────────────────────────

#[test]
fn basic_list_all() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(&["notes", "add", "-m", "Test note"], repo.path());
    let output = run_libra_command(&["notes", "list"], repo.path());
    assert_cli_success(&output, "notes list");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.trim().is_empty(), "expected list output, got empty");
}

#[test]
fn basic_list_empty() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["notes", "list"], repo.path());
    assert_cli_success(&output, "notes list empty");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "expected empty output, got: {stdout}"
    );
}

#[test]
fn basic_list_json() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(&["notes", "add", "-m", "JSON note"], repo.path());
    let output = run_libra_command(&["--json", "notes", "list"], repo.path());
    assert_cli_success(&output, "notes list --json");
    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "notes");
    assert_eq!(json["data"]["action"], "list");
    assert!(
        json["data"]["ref"]
            .as_str()
            .unwrap()
            .contains("refs/notes/commits")
    );
    let notes = json["data"]["notes"]
        .as_array()
        .expect("expected notes array");
    assert_eq!(notes.len(), 1);
    assert!(notes[0]["note_hash"].as_str().is_some());
    assert!(notes[0]["annotated_object"].as_str().is_some());
}

#[test]
fn basic_list_json_empty_returns_empty_array() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["--json", "notes", "list"], repo.path());
    assert_cli_success(&output, "notes list --json empty");
    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "notes");
    assert_eq!(json["data"]["action"], "list");
    let notes = json["data"]["notes"]
        .as_array()
        .expect("expected notes array");
    assert!(notes.is_empty(), "expected empty notes array, got: {json}");
}

#[test]
fn basic_list_by_object() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(&["notes", "add", "-m", "Note for HEAD"], repo.path());
    let output = run_libra_command(&["notes", "list", "HEAD"], repo.path());
    assert_cli_success(&output, "notes list HEAD");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.trim().is_empty(), "expected list output for HEAD");
}

#[test]
fn basic_list_json_by_object() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(&["notes", "add", "-m", "JSON filtered note"], repo.path());
    let output = run_libra_command(&["--json", "notes", "list", "HEAD"], repo.path());
    assert_cli_success(&output, "notes list HEAD --json");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["action"], "list");
    let notes = json["data"]["notes"]
        .as_array()
        .expect("expected notes array");
    assert_eq!(notes.len(), 1);
}

// ── show ───────────────────────────────────────────────────────────────

#[test]
fn basic_show() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(&["notes", "add", "-m", "Show this note"], repo.path());
    let output = run_libra_command(&["notes", "show"], repo.path());
    assert_cli_success(&output, "notes show");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "Show this note");
}

#[test]
fn basic_show_multiline() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(
        &["notes", "add", "-m", "Line 1\nLine 2\nLine 3"],
        repo.path(),
    );
    let output = run_libra_command(&["notes", "show"], repo.path());
    assert_cli_success(&output, "notes show multiline");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Line 1"), "unexpected stdout: {stdout}");
    assert!(stdout.contains("Line 3"), "unexpected stdout: {stdout}");
}

#[test]
fn basic_show_json() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(&["notes", "add", "-m", "JSON show test"], repo.path());
    let output = run_libra_command(&["--json", "notes", "show"], repo.path());
    assert_cli_success(&output, "notes show --json");
    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "notes");
    assert_eq!(json["data"]["action"], "show");
    assert_eq!(json["data"]["text"], "JSON show test");
    assert!(
        json["data"]["ref"]
            .as_str()
            .unwrap()
            .contains("refs/notes/commits")
    );
    assert!(json["data"]["object"].as_str().is_some());
    assert!(json["data"]["note_hash"].as_str().is_some());
}

#[test]
fn basic_show_specific_object() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(&["notes", "add", "-m", "Note on HEAD"], repo.path());
    let output = run_libra_command(&["notes", "show", "HEAD"], repo.path());
    assert_cli_success(&output, "notes show HEAD");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "Note on HEAD");
}

// ── remove ─────────────────────────────────────────────────────────────

#[test]
fn basic_remove_single() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(&["notes", "add", "-m", "To be removed"], repo.path());
    let output = run_libra_command(&["notes", "remove", "HEAD"], repo.path());
    assert_cli_success(&output, "notes remove HEAD");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Removed note from"),
        "unexpected stdout: {stdout}"
    );

    // Verify it's gone
    let show_output = run_libra_command(&["notes", "show"], repo.path());
    assert!(
        !show_output.status.success(),
        "show should fail after remove"
    );
}

#[test]
fn basic_remove_default_head() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(&["notes", "add", "-m", "Note on HEAD"], repo.path());
    let output = run_libra_command(&["notes", "remove"], repo.path());
    assert_cli_success(&output, "notes remove default");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Removed note from"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn basic_remove_json() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(&["notes", "add", "-m", "JSON remove test"], repo.path());
    let output = run_libra_command(&["--json", "notes", "remove", "HEAD"], repo.path());
    assert_cli_success(&output, "notes remove --json");
    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "notes");
    assert_eq!(json["data"]["action"], "remove");
    assert!(
        json["data"]["ref"]
            .as_str()
            .unwrap()
            .contains("refs/notes/commits")
    );
    let removed = json["data"]["removed"]
        .as_array()
        .expect("expected removed array");
    assert_eq!(removed.len(), 1);
    assert!(removed[0]["object"].as_str().is_some());
    assert!(removed[0]["note_hash"].as_str().is_some());
}

// ── custom ref ─────────────────────────────────────────────────────────

#[test]
fn basic_custom_ref() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(
        &[
            "notes",
            "--ref",
            "refs/notes/qa",
            "add",
            "-m",
            "QA reviewed",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "notes add --ref refs/notes/qa");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("refs/notes/qa"),
        "unexpected stdout: {stdout}"
    );

    // List from custom ref
    let list_output = run_libra_command(&["notes", "--ref", "refs/notes/qa", "list"], repo.path());
    assert_cli_success(&list_output, "notes list --ref refs/notes/qa");
    let list_stdout = String::from_utf8_lossy(&list_output.stdout);
    assert!(
        !list_stdout.trim().is_empty(),
        "expected list output for qa ref"
    );

    // Default ref should be empty (no notes there)
    let default_list = run_libra_command(&["notes", "list"], repo.path());
    assert_cli_success(&default_list, "notes list default ref");
    let default_stdout = String::from_utf8_lossy(&default_list.stdout);
    assert!(
        default_stdout.trim().is_empty(),
        "default ref should be empty, got: {default_stdout}"
    );
}

#[test]
fn basic_custom_ref_show_and_remove() {
    let repo = create_committed_repo_via_cli();
    let ref_arg = "--ref";
    let ref_val = "refs/notes/audit";

    // add
    run_libra_command(
        &["notes", ref_arg, ref_val, "add", "-m", "Audit trail"],
        repo.path(),
    );

    // show
    let show_out = run_libra_command(&["notes", ref_arg, ref_val, "show"], repo.path());
    assert_cli_success(&show_out, "show on custom ref");
    assert!(String::from_utf8_lossy(&show_out.stdout).contains("Audit trail"));

    // remove
    let remove_out = run_libra_command(&["notes", ref_arg, ref_val, "remove", "HEAD"], repo.path());
    assert_cli_success(&remove_out, "remove on custom ref");

    // verify gone
    let list_out = run_libra_command(&["notes", ref_arg, ref_val, "list"], repo.path());
    assert_cli_success(&list_out, "list after remove on custom ref");
    assert!(String::from_utf8_lossy(&list_out.stdout).trim().is_empty());
}

// ── quiet ──────────────────────────────────────────────────────────────

#[test]
fn basic_quiet_suppresses_stdout() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(
        &["--quiet", "notes", "add", "-m", "Quiet note"],
        repo.path(),
    );
    assert_cli_success(&output, "quiet notes add");
    assert!(
        output.stdout.is_empty(),
        "quiet mode should keep stdout empty"
    );
}

// ── default subcommand (defaults to list) ─────────────────────────────

#[test]
fn basic_default_subcommand_is_list() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(&["notes", "add", "-m", "Default list test"], repo.path());
    let output = run_libra_command(&["notes"], repo.path());
    assert_cli_success(&output, "notes without subcommand");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.trim().is_empty(), "expected default list output");
}

#[test]
fn basic_default_subcommand_empty() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["notes"], repo.path());
    assert_cli_success(&output, "notes without subcommand (empty)");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "expected empty output, got: {stdout}"
    );
}

// ===========================================================================
// Tier 2 — Boundary condition tests
// ===========================================================================

// ── content edge cases ─────────────────────────────────────────────────

#[test]
fn boundary_add_empty_message() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["notes", "add", "-m", ""], repo.path());
    assert!(!output.status.success(), "empty message should be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("empty note content"),
        "expected empty content error, got: {stderr}"
    );
}

#[test]
fn boundary_add_unicode_content() {
    let repo = create_committed_repo_via_cli();
    let unicode_msg = "审查通过 ✅ — 日本語テスト — 한글 테스트 — émoji 🚀";
    run_libra_command(&["notes", "add", "-m", unicode_msg], repo.path());
    let output = run_libra_command(&["notes", "show"], repo.path());
    assert_cli_success(&output, "show unicode note");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("审查通过"),
        "expected CJK content, got: {stdout}"
    );
    assert!(stdout.contains("🚀"), "expected emoji, got: {stdout}");
}

#[test]
fn boundary_add_very_long_message() {
    let repo = create_committed_repo_via_cli();
    let long_msg = "A".repeat(10_000);
    let output = run_libra_command(&["notes", "add", "-m", &long_msg], repo.path());
    assert_cli_success(&output, "notes add long message");

    let show = run_libra_command(&["notes", "show"], repo.path());
    assert_cli_success(&show, "show long note");
    let stdout = String::from_utf8_lossy(&show.stdout);
    assert_eq!(
        stdout.trim().len(),
        10_000,
        "expected {} chars, got {}",
        10_000,
        stdout.trim().len()
    );
}

#[test]
fn boundary_add_special_chars() {
    let repo = create_committed_repo_via_cli();
    let special = "backslash: \\ \ttab\x1bescape\nnewline\rreturn";
    run_libra_command(&["notes", "add", "-m", special], repo.path());
    let output = run_libra_command(&["notes", "show"], repo.path());
    assert_cli_success(&output, "show special chars note");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("backslash:"),
        "expected backslash content, got: {stdout}"
    );
}

#[test]
fn boundary_add_combined_message_and_file() {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("extra.txt"), "from file\n").unwrap();

    let output = run_libra_command(
        &["notes", "add", "-m", "from -m", "-F", "extra.txt"],
        repo.path(),
    );
    assert_cli_success(&output, "notes add -m and -F combined");

    // Content should be joined with "\n\n": first -m parts, then -F parts
    let show = run_libra_command(&["notes", "show"], repo.path());
    assert_cli_success(&show, "show combined note");
    let stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        stdout.contains("from -m"),
        "expected -m content, got: {stdout}"
    );
    assert!(
        stdout.contains("from file"),
        "expected -F content, got: {stdout}"
    );
}

// ── multi-object / multi-ref ───────────────────────────────────────────

#[test]
fn boundary_multiple_notes_same_ref() {
    let repo = create_committed_repo_via_cli();

    // Create a second commit
    std::fs::write(repo.path().join("f2.txt"), "content2\n").unwrap();
    run_libra_command(&["add", "f2.txt"], repo.path());
    run_libra_command(
        &["commit", "-m", "Second commit", "--no-verify"],
        repo.path(),
    );

    // Add notes to both commits
    run_libra_command(
        &["notes", "add", "-m", "Note on HEAD~1", "HEAD~1"],
        repo.path(),
    );
    run_libra_command(&["notes", "add", "-m", "Note on HEAD", "HEAD"], repo.path());

    // List should now have 2 entries
    let output = run_libra_command(&["--json", "notes", "list"], repo.path());
    assert_cli_success(&output, "list multiple notes --json");
    let json = parse_json_stdout(&output);
    let notes = json["data"]["notes"]
        .as_array()
        .expect("expected notes array");
    assert_eq!(notes.len(), 2, "expected 2 notes, got: {json}");
}

#[test]
fn boundary_remove_multiple_objects() {
    let repo = create_committed_repo_via_cli();

    std::fs::write(repo.path().join("f2.txt"), "content2\n").unwrap();
    run_libra_command(&["add", "f2.txt"], repo.path());
    run_libra_command(
        &["commit", "-m", "Second commit", "--no-verify"],
        repo.path(),
    );

    run_libra_command(&["notes", "add", "-m", "Note 1", "HEAD~1"], repo.path());
    run_libra_command(&["notes", "add", "-m", "Note 2", "HEAD"], repo.path());

    // Remove both notes at once
    let output = run_libra_command(&["notes", "remove", "HEAD~1", "HEAD"], repo.path());
    assert_cli_success(&output, "remove multiple objects");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Removed note from"),
        "unexpected stdout: {stdout}"
    );

    // Verify both are gone
    let list = run_libra_command(&["notes", "list"], repo.path());
    assert_cli_success(&list, "list after removing all");
    assert!(
        String::from_utf8_lossy(&list.stdout).trim().is_empty(),
        "expected empty list after removing all notes"
    );
}

#[test]
fn boundary_remove_atomic_on_mixed_valid_invalid() {
    let repo = create_committed_repo_via_cli();

    // Add a note to HEAD
    run_libra_command(&["notes", "add", "-m", "Keep me"], repo.path());

    // Try to remove HEAD (has a note) + a non-existent object
    let output = run_libra_command(
        &[
            "notes",
            "remove",
            "HEAD",
            "deadbeef00000000000000000000000000000000",
        ],
        repo.path(),
    );
    assert!(
        !output.status.success(),
        "should fail because deadbeef does not exist"
    );

    // The note on HEAD must still exist — the remove was not partially applied
    let show = run_libra_command(&["notes", "show", "HEAD"], repo.path());
    assert_cli_success(&show, "HEAD note must survive atomic remove failure");
    assert_eq!(String::from_utf8_lossy(&show.stdout).trim(), "Keep me");
}

#[test]
fn boundary_cross_ref_isolation() {
    let repo = create_committed_repo_via_cli();

    // Add notes in two different refs for the same object
    run_libra_command(
        &["notes", "--ref", "refs/notes/qa", "add", "-m", "QA note"],
        repo.path(),
    );
    run_libra_command(
        &[
            "notes",
            "--ref",
            "refs/notes/review",
            "add",
            "-m",
            "Review note",
        ],
        repo.path(),
    );

    // qa ref: 1 note
    let qa = run_libra_command(
        &["--json", "notes", "--ref", "refs/notes/qa", "list"],
        repo.path(),
    );
    assert_cli_success(&qa, "list qa --json");
    assert_eq!(
        parse_json_stdout(&qa)["data"]["notes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // review ref: 1 note
    let review = run_libra_command(
        &["--json", "notes", "--ref", "refs/notes/review", "list"],
        repo.path(),
    );
    assert_cli_success(&review, "list review --json");
    assert_eq!(
        parse_json_stdout(&review)["data"]["notes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // default ref: 0 notes
    let default = run_libra_command(&["--json", "notes", "list"], repo.path());
    assert_cli_success(&default, "list default --json");
    assert_eq!(
        parse_json_stdout(&default)["data"]["notes"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn boundary_force_update_preserves_object_hash() {
    let repo = create_committed_repo_via_cli();

    let first = run_libra_command(&["--json", "notes", "add", "-m", "First"], repo.path());
    assert_cli_success(&first, "first add");
    let first_obj = parse_json_stdout(&first)["data"]["object"]
        .as_str()
        .unwrap()
        .to_string();

    let second = run_libra_command(
        &["--json", "notes", "add", "-m", "Second", "-f"],
        repo.path(),
    );
    assert_cli_success(&second, "force add");
    let second_obj = parse_json_stdout(&second)["data"]["object"]
        .as_str()
        .unwrap()
        .to_string();

    // Object hash should stay the same (same commit)
    assert_eq!(
        first_obj, second_obj,
        "object hash changed after force update"
    );
    // Note hash should differ (different content)
    let first_hash = parse_json_stdout(&first)["data"]["note_hash"]
        .as_str()
        .unwrap()
        .to_string();
    let second_hash = parse_json_stdout(&second)["data"]["note_hash"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(
        first_hash, second_hash,
        "note hash should change after force update"
    );
}

#[test]
fn boundary_json_list_multiple_entries() {
    let repo = create_committed_repo_via_cli();

    // Add 3 notes (all on same object — they're 3 rows with different refs)
    run_libra_command(
        &["notes", "--ref", "refs/notes/a", "add", "-m", "A"],
        repo.path(),
    );
    run_libra_command(
        &["notes", "--ref", "refs/notes/b", "add", "-m", "B"],
        repo.path(),
    );
    run_libra_command(
        &["notes", "--ref", "refs/notes/c", "add", "-m", "C"],
        repo.path(),
    );

    // Each ref lists 1 note
    for ref_name in &["refs/notes/a", "refs/notes/b", "refs/notes/c"] {
        let out = run_libra_command(&["--json", "notes", "--ref", ref_name, "list"], repo.path());
        assert_cli_success(&out, &format!("list {ref_name}"));
        let json = parse_json_stdout(&out);
        let notes = json["data"]["notes"]
            .as_array()
            .expect("expected notes array");
        assert_eq!(notes.len(), 1, "expected 1 note in {ref_name}");
        assert_eq!(notes[0]["note_hash"].as_str().unwrap().len(), 40);
        assert_eq!(notes[0]["annotated_object"].as_str().unwrap().len(), 40);
    }
}

#[test]
fn boundary_ref_exact_prefix() {
    let repo = create_committed_repo_via_cli();
    // "refs/notes/" is a valid prefix but short; it should be accepted
    let output = run_libra_command(
        &["notes", "--ref", "refs/notes/", "add", "-m", "At root"],
        repo.path(),
    );
    // This is technically a valid notes ref per validation
    // (starts_with "refs/notes/") — but may or may not work depending on
    // internal use. Just verify it's handled gracefully.
    let _ = output; // Accept either success or clean error
}

// ===========================================================================
// Tier 3 — Error handling tests
// ===========================================================================

// ── usage / argument errors ────────────────────────────────────────────

#[test]
fn error_add_without_message_falls_back_to_editor_then_no_editor() {
    let repo = create_committed_repo_via_cli();
    // No -m/-F: `notes add` now opens an editor. The test environment has no
    // GIT_EDITOR/EDITOR and a non-terminal stdin, so it fails with a clear
    // "no editor configured" error (exit 128) rather than the old usage error.
    let output = run_libra_command(&["notes", "add"], repo.path());
    let (stderr, _report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert!(
        stderr.contains("no editor configured"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn error_add_json_without_message() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["--json", "notes", "add"], repo.path());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("expected stderr JSON");

    // No editor in the test env → the editor fallback reports a fatal error.
    assert_eq!(output.status.code(), Some(128));
    assert!(
        output.stdout.is_empty(),
        "json error should keep stdout empty"
    );
    assert_eq!(report["error_code"], "LBR-REPO-003");
}

#[test]
fn error_add_nonexistent_file() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["notes", "add", "-F", "no_such_file.txt"], repo.path());

    assert!(
        !output.status.success(),
        "expected failure for nonexistent file"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no_such_file") || stderr.contains("failed to read"),
        "expected file-not-found message, got: {stderr}"
    );
}

#[test]
fn error_add_invalid_object() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(
        &[
            "notes",
            "add",
            "-m",
            "Test",
            "deadbeef00000000000000000000000000000000",
        ],
        repo.path(),
    );
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        stderr.contains("invalid object"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        report.hints.iter().any(|h| h.contains("libra log")),
        "expected hint about libra log, got: {:?}",
        report.hints
    );
}

#[test]
fn error_add_json_invalid_object() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(
        &[
            "--json",
            "notes",
            "add",
            "-m",
            "Test",
            "deadbeef00000000000000000000000000000000",
        ],
        repo.path(),
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("expected stderr JSON");

    assert_eq!(output.status.code(), Some(129));
    assert!(output.stdout.is_empty());
    assert_eq!(report["error_code"], "LBR-CLI-003");
}

// ── conflict errors ───────────────────────────────────────────────────

#[test]
fn error_add_duplicate_without_force() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(&["notes", "add", "-m", "Note 1"], repo.path());

    let output = run_libra_command(&["notes", "add", "-m", "Note 2"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("note already exists"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        report.hints.iter().any(|h| h.contains("-f")),
        "expected hint about -f, got: {:?}",
        report.hints
    );
    assert!(output.stdout.is_empty(), "error should keep stdout empty");
}

#[test]
fn error_add_json_duplicate_without_force() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(&["notes", "add", "-m", "Note 1"], repo.path());

    let output = run_libra_command(&["--json", "notes", "add", "-m", "Note 2"], repo.path());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("expected stderr JSON");

    assert_eq!(output.status.code(), Some(128));
    assert!(
        output.stdout.is_empty(),
        "json error should keep stdout empty"
    );
    assert_eq!(report["error_code"], "LBR-CONFLICT-002");
}

// ── repo state errors ─────────────────────────────────────────────────

#[test]
fn error_add_unborn_head() {
    let repo = tempdir().expect("failed to create repo root");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["notes", "add", "-m", "Test"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert_eq!(report.category, "repo");
    assert!(
        stderr.contains("HEAD does not point to a commit"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        report.hints.iter().any(|h| h.contains("create a commit")),
        "expected hint about creating a commit, got: {:?}",
        report.hints
    );
}

#[test]
fn error_show_unborn_head() {
    let repo = tempdir().expect("failed to create repo root");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["notes", "show"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert!(
        stderr.contains("HEAD does not point to a commit"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn error_remove_unborn_head() {
    let repo = tempdir().expect("failed to create repo root");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["notes", "remove"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert!(
        stderr.contains("HEAD does not point to a commit"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn error_list_by_object_unborn_head() {
    let repo = tempdir().expect("failed to create repo root");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["notes", "list", "HEAD"], repo.path());
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
}

#[test]
fn error_add_outside_repo() {
    let cwd = tempdir().expect("failed to create non-repo directory");
    let output = run_libra_command(&["notes", "add", "-m", "Test"], cwd.path());
    assert!(!output.status.success(), "expected failure outside repo");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not a Libra repository")
            || stderr.contains("no libra")
            || stderr.contains("fatal:"),
        "expected recognizable error stderr, got: {stderr}"
    );
}

// ── invalid ref errors ────────────────────────────────────────────────

#[test]
fn error_add_invalid_ref() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(
        &["notes", "--ref", "refs/heads/wrong", "add", "-m", "Test"],
        repo.path(),
    );
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        stderr.contains("must start with 'refs/notes/'"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        report.hints.iter().any(|h| h.contains("refs/notes/")),
        "expected hint about refs/notes/, got: {:?}",
        report.hints
    );
}

#[test]
fn error_list_invalid_ref() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["notes", "--ref", "refs/tags/bad", "list"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        stderr.contains("must start with 'refs/notes/'"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn error_show_invalid_ref() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["notes", "--ref", "refs/heads/main", "show"], repo.path());
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-002");
}

#[test]
fn error_remove_invalid_ref() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(
        &["notes", "--ref", "refs/blobs/x", "remove", "HEAD"],
        repo.path(),
    );
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-002");
}

// ── not found errors ──────────────────────────────────────────────────

#[test]
fn error_show_not_found() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["notes", "show"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        stderr.contains("no note found"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        report.hints.iter().any(|h| h.contains("notes list")),
        "expected hint about notes list, got: {:?}",
        report.hints
    );
}

#[test]
fn error_show_json_not_found() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["--json", "notes", "show"], repo.path());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("expected stderr JSON");

    assert_eq!(output.status.code(), Some(129));
    assert!(output.stdout.is_empty());
    assert_eq!(report["error_code"], "LBR-CLI-003");
}

#[test]
fn error_remove_not_found() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["notes", "remove", "HEAD"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        stderr.contains("no note found"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn error_remove_json_not_found() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["--json", "notes", "remove", "HEAD"], repo.path());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("expected stderr JSON");

    assert_eq!(output.status.code(), Some(129));
    assert!(output.stdout.is_empty());
    assert_eq!(report["error_code"], "LBR-CLI-003");
}

#[test]
fn boundary_list_by_object_returns_null_hash_when_no_note() {
    let repo = create_committed_repo_via_cli();

    // Create a second commit that has no note
    std::fs::write(repo.path().join("new.txt"), "new content\n").unwrap();
    run_libra_command(&["add", "new.txt"], repo.path());
    run_libra_command(
        &["commit", "-m", "Second commit", "--no-verify"],
        repo.path(),
    );

    // JSON: success with note_hash: null
    let output = run_libra_command(&["--json", "notes", "list", "HEAD"], repo.path());
    assert_cli_success(&output, "list HEAD with no note (JSON)");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["action"], "list");
    let notes = json["data"]["notes"]
        .as_array()
        .expect("expected notes array");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["note_hash"], serde_json::Value::Null);
    assert!(notes[0]["annotated_object"].as_str().is_some());
}

#[test]
fn boundary_list_by_object_prints_none_when_no_note() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["notes", "list", "HEAD"], repo.path());
    assert_cli_success(&output, "list HEAD with no note (human)");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(none)"),
        "expected (none) placeholder, got: {stdout}"
    );
}

#[test]
fn error_show_invalid_object() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["notes", "show", "this-ref-does-not-exist"], repo.path());
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
}

#[test]
fn error_remove_with_invalid_object() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["notes", "remove", "nonexistent-ref-9999"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        stderr.contains("invalid object"),
        "unexpected stderr: {stderr}"
    );
}

// ── JSON error output structure ────────────────────────────────────────

#[test]
fn error_json_add_unborn_head() {
    let repo = tempdir().expect("failed to create repo root");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--json", "notes", "add", "-m", "Test"], repo.path());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("expected stderr JSON");

    assert_eq!(output.status.code(), Some(128));
    assert!(output.stdout.is_empty());
    assert_eq!(report["error_code"], "LBR-REPO-003");
}

#[test]
fn error_json_show_not_found_on_clean_repo() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["--json", "notes", "show"], repo.path());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("expected stderr JSON");

    assert_eq!(output.status.code(), Some(129));
    assert!(output.stdout.is_empty());
    assert_eq!(report["error_code"], "LBR-CLI-003");
    assert!(
        report["message"]
            .as_str()
            .unwrap()
            .contains("no note found for object"),
        "expected message to contain 'no note found for object', got: {report}"
    );
    assert_eq!(report["category"], "cli");
    assert!(
        report["hints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h.as_str().unwrap().contains("notes list")),
        "expected hint about notes list, got: {:?}",
        report["hints"]
    );
}

#[test]
fn notes_append_concatenates_to_existing_note() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "first line"], p),
        "notes add",
    );
    assert_cli_success(
        &run_libra_command(&["notes", "append", "-m", "second line"], p),
        "notes append",
    );

    let show = run_libra_command(&["notes", "show"], p);
    assert_cli_success(&show, "notes show after append");
    let text = String::from_utf8_lossy(&show.stdout);
    // Existing note + blank line + appended message.
    assert!(
        text.contains("first line") && text.contains("second line"),
        "appended note keeps both messages: {text:?}"
    );
    let first = text.find("first line").unwrap();
    let second = text.find("second line").unwrap();
    assert!(first < second, "append order preserved: {text:?}");
    assert!(
        text[first..second].contains("\n\n"),
        "messages separated by a blank line: {text:?}"
    );
}

#[test]
fn notes_append_creates_note_when_absent() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // No prior note: append behaves like add.
    assert_cli_success(
        &run_libra_command(&["notes", "append", "-m", "fresh note"], p),
        "notes append (no existing note)",
    );
    let show = run_libra_command(&["notes", "show"], p);
    assert_cli_success(&show, "notes show after append-create");
    assert!(
        String::from_utf8_lossy(&show.stdout).contains("fresh note"),
        "append created the note"
    );
}

#[test]
fn notes_copy_duplicates_note_to_another_object() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // Two commits so we have two distinct objects.
    std::fs::write(p.join("f.txt"), "1\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD~1"], p).stdout)
        .trim()
        .to_string();
    let c2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "source note", &c1], p),
        "notes add on c1",
    );
    // Copy c1's note onto c2.
    assert_cli_success(
        &run_libra_command(&["notes", "copy", &c1, &c2], p),
        "notes copy c1 -> c2",
    );
    let show = run_libra_command(&["notes", "show", &c2], p);
    assert_cli_success(&show, "notes show c2 after copy");
    assert!(
        String::from_utf8_lossy(&show.stdout).contains("source note"),
        "c2 should now carry c1's note"
    );

    // Copying onto an object that already has a note fails without -f.
    let bad = run_libra_command(&["notes", "copy", &c1, &c2], p);
    assert!(
        !bad.status.success(),
        "copy over existing note should fail without -f"
    );
    // ...and succeeds with -f.
    assert_cli_success(
        &run_libra_command(&["notes", "copy", "-f", &c1, &c2], p),
        "notes copy -f over existing note",
    );
}

#[test]
fn notes_copy_fails_when_source_has_no_note() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("f.txt"), "1\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD~1"], p).stdout)
        .trim()
        .to_string();
    let c2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    // Neither object has a note: copying from a note-less source must fail.
    let bad = run_libra_command(&["notes", "copy", &c1, &c2], p);
    assert!(
        !bad.status.success(),
        "copy from an object with no note should fail: {}",
        String::from_utf8_lossy(&bad.stderr)
    );
}

#[test]
fn notes_edit_sets_and_replaces_note() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // edit creates a note when absent...
    assert_cli_success(
        &run_libra_command(&["notes", "edit", "-m", "first"], p),
        "notes edit (create)",
    );
    assert!(
        String::from_utf8_lossy(&run_libra_command(&["notes", "show"], p).stdout).contains("first"),
        "edit created the note"
    );

    // ...and replaces an existing note WITHOUT -f (unlike add).
    assert_cli_success(
        &run_libra_command(&["notes", "edit", "-m", "second"], p),
        "notes edit (replace)",
    );
    let shown =
        String::from_utf8_lossy(&run_libra_command(&["notes", "show"], p).stdout).into_owned();
    assert!(
        shown.contains("second"),
        "edit replaced the note: {shown:?}"
    );
    assert!(!shown.contains("first"), "old note text is gone: {shown:?}");
}

/// PM-09: a manual conflict now leaves `NOTES_MERGE_*` state; these legacy
/// strategy tests clean it up so the later merges in the same test can run.
fn clear_notes_merge_state(repo: &std::path::Path) {
    let dir = repo.join(".libra");
    let _ = std::fs::remove_dir_all(dir.join("NOTES_MERGE_WORKTREE"));
    let _ = std::fs::remove_file(dir.join("NOTES_MERGE_STATE"));
    let _ = std::fs::remove_file(dir.join("NOTES_MERGE_COMMITTING"));
}

#[test]
fn test_notes_merge_strategies_copy_and_manual_conflict() {
    // `notes merge <other-ref>` is a 2-way merge of the flat notes rows: copy
    // objects annotated only in <other>, skip identical notes, and resolve a
    // differing note per --strategy (manual aborts).
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // A second commit so we have two distinct annotatable objects (HEAD, HEAD~1).
    std::fs::write(p.join("f.txt"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f.txt");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );

    // Conflicting notes on HEAD: current ref "AAA", other ref "BBB".
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "AAA", "HEAD"], p),
        "current AAA",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "notes",
                "--ref",
                "refs/notes/other",
                "add",
                "-m",
                "BBB",
                "HEAD",
            ],
            p,
        ),
        "other BBB",
    );
    // Other ref also annotates HEAD~1 (object new to the current ref → copy).
    assert_cli_success(
        &run_libra_command(
            &[
                "notes",
                "--ref",
                "refs/notes/other",
                "add",
                "-m",
                "ONLY",
                "HEAD~1",
            ],
            p,
        ),
        "other ONLY on HEAD~1",
    );

    // Manual (default) aborts on the HEAD conflict and changes nothing.
    let manual = run_libra_command(&["notes", "merge", "refs/notes/other"], p);
    assert!(
        !manual.status.success(),
        "manual merge must abort on conflict"
    );
    assert!(
        String::from_utf8_lossy(&manual.stderr).contains("conflict"),
        "stderr should mention the conflict: {}",
        String::from_utf8_lossy(&manual.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run_libra_command(&["notes", "show", "HEAD"], p).stdout)
            .contains("AAA"),
        "manual abort leaves the current note unchanged"
    );
    // The non-conflicting copy must NOT have happened either (all-or-nothing).
    assert!(
        !run_libra_command(&["notes", "show", "HEAD~1"], p)
            .status
            .success(),
        "manual abort applies nothing, including the copy"
    );
    clear_notes_merge_state(p);

    // --strategy=theirs: HEAD takes the other note, HEAD~1 is copied.
    assert_cli_success(
        &run_libra_command(
            &["notes", "merge", "--strategy=theirs", "refs/notes/other"],
            p,
        ),
        "merge theirs",
    );
    assert!(
        String::from_utf8_lossy(&run_libra_command(&["notes", "show", "HEAD"], p).stdout)
            .contains("BBB"),
        "theirs takes the other note"
    );
    assert!(
        String::from_utf8_lossy(&run_libra_command(&["notes", "show", "HEAD~1"], p).stdout)
            .contains("ONLY"),
        "the non-conflicting note was copied"
    );

    // --strategy=union concatenates both note contents. Re-create a conflict on
    // HEAD (current is now BBB), then a third ref with CCC.
    assert_cli_success(
        &run_libra_command(
            &[
                "notes",
                "--ref",
                "refs/notes/third",
                "add",
                "-m",
                "CCC",
                "HEAD",
            ],
            p,
        ),
        "third CCC",
    );
    assert_cli_success(
        &run_libra_command(
            &["notes", "merge", "--strategy=union", "refs/notes/third"],
            p,
        ),
        "merge union",
    );
    let unioned = String::from_utf8_lossy(&run_libra_command(&["notes", "show", "HEAD"], p).stdout)
        .into_owned();
    assert!(
        unioned.contains("BBB") && unioned.contains("CCC"),
        "union concatenates both notes: {unioned}"
    );

    // Unsupported strategy → usage error (exit 129).
    let bad = run_libra_command(
        &["notes", "merge", "--strategy=bogus", "refs/notes/other"],
        p,
    );
    assert_eq!(
        bad.status.code(),
        Some(129),
        "unknown strategy is a usage error"
    );
}

#[test]
fn test_notes_merge_ours_cat_sort_uniq_and_manual_code() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // --strategy=ours keeps the current note on conflict.
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "KEEP", "HEAD"], p),
        "current KEEP",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "notes",
                "--ref",
                "refs/notes/o",
                "add",
                "-m",
                "DROP",
                "HEAD",
            ],
            p,
        ),
        "other DROP",
    );
    assert_cli_success(
        &run_libra_command(&["notes", "merge", "--strategy=ours", "refs/notes/o"], p),
        "merge ours",
    );
    let kept = String::from_utf8_lossy(&run_libra_command(&["notes", "show", "HEAD"], p).stdout)
        .into_owned();
    assert!(
        kept.contains("KEEP") && !kept.contains("DROP"),
        "ours keeps current: {kept}"
    );

    // Manual conflict carries the stable conflict code (and a non-zero exit).
    let manual = run_libra_command(&["notes", "merge", "refs/notes/o"], p);
    assert!(!manual.status.success(), "manual conflict aborts");
    let (_, report) = parse_cli_error_stderr(&manual.stderr);
    assert_eq!(
        report.error_code, "LBR-CONFLICT-002",
        "manual notes conflict should carry the conflict-blocked stable code"
    );
    clear_notes_merge_state(p);

    // --strategy=cat_sort_uniq combines, sorts, and de-duplicates the lines.
    std::fs::write(p.join("cur.txt"), "banana\napple\n").unwrap();
    std::fs::write(p.join("oth.txt"), "cherry\napple\n").unwrap();
    assert_cli_success(
        &run_libra_command(
            &[
                "notes",
                "--ref",
                "refs/notes/csu_cur",
                "add",
                "-F",
                "cur.txt",
                "HEAD",
            ],
            p,
        ),
        "csu current",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "notes",
                "--ref",
                "refs/notes/csu_oth",
                "add",
                "-F",
                "oth.txt",
                "HEAD",
            ],
            p,
        ),
        "csu other",
    );
    // Merge oth into cur with cat_sort_uniq.
    assert_cli_success(
        &run_libra_command(
            &[
                "notes",
                "--ref",
                "refs/notes/csu_cur",
                "merge",
                "--strategy=cat_sort_uniq",
                "refs/notes/csu_oth",
            ],
            p,
        ),
        "merge cat_sort_uniq",
    );
    let csu = String::from_utf8_lossy(
        &run_libra_command(&["notes", "--ref", "refs/notes/csu_cur", "show", "HEAD"], p).stdout,
    )
    .into_owned();
    // Sorted unique lines: apple, banana, cherry (apple de-duplicated).
    assert_eq!(
        csu.matches("apple").count(),
        1,
        "cat_sort_uniq de-duplicates 'apple': {csu}"
    );
    let apple = csu.find("apple");
    let banana = csu.find("banana");
    let cherry = csu.find("cherry");
    assert!(
        apple < banana && banana < cherry,
        "cat_sort_uniq sorts the lines (apple<banana<cherry): {csu}"
    );
}

#[test]
fn get_ref_prints_active_notes_ref() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    let default = run_libra_command(&["notes", "get-ref"], p);
    assert_cli_success(&default, "notes get-ref");
    assert_eq!(
        String::from_utf8_lossy(&default.stdout).trim(),
        "refs/notes/commits"
    );

    let custom = run_libra_command(&["notes", "--ref", "refs/notes/review", "get-ref"], p);
    assert_cli_success(&custom, "notes --ref get-ref");
    assert_eq!(
        String::from_utf8_lossy(&custom.stdout).trim(),
        "refs/notes/review"
    );
}

#[test]
fn prune_removes_notes_for_missing_objects_only() {
    use super::loose_object_path;

    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // A note on HEAD (its object stays reachable) and a note on a second commit
    // whose object we then delete, making that note stale.
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "keep"], p),
        "note on HEAD",
    );
    let head = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    std::fs::write(p.join("f2.txt"), "2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f2.txt"], p), "add f2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );
    let c2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "stale"], p),
        "note on c2",
    );

    // Move HEAD off c2 and delete c2's object so its note is now orphaned.
    assert_cli_success(&run_libra_command(&["reset", "--hard", &head], p), "reset");
    std::fs::remove_file(loose_object_path(p, &c2)).expect("remove c2 object");

    // `--dry-run -v` reports the stale object without deleting it.
    let dry = run_libra_command(&["notes", "prune", "--dry-run", "-v"], p);
    assert_cli_success(&dry, "notes prune --dry-run");
    assert!(
        String::from_utf8_lossy(&dry.stdout).trim() == c2,
        "dry-run lists the stale object: {}",
        String::from_utf8_lossy(&dry.stdout)
    );

    // The real prune removes the stale note; the HEAD note survives.
    let pruned = run_libra_command(&["notes", "prune", "-v"], p);
    assert_cli_success(&pruned, "notes prune");
    assert_eq!(String::from_utf8_lossy(&pruned.stdout).trim(), c2);

    let list = run_libra_command(&["notes", "list"], p);
    assert_cli_success(&list, "notes list");
    let listed = String::from_utf8_lossy(&list.stdout);
    assert!(
        listed.contains(&head[..7]) && !listed.contains(&c2[..7]),
        "the HEAD note survives and the stale note is gone: {listed}"
    );
}

#[test]
fn prune_aborts_on_unreadable_object_and_keeps_note() {
    use super::loose_object_path;

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "keep"], p),
        "note on HEAD",
    );
    let head = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    // Corrupt (not remove) HEAD's loose object: the file still exists, so the
    // object-store read fails with a NON-ObjectNotFound error. Prune must abort
    // rather than treat the read failure as "object missing".
    std::fs::write(loose_object_path(p, &head), b"corrupt not-an-object").expect("corrupt object");

    let out = run_libra_command(&["notes", "prune"], p);
    assert!(
        !out.status.success(),
        "prune must abort on an unreadable (corrupt) object, not prune it: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // The note must survive — `notes list` reads DB rows (not the corrupt
    // object), so it still reports the note.
    let list = run_libra_command(&["notes", "list"], p);
    assert_cli_success(&list, "notes list after aborted prune");
    assert!(
        String::from_utf8_lossy(&list.stdout).contains(&head[..7]),
        "the note survives the aborted prune: {}",
        String::from_utf8_lossy(&list.stdout)
    );
}

// ── editor fallback (no -m/-F) ──────────────────────────────────────────

/// Write an executable scripted editor that overwrites the buffer ($1) with
/// `body`, returning its path.
#[cfg(unix)]
fn write_note_editor(dir: &std::path::Path, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\nprintf '%s' '{body}' > \"$1\"\n")).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path.to_string_lossy().into_owned()
}

/// `notes add` with no -m/-F opens an editor; the composed text becomes the
/// note. A note may contain `#` lines (they are NOT stripped as comments).
#[cfg(unix)]
#[test]
fn add_without_message_composes_via_editor() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let editor = write_note_editor(p, "compose.sh", "from editor\n# kept hash line\n");
    let out = run_libra_command_with_stdin_and_env(
        &["notes", "add"],
        p,
        "",
        &[("GIT_EDITOR", editor.as_str())],
    );
    assert_cli_success(&out, "notes add via editor");
    let show = run_libra_command(&["notes", "show"], p);
    let shown = String::from_utf8_lossy(&show.stdout);
    assert!(shown.contains("from editor"), "composed note: {shown}");
    assert!(
        shown.contains("# kept hash line"),
        "notes preserve # lines: {shown}"
    );
}

/// `notes edit` with no -m/-F pre-fills the editor with the existing note.
#[cfg(unix)]
#[test]
fn edit_without_message_prefills_existing_note_in_editor() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "original note"], p),
        "seed note",
    );

    // A no-op editor (exit 0) leaves the pre-filled buffer untouched, so the
    // existing note survives the edit — proving it was loaded into the editor.
    let noop = {
        use std::os::unix::fs::PermissionsExt;
        let path = p.join("noop.sh");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path.to_string_lossy().into_owned()
    };
    let out = run_libra_command_with_stdin_and_env(
        &["notes", "edit"],
        p,
        "",
        &[("GIT_EDITOR", noop.as_str())],
    );
    assert_cli_success(&out, "notes edit via editor");
    let show = run_libra_command(&["notes", "show"], p);
    assert!(
        String::from_utf8_lossy(&show.stdout).contains("original note"),
        "edit pre-fills and keeps the existing note: {}",
        String::from_utf8_lossy(&show.stdout)
    );
}

/// `notes append` with no -m/-F composes the appended text in an editor and
/// concatenates it after the existing note (separated by a blank line).
#[cfg(unix)]
#[test]
fn append_without_message_composes_via_editor() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "first line"], p),
        "seed note",
    );
    let editor = write_note_editor(p, "append.sh", "appended line\n");
    let out = run_libra_command_with_stdin_and_env(
        &["notes", "append"],
        p,
        "",
        &[("GIT_EDITOR", editor.as_str())],
    );
    assert_cli_success(&out, "notes append via editor");
    let show = run_libra_command(&["notes", "show"], p);
    let shown = String::from_utf8_lossy(&show.stdout);
    assert!(shown.contains("first line"), "keeps original: {shown}");
    assert!(shown.contains("appended line"), "appends new: {shown}");
}

/// Interactive `notes add` on an object that already has a note pre-fills the
/// editor with that note (with -f) rather than opening an empty buffer.
#[cfg(unix)]
#[test]
fn add_force_without_message_prefills_existing_note() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "preexisting"], p),
        "seed note",
    );
    // A no-op editor keeps the pre-filled buffer; with -f the note is upserted,
    // so the pre-existing text survives — proving it was loaded.
    let noop = write_note_editor(p, "noop_add.sh", "");
    // (overwrite the script to be a true no-op rather than emptying the buffer)
    std::fs::write(&noop, "#!/bin/sh\nexit 0\n").unwrap();
    let out = run_libra_command_with_stdin_and_env(
        &["notes", "add", "-f"],
        p,
        "",
        &[("GIT_EDITOR", noop.as_str())],
    );
    assert_cli_success(&out, "notes add -f via editor");
    assert!(
        String::from_utf8_lossy(&run_libra_command(&["notes", "show"], p).stdout)
            .contains("preexisting"),
        "add -f pre-fills and keeps the existing note"
    );

    // Without -f, interactive add on an existing note aborts early.
    let out2 = run_libra_command_with_stdin_and_env(
        &["notes", "add"],
        p,
        "",
        &[("GIT_EDITOR", noop.as_str())],
    );
    assert!(
        !out2.status.success(),
        "interactive add without -f aborts on an existing note"
    );
    assert!(
        String::from_utf8_lossy(&out2.stderr).contains("already exists"),
        "abort message: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
}

/// An editor that produces only blank/whitespace content aborts the note.
#[cfg(unix)]
#[test]
fn add_with_empty_editor_buffer_aborts() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let blank = write_note_editor(p, "blank.sh", "   \n\n");
    let out = run_libra_command_with_stdin_and_env(
        &["notes", "add"],
        p,
        "",
        &[("GIT_EDITOR", blank.as_str())],
    );
    assert!(!out.status.success(), "empty editor buffer aborts");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("empty note content"),
        "abort message: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// LG-21 (M-NOTESC C1–C4): `-C`/`-c` blob reuse and full-OID `notes list`.
#[test]
#[serial(cwd)]
fn test_notes_blob_reuse_and_list_output() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let blob_path = p.join("blob.txt");
    fs::write(&blob_path, "blob note").unwrap();
    let blob = String::from_utf8_lossy(
        &run_libra_command(&["hash-object", "-w", blob_path.to_str().unwrap()], p).stdout,
    )
    .trim()
    .to_string();
    assert_eq!(blob.len(), 40, "blob oid: {blob}");

    // C1: `-C` reuses the blob content.
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-f", "-C", &blob, "HEAD"], p),
        "notes add -C",
    );
    let shown = run_libra_command(&["notes", "show", "HEAD"], p);
    assert_eq!(String::from_utf8_lossy(&shown.stdout), "blob note");

    // C2: `-c` reuses and edits (editor `true` keeps the text).
    let reedit = run_libra_command_with_env(
        &["notes", "add", "-f", "-c", &blob, "HEAD"],
        p,
        &[("GIT_EDITOR", "true")],
    );
    assert_cli_success(&reedit, "notes add -c");
    let shown = run_libra_command(&["notes", "show", "HEAD"], p);
    assert_eq!(String::from_utf8_lossy(&shown.stdout), "blob note");

    // C3: `-m` composes with `-C`; `append -C` appends the same blob.
    assert_cli_success(
        &run_libra_command(
            &["notes", "add", "-f", "-m", "first", "-C", &blob, "HEAD"],
            p,
        ),
        "notes add -m -C",
    );
    let combined =
        String::from_utf8_lossy(&run_libra_command(&["notes", "show", "HEAD"], p).stdout)
            .into_owned();
    assert!(combined.contains("first"), "{combined}");
    assert!(combined.contains("blob note"), "{combined}");
    assert_cli_success(
        &run_libra_command(&["notes", "append", "-C", &blob, "HEAD"], p),
        "notes append -C",
    );
    let appended =
        String::from_utf8_lossy(&run_libra_command(&["notes", "show", "HEAD"], p).stdout)
            .into_owned();
    assert_eq!(appended.matches("blob note").count(), 2, "{appended}");

    // C4: `notes list` prints full 40-character object ids.
    let list = run_libra_command(&["notes", "list"], p);
    assert_cli_success(&list, "notes list");
    let list_text = String::from_utf8_lossy(&list.stdout).into_owned();
    let fields: Vec<&str> = list_text.split_whitespace().collect();
    assert_eq!(fields.len(), 2, "{list_text}");
    assert_eq!(fields[0].len(), 40, "{list_text}");
    assert_eq!(fields[1].len(), 40, "{list_text}");
    let scoped = run_libra_command(&["notes", "list", "HEAD"], p);
    assert_cli_success(&scoped, "notes list HEAD");
    assert_eq!(String::from_utf8_lossy(&scoped.stdout).trim().len(), 40);
}

/// PM-08: the default notes ref follows `core.notesRef` (local → global →
/// system); an explicit `--ref` wins, and a configured value outside
/// `refs/notes/` is rejected by every subcommand except `get-ref`.
#[test]
fn notes_default_ref_follows_config_scopes() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    std::fs::write(repo.path().join("f.txt"), "a\n").expect("write fixture");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], repo.path()), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], repo.path()),
        "commit",
    );
    let commit =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], repo.path()).stdout)
            .trim()
            .to_string();
    let set = |scope: &[&str], value: &str| {
        let mut args = vec!["config"];
        args.extend_from_slice(scope);
        args.extend_from_slice(&["set", "core.notesRef", value]);
        assert_cli_success(&run_libra_command(&args, repo.path()), "config set");
    };
    let unset = |scope: &[&str]| {
        let mut args = vec!["config"];
        args.extend_from_slice(scope);
        args.extend_from_slice(&["unset", "core.notesRef"]);
        let _ = run_libra_command(&args, repo.path());
    };
    let get_ref = || {
        let out = run_libra_command(&["notes", "get-ref"], repo.path());
        assert_cli_success(&out, "notes get-ref");
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    // AC1/AC2: a local value is used, and `add` writes under that ref.
    set(&[], "refs/notes/custom");
    assert_eq!(get_ref(), "refs/notes/custom\n", "AC1");
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "x", &commit], repo.path()),
        "AC2 add",
    );
    let shown = run_libra_command(
        &["notes", "--ref", "refs/notes/custom", "show", &commit],
        repo.path(),
    );
    assert_cli_success(&shown, "AC2 show custom");
    assert_eq!(String::from_utf8_lossy(&shown.stdout), "x", "AC2 note body");
    let other = run_libra_command(
        &["notes", "--ref", "refs/notes/commits", "show", &commit],
        repo.path(),
    );
    assert!(!other.status.success(), "AC2 the default ref stays empty");

    // AC6: an explicit `--ref` wins over the configuration.
    let explicit = run_libra_command(&["notes", "--ref", "opt", "get-ref"], repo.path());
    assert_cli_success(&explicit, "AC6");
    assert_eq!(
        String::from_utf8_lossy(&explicit.stdout),
        "refs/notes/opt\n",
        "AC6"
    );

    // AC8/AC7: `get-ref` echoes an out-of-namespace value verbatim, but `add`
    // rejects it before writing.
    set(&[], "refs/heads/x");
    assert_eq!(get_ref(), "refs/heads/x\n", "AC8");
    let rejected = run_libra_command(&["notes", "add", "-m", "y", &commit], repo.path());
    assert!(!rejected.status.success(), "AC7 must fail");
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("LBR-CLI-002"),
        "AC7 code"
    );
    unset(&[]);

    // AC3/AC4: global applies when local is absent, and local wins when both are.
    set(&["--global"], "refs/notes/glob");
    assert_eq!(get_ref(), "refs/notes/glob\n", "AC3");
    set(&[], "refs/notes/custom");
    assert_eq!(get_ref(), "refs/notes/custom\n", "AC4");
    unset(&[]);
    unset(&["--global"]);

    // AC5: the system scope participates in the cascade too.
    set(&["--system"], "refs/notes/sys");
    assert_eq!(get_ref(), "refs/notes/sys\n", "AC5");
    unset(&["--system"]);
}

/// PM-09: a manual conflict materializes `NOTES_MERGE_WORKTREE/<oid>` and the
/// versioned `NOTES_MERGE_STATE`, and every leftover blocks a new merge.
#[test]
fn notes_merge_manual_conflict_materializes_state() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let p = repo.path();
    std::fs::write(p.join("a.txt"), "a\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add a");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    std::fs::write(p.join("b.txt"), "b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], p), "add b");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );
    let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD~1"], p).stdout)
        .trim()
        .to_string();
    let c2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "x", &c1], p),
        "notes add x",
    );
    assert_cli_success(
        &run_libra_command(
            &["notes", "--ref", "refs/notes/other", "add", "-m", "y", &c1],
            p,
        ),
        "notes other y",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "notes",
                "--ref",
                "refs/notes/other",
                "add",
                "-m",
                "only other",
                &c2,
            ],
            p,
        ),
        "notes other c2",
    );
    let blob = |text: &str| {
        let out = run_libra_command_with_stdin(&["hash-object", "--stdin"], p, text);
        assert_cli_success(&out, "hash-object");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let blob_x = blob("x");
    let blob_y = blob("y");
    let blob_only = blob("only other");
    let gitdir = p.join(".libra");

    // AC1: the stable conflict code is unchanged.
    let merge = run_libra_command(&["notes", "merge", "refs/notes/other"], p);
    assert!(!merge.status.success(), "AC1 must fail");
    assert!(
        String::from_utf8_lossy(&merge.stderr).contains("LBR-CONFLICT-002"),
        "AC1 code"
    );

    // AC2: the conflict file carries Git's marker format.
    let conflict = std::fs::read_to_string(gitdir.join("NOTES_MERGE_WORKTREE").join(&c1))
        .expect("AC2 conflict file");
    assert_eq!(
        conflict, "<<<<<<< refs/notes/commits\nx\n=======\ny\n>>>>>>> refs/notes/other\n",
        "AC2"
    );

    // AC3: the target ref is untouched (no conflict write, no copied entry).
    let shown = run_libra_command(&["notes", "--ref", "refs/notes/commits", "show", &c1], p);
    assert_cli_success(&shown, "AC3 existing note");
    assert_eq!(String::from_utf8_lossy(&shown.stdout), "x", "AC3 note body");
    let copied = run_libra_command(&["notes", "--ref", "refs/notes/commits", "show", &c2], p);
    assert!(!copied.status.success(), "AC3 nothing copied");

    // AC4: the versioned state records both sides exactly.
    let state_text =
        std::fs::read_to_string(gitdir.join("NOTES_MERGE_STATE")).expect("AC4 state file");
    let state: serde_json::Value = serde_json::from_str(&state_text).expect("AC4 JSON");
    assert_eq!(state["version"], 1, "AC4 version");
    assert_eq!(state["target_ref"], "refs/notes/commits", "AC4 target");
    assert_eq!(state["source_ref"], "refs/notes/other", "AC4 source");
    assert_eq!(
        state["conflicts"],
        serde_json::json!([{"object": c1, "before": blob_x, "source": blob_y}]),
        "AC4 conflicts"
    );
    assert_eq!(
        state["to_copy"],
        serde_json::json!([{"object": c2, "source": blob_only}]),
        "AC4 copies"
    );

    // AC5: any state blocks a new merge.
    let again = run_libra_command(&["notes", "merge", "refs/notes/other"], p);
    assert!(!again.status.success(), "AC5 must fail");
    assert!(
        String::from_utf8_lossy(&again.stderr).contains("LBR-REPO-003"),
        "AC5 code"
    );

    // AC6: only the worktree (a crash before STATE) still blocks.
    std::fs::remove_file(gitdir.join("NOTES_MERGE_STATE")).unwrap();
    let orphan = run_libra_command(&["notes", "merge", "refs/notes/other"], p);
    assert!(!orphan.status.success(), "AC6 must fail");
    assert!(
        String::from_utf8_lossy(&orphan.stderr).contains("LBR-REPO-003"),
        "AC6 code"
    );
    std::fs::remove_dir_all(gitdir.join("NOTES_MERGE_WORKTREE")).unwrap();

    // AC7: an existing lock refuses before touching any state.
    std::fs::write(gitdir.join("NOTES_MERGE_LOCK"), b"").unwrap();
    let locked = run_libra_command(&["notes", "merge", "refs/notes/other"], p);
    assert!(!locked.status.success(), "AC7 must fail");
    assert!(
        String::from_utf8_lossy(&locked.stderr).contains("LBR-CONFLICT-002"),
        "AC7 code"
    );
    std::fs::remove_file(gitdir.join("NOTES_MERGE_LOCK")).unwrap();

    // AC8: a lone commit-in-progress marker also blocks a new merge.
    std::fs::write(gitdir.join("NOTES_MERGE_COMMITTING"), b"").unwrap();
    let committing = run_libra_command(&["notes", "merge", "refs/notes/other"], p);
    assert!(!committing.status.success(), "AC8 must fail");
    assert!(
        String::from_utf8_lossy(&committing.stderr).contains("LBR-REPO-003"),
        "AC8 code"
    );
}

/// PM-29: `notes merge --commit` applies the materialized resolutions in one
/// transaction and cleans up every state file.
mod notes_merge_commit_support {
    use sea_orm::{ConnectOptions, ConnectionTrait, Database, Statement};

    use super::*;

    pub(super) fn db_uri(repo: &std::path::Path) -> String {
        format!(
            "sqlite://{}",
            repo.join(".libra").join("libra.db").display()
        )
    }

    pub(super) fn rows(repo: &std::path::Path) -> Vec<(i64, String, String, String)> {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let mut opts = ConnectOptions::new(db_uri(repo));
            opts.sqlx_logging(false);
            let conn = Database::connect(opts).await.expect("connect");
            let result = conn
                .query_all_raw(Statement::from_string(
                    sea_orm::DatabaseBackend::Sqlite,
                    "SELECT id, notes_ref, object, blob FROM notes ORDER BY id".to_string(),
                ))
                .await
                .expect("read notes rows")
                .into_iter()
                .map(|row| {
                    (
                        row.try_get::<i64>("", "id").unwrap_or_default(),
                        row.try_get::<String>("", "notes_ref").unwrap_or_default(),
                        row.try_get::<String>("", "object").unwrap_or_default(),
                        row.try_get::<String>("", "blob").unwrap_or_default(),
                    )
                })
                .collect();
            conn.close().await.expect("close");
            result
        })
    }

    pub(super) fn exec(repo: &std::path::Path, sql: &str) {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let mut opts = ConnectOptions::new(db_uri(repo));
            opts.sqlx_logging(false);
            let conn = Database::connect(opts).await.expect("connect");
            conn.execute_unprepared(sql).await.expect("exec");
            conn.close().await.expect("close");
        });
    }

    pub(super) fn blob(repo: &std::path::Path, text: &str) -> String {
        let out = run_libra_command_with_stdin(&["hash-object", "--stdin"], repo, text);
        assert_cli_success(&out, "hash-object");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

#[test]
fn notes_merge_commit_applies_resolutions() {
    use notes_merge_commit_support::{blob, exec, rows};

    let build = || {
        let repo = tempdir().expect("failed to create repository root");
        init_repo_via_cli(repo.path());
        configure_identity_via_cli(repo.path());
        let p = repo.path();
        std::fs::write(p.join("a.txt"), "a\n").unwrap();
        assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add a");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
            "commit c1",
        );
        std::fs::write(p.join("b.txt"), "b\n").unwrap();
        assert_cli_success(&run_libra_command(&["add", "b.txt"], p), "add b");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
            "commit c2",
        );
        let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD~1"], p).stdout)
            .trim()
            .to_string();
        let c2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
            .trim()
            .to_string();
        assert_cli_success(
            &run_libra_command(&["notes", "add", "-m", "x", &c1], p),
            "notes add x",
        );
        assert_cli_success(
            &run_libra_command(
                &["notes", "--ref", "refs/notes/other", "add", "-m", "y", &c1],
                p,
            ),
            "notes other y",
        );
        assert_cli_success(
            &run_libra_command(
                &[
                    "notes",
                    "--ref",
                    "refs/notes/other",
                    "add",
                    "-m",
                    "only other",
                    &c2,
                ],
                p,
            ),
            "notes other c2",
        );
        let started = run_libra_command(&["notes", "merge", "refs/notes/other"], p);
        assert!(!started.status.success(), "manual merge starts the state");
        (repo, c1, c2)
    };

    // AC1/AC2: the resolution replaces the conflict row, the copy is applied,
    // and every state file disappears.
    let (repo, c1, c2) = build();
    let p = repo.path();
    let before = rows(p);
    let resolved = blob(p, "resolved\n");
    std::fs::write(
        p.join(".libra").join("NOTES_MERGE_WORKTREE").join(&c1),
        "resolved\n",
    )
    .unwrap();
    assert_cli_success(
        &run_libra_command(&["notes", "merge", "--commit"], p),
        "AC1 commit",
    );
    let after = rows(p);
    let old = before
        .iter()
        .find(|(_, r, o, _)| r == "refs/notes/commits" && o == &c1)
        .expect("old row");
    let new = after
        .iter()
        .find(|(_, r, o, _)| r == "refs/notes/commits" && o == &c1)
        .expect("new row");
    assert_eq!(new.0, old.0, "AC1 id preserved");
    assert_eq!(new.3, resolved, "AC1 resolved blob");
    assert!(
        after.iter().any(|(id, r, o, b)| {
            r == "refs/notes/commits" && o == &c2 && b == &blob(p, "only other") && *id > old.0
        }),
        "AC1 copied row"
    );
    for name in [
        "NOTES_MERGE_STATE",
        "NOTES_MERGE_WORKTREE",
        "NOTES_MERGE_COMMITTING",
    ] {
        assert!(!p.join(".libra").join(name).exists(), "AC2 {name} removed");
    }

    // AC3: a deleted resolution file keeps the target-side note.
    let (repo, c1, c2) = build();
    let p = repo.path();
    let before = rows(p);
    std::fs::remove_file(p.join(".libra").join("NOTES_MERGE_WORKTREE").join(&c1)).unwrap();
    assert_cli_success(
        &run_libra_command(&["notes", "merge", "--commit"], p),
        "AC3 commit",
    );
    let after = rows(p);
    assert!(
        after
            .iter()
            .any(|(_, r, o, b)| r == "refs/notes/commits" && o == &c1 && b == &blob(p, "x")),
        "AC3 conflict row keeps its value"
    );
    assert!(
        after.iter().any(|(id, r, o, _)| r == "refs/notes/commits"
            && o == &c2
            && !before.iter().any(|(old_id, ..)| old_id == id)),
        "AC3 copy row"
    );

    // AC4: a concurrent change is detected BEFORE any write. A probe trigger
    // turns any notes write into an error, so an implementation that writes
    // first would fail with the probe error instead of the conflict code.
    let (repo, c1, _c2) = build();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-f", "-m", "changed", &c1], p),
        "AC4 concurrent add",
    );
    exec(
        p,
        "CREATE TRIGGER notes_write_probe BEFORE INSERT ON notes BEGIN SELECT RAISE(ABORT, 'probe'); END;",
    );
    exec(
        p,
        "CREATE TRIGGER notes_write_probe_update BEFORE UPDATE ON notes BEGIN SELECT RAISE(ABORT, 'probe'); END;",
    );
    std::fs::write(
        p.join(".libra").join("NOTES_MERGE_WORKTREE").join(&c1),
        "resolved\n",
    )
    .unwrap();
    let raced = run_libra_command(&["notes", "merge", "--commit"], p);
    assert!(!raced.status.success(), "AC4 must fail");
    assert!(
        String::from_utf8_lossy(&raced.stderr).contains("LBR-CONFLICT-002"),
        "AC4 code: {}",
        String::from_utf8_lossy(&raced.stderr)
    );
    assert!(
        p.join(".libra").join("NOTES_MERGE_STATE").exists(),
        "AC4 state is kept"
    );

    // AC5: no merge state.
    let plain = tempdir().expect("failed to create repository root");
    init_repo_via_cli(plain.path());
    configure_identity_via_cli(plain.path());
    let none = run_libra_command(&["notes", "merge", "--commit"], plain.path());
    assert!(!none.status.success(), "AC5 must fail");
    assert!(
        String::from_utf8_lossy(&none.stderr).contains("LBR-REPO-003"),
        "AC5 code"
    );

    // AC6: corrupt state.
    let (repo, _c1, _c2) = build();
    let p = repo.path();
    std::fs::write(p.join(".libra").join("NOTES_MERGE_STATE"), "garbage").unwrap();
    let corrupt = run_libra_command(&["notes", "merge", "--commit"], p);
    assert!(!corrupt.status.success(), "AC6 must fail");
    assert!(
        String::from_utf8_lossy(&corrupt.stderr).contains("LBR-REPO-002"),
        "AC6 code"
    );

    // AC7: an existing lock wins over any state access.
    let (repo, _c1, _c2) = build();
    let p = repo.path();
    std::fs::write(p.join(".libra").join("NOTES_MERGE_LOCK"), b"").unwrap();
    let locked = run_libra_command(&["notes", "merge", "--commit"], p);
    assert!(!locked.status.success(), "AC7 must fail");
    assert!(
        String::from_utf8_lossy(&locked.stderr).contains("LBR-CONFLICT-002"),
        "AC7 code"
    );

    // AC8: a copy row already holding the target value is skipped, not raced.
    let (repo, c1, c2) = build();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "only other", &c2], p),
        "AC8 pre-copy",
    );
    let s8 = rows(p);
    std::fs::write(
        p.join(".libra").join("NOTES_MERGE_WORKTREE").join(&c1),
        "resolved\n",
    )
    .unwrap();
    assert_cli_success(
        &run_libra_command(&["notes", "merge", "--commit"], p),
        "AC8 commit",
    );
    let after = rows(p);
    assert_eq!(after.len(), s8.len(), "AC8 no duplicate copy row");
    assert_eq!(
        after
            .iter()
            .find(|(_, r, o, _)| r == "refs/notes/commits" && o == &c1)
            .map(|(_, _, _, b)| b.clone()),
        Some(blob(p, "resolved\n")),
        "AC8 resolution applied"
    );
}

/// PM-38: retrying `notes merge --commit` after a crash under the
/// `NOTES_MERGE_COMMITTING` marker only confirms, never replays.
#[test]
fn notes_merge_commit_retry_after_crash() {
    use notes_merge_commit_support::{blob, exec, rows};

    let build = || {
        let repo = tempdir().expect("failed to create repository root");
        init_repo_via_cli(repo.path());
        configure_identity_via_cli(repo.path());
        let p = repo.path();
        std::fs::write(p.join("a.txt"), "a\n").unwrap();
        assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add a");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
            "commit c1",
        );
        std::fs::write(p.join("b.txt"), "b\n").unwrap();
        assert_cli_success(&run_libra_command(&["add", "b.txt"], p), "add b");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
            "commit c2",
        );
        let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD~1"], p).stdout)
            .trim()
            .to_string();
        let c2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
            .trim()
            .to_string();
        assert_cli_success(
            &run_libra_command(&["notes", "add", "-m", "x", &c1], p),
            "notes add x",
        );
        assert_cli_success(
            &run_libra_command(
                &["notes", "--ref", "refs/notes/other", "add", "-m", "y", &c1],
                p,
            ),
            "notes other y",
        );
        assert_cli_success(
            &run_libra_command(
                &[
                    "notes",
                    "--ref",
                    "refs/notes/other",
                    "add",
                    "-m",
                    "only other",
                    &c2,
                ],
                p,
            ),
            "notes other c2",
        );
        let started = run_libra_command(&["notes", "merge", "refs/notes/other"], p);
        assert!(!started.status.success(), "manual merge starts the state");
        (repo, c1, c2)
    };

    // Simulate "crashed after the transaction committed but before cleanup" by
    // replaying the state we saved before a real commit.
    let save_state = |p: &std::path::Path, dir: &std::path::Path| {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::copy(
            p.join(".libra").join("NOTES_MERGE_STATE"),
            dir.join("NOTES_MERGE_STATE"),
        )
        .expect("save NOTES_MERGE_STATE");
        let worktree = p.join(".libra").join("NOTES_MERGE_WORKTREE");
        let saved = dir.join("NOTES_MERGE_WORKTREE");
        std::fs::create_dir_all(&saved).unwrap();
        for entry in std::fs::read_dir(&worktree).expect("worktree") {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), saved.join(entry.file_name())).unwrap();
        }
    };
    let restore_state = |p: &std::path::Path, dir: &std::path::Path, with_marker: bool| {
        let _ = std::fs::remove_dir_all(p.join(".libra").join("NOTES_MERGE_WORKTREE"));
        eprintln!(
            "restore from {:?} => {:?}",
            dir.join("NOTES_MERGE_STATE"),
            p.join(".libra")
        );
        eprintln!("saved exists: {}", dir.join("NOTES_MERGE_STATE").exists());
        std::fs::copy(
            dir.join("NOTES_MERGE_STATE"),
            p.join(".libra").join("NOTES_MERGE_STATE"),
        )
        .unwrap();
        let saved = dir.join("NOTES_MERGE_WORKTREE");
        let worktree = p.join(".libra").join("NOTES_MERGE_WORKTREE");
        std::fs::create_dir_all(&worktree).unwrap();
        for entry in std::fs::read_dir(&saved).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), worktree.join(entry.file_name())).unwrap();
        }
        if with_marker {
            std::fs::write(p.join(".libra").join("NOTES_MERGE_COMMITTING"), b"").unwrap();
        }
    };

    // AC1/AC2: post-commit crash, probe in place -> confirm-only success.
    let (repo, c1, _c2) = build();
    let p = repo.path();
    let saved = tempdir().expect("saved state");
    std::fs::write(
        p.join(".libra").join("NOTES_MERGE_WORKTREE").join(&c1),
        "resolved\n",
    )
    .unwrap();
    save_state(p, saved.path());
    assert_cli_success(
        &run_libra_command(&["notes", "merge", "--commit"], p),
        "real commit",
    );
    let s1 = rows(p);
    restore_state(p, saved.path(), true);
    exec(
        p,
        "CREATE TRIGGER notes_write_probe BEFORE INSERT ON notes BEGIN SELECT RAISE(ABORT, 'probe'); END;",
    );
    exec(
        p,
        "CREATE TRIGGER notes_write_probe_update BEFORE UPDATE ON notes BEGIN SELECT RAISE(ABORT, 'probe'); END;",
    );
    let retry = run_libra_command(&["notes", "merge", "--commit"], p);
    assert_cli_success(&retry, "AC1 retry");
    assert_eq!(rows(p), s1, "AC1 no rows written");
    for name in [
        "NOTES_MERGE_STATE",
        "NOTES_MERGE_WORKTREE",
        "NOTES_MERGE_COMMITTING",
    ] {
        assert!(!p.join(".libra").join(name).exists(), "AC2 {name} removed");
    }

    // AC3: a value changed back to the pre-merge value must fail closed.
    let (repo, c1, _c2) = build();
    let p = repo.path();
    let saved = tempdir().expect("saved state");
    std::fs::write(
        p.join(".libra").join("NOTES_MERGE_WORKTREE").join(&c1),
        "resolved\n",
    )
    .unwrap();
    save_state(p, saved.path());
    assert_cli_success(
        &run_libra_command(&["notes", "merge", "--commit"], p),
        "commit",
    );
    restore_state(p, saved.path(), true);
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-f", "-m", "x note", &c1], p),
        "AC3 revert",
    );
    let failed = run_libra_command(&["notes", "merge", "--commit"], p);
    assert!(!failed.status.success(), "AC3 must fail");
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("LBR-CONFLICT-002"),
        "AC3 code"
    );

    // AC4: deleting the already-copied entry must also fail closed.
    let (repo, _c1, c2) = build();
    let p = repo.path();
    let saved = tempdir().expect("saved state");
    save_state(p, saved.path());
    assert_cli_success(
        &run_libra_command(&["notes", "merge", "--commit"], p),
        "commit",
    );
    restore_state(p, saved.path(), true);
    assert_cli_success(
        &run_libra_command(&["notes", "remove", &c2], p),
        "AC4 remove copy",
    );
    let failed = run_libra_command(&["notes", "merge", "--commit"], p);
    assert!(!failed.status.success(), "AC4 must fail");
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("LBR-CONFLICT-002"),
        "AC4 code"
    );

    // AC5: a pre-transaction crash (rows not at their targets) fails closed.
    let (repo, _c1, _c2) = build();
    let p = repo.path();
    std::fs::write(p.join(".libra").join("NOTES_MERGE_COMMITTING"), b"").unwrap();
    let failed = run_libra_command(&["notes", "merge", "--commit"], p);
    assert!(!failed.status.success(), "AC5 must fail");
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("LBR-CONFLICT-002"),
        "AC5 code"
    );

    // AC6/AC7: F38b — a merge with a single conflict entry and no copied
    // entries; the deleted resolution file keeps `ours`, so every target value
    // already equals the current value and the retry only cleans up.
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let p = repo.path();
    std::fs::write(
        p.join("a.txt"),
        "a
",
    )
    .unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add a");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "x", &c1], p),
        "note x",
    );
    assert_cli_success(
        &run_libra_command(
            &["notes", "--ref", "refs/notes/other", "add", "-m", "y", &c1],
            p,
        ),
        "note y",
    );
    assert!(
        !run_libra_command(&["notes", "merge", "refs/notes/other"], p)
            .status
            .success()
    );
    std::fs::remove_file(p.join(".libra").join("NOTES_MERGE_WORKTREE").join(&c1)).unwrap();
    std::fs::write(p.join(".libra").join("NOTES_MERGE_COMMITTING"), b"").unwrap();
    let s3 = rows(p);
    exec(
        p,
        "CREATE TRIGGER notes_write_probe BEFORE INSERT ON notes BEGIN SELECT RAISE(ABORT, 'probe'); END;",
    );
    exec(
        p,
        "CREATE TRIGGER notes_write_probe_update BEFORE UPDATE ON notes BEGIN SELECT RAISE(ABORT, 'probe'); END;",
    );
    let cleaned = run_libra_command(&["notes", "merge", "--commit"], p);
    assert_cli_success(&cleaned, "AC6 cleanup");
    for name in [
        "NOTES_MERGE_STATE",
        "NOTES_MERGE_WORKTREE",
        "NOTES_MERGE_COMMITTING",
    ] {
        assert!(!p.join(".libra").join(name).exists(), "AC6 {name} removed");
    }
    assert_eq!(rows(p), s3, "AC7 no rows written");

    // AC8: a missing STATE (crash while deleting it) is a state error.
    let (repo, _c1, _c2) = build();
    let p = repo.path();
    std::fs::remove_file(p.join(".libra").join("NOTES_MERGE_STATE")).unwrap();
    std::fs::write(p.join(".libra").join("NOTES_MERGE_COMMITTING"), b"").unwrap();
    let failed = run_libra_command(&["notes", "merge", "--commit"], p);
    assert!(!failed.status.success(), "AC8 must fail");
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("LBR-REPO-003"),
        "AC8 code"
    );
    let _ = blob(p, "unused");
}

/// PM-23: `notes merge --abort` / `--quit` discard the merge state without
/// touching the notes table.
#[test]
fn notes_merge_abort_and_quit_discard_state() {
    use notes_merge_commit_support::rows;

    let build = || {
        let repo = tempdir().expect("failed to create repository root");
        init_repo_via_cli(repo.path());
        configure_identity_via_cli(repo.path());
        let p = repo.path();
        std::fs::write(p.join("a.txt"), "a\n").unwrap();
        assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add a");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
            "commit c1",
        );
        std::fs::write(p.join("b.txt"), "b\n").unwrap();
        assert_cli_success(&run_libra_command(&["add", "b.txt"], p), "add b");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
            "commit c2",
        );
        let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD~1"], p).stdout)
            .trim()
            .to_string();
        let c2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
            .trim()
            .to_string();
        assert_cli_success(
            &run_libra_command(&["notes", "add", "-m", "x", &c1], p),
            "notes add x",
        );
        assert_cli_success(
            &run_libra_command(
                &["notes", "--ref", "refs/notes/other", "add", "-m", "y", &c1],
                p,
            ),
            "notes other y",
        );
        assert_cli_success(
            &run_libra_command(
                &[
                    "notes",
                    "--ref",
                    "refs/notes/other",
                    "add",
                    "-m",
                    "only other",
                    &c2,
                ],
                p,
            ),
            "notes other c2",
        );
        (repo, c1)
    };
    let state_absent = |p: &std::path::Path| {
        for name in [
            "NOTES_MERGE_STATE",
            "NOTES_MERGE_WORKTREE",
            "NOTES_MERGE_COMMITTING",
        ] {
            assert!(
                !p.join(".libra").join(name).exists(),
                "state {name} removed"
            );
        }
    };

    // AC1/AC2: abort clears everything and leaves the notes table untouched.
    let (repo, _c1) = build();
    let p = repo.path();
    let s0 = rows(p);
    assert!(
        !run_libra_command(&["notes", "merge", "refs/notes/other"], p)
            .status
            .success()
    );
    assert_cli_success(
        &run_libra_command(&["notes", "merge", "--abort"], p),
        "AC1 abort",
    );
    state_absent(p);
    assert_eq!(rows(p), s0, "AC2 notes unchanged");

    // AC3/AC4: `--quit` has the same semantics (Libra extension).
    let (repo, _c1) = build();
    let p = repo.path();
    let s0 = rows(p);
    assert!(
        !run_libra_command(&["notes", "merge", "refs/notes/other"], p)
            .status
            .success()
    );
    assert_cli_success(
        &run_libra_command(&["notes", "merge", "--quit"], p),
        "AC3 quit",
    );
    state_absent(p);
    assert_eq!(rows(p), s0, "AC4 notes unchanged");

    // AC5/AC6: a partial residue (worktree + marker, no STATE) is cleaned too.
    let (repo, _c1) = build();
    let p = repo.path();
    let s2 = rows(p);
    std::fs::create_dir_all(p.join(".libra").join("NOTES_MERGE_WORKTREE")).unwrap();
    std::fs::write(p.join(".libra").join("NOTES_MERGE_COMMITTING"), b"").unwrap();
    assert_cli_success(
        &run_libra_command(&["notes", "merge", "--abort"], p),
        "AC5 abort",
    );
    state_absent(p);
    assert_eq!(rows(p), s2, "AC6 notes unchanged");

    // AC7: no merge state at all.
    let plain = tempdir().expect("failed to create repository root");
    init_repo_via_cli(plain.path());
    configure_identity_via_cli(plain.path());
    let none = run_libra_command(&["notes", "merge", "--abort"], plain.path());
    assert!(!none.status.success(), "AC7 must fail");
    assert!(
        String::from_utf8_lossy(&none.stderr).contains("LBR-REPO-003"),
        "AC7 code"
    );

    // AC8: an existing lock wins over the state check.
    let (repo, _c1) = build();
    let p = repo.path();
    assert!(
        !run_libra_command(&["notes", "merge", "refs/notes/other"], p)
            .status
            .success()
    );
    std::fs::write(p.join(".libra").join("NOTES_MERGE_LOCK"), b"").unwrap();
    let locked = run_libra_command(&["notes", "merge", "--abort"], p);
    assert!(!locked.status.success(), "AC8 must fail");
    assert!(
        String::from_utf8_lossy(&locked.stderr).contains("LBR-CONFLICT-002"),
        "AC8 code"
    );
}

/// PM-39: the `notes merge` control forms are validated before any lock or
/// state access.
#[test]
fn notes_merge_mode_validation() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let p = repo.path();

    // AC1: the combination check wins over an existing lock.
    std::fs::write(p.join(".libra").join("NOTES_MERGE_LOCK"), b"").unwrap();
    let locked_controls = run_libra_command(&["notes", "merge", "--commit", "--abort"], p);
    assert!(!locked_controls.status.success(), "AC1 must fail");
    assert!(
        String::from_utf8_lossy(&locked_controls.stderr).contains("LBR-CLI-002"),
        "AC1 code: {}",
        String::from_utf8_lossy(&locked_controls.stderr)
    );
    std::fs::remove_file(p.join(".libra").join("NOTES_MERGE_LOCK")).unwrap();

    for args in [
        // AC2: two control forms.
        vec!["notes", "merge", "--abort", "--quit"],
        // AC3/AC7/AC8: a control form with a strategy.
        vec!["notes", "merge", "-s", "ours", "--commit"],
        vec!["notes", "merge", "-s", "ours", "--abort"],
        vec!["notes", "merge", "-s", "ours", "--quit"],
        // AC4/AC5: a control form with a positional ref.
        vec!["notes", "merge", "--abort", "refs/notes/other"],
        vec!["notes", "merge", "--commit", "refs/notes/other"],
        // AC6: commit + quit.
        vec!["notes", "merge", "--commit", "--quit"],
    ] {
        let out = run_libra_command(&args, p);
        assert!(!out.status.success(), "{args:?} must fail");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-002"),
            "{args:?} code: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // A strategy merge with a positional ref stays valid.
    std::fs::write(p.join("a.txt"), "a\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    let commit = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "x", &commit], p),
        "note x",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "notes",
                "--ref",
                "refs/notes/other",
                "add",
                "-m",
                "y",
                &commit,
            ],
            p,
        ),
        "note y",
    );
    assert_cli_success(
        &run_libra_command(&["notes", "merge", "-s", "ours", "refs/notes/other"], p),
        "strategy merge with a positional ref",
    );
}
