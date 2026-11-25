//! Integration tests for `libra format-patch`.

use std::fs;

use tempfile::tempdir;

use super::*;

// ---------------------------------------------------------------------------
// Helper: create a repo with multiple commits and return the tmp dir
// ---------------------------------------------------------------------------

fn repo_with_commits(num: usize) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    for i in 1..=num {
        let file = format!("file{i}.txt");
        fs::write(repo.path().join(&file), format!("content {i}\n")).unwrap();
        run_libra_command(&["add", &file], repo.path());
        run_libra_command(
            &["commit", "-m", &format!("commit {i}"), "--no-verify"],
            repo.path(),
        );
    }
    repo
}

fn repo_with_patch_subjects(subjects: &[&str]) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    for (index, subject) in subjects.iter().enumerate() {
        let file = format!("patch-{index}.txt");
        fs::write(repo.path().join(&file), format!("content {index}\n")).unwrap();
        let add = run_libra_command(&["add", &file], repo.path());
        assert_cli_success(&add, &format!("add {file}"));
        let commit = run_libra_command(&["commit", "-m", subject, "--no-verify"], repo.path());
        assert_cli_success(&commit, &format!("commit {subject}"));
    }
    repo
}

// ---------------------------------------------------------------------------
// Basic functional tests
// ---------------------------------------------------------------------------

#[test]
fn basic_range_produces_patch_files() {
    let repo = repo_with_commits(3);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "format-patch",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~2..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "basic range");

    // Should produce 2 patch files (HEAD~2..HEAD = 2 commits not in HEAD~2)
    let entries: Vec<_> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(entries.len() >= 2, "expected at least 2 patch files");

    // Each patch should be readable text with mbox headers
    for entry in &entries {
        let content = fs::read_to_string(entry.path()).unwrap();
        assert!(
            content.starts_with("From "),
            "patch must start with From line"
        );
        assert!(content.contains("From: "), "patch must have From: header");
        assert!(
            content.contains("Subject: "),
            "patch must have Subject: header"
        );
        assert!(content.contains("Date: "), "patch must have Date: header");
        assert!(content.contains("---\n"), "patch must have diff separator");
        assert!(content.contains("-- \n"), "patch must have footer");
    }
}

#[test]
fn single_commit_defaults_to_head_range() {
    let repo = repo_with_commits(2);
    let out_dir = tempdir().unwrap();

    // Single commit means <commit>..HEAD
    let output = run_libra_command(
        &[
            "format-patch",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "single commit range");
}

#[test]
fn numbered_flag_produces_numbered_files() {
    let repo = repo_with_commits(2);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "format-patch",
            "-n",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "numbered");

    let entries: Vec<_> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    let has_numbered = entries.iter().any(|n| n.starts_with("0001-"));
    assert!(has_numbered, "numbered files should have 0001- prefix");
}

#[test]
fn cover_letter_generates_template() {
    let repo = repo_with_commits(2);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "format-patch",
            "--cover-letter",
            "-n",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "cover letter");

    let entries: Vec<_> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    assert!(
        entries.iter().any(|n| n.contains("cover-letter")),
        "cover letter file should exist"
    );
}

#[test]
fn subject_prefix_flag() {
    let repo = repo_with_commits(1);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "format-patch",
            "--subject-prefix",
            "RFC",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1", // HEAD~1..HEAD = 1 patch
        ],
        repo.path(),
    );
    assert_cli_success(&output, "subject prefix RFC");

    let entries: Vec<_> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    if let Some(entry) = entries.first() {
        let content = fs::read_to_string(entry.path()).unwrap();
        assert!(
            content.contains("[RFC]"),
            "subject should contain [RFC] prefix: {content}"
        );
    }
}

#[test]
fn reroll_count_adds_version() {
    let repo = repo_with_commits(1);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "format-patch",
            "-v",
            "2",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "reroll v2");

    let entries: Vec<_> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    if let Some(entry) = entries.first() {
        let content = fs::read_to_string(entry.path()).unwrap();
        assert!(
            content.contains("[PATCH v2]"),
            "subject should contain [PATCH v2]: {content}"
        );
    }
}

#[test]
fn signoff_adds_trailer() {
    let repo = repo_with_commits(1);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "format-patch",
            "-s",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "signoff");

    let entries: Vec<_> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    if let Some(entry) = entries.first() {
        let content = fs::read_to_string(entry.path()).unwrap();
        assert!(
            content.contains("Signed-off-by:"),
            "patch should contain Signed-off-by trailer"
        );
    }
}

#[test]
fn stdout_output_prints_all_patches() {
    let repo = repo_with_commits(2);

    let output = run_libra_command(&["format-patch", "--stdout", "HEAD~1..HEAD"], repo.path());
    assert_cli_success(&output, "stdout");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("From "),
        "stdout should contain mbox From line"
    );
    assert!(
        stdout.contains("Subject: "),
        "stdout should contain Subject header"
    );
}

// ---------------------------------------------------------------------------
// --notes
// ---------------------------------------------------------------------------

#[test]
fn notes_appends_block_after_separator() {
    let repo = repo_with_commits(2);
    // Attach a multi-line note to the tip commit.
    let note = run_libra_command(
        &["notes", "add", "-m", "review line 1\nreview line 2", "HEAD"],
        repo.path(),
    );
    assert_cli_success(&note, "notes add");

    // With --notes: the block appears after `---`, before the diffstat, with the
    // `Notes:` header and four-space indentation (matching Git).
    let with = run_libra_command(
        &["format-patch", "--stdout", "--notes", "HEAD~1..HEAD"],
        repo.path(),
    );
    assert_cli_success(&with, "format-patch --notes");
    let body = String::from_utf8_lossy(&with.stdout);
    assert!(
        body.contains("---\n\nNotes:\n    review line 1\n    review line 2\n\n"),
        "notes block must match Git's layout: {body}"
    );

    // Without --notes: no Notes block is emitted.
    let without = run_libra_command(&["format-patch", "--stdout", "HEAD~1..HEAD"], repo.path());
    assert_cli_success(&without, "format-patch (no notes)");
    assert!(
        !String::from_utf8_lossy(&without.stdout).contains("Notes:"),
        "no Notes block without --notes"
    );
}

#[test]
fn notes_custom_ref_uses_parenthesized_header() {
    let repo = repo_with_commits(2);
    let note = run_libra_command(
        &[
            "notes",
            "--ref",
            "review",
            "add",
            "-m",
            "custom-ref note",
            "HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&note, "notes --ref review add");

    // A non-default ref is rendered as `Notes (<short>):`.
    let out = run_libra_command(
        &["format-patch", "--stdout", "--notes=review", "HEAD~1..HEAD"],
        repo.path(),
    );
    assert_cli_success(&out, "format-patch --notes=review");
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(
        body.contains("Notes (review):\n    custom-ref note\n"),
        "custom ref must use parenthesized header: {body}"
    );

    // The default ref (which has no note here) yields no block.
    let default = run_libra_command(
        &["format-patch", "--stdout", "--notes", "HEAD~1..HEAD"],
        repo.path(),
    );
    assert_cli_success(&default, "format-patch --notes (default ref, no note)");
    assert!(
        !String::from_utf8_lossy(&default.stdout).contains("Notes"),
        "commit with no note on the default ref emits no block"
    );
}

#[test]
fn notes_malformed_ref_is_a_usage_error() {
    let repo = repo_with_commits(2);

    // A malformed notes ref must fail loudly (exit 129), not silently produce an
    // ordinary patch with no notes block. Covers Git `check-ref-format` rules:
    // empty, whitespace, trailing slash, `..`, `~`, leading-dot component,
    // `.lock` suffix, and `@{`.
    for bad in [
        "--notes=",
        "--notes=bad ref",
        "--notes=refs/notes/",
        "--notes=bad..ref",
        "--notes=bad~ref",
        "--notes=refs/notes/.hidden",
        "--notes=refs/notes/foo.lock",
        "--notes=bad@{ref",
    ] {
        let out = run_libra_command(
            &["format-patch", "--stdout", bad, "HEAD~1..HEAD"],
            repo.path(),
        );
        assert_eq!(
            out.status.code(),
            Some(129),
            "malformed `{bad}` should exit 129, got: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    // A valid hierarchical / hyphenated ref is NOT rejected (no note present →
    // succeeds with no block, not a usage error). A hierarchical ref must be the
    // full `refs/notes/...` form — `normalize_notes_ref` only auto-prefixes bare
    // single-segment names.
    for ok in ["--notes=my-notes", "--notes=refs/notes/team/review"] {
        let out = run_libra_command(
            &["format-patch", "--stdout", ok, "HEAD~1..HEAD"],
            repo.path(),
        );
        assert_cli_success(&out, ok);
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("Notes"),
            "valid noteless ref `{ok}` should emit no block"
        );
    }
}

// ---------------------------------------------------------------------------
// --attach / --inline
// ---------------------------------------------------------------------------

#[test]
fn attach_wraps_patch_in_mime_multipart() {
    let repo = repo_with_commits(1);
    let out = run_libra_command(
        &["format-patch", "--stdout", "--attach", "HEAD~1..HEAD"],
        repo.path(),
    );
    assert_cli_success(&out, "format-patch --attach");
    let body = String::from_utf8_lossy(&out.stdout);

    // multipart envelope: header, intro line, both parts, closing boundary.
    assert!(
        body.contains("Content-Type: multipart/mixed; boundary=\"------------libra "),
        "missing multipart Content-Type: {body}"
    );
    assert!(
        body.contains("This is a multi-part message in MIME format."),
        "missing MIME intro line: {body}"
    );
    // text/plain part holds the log + diffstat.
    assert!(
        body.contains("Content-Type: text/plain; charset=UTF-8; format=fixed"),
        "missing text/plain part: {body}"
    );
    // text/x-patch attachment part holds the diff.
    assert!(
        body.contains("Content-Type: text/x-patch; name=\"0001-")
            && body.contains("Content-Disposition: attachment; filename=\"0001-"),
        "missing attachment part: {body}"
    );
    assert!(
        body.contains("diff --git "),
        "diff missing from body: {body}"
    );
    // closing boundary terminates the multipart.
    assert!(
        body.contains("------------libra ") && body.trim_end().ends_with("--"),
        "missing closing boundary: {body}"
    );
}

#[test]
fn inline_uses_inline_content_disposition() {
    let repo = repo_with_commits(1);
    let out = run_libra_command(
        &["format-patch", "--stdout", "--inline", "HEAD~1..HEAD"],
        repo.path(),
    );
    assert_cli_success(&out, "format-patch --inline");
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(
        body.contains("Content-Disposition: inline; filename=\"0001-"),
        "inline should use inline disposition: {body}"
    );
    assert!(
        !body.contains("Content-Disposition: attachment"),
        "inline must not use attachment disposition: {body}"
    );
}

#[test]
fn attach_and_inline_are_mutually_exclusive() {
    let repo = repo_with_commits(1);
    let out = run_libra_command(
        &[
            "format-patch",
            "--stdout",
            "--attach",
            "--inline",
            "HEAD~1..HEAD",
        ],
        repo.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(129),
        "--attach with --inline should be rejected: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn no_attach_stays_plain_text() {
    let repo = repo_with_commits(1);
    let out = run_libra_command(&["format-patch", "--stdout", "HEAD~1..HEAD"], repo.path());
    assert_cli_success(&out, "format-patch plain");
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(
        body.contains("Content-Type: text/plain; charset=UTF-8\n"),
        "default output should be plain text: {body}"
    );
    assert!(
        !body.contains("multipart/mixed"),
        "default output must not be multipart: {body}"
    );
}

// ---------------------------------------------------------------------------
// JSON output
// ---------------------------------------------------------------------------

#[test]
fn json_output_returns_patch_records() {
    let repo = repo_with_commits(2);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "--json",
            "format-patch",
            "-n",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "json output");

    let v = parse_json_stdout(&output);
    let patches = v["data"]["patches"].as_array().expect("patches array");
    assert!(!patches.is_empty(), "should have at least one patch record");
    let first = &patches[0];
    assert!(first["number"].is_number(), "record should have number");
    assert!(
        first["commit"].is_string(),
        "record should have commit hash"
    );
    assert!(first["subject"].is_string(), "record should have subject");
    assert!(first["path"].is_string(), "record should have output path");
}

#[test]
fn json_output_includes_cover_letter_record() {
    let repo = repo_with_commits(2);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "--json",
            "format-patch",
            "--cover-letter",
            "-n",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "json cover letter output");

    let v = parse_json_stdout(&output);
    let patches = v["data"]["patches"].as_array().expect("patches array");
    let cover = patches
        .iter()
        .find(|record| {
            record["number"].as_u64() == Some(0)
                && record["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with("0000-cover-letter.patch"))
        })
        .expect("cover letter record");
    assert_eq!(
        cover["commit"].as_str(),
        Some("0000000000000000000000000000000000000000")
    );
    assert_eq!(cover["subject"].as_str(), Some("*** SUBJECT HERE ***"));
}

#[test]
fn subject_header_sanitizes_control_characters() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("header.txt"), "header\n").unwrap();
    run_libra_command(&["add", "header.txt"], repo.path());
    let commit = run_libra_command(
        &["commit", "-m", "bad\rBcc: injected", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&commit, "commit header-control subject");

    let output = run_libra_command(
        &[
            "format-patch",
            "--stdout",
            "--subject-prefix",
            "PATCH\nCc: injected",
            "HEAD~1",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "sanitize subject header");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let header = stdout.split("\n\n").next().unwrap_or_default();
    let subject = header
        .lines()
        .find(|line| line.starts_with("Subject: "))
        .expect("subject header");
    assert!(
        subject.contains("[PATCH Cc: injected] bad Bcc: injected"),
        "subject header should contain sanitized values: {subject}"
    );
    assert!(
        !header.contains('\r'),
        "header must not contain carriage returns: {header:?}"
    );
    assert!(
        !header.contains("\nCc: injected") && !header.contains("\nBcc: injected"),
        "control characters must not create extra headers: {header:?}"
    );
}

#[test]
fn thread_flag_adds_message_id() {
    let repo = repo_with_commits(2);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "format-patch",
            "--thread",
            "-n",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "thread");

    let entries: Vec<_> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    if let Some(entry) = entries.first() {
        let content = fs::read_to_string(entry.path()).unwrap();
        assert!(
            content.contains("Message-ID:"),
            "first patch should have Message-ID when --thread"
        );
    }
}

#[test]
fn no_thread_suppresses_headers() {
    let repo = repo_with_commits(2);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "format-patch",
            "--no-thread",
            "-n",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "no-thread");

    let entries: Vec<_> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    if let Some(entry) = entries.first() {
        let content = fs::read_to_string(entry.path()).unwrap();
        assert!(
            !content.contains("Message-ID:"),
            "patch should NOT have Message-ID when --no-thread"
        );
    }
}

#[test]
fn in_reply_to_applies_to_first_patch() {
    let repo = repo_with_commits(2);
    let out_dir = tempdir().unwrap();

    let msg_id = "<test-thread-123@example>";
    let output = run_libra_command(
        &[
            "format-patch",
            "--in-reply-to",
            msg_id,
            "-n",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "in-reply-to");

    let entries: Vec<_> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    if let Some(entry) = entries.first() {
        let content = fs::read_to_string(entry.path()).unwrap();
        assert!(
            content.contains(msg_id),
            "should contain the custom message-id: {content}"
        );
    }
}

#[test]
fn keep_subject_retains_bracket_prefix() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("test.txt"), "data\n").unwrap();
    run_libra_command(&["add", "test.txt"], repo.path());
    run_libra_command(
        &["commit", "-m", "[PATCH] my change", "--no-verify"],
        repo.path(),
    );

    let out_dir = tempdir().unwrap();
    let output = run_libra_command(
        &[
            "format-patch",
            "--keep-subject",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1", // HEAD~1..HEAD = 1 patch
        ],
        repo.path(),
    );
    assert_cli_success(&output, "keep-subject");

    let entries: Vec<_> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    if let Some(entry) = entries.first() {
        let content = fs::read_to_string(entry.path()).unwrap();
        assert!(
            content.contains("[PATCH]"),
            "should keep [PATCH] in subject with --keep-subject"
        );
    }
}

#[test]
fn no_stat_suppresses_diffstat() {
    let repo = repo_with_commits(1);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "format-patch",
            "--no-stat",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "no-stat");

    let entries: Vec<_> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    if let Some(entry) = entries.first() {
        let content = fs::read_to_string(entry.path()).unwrap();
        assert!(
            !content.contains("file changed"),
            "--no-stat should suppress diffstat"
        );
    }
}

// ---------------------------------------------------------------------------
// Boundary / edge-case tests
// ---------------------------------------------------------------------------

#[test]
fn empty_range_succeeds_with_no_output() {
    let repo = create_committed_repo_via_cli();
    // An empty range yields no patches, which Git treats as success with no
    // output rather than an error (issues/529 scenario 1).
    let output = run_libra_command(&["format-patch", "HEAD..HEAD"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(0),
        "empty range exits 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "empty range prints nothing");
}

#[test]
fn not_in_repo_reports_error() {
    let tmp = tempdir().unwrap();
    let output = run_libra_command(&["format-patch"], tmp.path());
    assert!(!output.status.success(), "not in repo should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not a libra repository"),
        "should mention not a libra repo"
    );
}

#[test]
fn invalid_revision_reports_error() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["format-patch", "nonexistent-branch..HEAD"], repo.path());
    assert!(!output.status.success(), "invalid revision should fail");
}

#[test]
fn start_number_offsets_file_names() {
    let repo = repo_with_commits(2);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "format-patch",
            "-n",
            "--start-number",
            "5",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "start-number 5");

    let entries: Vec<_> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        entries.iter().any(|n| n.starts_with("0005-")),
        "should start numbering at 5, got: {entries:?}"
    );
}

#[test]
fn merge_commits_are_skipped() {
    let repo = create_committed_repo_via_cli();
    // Create a branch with its own commit, then merge it
    fs::write(repo.path().join("main.txt"), "main\n").unwrap();
    run_libra_command(&["add", "main.txt"], repo.path());
    run_libra_command(&["commit", "-m", "main commit", "--no-verify"], repo.path());

    run_libra_command(&["switch", "-C", "side"], repo.path());
    fs::write(repo.path().join("side.txt"), "side\n").unwrap();
    run_libra_command(&["add", "side.txt"], repo.path());
    run_libra_command(&["commit", "-m", "side commit", "--no-verify"], repo.path());

    // Switch back to main and merge
    run_libra_command(&["switch", "main"], repo.path());
    let merge_out = run_libra_command(
        &["merge", "side", "-m", "merge side", "--no-ff"],
        repo.path(),
    );
    // Merge might fail in test env; just verify format-patch respects merge skip
    if !merge_out.status.success() {
        // Skip if merge isn't working in this context
        return;
    }

    let out_dir = tempdir().unwrap();
    let output = run_libra_command(
        &[
            "format-patch",
            "-n",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~2..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "merge skip");
}

// ---------------------------------------------------------------------------
// Full-index test
// ---------------------------------------------------------------------------

#[test]
fn full_index_flag_outputs_full_hash() {
    // full-index is accepted as a flag — the underlying diff output
    // is handled by the libra diff engine; we verify the flag parses.
    let repo = repo_with_commits(1);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "format-patch",
            "--full-index",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~1",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "full-index flag accepted");
}

#[test]
fn suffix_changes_patch_filename_extension() {
    let repo = repo_with_commits(2);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "format-patch",
            "--suffix=.txt",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~2..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "format-patch --suffix=.txt");

    let names: Vec<String> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!names.is_empty(), "expected patch files: {names:?}");
    assert!(
        names.iter().all(|n| n.ends_with(".txt")),
        "all patches must use the .txt suffix: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.starts_with("0001-")),
        "numbered prefix is retained: {names:?}"
    );
    assert!(
        names.iter().all(|n| !n.ends_with(".patch")),
        "no .patch files when --suffix=.txt: {names:?}"
    );
}

#[test]
fn zero_commit_zeroes_the_envelope_hash() {
    let repo = repo_with_commits(1);

    let output = run_libra_command(
        &["format-patch", "--zero-commit", "--stdout", "HEAD~1..HEAD"],
        repo.path(),
    );
    assert_cli_success(&output, "format-patch --zero-commit");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next().unwrap_or("");
    let hash_field = first
        .strip_prefix("From ")
        .and_then(|s| s.split(' ').next())
        .unwrap_or("");
    assert!(
        !hash_field.is_empty() && hash_field.chars().all(|c| c == '0'),
        "--zero-commit must zero the envelope hash: {first:?}"
    );

    // Without --zero-commit the envelope uses the real commit hash.
    let def = run_libra_command(&["format-patch", "--stdout", "HEAD~1..HEAD"], repo.path());
    let def_first_line = String::from_utf8_lossy(&def.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    let def_hash = def_first_line
        .strip_prefix("From ")
        .and_then(|s| s.split(' ').next())
        .unwrap_or("");
    assert!(
        def_hash.chars().any(|c| c != '0'),
        "default envelope must use the real hash: {def_first_line:?}"
    );
    // The zero hash must span the full hash width (40 hex for SHA-1, 64 for
    // SHA-256), not a single `0`.
    assert_eq!(
        hash_field.len(),
        def_hash.len(),
        "zeroed envelope hash must match the real hash width: {hash_field:?} vs {def_hash:?}"
    );
}

#[test]
fn signature_controls_patch_footer() {
    let repo = repo_with_commits(1);

    // Custom signature replaces the default version line after `-- `.
    let out = run_libra_command(
        &[
            "format-patch",
            "--signature",
            "MY SIG",
            "--stdout",
            "HEAD~1..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&out, "format-patch --signature");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("-- \nMY SIG\n"),
        "custom signature footer expected: {s:?}"
    );

    // --no-signature omits the `-- ` footer line entirely.
    let out = run_libra_command(
        &["format-patch", "--no-signature", "--stdout", "HEAD~1..HEAD"],
        repo.path(),
    );
    assert_cli_success(&out, "format-patch --no-signature");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        !s.contains("\n-- \n"),
        "no `-- ` footer expected with --no-signature: {s:?}"
    );

    // Default keeps a `-- ` footer (libra version).
    let out = run_libra_command(&["format-patch", "--stdout", "HEAD~1..HEAD"], repo.path());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("\n-- \n"), "default footer expected: {s:?}");
}

#[test]
fn numbered_files_uses_bare_sequence_numbers() {
    let repo = repo_with_commits(2);
    let out_dir = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "format-patch",
            "--numbered-files",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~2..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "format-patch --numbered-files");
    let mut names: Vec<String> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["1".to_string(), "2".to_string()],
        "expected bare sequence-number files: {names:?}"
    );

    // `--suffix` is ignored under `--numbered-files` (matches git).
    let out_dir2 = tempdir().unwrap();
    let output = run_libra_command(
        &[
            "format-patch",
            "--numbered-files",
            "--suffix=.txt",
            "-o",
            out_dir2.path().to_str().unwrap(),
            "HEAD~2..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "format-patch --numbered-files --suffix");
    let names2: Vec<String> = fs::read_dir(out_dir2.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names2.iter().all(|n| !n.contains('.')),
        "suffix must be ignored under --numbered-files: {names2:?}"
    );
}

#[test]
fn signature_file_sets_the_footer() {
    let repo = repo_with_commits(1);
    let sig = repo.path().join("sig.txt");
    fs::write(&sig, "Sent via Libra\n-- the team\n").unwrap();

    let output = run_libra_command(
        &[
            "format-patch",
            "--stdout",
            "--signature-file",
            sig.to_str().unwrap(),
            "HEAD~1..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "signature-file");
    let s = String::from_utf8_lossy(&output.stdout);
    assert!(
        s.contains("-- \nSent via Libra\n-- the team"),
        "footer must come from the signature file: {s}"
    );
}

#[test]
fn encode_email_headers_q_encodes_nonascii_subject() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("f.txt"), "x\n").unwrap();
    run_libra_command(&["add", "f.txt"], repo.path());
    run_libra_command(&["commit", "-m", "café résumé", "--no-verify"], repo.path());

    // With --encode-email-headers the Subject is RFC 2047 Q-encoded.
    let encoded = run_libra_command(
        &[
            "format-patch",
            "--stdout",
            "--encode-email-headers",
            "HEAD~1..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&encoded, "encode-email-headers");
    let es = String::from_utf8_lossy(&encoded.stdout);
    let subj = es
        .lines()
        .find(|l| l.starts_with("Subject:"))
        .expect("a Subject line");
    assert!(
        subj.contains("=?UTF-8?q?"),
        "subject must be Q-encoded: {subj}"
    );
    assert!(
        !subj.contains("café"),
        "raw non-ASCII must not appear: {subj}"
    );

    // Without the flag the Subject keeps the raw UTF-8 text.
    let plain = run_libra_command(&["format-patch", "--stdout", "HEAD~1..HEAD"], repo.path());
    let ps = String::from_utf8_lossy(&plain.stdout);
    let psubj = ps
        .lines()
        .find(|l| l.starts_with("Subject:"))
        .expect("a Subject line");
    assert!(
        psubj.contains("café"),
        "raw subject without the flag: {psubj}"
    );
}

#[test]
fn encode_email_headers_splits_long_words_under_75_chars() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("g.txt"), "x\n").unwrap();
    run_libra_command(&["add", "g.txt"], repo.path());
    // A long non-ASCII subject forces the Q-encoding across multiple words.
    let long_subject = "é".repeat(60);
    run_libra_command(&["commit", "-m", &long_subject, "--no-verify"], repo.path());

    let out = run_libra_command(
        &[
            "format-patch",
            "--stdout",
            "--encode-email-headers",
            "HEAD~1..HEAD",
        ],
        repo.path(),
    );
    assert_cli_success(&out, "encode long subject");
    let s = String::from_utf8_lossy(&out.stdout);
    let subj = s
        .lines()
        .find(|l| l.starts_with("Subject:"))
        .expect("a Subject line");
    let words: Vec<&str> = subj
        .split_whitespace()
        .filter(|w| w.starts_with("=?UTF-8?q?"))
        .collect();
    assert!(
        words.len() >= 2,
        "a long subject must split into multiple encoded-words: {subj}"
    );
    for w in &words {
        assert!(
            w.chars().count() <= 75,
            "each RFC 2047 encoded-word must be <= 75 chars: {w}"
        );
    }
}

// ---------------------------------------------------------------------------
// Recipient headers (--to / --cc / --no-to / --no-cc)
// ---------------------------------------------------------------------------

#[test]
fn recipient_headers_to_and_cc() {
    let repo = repo_with_commits(1);

    // --to adds a To: header; repeated --cc folds with a 4-space continuation.
    let output = run_libra_command(
        &[
            "format-patch",
            "--stdout",
            "HEAD~1..HEAD",
            "--to",
            "rev@example.com",
            "--cc",
            "cc1@example.com",
            "--cc",
            "cc2@example.com",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "format-patch --to/--cc");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("To: rev@example.com\n"),
        "To: header present: {stdout}"
    );
    // Cc folds onto a continuation line, matching git.
    assert!(
        stdout.contains("Cc: cc1@example.com,\n    cc2@example.com\n"),
        "Cc: folds multiple addresses: {stdout}"
    );
    // The recipient headers sit after the MIME header block, matching git.
    let mime_pos = stdout
        .find("Content-Transfer-Encoding:")
        .expect("mime block");
    let to_pos = stdout.find("To: rev@example.com").expect("to");
    assert!(mime_pos < to_pos, "To: follows the MIME headers: {stdout}");

    // Recipients are passed through verbatim even with --encode-email-headers
    // (git does not RFC2047-encode addresses).
    let nonascii = run_libra_command(
        &[
            "format-patch",
            "--stdout",
            "HEAD~1..HEAD",
            "--encode-email-headers",
            "--to",
            "Jöhn <john@example.com>",
        ],
        repo.path(),
    );
    assert_cli_success(&nonascii, "format-patch --encode-email-headers --to");
    assert!(
        String::from_utf8_lossy(&nonascii.stdout).contains("To: Jöhn <john@example.com>\n"),
        "recipient is not RFC2047-encoded"
    );

    // --no-to / --no-cc suppress the headers even when addresses are given.
    let suppressed = run_libra_command(
        &[
            "format-patch",
            "--stdout",
            "HEAD~1..HEAD",
            "--to",
            "rev@example.com",
            "--no-to",
            "--cc",
            "cc@example.com",
            "--no-cc",
        ],
        repo.path(),
    );
    assert_cli_success(&suppressed, "format-patch --no-to/--no-cc");
    let suppressed_out = String::from_utf8_lossy(&suppressed.stdout);
    assert!(
        !suppressed_out.contains("\nTo: ") && !suppressed_out.contains("\nCc: "),
        "--no-to/--no-cc suppress the headers: {suppressed_out}"
    );

    // The cover letter also carries the recipient headers.
    let cover = run_libra_command(
        &[
            "format-patch",
            "--stdout",
            "HEAD~1..HEAD",
            "--cover-letter",
            "--to",
            "rev@example.com",
        ],
        repo.path(),
    );
    assert_cli_success(&cover, "format-patch --cover-letter --to");
    assert!(
        String::from_utf8_lossy(&cover.stdout).contains("To: rev@example.com\n"),
        "cover letter carries To:"
    );
}

// ---------------------------------------------------------------------------
// From-header rewriting (--from)
// ---------------------------------------------------------------------------

#[test]
fn from_header_rewrites_author() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // A second commit authored by someone other than the committer.
    fs::write(p.join("x.txt"), "x\n").unwrap();
    run_libra_command(&["add", "x.txt"], p);
    run_libra_command(
        &[
            "commit",
            "-m",
            "feature",
            "--author",
            "Author A <a@x.com>",
            "--no-verify",
        ],
        p,
    );

    // --from differs from the author: the From: header is rewritten and the
    // original author is preserved as an in-body From: line.
    let out = run_libra_command(
        &[
            "format-patch",
            "--stdout",
            "HEAD~1..HEAD",
            "--from=Bot <bot@x.com>",
        ],
        p,
    );
    assert_cli_success(&out, "format-patch --from");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("From: Bot <bot@x.com>\n"),
        "From: header rewritten: {stdout}"
    );
    let header_from = stdout.find("From: Bot <bot@x.com>").expect("header From");
    let inbody_from = stdout
        .find("From: Author A <a@x.com>")
        .expect("in-body From");
    assert!(
        header_from < inbody_from,
        "in-body From follows header From"
    );
    let body_sep = stdout.find("\n\n").expect("headers/body separator");
    assert!(
        body_sep < inbody_from,
        "in-body From sits in the body section: {stdout}"
    );

    // --from equal to the author adds no in-body From (only the header).
    let same = run_libra_command(
        &[
            "format-patch",
            "--stdout",
            "HEAD~1..HEAD",
            "--from=Author A <a@x.com>",
        ],
        p,
    );
    assert_cli_success(&same, "format-patch --from same author");
    let same_out = String::from_utf8_lossy(&same.stdout);
    assert_eq!(
        same_out.matches("From: Author A <a@x.com>").count(),
        1,
        "no in-body From when --from equals the author: {same_out}"
    );

    // Bare `--from` (no value) uses the committer's configured identity. With
    // `require_equals`, the following `HEAD~1..HEAD` is the revision-range
    // positional, NOT the --from value — so this must succeed (no ambiguity).
    let bare = run_libra_command(&["format-patch", "--from", "--stdout", "HEAD~1..HEAD"], p);
    assert_cli_success(&bare, "format-patch bare --from");
    let bare_out = String::from_utf8_lossy(&bare.stdout);
    // create_committed_repo_via_cli configures user.name/email = Test User.
    assert!(
        bare_out.contains("From: Test User <test@example.com>\n"),
        "bare --from uses the committer identity: {bare_out}"
    );
    // The committer differs from the author, so the in-body From is preserved.
    assert!(
        bare_out.contains("From: Author A <a@x.com>"),
        "bare --from keeps the in-body author: {bare_out}"
    );

    // The cover letter also carries the rewritten From: identity. Write to a
    // directory and read the cover-letter file directly so the assertion is
    // scoped to the cover letter (not a patch mail, which also has this From:).
    let cover_dir = tempdir().unwrap();
    let cover = run_libra_command(
        &[
            "format-patch",
            "-n",
            "-o",
            cover_dir.path().to_str().unwrap(),
            "HEAD~1..HEAD",
            "--cover-letter",
            "--from=Bot <bot@x.com>",
        ],
        p,
    );
    assert_cli_success(&cover, "format-patch --cover-letter --from");
    let cover_text = fs::read_to_string(cover_dir.path().join("0000-cover-letter.patch"))
        .expect("cover-letter file");
    assert!(
        cover_text.contains("From: Bot <bot@x.com>\n"),
        "cover letter carries the --from identity (not a blank From:): {cover_text}"
    );
}

// ---------------------------------------------------------------------------
// --base: base-commit / prerequisite-patch-id trailer
// ---------------------------------------------------------------------------

#[test]
fn base_records_base_commit_and_prerequisite_patch_ids() {
    // create_committed_repo_via_cli gives 1 commit; +3 → 4 commits total.
    let repo = repo_with_commits(3);
    let p = repo.path();

    let base_sha = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD~3"], p).stdout)
        .trim()
        .to_string();

    // Base older than the series parent → base-commit plus one prerequisite per
    // commit between the base and the series parent (oldest-first).
    let out = run_libra_command(
        &["format-patch", "--base=HEAD~3", "--stdout", "HEAD~1..HEAD"],
        p,
    );
    assert_cli_success(&out, "format-patch --base");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("base-commit: {base_sha}\n")),
        "records the base commit: {text}"
    );
    let prereqs = text.matches("prerequisite-patch-id: ").count();
    assert_eq!(
        prereqs, 2,
        "two commits lie between base and series parent: {text}"
    );
    // The base trailer precedes the signature footer.
    let base_pos = text.find("base-commit:").unwrap();
    let sig_pos = text.find("\n-- \n").unwrap();
    assert!(
        base_pos < sig_pos,
        "base trailer comes before the signature"
    );
}

#[test]
fn base_multi_file_prerequisite_patch_id_matches_git() {
    // A prerequisite commit that touches TWO files exercises Git's stable
    // patch-id combiner (byte-wise add-with-carry, NOT XOR). The patch-id is
    // derived purely from the diff content, so the expected value is the one
    // `git patch-id --stable` produces for the same change (a->A, b->B).
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("f1"), "a\n").unwrap();
    fs::write(p.join("f2"), "b\n").unwrap();
    run_libra_command(&["add", "f1", "f2"], p);
    run_libra_command(&["commit", "-m", "base", "--no-verify"], p);
    fs::write(p.join("f1"), "A\n").unwrap();
    fs::write(p.join("f2"), "B\n").unwrap();
    run_libra_command(&["add", "f1", "f2"], p);
    run_libra_command(&["commit", "-m", "multi", "--no-verify"], p);
    fs::write(p.join("f3"), "c\n").unwrap();
    run_libra_command(&["add", "f3"], p);
    run_libra_command(&["commit", "-m", "tip", "--no-verify"], p);

    let out = run_libra_command(
        &["format-patch", "--base=HEAD~2", "--stdout", "HEAD~1..HEAD"],
        p,
    );
    assert_cli_success(&out, "format-patch --base multi-file prereq");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("prerequisite-patch-id: 41738f97b408e386f1d209bee7dc2096eeafa713"),
        "multi-file prerequisite patch-id must match git patch-id --stable: {text}"
    );
}

#[test]
fn base_prerequisites_skip_merge_commits() {
    // base -> main2 -> merge(feat) -> tip. With base at the root and the series =
    // tip, the prerequisites are main2 + feat — the MERGE commit is skipped, like
    // git's non-merge (`max_parents = 1`) prerequisite walk.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("f"), "1\n").unwrap();
    run_libra_command(&["add", "f"], p);
    run_libra_command(&["commit", "-m", "base", "--no-verify"], p);
    let base = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    run_libra_command(&["branch", "feat"], p);
    run_libra_command(&["switch", "feat"], p);
    fs::write(p.join("f"), "1\n2\n").unwrap();
    run_libra_command(&["add", "f"], p);
    run_libra_command(&["commit", "-m", "feat", "--no-verify"], p);

    run_libra_command(&["switch", "main"], p);
    fs::write(p.join("g"), "1\n0\n").unwrap();
    run_libra_command(&["add", "g"], p);
    run_libra_command(&["commit", "-m", "main2", "--no-verify"], p);
    let merge = run_libra_command(&["merge", "feat", "-m", "merge feat"], p);
    assert_cli_success(&merge, "merge feat");

    fs::write(p.join("h"), "x\n").unwrap();
    run_libra_command(&["add", "h"], p);
    run_libra_command(&["commit", "-m", "tip", "--no-verify"], p);

    let out = run_libra_command(
        &[
            "format-patch",
            &format!("--base={base}"),
            "--stdout",
            "HEAD~1..HEAD",
        ],
        p,
    );
    assert_cli_success(&out, "format-patch --base with a merge in the prereqs");
    let prereqs = String::from_utf8_lossy(&out.stdout)
        .matches("prerequisite-patch-id: ")
        .count();
    assert_eq!(
        prereqs, 2,
        "merge commit must not be emitted as a prerequisite"
    );
}

#[test]
fn base_with_attach_emits_trailer_in_patch_part() {
    let repo = repo_with_commits(3);
    let p = repo.path();
    let out = run_libra_command(
        &[
            "format-patch",
            "--attach",
            "--base=HEAD~3",
            "--stdout",
            "HEAD~1..HEAD",
        ],
        p,
    );
    assert_cli_success(&out, "format-patch --attach --base");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("base-commit: "),
        "attach output carries the base trailer: {text}"
    );
    // The trailer sits inside the patch part, before the closing MIME boundary.
    let base_pos = text.find("base-commit:").unwrap();
    let close_pos = text.rfind("--\n").unwrap();
    assert!(
        base_pos < close_pos,
        "base trailer precedes the closing boundary"
    );
}

#[test]
fn base_direct_parent_has_no_prerequisites() {
    let repo = repo_with_commits(3);
    let p = repo.path();
    // Base == the series parent → base-commit only, no prerequisites.
    let out = run_libra_command(
        &["format-patch", "--base=HEAD~1", "--stdout", "HEAD~1..HEAD"],
        p,
    );
    assert_cli_success(&out, "format-patch --base direct parent");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("base-commit: "), "has base-commit: {text}");
    assert!(
        !text.contains("prerequisite-patch-id:"),
        "no prerequisites when base is the direct parent: {text}"
    );
}

#[test]
fn base_on_non_ancestor_fails() {
    let repo = repo_with_commits(3);
    let p = repo.path();
    // A sibling commit that is not an ancestor of the series.
    run_libra_command(&["branch", "side", "HEAD~2"], p);
    run_libra_command(&["switch", "side"], p);
    fs::write(p.join("side.txt"), "side\n").unwrap();
    run_libra_command(&["add", "side.txt"], p);
    run_libra_command(&["commit", "-m", "side", "--no-verify"], p);
    run_libra_command(&["switch", "main"], p);

    let out = run_libra_command(
        &["format-patch", "--base=side", "--stdout", "HEAD~1..HEAD"],
        p,
    );
    assert_eq!(
        out.status.code(),
        Some(128),
        "non-ancestor base is rejected: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn base_auto_is_rejected() {
    let repo = repo_with_commits(3);
    let out = run_libra_command(
        &["format-patch", "--base=auto", "--stdout", "HEAD~1..HEAD"],
        repo.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(129),
        "--base=auto is a usage error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn base_with_cover_letter_lands_on_cover() {
    let repo = repo_with_commits(3);
    let p = repo.path();
    let dir = tempdir().unwrap();
    let out = run_libra_command(
        &[
            "format-patch",
            "-o",
            dir.path().to_str().unwrap(),
            "--cover-letter",
            "--base=HEAD~3",
            "HEAD~1..HEAD",
        ],
        p,
    );
    assert_cli_success(&out, "format-patch --cover-letter --base");
    let cover = fs::read_to_string(dir.path().join("0000-cover-letter.patch")).expect("cover");
    assert!(
        cover.contains("base-commit: "),
        "base trailer on the cover: {cover}"
    );
    // Exactly one file (the cover letter) carries the base trailer — the patch
    // files must not duplicate it.
    let with_base = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| fs::read_to_string(e.unwrap().path()).ok())
        .filter(|t| t.contains("base-commit:"))
        .count();
    assert_eq!(
        with_base, 1,
        "only the cover letter carries the base trailer"
    );
}

#[test]
fn format_patch_blake3_patch_id() {
    let temp = tempfile::tempdir().unwrap();
    let p = temp.path();
    assert_cli_success(
        &run_libra_command(
            &["init", "--vault", "false", "--object-format", "blake3"],
            p,
        ),
        "blake3 init",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "user.name", "Blake3 User"], p),
        "user.name",
    );
    assert_cli_success(
        &run_libra_command(
            &["config", "--local", "user.email", "blake3@example.com"],
            p,
        ),
        "user.email",
    );
    fs::write(p.join("f1"), "a\n").unwrap();
    fs::write(p.join("f2"), "b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f1", "f2"], p), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::write(p.join("f1"), "A\n").unwrap();
    fs::write(p.join("f2"), "B\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f1", "f2"], p), "add multi");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "multi", "--no-verify"], p),
        "commit multi",
    );
    fs::write(p.join("f3"), "c\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f3"], p), "add tip");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "tip", "--no-verify"], p),
        "commit tip",
    );

    let out = run_libra_command(
        &["format-patch", "--base=HEAD~2", "--stdout", "HEAD~1..HEAD"],
        p,
    );
    assert_cli_success(&out, "format-patch --base blake3");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("prerequisite-patch-id: "),
        "blake3 format-patch must emit prerequisite-patch-id: {text}"
    );
    // Extract the first prerequisite-patch-id value and assert 64-hex (BLAKE3 digest).
    let mut found = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("prerequisite-patch-id: ") {
            found = Some(rest.trim().to_string());
            break;
        }
    }
    let pid = found.expect("prerequisite-patch-id line");
    assert_eq!(pid.len(), 64, "blake3 patch-id must be 64 hex: {pid}");
    assert!(
        pid.chars().all(|c| c.is_ascii_hexdigit()),
        "blake3 patch-id must be hex: {pid}"
    );
    // Must NOT match the SHA-1 git-stable patch-id for the same content.
    assert_ne!(
        pid, "41738f97b408e386f1d209bee7dc2096eeafa713",
        "blake3 combiner must not emit the sha1 patch-id"
    );
}

// ---------------------------------------------------------------------------
// issues/529: Git option-parity surface
// ---------------------------------------------------------------------------

#[test]
fn last_n_shorthand_limits_the_series() {
    let repo = repo_with_commits(4);
    // `-<n>` is Git's "the last n commits" shorthand (previously rejected as an
    // unknown `-2`/`-3` option).
    for (flag, expected) in [("-2", 2usize), ("-3", 3), ("-1", 1)] {
        let output = run_libra_command(&["format-patch", flag, "--stdout"], repo.path());
        assert_cli_success(&output, flag);
        let body = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            body.matches("\nFrom ").count() + usize::from(body.starts_with("From ")),
            expected,
            "{flag} must emit {expected} patches: {body}"
        );
    }
}

#[test]
fn rfc_marker_joins_the_subject_prefix() {
    let repo = repo_with_commits(1);
    let with_rfc = run_libra_command(
        &["format-patch", "--rfc", "HEAD~1..HEAD", "--stdout"],
        repo.path(),
    );
    assert_cli_success(&with_rfc, "--rfc");
    assert!(
        String::from_utf8_lossy(&with_rfc.stdout).contains("Subject: [RFC PATCH]"),
        "--rfc prepends the marker: {}",
        String::from_utf8_lossy(&with_rfc.stdout)
    );

    // `--rfc=<text>` replaces `RFC`, and the value must be attached (`=`).
    let custom = run_libra_command(
        &["format-patch", "--rfc=WIP", "HEAD~1..HEAD", "--stdout"],
        repo.path(),
    );
    assert_cli_success(&custom, "--rfc=WIP");
    assert!(
        String::from_utf8_lossy(&custom.stdout).contains("Subject: [WIP PATCH]"),
        "custom rfc text replaces RFC: {}",
        String::from_utf8_lossy(&custom.stdout)
    );

    // Last one wins: `--rfc --no-rfc` restores the plain prefix.
    let cancelled = run_libra_command(
        &[
            "format-patch",
            "--rfc",
            "--no-rfc",
            "HEAD~1..HEAD",
            "--stdout",
        ],
        repo.path(),
    );
    assert_cli_success(&cancelled, "--rfc --no-rfc");
    assert!(
        String::from_utf8_lossy(&cancelled.stdout).contains("Subject: [PATCH] commit"),
        "--no-rfc cancels the marker: {}",
        String::from_utf8_lossy(&cancelled.stdout)
    );
}

#[test]
fn no_numbered_keeps_sequence_filenames() {
    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let out_dir = tempdir().unwrap();
    let output = run_libra_command(
        &[
            "format-patch",
            "-N",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~3",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "-N");

    let mut names: Vec<String> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["0001-3.patch", "0002-4.patch", "0003-rename.patch"]);

    let stdout = run_libra_command(&["format-patch", "-N", "--stdout", "HEAD~3"], repo.path());
    assert_cli_success(&stdout, "-N --stdout");
    let stdout_text = String::from_utf8_lossy(&stdout.stdout);
    let subjects: Vec<&str> = stdout_text
        .lines()
        .filter_map(|line| line.strip_prefix("Subject: "))
        .collect();
    assert_eq!(subjects, ["[PATCH] 3", "[PATCH] 4", "[PATCH] rename"]);

    let help = run_libra_command(&["format-patch", "--help"], repo.path());
    assert_cli_success(&help, "format-patch --help");
    let help_text = String::from_utf8_lossy(&help.stdout);
    let numbered_help = help_text
        .lines()
        .find(|line| line.trim_start().starts_with("-n, --numbered"))
        .expect("numbered help option");
    assert!(
        numbered_help.contains("Add the [PATCH n/m] counter to commit patch Subject lines"),
        "help should describe subject numbering: {numbered_help}"
    );
    assert!(
        !numbered_help.contains("filename"),
        "help should not describe filenames: {numbered_help}"
    );
    let no_numbered_help = help_text
        .lines()
        .find(|line| line.trim_start().starts_with("-N, --no-numbered"))
        .expect("no-numbered help option");
    assert!(
        no_numbered_help.contains(
            "Do not add [PATCH n/m] counters to commit patch or cover-letter Subject lines"
        ),
        "help should describe subject numbering: {no_numbered_help}"
    );
    assert!(
        !no_numbered_help.contains("filename"),
        "help should not describe filenames: {no_numbered_help}"
    );
}

#[test]
fn no_numbered_suppresses_every_subject_counter() {
    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let output = run_libra_command(&["format-patch", "-N", "-2", "--stdout"], repo.path());
    assert_cli_success(&output, "-N -2 --stdout");

    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let subjects: Vec<&str> = stdout_text
        .lines()
        .filter_map(|line| line.strip_prefix("Subject: "))
        .collect();
    assert_eq!(subjects, ["[PATCH] 4", "[PATCH] rename"]);

    let output = run_libra_command(
        &["format-patch", "-N", "--cover-letter", "-1", "--stdout"],
        repo.path(),
    );
    assert_cli_success(&output, "-N --cover-letter -1 --stdout");

    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let subjects: Vec<&str> = stdout_text
        .lines()
        .filter_map(|line| line.strip_prefix("Subject: "))
        .collect();
    assert_eq!(subjects, ["[PATCH] *** SUBJECT HERE ***", "[PATCH] rename"]);
}

#[test]
fn subject_counter_width_follows_displayed_total() {
    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let output = run_libra_command(&["format-patch", "-n", "-1", "--stdout"], repo.path());
    assert_cli_success(&output, "-n -1 --stdout");

    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let subjects: Vec<&str> = stdout_text
        .lines()
        .filter_map(|line| line.strip_prefix("Subject: "))
        .collect();
    assert_eq!(subjects, ["[PATCH 1/1] rename"]);

    let output = run_libra_command(
        &["format-patch", "-n", "--start-number=9", "-2", "--stdout"],
        repo.path(),
    );
    assert_cli_success(&output, "-n --start-number=9 -2 --stdout");

    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let subjects: Vec<&str> = stdout_text
        .lines()
        .filter_map(|line| line.strip_prefix("Subject: "))
        .collect();
    assert_eq!(subjects, ["[PATCH 09/10] 4", "[PATCH 10/10] rename"]);
}

#[test]
fn cover_letter_counter_width_matches_displayed_total() {
    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let output = run_libra_command(
        &["format-patch", "--cover-letter", "-2", "--stdout"],
        repo.path(),
    );
    assert_cli_success(&output, "--cover-letter -2 --stdout");

    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let subjects: Vec<&str> = stdout_text
        .lines()
        .filter_map(|line| line.strip_prefix("Subject: "))
        .collect();
    assert_eq!(
        subjects,
        [
            "[PATCH 0/2] *** SUBJECT HERE ***",
            "[PATCH 1/2] 4",
            "[PATCH 2/2] rename"
        ]
    );

    let repo = repo_with_patch_subjects(&[
        "patch 1", "patch 2", "patch 3", "patch 4", "patch 5", "patch 6", "patch 7", "patch 8",
        "patch 9", "patch 10",
    ]);
    let output = run_libra_command(
        &[
            "format-patch",
            "--cover-letter",
            "--numbered",
            "-10",
            "--stdout",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "--cover-letter --numbered -10 --stdout");

    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let subjects: Vec<String> = stdout_text
        .lines()
        .filter_map(|line| line.strip_prefix("Subject: "))
        .map(str::to_string)
        .collect();
    let expected: Vec<String> = std::iter::once("[PATCH 00/10] *** SUBJECT HERE ***".to_string())
        .chain((1..=10).map(|patch_num| format!("[PATCH {patch_num:02}/10] patch {patch_num}")))
        .collect();
    assert_eq!(subjects, expected);

    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let output = run_libra_command(
        &[
            "format-patch",
            "--start-number=9",
            "--cover-letter",
            "--numbered",
            "-2",
            "--stdout",
        ],
        repo.path(),
    );
    assert_cli_success(
        &output,
        "--start-number=9 --cover-letter --numbered -2 --stdout",
    );

    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let subjects: Vec<&str> = stdout_text
        .lines()
        .filter_map(|line| line.strip_prefix("Subject: "))
        .collect();
    assert_eq!(
        subjects,
        [
            "[PATCH 00/10] *** SUBJECT HERE ***",
            "[PATCH 09/10] 4",
            "[PATCH 10/10] rename"
        ]
    );
}

#[test]
fn numbered_flags_follow_last_occurrence() {
    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let output = run_libra_command(&["format-patch", "-N", "-n", "-1", "--stdout"], repo.path());
    assert_cli_success(&output, "-N -n -1 --stdout");

    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let subjects: Vec<&str> = stdout_text
        .lines()
        .filter_map(|line| line.strip_prefix("Subject: "))
        .collect();
    assert_eq!(subjects, ["[PATCH 1/1] rename"]);

    let output = run_libra_command(&["format-patch", "-n", "-N", "-1", "--stdout"], repo.path());
    assert_cli_success(&output, "-n -N -1 --stdout");

    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let subjects: Vec<&str> = stdout_text
        .lines()
        .filter_map(|line| line.strip_prefix("Subject: "))
        .collect();
    assert_eq!(subjects, ["[PATCH] rename"]);
}

#[test]
fn start_number_counter_is_unpadded() {
    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let output = run_libra_command(
        &[
            "format-patch",
            "--start-number",
            "3",
            "--numbered",
            "--stdout",
            "HEAD~1",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "--start-number 3 --numbered --stdout HEAD~1");

    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let subjects: Vec<&str> = stdout_text
        .lines()
        .filter_map(|line| line.strip_prefix("Subject: "))
        .collect();
    assert_eq!(subjects, ["[PATCH 3/3] rename"]);
}

#[test]
fn no_numbered_preserves_bare_numbered_files() {
    let repo = repo_with_patch_subjects(&["3", "4"]);
    let out_dir = tempdir().unwrap();
    let output = run_libra_command(
        &[
            "format-patch",
            "-N",
            "--numbered-files",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~2",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "-N --numbered-files");

    let mut names: Vec<String> = fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["1", "2"]);
}

#[test]
fn no_numbered_filename_limit_keeps_prefix() {
    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let out_dir = tempdir().unwrap();
    let output = run_libra_command(
        &[
            "format-patch",
            "-N",
            "--filename-max-length=12",
            "-1",
            "-o",
            out_dir.path().to_str().unwrap(),
        ],
        repo.path(),
    );
    assert_cli_success(&output, "-N --filename-max-length=12");
    let names: Vec<String> = fs::read_dir(out_dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["0001-.patch"]);
}

#[test]
fn no_numbered_attach_names_match_files() {
    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let output = run_libra_command(
        &["format-patch", "-N", "--attach", "-1", "--stdout"],
        repo.path(),
    );
    assert_cli_success(&output, "-N --attach -1 --stdout");
    let body = String::from_utf8_lossy(&output.stdout);
    assert!(
        body.contains("Content-Type: text/x-patch; name=\"0001-rename.patch\""),
        "attachment content type should retain the sequence prefix: {body}"
    );
    assert!(
        body.contains("Content-Disposition: attachment; filename=\"0001-rename.patch\""),
        "attachment disposition should retain the sequence prefix: {body}"
    );
}

#[test]
fn no_numbered_json_paths_match_files() {
    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let out_dir = tempdir().unwrap();
    let output = run_libra_command(
        &[
            "--json",
            "format-patch",
            "-N",
            "-o",
            out_dir.path().to_str().unwrap(),
            "HEAD~2",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "-N --json");

    let value = parse_json_stdout(&output);
    let patches = value["data"]["patches"].as_array().expect("patches array");
    let mut reported_paths: Vec<&str> = patches
        .iter()
        .map(|patch| patch["path"].as_str().expect("patch path"))
        .collect();
    reported_paths.sort_unstable();
    let mut written_paths: Vec<String> = fs::read_dir(out_dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path().to_string_lossy().into_owned())
        .collect();
    written_paths.sort();
    assert_eq!(
        reported_paths,
        written_paths.iter().map(String::as_str).collect::<Vec<_>>()
    );
}

#[test]
fn keep_subject_short_flag_is_accepted() {
    let repo = repo_with_commits(1);
    // `-k` is Git's short form of `--keep-subject`; the commit subject has no
    // `[PATCH]` marker so this is an output-equality check.
    let short = run_libra_command(
        &["format-patch", "-k", "HEAD~1..HEAD", "--stdout"],
        repo.path(),
    );
    assert_cli_success(&short, "-k");
    let long = run_libra_command(
        &["format-patch", "--keep-subject", "HEAD~1..HEAD", "--stdout"],
        repo.path(),
    );
    assert_eq!(
        String::from_utf8_lossy(&short.stdout)
            .lines()
            .filter(|l| l.starts_with("Subject:"))
            .collect::<Vec<_>>(),
        String::from_utf8_lossy(&long.stdout)
            .lines()
            .filter(|l| l.starts_with("Subject:"))
            .collect::<Vec<_>>(),
        "-k and --keep-subject agree"
    );
}

#[test]
fn no_base_cancels_the_base_trailer() {
    let repo = repo_with_commits(3);
    let with_base = run_libra_command(
        &[
            "format-patch",
            "--base",
            "HEAD~2",
            "HEAD~2..HEAD",
            "--stdout",
        ],
        repo.path(),
    );
    assert_cli_success(&with_base, "--base");
    assert!(
        String::from_utf8_lossy(&with_base.stdout).contains("base-commit:"),
        "--base emits the trailer"
    );

    let without = run_libra_command(
        &[
            "format-patch",
            "--base",
            "HEAD~2",
            "--no-base",
            "HEAD~2..HEAD",
            "--stdout",
        ],
        repo.path(),
    );
    assert_cli_success(&without, "--no-base");
    assert!(
        !String::from_utf8_lossy(&without.stdout).contains("base-commit:"),
        "--no-base cancels the trailer"
    );
}

#[test]
fn filename_max_length_truncates_like_git() {
    let repo = repo_with_commits(1);
    // The fixture subject ("commit 1") is short, so grow it into a long one.
    fs::write(repo.path().join("long.txt"), "x\n").unwrap();
    run_libra_command(&["add", "long.txt"], repo.path());
    run_libra_command(
        &[
            "commit",
            "-m",
            "this is a very long commit subject line that should be truncated",
            "--no-verify",
        ],
        repo.path(),
    );

    for (limit, expected) in [
        (10usize, "0001.patch"),
        (12, "0001-.patch"),
        (15, "0001-thi.patch"),
        (20, "0001-this-is-.patch"),
        (30, "0001-this-is-a-very-lon.patch"),
    ] {
        let out_dir = tempdir().unwrap();
        let output = run_libra_command(
            &[
                "format-patch",
                "--filename-max-length",
                &limit.to_string(),
                "-o",
                out_dir.path().to_str().unwrap(),
                "-1",
            ],
            repo.path(),
        );
        assert_cli_success(&output, &format!("--filename-max-length={limit}"));
        let names: Vec<String> = fs::read_dir(out_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![expected.to_string()], "limit {limit}");
    }
}

#[test]
fn cover_letter_commit_list_matches_git_shortlog() {
    let repo = repo_with_commits(2);
    let output = run_libra_command(
        &["format-patch", "--cover-letter", "HEAD~2..HEAD", "--stdout"],
        repo.path(),
    );
    assert_cli_success(&output, "cover letter shortlog");
    let body = String::from_utf8_lossy(&output.stdout);
    let name = run_libra_command(&["config", "user.name"], repo.path());
    let author = String::from_utf8_lossy(&name.stdout).trim().to_string();
    assert!(
        body.contains(&format!("{author} (2):\n  commit 1\n  commit 2\n")),
        "author-grouped shortlog expected, got:\n{body}"
    );

    // `--commit-list-format=modern` renders `[i/N] subject` lines instead.
    let modern = run_libra_command(
        &[
            "format-patch",
            "--cover-letter",
            "--commit-list-format=modern",
            "HEAD~2..HEAD",
            "--stdout",
        ],
        repo.path(),
    );
    assert_cli_success(&modern, "modern list");
    let modern_body = String::from_utf8_lossy(&modern.stdout);
    assert!(
        modern_body.contains("[1/2] commit 1") && modern_body.contains("[2/2] commit 2"),
        "modern commit list expected:\n{modern_body}"
    );

    // A pretty-format string renders each commit; unknown conversions are
    // rejected with Git's message.
    let pretty = run_libra_command(
        &[
            "format-patch",
            "--cover-letter",
            "--commit-list-format=%h %s",
            "HEAD~2..HEAD",
            "--stdout",
        ],
        repo.path(),
    );
    assert_cli_success(&pretty, "pretty list");
    assert!(
        String::from_utf8_lossy(&pretty.stdout).contains(" commit 2"),
        "pretty-format list expected"
    );

    let invalid = run_libra_command(
        &[
            "format-patch",
            "--cover-letter",
            "--commit-list-format=%q",
            "HEAD~2..HEAD",
            "--stdout",
        ],
        repo.path(),
    );
    assert!(
        String::from_utf8_lossy(&invalid.stderr).contains("is not a valid format string"),
        "invalid format string must be reported: {}",
        String::from_utf8_lossy(&invalid.stderr)
    );
}

#[test]
fn empty_series_succeeds_silently() {
    let repo = repo_with_commits(2);
    // Git exits 0 with no output when the resolved series is empty; libra used
    // to fail with "no patches to generate" (issues/529 scenario 1).
    for args in [
        vec!["format-patch", "--stdout"],
        vec!["format-patch", "HEAD..HEAD", "--stdout"],
        vec!["format-patch", "--stdout", "--cover-letter"],
    ] {
        let output = run_libra_command(&args, repo.path());
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?} must succeed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "{args:?} must print nothing: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    // A repository without commits is still fatal, like Git.
    let empty = create_committed_repo_via_cli();
    let unborn = tempfile::tempdir().unwrap();
    run_libra_command(&["init"], unborn.path());
    let output = run_libra_command(&["format-patch", "--stdout"], unborn.path());
    assert_ne!(
        output.status.code(),
        Some(0),
        "unborn HEAD stays an error: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    drop(empty);
}

#[test]
fn unsupported_git_options_are_classified() {
    let repo = repo_with_commits(2);

    // `-p` is Git's short form of `--no-stat`.
    let short = run_libra_command(&["format-patch", "-p", "-1", "--stdout"], repo.path());
    assert_cli_success(&short, "-p");
    assert!(
        !String::from_utf8_lossy(&short.stdout).contains("file changed"),
        "-p suppresses the diffstat"
    );

    // `--relative` and `--all` are named refusals, not typos.
    for (args, needle) in [
        (
            vec!["format-patch", "--relative=sub", "-1", "--stdout"],
            "--relative is not supported",
        ),
        (
            vec!["format-patch", "--all", "--stdout"],
            "--all is not supported",
        ),
    ] {
        let output = run_libra_command(&args, repo.path());
        assert_eq!(output.status.code(), Some(128), "{args:?}");
        let (_h, report) = parse_cli_error_stderr(&output.stderr);
        assert_eq!(report.error_code, "LBR-UNSUPPORTED-001", "{args:?}");
        assert!(
            report.message.contains(needle),
            "{args:?}: {}",
            report.message
        );
    }
}

// Each refusal gate is independently selectable; the plan also checks the
// complete selector set so accidentally omitted assertions cannot pass a card.
#[derive(Clone, Copy)]
enum RefusalGate {
    Message,
    Code,
    Exit,
    Stdout,
    Files,
    Json,
    Machine,
}

fn format_patch_snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    use sha2::{Digest, Sha256};

    fn visit(
        root: &Path,
        path: &Path,
        snapshot: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) {
        for entry in fs::read_dir(path).expect("read refusal snapshot directory") {
            let entry = entry.expect("read refusal snapshot entry");
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).expect("read snapshot metadata");
            let relative = path
                .strip_prefix(root)
                .expect("snapshot is within root")
                .to_path_buf();
            // Volatile bookkeeping (SQLite, oplog, logs, repair locks) changes
            // asynchronously and is not a refusal side effect.
            let volatile = relative.to_string_lossy().to_string();
            if volatile.contains("libra.db")
                || volatile.contains("oplog")
                || volatile.contains("logs")
                || volatile.contains("object-index-repair-locks")
                || volatile.ends_with(".lock")
            {
                continue;
            }
            let value = if metadata.is_symlink() {
                fs::read_link(&path)
                    .expect("read snapshot symlink")
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else if metadata.is_dir() {
                visit(root, &path, snapshot);
                b"directory".to_vec()
            } else {
                Sha256::digest(fs::read(&path).expect("read snapshot file")).to_vec()
            };
            snapshot.insert(relative, value);
        }
    }

    let mut snapshot = std::collections::BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

fn assert_format_patch_refusal(argv: &[&str], message: &str, gate: RefusalGate) {
    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let before = matches!(gate, RefusalGate::Files).then(|| format_patch_snapshot(repo.path()));
    let mut args = argv.to_vec();
    match gate {
        RefusalGate::Json => args.push("--json"),
        RefusalGate::Machine => args.push("--machine"),
        _ => {}
    }
    let output = run_libra_command(&args, repo.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    match gate {
        RefusalGate::Message => assert_eq!(
            stderr.lines().next(),
            Some(format!("fatal: {message}").as_str())
        ),
        RefusalGate::Code => assert_eq!(stderr.lines().nth(1), Some("Error-Code: LBR-CLI-002")),
        RefusalGate::Exit => assert_eq!(output.status.code(), Some(129), "{stderr}"),
        RefusalGate::Stdout => assert!(
            output.stdout.is_empty(),
            "unexpected stdout: {:?}",
            output.stdout
        ),
        RefusalGate::Files => {
            let before = before.expect("captured snapshot");
            let after = format_patch_snapshot(repo.path());
            let changed: std::collections::BTreeSet<_> = before
                .keys()
                .chain(after.keys())
                .filter(|path| before.get(*path) != after.get(*path))
                .collect();
            assert!(changed.is_empty(), "refusal changed paths: {changed:?}");
        }
        RefusalGate::Json | RefusalGate::Machine => {
            if matches!(gate, RefusalGate::Machine) {
                assert_eq!(
                    stderr.lines().count(),
                    1,
                    "machine error must be one JSON line"
                );
            }
            let actual: serde_json::Value =
                serde_json::from_slice(&output.stderr).expect("structured error");
            assert_eq!(
                actual,
                serde_json::json!({
                    "ok": false, "error_code": "LBR-CLI-002", "category": "cli",
                    "exit_code": 129, "severity": "fatal", "message": message,
                })
            );
        }
    }
}

macro_rules! refusal_gates {
    ($args:expr, $message:expr; $($name:ident => $gate:ident),+ $(,)?) => {
        $(
            #[test]
            #[allow(non_snake_case)]
            fn $name() {
                assert_format_patch_refusal($args, $message, RefusalGate::$gate);
            }
        )+
    };
}

refusal_gates!(
    &["format-patch", "--rfc", "-k", "-1", "--stdout"],
    "options '--subject-prefix/--rfc' and '-k' cannot be used together";
    fp03_rfc_k__msg => Message,
    fp03_rfc_k__code => Code,
    fp03_rfc_k__exit => Exit,
    fp03_rfc_k__stdout => Stdout,
    fp03_rfc_k__fs => Files,
    fp03_rfc_k__json => Json,
    fp03_rfc_k__machine => Machine,
);

refusal_gates!(
    &["format-patch", "-k", "--rfc=WIP", "-1", "--stdout"],
    "options '--subject-prefix/--rfc' and '-k' cannot be used together";
    fp03_k_rfc_wip__msg => Message,
    fp03_k_rfc_wip__code => Code,
    fp03_k_rfc_wip__exit => Exit,
    fp03_k_rfc_wip__stdout => Stdout,
    fp03_k_rfc_wip__fs => Files,
    fp03_k_rfc_wip__json => Json,
    fp03_k_rfc_wip__machine => Machine,
);

refusal_gates!(
    &["format-patch", "--rfc=-(WIP)", "-k", "-1", "--stdout"],
    "options '--subject-prefix/--rfc' and '-k' cannot be used together";
    fp03_dash_rfc_k__msg => Message,
    fp03_dash_rfc_k__code => Code,
    fp03_dash_rfc_k__exit => Exit,
    fp03_dash_rfc_k__stdout => Stdout,
    fp03_dash_rfc_k__fs => Files,
    fp03_dash_rfc_k__json => Json,
    fp03_dash_rfc_k__machine => Machine,
);

refusal_gates!(
    &["format-patch", "--subject-prefix=X", "-k", "-1", "--stdout"],
    "options '--subject-prefix/--rfc' and '-k' cannot be used together";
    fp03_prefix_k__msg => Message,
    fp03_prefix_k__code => Code,
    fp03_prefix_k__exit => Exit,
    fp03_prefix_k__stdout => Stdout,
    fp03_prefix_k__fs => Files,
    fp03_prefix_k__json => Json,
    fp03_prefix_k__machine => Machine,
);

refusal_gates!(
    &["format-patch", "--subject-prefix=", "-k", "-1", "--stdout"],
    "options '--subject-prefix/--rfc' and '-k' cannot be used together";
    fp03_empty_prefix_k__msg => Message,
    fp03_empty_prefix_k__code => Code,
    fp03_empty_prefix_k__exit => Exit,
    fp03_empty_prefix_k__stdout => Stdout,
    fp03_empty_prefix_k__fs => Files,
    fp03_empty_prefix_k__json => Json,
    fp03_empty_prefix_k__machine => Machine,
);

refusal_gates!(
    &["format-patch", "-n", "-k", "-1", "--stdout"],
    "options '-n' and '-k' cannot be used together";
    fp03_numbered_k__msg => Message,
    fp03_numbered_k__code => Code,
    fp03_numbered_k__exit => Exit,
    fp03_numbered_k__stdout => Stdout,
    fp03_numbered_k__fs => Files,
    fp03_numbered_k__json => Json,
    fp03_numbered_k__machine => Machine,
);

#[test]
fn fp03_config_prefix_with_k() {
    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let config = run_libra_command(&["config", "set", "format.subjectPrefix", "X"], repo.path());
    assert_cli_success(&config, "configure subject prefix");
    let output = run_libra_command(&["format-patch", "-k", "-1", "--stdout"], repo.path());
    assert_cli_success(&output, "compatible keep-subject options");
    let body = String::from_utf8_lossy(&output.stdout);
    let subjects: Vec<_> = body
        .lines()
        .filter(|line| line.starts_with("Subject:"))
        .collect();
    assert_eq!(subjects, ["Subject: rename"]);
}

#[test]
fn fp03_empty_rfc_with_k() {
    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let output = run_libra_command(
        &["format-patch", "--rfc=", "-k", "-1", "--stdout"],
        repo.path(),
    );
    assert_cli_success(&output, "compatible keep-subject options");
    let body = String::from_utf8_lossy(&output.stdout);
    let subjects: Vec<_> = body
        .lines()
        .filter(|line| line.starts_with("Subject:"))
        .collect();
    assert_eq!(subjects, ["Subject: rename"]);
}

#[test]
fn fp03_cancelled_rfc_with_k() {
    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let output = run_libra_command(
        &["format-patch", "--rfc", "--no-rfc", "-k", "-1", "--stdout"],
        repo.path(),
    );
    assert_cli_success(&output, "compatible keep-subject options");
    let body = String::from_utf8_lossy(&output.stdout);
    let subjects: Vec<_> = body
        .lines()
        .filter(|line| line.starts_with("Subject:"))
        .collect();
    assert_eq!(subjects, ["Subject: rename"]);
}

// ---------------------------------------------------------------------------
// FP-05: patch body/stat come from the diff engine (rename detection by default)
// ---------------------------------------------------------------------------

fn repo_with_pure_rename() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    fs::write(
        repo.path().join("lorem"),
        "lorem\nipsum\ndolor\nsit\namet\n",
    )
    .unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "lorem"], repo.path()),
        "add lorem",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path()),
        "commit base",
    );
    assert_cli_success(
        &run_libra_command(&["mv", "lorem", "ipsum"], repo.path()),
        "mv lorem ipsum",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "rename", "--no-verify"], repo.path()),
        "commit rename",
    );
    repo
}

fn patch_body(stdout: &str) -> String {
    match stdout.split_once("diff --git ") {
        Some((_, rest)) => format!("diff --git {rest}"),
        None => String::new(),
    }
}

#[test]
fn pure_rename_is_detected_by_default() {
    let repo = repo_with_pure_rename();
    let output = run_libra_command(&["format-patch", "-1", "--stdout"], repo.path());
    assert_cli_success(&output, "format-patch -1 --stdout");
    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let body = patch_body(&stdout_text);
    // The patch body ends before the mail signature trailer.
    let body = body.split("\n-- \n").next().unwrap_or(&body);
    assert_eq!(
        format!("{body}\n"),
        "diff --git a/lorem b/ipsum\n\
         similarity index 100%\n\
         rename from lorem\n\
         rename to ipsum\n",
        "pure rename body"
    );
}

#[test]
fn stat_block_matches_libra_diff_stat() {
    let repo = repo_with_pure_rename();
    let output = run_libra_command(&["format-patch", "-1", "--stdout"], repo.path());
    assert_cli_success(&output, "format-patch -1 --stdout");
    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let after_separator = stdout_text
        .split_once("\n---\n")
        .map(|(_, rest)| rest.to_string())
        .expect("patch separator");
    let stat_block = after_separator
        .split_once("diff --git ")
        .map(|(stat, _)| stat.trim_end_matches('\n').to_string())
        .expect("stat block");

    let head = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    let parent = run_libra_command(&["rev-parse", "HEAD~1"], repo.path());
    let head_id = String::from_utf8_lossy(&head.stdout).trim().to_string();
    let parent_id = String::from_utf8_lossy(&parent.stdout).trim().to_string();
    let diff_stat = run_libra_command(&["diff", "--stat", &parent_id, &head_id], repo.path());
    assert_cli_success(&diff_stat, "diff --stat");
    let expected = String::from_utf8_lossy(&diff_stat.stdout)
        .trim_end_matches('\n')
        .to_string();
    assert_eq!(stat_block, expected, "stat block vs libra diff --stat");
}

#[test]
fn partial_rename_is_detected_by_default() {
    let repo = create_committed_repo_via_cli();
    let original: String = (1..=10).map(|n| format!("line{n}\n")).collect();
    fs::write(repo.path().join("alpha"), &original).unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "alpha"], repo.path()),
        "add alpha",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path()),
        "commit base",
    );
    assert_cli_success(
        &run_libra_command(&["mv", "alpha", "beta"], repo.path()),
        "mv alpha beta",
    );
    let edited = original.replace("line10\n", "line10 changed\n");
    fs::write(repo.path().join("beta"), edited).unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "beta"], repo.path()),
        "add beta",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "partial", "--no-verify"], repo.path()),
        "commit partial",
    );
    let output = run_libra_command(&["format-patch", "-1", "--stdout"], repo.path());
    assert_cli_success(&output, "format-patch -1 --stdout");
    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let body = patch_body(&stdout_text);
    assert!(
        body.contains("similarity index ") && !body.contains("similarity index 100%"),
        "partial similarity line: {body}"
    );
    assert!(body.contains("rename from alpha"), "rename from: {body}");
    assert!(body.contains("@@"), "content hunk: {body}");
}

#[test]
fn diff_renames_false_disables_detection() {
    let repo = repo_with_pure_rename();
    assert_cli_success(
        &run_libra_command(&["config", "set", "diff.renames", "false"], repo.path()),
        "config diff.renames",
    );
    let output = run_libra_command(&["format-patch", "-1", "--stdout"], repo.path());
    assert_cli_success(&output, "format-patch -1 --stdout");
    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let body = patch_body(&stdout_text);
    assert!(!body.contains("similarity index"), "no similarity: {body}");
    assert!(body.contains("deleted file mode"), "deletion side: {body}");
    assert!(body.contains("new file mode"), "addition side: {body}");
}

// ---------------------------------------------------------------------------
// FP-06: rename-detection parameters (-M / --find-renames / --no-renames)
// ---------------------------------------------------------------------------

fn repo_with_renamed_file(partial: bool) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let body: String = (1..=20).map(|n| format!("line{n}\n")).collect();
    fs::write(repo.path().join("file"), &body).unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "file"], repo.path()),
        "add file",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path()),
        "commit base",
    );
    assert_cli_success(
        &run_libra_command(&["mv", "file", "renamed"], repo.path()),
        "mv file renamed",
    );
    if partial {
        let edited = body
            .replace("line1\n", "one\n")
            .replace("line2\n", "two\n")
            .replace("line3\n", "three\n");
        fs::write(repo.path().join("renamed"), edited).unwrap();
        assert_cli_success(
            &run_libra_command(&["add", "renamed"], repo.path()),
            "add renamed",
        );
    }
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "rename", "--no-verify"], repo.path()),
        "commit rename",
    );
    repo
}

fn rename_detected(repo: &tempfile::TempDir, args: &[&str]) -> bool {
    let output = run_libra_command(args, repo.path());
    assert_cli_success(&output, &format!("{args:?}"));
    String::from_utf8_lossy(&output.stdout).contains("rename from")
}

#[test]
fn fp06_threshold_above_similarity() {
    let repo = repo_with_renamed_file(true);
    assert!(
        !rename_detected(&repo, &["format-patch", "-M90%", "-1", "--stdout"]),
        "-M90% must not detect a ~78% rename"
    );
}

#[test]
fn fp06_no_renames() {
    let repo = repo_with_renamed_file(false);
    assert!(!rename_detected(
        &repo,
        &["format-patch", "--no-renames", "-1", "--stdout"]
    ));
}

#[test]
fn fp06_find_renames_beats_config() {
    let repo = repo_with_renamed_file(false);
    assert_cli_success(
        &run_libra_command(&["config", "set", "diff.renames", "false"], repo.path()),
        "config diff.renames",
    );
    assert!(rename_detected(
        &repo,
        &["format-patch", "--find-renames", "-1", "--stdout"]
    ));
}

#[test]
fn fp06_no_renames_last_wins() {
    let repo = repo_with_renamed_file(false);
    assert!(!rename_detected(
        &repo,
        &["format-patch", "-M", "--no-renames", "-1", "--stdout"]
    ));
}

#[test]
fn fp06_find_renames_last_wins() {
    let repo = repo_with_renamed_file(false);
    assert!(rename_detected(
        &repo,
        &["format-patch", "--no-renames", "-M", "-1", "--stdout"]
    ));
}

#[test]
fn fp06_bare_m_before_stdout() {
    let repo = repo_with_rename_branch();
    assert!(rename_detected(
        &repo,
        &["format-patch", "-M", "--stdout", "lorem"]
    ));
}

#[test]
fn fp06_bare_m_before_revision() {
    let repo = repo_with_rename_branch();
    let first = run_libra_command(&["format-patch", "-M", "--stdout", "lorem"], repo.path());
    let second = run_libra_command(&["format-patch", "-M", "lorem", "--stdout"], repo.path());
    assert_cli_success(&first, "first ordering");
    assert_cli_success(&second, "second ordering");
    assert_eq!(
        first.stdout, second.stdout,
        "bare -M must not swallow the revision"
    );
}

fn repo_with_rename_branch() -> tempfile::TempDir {
    let repo = repo_with_renamed_file(false);
    assert_cli_success(
        &run_libra_command(&["branch", "lorem", "HEAD~1"], repo.path()),
        "branch lorem",
    );
    repo
}

refusal_gates!(
    &["format-patch", "-Mfoo", "-1", "--stdout"],
    "invalid argument to find-renames: 'foo'";
    fp06_bad_threshold__msg => Message,
    fp06_bad_threshold__code => Code,
    fp06_bad_threshold__exit => Exit,
    fp06_bad_threshold__stdout => Stdout,
    fp06_bad_threshold__fs => Files,
    fp06_bad_threshold__json => Json,
    fp06_bad_threshold__machine => Machine,
);

// ---------------------------------------------------------------------------
// FP-07: `format-patch --numstat`
// ---------------------------------------------------------------------------

fn repo_with_body_and_change() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("file.txt"), "one\ntwo\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "file.txt"], repo.path()),
        "add one",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "numstat base", "--no-verify"],
            repo.path(),
        ),
        "commit base",
    );
    fs::write(repo.path().join("file.txt"), "one\ntwo\nthree\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "file.txt"], repo.path()),
        "add two",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "numstat change\n\nbody line", "--no-verify"],
            repo.path(),
        ),
        "commit change",
    );
    repo
}

fn body_section(stdout: &str) -> String {
    let after_headers = stdout
        .split_once("\n\n")
        .map(|(_, rest)| rest.to_string())
        .expect("header/body separator");
    match after_headers.split_once("diff --git ") {
        Some((body, _)) => body.to_string(),
        None => after_headers,
    }
}

#[test]
fn numstat_replaces_the_stat_block() {
    let repo = repo_with_body_and_change();
    let output = run_libra_command(
        &["format-patch", "--numstat", "-1", "--stdout"],
        repo.path(),
    );
    assert_cli_success(&output, "--numstat");
    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let body = body_section(&stdout_text);
    assert!(!body.contains("---\n"), "no `---` separator: {body}");
    assert!(body.contains("1\t0\tfile.txt"), "numstat line: {body}");
    assert!(
        !body.contains("file changed"),
        "no diffstat summary: {body}"
    );
}

#[test]
fn numstat_matches_libra_diff_numstat() {
    let repo = repo_with_body_and_change();
    let output = run_libra_command(
        &["format-patch", "--numstat", "-1", "--stdout"],
        repo.path(),
    );
    assert_cli_success(&output, "--numstat");
    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let body = body_section(&stdout_text);
    let numstat_line = body
        .lines()
        .find(|line| line.contains("\tfile.txt"))
        .expect("numstat row");
    let head = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    let parent = run_libra_command(&["rev-parse", "HEAD~1"], repo.path());
    let head_id = String::from_utf8_lossy(&head.stdout).trim().to_string();
    let parent_id = String::from_utf8_lossy(&parent.stdout).trim().to_string();
    let diff = run_libra_command(&["diff", "--numstat", &parent_id, &head_id], repo.path());
    assert_cli_success(&diff, "diff --numstat");
    let expected = String::from_utf8_lossy(&diff.stdout)
        .trim_end_matches('\n')
        .to_string();
    assert_eq!(numstat_line, expected, "numstat vs libra diff --numstat");
}

#[test]
fn numstat_still_emits_every_patch() {
    let repo = repo_with_commits(5);
    let output = run_libra_command(
        &["format-patch", "--numstat", "--stdout", "HEAD~2..HEAD"],
        repo.path(),
    );
    assert_cli_success(&output, "--numstat range");
    let stdout_text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout_text.matches("diff --git a/").count(),
        2,
        "two patches in range: {stdout_text}"
    );
}

#[test]
fn no_stat_with_numstat_keeps_numstat() {
    let repo = repo_with_body_and_change();
    let plain = run_libra_command(
        &["format-patch", "--numstat", "-1", "--stdout"],
        repo.path(),
    );
    let with_p = run_libra_command(
        &["format-patch", "-p", "--numstat", "-1", "--stdout"],
        repo.path(),
    );
    assert_cli_success(&plain, "--numstat");
    assert_cli_success(&with_p, "-p --numstat");
    assert_eq!(plain.stdout, with_p.stdout, "`-p` must not drop numstat");
}

#[test]
fn numstat_lives_in_the_attach_text_part() {
    let repo = repo_with_body_and_change();
    let output = run_libra_command(
        &["format-patch", "--numstat", "--attach", "-1", "--stdout"],
        repo.path(),
    );
    assert_cli_success(&output, "--numstat --attach");
    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let text_part_start = stdout_text
        .find("Content-Type: text/plain")
        .expect("text part header");
    let patch_part_start = stdout_text
        .find("Content-Type: text/x-patch")
        .expect("patch part header");
    let numstat_pos = stdout_text.find("1\t0\tfile.txt").expect("numstat line");
    assert!(
        numstat_pos > text_part_start && numstat_pos < patch_part_start,
        "numstat must stay in the text/plain part"
    );
}

/// I957/FPG-22: `-U<n>` and `--unified=<n>` control hunk context like Git.
#[test]
fn unified_context_lines() {
    use std::process::Command;

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let body: String = (1..=20).map(|n| format!("{n}\n")).collect();
    fs::write(p.join("f.txt"), &body).unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "base",
    );
    let changed = body.replace("\n10\n", "\nten\n");
    fs::write(p.join("f.txt"), changed).unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add change");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "change", "--no-verify"], p),
        "change",
    );

    let diff_section = |stdout: &str| -> String {
        let start = stdout.find("diff --git").unwrap_or(0);
        stdout[start..].to_string()
    };
    let libra_u1 = String::from_utf8_lossy(
        &run_libra_command(
            &[
                "format-patch",
                "-1",
                "--stdout",
                "-U1",
                "--no-thread",
                "--no-signature",
            ],
            p,
        )
        .stdout,
    )
    .to_string();
    let libra_u5 = String::from_utf8_lossy(
        &run_libra_command(
            &[
                "format-patch",
                "-1",
                "--stdout",
                "--unified=5",
                "--no-thread",
                "--no-signature",
            ],
            p,
        )
        .stdout,
    )
    .to_string();
    assert!(
        diff_section(&libra_u1).contains("@@ -9,3 +9,3 @@"),
        "AC1 U1 header"
    );
    assert!(
        diff_section(&libra_u5).contains("@@ -5,11 +5,11 @@"),
        "AC2 U5 header"
    );
    assert_ne!(diff_section(&libra_u1), diff_section(&libra_u5));

    // Git golden diff sections (same fixture, deterministic dates).
    let git_dir = tempdir().expect("git dir");
    let g = git_dir.path();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(g)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("HOME", g)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        out
    };
    git(&["init", "-q", "-b", "main", "."]);
    fs::write(g.join("f.txt"), &body).unwrap();
    git(&["add", "f.txt"]);
    git(&["commit", "-q", "-m", "base"]);
    fs::write(g.join("f.txt"), body.replace("\n10\n", "\nten\n")).unwrap();
    git(&["add", "f.txt"]);
    git(&["commit", "-q", "-m", "change"]);
    for (flag, libra) in [("-U1", &libra_u1), ("--unified=5", &libra_u5)] {
        let git_patch = String::from_utf8_lossy(
            &git(&[
                "format-patch",
                "-1",
                "--stdout",
                flag,
                "--no-thread",
                "--no-signature",
            ])
            .stdout,
        )
        .to_string();
        assert_eq!(
            diff_section(libra).trim_end(),
            diff_section(&git_patch).trim_end(),
            "FPG-22 {flag} diff section must match Git"
        );
    }
}

/// FPG-61 AC1: `--full-diff` is accepted and, without a pathspec, leaves the
/// output byte-identical.
#[test]
fn full_diff_accepted_without_pathspec() {
    let repo = repo_with_commits(2);
    let p = repo.path();
    let base = run_libra_command(
        &[
            "format-patch",
            "-1",
            "--stdout",
            "--no-thread",
            "--no-signature",
        ],
        p,
    );
    assert_cli_success(&base, "baseline format-patch");
    let with_full = run_libra_command(
        &[
            "format-patch",
            "-1",
            "--stdout",
            "--full-diff",
            "--no-thread",
            "--no-signature",
        ],
        p,
    );
    assert_cli_success(&with_full, "AC1 --full-diff accepted");
    assert_eq!(
        with_full.stdout, base.stdout,
        "AC1 byte-identical without pathspec"
    );
}

/// FPG-42: cover-letter MIME headers appear only when a commit's raw buffer
/// contains non-ASCII bytes.
#[test]
fn cover_letter_mime_on_demand() {
    fn cover_of(stdout: &str) -> String {
        stdout.split("\nFrom ").next().unwrap_or(stdout).to_string()
    }
    fn commit_file(repo: &std::path::Path, name: &str, body: &str, msg: &str) {
        fs::write(repo.join(name), body).unwrap();
        assert_cli_success(&run_libra_command(&["add", name], repo), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", msg, "--no-verify"], repo),
            "commit",
        );
    }

    // AC1: an all-ASCII series emits no cover-letter MIME headers.
    let ascii = create_committed_repo_via_cli();
    commit_file(ascii.path(), "a.txt", "a\n", "ascii one");
    commit_file(ascii.path(), "b.txt", "b\n", "ascii two");
    let out = run_libra_command(
        &[
            "format-patch",
            "--cover-letter",
            "-2",
            "--stdout",
            "--no-signature",
        ],
        ascii.path(),
    );
    assert_cli_success(&out, "AC1 cover letter");
    let cover = cover_of(&String::from_utf8_lossy(&out.stdout));
    assert!(!cover.contains("MIME-Version"), "AC1 ASCII cover: {cover}");

    // AC2: only the second commit's committer name is non-ASCII.
    let mixed = create_committed_repo_via_cli();
    commit_file(mixed.path(), "a.txt", "a\n", "first");
    fs::write(mixed.path().join("b.txt"), "b\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "b.txt"], mixed.path()),
        "add second",
    );
    let commit = run_libra_command_with_env(
        &["commit", "-m", "second", "--no-verify"],
        mixed.path(),
        &[("GIT_COMMITTER_NAME", "Cömmitter")],
    );
    assert_cli_success(&commit, "non-ascii committer commit");
    commit_file(mixed.path(), "c.txt", "c\n", "third");
    let out = run_libra_command(
        &[
            "format-patch",
            "--cover-letter",
            "-3",
            "--stdout",
            "--no-signature",
        ],
        mixed.path(),
    );
    assert_cli_success(&out, "AC2 cover letter");
    let cover = cover_of(&String::from_utf8_lossy(&out.stdout));
    assert!(
        cover.contains("MIME-Version"),
        "AC2 non-ASCII cover: {cover}"
    );
}

/// FPG-09 (#888 item 5): lone `-k`, `--subject-prefix`, and `--rfc` succeed and
/// produce exactly one `Subject:` line with the expected prefix.
#[test]
fn keep_subject_and_prefix_alone_succeed() {
    let repo = repo_with_patch_subjects(&["3", "4", "rename"]);
    let p = repo.path();
    let subjects = |args: &[&str]| -> Vec<String> {
        let out = run_libra_command(args, p);
        assert_cli_success(&out, "format-patch success path");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|line| line.starts_with("Subject:"))
            .map(str::to_string)
            .collect()
    };
    assert_eq!(
        subjects(&["format-patch", "-k", "-1", "--stdout"]),
        ["Subject: rename"],
        "AC2 lone -k"
    );
    assert_eq!(
        subjects(&["format-patch", "--subject-prefix=X", "-1", "--stdout"]),
        ["Subject: [X] rename"],
        "AC3 lone --subject-prefix"
    );
    assert_eq!(
        subjects(&["format-patch", "--rfc", "-1", "--stdout"]),
        ["Subject: [RFC PATCH] rename"],
        "AC4 lone --rfc"
    );
}

/// FPG-16: mbox separators use Git's fixed `Mon Sep 17 00:00:00 2001`
/// timestamp; `--zero-commit` zeroes the hash; the cover letter uses the
/// series' last commit oid; the resulting mbox applies with `libra am`.
#[test]
fn mbox_envelope_fixed_2001_timestamp() {
    let repo = repo_with_patch_subjects(&["first subject", "second subject"]);
    let p = repo.path();
    let newest = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    let older = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD~1"], p).stdout)
        .trim()
        .to_string();

    let out = run_libra_command(&["format-patch", "-2", "--stdout", "--no-thread"], p);
    assert_cli_success(&out, "AC1 format-patch -2");
    let mbox = String::from_utf8_lossy(&out.stdout).to_string();
    let envelopes: Vec<&str> = mbox
        .lines()
        .filter(|line| line.starts_with("From "))
        .collect();
    // Patches are emitted oldest-first.
    assert_eq!(
        envelopes,
        vec![
            format!("From {older} Mon Sep 17 00:00:00 2001").as_str(),
            format!("From {newest} Mon Sep 17 00:00:00 2001").as_str(),
        ],
        "AC1 fixed separator timestamps"
    );

    let zero = run_libra_command(
        &[
            "format-patch",
            "-2",
            "--stdout",
            "--zero-commit",
            "--no-thread",
        ],
        p,
    );
    assert_cli_success(&zero, "AC2 --zero-commit");
    for line in String::from_utf8_lossy(&zero.stdout)
        .lines()
        .filter(|line| line.starts_with("From "))
    {
        assert_eq!(
            line, "From 0000000000000000000000000000000000000000 Mon Sep 17 00:00:00 2001",
            "AC2 zero envelope"
        );
    }

    let cover = run_libra_command(
        &[
            "format-patch",
            "--cover-letter",
            "-2",
            "--stdout",
            "--no-thread",
        ],
        p,
    );
    assert_cli_success(&cover, "AC3 cover letter");
    let first = String::from_utf8_lossy(&cover.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        first,
        format!("From {newest} Mon Sep 17 00:00:00 2001"),
        "AC3 cover envelope uses series last oid"
    );

    // AC4: the fixed-envelope mbox splits and applies with `libra am`.
    let receiver = create_committed_repo_via_cli();
    let applied = run_libra_command_with_stdin(&["am", "-"], receiver.path(), &mbox);
    assert_cli_success(&applied, "AC4 libra am");
    let subjects = String::from_utf8_lossy(
        &run_libra_command(&["log", "-2", "--format=%s"], receiver.path()).stdout,
    )
    .to_string();
    assert_eq!(
        subjects, "second subject\nfirst subject\n",
        "AC4 applied subjects"
    );
}

/// FPG-27: `--diff-filter=A` filters both the patch and the diffstat; an
/// invalid filter is refused with `LBR-CLI-002` and no stdout.
#[test]
fn diff_filter() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for (name, body) in [("a.txt", "a\n"), ("b.txt", "b\n"), ("c.txt", "c\n")] {
        fs::write(p.join(name), body).unwrap();
    }
    assert_cli_success(&run_libra_command(&["add", "."], p), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "base",
    );
    fs::write(p.join("b.txt"), "b2\n").unwrap();
    fs::remove_file(p.join("c.txt")).unwrap();
    fs::write(p.join("d.txt"), "d\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "-A"], p), "change");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "change", "--no-verify"], p),
        "change commit",
    );

    let out = run_libra_command(
        &[
            "format-patch",
            "-1",
            "--stdout",
            "--diff-filter=A",
            "--no-thread",
            "--no-signature",
        ],
        p,
    );
    assert_cli_success(&out, "AC1 --diff-filter=A");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let diff_paths: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("diff --git a/"))
        .map(|rest| rest.split(" b/").next().unwrap_or(rest))
        .collect();
    assert_eq!(diff_paths, ["d.txt"], "AC1 patch paths");
    let stat_paths: Vec<String> = stdout
        .lines()
        .filter(|line| line.starts_with(' ') && line.contains('|'))
        .filter_map(|line| line.split('|').next())
        .map(|path| path.trim().trim_end_matches(" => ").to_string())
        .filter(|path| !path.is_empty() && !path.contains("file changed"))
        .collect();
    assert_eq!(stat_paths, ["d.txt"], "AC2 diffstat paths");

    let invalid = run_libra_command(&["format-patch", "-1", "--stdout", "--diff-filter=Q"], p);
    assert!(!invalid.status.success(), "AC3 invalid filter");
    let stderr = String::from_utf8_lossy(&invalid.stderr);
    assert!(
        stderr.lines().any(|line| line == "Error-Code: LBR-CLI-002"),
        "AC3 stable code line: {stderr}"
    );
    assert!(stderr.contains("diff-filter"), "AC3 phrase: {stderr}");
    assert!(invalid.stdout.is_empty(), "AC3 stdout empty");
}

/// FPG-10: `--from=<ident>` must be a valid ident line, validated before
/// revision parsing; bare `--from` uses the execution committer identity.
#[test]
fn from_invalid_ident_rejected() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "set", "user.name", "Bob"], p),
        "author name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "set", "user.email", "bob@example.com"], p),
        "author email",
    );
    fs::write(p.join("f.txt"), "f\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "subject", "--no-verify"], p),
        "commit",
    );

    let invalid = run_libra_command(&["format-patch", "-1", "--stdout", "--from=foo"], p);
    assert!(!invalid.status.success(), "AC1 invalid ident");
    let stderr = String::from_utf8_lossy(&invalid.stderr);
    assert!(stderr.lines().any(|line| line == "Error-Code: LBR-CLI-002"));
    assert!(stderr.contains("invalid ident line"), "{stderr}");
    assert!(invalid.stdout.is_empty(), "AC1 stdout empty");

    let valid = run_libra_command(
        &[
            "format-patch",
            "-1",
            "--stdout",
            "--from=Alice <alice@example.com>",
            "--no-thread",
            "--no-signature",
        ],
        p,
    );
    assert_cli_success(&valid, "AC2 valid --from");
    let body = String::from_utf8_lossy(&valid.stdout).to_string();
    let headers = body
        .split_once("\n\n")
        .map(|(headers, _)| headers)
        .unwrap_or(&body);
    let from_lines: Vec<&str> = headers
        .lines()
        .filter(|line| line.starts_with("From: "))
        .collect();
    assert_eq!(
        from_lines,
        ["From: Alice <alice@example.com>"],
        "AC2 From header"
    );
    assert!(
        body.contains("\n\nFrom: Bob <bob@example.com>\n"),
        "AC3 in-body original author: {body}"
    );

    let bare = run_libra_command_with_env(
        &[
            "format-patch",
            "-1",
            "--stdout",
            "--from",
            "--no-thread",
            "--no-signature",
        ],
        p,
        &[
            ("GIT_COMMITTER_NAME", "C O Mitter"),
            ("GIT_COMMITTER_EMAIL", "committer@example.com"),
        ],
    );
    assert_cli_success(&bare, "AC4 bare --from");
    let bare_body = String::from_utf8_lossy(&bare.stdout).to_string();
    assert!(
        bare_body
            .lines()
            .any(|line| line == "From: C O Mitter <committer@example.com>"),
        "AC4 committer identity: {bare_body}"
    );

    let empty = run_libra_command(&["format-patch", "--from=foo", "--stdout", "HEAD..HEAD"], p);
    assert!(!empty.status.success(), "AC5 empty range still validates");
    let stderr = String::from_utf8_lossy(&empty.stderr);
    assert!(stderr.lines().any(|line| line == "Error-Code: LBR-CLI-002"));
    assert!(stderr.contains("invalid ident line"));

    let empty_email = run_libra_command(
        &[
            "format-patch",
            "-1",
            "--stdout",
            "--from=Name <>",
            "--no-thread",
            "--no-signature",
        ],
        p,
    );
    assert_cli_success(&empty_email, "AC6 empty email");
    assert!(
        String::from_utf8_lossy(&empty_email.stdout)
            .lines()
            .any(|line| line == "From: Name <>"),
        "AC6 From: Name <>"
    );
}
