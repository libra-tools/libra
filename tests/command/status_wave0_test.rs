//! Wave-0 status regressions (plan-20260714 Part B).
//!
//! The canonical list of tests in this module lives in
//! `tests/compat/status_wave0_manifest.rs` (`STATUS_WAVE0_TESTS`); the
//! `compat_status_wave0_register` gate asserts the two stay in sync in both
//! directions. Rename-detection behavior tests join this module slice by
//! slice as R0-1..R0-8 land (see plan B.8/B.9).

use std::{fs, path::Path};

use git_internal::internal::{
    index::{Index, IndexEntry},
    object::blob::Blob,
};
use libra::{command::save_object, utils::path};

use super::*;

fn create_repo_with_committed_file(path: &str, content: &str) -> tempfile::TempDir {
    let repo = tempdir().expect("failed to create temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::write(repo.path().join(path), content).expect("failed to write committed fixture");

    let add = run_libra_command(&["add", path], repo.path());
    assert_cli_success(&add, "stage committed fixture");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit fixture");
    repo
}

fn status_stdout(repo: &Path, args: &[&str]) -> String {
    let output = run_libra_command(args, repo);
    assert_cli_success(&output, "status command");
    String::from_utf8(output.stdout).expect("status stdout should be utf-8")
}

fn write_blob_to_repo(content: &str) -> (ObjectHash, u32) {
    let blob = Blob::from_content(content);
    save_object(&blob, &blob.id).expect("failed to save blob");
    (blob.id, blob.data.len() as u32)
}

fn add_index_stage(index: &mut Index, file: &str, content: &str, stage: u8) {
    let (hash, size) = write_blob_to_repo(content);
    let mut entry = IndexEntry::new_from_blob(file.to_string(), hash, size);
    entry.flags.stage = stage;
    index.add(entry);
}

#[test]
#[serial]
fn porcelain_v2_unmerged_u_line() {
    let repo = create_repo_with_committed_file("conflict.txt", "base\n");
    let _guard = ChangeDirGuard::new(repo.path());
    let mut index = Index::new();
    add_index_stage(&mut index, "conflict.txt", "base\n", 1);
    add_index_stage(&mut index, "conflict.txt", "ours\n", 2);
    add_index_stage(&mut index, "conflict.txt", "theirs\n", 3);
    index
        .save(path::index())
        .expect("failed to write unmerged index");

    let output = status_stdout(repo.path(), &["status", "--porcelain", "v2"]);
    let u_line = output
        .lines()
        .find(|line| line.starts_with("u UU "))
        .expect("expected porcelain v2 unmerged row");
    let fields: Vec<_> = u_line.split_whitespace().collect();

    assert_eq!(fields.len(), 11, "unexpected u-line fields: {u_line}");
    assert_eq!(fields[1], "UU");
    assert_eq!(fields[10], "conflict.txt");
}

#[test]
#[serial]
fn resolved_conflict_with_stage0_emits_no_u_line() {
    let repo = create_repo_with_committed_file("conflict.txt", "base\n");
    let _guard = ChangeDirGuard::new(repo.path());
    let mut index = Index::new();
    add_index_stage(&mut index, "conflict.txt", "base\n", 1);
    add_index_stage(&mut index, "conflict.txt", "ours\n", 2);
    add_index_stage(&mut index, "conflict.txt", "theirs\n", 3);
    add_index_stage(&mut index, "conflict.txt", "resolved\n", 0);
    index
        .save(path::index())
        .expect("failed to write resolved index");

    let output = status_stdout(repo.path(), &["status", "--porcelain", "v2"]);

    assert!(
        !output.lines().any(|line| line.starts_with("u ")),
        "resolved stage-0 path must not emit u line:\n{output}"
    );
    assert!(
        output.lines().any(|line| line.starts_with("1 M")),
        "resolved stage-0 path should be rendered as a normal tracked row:\n{output}"
    );
}

#[test]
#[serial]
fn unmerged_stage_presence_to_xy_mapping() {
    // Exercises the seven Git unmerged stage-presence combinations through the
    // public `--short` surface (stage 1 = base, 2 = ours, 3 = theirs).
    let cases = [
        ((false, true, true), "AA"),
        ((true, false, false), "DD"),
        ((false, true, false), "AU"),
        ((false, false, true), "UA"),
        ((true, false, true), "DU"),
        ((true, true, false), "UD"),
        ((true, true, true), "UU"),
    ];

    for ((base, ours, theirs), expected) in cases {
        let repo = create_repo_with_committed_file("conflict.txt", "base\n");
        let _guard = ChangeDirGuard::new(repo.path());
        let mut index = Index::new();
        if base {
            add_index_stage(&mut index, "conflict.txt", "base\n", 1);
        }
        if ours {
            add_index_stage(&mut index, "conflict.txt", "ours\n", 2);
        }
        if theirs {
            add_index_stage(&mut index, "conflict.txt", "theirs\n", 3);
        }
        index
            .save(path::index())
            .expect("failed to write unmerged index");

        let output = status_stdout(repo.path(), &["status", "--short"]);
        assert!(
            output
                .lines()
                .any(|line| line.starts_with(expected) && line.ends_with("conflict.txt")),
            "expected XY {expected} for base={base} ours={ours} theirs={theirs}, got:\n{output}"
        );
    }
}

#[test]
fn porcelain_v1_rename_output_stays_add_delete() {
    let staged = libra::command::status::Changes {
        new: vec!["b.txt".into()],
        modified: vec![],
        deleted: vec!["a.txt".into()],
        renamed: vec![],
    };
    let unstaged = libra::command::status::Changes::default();
    let mut output = Vec::new();

    libra::command::status::output_porcelain(&staged, &unstaged, false, &mut output)
        .expect("porcelain v1 output should succeed");

    let rendered = String::from_utf8(output).expect("porcelain v1 should be utf-8");
    assert_eq!(rendered, "D  a.txt\nA  b.txt\n");
}

// ── R0-2/R0-4: engine-backed rename detection, default-on (§B.4/§B.5) ─────────

/// A staged move of unchanged content is an exact rename, detected by default
/// (rename detection is ON without any flag, matching Git).
#[test]
fn rename_exact_staged_detected_by_default() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");

    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        out.contains("renamed:") && out.contains("a.txt") && out.contains("b.txt"),
        "default status should report the rename: {out}"
    );
    // The endpoints must NOT also appear as a separate delete + new file.
    assert!(
        !out.contains("deleted: a.txt") && !out.contains("new file: b.txt"),
        "rename endpoints must not double as add/delete: {out}"
    );
}

/// A staged move with a small content edit is still a rename (inexact,
/// spanhash similarity above the 50% default threshold).
#[test]
fn rename_inexact_content_change_detected() {
    let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let repo = create_repo_with_committed_file("orig.txt", &base);
    let mv = run_libra_command(&["mv", "orig.txt", "moved.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");
    // Edit one line of the moved file, then re-stage it.
    let edited = base.replace("line 5\n", "line five changed\n");
    fs::write(repo.path().join("moved.txt"), edited).unwrap();
    let add = run_libra_command(&["add", "moved.txt"], repo.path());
    assert_cli_success(&add, "restage edited moved file");

    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        out.contains("renamed:") && out.contains("orig.txt") && out.contains("moved.txt"),
        "inexact rename should still be detected: {out}"
    );
}

/// `--no-renames` disables detection, so the same move renders as a delete +
/// add pair.
#[test]
fn rename_no_renames_flag_splits_add_delete() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");

    let out = status_stdout(repo.path(), &["status", "--no-renames"]);
    assert!(
        out.contains("deleted:") && out.contains("a.txt") && out.contains("b.txt"),
        "--no-renames should split into delete + new file: {out}"
    );
    assert!(
        !out.contains("renamed:"),
        "--no-renames must not report a rename: {out}"
    );
}

/// `status.renames=false` disables detection through the config cascade,
/// even though the feature default is on (§B.5).
#[test]
fn rename_config_status_renames_false_disables() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    let cfg = run_libra_command(&["config", "status.renames", "false"], repo.path());
    assert_cli_success(&cfg, "set status.renames=false");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");

    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        !out.contains("renamed:") && out.contains("deleted:"),
        "status.renames=false should disable rename detection: {out}"
    );
}

/// A CLI `--find-renames` always wins over a config `status.renames=false`.
#[test]
fn rename_config_cli_find_renames_overrides_false() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    let cfg = run_libra_command(&["config", "status.renames", "false"], repo.path());
    assert_cli_success(&cfg, "set status.renames=false");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");

    let out = status_stdout(repo.path(), &["status", "--find-renames"]);
    assert!(
        out.contains("renamed:"),
        "--find-renames must override status.renames=false: {out}"
    );
}

/// `--short` renders a detected rename as a single Git-style `R  old -> new`
/// line, not two separate `R` rows (§B.6.1).
#[test]
fn rename_short_format_uses_arrow() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");

    // Force no color so the line is the plain `R  a.txt -> b.txt` form.
    let out = status_stdout(repo.path(), &["--no-color", "status", "--short"]);
    assert!(
        out.lines().any(|l| l.contains("a.txt -> b.txt")),
        "short rename should use the arrow form: {out}"
    );
    // The endpoints must not also appear as two separate `R` rows.
    assert!(
        !out.lines().any(|l| l.trim_end() == "R  a.txt"),
        "rename endpoints must not double as separate R rows: {out}"
    );
}

/// A tracked file that cannot be read (permission denied on its parent) must
/// NOT be reported as deleted — status fails closed instead (§B.6.0.1). This
/// prevents `commit -a` from recording a spurious removal.
#[test]
#[cfg(unix)]
fn tracked_unreadable_path_fails_closed_not_deleted() {
    use std::os::unix::fs::PermissionsExt;
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::create_dir(repo.path().join("locked")).unwrap();
    fs::write(repo.path().join("locked/secret.txt"), "top secret\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "locked/secret.txt"], repo.path()),
        "stage tracked file",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path()),
        "commit tracked file",
    );

    // Make the parent directory unreadable/untraversable so symlink_metadata
    // on the tracked file returns EACCES rather than NotFound.
    let dir = repo.path().join("locked");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();

    let output = run_libra_command(&["status"], repo.path());
    // Restore permissions before asserting so the tempdir can be cleaned up.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(
        !output.status.success(),
        "unreadable tracked path must fail closed, not succeed with a deletion"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LBR-IO-001") || stderr.contains("cannot read tracked path"),
        "fails closed with an IO error: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("deleted:") && !stdout.contains("secret.txt"),
        "must not report the unreadable file as deleted: {stdout}"
    );
}

/// `--porcelain=v2` emits Git's single `2 R<score> …\t<old>` rename record
/// (real HEAD/index modes and hashes), not two `1 R` change rows (§B.6.4).
#[test]
fn rename_porcelain_v2_emits_rename_record() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");

    let out = status_stdout(repo.path(), &["status", "--porcelain=v2"]);
    let line = out
        .lines()
        .find(|l| l.starts_with("2 "))
        .unwrap_or_else(|| panic!("expected a porcelain v2 rename record: {out}"));
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(fields[0], "2", "{line}");
    assert_eq!(fields[1], "R.", "staged rename xy: {line}");
    // Score field is `R<digits>` at index 8 (NOT the xy `R.` at index 1).
    let score = fields[8];
    assert!(
        score.starts_with('R') && score[1..].chars().all(|c| c.is_ascii_digit()),
        "score field is R<pct>: {line}"
    );
    assert_eq!(&score[1..], "100", "exact rename scores 100: {line}");
    // HEAD/index hashes must be real (non-zero) for an exact staged rename.
    assert_ne!(
        fields[6],
        "0".repeat(fields[6].len()),
        "hH non-zero: {line}"
    );
    assert_ne!(
        fields[7],
        "0".repeat(fields[7].len()),
        "hI non-zero: {line}"
    );
    // The record carries both paths (new\told); endpoints must not also
    // appear as separate `1 R` rows.
    assert!(line.contains("b.txt") && line.contains("a.txt"), "{line}");
    assert!(
        !out.lines().any(|l| l.starts_with("1 R")),
        "endpoints must not double as `1 R` rows: {out}"
    );
}

/// `--json` includes a top-level `renames[]` array with `score`, `exact`, and
/// side flags (§B.6.5).
#[test]
fn rename_json_includes_score_and_side() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");

    let out = status_stdout(repo.path(), &["--json", "status"]);
    let doc: serde_json::Value = serde_json::from_str(&out).expect("json status");
    let renames = doc["data"]["renames"]
        .as_array()
        .expect("renames array present");
    let entry = renames
        .iter()
        .find(|r| r["to"] == "b.txt")
        .unwrap_or_else(|| panic!("rename entry for b.txt: {out}"));
    assert_eq!(entry["from"], "a.txt");
    assert_eq!(entry["score"], 100);
    assert_eq!(entry["exact"], true);
    assert_eq!(entry["staged"], true);
    assert_eq!(entry["unstaged"], false);
}

/// Detection runs on repo-relative keys, so a rename is found even when
/// `status` is invoked from a subdirectory (the historical subdir bug).
#[test]
fn rename_from_subdirectory_detected() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::create_dir(repo.path().join("sub")).unwrap();
    fs::write(
        repo.path().join("sub/a.txt"),
        "subdir rename content\nsecond line here\n",
    )
    .unwrap();
    let add = run_libra_command(&["add", "sub/a.txt"], repo.path());
    assert_cli_success(&add, "stage subdir file");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit subdir file");
    let mv = run_libra_command(&["mv", "sub/a.txt", "sub/b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv in subdir");

    // Invoke status FROM the subdirectory.
    let out = status_stdout(&repo.path().join("sub"), &["status"]);
    assert!(
        out.contains("renamed:") && out.contains("a.txt") && out.contains("b.txt"),
        "rename must be detected from a subdirectory: {out}"
    );
}
