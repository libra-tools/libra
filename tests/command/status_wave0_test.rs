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

/// True when the host filesystem can create a file whose name is not valid
/// UTF-8. Linux filesystems accept arbitrary non-NUL bytes, but APFS
/// (macOS) rejects such names with EILSEQ, so the byte-level path fixtures
/// cannot exist there at all. Callers return early after this prints the
/// standard `skipped (...)` notice — the same convention as the gated
/// L2/L3 tests — instead of failing on hosts that cannot express the name.
#[cfg(unix)]
fn fs_supports_non_utf8_names(dir: &Path) -> bool {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let canary = dir.join(OsStr::from_bytes(b"non-utf8-\xff-canary"));
    match fs::write(&canary, b"") {
        Ok(()) => {
            let _ = fs::remove_file(&canary);
            true
        }
        Err(error) => {
            eprintln!("skipped (filesystem refuses non-UTF-8 file names: {error})");
            false
        }
    }
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
    // An unresolved conflict lives ONLY in its `u` record — the
    // stage-0-less index must not leak a duplicate `1 D.` row
    // (2026-08-06 R0-5 review).
    assert_eq!(
        output
            .lines()
            .filter(|line| (line.starts_with("1 ") || line.starts_with("2 "))
                && line.contains("conflict.txt"))
            .count(),
        0,
        "no ordinary record duplicates the conflict: {output}"
    );
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

/// `--porcelain` (v1) renders a detected rename as a single `R  old -> new`
/// record, not two `R` endpoint rows (§B.6.3).
#[test]
fn porcelain_v1_uses_rename_arrow_when_detected() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");

    let out = status_stdout(repo.path(), &["status", "--porcelain"]);
    assert!(
        out.lines().any(|l| l == "R  a.txt -> b.txt"),
        "porcelain v1 rename should be a single arrow record: {out:?}"
    );
    assert!(
        !out.lines().any(|l| l == "R  a.txt" || l == "R  b.txt"),
        "endpoints must not double as separate R rows: {out:?}"
    );
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
    // Exactly one change row, and it is the rename: a leaked `1 ` endpoint
    // row or a spurious `.R` would both slip past a first-`2`-line check.
    let change_lines: Vec<&str> = out
        .lines()
        .filter(|l| l.starts_with("1 ") || l.starts_with("2 "))
        .collect();
    assert_eq!(
        change_lines.len(),
        1,
        "the rename record is the only change row: {out}"
    );
    assert!(
        !out.lines()
            .any(|l| l.starts_with("2 ") && l.contains(" .R ")),
        "a staged rename must not also render an unstaged `.R`: {out}"
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

// ── §B.3.1: untracked paths as rename destinations (Libra extension) ─────────

/// Default (`status.renameUntracked` unset = false, Git parity): a chain of
/// staged `a→b` plus an unstaged worktree move `b→c` reports the staged
/// rename normally, but the second hop stays `D` + `??` — the untracked
/// destination is NOT consumed into an unstaged rename record.
#[test]
fn chain_rename_default_untracked_d_and_question() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv (staged hop)");
    // Unstaged second hop: move b.txt → c.txt purely in the worktree.
    let contents = fs::read(repo.path().join("b.txt")).expect("read staged move target");
    fs::remove_file(repo.path().join("b.txt")).expect("remove worktree b.txt");
    fs::write(repo.path().join("c.txt"), contents).expect("write untracked c.txt");

    let out = status_stdout(repo.path(), &["status", "--porcelain"]);
    // Git parity: the worktree deletion of the rename destination rides in
    // the Y column of the rename record (`RD`), not as a separate ` D` row.
    assert!(
        out.lines().any(|l| l == "RD a.txt -> b.txt"),
        "staged rename record carries the worktree delete as Y=D: {out:?}"
    );
    assert!(
        out.lines().any(|l| l == "?? c.txt"),
        "unstaged hop destination stays untracked: {out:?}"
    );
    assert!(
        !out.lines().any(|l| l.contains("b.txt -> c.txt")),
        "no unstaged rename record without status.renameUntracked: {out:?}"
    );

    // Porcelain v2: xy = RD and mW = 000000 (no worktree entry for the
    // deleted destination — must not fabricate 100644).
    let v2 = status_stdout(repo.path(), &["status", "--porcelain=v2"]);
    let record = v2
        .lines()
        .find(|l| l.starts_with("2 "))
        .unwrap_or_else(|| panic!("expected v2 rename record: {v2}"));
    let fields: Vec<&str> = record.split_whitespace().collect();
    assert_eq!(
        fields[1], "RD",
        "v2 xy carries the worktree delete: {record}"
    );
    assert_eq!(
        fields[5], "000000",
        "mW is zero for a deleted destination: {record}"
    );
    // Completeness of the DEFAULT v2 view, not just its first record: with
    // `status.renameUntracked` off, the chain collapses to exactly one `2 RD`
    // record — the second hop is untracked, so it must appear as `? c.txt`
    // and never as a `.R`. Reading only the first `2 ` line cannot see a
    // leaked endpoint row or an extra rename record.
    let v2_records: Vec<&str> = v2.lines().filter(|l| l.starts_with("2 ")).collect();
    assert_eq!(v2_records.len(), 1, "exactly one v2 rename record: {v2}");
    let v2_changes: Vec<&str> = v2
        .lines()
        .filter(|l| l.starts_with("1 ") || l.starts_with("2 "))
        .collect();
    assert_eq!(
        v2_changes.len(),
        1,
        "and it is the only change row — no leaked endpoints: {v2}"
    );
    assert!(
        !v2.lines()
            .any(|l| l.starts_with("2 ") && l.contains(" .R ")),
        "the extension is off, so no unstaged rename record: {v2}"
    );
    assert!(
        v2.lines().any(|l| l.trim() == "? c.txt"),
        "the chain's second hop stays untracked in v2: {v2}"
    );

    // `-z` v1: `RD SP <new> NUL <old> NUL` record shape.
    let z = run_libra_command(&["status", "--porcelain", "-z"], repo.path());
    assert!(z.status.success());
    let raw = String::from_utf8_lossy(&z.stdout);
    assert!(
        raw.contains("RD b.txt\0a.txt\0"),
        "-z record keeps the RD xy with NUL-separated new/old: {raw:?}"
    );
}

/// A staged rename whose destination is then modified in the worktree emits
/// a single `RM old -> new` record (§B.9 `staged_rename_then_modify_emits_rm`)
/// in short and porcelain v1, and `R.`→`RM` in the v2 xy field.
#[test]
fn staged_rename_then_modify_emits_rm() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");
    fs::write(
        repo.path().join("b.txt"),
        "hello rename world\ncontent line two\nworktree edit\n",
    )
    .expect("modify rename destination in worktree");

    let v1 = status_stdout(repo.path(), &["status", "--porcelain"]);
    assert!(
        v1.lines().any(|l| l == "RM a.txt -> b.txt"),
        "porcelain v1 merges the worktree edit into RM: {v1:?}"
    );
    let short = status_stdout(repo.path(), &["--no-color", "status", "--short"]);
    assert!(
        short.lines().any(|l| l.starts_with("RM ")),
        "short format carries Y=M on the rename line: {short:?}"
    );
    let v2 = status_stdout(repo.path(), &["status", "--porcelain=v2"]);
    let record = v2
        .lines()
        .find(|l| l.starts_with("2 "))
        .unwrap_or_else(|| panic!("expected v2 rename record: {v2}"));
    assert!(
        record.split_whitespace().nth(1) == Some("RM"),
        "v2 xy is RM when the destination has a worktree edit: {record}"
    );
    // The pairing must be the STAGED HEAD↔index one: exact, score 100. A
    // HEAD↔worktree implementation would also render `RM` here, but would
    // score the edited worktree bytes inexactly — so the rendered code alone
    // cannot tell the two apart.
    let json = status_stdout(repo.path(), &["--json", "status"]);
    let doc: serde_json::Value = serde_json::from_str(&json).expect("json status");
    let renames = doc["data"]["renames"].as_array().expect("renames");
    assert_eq!(renames.len(), 1, "one rename record: {json}");
    assert_eq!(
        renames[0]["exact"], true,
        "staged side is HEAD↔index: {json}"
    );
    assert_eq!(renames[0]["score"], 100, "and therefore exact: {json}");
    assert_eq!(renames[0]["staged"], true, "{json}");
    assert_eq!(renames[0]["unstaged"], false, "{json}");
    // The worktree edit is still reported, separately.
    assert!(
        doc["data"]["unstaged"]["modified"]
            .as_array()
            .is_some_and(|m| !m.is_empty()),
        "the worktree modification survives as its own entry: {json}"
    );
}

/// `status.renameUntracked` is a strict-bool config cascade: enabling it (in
/// either scope, local overriding global) lets the same worktree move pair
/// into an unstaged rename, and an invalid value fails closed before output.
#[test]
fn rename_untracked_config_cascade() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    fs::remove_file(repo.path().join("a.txt")).expect("remove worktree a.txt");
    fs::write(
        repo.path().join("moved.txt"),
        "hello rename world\ncontent line two\n",
    )
    .expect("write untracked moved.txt");

    // Default off: D + ??.
    let off = status_stdout(repo.path(), &["status", "--porcelain"]);
    assert!(
        off.lines().any(|l| l == " D a.txt") && off.lines().any(|l| l == "?? moved.txt"),
        "default keeps D + ??: {off:?}"
    );

    // Global scope enables it (numeric Git boolean exercises strict parsing).
    let global = run_libra_command(
        &["config", "set", "--global", "status.renameUntracked", "1"],
        repo.path(),
    );
    assert_cli_success(&global, "set global status.renameUntracked=1");
    let on = status_stdout(repo.path(), &["status", "--porcelain"]);
    assert!(
        on.lines().any(|l| l == " R a.txt -> moved.txt"),
        "global true enables the unstaged rename pair: {on:?}"
    );

    // Local false overrides global true (cascade order).
    let local = run_libra_command(
        &["config", "set", "status.renameUntracked", "false"],
        repo.path(),
    );
    assert_cli_success(&local, "set local status.renameUntracked=false");
    let overridden = status_stdout(repo.path(), &["status", "--porcelain"]);
    assert!(
        overridden.lines().any(|l| l == " D a.txt"),
        "local false wins over global true: {overridden:?}"
    );

    // Invalid value fails closed before any status output.
    let invalid = run_libra_command(
        &["config", "set", "status.renameUntracked", "sideways"],
        repo.path(),
    );
    assert_cli_success(&invalid, "store invalid value");
    let failed = run_libra_command(&["status", "--porcelain"], repo.path());
    assert!(
        !failed.status.success(),
        "invalid status.renameUntracked must fail closed"
    );
    assert!(
        failed.stdout.is_empty(),
        "no partial porcelain before the config failure: {:?}",
        String::from_utf8_lossy(&failed.stdout)
    );
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
        stderr.contains("status.renameUntracked"),
        "diagnostic names the offending key: {stderr}"
    );
}

/// Per-end pathspec semantics (§B.3): a rename record survives only when
/// BOTH endpoints match the pathspec. An old-only match reports the in-scope
/// deletion, a new-only match the in-scope addition — an out-of-scope
/// endpoint never leaks through a rename record (default staged path).
#[test]
fn pathspec_old_only_new_only_matrix() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");

    // Old endpoint only: the deletion is in scope, the rename is not.
    let old_only = status_stdout(repo.path(), &["status", "--porcelain=v1", "a.txt"]);
    assert!(
        old_only.lines().any(|l| l == "D  a.txt"),
        "old-only pathspec demotes to a deletion: {old_only:?}"
    );
    assert!(
        !old_only.contains("b.txt"),
        "out-of-scope destination must not leak: {old_only:?}"
    );

    // New endpoint only: the addition is in scope, the rename is not.
    let new_only = status_stdout(repo.path(), &["status", "--porcelain=v1", "b.txt"]);
    assert!(
        new_only.lines().any(|l| l == "A  b.txt"),
        "new-only pathspec demotes to an addition: {new_only:?}"
    );
    assert!(
        !new_only.contains("a.txt"),
        "out-of-scope source must not leak: {new_only:?}"
    );

    // Both endpoints in scope: the rename record is kept.
    let both = status_stdout(repo.path(), &["status", "--porcelain=v1", "a.txt", "b.txt"]);
    assert!(
        both.lines().any(|l| l == "R  a.txt -> b.txt"),
        "both endpoints in scope keep the rename record: {both:?}"
    );
}

/// A staged rename whose destination is then DELETED in the worktree emits a
/// single `RD old -> new` record in short and porcelain v1, and `RD` with
/// `mW=000000` in v2 (§B.9 `staged_rename_then_delete_emits_rd`).
#[test]
fn staged_rename_then_delete_emits_rd() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");
    fs::remove_file(repo.path().join("b.txt")).expect("delete rename destination in worktree");

    let short = status_stdout(repo.path(), &["--no-color", "status", "--short"]);
    assert!(
        short.lines().any(|l| l == "RD a.txt -> b.txt"),
        "short format carries Y=D on the rename line: {short:?}"
    );
    let v1 = status_stdout(repo.path(), &["status", "--porcelain"]);
    assert!(
        v1.lines().any(|l| l == "RD a.txt -> b.txt"),
        "porcelain v1 merges the worktree delete into RD: {v1:?}"
    );
    let v2 = status_stdout(repo.path(), &["status", "--porcelain=v2"]);
    let record = v2
        .lines()
        .find(|l| l.starts_with("2 "))
        .unwrap_or_else(|| panic!("expected v2 rename record: {v2}"));
    let fields: Vec<&str> = record.split_whitespace().collect();
    assert_eq!(
        fields[1], "RD",
        "v2 xy carries the worktree delete: {record}"
    );
    assert_eq!(
        fields[5], "000000",
        "mW is zero for a deleted destination: {record}"
    );
}

// ── R0-8a: structured warnings + 9≻1 exit arbitration (§B.5) ────────────────

/// Serialization names of the warning schema are pinned (§B.5): `code` and
/// `source` serialize in snake_case exactly as documented.
#[test]
fn json_warnings_schema_snapshot() {
    let warning = libra::command::status::StatusWarning {
        code: libra::command::status::StatusWarningCode::RenameLimitProductSkipped,
        message: "m".to_string(),
        source: libra::command::status::StatusWarningSource::RenameDetect,
    };
    let value = serde_json::to_value(&warning).expect("serialize warning");
    assert_eq!(
        value,
        serde_json::json!({
            "code": "rename_limit_product_skipped",
            "message": "m",
            "source": "rename_detect",
        }),
        "full object shape is pinned"
    );
    let budget =
        serde_json::to_value(libra::command::status::StatusWarningCode::SimilarityBudgetExceeded)
            .expect("serialize code");
    assert_eq!(budget, "similarity_budget_exceeded");
    for (code, name) in [
        (
            libra::command::status::StatusWarningCode::ProbeTruncated,
            "probe_truncated",
        ),
        (
            libra::command::status::StatusWarningCode::MetadataUnavailable,
            "metadata_unavailable",
        ),
        (
            libra::command::status::StatusWarningCode::MetadataBudgetExceeded,
            "metadata_budget_exceeded",
        ),
        (
            libra::command::status::StatusWarningCode::WorktreeBudgetExceeded,
            "worktree_budget_exceeded",
        ),
        (
            libra::command::status::StatusWarningCode::WorktreeReadFailed,
            "worktree_read_failed",
        ),
        (
            libra::command::status::StatusWarningCode::WorktreePermissionDenied,
            "worktree_permission_denied",
        ),
        (
            libra::command::status::StatusWarningCode::WorktreeIoTimeout,
            "worktree_io_timeout",
        ),
        (
            libra::command::status::StatusWarningCode::RenamePathEncodingUnsupported,
            "rename_path_encoding_unsupported",
        ),
        (
            libra::command::status::StatusWarningCode::DirtyCacheLockStolen,
            "dirty_cache_lock_stolen",
        ),
        (
            libra::command::status::StatusWarningCode::DirtyCacheStaleFallback,
            "dirty_cache_stale_fallback",
        ),
        (
            libra::command::status::StatusWarningCode::DirtyCacheConcurrentInvalidate,
            "dirty_cache_concurrent_invalidate",
        ),
        (
            libra::command::status::StatusWarningCode::DirtyCachePathUnencodable,
            "dirty_cache_path_unencodable",
        ),
        (
            libra::command::status::StatusWarningCode::RepositoryPreflight,
            "repository_preflight",
        ),
    ] {
        assert_eq!(serde_json::to_value(code).expect("serialize code"), name);
    }
    // The code list above must be the COMPLETE frozen set: a new code
    // without a pinned wire name here would be free to change spelling.
    assert_eq!(
        libra::command::status::StatusWarningCode::ALL.len(),
        15,
        "a warning code was added or removed; pin its wire name in this snapshot"
    );

    // The numeric discriminants are public surface too (a fieldless public
    // enum can be cast by embedders): frozen to the original pre-macro
    // declaration mapping — a swapped or mistyped explicit value compiles
    // but fails here (2026-08-06 R0-7 review).
    let discriminants: Vec<(String, u32)> = libra::command::status::StatusWarningCode::ALL
        .iter()
        .map(|code| {
            (
                serde_json::to_value(code)
                    .expect("code")
                    .as_str()
                    .expect("name")
                    .to_string(),
                *code as u32,
            )
        })
        .collect();
    for (name, expected) in [
        ("similarity_budget_exceeded", 0u32),
        ("rename_limit_product_skipped", 1),
        ("probe_truncated", 2),
        ("metadata_unavailable", 3),
        ("metadata_budget_exceeded", 4),
        ("worktree_budget_exceeded", 5),
        ("worktree_read_failed", 6),
        ("worktree_permission_denied", 7),
        ("worktree_io_timeout", 8),
        ("rename_path_encoding_unsupported", 9),
        ("dirty_cache_lock_stolen", 10),
        ("dirty_cache_stale_fallback", 11),
        ("dirty_cache_concurrent_invalidate", 12),
        ("dirty_cache_path_unencodable", 13),
        ("repository_preflight", 14),
    ] {
        assert!(
            discriminants
                .iter()
                .any(|(wire, value)| wire == name && *value == expected),
            "frozen discriminant for {name} must be {expected}: {discriminants:?}"
        );
    }
    for (source, expected) in [
        (libra::command::status::StatusWarningSource::Config, 0u32),
        (libra::command::status::StatusWarningSource::Probe, 1),
        (libra::command::status::StatusWarningSource::RenameDetect, 2),
        (libra::command::status::StatusWarningSource::Cache, 3),
        (libra::command::status::StatusWarningSource::Metadata, 4),
        (libra::command::status::StatusWarningSource::Worktree, 5),
    ] {
        assert_eq!(
            source as u32, expected,
            "frozen source discriminant for {source:?}"
        );
    }

    // The full §B.5 source enum is frozen public schema — consumers switch
    // on it to tell "we may not have seen every candidate" (probe) apart
    // from "we saw them but could not score them" (rename_detect), and a
    // worktree block apart from an object-store block. `Source::ALL` is
    // generated from the enum declaration itself (a variant cannot exist
    // outside it), so EXACT set equality here freezes the wire names with
    // structural completeness (2026-08-06 R0-7 review).
    let serialized: std::collections::BTreeSet<String> =
        libra::command::status::StatusWarningSource::ALL
            .iter()
            .map(|source| {
                serde_json::to_value(source)
                    .expect("src")
                    .as_str()
                    .expect("string wire name")
                    .to_string()
            })
            .collect();
    let expected: std::collections::BTreeSet<String> = [
        "config",
        "probe",
        "rename_detect",
        "cache",
        "metadata",
        "worktree",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(
        serialized, expected,
        "the §B.5 source wire-name set is frozen; a new variant needs a \
         deliberate snapshot update"
    );
}

/// Exceeding the per-side rename limit (1000) degrades the inexact pass with
/// a structured warning; with `--exit-code-on-warning` the warning exit 9
/// beats the `--exit-code` dirty exit 1 in text AND JSON modes, and JSON
/// carries the warning in `data.warnings[]` with no stderr line.
#[test]
fn rename_limit_warning_exit_nine_over_dirty() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    for i in 0..1001 {
        fs::write(repo.path().join(format!("f{i}.txt")), format!("base {i}\n")).unwrap();
    }
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage base files");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit base files");
    for i in 0..1001 {
        fs::remove_file(repo.path().join(format!("f{i}.txt"))).unwrap();
        fs::write(
            repo.path().join(format!("g{i}.txt")),
            format!("completely different payload {i}\n"),
        )
        .unwrap();
    }
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage mass rename");

    // Text mode: warning on stderr; exit 9 beats dirty 1.
    let text = run_libra_command(
        &["--exit-code-on-warning", "status", "--exit-code"],
        repo.path(),
    );
    assert_eq!(
        text.status.code(),
        Some(9),
        "text: warning exit 9 over dirty 1"
    );
    let stderr = String::from_utf8_lossy(&text.stderr);
    assert!(
        stderr.contains("warning:") && stderr.contains("renameLimit"),
        "text stderr carries the structured warning: {stderr}"
    );
    // The FULL message is the public degrade contract (R0-1): only the
    // exhaustive inexact stage is skipped — exact and scored unique-basename
    // pairs survive — never "inexact matching" as a whole.
    assert!(
        stderr.contains(
            "rename detection skipped the exhaustive inexact pass: too many candidates on one side (renameLimit); exact and unique-basename matches were kept"
        ),
        "text warning pins the complete survivor semantics: {stderr}"
    );
    assert!(
        !stderr.contains("skipped inexact matching"),
        "must not claim the whole inexact match was skipped: {stderr}"
    );

    // JSON mode: warnings ride in data.warnings[], stderr stays clean, and
    // the same silent exit 9 wins.
    let json = run_libra_command(
        &["--json", "--exit-code-on-warning", "status", "--exit-code"],
        repo.path(),
    );
    assert_eq!(
        json.status.code(),
        Some(9),
        "json: warning exit 9 over dirty 1"
    );
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&json.stdout)).expect("json envelope");
    let warnings = doc["data"]["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w["code"] == "rename_limit_product_skipped"),
        "json data.warnings carries the code: {doc}"
    );
    let limit_warning = warnings
        .iter()
        .find(|w| w["code"] == "rename_limit_product_skipped")
        .expect("rename_limit_product_skipped warning");
    let limit_message = limit_warning["message"].as_str().expect("message str");
    assert_eq!(
        limit_message,
        "rename detection skipped the exhaustive inexact pass: too many candidates on one side (renameLimit); exact and unique-basename matches were kept",
        "json warning message pins the complete survivor semantics"
    );
    assert!(
        json.stderr.is_empty(),
        "json mode keeps stderr completely empty: {:?}",
        String::from_utf8_lossy(&json.stderr)
    );

    // Without --exit-code-on-warning the dirty exit 1 still applies.
    let plain = run_libra_command(&["status", "--exit-code"], repo.path());
    assert_eq!(
        plain.status.code(),
        Some(1),
        "dirty exit stays 1 without on-warning"
    );

    // --quiet suppresses the body but never the diagnostics; 9 still wins.
    let quiet = run_libra_command(
        &["--quiet", "--exit-code-on-warning", "status", "--exit-code"],
        repo.path(),
    );
    assert_eq!(quiet.status.code(), Some(9), "quiet: warning exit 9");
    assert!(
        String::from_utf8_lossy(&quiet.stderr).contains("warning:"),
        "quiet keeps stderr diagnostics"
    );

    // --scan runs the same collection: delivery + arbitration hold there too.
    let scan = run_libra_command(
        &["--exit-code-on-warning", "status", "--scan", "--exit-code"],
        repo.path(),
    );
    assert_eq!(scan.status.code(), Some(9), "scan path: warning exit 9");
    assert!(
        String::from_utf8_lossy(&scan.stderr).contains("warning:"),
        "scan path delivers stderr warnings"
    );
}

/// Exceeding the similarity-comparison budget surfaces
/// `similarity_budget_exceeded` end to end: 750 deleted paths sharing one
/// blob OID × 750 added paths sharing another are only TWO object reads
/// (per-OID caching) but 562k inexact comparisons > the 500k budget.
#[test]
fn similarity_budget_warning() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    for i in 0..750 {
        fs::write(
            repo.path().join(format!("old{i}.txt")),
            "shared alpha payload with enough content to hash\n",
        )
        .unwrap();
    }
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage base");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit base");
    for i in 0..750 {
        fs::remove_file(repo.path().join(format!("old{i}.txt"))).unwrap();
        fs::write(
            repo.path().join(format!("zz{i}.dat")),
            "shared omega payload with enough content to hash\n",
        )
        .unwrap();
    }
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage churn");

    let json = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&json, "json status");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&json.stdout)).expect("envelope");
    assert!(
        doc["data"]["warnings"]
            .as_array()
            .expect("warnings array")
            .iter()
            .any(|w| w["code"] == "similarity_budget_exceeded"
                && w["message"].as_str().is_some_and(|message| message
                    .contains("exact and already-scored unique-basename matches were kept"))),
        "budget exhaustion surfaces end to end with survivor semantics: {doc}"
    );
}

// ── R0-4 (first slice): bare -z/--null format arbitration ───────────────────

/// Bare `-z` with no explicit format forces porcelain v1 + NUL records
/// (§B.6): machine intent, not a NUL-terminated human format.
#[test]
fn bare_z_emits_porcelain_v1() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    fs::write(repo.path().join("b.txt"), "untracked\n").unwrap();
    let out = run_libra_command(&["status", "-z"], repo.path());
    assert_cli_success(&out, "bare -z status");
    let raw = String::from_utf8_lossy(&out.stdout);
    assert!(
        raw.contains("?? b.txt\0"),
        "porcelain v1 NUL records: {raw:?}"
    );
    assert!(
        !raw.contains("Untracked files"),
        "no human sections under bare -z: {raw:?}"
    );
}

/// The `st` alias behaves identically for bare `-z`.
#[test]
fn st_bare_z_emits_porcelain_v1() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    fs::write(repo.path().join("b.txt"), "untracked\n").unwrap();
    let out = run_libra_command(&["st", "-z"], repo.path());
    assert_cli_success(&out, "st -z");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("?? b.txt\0"),
        "alias parity"
    );
}

/// `--null` is the Git-parity long alias of `-z`, with the same bare-format
/// forcing; combining it with `--long` or `--cached` fails closed.
#[test]
fn bare_null_emits_porcelain_v1() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    fs::write(repo.path().join("b.txt"), "untracked\n").unwrap();
    let out = run_libra_command(&["status", "--null"], repo.path());
    assert_cli_success(&out, "bare --null status");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("?? b.txt\0"),
        "--null ≡ bare -z"
    );
    let conflict = run_libra_command(&["status", "--long", "--null"], repo.path());
    assert!(!conflict.status.success(), "--long --null fails closed");
    let cached = run_libra_command(&["status", "--cached", "-z"], repo.path());
    assert!(!cached.status.success(), "--cached -z fails closed");
    let scan = run_libra_command(&["status", "--scan", "-z"], repo.path());
    assert!(!scan.status.success(), "--scan -z fails closed");
}

// ── R0-4: Git raw --find-renames grammar + three-way last-wins ──────────────

/// Git raw score grammar reaches status via the argv resolution, verified
/// with an INEXACT rename (~70% similar) so the threshold actually decides:
/// `=505` (50.5%) pairs it, `=80%` splits it, and the three spellings obey
/// last-one-wins including `--renames`.
#[test]
fn find_renames_raw_grammar_and_last_wins() {
    let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let repo = create_repo_with_committed_file("orig.txt", &base);
    // ~30% of lines changed → similarity ≈ 70%: between 50.5% and 80%.
    let mut edited = base.clone();
    for i in 0..12 {
        edited = edited.replace(&format!("line {i}\n"), &format!("edited {i}\n"));
    }
    let mv = run_libra_command(&["mv", "orig.txt", "moved.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");
    fs::write(repo.path().join("moved.txt"), edited).unwrap();
    let add = run_libra_command(&["add", "moved.txt"], repo.path());
    assert_cli_success(&add, "restage edited move");

    let arrow = "R  orig.txt -> moved.txt";
    let run = |args: &[&str]| {
        let out = run_libra_command(args, repo.path());
        assert_cli_success(&out, "status run");
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    // Raw integer 505 = 50.5%: pairs the ~70% rename.
    assert!(
        run(&["status", "--porcelain=v1", "--find-renames=505"]).contains(arrow),
        "=505 (50.5%) pairs a ~70% rename"
    );
    // 80% literal percent: does NOT pair → threshold is really applied.
    let strict = run(&["status", "--porcelain=v1", "--find-renames=80%"]);
    assert!(
        !strict.contains("->") && strict.contains("orig.txt"),
        "=80% splits the ~70% rename: {strict:?}"
    );
    // Decimal form equivalence.
    assert!(
        run(&["status", "--porcelain=v1", "--find-renames=0.505"]).contains(arrow),
        "decimal raw form works"
    );
    // Git raw grammar reads a bare integer as `0.<digits>`: `=50000` is
    // 0.50000 = 50%, so the ~70% rename still pairs; the exact-only `100%`
    // literal splits it (card-designated raw forms).
    assert!(
        run(&["status", "--porcelain=v1", "--find-renames=50000"]).contains(arrow),
        "=50000 parses as the 50% raw form"
    );
    assert!(
        !run(&["status", "--porcelain=v1", "--find-renames=100%"]).contains("->"),
        "=100% exact-only splits an inexact rename"
    );
    // Three-way last-wins including --renames: strict 80% is overridden by a
    // later --renames (50% default) → pairs again.
    assert!(
        run(&[
            "status",
            "--porcelain=v1",
            "--find-renames=80%",
            "--renames"
        ])
        .contains(arrow),
        "--renames after a strict threshold rewinds to the 50% default"
    );
    // Disable last vs find last.
    let disabled = run(&[
        "status",
        "--porcelain=v1",
        "--find-renames=505",
        "--no-renames",
    ]);
    assert!(!disabled.contains("->"), "--no-renames last disables");
    assert!(
        run(&[
            "status",
            "--porcelain=v1",
            "--no-renames",
            "--find-renames=505"
        ])
        .contains(arrow),
        "--find-renames after --no-renames re-enables at 50.5%"
    );
    // An optional-value GLOBAL before the subcommand must not shift the
    // normalizer's status detection: raw grammar still works under --json.
    let global = run_libra_command(&["--json", "status", "--find-renames=505"], repo.path());
    assert_cli_success(&global, "--json + raw grammar");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&global.stdout)).expect("envelope");
    assert!(
        doc["data"]["renames"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "raw threshold applied under a global optional-value flag: {doc}"
    );

    // Invalid raw LAST fails closed; invalid overridden by later valid wins.
    let bad_last = run_libra_command(
        &["status", "--find-renames=505", "--find-renames=foo"],
        repo.path(),
    );
    assert!(!bad_last.status.success(), "invalid last raw fails closed");
    assert!(
        run(&[
            "status",
            "--porcelain=v1",
            "--find-renames=foo",
            "--find-renames=505"
        ])
        .contains(arrow),
        "invalid overridden by later valid wins"
    );
}

/// `status`/`st` as another command's argument is never rewritten. The pin:
/// diff's own bare `--find-renames` EATS the next token as its value (clap
/// `num_args=0..=1`), so `libra diff --find-renames status` must fail with
/// diff's invalid-value error — if the normalizer had rewritten the bare
/// flag to a placeholder, `status` would have survived as a pathspec and the
/// command would behave differently.
#[test]
fn normalize_ignores_diff_pathspec_status() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\ncontent line two\n");
    let eaten = run_libra_command(&["diff", "--find-renames", "status"], repo.path());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&eaten.stdout),
        String::from_utf8_lossy(&eaten.stderr)
    );
    assert!(
        !eaten.status.success() && combined.contains("status"),
        "diff still eats 'status' as its --find-renames value (no rewrite): {combined}"
    );
    // Same for the `st` spelling.
    let st = run_libra_command(&["diff", "--find-renames", "st"], repo.path());
    assert!(
        !st.status.success(),
        "'st' after diff's bare --find-renames stays its (invalid) value"
    );
    // And in status itself the SAME shape succeeds because the placeholder
    // rewrite protects the pathspec.
    let ours = run_libra_command(&["status", "--find-renames", "a.txt"], repo.path());
    assert_cli_success(&ours, "status bare --find-renames keeps the pathspec");
}

// ── R0-2 residuals: designated snapshot/evidence tests ───────────────────────

/// A repository with no HEAD commit (unborn branch) must not crash rename
/// detection: everything staged is a plain `new file`, the JSON `renames[]`
/// array is empty, and the run succeeds.
#[test]
fn no_head_staged_rename() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::write(repo.path().join("fresh.txt"), "no head yet\n").unwrap();
    let add = run_libra_command(&["add", "fresh.txt"], repo.path());
    assert_cli_success(&add, "stage file on unborn HEAD");

    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        out.contains("new file") && out.contains("fresh.txt") && !out.contains("renamed:"),
        "unborn-HEAD staging must render as a plain add: {out}"
    );

    let json = status_stdout(repo.path(), &["--json", "status"]);
    let doc: serde_json::Value = serde_json::from_str(&json).expect("json status");
    assert_eq!(
        doc["data"]["renames"].as_array().map(Vec::len),
        Some(0),
        "no rename entries can exist without a HEAD side: {json}"
    );
}

/// §B.5 delivery matrix: JSON paths are repository-root-relative even when
/// `status` runs from a subdirectory, and the score/exactness lookup still
/// resolves (no silent 100/exact fallback).
#[test]
fn json_repo_relative_from_subdir() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::create_dir(repo.path().join("sub")).unwrap();
    let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
    fs::write(repo.path().join("sub/a.txt"), &base).unwrap();
    let add = run_libra_command(&["add", "sub/a.txt"], repo.path());
    assert_cli_success(&add, "stage subdir file");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit subdir file");
    let mv = run_libra_command(&["mv", "sub/a.txt", "sub/b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv in subdir");
    // Make the rename inexact so a details-map miss (which would fall back
    // to score=100/exact=true) is detectable.
    let edited = base.replace("line 5\n", "line five changed\n");
    fs::write(repo.path().join("sub/b.txt"), edited).unwrap();
    let add = run_libra_command(&["add", "sub/b.txt"], repo.path());
    assert_cli_success(&add, "restage edited moved file");

    let out = status_stdout(&repo.path().join("sub"), &["--json", "status"]);
    let doc: serde_json::Value = serde_json::from_str(&out).expect("json status");
    let renames = doc["data"]["renames"]
        .as_array()
        .expect("renames array present");
    let entry = renames
        .iter()
        .find(|r| r["to"] == "sub/b.txt")
        .unwrap_or_else(|| panic!("rename entry must be repo-relative: {out}"));
    assert_eq!(entry["from"], "sub/a.txt", "from is repo-relative: {out}");
    assert_eq!(
        entry["exact"], false,
        "edited rename must be inexact: {out}"
    );
    let score = entry["score"].as_u64().expect("score");
    assert!(
        (50..100).contains(&score),
        "inexact score must come from the real details map, got {score}: {out}"
    );
    // The staged change set is repo-relative too.
    let staged = doc["data"]["staged"]["renamed"]
        .as_array()
        .expect("staged renamed");
    assert!(
        staged
            .iter()
            .any(|p| p["from"] == "sub/a.txt" && p["to"] == "sub/b.txt"),
        "staged.renamed must be repo-relative: {out}"
    );
}

/// §B.4.1 empty-file rule end-to-end: an empty file pairs EXACT (same OID)
/// on a pure move, but never joins inexact scoring — an empty source plus a
/// non-empty replacement stays a delete + add.
#[test]
fn rename_empty_file_exact_pair_only() {
    // Pure move of an empty file: exact rename.
    let repo = create_repo_with_committed_file("empty.txt", "");
    let mv = run_libra_command(&["mv", "empty.txt", "renamed-empty.txt"], repo.path());
    assert_cli_success(&mv, "mv empty file");
    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        out.contains("renamed:") && out.contains("renamed-empty.txt"),
        "empty-file pure move must pair exactly by OID: {out}"
    );

    // Empty source + non-empty destination: no inexact pairing.
    let repo = create_repo_with_committed_file("empty.txt", "");
    let rm = run_libra_command(&["rm", "empty.txt"], repo.path());
    assert_cli_success(&rm, "delete empty file");
    fs::write(repo.path().join("full.txt"), "now with content\n").unwrap();
    let add = run_libra_command(&["add", "full.txt"], repo.path());
    assert_cli_success(&add, "stage replacement");
    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        !out.contains("renamed:"),
        "an empty file must never pair inexactly: {out}"
    );
    assert!(
        out.contains("deleted:") && out.contains("new file"),
        "the endpoints stay a delete + add: {out}"
    );
}

/// GC-02 hash-kind neutrality: rename detection (exact and inexact) works
/// identically in a SHA-256 repository.
#[test]
fn rename_sha256_repo_detected() {
    let repo = tempdir().expect("temp repo");
    fs::create_dir_all(repo.path()).unwrap();
    let init = run_libra_command(&["init", "--object-format", "sha256"], repo.path());
    assert_cli_success(&init, "init sha256 repo");
    configure_identity_via_cli(repo.path());
    let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
    fs::write(repo.path().join("wide.txt"), &base).unwrap();
    let add = run_libra_command(&["add", "wide.txt"], repo.path());
    assert_cli_success(&add, "stage sha256 fixture");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit sha256 fixture");

    // Exact rename.
    let mv = run_libra_command(&["mv", "wide.txt", "moved.txt"], repo.path());
    assert_cli_success(&mv, "mv in sha256 repo");
    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        out.contains("renamed:") && out.contains("moved.txt"),
        "sha256 exact rename must be detected: {out}"
    );

    // Inexact rename after a small edit.
    let edited = base.replace("line 5\n", "line five changed\n");
    fs::write(repo.path().join("moved.txt"), edited).unwrap();
    let add = run_libra_command(&["add", "moved.txt"], repo.path());
    assert_cli_success(&add, "restage edited sha256 file");
    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        out.contains("renamed:") && out.contains("moved.txt"),
        "sha256 inexact rename must be detected: {out}"
    );
}

// ── R0-5 residuals: porcelain v2 rename record field pins (§B.6.4) ───────────

/// Parse the first `2 …` record of a porcelain v2 dump into whitespace
/// fields (paths carry no spaces in these fixtures, so `new`/`old` land at
/// indices 9/10).
fn first_v2_record(out: &str) -> Vec<String> {
    let line = out
        .lines()
        .find(|l| l.starts_with("2 "))
        .unwrap_or_else(|| panic!("expected a porcelain v2 rename record: {out}"));
    line.split_whitespace().map(str::to_string).collect()
}

/// Staged inexact rename with a mode flip: every metadata field of the
/// `2 R.` record is pinned against fixture-known modes and OIDs
/// (§B.6.4 field order `2 <xy> <sub> <mH> <mI> <mW> <hH> <hI> R<pct>
/// <new>\t<old>`).
#[test]
fn porcelain_v2_staged_rename_mode_hash_fields() {
    let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let repo = create_repo_with_committed_file("orig.txt", &base);
    let head_oid = {
        let out = run_libra_command(&["rev-parse", "HEAD:orig.txt"], repo.path());
        assert_cli_success(&out, "rev-parse HEAD:orig.txt");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };

    let mv = run_libra_command(&["mv", "orig.txt", "moved.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");
    let edited = base.replace("line 5\n", "line five changed\n");
    fs::write(repo.path().join("moved.txt"), &edited).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            repo.path().join("moved.txt"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let add = run_libra_command(&["add", "moved.txt"], repo.path());
    assert_cli_success(&add, "restage edited moved file");
    // Fixture truth for the index side from ls-files --stage.
    let stage = {
        let out = run_libra_command(&["ls-files", "--stage"], repo.path());
        assert_cli_success(&out, "ls-files --stage");
        String::from_utf8(out.stdout).unwrap()
    };
    let stage_line = stage
        .lines()
        .find(|l| l.ends_with("moved.txt"))
        .unwrap_or_else(|| panic!("staged entry for moved.txt: {stage}"));
    let stage_fields: Vec<&str> = stage_line.split_whitespace().collect();
    let (index_mode, index_oid) = (stage_fields[0], stage_fields[1]);

    let out = status_stdout(repo.path(), &["status", "--porcelain=v2"]);
    let fields = first_v2_record(&out);
    assert_eq!(fields[1], "R.", "staged-only rename xy: {out}");
    assert_eq!(fields[2], "N...", "ordinary entry sub field: {out}");
    // Pin the FIXTURE first: if `add` did not actually record the 0o755
    // flip, every mode assertion below would compare 100644 to 100644 and
    // pass while proving nothing about the mode fields.
    #[cfg(unix)]
    {
        assert_eq!(
            index_mode, "100755",
            "the fixture must really have flipped the executable bit: {stage}"
        );
        assert_ne!(
            index_mode, "100644",
            "mH and mI must differ or the mode columns are untested: {stage}"
        );
    }
    assert_eq!(fields[3], "100644", "mH is the committed mode: {out}");
    assert_eq!(fields[4], index_mode, "mI matches ls-files --stage: {out}");
    assert_eq!(fields[5], index_mode, "mW matches the on-disk mode: {out}");
    #[cfg(unix)]
    assert_ne!(fields[3], fields[4], "the mode flip is visible: {out}");
    assert_eq!(fields[6], head_oid, "hH is the HEAD blob: {out}");
    assert_eq!(fields[7], index_oid, "hI matches ls-files --stage: {out}");
    assert_ne!(fields[6], fields[7], "content edit keeps hH != hI: {out}");
    let pct: u32 = fields[8]
        .strip_prefix('R')
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("score field R<pct>: {out}"));
    assert!((50..100).contains(&pct), "inexact score in range: {out}");
    assert_eq!(fields[9], "moved.txt", "new path first: {out}");
    assert_eq!(fields[10], "orig.txt", "old path second: {out}");
}

/// Unstaged-only rename (`.R`, `status.renameUntracked=true`): Git copies the
/// index fields into the HEAD columns — hH == hI == the REAL index OID and
/// mH == mI == the real mode; the all-zero fallback must fail this test.
#[test]
fn porcelain_v2_unstaged_dot_r_hash_fixup() {
    // Executable from the START: chmod-then-re-add does not change the
    // recorded mode, because the content is unchanged and `add` sees a clean
    // entry. Committing it executable gives the mode columns a value that
    // differs from the 100644 default a hardcoding bug would emit.
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::write(
        repo.path().join("a.txt"),
        "hash fixup content\nsecond line\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(repo.path().join("a.txt"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let add = run_libra_command(&["add", "a.txt"], repo.path());
    assert_cli_success(&add, "stage the executable fixture");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit the fixture");
    let cfg = run_libra_command(&["config", "status.renameUntracked", "true"], repo.path());
    assert_cli_success(&cfg, "enable renameUntracked");
    let index_oid = {
        let out = run_libra_command(&["rev-parse", "HEAD:a.txt"], repo.path());
        assert_cli_success(&out, "rev-parse HEAD:a.txt");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    // Pure worktree move: index keeps a.txt, disk has b.txt.
    fs::rename(repo.path().join("a.txt"), repo.path().join("b.txt")).unwrap();

    // Read the REAL index mode instead of assuming the 100644 default: with
    // an executable fixture, a `.R` branch that hardcoded `100644` would
    // pass a test that compares two constants to each other.
    let index_mode = {
        let out = run_libra_command(&["ls-files", "--stage"], repo.path());
        assert_cli_success(&out, "ls-files --stage");
        let stage = String::from_utf8(out.stdout).unwrap();
        stage
            .lines()
            .find(|l| l.ends_with("a.txt"))
            .and_then(|l| l.split_whitespace().next())
            .unwrap_or_else(|| panic!("staged entry for a.txt: {stage}"))
            .to_string()
    };
    #[cfg(unix)]
    assert_eq!(
        index_mode, "100755",
        "the fixture really is executable, so the mode columns are testable"
    );

    let out = status_stdout(repo.path(), &["status", "--porcelain=v2"]);
    let fields = first_v2_record(&out);
    assert_eq!(fields[1], ".R", "unstaged-only rename xy: {out}");
    assert_eq!(fields[3], index_mode, "mH copies the index mode: {out}");
    assert_eq!(fields[4], index_mode, "mI is the index mode: {out}");
    assert_eq!(fields[5], index_mode, "mW is the real worktree mode: {out}");
    assert_eq!(fields[6], index_oid, "hH copies the index OID: {out}");
    assert_eq!(fields[7], index_oid, "hI is the index OID: {out}");
    assert_eq!(fields[8], "R100", "pure move is exact: {out}");
    assert_eq!(fields[9], "b.txt", "new path first: {out}");
    assert_eq!(fields[10], "a.txt", "old path second: {out}");
}

/// A staged rename chained with a worktree rename produces exactly two v2
/// records (`R.` old→mid, `.R` mid→new), two short-format lines, and two
/// JSON entries — never a merged or dropped hop.
#[test]
fn chain_rename_two_records() {
    // The FIRST hop is inexact on purpose: with an exact a→b hop, `a@HEAD`
    // and `b@index` share one OID, so the second record's hH/hI fields
    // would pass even if the renderer wrongly reported the first hop's
    // object. Editing the content while staging forces the two hops to
    // carry different ids, making the second record's fields meaningful.
    let base: String = (0..40).map(|i| format!("chain line {i}\n")).collect();
    let repo = create_repo_with_committed_file("a.txt", &base);
    let cfg = run_libra_command(&["config", "status.renameUntracked", "true"], repo.path());
    assert_cli_success(&cfg, "enable renameUntracked");
    let head_oid = {
        let out = run_libra_command(&["rev-parse", "HEAD:a.txt"], repo.path());
        assert_cli_success(&out, "rev-parse HEAD:a.txt");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "staged hop a->b");
    fs::write(
        repo.path().join("b.txt"),
        base.replace("chain line 7\n", "chain line seven edited\n"),
    )
    .unwrap();
    let add = run_libra_command(&["add", "b.txt"], repo.path());
    assert_cli_success(&add, "restage the edited first hop");
    let index_oid = {
        let out = run_libra_command(&["ls-files", "--stage"], repo.path());
        assert_cli_success(&out, "ls-files --stage");
        let stage = String::from_utf8(out.stdout).unwrap();
        stage
            .lines()
            .find(|l| l.ends_with("b.txt"))
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or_else(|| panic!("staged entry for b.txt: {stage}"))
            .to_string()
    };
    assert_ne!(
        head_oid, index_oid,
        "the first hop must be inexact or the chain assertions are vacuous"
    );
    fs::rename(repo.path().join("b.txt"), repo.path().join("c.txt")).unwrap();

    let v2 = status_stdout(repo.path(), &["status", "--porcelain=v2"]);
    let records: Vec<&str> = v2.lines().filter(|l| l.starts_with("2 ")).collect();
    assert_eq!(records.len(), 2, "exactly two rename records: {v2}");
    // And NOTHING else: an endpoint that also leaked a `1 ` change row would
    // make consumers replay the same path twice. Counting only `2 ` lines
    // cannot see that.
    let change_lines: Vec<&str> = v2
        .lines()
        .filter(|l| l.starts_with("1 ") || l.starts_with("2 "))
        .collect();
    assert_eq!(
        change_lines.len(),
        2,
        "the two rename records are the ONLY change rows — no leaked endpoints: {v2}"
    );
    let staged_hop = records
        .iter()
        .find(|r| r.contains(" R. ") && r.ends_with("b.txt\ta.txt"))
        .unwrap_or_else(|| panic!("staged hop a->b: {v2}"));
    let worktree_hop = records
        .iter()
        .find(|r| r.contains(" .R ") && r.ends_with("c.txt\tb.txt"))
        .unwrap_or_else(|| panic!("worktree hop b->c: {v2}"));

    // Hop 1 (staged, inexact): HEAD object → the edited index object.
    let staged_fields: Vec<&str> = staged_hop.split_whitespace().collect();
    assert_eq!(staged_fields[6], head_oid, "hop 1 hH is HEAD:a.txt: {v2}");
    assert_eq!(staged_fields[7], index_oid, "hop 1 hI is the index: {v2}");

    // Hop 2 (unstaged): both HEAD columns copy the INDEX object — taking
    // the first hop's HEAD id here is the exact bug this fixture exposes.
    let worktree_fields: Vec<&str> = worktree_hop.split_whitespace().collect();
    assert_eq!(
        worktree_fields[6], index_oid,
        "hop 2 hH copies b@index, not a@HEAD: {v2}"
    );
    assert_eq!(worktree_fields[7], index_oid, "hop 2 hI is b@index: {v2}");
    assert_eq!(
        worktree_fields[3], worktree_fields[4],
        "hop 2 mH copies mI: {v2}"
    );
    assert_eq!(worktree_fields[8], "R100", "hop 2 is a pure move: {v2}");

    let short = status_stdout(repo.path(), &["status", "--short"]);
    let arrows = short.lines().filter(|l| l.contains(" -> ")).count();
    assert_eq!(arrows, 2, "two short-format rename lines: {short}");

    let json = status_stdout(repo.path(), &["--json", "status"]);
    let doc: serde_json::Value = serde_json::from_str(&json).expect("json status");
    let renames = doc["data"]["renames"].as_array().expect("renames array");
    assert_eq!(renames.len(), 2, "two JSON rename entries: {json}");
    assert!(
        renames
            .iter()
            .any(|r| r["from"] == "a.txt" && r["to"] == "b.txt" && r["staged"] == true)
    );
    assert!(
        renames
            .iter()
            .any(|r| r["from"] == "b.txt" && r["to"] == "c.txt" && r["unstaged"] == true)
    );
}

/// Porcelain v2 under `-z`: the rename record ends `… R<pct> <new> NUL <old>
/// NUL` with no trailing newline after the final NUL (§B.6.4).
#[test]
fn porcelain_v2_z_rename_record_nul_paths() {
    let repo = create_repo_with_committed_file("a.txt", "nul separated rename\nsecond line\n");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");

    let out = status_stdout(repo.path(), &["status", "--porcelain=v2", "-z"]);
    assert!(
        out.contains("R100 b.txt\0a.txt\0"),
        "-z rename paths are NUL separated, new first: {out:?}"
    );
    assert!(
        !out.contains("b.txt\ta.txt"),
        "-z must not fall back to the TAB form: {out:?}"
    );
    assert!(
        out.ends_with('\0') && !out.ends_with("\0\n"),
        "records are NUL terminated with no trailing newline: {out:?}"
    );
}

// ── R0-7 residuals: renameLimit cascade + JSON score contracts ───────────────

/// Build a repo with two committed files whose basenames do NOT recur on the
/// destination side, forcing rename pairing through the bounded exhaustive
/// stage (the per-side renameLimit's only gate).
fn repo_with_two_exhaustive_candidates() -> tempfile::TempDir {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let base_a: String = (0..40).map(|i| format!("alpha {i}\n")).collect();
    let base_b: String = (0..40).map(|i| format!("beta {i}\n")).collect();
    fs::write(repo.path().join("a1.txt"), &base_a).unwrap();
    fs::write(repo.path().join("a2.txt"), &base_b).unwrap();
    let add = run_libra_command(&["add", "a1.txt", "a2.txt"], repo.path());
    assert_cli_success(&add, "stage exhaustive fixtures");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit exhaustive fixtures");
    // Delete both and stage differently named, lightly edited replacements.
    let rm = run_libra_command(&["rm", "a1.txt", "a2.txt"], repo.path());
    assert_cli_success(&rm, "delete old side");
    fs::write(
        repo.path().join("b1.txt"),
        base_a.replace("alpha 5\n", "alpha five\n"),
    )
    .unwrap();
    fs::write(
        repo.path().join("b2.txt"),
        base_b.replace("beta 5\n", "beta five\n"),
    )
    .unwrap();
    let add = run_libra_command(&["add", "b1.txt", "b2.txt"], repo.path());
    assert_cli_success(&add, "stage new side");
    repo
}

/// `status.renameLimit` cascades (falling back to `diff.renameLimit`, CLI
/// semantics: 0 = uncapped) and gates ONLY the exhaustive stage (§B.5).
#[test]
fn rename_limit_config_cascade() {
    // status.renameLimit=1 skips the 2×2 exhaustive stage with the warning.
    let repo = repo_with_two_exhaustive_candidates();
    let cfg = run_libra_command(&["config", "status.renameLimit", "1"], repo.path());
    assert_cli_success(&cfg, "set status.renameLimit");
    let json = status_stdout(repo.path(), &["--json", "status"]);
    let doc: serde_json::Value = serde_json::from_str(&json).expect("json status");
    assert_eq!(doc["data"]["renames"].as_array().map(Vec::len), Some(0));
    assert!(
        json.contains("rename_limit_product_skipped"),
        "limit warning surfaces: {json}"
    );

    // diff.renameLimit is the fallback when the status key is absent...
    let repo = repo_with_two_exhaustive_candidates();
    let cfg = run_libra_command(&["config", "diff.renameLimit", "1"], repo.path());
    assert_cli_success(&cfg, "set diff.renameLimit");
    let json = status_stdout(repo.path(), &["--json", "status"]);
    assert!(
        json.contains("rename_limit_product_skipped"),
        "diff.renameLimit fallback applies: {json}"
    );

    // ...and status.renameLimit wins over it; 0 disables the cap entirely.
    let cfg = run_libra_command(&["config", "status.renameLimit", "0"], repo.path());
    assert_cli_success(&cfg, "override with status.renameLimit=0");
    let json = status_stdout(repo.path(), &["--json", "status"]);
    let doc: serde_json::Value = serde_json::from_str(&json).expect("json status");
    assert_eq!(
        doc["data"]["renames"].as_array().map(Vec::len),
        Some(2),
        "status.renameLimit=0 uncaps and wins over diff.renameLimit: {json}"
    );
    assert!(!json.contains("rename_limit_product_skipped"), "{json}");

    // Invalid values fail closed before any output.
    let cfg = run_libra_command(&["config", "status.renameLimit", "banana"], repo.path());
    assert_cli_success(&cfg, "set invalid status.renameLimit");
    let out = run_libra_command(&["status"], repo.path());
    assert!(
        !out.status.success(),
        "invalid renameLimit must fail closed"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("status.renameLimit"),
        "failure names the key: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// JSON chain contract: a staged inexact rename whose destination is also
/// modified in the worktree (`RM` in short form) keeps ONE renames[] entry
/// with the real partial score plus the unstaged modification listed
/// separately.
#[test]
fn json_rename_rm_partial_chain() {
    let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let repo = create_repo_with_committed_file("orig.txt", &base);
    let mv = run_libra_command(&["mv", "orig.txt", "moved.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");
    let edited = base.replace("line 5\n", "line five changed\n");
    fs::write(repo.path().join("moved.txt"), &edited).unwrap();
    let add = run_libra_command(&["add", "moved.txt"], repo.path());
    assert_cli_success(&add, "restage edited moved file");
    // Worktree-only edit on top of the staged rename destination.
    fs::write(
        repo.path().join("moved.txt"),
        edited.replace("line 9\n", "line nine\n"),
    )
    .unwrap();

    let short = status_stdout(repo.path(), &["status", "--short"]);
    assert!(
        short.lines().any(|l| l.starts_with("RM ")),
        "short form renders RM: {short}"
    );

    let json = status_stdout(repo.path(), &["--json", "status"]);
    let doc: serde_json::Value = serde_json::from_str(&json).expect("json status");
    let renames = doc["data"]["renames"].as_array().expect("renames array");
    assert_eq!(renames.len(), 1, "one rename entry: {json}");
    let entry = &renames[0];
    assert_eq!(entry["from"], "orig.txt");
    assert_eq!(entry["to"], "moved.txt");
    assert_eq!(entry["exact"], false);
    let score = entry["score"].as_u64().expect("score");
    assert!((50..100).contains(&score), "partial score: {json}");
    // §B.6.5 side semantics for RM: the rename itself is HEAD→index, so it
    // is a STAGED pair. The trailing worktree edit is reported separately as
    // an unstaged modification — it must not flip the pair's side flags,
    // which would make consumers replay the rename twice.
    assert_eq!(entry["staged"], true, "RM pairs HEAD→index: {json}");
    assert_eq!(
        entry["unstaged"], false,
        "the worktree edit is not a second rename: {json}"
    );
    assert!(
        doc["data"]["unstaged"]["modified"]
            .as_array()
            .expect("unstaged modified")
            .iter()
            .any(|p| p == "moved.txt"),
        "worktree edit listed as unstaged modification: {json}"
    );
}

/// Spanhash similarity is line-multiset based: a fully reordered file scores
/// internal 60000 → JSON `score: 100` while staying `exact: false` (the
/// OIDs differ).
#[test]
fn json_inexact_reordered_score_100() {
    let lines: Vec<String> = (0..40).map(|i| format!("line {i}\n")).collect();
    let base: String = lines.concat();
    let repo = create_repo_with_committed_file("orig.txt", &base);
    let mv = run_libra_command(&["mv", "orig.txt", "moved.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");
    let reversed: String = lines.iter().rev().cloned().collect::<Vec<_>>().concat();
    fs::write(repo.path().join("moved.txt"), reversed).unwrap();
    let add = run_libra_command(&["add", "moved.txt"], repo.path());
    assert_cli_success(&add, "restage reordered file");

    let json = status_stdout(repo.path(), &["--json", "status"]);
    let doc: serde_json::Value = serde_json::from_str(&json).expect("json status");
    let renames = doc["data"]["renames"].as_array().expect("renames array");
    assert_eq!(renames.len(), 1, "{json}");
    assert_eq!(renames[0]["exact"], false, "OIDs differ: {json}");
    assert_eq!(
        renames[0]["score"], 100,
        "reordered lines score 100: {json}"
    );
}

/// Partial similarity floors (Git floor semantics, §B.9 59999→99): the
/// internal 0..=60000 scale is divided by 600 and TRUNCATED, so a pair that
/// is 99.998% similar still displays 99 — only a literal 60000 reaches 100.
/// Pinned on the conversion itself (the boundary is unreachable from a
/// content fixture) plus an end-to-end check that a near-identical rename
/// never rounds up.
#[test]
fn json_inexact_spanhash_score_floor() {
    use libra::command::rename_detect::RenameMatch;

    let percent = |internal: u32| {
        RenameMatch {
            old: std::path::PathBuf::from("a"),
            new: std::path::PathBuf::from("b"),
            exact: false,
            internal_score: internal,
        }
        .score_percent()
    };
    // The §B.9 boundary: one unit below exact must NOT round up to 100.
    assert_eq!(
        percent(59999),
        99,
        "59999 floors to 99, never rounds to 100"
    );
    assert_eq!(percent(60000), 100, "only a full 60000 reaches 100");
    assert_eq!(percent(59400), 99, "59400 is exactly 99%");
    assert_eq!(percent(30000), 50, "the 50% default threshold");
    assert_eq!(percent(599), 0, "sub-1% floors to 0, it does not vanish");

    let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let repo = create_repo_with_committed_file("orig.txt", &base);
    let mv = run_libra_command(&["mv", "orig.txt", "moved.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");
    let edited = base.replace("line 5\n", "line five changed\n");
    fs::write(repo.path().join("moved.txt"), edited).unwrap();
    let add = run_libra_command(&["add", "moved.txt"], repo.path());
    assert_cli_success(&add, "restage edited file");

    let json = status_stdout(repo.path(), &["--json", "status"]);
    let doc: serde_json::Value = serde_json::from_str(&json).expect("json status");
    let renames = doc["data"]["renames"].as_array().expect("renames array");
    assert_eq!(renames.len(), 1, "{json}");
    assert_eq!(renames[0]["exact"], false, "{json}");
    let score = renames[0]["score"].as_u64().expect("score");
    // 39 of 40 lines survive verbatim: high similarity that must still stay
    // strictly under 100 because the pair is not byte-identical.
    assert!(
        (90..100).contains(&score),
        "a one-line edit stays high but must never display 100: {json}"
    );
}

// ── R0-6: quote_pathname / core.quotePath / public entries API ───────────────

/// TAB (and every control character) is escaped REGARDLESS of
/// `core.quotePath` (§B.6.6 always-escape class).
#[test]
fn quote_path_always_escapes_tab() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::write(repo.path().join("tab\there.txt"), "tabbed\n").unwrap();

    let out = status_stdout(repo.path(), &["status", "--porcelain"]);
    assert!(
        out.contains("?? \"tab\\there.txt\""),
        "TAB must be escaped and the path quoted: {out:?}"
    );

    // Still escaped with core.quotePath=false.
    let cfg = run_libra_command(&["config", "core.quotePath", "false"], repo.path());
    assert_cli_success(&cfg, "set core.quotePath=false");
    let out = status_stdout(repo.path(), &["status", "--porcelain"]);
    assert!(
        out.contains("?? \"tab\\there.txt\""),
        "control characters stay escaped under quotePath=false: {out:?}"
    );
}

/// `core.quotePath=false` leaves non-ASCII UTF-8 unescaped in the short
/// format (default `true` escapes it as octal).
#[test]
fn short_quote_path_core_false() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::write(repo.path().join("café.txt"), "accented\n").unwrap();

    let out = status_stdout(repo.path(), &["status", "--short"]);
    assert!(
        out.contains("?? \"caf\\303\\251.txt\""),
        "default quotePath=true escapes non-ASCII bytes: {out:?}"
    );

    let cfg = run_libra_command(&["config", "core.quotePath", "false"], repo.path());
    assert_cli_success(&cfg, "set core.quotePath=false");
    let out = status_stdout(repo.path(), &["status", "--short"]);
    assert!(
        out.contains("?? café.txt") && !out.contains("\\303"),
        "quotePath=false renders raw UTF-8 without quotes: {out:?}"
    );
}

/// Porcelain v1/v2 quote non-ASCII paths under the default `core.quotePath`,
/// while `-z` always emits raw unquoted bytes.
#[test]
fn porcelain_quote_path_utf8_non_ascii() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::write(repo.path().join("café.txt"), "accented\n").unwrap();

    let v1 = status_stdout(repo.path(), &["status", "--porcelain"]);
    assert!(v1.contains("?? \"caf\\303\\251.txt\""), "v1 quotes: {v1:?}");
    let v2 = status_stdout(repo.path(), &["status", "--porcelain=v2"]);
    assert!(v2.contains("? \"caf\\303\\251.txt\""), "v2 quotes: {v2:?}");
    let z = status_stdout(repo.path(), &["status", "--porcelain", "-z"]);
    assert!(
        z.contains("?? café.txt\0") && !z.contains("\\303"),
        "-z stays raw and unquoted: {z:?}"
    );
}

/// `core.quotePath` is a strict Git boolean: invalid values fail closed
/// before any output (§B.5).
#[test]
fn core_quote_path_invalid_fail_closed() {
    let repo = create_repo_with_committed_file("plain.txt", "content\n");
    let cfg = run_libra_command(&["config", "core.quotePath", "banana"], repo.path());
    assert_cli_success(&cfg, "set invalid core.quotePath");
    let out = run_libra_command(&["status"], repo.path());
    assert!(!out.status.success(), "invalid quotePath must fail closed");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("core.quotePath"),
        "failure names the key: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The public `generate_short_status_entries` API keeps renames first-class
/// (destination worktree state in the unstaged column) while the legacy
/// tuple API decomposes them into pre-R0 D/A/?? endpoints.
#[test]
fn generate_short_status_entries_api() {
    use std::path::PathBuf;

    use libra::command::status::{
        Changes, ShortStatusEntry, generate_short_format_status, generate_short_status_entries,
    };

    let staged = Changes {
        renamed: vec![(PathBuf::from("a.txt"), PathBuf::from("b.txt"))],
        ..Default::default()
    };
    let unstaged = Changes {
        modified: vec![PathBuf::from("b.txt")],
        new: vec![PathBuf::from("loose.txt")],
        ..Default::default()
    };

    let entries = generate_short_status_entries(&staged, &unstaged);
    assert_eq!(
        entries,
        vec![
            ShortStatusEntry::Rename {
                old: PathBuf::from("a.txt"),
                new: PathBuf::from("b.txt"),
                staged: 'R',
                unstaged: 'M',
            },
            ShortStatusEntry::Path {
                path: PathBuf::from("loose.txt"),
                staged: '?',
                unstaged: '?',
            },
        ],
        "renames stay first-class with the destination's worktree state"
    );

    let legacy = generate_short_format_status(&staged, &unstaged);
    assert_eq!(
        legacy,
        vec![
            (PathBuf::from("a.txt"), 'D', ' '),
            (PathBuf::from("b.txt"), 'A', 'M'),
            (PathBuf::from("loose.txt"), '?', '?'),
        ],
        "the legacy tuple API decomposes renames into pre-R0 endpoints"
    );
}

/// Human long format renders a rename as `renamed: <old> -> <new>` (§B.6.1).
#[test]
fn human_rename_long_format() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\nsecond line\n");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");

    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        out.lines()
            .any(|l| l.trim_start().starts_with("renamed:") && l.contains("a.txt -> b.txt")),
        "long format uses the renamed: arrow form: {out}"
    );
}

/// Human rendering routes every path through `quote_pathname` (§B.6.6): a
/// TAB inside a rename destination is escaped, never emitted literally
/// where it would break column parsing downstream.
#[test]
fn human_rename_quotes_control_characters() {
    let repo = create_repo_with_committed_file("old.txt", "body\n");
    let mv = run_libra_command(&["mv", "old.txt", "new\tname.txt"], repo.path());
    assert_cli_success(&mv, "libra mv to a tabbed name");

    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        out.lines().any(|l| l.trim_start().starts_with("renamed:")
            && l.contains("old.txt -> \"new\\tname.txt\"")),
        "the tab is escaped through quote_pathname: {out}"
    );
    assert!(
        !out.contains("new\tname.txt"),
        "and never emitted as a literal tab: {out:?}"
    );
}

/// Strip `ESC [ ... m` SGR sequences so a colored run can be compared
/// byte-for-byte with the plain one.
#[cfg(unix)]
fn strip_ansi_codes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1b && index + 1 < bytes.len() && bytes[index + 1] == b'[' {
            let mut end = index + 2;
            while end < bytes.len() && bytes[end] != b'm' {
                end += 1;
            }
            index = end + 1;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    out
}

/// Forced color must not defeat `core.quotePath=false`: the XY status
/// letters are ANSI-colored, but the path bytes stay raw (Git parity).
#[cfg(unix)]
#[test]
fn short_forced_color_keeps_raw_high_bytes_with_quote_path_false() {
    use std::os::unix::ffi::OsStrExt;

    let repo = create_repo_with_committed_file("old.txt", "body\\n");
    if !fs_supports_non_utf8_names(repo.path()) {
        return;
    }
    fs::write(
        repo.path()
            .join(std::ffi::OsStr::from_bytes(b"new-\xff.txt")),
        "payload\\n",
    )
    .unwrap();
    let cfg = run_libra_command(&["config", "core.quotePath", "false"], repo.path());
    assert_cli_success(&cfg, "set core.quotePath false");

    let out = run_libra_command(&["status", "--short", "--color=always"], repo.path());
    assert_cli_success(&out, "forced-color short status");
    assert!(
        out.stdout.windows(5).any(|window| window == b"new-\xff"),
        "raw high bytes under forced color: {:?}",
        out.stdout
    );
    assert!(
        !out.stdout.windows(4).any(|window| window == b"\\377"),
        "no octal escaping under forced color: {:?}",
        out.stdout
    );
    assert!(
        out.stdout.windows(2).any(|window| window == b"\x1b["),
        "and the XY letters really are ANSI-colored (the forced mode works): {:?}",
        out.stdout
    );

    // A rename row under forced color: stripping the SGR codes must leave
    // exactly the plain run's bytes — the byte-faithful colored rename arm
    // is identical to the former String renderer for valid UTF-8.
    let repo2 = create_repo_with_committed_file("a.txt", "body\n");
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo2.path());
    assert_cli_success(&mv, "libra mv");
    let plain = run_libra_command(&["status", "--short", "--color=never"], repo2.path());
    assert_cli_success(&plain, "plain short status");
    let plain_text = String::from_utf8_lossy(&plain.stdout);
    assert!(
        plain_text
            .lines()
            .any(|line| line.starts_with('R') && line.contains("a.txt -> b.txt")),
        "the fixture really produces a rename row: {plain_text}"
    );
    let colored = run_libra_command(&["status", "--short", "--color=always"], repo2.path());
    assert_cli_success(&colored, "forced-color short status");
    assert!(
        colored.stdout.windows(2).any(|window| window == b"\x1b["),
        "the CLI-forced run really emitted SGR codes: {:?}",
        colored.stdout
    );
    assert_eq!(
        strip_ansi_codes(&colored.stdout),
        plain.stdout,
        "stripped forced-color output equals the plain output"
    );
    // The same through the config-scoped forced color: the byte-faithful
    // colored branch runs and must not alter the bytes (the colored
    // crate's own gate decides code emission; `--color=always` above is the
    // route that forces SGR on a pipe).
    let cfg = run_libra_command(&["config", "color.status.short", "always"], repo2.path());
    assert_cli_success(&cfg, "set color.status.short=always");
    let via_config = run_libra_command(&["status", "--short"], repo2.path());
    assert_cli_success(&via_config, "config-forced short status");
    assert_eq!(
        strip_ansi_codes(&via_config.stdout),
        plain.stdout,
        "config-forced output equals the plain output after stripping"
    );
}

/// A non-UTF-8 directory marker keeps its raw bytes end to end: collapsed
/// `? dir/` markers are built from the raw name, never through `display()`.
#[cfg(unix)]
#[test]
fn untracked_dir_marker_keeps_raw_bytes_with_quote_path_false() {
    use std::os::unix::ffi::OsStrExt;

    let repo = create_repo_with_committed_file("old.txt", "body\\n");
    if !fs_supports_non_utf8_names(repo.path()) {
        return;
    }
    let dir = repo.path().join(std::ffi::OsStr::from_bytes(b"dir-\xff"));
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("file.txt"), "payload\\n").unwrap();

    let on = status_stdout(repo.path(), &["status", "--short"]);
    assert!(
        on.contains("dir-\\377/"),
        "the default escapes the marker: {on}"
    );

    let cfg = run_libra_command(&["config", "core.quotePath", "false"], repo.path());
    assert_cli_success(&cfg, "set core.quotePath false");
    let out = run_libra_command(&["status", "--short"], repo.path());
    assert_cli_success(&out, "short status");
    assert!(
        out.stdout.windows(5).any(|window| window == b"dir-\xff"),
        "the marker keeps its raw bytes: {:?}",
        out.stdout
    );

    // The porcelain projection (`data_with_repo_root_paths`) keeps the raw
    // marker bytes too — this is the surface even `-z` flows through.
    let out = run_libra_command(&["status", "--porcelain"], repo.path());
    assert_cli_success(&out, "porcelain status");
    assert!(
        out.stdout.windows(5).any(|window| window == b"dir-\xff"),
        "porcelain keeps the raw marker bytes: {:?}",
        out.stdout
    );

    // JSON carries the ESCAPED original bytes — never a U+FFFD corruption.
    let out = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&out, "json status");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let untracked = doc["data"]["untracked"].as_array().expect("untracked");
    assert!(
        untracked.iter().any(|p| p == "dir-\\377/"),
        "JSON escapes the marker bytes without corrupting them: {doc}"
    );
    assert!(
        !untracked.iter().any(|p| p.to_string().contains('\u{fffd}')),
        "and never a replacement character: {doc}"
    );

    // Ignored markers ride the same raw-bytes projection (the ignore
    // pattern itself is a UTF-8-safe glob — ignore files must be UTF-8).
    fs::write(repo.path().join(".libraignore"), "dir-i*/\n").unwrap();
    let ignored_dir = repo.path().join(std::ffi::OsStr::from_bytes(b"dir-i\xff"));
    fs::create_dir(&ignored_dir).unwrap();
    fs::write(ignored_dir.join("file.txt"), "payload\n").unwrap();
    let out = run_libra_command(&["status", "--short", "--ignored"], repo.path());
    assert_cli_success(&out, "short status --ignored");
    let lines: Vec<&[u8]> = out.stdout.split(|b| *b == b'\n').collect();
    assert!(
        lines
            .iter()
            .any(|line| *line == b"!! dir-i\xff/".as_slice()),
        "the ignored directory is reported with its `/` marker: {:?}",
        out.stdout
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with(b"?? dir-i".as_slice())),
        "and never as an untracked row: {:?}",
        out.stdout
    );
}

/// `core.quotePath` controls >0x7F bytes: with the default the invalid-UTF-8
/// name is octal-escaped; with `false` the short format writes the RAW high
/// bytes (Git parity) instead of forcing the escape.
#[cfg(unix)]
#[test]
fn short_non_utf8_quote_path_false_writes_raw_high_bytes() {
    use std::os::unix::ffi::OsStrExt;

    let repo = create_repo_with_committed_file("old.txt", "body\n");
    if !fs_supports_non_utf8_names(repo.path()) {
        return;
    }
    // An untracked name with an invalid-UTF-8 high byte; `libra add` refuses
    // such paths, so the fixture stays at the `??` row.
    fs::write(
        repo.path()
            .join(std::ffi::OsStr::from_bytes(b"new-\xff.txt")),
        "payload\n",
    )
    .unwrap();

    let on = status_stdout(repo.path(), &["status", "--short"]);
    assert!(
        on.contains("\\377"),
        "quotePath=true octal-escapes the high byte: {on}"
    );

    let cfg = run_libra_command(&["config", "core.quotePath", "false"], repo.path());
    assert_cli_success(&cfg, "set core.quotePath false");
    let out = run_libra_command(&["status", "--short"], repo.path());
    assert_cli_success(&out, "short status with quotePath false");
    assert!(
        out.stdout.windows(5).any(|window| window == b"new-\xff"),
        "quotePath=false writes the raw high bytes: {:?}",
        out.stdout
    );
    assert!(
        !out.stdout.windows(4).any(|window| window == b"\\377"),
        "and nothing is octal-escaped: {:?}",
        out.stdout
    );
}

/// §B.6.0.1: an unstaged rename whose SOURCE carries a staged change keeps
/// that component on the rename-aware short/porcelain-v1 rows — `MR`/`AR`
/// `old -> new`, never a space-staged ` R` that erases the state porcelain
/// v2 derives correctly (2026-08-06 R0-6 review; mirrors the v2 MR pin).
#[test]
fn short_rename_keeps_staged_source_component() {
    let repo = create_repo_with_committed_file("a.txt", "one\ntwo\nthree\n");
    enable_rename_untracked(repo.path());
    fs::write(repo.path().join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    let add = run_libra_command(&["add", "a.txt"], repo.path());
    assert_cli_success(&add, "stage the modification");
    fs::rename(repo.path().join("a.txt"), repo.path().join("b.txt")).unwrap();

    let short = status_stdout(repo.path(), &["status", "--short"]);
    assert!(
        short
            .lines()
            .any(|line| line.starts_with("MR") && line.contains("a.txt -> b.txt")),
        "short keeps the staged M on the rename row: {short}"
    );
    let v1 = status_stdout(repo.path(), &["status", "--porcelain"]);
    assert!(
        v1.lines()
            .any(|line| line.starts_with("MR") && line.contains("a.txt -> b.txt")),
        "porcelain v1 keeps the staged M: {v1}"
    );

    // AR twin: a staged-new source renamed in the worktree.
    let repo2 = create_repo_with_committed_file("base.txt", "base\n");
    enable_rename_untracked(repo2.path());
    fs::write(repo2.path().join("c.txt"), "fresh content\n").unwrap();
    let add = run_libra_command(&["add", "c.txt"], repo2.path());
    assert_cli_success(&add, "stage the addition");
    fs::rename(repo2.path().join("c.txt"), repo2.path().join("d.txt")).unwrap();
    let short = status_stdout(repo2.path(), &["status", "--short"]);
    assert!(
        short
            .lines()
            .any(|line| line.starts_with("AR") && line.contains("c.txt -> d.txt")),
        "short keeps the staged A on the rename row: {short}"
    );
}

/// §B.6.0.1 racily-clean guard (Git parity; 2026-08-06 R0-8 review): an
/// index entry whose stat triple MATCHES the worktree but whose file is
/// not strictly older than the index snapshot must be content-compared.
/// The index writers stat AFTER hashing, so an edit landing in that
/// window pairs a post-edit stat with a pre-edit hash — plain status then
/// fabricated "clean" for a modified file (the observed CHANGELOG.md
/// incident) while diff's stricter shortcut still caught it.
#[test]
#[serial]
fn racily_clean_entry_is_content_compared_not_trusted() {
    let repo = create_repo_with_committed_file("base.txt", "base\n");
    let _guard = ChangeDirGuard::new(repo.path());

    // The worktree file holds the EDITED content...
    fs::write(repo.path().join("racy.txt"), "edited content\n").unwrap();
    // ...but the entry records the PRE-edit blob paired with the CURRENT
    // (post-edit) stat — exactly the poisoned pairing the race produces
    // (`new_from_file` is the vulnerable stat-after-hash constructor).
    let (stale_hash, _) = write_blob_to_repo("original content\n");
    let mut index = Index::new();
    let entry = IndexEntry::new_from_file(Path::new("racy.txt"), stale_hash, repo.path())
        .expect("build the poisoned entry");
    index.add(entry);
    index.save(path::index()).expect("write the index");

    // Deterministically place the file INSIDE the racy window: rewind the
    // INDEX FILE's mtime to the worktree file's own mtime, so the file is
    // not strictly older than the snapshot (equal counts as racy).
    let file_mtime = fs::metadata(repo.path().join("racy.txt"))
        .unwrap()
        .modified()
        .unwrap();
    let index_file = fs::OpenOptions::new()
        .write(true)
        .open(path::index())
        .expect("open index for time rewind");
    index_file
        .set_times(fs::FileTimes::new().set_modified(file_mtime))
        .expect("rewind index mtime");
    drop(index_file);

    let out = run_libra_command(&["status", "--short"], repo.path());
    assert_cli_success(&out, "status with a racily-clean entry");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.lines()
            .any(|line| line.contains("racy.txt") && line.contains('M')),
        "the poisoned stat triple is not trusted — content comparison \
         reports the modification: {text}"
    );
}

/// §B.6.4: an unresolved conflict must never pair as a staged-rename
/// SOURCE, even when a same-content staged addition exists — the
/// stage-0-less index classifies the conflict as staged-deleted, and
/// rename detection would otherwise consume it into a `2` record whose
/// only truthful spelling is the `u` row (2026-08-06 R0-5 review).
#[test]
#[serial]
fn unmerged_path_never_pairs_as_a_staged_rename_source() {
    let body = "same content on either side of the would-be rename\n";
    let repo = create_repo_with_committed_file("conflict.txt", body);
    let _guard = ChangeDirGuard::new(repo.path());
    let mut index = Index::new();
    add_index_stage(&mut index, "conflict.txt", body, 1);
    add_index_stage(&mut index, "conflict.txt", "ours\n", 2);
    add_index_stage(&mut index, "conflict.txt", "theirs\n", 3);
    // The bait: a staged addition with EXACTLY the conflict's HEAD
    // content — an eligible exact rename destination if the conflict
    // were treated as a staged deletion.
    add_index_stage(&mut index, "added.txt", body, 0);
    index
        .save(path::index())
        .expect("write unmerged index with the staged add");
    fs::write(repo.path().join("added.txt"), body).unwrap();

    let output = status_stdout(repo.path(), &["status", "--porcelain=v2"]);
    assert!(
        output
            .lines()
            .any(|line| line.starts_with("u UU") && line.ends_with("conflict.txt")),
        "the conflict keeps its u record: {output}"
    );
    assert_eq!(
        output
            .lines()
            .filter(|line| (line.starts_with("2 ") || line.starts_with("1 "))
                && line.contains("conflict.txt"))
            .count(),
        0,
        "no rename or ordinary record consumes the conflict: {output}"
    );
    assert!(
        output
            .lines()
            .any(|line| line.starts_with("1 A.") && line.ends_with("added.txt")),
        "the staged addition stays an ordinary A row: {output}"
    );
}

/// §B.6.4: a staged deletion's `1 D.` row spells BOTH absent sides as
/// `000000` plus the zero hash — stage 0 has no entry and the worktree
/// file is gone; the old fallback fabricated `100644` for a lookup that
/// cannot succeed (2026-08-06 R0-5 review).
#[test]
fn porcelain_v2_staged_deletion_spells_absent_sides_as_zero() {
    let repo = create_repo_with_committed_file("doomed.txt", "content\n");
    let head = run_libra_command(&["rev-parse", "HEAD:doomed.txt"], repo.path());
    assert_cli_success(&head, "resolve the committed blob oid");
    let head_oid = String::from_utf8_lossy(&head.stdout).trim().to_string();
    let rm = run_libra_command(&["rm", "doomed.txt"], repo.path());
    assert_cli_success(&rm, "stage the deletion");

    let out = run_libra_command(&["status", "--porcelain=v2"], repo.path());
    assert_cli_success(&out, "porcelain v2 with a staged deletion");
    let text = String::from_utf8_lossy(&out.stdout);
    let row = text
        .lines()
        .find(|line| line.starts_with("1 D."))
        .unwrap_or_else(|| panic!("expected the staged-deletion row: {text}"));
    let fields: Vec<_> = row.split_whitespace().collect();
    assert_eq!(fields[3], "100644", "real HEAD mode survives: {row}");
    assert_eq!(
        fields[4], "000000",
        "absent index side is 000000, never 100644: {row}"
    );
    assert_eq!(
        fields[5], "000000",
        "absent worktree side is 000000, never 100644: {row}"
    );
    assert_eq!(fields[6], head_oid, "real HEAD hash: {row}");
    assert_eq!(
        fields[7],
        "0".repeat(head_oid.len()),
        "absent index hash is all-zero: {row}"
    );
    assert_eq!(fields[8], "doomed.txt");
}

/// §B.6.4 single projection, `u` rows: the unmerged payload is
/// repository-root-relative too, so from a subdirectory the worktree-mode
/// column must read the REAL on-disk mode — the old double projection
/// stat'ed a nonexistent doubled path and spelled it `000000`
/// (2026-08-06 R0-5 review).
#[cfg(unix)]
#[test]
#[serial]
fn porcelain_v2_unmerged_row_from_subdirectory_keeps_real_worktree_mode() {
    use std::os::unix::fs::PermissionsExt;

    let repo = create_repo_with_committed_file("base.txt", "base\n");
    let _guard = ChangeDirGuard::new(repo.path());
    let mut index = Index::new();
    add_index_stage(&mut index, "sub/conflict.sh", "base\n", 1);
    add_index_stage(&mut index, "sub/conflict.sh", "ours\n", 2);
    add_index_stage(&mut index, "sub/conflict.sh", "theirs\n", 3);
    index.save(path::index()).expect("write unmerged index");
    fs::create_dir_all(repo.path().join("sub")).unwrap();
    fs::write(repo.path().join("sub/conflict.sh"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(
        repo.path().join("sub/conflict.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let out = run_libra_command(&["status", "--porcelain=v2"], &repo.path().join("sub"));
    assert_cli_success(&out, "porcelain v2 from the subdirectory");
    let text = String::from_utf8_lossy(&out.stdout);
    let row = text
        .lines()
        .find(|line| line.starts_with("u UU"))
        .unwrap_or_else(|| panic!("expected the unmerged row: {text}"));
    let fields: Vec<_> = row.split_whitespace().collect();
    assert_eq!(fields.len(), 11, "u-line arity: {row}");
    assert_eq!(
        fields[6], "100755",
        "real on-disk worktree mode, not a missed stat's fallback: {row}"
    );
    assert_eq!(
        fields[10], "sub/conflict.sh",
        "repo-root-relative path: {row}"
    );
    assert_eq!(
        text.lines()
            .filter(|line| (line.starts_with("1 ") || line.starts_with("2 "))
                && line.contains("conflict.sh"))
            .count(),
        0,
        "no ordinary record duplicates the conflict: {text}"
    );
}

/// §B.6.4 single projection, NON-rename rows: the porcelain v2 payload is
/// already repository-root-relative, so rendering from a subdirectory must
/// keep the REAL index/HEAD/worktree metadata. The old double projection
/// made every lookup miss from a subdir and fabricated 100644/zero-hash
/// columns while the printed path stayed correct (2026-08-06 R0-5 review).
#[test]
#[cfg(unix)]
fn porcelain_v2_non_rename_rows_from_subdirectory_keep_real_metadata() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::create_dir(repo.path().join("sub")).unwrap();
    fs::write(repo.path().join("sub/inner.sh"), "#!/bin/sh\necho one\n").unwrap();
    fs::set_permissions(
        repo.path().join("sub/inner.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let add = run_libra_command(&["add", "sub/inner.sh"], repo.path());
    assert_cli_success(&add, "stage the executable");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit the executable");
    // Unstaged content edit; truncating write keeps the 0755 bit.
    fs::write(repo.path().join("sub/inner.sh"), "#!/bin/sh\necho two\n").unwrap();

    let head = run_libra_command(&["rev-parse", "HEAD:sub/inner.sh"], repo.path());
    assert_cli_success(&head, "resolve the committed blob oid");
    let head_oid = String::from_utf8_lossy(&head.stdout).trim().to_string();

    let out = run_libra_command(&["status", "--porcelain=v2"], &repo.path().join("sub"));
    assert_cli_success(&out, "porcelain v2 from the subdirectory");
    let text = String::from_utf8_lossy(&out.stdout);
    let row = text
        .lines()
        .find(|line| line.starts_with("1 .M"))
        .unwrap_or_else(|| panic!("expected the modified row: {text}"));
    let fields: Vec<_> = row.split_whitespace().collect();
    assert_eq!(
        fields[3], "100755",
        "real HEAD mode, not a fabricated fallback: {row}"
    );
    assert_eq!(
        fields[4], "100755",
        "real index mode, not a missed lookup's 100644: {row}"
    );
    assert_eq!(
        fields[5], "100755",
        "real worktree mode, not a missed stat's 100644: {row}"
    );
    assert_eq!(fields[6], head_oid, "real HEAD hash, not all-zero: {row}");
    assert_eq!(
        fields[7], head_oid,
        "index equals HEAD for an unstaged edit: {row}"
    );
    assert_eq!(fields[8], "sub/inner.sh", "repo-root-relative path: {row}");
}

/// §B.4.3: `--null` AFTER the `--` separator is a pathspec, never a format
/// flag — the normalize window and clap's escape both stop at `--`. (§B.9
/// spec-named test that was never written; added 2026-08-05 R0-4 review.)
#[test]
fn null_after_separator_ignored() {
    let repo = create_repo_with_committed_file("a.txt", "content\n");
    fs::write(repo.path().join("a.txt"), "changed\n").unwrap();
    // A literal FILE named `--null`: the token after `--` must reach the
    // pathspec filter (matching this file, excluding a.txt), proving it
    // was propagated as a pathspec rather than dropped or read as a flag.
    fs::write(repo.path().join("--null"), "pathspec target\n").unwrap();
    let out = run_libra_command(&["status", "--", "--null"], repo.path());
    assert_cli_success(&out, "a pathspec spelled --null parses");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.stdout.contains(&0u8),
        "no NUL separators — the token was a pathspec, not a format flag: {text:?}"
    );
    assert!(
        text.contains("--null"),
        "the untracked file named --null matches its own pathspec: {text}"
    );
    assert!(
        !text.contains("a.txt"),
        "the modified a.txt is excluded by the pathspec filter: {text}"
    );
}

// ── R0-4 residuals: cluster/value safety pins (§B.4.3) ───────────────────────

/// A global short option's ATTACHED value must never leak letters into
/// format detection, and a valued global before the subcommand must not
/// shift the status slice (post-clap parsing is structurally immune; these
/// pins keep it that way).
#[test]
fn global_short_value_contains_z_not_format() {
    let repo = create_repo_with_committed_file("a.txt", "hello rename world\nsecond line\n");

    // `-J=ndjson` carries 's'/'j'/'n' letters in its attached value: output
    // must be one NDJSON envelope — no porcelain, no NUL, no short rows.
    let out = status_stdout(repo.path(), &["-J=ndjson", "status"]);
    let first = out.lines().next().expect("one ndjson line");
    let doc: serde_json::Value = serde_json::from_str(first).expect("ndjson envelope");
    assert_eq!(doc["command"], "status");
    assert!(!out.contains('\0'), "no NUL records: {out:?}");

    // Subcommand location is unaffected: the rename threshold still applies
    // after a valued global option.
    let mv = run_libra_command(&["mv", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");
    let out = status_stdout(repo.path(), &["-J=ndjson", "st", "--find-renames=505"]);
    let doc: serde_json::Value =
        serde_json::from_str(out.lines().next().expect("line")).expect("json");
    assert!(
        doc["data"]["renames"]
            .as_array()
            .is_some_and(|r| r.len() == 1),
        "threshold flag applies after a valued global: {out}"
    );
}

/// A status-slice value option swallowing `s` must produce the option's own
/// invalid-value error — never the short format.
#[test]
fn status_short_value_contains_s_not_short() {
    let repo = create_repo_with_committed_file("a.txt", "content\n");
    let out = run_libra_command(&["status", "-bus"], repo.path());
    assert!(!out.status.success(), "-bus must fail on the -u value");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid value 's'") && stderr.contains("untracked-files"),
        "the 's' is -u's value, not the short flag: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).is_empty(),
        "fail-closed before any output"
    );
}

/// A cluster stops at the first value-eating option: the remaining letters
/// are its value (`-buno` = `-b -u no`), and a trailing `z` is a value too.
#[test]
fn cluster_stops_at_value_option() {
    let repo = create_repo_with_committed_file("a.txt", "content\n");
    fs::write(repo.path().join("loose.txt"), "untracked\n").unwrap();

    // `-buno`: branch + untracked-files=no — the untracked file disappears
    // and the output stays the LONG format (no XY rows).
    let out = status_stdout(repo.path(), &["status", "-buno"]);
    assert!(
        !out.contains("loose.txt") && !out.lines().any(|l| l.starts_with("??")),
        "-buno hides untracked and stays long-format: {out}"
    );

    // `-buz`: the `z` is -u's (invalid) value, never the -z flag.
    let out = run_libra_command(&["status", "-buz"], repo.path());
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("invalid value 'z'"),
        "the 'z' is -u's value, not -z: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The embedding API never mutates the process warning tracker: warnings
/// ride the returned envelope only (§B.4.3 isolation contract; the internal
/// design keeps the global tracker on the CLI delivery path exclusively).
#[tokio::test]
#[serial]
async fn api_warning_no_global_exit_pollution() {
    use libra::utils::test::ChangeDirGuard;

    let repo = repo_with_two_exhaustive_candidates();
    let cfg = run_libra_command(&["config", "status.renameLimit", "1"], repo.path());
    assert_cli_success(&cfg, "set limit for warning");

    let _guard = ChangeDirGuard::new(repo.path());
    let before = libra::utils::output::warning_was_emitted();
    let envelope = libra::command::status::collect_status_json_envelope_for_api(repo.path())
        .await
        .expect("api status");
    assert!(
        envelope["data"]["warnings"]
            .as_array()
            .is_some_and(|w| !w.is_empty()),
        "the degradation warning rides the envelope: {envelope}"
    );
    assert_eq!(
        libra::utils::output::warning_was_emitted(),
        before,
        "the API path must not touch the process warning tracker"
    );
}

/// Two concurrent API invocations each carry their own warnings[] without
/// cross-talk or global-tracker mutation. (The API is same-cwd by design —
/// both calls target the same repository.)
#[tokio::test]
#[serial]
async fn api_warning_concurrent_isolated() {
    use libra::utils::test::ChangeDirGuard;

    let repo = repo_with_two_exhaustive_candidates();
    let cfg = run_libra_command(&["config", "status.renameLimit", "1"], repo.path());
    assert_cli_success(&cfg, "set limit for warning");

    let _guard = ChangeDirGuard::new(repo.path());
    let before = libra::utils::output::warning_was_emitted();
    let (a, b) = tokio::join!(
        libra::command::status::collect_status_json_envelope_for_api(repo.path()),
        libra::command::status::collect_status_json_envelope_for_api(repo.path()),
    );
    // The API entry serializes concurrent collections internally, so BOTH
    // calls succeed with complete, consistent envelopes carrying their own
    // warnings[], and the process tracker stays untouched.
    for envelope in [a.expect("first api call"), b.expect("second api call")] {
        assert!(
            envelope["data"]["warnings"]
                .as_array()
                .is_some_and(|w| !w.is_empty()),
            "each concurrent call carries its own warnings: {envelope}"
        );
    }
    assert_eq!(libra::utils::output::warning_was_emitted(), before);
}

/// Warnings survive await points inside and between API calls (no
/// thread_local storage that a task migration could drop).
#[tokio::test]
#[serial]
async fn api_warning_survives_await() {
    use libra::utils::test::ChangeDirGuard;

    let repo = repo_with_two_exhaustive_candidates();
    let cfg = run_libra_command(&["config", "status.renameLimit", "1"], repo.path());
    assert_cli_success(&cfg, "set limit for warning");

    let _guard = ChangeDirGuard::new(repo.path());
    let first = libra::command::status::collect_status_json_envelope_for_api(repo.path())
        .await
        .expect("first api call");
    tokio::task::yield_now().await;
    let second = libra::command::status::collect_status_json_envelope_for_api(repo.path())
        .await
        .expect("second api call");
    for envelope in [first, second] {
        assert!(
            envelope["data"]["warnings"]
                .as_array()
                .is_some_and(|w| !w.is_empty()),
            "warnings persist across await points: {envelope}"
        );
    }
}

// ── R0-3: bounded rename-destination probe (§B.3.1–§B.3.2, §B.3.5) ──────────

/// Repo with a committed file moved on disk (worktree move: index keeps the
/// old path, the new path is untracked).
fn repo_with_worktree_move(dest: &str) -> tempfile::TempDir {
    let repo = create_repo_with_committed_file("moved-src.txt", "probe rename content\nline two\n");
    if let Some(parent) = std::path::Path::new(dest).parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(repo.path().join(parent)).unwrap();
    }
    fs::rename(repo.path().join("moved-src.txt"), repo.path().join(dest)).unwrap();
    repo
}

fn enable_rename_untracked(repo: &Path) {
    let cfg = run_libra_command(&["config", "status.renameUntracked", "true"], repo);
    assert_cli_success(&cfg, "enable renameUntracked");
}

/// Without the `status.renameUntracked` extension the probe NEVER runs: an
/// unreadable directory that would block the probe does not fail status,
/// and the move stays `D` + `??` (Git parity).
#[test]
fn probe_skipped_when_untracked_disabled() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repo_with_worktree_move("dest.txt");
    let locked = repo.path().join("locked-dir");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    // The base scan reports the unreadable directory (§B.3.3), so read the
    // partial JSON view; the POINT of this test is that no probe ran, i.e.
    // no rename was claimed.
    let out = run_libra_command(&["--json", "status"], repo.path());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_cli_success(&out, "json status without the probe");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert_eq!(
        doc["data"]["renames"],
        serde_json::json!([]),
        "default stays D + ?? without probing: {doc}"
    );
    assert!(
        doc["data"]["unstaged"]["deleted"]
            .as_array()
            .is_some_and(|d| d.iter().any(|p| p == "moved-src.txt")),
        "the base deletion survives: {doc}"
    );
    assert!(
        doc["data"]["untracked"]
            .as_array()
            .is_some_and(|u| u.iter().any(|p| p == "dest.txt" || p == "dest.txt/")),
        "and so does the untracked destination — an early marker collapse \
         would silently lose it: {doc}"
    );
}

/// With renames disabled (`--no-renames`) the probe never runs even under
/// the extension — the unreadable directory cannot fail status.
#[test]
fn probe_skipped_when_renames_disabled() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    let locked = repo.path().join("locked-dir");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    let out = run_libra_command(&["--json", "status", "--no-renames"], repo.path());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_cli_success(&out, "json status --no-renames with locked dir");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert_eq!(
        doc["data"]["renames"],
        serde_json::json!([]),
        "no probe, no rename: {doc}"
    );
}

/// `-uno` hides untracked DISPLAY but never the probe: the rename still
/// pairs (§B.3.1.1 item 5) …
#[test]
fn rename_untracked_true_uno_probe_success() {
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    let out = status_stdout(repo.path(), &["status", "-uno"]);
    assert!(
        out.contains("renamed:") && out.contains("dest.txt"),
        "-uno must not hide probe destinations: {out}"
    );
}

/// … and the probe never injects `?`/`??` markers under `-uno`.
#[test]
fn rename_untracked_true_uno_no_question_mark() {
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    fs::write(repo.path().join("unrelated.txt"), "loose\n").unwrap();
    let out = status_stdout(repo.path(), &["status", "--short", "-uno"]);
    assert!(
        !out.lines().any(|l| l.starts_with("??")),
        "-uno keeps every ?? marker hidden: {out}"
    );
    assert!(out.contains(" -> "), "rename still pairs under -uno: {out}");
}

/// An exclude-only pathspec set keeps the probe rooted at the repository
/// root (§B.3.1.1 item 2) — the rename still pairs.
#[test]
fn probe_roots_exclude_only_repo_root() {
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    let out = status_stdout(repo.path(), &["status", "--", ":(exclude)unrelated"]);
    assert!(
        out.contains("renamed:") && out.contains("dest.txt"),
        "exclude-only specs must not shrink the probe: {out}"
    );
}

/// §B.3.1.1 rule 3: a pathspec that cannot be narrowed safely on a
/// case-sensitive filesystem — `:(icase)` and case-folded specs — falls back
/// to the repository root. Over-probing is acceptable; missing a destination
/// is not, so a case-mismatched spec must still find the rename.
#[test]
fn probe_roots_icase_falls_back_repo_root() {
    let repo = repo_with_worktree_move("nested/DEST.txt");
    enable_rename_untracked(repo.path());

    for spec in [":(icase)nested", ":(icase)NESTED"] {
        let out = status_stdout(repo.path(), &["status", "--", spec, "moved-src.txt"]);
        assert!(
            out.contains("renamed:") && out.contains("DEST.txt"),
            "{spec} must fall back to the repo root rather than under-probe: {out}"
        );
    }
}

/// §B.3.1.1 rule 4: probe roots are deduplicated after normalization, and a
/// root that an ANCESTOR root already covers is dropped (`a/` covers
/// `a/b/`). Walking both would double-charge the shared subtree against the
/// enumeration budget and could truncate a scan that fits.
#[test]
fn probe_roots_dedup_nested_ancestors() {
    let repo = repo_with_worktree_move("outer/inner/dest.txt");
    enable_rename_untracked(repo.path());
    // Enough sibling noise that double-walking `outer/` would exceed a
    // budget that a single walk fits inside.
    for i in 0..8 {
        fs::write(repo.path().join(format!("outer/noise-{i}.txt")), "noise\n").unwrap();
    }

    let out = run_libra_command_with_stdin_and_env(
        &[
            "--json",
            "status",
            "--",
            "outer",
            "outer/inner",
            "moved-src.txt",
        ],
        repo.path(),
        "",
        // Comfortably above one walk of `outer/`, below two.
        &[("LIBRA_TEST_STATUS_PROBE_ENUM_BUDGET", "16")],
    );
    assert_cli_success(&out, "overlapping probe roots");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["renames"]
            .as_array()
            .is_some_and(|r| r.iter().any(|x| x["to"] == "outer/inner/dest.txt")),
        "the nested root is covered by its ancestor, walked once: {doc}"
    );
    assert_eq!(
        doc["data"]["rename_detection_complete"], true,
        "deduplicated roots must not truncate a scan that fits: {doc}"
    );
}

/// §B.3.2 composite result: EACCES on one directory AND a budget trip in
/// the same run are BOTH reported — `io_blocked[]` plus the truncation
/// warning — while surviving pairs still render. Text formats fail closed
/// on the same repository.
#[test]
#[cfg(unix)]
fn probe_eacces_then_truncated_keeps_both() {
    use std::os::unix::fs::PermissionsExt;

    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    // The noise lives in a SUBDIRECTORY so the ROOT listing (a handful of
    // entries) completes deterministically — the rename destination is
    // therefore always enumerated — and the budget then trips inside the
    // subdirectory regardless of the filesystem's `read_dir` order.
    fs::create_dir_all(repo.path().join("noisedir")).unwrap();
    for i in 0..24 {
        fs::write(
            repo.path().join(format!("noisedir/noise-{i:02}.txt")),
            "noise\n",
        )
        .unwrap();
    }
    let blocked = repo.path().join("blocked");
    fs::create_dir_all(&blocked).unwrap();
    fs::write(blocked.join("inner.txt"), "inner\n").unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();

    let out = run_libra_command_with_stdin_and_env(
        &["--json", "status"],
        repo.path(),
        "",
        &[("LIBRA_TEST_STATUS_PROBE_ENUM_BUDGET", "10")],
    );
    let text = run_libra_command_with_stdin_and_env(
        &["status", "--porcelain"],
        repo.path(),
        "",
        &[("LIBRA_TEST_STATUS_PROBE_ENUM_BUDGET", "10")],
    );
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o755)).unwrap();

    assert_cli_success(&out, "json reports the composite partial result");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        !doc["data"]["io_blocked"]
            .as_array()
            .expect("io_blocked")
            .is_empty(),
        "the blocked directory is reported: {doc}"
    );
    assert!(
        doc["data"]["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|x| x["code"] == "probe_truncated")),
        "the budget trip is reported ALONGSIDE the block: {doc}"
    );
    assert_eq!(doc["data"]["rename_detection_complete"], false, "{doc}");
    // Partial pairing SURVIVES both degradations: a truncated + blocked run
    // still reports the pairs it did find, and the base rows stay truthful.
    // (This is the half that distinguishes "degraded" from "gave up".)
    assert!(
        doc["data"]["renames"]
            .as_array()
            .is_some_and(|r| r.iter().any(|x| x["to"] == "dest.txt")),
        "the rename found before the budget tripped is still reported: {doc}"
    );
    let blocked_paths: Vec<&str> = doc["data"]["io_blocked"]
        .as_array()
        .expect("io_blocked")
        .iter()
        .filter_map(|e| e["path"]["display"].as_str())
        .collect();
    let mut sorted = blocked_paths.clone();
    sorted.sort_unstable();
    assert_eq!(
        blocked_paths, sorted,
        "blocked paths are emitted in stable sorted order: {doc}"
    );
    assert!(
        !text.status.success(),
        "text formats fail closed on the same repository: {}",
        String::from_utf8_lossy(&text.stderr)
    );
}

/// §B.3.1.2: an UNMERGED path never qualifies as a rename destination —
/// a conflicted file is mid-resolution, not the landing site of a move.
#[test]
fn probe_dest_rejects_unmerged() {
    let repo = create_repo_with_committed_file("shared.txt", "base one\nbase two\n");
    enable_rename_untracked(repo.path());
    let branch = run_libra_command(&["switch", "-c", "other"], repo.path());
    assert_cli_success(&branch, "branch off");
    fs::write(repo.path().join("shared.txt"), "their one\nbase two\n").unwrap();
    let add = run_libra_command(&["add", "shared.txt"], repo.path());
    assert_cli_success(&add, "stage theirs");
    let commit = run_libra_command(&["commit", "-m", "theirs"], repo.path());
    assert_cli_success(&commit, "commit theirs");
    let back = run_libra_command(&["switch", "main"], repo.path());
    assert_cli_success(&back, "back to main");
    fs::write(repo.path().join("shared.txt"), "our one\nbase two\n").unwrap();
    let add = run_libra_command(&["add", "shared.txt"], repo.path());
    assert_cli_success(&add, "stage ours");
    let commit = run_libra_command(&["commit", "-m", "ours"], repo.path());
    assert_cli_success(&commit, "commit ours");
    // Conflict.
    let merge = run_libra_command(&["merge", "other"], repo.path());
    assert!(!merge.status.success(), "the merge must conflict");

    let out = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&out, "json status mid-conflict");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["renames"]
            .as_array()
            .is_some_and(|r| r.iter().all(|x| x["to"] != "shared.txt")),
        "an unmerged path is never a rename destination: {doc}"
    );
    assert!(
        !doc["data"]["unmerged"]
            .as_array()
            .expect("unmerged")
            .is_empty(),
        "the conflict itself is still reported: {doc}"
    );
}

/// §B.3.1.2: a tracked path never qualifies as a rename destination — a
/// deletion plus a modified tracked file must not pair.
#[test]
fn probe_dest_rejects_tracked_path() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let body = "probe rename content\nline two\n";
    fs::write(repo.path().join("a.txt"), body).unwrap();
    fs::write(repo.path().join("b.txt"), "different original\n").unwrap();
    let add = run_libra_command(&["add", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&add, "stage both");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit both");
    enable_rename_untracked(repo.path());
    fs::remove_file(repo.path().join("a.txt")).unwrap();
    fs::write(repo.path().join("b.txt"), body).unwrap(); // tracked, now identical

    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        !out.contains("renamed:") && out.contains("deleted:"),
        "a tracked path must never be a destination: {out}"
    );
}

/// §B.3.1.2: an ignored path never qualifies as a rename destination.
#[test]
fn probe_dest_rejects_ignored() {
    let repo = repo_with_worktree_move("dest.ign");
    enable_rename_untracked(repo.path());
    fs::write(repo.path().join(".libraignore"), "*.ign\n").unwrap();
    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        !out.contains("renamed:") && out.contains("deleted:"),
        "ignored candidates stay out of pairing: {out}"
    );
}

/// §B.3.2: the enumeration budget bounds a wide directory — status still
/// succeeds, reports the truncation warning, and never hangs.
#[test]
fn probe_budget_bounds_wide_directory() {
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    for i in 0..24 {
        fs::write(repo.path().join(format!("noise-{i:02}.txt")), "noise\n").unwrap();
    }
    let out = run_libra_command_with_stdin_and_env(
        &["--json", "status"],
        repo.path(),
        "",
        &[("LIBRA_TEST_STATUS_PROBE_ENUM_BUDGET", "5")],
    );
    assert_cli_success(&out, "truncated probe still succeeds");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let message = doc["data"]["warnings"]
        .as_array()
        .and_then(|w| w.iter().find(|x| x["code"] == "probe_truncated"))
        .and_then(|w| w["message"].as_str())
        .unwrap_or_else(|| panic!("truncation surfaces as a structured warning: {doc}"));
    // §B.3.2 requires the warning to ATTRIBUTE the cause: "we stopped
    // listing" and "we stopped collecting candidates" call for different
    // operator responses (narrow the pathspec vs. raise the destination
    // cap), so a generic "truncated" is not enough.
    assert!(
        message.contains("enumeration"),
        "the enumeration budget must be named as the cause: {message}"
    );

    // The OTHER budget, in isolation: enough enumeration headroom to see
    // every entry, but a destination cap of 1.
    let out = run_libra_command_with_stdin_and_env(
        &["--json", "status"],
        repo.path(),
        "",
        &[
            ("LIBRA_TEST_STATUS_PROBE_ENUM_BUDGET", "10000"),
            ("LIBRA_TEST_STATUS_PROBE_DEST_BUDGET", "1"),
        ],
    );
    assert_cli_success(&out, "destination-capped probe still succeeds");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let message = doc["data"]["warnings"]
        .as_array()
        .and_then(|w| w.iter().find(|x| x["code"] == "probe_truncated"))
        .and_then(|w| w["message"].as_str())
        .unwrap_or_else(|| panic!("destination truncation is reported: {doc}"));
    assert!(
        message.contains("destination"),
        "the destination budget must be named as the cause: {message}"
    );
    assert_eq!(
        doc["data"]["rename_detection_complete"], false,
        "either budget marks detection incomplete: {doc}"
    );
    assert_eq!(
        doc["data"]["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .find(|w| w["code"] == "probe_truncated")
            .expect("probe warning")["source"],
        "probe",
        "a probe budget is a probe-source warning, not rename_detect: {doc}"
    );
}

/// §B.3.1.2: only regular files and symlinks qualify as rename
/// destinations. A FIFO (or any other non-regular node) is never enumerated
/// as a candidate, so `status` cannot block forever hashing it — the class
/// of path that would otherwise wedge the probe is excluded by TYPE, before
/// any read is attempted. Complements the tracked-scan deadline test, which
/// covers a FIFO that IS reachable (because the index says the path should
/// be a regular file).
#[test]
#[cfg(unix)]
fn probe_never_reads_a_non_regular_candidate() {
    let repo = create_repo_with_committed_file("moved-src.txt", "probe rename content\nline two\n");
    enable_rename_untracked(repo.path());
    fs::remove_file(repo.path().join("moved-src.txt")).unwrap();
    let fifo = repo.path().join("dest.txt");
    let created = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !created {
        eprintln!("skipped (mkfifo unavailable)");
        return;
    }

    let started = std::time::Instant::now();
    let out = run_libra_command(&["--json", "status"], repo.path());
    let elapsed = started.elapsed();
    assert_cli_success(&out, "status does not block on a FIFO in the tree");
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "a non-regular node is skipped by type, never opened: took {elapsed:?}"
    );

    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert_eq!(
        doc["data"]["renames"],
        serde_json::json!([]),
        "a FIFO never becomes a rename destination: {doc}"
    );
    assert!(
        doc["data"]["unstaged"]["deleted"]
            .as_array()
            .is_some_and(|d| d.iter().any(|p| p == "moved-src.txt")),
        "the base deletion is reported unchanged: {doc}"
    );
    assert!(
        doc["data"]["untracked"]
            .as_array()
            .is_some_and(|u| u.iter().all(|p| p != "dest.txt")),
        "and it is not listed as an untracked FILE either (Git parity): {doc}"
    );
}

/// §B.3.2 determinism: a COMPLETE probe processes entries in repo-relative
/// byte order, and a TRUNCATED one still emits whatever it collected in
/// stable sorted order. The member set of a truncated partial is not
/// promised across filesystems (OS `read_dir` order is arbitrary), but the
/// ordering of what is reported is — otherwise two runs on one machine
/// could disagree and no consumer could diff them.
#[test]
fn probe_complete_entries_sorted() {
    let repo = create_repo_with_committed_file("moved-src.txt", "sorted content\nline two\n");
    enable_rename_untracked(repo.path());
    fs::remove_file(repo.path().join("moved-src.txt")).unwrap();
    // Several equally-good destinations, created in a deliberately
    // non-alphabetical order so insertion order cannot pass for sorting.
    for name in ["zeta.txt", "alpha.txt", "mid.txt", "beta.txt"] {
        fs::write(repo.path().join(name), "sorted content\nline two\n").unwrap();
    }

    let sorted_untracked = |args: &[&str], env: &[(&str, &str)]| -> Vec<String> {
        let out = run_libra_command_with_stdin_and_env(args, repo.path(), "", env);
        assert_cli_success(&out, "status run");
        let doc: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
        doc["data"]["untracked"]
            .as_array()
            .expect("untracked")
            .iter()
            .filter_map(|p| p.as_str().map(str::to_string))
            .collect()
    };

    // Complete probe: output is byte-sorted.
    let complete = sorted_untracked(&["--json", "status", "-uall"], &[]);
    let mut expected = complete.clone();
    expected.sort();
    assert_eq!(
        complete, expected,
        "a complete run reports entries in byte order"
    );

    // Truncated probe: the qualified destinations are the alphabetically
    // first two, surfaced through the RENAME RECORDS — the probe's own
    // output contract — not merely the main scan's listing. Two deleted
    // sources compete for a destination budget of two, so the pairing
    // reveals exactly which destinations the probe qualified.
    let repo2 = tempdir().expect("second repo");
    init_repo_via_cli(repo2.path());
    configure_identity_via_cli(repo2.path());
    for name in ["src-a.txt", "src-b.txt"] {
        fs::write(repo2.path().join(name), "shared body\nline two\n").unwrap();
    }
    let add = run_libra_command(&["add", "."], repo2.path());
    assert_cli_success(&add, "stage sources");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo2.path());
    assert_cli_success(&commit, "commit sources");
    for name in ["src-a.txt", "src-b.txt"] {
        fs::remove_file(repo2.path().join(name)).unwrap();
    }
    for name in ["zeta.txt", "alpha.txt", "mid.txt", "beta.txt"] {
        fs::write(repo2.path().join(name), "shared body\nline two\n").unwrap();
    }
    enable_rename_untracked(repo2.path());

    let truncated_probe = || {
        let out = run_libra_command_with_stdin_and_env(
            &["--json", "status"],
            repo2.path(),
            "",
            &[("LIBRA_TEST_STATUS_PROBE_DEST_BUDGET", "2")],
        );
        assert_cli_success(&out, "status run");
        let doc: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
        assert!(
            doc["data"]["warnings"]
                .as_array()
                .is_some_and(|w| w.iter().any(|x| x["code"] == "probe_truncated")),
            "the fixture really trips the destination budget: {doc}"
        );
        let mut to: Vec<String> = doc["data"]["renames"]
            .as_array()
            .expect("renames")
            .iter()
            .filter_map(|r| r["to"].as_str().map(str::to_string))
            .collect();
        to.sort();
        to
    };
    let first = truncated_probe();
    assert_eq!(
        first,
        vec!["alpha.txt".to_string(), "beta.txt".to_string()],
        "a truncated probe qualifies the sorted-first destinations"
    );
    assert_eq!(
        first,
        truncated_probe(),
        "and deterministically so across runs"
    );
}

/// §B.3.2: a nested repository or gitlink is excluded UNCONDITIONALLY —
/// including when a pathspec selects it directly as a probe root. The
/// per-entry filter only ever sees children, so a directly named root
/// would otherwise walk straight into a foreign repository and offer its
/// files as rename destinations.
#[test]
fn probe_rejects_a_directly_selected_nested_repository_root() {
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    // A nested repository holding content identical to the deleted source.
    let nested = repo.path().join("nested");
    fs::create_dir_all(nested.join(".libra")).unwrap();
    fs::write(nested.join("decoy.txt"), "probe rename content\nline two\n").unwrap();

    let out = run_libra_command(&["--json", "status", "--", "nested"], repo.path());
    assert_cli_success(&out, "status narrowed to a nested repository");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["renames"]
            .as_array()
            .is_some_and(|r| r.is_empty()),
        "a nested-repo root yields no rename destinations: {doc}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("decoy"),
        "the nested repository's contents are never enumerated: {doc}"
    );
}

/// A metadata root selected through an ESCAPING symlink is not silently
/// skipped as "excluded metadata": the containment breach is recorded in
/// `io_blocked[]`. The exclusions must never fire before containment, or
/// `status -- link/.git` (where `link` points outside the worktree and the
/// target happens to hold a `.git`) would vanish without a trace.
#[cfg(unix)]
#[test]
fn probe_records_escape_for_a_metadata_root_behind_a_symlink() {
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    // `link` escapes the worktree; the OUTSIDE directory holds a `.git`.
    let outside = tempdir().expect("outside dir");
    fs::create_dir_all(outside.path().join(".git")).unwrap();
    std::os::unix::fs::symlink(outside.path(), repo.path().join("link")).unwrap();

    let out = run_libra_command(&["--json", "status", "--", "link/.git"], repo.path());
    assert_cli_success(&out, "an escaping metadata root degrades, it does not fail");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["io_blocked"]
            .as_array()
            .is_some_and(|b| b.iter().any(|e| e["path"]["display"] == "link/.git")),
        "the escape is recorded as io_blocked, not silently skipped as metadata: {doc}"
    );
}

/// The probe never descends into `.libra`/`.git` metadata directories.
#[test]
fn probe_never_enters_libra_or_git_metadata() {
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    // Plant an identical-content decoy inside a fake .git directory.
    fs::create_dir_all(repo.path().join("sub/.git")).unwrap();
    fs::write(
        repo.path().join("sub/.git/decoy.txt"),
        "probe rename content\nline two\n",
    )
    .unwrap();
    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        out.contains("renamed:") && out.contains("dest.txt") && !out.contains("decoy"),
        "metadata directories are never probed: {out}"
    );

    // The exclusion is unconditional, not merely a child-name filter: a
    // pathspec that names a metadata directory DIRECTLY must not make it a
    // probe root either.
    for spec in [".libra", "sub/.git"] {
        let narrowed = run_libra_command(&["--json", "status", "--", spec], repo.path());
        assert_cli_success(&narrowed, "status narrowed to a metadata pathspec");
        let doc: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&narrowed.stdout)).expect("json");
        assert!(
            doc["data"]["renames"]
                .as_array()
                .is_some_and(|r| r.is_empty()),
            "a metadata pathspec yields no rename destinations: {doc}"
        );
        assert!(
            !String::from_utf8_lossy(&narrowed.stdout).contains("decoy"),
            "a metadata pathspec never enumerates repository internals: {spec}"
        );
    }
}

/// §B.6.2 regression: a collapsed untracked directory keeps its trailing
/// `/` marker after the repo-root path projection. Losing it renders
/// `?? dir/` as `?? dir`, which every Git consumer reads as an untracked
/// FILE named `dir` — the projection normalizes through path components,
/// which silently drops the marker unless it is re-attached.
#[test]
fn porcelain_untracked_directory_marker_survives_projection() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    let add = run_libra_command(&["add", "base.txt"], repo.path());
    assert_cli_success(&add, "stage base");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit base");
    fs::create_dir_all(repo.path().join("dir/deeper")).unwrap();
    fs::write(repo.path().join("dir/deeper/nested.txt"), "n\n").unwrap();

    for format in [
        vec!["status", "--porcelain"],
        vec!["status", "--porcelain", "v2"],
        vec!["status", "--short"],
    ] {
        let out = status_stdout(repo.path(), &format);
        assert!(
            out.lines().any(|line| line.ends_with("dir/")),
            "{format:?} must keep the collapsed-directory marker: {out:?}"
        );
    }

    // The projection is the interesting path: run from a subdirectory so
    // the cwd-relative collection has to be rewritten to repo-root form.
    let sub = repo.path().join("dir/deeper");
    let out = status_stdout(&sub, &["status", "--porcelain"]);
    assert!(
        out.lines().any(|line| line == "?? dir/"),
        "the marker survives the subdirectory projection too: {out:?}"
    );
}

/// §B.3.2: a directory the index records as a gitlink (mode 0o160000) is a
/// submodule checkout and is never traversed — including when it has not
/// been initialized, so it carries no `.git`/`.libra` marker for the
/// existence test to catch.
#[test]
fn probe_excludes_uninitialized_gitlink_dir() {
    use git_internal::internal::index::{Index, IndexEntry};

    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    // A materialized-but-uninitialized submodule: real files, no marker.
    fs::create_dir_all(repo.path().join("submod")).unwrap();
    fs::write(
        repo.path().join("submod/decoy.txt"),
        "probe rename content\nline two\n",
    )
    .unwrap();

    // Record `submod` in the index as a gitlink.
    let index_path = repo.path().join(".libra/index");
    let mut index = Index::load(&index_path).expect("load index");
    let mut entry = IndexEntry::new_from_blob("submod".to_string(), ObjectHash::default(), 0);
    entry.mode = 0o160000;
    index.add(entry);
    index.save(&index_path).expect("save index");

    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        !out.contains("submod/decoy.txt"),
        "a gitlink subtree is never walked, marker or not: {out}"
    );
}

/// A directory symlink is a LEAF for the probe (never recursed).
#[test]
fn probe_directory_symlink_is_leaf() {
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    fs::create_dir(repo.path().join("real-dir")).unwrap();
    fs::write(
        repo.path().join("real-dir/inner.txt"),
        "probe rename content\nline two\n",
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("real-dir", repo.path().join("dir-link")).unwrap();

    let out = status_stdout(repo.path(), &["status"]);
    assert!(
        out.contains("renamed:") && !out.contains("dir-link/inner.txt"),
        "symlinked directories are leaves, not recursion points: {out}"
    );
}

/// §B.3.5: when the probe is complete and a directory's only candidate was
/// consumed by a rename, the `? dir/` marker collapses …
#[test]
fn probe_complete_consumes_dir_marker() {
    let repo = repo_with_worktree_move("newdir/dest.txt");
    enable_rename_untracked(repo.path());
    let out = status_stdout(repo.path(), &["status", "--short"]);
    assert!(out.contains(" -> "), "the move pairs: {out}");
    assert!(
        !out.contains("?? newdir/"),
        "a fully-consumed directory loses its marker: {out}"
    );
}

/// … while a truncated probe conservatively keeps the marker.
#[test]
fn probe_truncated_keeps_dir_marker() {
    let repo = repo_with_worktree_move("newdir/dest.txt");
    enable_rename_untracked(repo.path());
    for i in 0..24 {
        fs::write(repo.path().join(format!("noise-{i:02}.txt")), "noise\n").unwrap();
    }
    let out = run_libra_command_with_stdin_and_env(
        &["status", "--short"],
        repo.path(),
        "",
        &[("LIBRA_TEST_STATUS_PROBE_ENUM_BUDGET", "3")],
    );
    assert_cli_success(&out, "truncated probe still succeeds");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("?? newdir/"),
        "a truncated probe keeps directory markers: {text}"
    );
}

/// R0-3 conservative narrowing: a blocked probe path fails the whole status
/// closed (LBR-IO-001) — "cannot inspect" never degrades into "no rename".
/// (R0-8 relaxes JSON to the io_blocked[] partial contract.)
#[test]
fn probe_dir_eacces_fail_closed() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    let locked = repo.path().join("locked-dir");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    let out = run_libra_command(&["status"], repo.path());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!out.status.success(), "blocked probe fails closed");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-IO-001"),
        "blocked probe carries the stable code: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── R0-8: cache-mode × rename-flag clap exclusivity (§B.5) ───────────────────

/// `--cached`/`--check-dirty` never run rename detection, so combining them
/// with any rename spelling is a clap usage error (fail-closed before any
/// cache read).
#[test]
fn cached_conflicts_with_rename_flags() {
    let repo = create_repo_with_committed_file("a.txt", "content\n");
    for combo in [
        vec!["status", "--cached", "--renames"],
        vec!["status", "--cached", "--no-renames"],
        vec!["status", "--cached", "--find-renames=75"],
        vec!["status", "--check-dirty", "--renames"],
        vec!["status", "--check-dirty", "--no-renames"],
        vec!["status", "--check-dirty", "--find-renames=75"],
    ] {
        let out = run_libra_command(&combo, repo.path());
        assert!(!out.status.success(), "{combo:?} must be a usage error");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("cannot be used with"),
            "{combo:?} reports the clap conflict: {stderr}"
        );
    }
}

// ── R0-8: io_blocked partial contract (§B.3.3, §B.6.0.1) ─────────────────────

/// A repo whose `locked/` directory holds a COMMITTED file and is then made
/// unreadable — the tracked scan hits EACCES on the file inside, producing a
/// genuine io_blocked event (an empty unreadable dir is conservatively
/// reported as an untracked marker instead).
fn repo_with_locked_tracked_dir() -> tempfile::TempDir {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::create_dir(repo.path().join("locked")).unwrap();
    fs::write(repo.path().join("locked/inside.txt"), "tracked\n").unwrap();
    let add = run_libra_command(&["add", "locked/inside.txt"], repo.path());
    assert_cli_success(&add, "stage locked file");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit locked file");
    lock_dir(&repo.path().join("locked"));
    repo
}

fn lock_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
}

fn unlock_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// An I/O-blocked path with no other change: JSON reports `is_clean: false`
/// ("cannot inspect" is never clean) plus the blocked entry and its
/// worktree-family warning.
#[test]
fn json_io_blocked_is_clean_false() {
    let repo = repo_with_locked_tracked_dir();
    let locked = repo.path().join("locked");
    let out = run_libra_command(&["--json", "status"], repo.path());
    unlock_dir(&locked);
    assert_cli_success(&out, "json status with blocked dir");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert_eq!(doc["data"]["is_clean"], false, "{doc}");
    assert!(
        doc["data"]["io_blocked"]
            .as_array()
            .is_some_and(|b| !b.is_empty()),
        "{doc}"
    );
    assert!(
        doc["data"]["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|x| x["code"] == "worktree_permission_denied")),
        "{doc}"
    );
}

/// `--exit-code` treats an I/O block as dirty → exit 1 (JSON path).
#[test]
fn exit_code_ioblocked_is_one() {
    let repo = repo_with_locked_tracked_dir();
    let locked = repo.path().join("locked");
    let out = run_libra_command(&["--json", "status", "--exit-code"], repo.path());
    unlock_dir(&locked);
    assert_eq!(
        out.status.code(),
        Some(1),
        "io_blocked counts as dirty for --exit-code: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The io_blocked[] entry object shape and the completeness fields are a
/// public contract (§B.6.0.1).
#[test]
fn json_io_blocked_schema_snapshot() {
    let repo = repo_with_locked_tracked_dir();
    let locked = repo.path().join("locked");
    let out = run_libra_command(&["--json", "status"], repo.path());
    unlock_dir(&locked);
    assert_cli_success(&out, "json status with blocked dir");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let entry = &doc["data"]["io_blocked"][0];
    assert_eq!(
        entry
            .as_object()
            .map(|o| {
                let mut keys: Vec<_> = o.keys().cloned().collect();
                keys.sort();
                keys
            })
            .expect("entry object"),
        vec!["path", "reason", "rename", "staged"],
        "entry key set is frozen: {doc}"
    );
    assert_eq!(entry["path"]["display"], "locked", "{doc}");
    assert!(
        doc["data"]["io_blocked"].as_array().is_some_and(|b| b
            .iter()
            .any(|e| e["path"]["display"] == "locked/inside.txt")),
        "the tracked file inside records its own event: {doc}"
    );
    // Pin the nested key SET, not just the values: `doc["path"]["raw_base64"]`
    // yields Null for a missing field too, so an equality check against Null
    // would keep passing if the field were renamed or dropped.
    let path_keys: Vec<&str> = entry["path"]
        .as_object()
        .expect("path is an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        path_keys,
        vec!["display", "raw_base64"],
        "the nested path object's field names are frozen: {entry}"
    );
    let entry_keys: Vec<&str> = entry
        .as_object()
        .expect("entry is an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        entry_keys,
        vec!["path", "reason", "rename", "staged"],
        "the io_blocked entry's field names are frozen: {entry}"
    );
    assert_eq!(entry["path"]["raw_base64"], serde_json::Value::Null);
    assert_eq!(entry["staged"], serde_json::Value::Null);
    assert_eq!(entry["reason"], "permission_denied");
    assert_eq!(entry["rename"], serde_json::Value::Null);
    assert_eq!(doc["data"]["base_scan_complete"], false, "{doc}");
    // The BASE scan was blocked; rename detection was never degraded (it was
    // not even attempted here), so its own flag stays true — the two fields
    // describe different subsystems. See
    // `json_base_and_rename_completeness_matrix`.
    assert_eq!(doc["data"]["rename_detection_complete"], true, "{doc}");
    assert_eq!(doc["data"]["complete"], false, "{doc}");
}

/// §B.3.3 accumulator: siblings collected before/after a blocked directory
/// survive into the JSON untracked list.
#[test]
fn main_scan_ioblocked_keeps_prior_sibling_json() {
    let repo = create_repo_with_committed_file("plain.txt", "content\n");
    fs::write(repo.path().join("aa-loose.txt"), "kept\n").unwrap();
    fs::write(repo.path().join("zz-loose.txt"), "kept too\n").unwrap();
    let locked = repo.path().join("mm-locked");
    fs::create_dir(&locked).unwrap();
    lock_dir(&locked);
    let out = run_libra_command(&["--json", "status"], repo.path());
    unlock_dir(&locked);
    assert_cli_success(&out, "json status keeps siblings");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let untracked = doc["data"]["untracked"].as_array().expect("untracked");
    assert!(
        untracked.iter().any(|p| p == "aa-loose.txt")
            && untracked.iter().any(|p| p == "zz-loose.txt"),
        "siblings survive a blocked directory: {doc}"
    );
}

/// §B.3.3 accumulator: ignored entries collected before an EACCES-blocked
/// sibling survive into the JSON partial — the scan never drops what the
/// ignored walk already yielded. (Spec-named test that was never written;
/// added 2026-08-05 R0-3 review.)
#[test]
fn main_scan_ioblocked_keeps_ignored_json() {
    let repo = create_repo_with_committed_file("plain.txt", "content\n");
    fs::write(repo.path().join(".libraignore"), "aa-ignored.txt\n").unwrap();
    fs::write(repo.path().join("aa-ignored.txt"), "ignored\n").unwrap();
    let locked = repo.path().join("mm-locked");
    fs::create_dir(&locked).unwrap();
    lock_dir(&locked);
    let out = run_libra_command(&["--json", "status", "--ignored"], repo.path());
    unlock_dir(&locked);
    assert_cli_success(&out, "json status --ignored with a blocked sibling");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["ignored"]
            .as_array()
            .is_some_and(|ignored| ignored.iter().any(|p| p == "aa-ignored.txt")),
        "ignored entries collected before the block survive: {doc}"
    );
    assert!(
        doc["data"]["io_blocked"]
            .as_array()
            .is_some_and(|blocked| !blocked.is_empty()),
        "the blocked directory is reported: {doc}"
    );
}

/// §B.3.3 accumulator, mid-ITERATION arm: a `ReadDir::next` failure part-way
/// through a directory (the errno-passthrough class — NFS/FUSE — that no
/// tempdir fixture can produce, so a debug seam stands in) records the
/// directory as io_blocked and keeps every already-collected entry. The
/// discarded-inner-error shape claimed a COMPLETE scan while silently
/// dropping the directory's remaining entries (2026-08-05 R0-3 review).
#[test]
fn main_scan_mid_iteration_readdir_error_reports_partial() {
    let repo = create_repo_with_committed_file("base.txt", "content\n");
    fs::create_dir(repo.path().join("half")).unwrap();
    fs::write(repo.path().join("half/inside.txt"), "maybe seen\n").unwrap();
    fs::write(repo.path().join("other.txt"), "sibling\n").unwrap();

    let out = run_libra_command_with_stdin_and_env(
        &["--json", "status", "-uall"],
        repo.path(),
        "",
        &[("LIBRA_TEST_READDIR_ITER_ERROR_DIR", "half")],
    );
    assert_cli_success(
        &out,
        "a mid-iteration readdir error degrades, it does not fail",
    );
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["io_blocked"].as_array().is_some_and(|blocked| {
            blocked.iter().any(|event| {
                event["path"]["display"]
                    .as_str()
                    .is_some_and(|path| path.contains("half"))
            })
        }),
        "the failing directory is reported as io_blocked: {doc}"
    );
    assert_eq!(
        doc["data"]["complete"], false,
        "a directory whose listing broke mid-way is never a complete scan: {doc}"
    );
    assert!(
        doc["data"]["untracked"]
            .as_array()
            .is_some_and(|untracked| untracked.iter().any(|p| p == "other.txt")),
        "entries outside the failing directory are kept: {doc}"
    );
    assert!(
        doc["data"]["untracked"]
            .as_array()
            .is_some_and(|untracked| untracked.iter().any(|p| p == "half/inside.txt")),
        "the entry published BEFORE the mid-iteration error survives: {doc}"
    );

    // Same shape with a genuine `TimedOut` iteration error: the event must
    // carry the `io_timeout` reason (the taxonomy separates a hung mount
    // from an ordinary read failure), and nothing may be swallowed by a
    // sentinel arm keyed on the error kind.
    let out = run_libra_command_with_stdin_and_env(
        &["--json", "status", "-uall"],
        repo.path(),
        "",
        &[
            ("LIBRA_TEST_READDIR_ITER_ERROR_DIR", "half"),
            ("LIBRA_TEST_READDIR_ITER_ERROR_KIND", "timedout"),
        ],
    );
    assert_cli_success(&out, "a timed-out iteration degrades, it does not fail");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["io_blocked"].as_array().is_some_and(|blocked| {
            blocked.iter().any(|event| {
                event["reason"] == "io_timeout"
                    && event["path"]["display"]
                        .as_str()
                        .is_some_and(|path| path.contains("half"))
            })
        }),
        "a genuine TimedOut iteration error is recorded with the io_timeout reason: {doc}"
    );
    assert_eq!(
        doc["data"]["complete"], false,
        "and the timed-out listing is never a complete scan: {doc}"
    );

    // Entry-level NotFound is the OPPOSITE contract: one entry vanishing
    // mid-iteration is skipped, the iterator keeps going, nothing is
    // recorded, and the scan stays complete. The seam replaces exactly one
    // yielded entry with Err(NotFound), so of the two real files exactly
    // one survives — reverting the in-loop skip arm to plain `?` would
    // abandon the directory and fail the survivor count.
    let repo2 = create_repo_with_committed_file("base.txt", "content\n");
    fs::create_dir(repo2.path().join("pair")).unwrap();
    fs::write(repo2.path().join("pair/one.txt"), "1\n").unwrap();
    fs::write(repo2.path().join("pair/two.txt"), "2\n").unwrap();
    let out = run_libra_command_with_stdin_and_env(
        &["--json", "status", "-uall"],
        repo2.path(),
        "",
        &[("LIBRA_TEST_READDIR_ENTRY_NOTFOUND_DIR", "pair")],
    );
    assert_cli_success(&out, "a vanished entry is skipped, it is not fatal");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let survivors = doc["data"]["untracked"]
        .as_array()
        .expect("untracked")
        .iter()
        .filter(|p| p.as_str().is_some_and(|p| p.starts_with("pair/")))
        .count();
    assert_eq!(
        survivors, 1,
        "exactly the non-vanished entry survives the iteration: {doc}"
    );
    assert!(
        doc["data"]["io_blocked"]
            .as_array()
            .is_some_and(|blocked| blocked.is_empty()),
        "a vanished entry records nothing: {doc}"
    );
    assert_eq!(
        doc["data"]["complete"], true,
        "and the scan stays complete: {doc}"
    );
}

/// §B.3.3 tracked-scan accumulator: an unreadable tracked file records an
/// io_blocked entry while other tracked changes stay reported.
#[test]
fn tracked_scan_eacces_partial_json() {
    use std::os::unix::fs::PermissionsExt;
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::write(repo.path().join("readable.txt"), "one\n").unwrap();
    fs::write(repo.path().join("blocked.txt"), "two\n").unwrap();
    let add = run_libra_command(&["add", "readable.txt", "blocked.txt"], repo.path());
    assert_cli_success(&add, "stage both");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit both");
    fs::write(repo.path().join("readable.txt"), "one edited\n").unwrap();
    fs::write(repo.path().join("blocked.txt"), "two edited\n").unwrap();
    fs::set_permissions(
        repo.path().join("blocked.txt"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();

    let out = run_libra_command(&["--json", "status"], repo.path());
    fs::set_permissions(
        repo.path().join("blocked.txt"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert_cli_success(&out, "json partial tracked scan");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["unstaged"]["modified"]
            .as_array()
            .is_some_and(|m| m.iter().any(|p| p == "readable.txt")),
        "the readable modification stays reported: {doc}"
    );
    assert!(
        doc["data"]["io_blocked"]
            .as_array()
            .is_some_and(|b| b.iter().any(|e| e["path"]["display"] == "blocked.txt")),
        "the unreadable file records an io_blocked entry: {doc}"
    );
}

/// §B.3.3 dirty-cache guard: an I/O-blocked `--scan` writes NOTHING — the
/// previous snapshot survives verbatim.
#[test]
#[serial]
fn ioblocked_scan_does_not_replace_dirty_cache() {
    let repo = create_repo_with_committed_file("plain.txt", "content\n");
    fs::write(repo.path().join("cached-new.txt"), "snapshot row\n").unwrap();
    let scan = run_libra_command(&["status", "--scan"], repo.path());
    assert_cli_success(&scan, "initial scan");

    // Change the worktree AND block a directory, then try to re-scan.
    fs::remove_file(repo.path().join("cached-new.txt")).unwrap();
    let locked = repo.path().join("locked");
    fs::create_dir(&locked).unwrap();
    lock_dir(&locked);
    let rescan = run_libra_command(&["status", "--scan"], repo.path());
    unlock_dir(&locked);
    fs::remove_dir(&locked).unwrap();
    assert!(!rescan.status.success(), "blocked scan must fail closed");
    assert!(
        String::from_utf8_lossy(&rescan.stderr).contains("LBR-IO-001"),
        "blocked scan carries the stable code"
    );

    // The previous snapshot is intact: the cached view still lists the row
    // recorded by the FIRST scan.
    let cached = run_libra_command(&["--json", "status", "--cached"], repo.path());
    assert_cli_success(&cached, "cached view after failed rescan");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&cached.stdout)).expect("json");
    assert!(
        doc["data"]["untracked"]
            .as_array()
            .is_some_and(|u| u.iter().any(|p| p == "cached-new.txt")),
        "the pre-block snapshot survives verbatim: {doc}"
    );
}

/// §B.3.3 dirty-cache guard: `--check-dirty` never prunes a NEW row or
/// confirms a DELETED row whose path merely became unreadable.
#[test]
#[serial]
fn check_dirty_ioblocked_does_not_mutate_cache() {
    let repo = create_repo_with_committed_file("plain.txt", "content\n");
    fs::create_dir(repo.path().join("sub")).unwrap();
    fs::write(repo.path().join("sub/cached-new.txt"), "row\n").unwrap();
    let scan = run_libra_command(&["status", "--scan"], repo.path());
    assert_cli_success(&scan, "initial scan");

    // The NEW row's parent becomes unreadable: revalidation must keep it.
    lock_dir(&repo.path().join("sub"));
    let check = run_libra_command(&["--json", "status", "--check-dirty"], repo.path());
    unlock_dir(&repo.path().join("sub"));
    assert_cli_success(&check, "check-dirty with blocked parent");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&check.stdout)).expect("json");
    assert!(
        doc["data"]["untracked"]
            .as_array()
            .is_some_and(|u| u.iter().any(|p| p == "sub/cached-new.txt")),
        "an unreadable NEW row survives revalidation: {doc}"
    );

    // And the cache still holds it afterwards (nothing was pruned).
    let cached = run_libra_command(&["--json", "status", "--cached"], repo.path());
    assert_cli_success(&cached, "cached view after blocked check-dirty");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&cached.stdout)).expect("json");
    assert!(
        doc["data"]["untracked"]
            .as_array()
            .is_some_and(|u| u.iter().any(|p| p == "sub/cached-new.txt")),
        "the cache row was neither pruned nor rewritten: {doc}"
    );
}

/// `--check-dirty`: a cached MODIFIED row whose file STATS fine but cannot
/// be READ (mode 000) is its own blocked case — the row is kept, reported
/// in `io_blocked[]`, and the cache row is not rewritten, metadata
/// included (proven by a full-row DB snapshot, not just the visible list).
#[test]
#[serial]
#[cfg(unix)]
fn check_dirty_modified_row_content_hash_failure_is_blocked_and_kept() {
    use std::os::unix::fs::PermissionsExt;

    let repo = create_repo_with_committed_file("tracked.txt", "content\n");
    fs::write(repo.path().join("tracked.txt"), "changed\n").unwrap();
    let scan = run_libra_command(&["status", "--scan"], repo.path());
    assert_cli_success(&scan, "initial scan caches the MODIFIED row");
    let rows_before = dirty_rows_for(repo.path(), "tracked.txt");
    assert_eq!(
        rows_before.len(),
        1,
        "the fixture really seeds one MODIFIED cache row: {rows_before:?}"
    );
    assert!(
        rows_before[0].contains("|modified|scan|"),
        "and it is the scan-sourced modified row: {rows_before:?}"
    );

    let file = repo.path().join("tracked.txt");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
    let check = run_libra_command(&["--json", "status", "--check-dirty"], repo.path());
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    assert_cli_success(&check, "check-dirty with an unreadable MODIFIED row");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&check.stdout)).expect("json");
    assert!(
        doc["data"]["unstaged"]["modified"]
            .as_array()
            .is_some_and(|m| m.iter().any(|p| p == "tracked.txt")),
        "the unreadable row is kept as modified (over-report), never pruned: {doc}"
    );
    assert!(
        doc["data"]["io_blocked"]
            .as_array()
            .is_some_and(|b| !b.is_empty()),
        "the content-hash failure is reported in io_blocked, not silent: {doc}"
    );

    // The whole cache row is untouched — a confirm that only flips
    // source/verified_at while keeping the visible list must fail here.
    assert_eq!(
        dirty_rows_for(repo.path(), "tracked.txt"),
        rows_before,
        "a blocked row's cache entry is untouched, metadata included"
    );
}

/// Full snapshot of the `working_dirty` cache entries for one path through
/// the single-owner cache API (NOT raw SQL — the DB layer version must not
/// leak into the assertion): every metadata field, in stable order, so a
/// blocked run that secretly rewrote row metadata (source/verified_at/
/// marked_at) cannot pass as "untouched".
#[cfg(unix)]
fn dirty_rows_for(repo: &Path, file: &str) -> Vec<String> {
    let _guard = libra::utils::test::ChangeDirGuard::new(repo);
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        let mut rows: Vec<String> = libra::internal::dirty::DirtyCache::list()
            .await
            .expect("read dirty cache")
            .into_iter()
            .filter(|entry| entry.path == file)
            .map(|entry| {
                format!(
                    "{}|{}|{}|{}|{}",
                    entry.path,
                    entry.kind,
                    entry.source,
                    entry.marked_at,
                    entry.verified_at.as_deref().unwrap_or("<null>")
                )
            })
            .collect();
        rows.sort();
        rows
    })
}

// ── R0-8: object-read fault injection, non-UTF-8 contract, cache-fallback
//    exit arbitration (§B.4.1, §B.6.1, §B.5) ─────────────────────────────────

/// Stage an inexact rename pair, then break the OLD side's blob in the object
/// store with `mutilate`. Detection must skip only that candidate (D + A stay
/// truthful, no rename) and surface one deduplicated `metadata_unavailable`
/// warning; the JSON contract reports `rename_detection_complete: false` with
/// an intact base scan.
fn assert_object_fault_skips_inexact_only(mutilate: impl FnOnce(&Path)) {
    let old_body = "object fault line one\nline two\nline three\nline four\n";
    let repo = create_repo_with_committed_file("old.txt", old_body);
    // Locate the old blob's loose object file before breaking it.
    let ls = run_libra_command(&["ls-tree", "HEAD"], repo.path());
    assert_cli_success(&ls, "ls-tree HEAD");
    let listing = String::from_utf8_lossy(&ls.stdout).into_owned();
    let hash = listing
        .lines()
        .find(|l| l.ends_with("old.txt"))
        .and_then(|l| l.split_whitespace().nth(2))
        .expect("old.txt blob hash in ls-tree output")
        .to_string();
    let object_file = repo
        .path()
        .join(".libra/objects")
        .join(&hash[0..2])
        .join(&hash[2..]);
    assert!(object_file.exists(), "loose object at {object_file:?}");

    // Inexact pair: same shape, one line changed (score < 100, > 50%).
    fs::remove_file(repo.path().join("old.txt")).unwrap();
    fs::write(
        repo.path().join("new.txt"),
        "object fault line one\nline two CHANGED\nline three\nline four\n",
    )
    .unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage inexact move");

    mutilate(&object_file);

    let out = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&out, "json status with broken object");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let staged = &doc["data"]["staged"];
    assert!(
        staged["deleted"]
            .as_array()
            .is_some_and(|d| d.iter().any(|p| p == "old.txt"))
            && staged["new"]
                .as_array()
                .is_some_and(|n| n.iter().any(|p| p == "new.txt")),
        "base D + A survive the fault: {doc}"
    );
    assert_eq!(
        doc["data"]["renames"],
        serde_json::json!([]),
        "the affected candidate is skipped, not guessed: {doc}"
    );
    let warnings = doc["data"]["warnings"].as_array().expect("warnings");
    assert_eq!(
        warnings
            .iter()
            .filter(|w| w["code"] == "metadata_unavailable")
            .count(),
        1,
        "exactly ONE deduplicated metadata warning covers every affected \
         candidate — the fixture below breaks several at once: {doc}"
    );
    assert_eq!(doc["data"]["base_scan_complete"], true, "{doc}");
    assert_eq!(doc["data"]["rename_detection_complete"], false, "{doc}");
}

/// The dedup half of §B.3.4 that a single-candidate fixture cannot show:
/// MANY candidates hit the same object fault, and they collapse into one
/// `{code, source}` warning rather than one warning per candidate — while
/// every base D/A row survives and no rename is invented.
fn assert_object_fault_warning_is_deduplicated(mutilate: impl Fn(&Path)) {
    const PAIRS: usize = 4;
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let body = |i: usize| format!("fault {i} line one\nline two\nline three\nline four\n");
    for i in 0..PAIRS {
        fs::write(repo.path().join(format!("old{i}.txt")), body(i)).unwrap();
    }
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the originals");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit the originals");

    let ls = run_libra_command(&["ls-tree", "HEAD"], repo.path());
    assert_cli_success(&ls, "ls-tree HEAD");
    let listing = String::from_utf8_lossy(&ls.stdout).into_owned();
    let mut objects = Vec::new();
    for i in 0..PAIRS {
        let name = format!("old{i}.txt");
        let hash = listing
            .lines()
            .find(|l| l.ends_with(&name))
            .and_then(|l| l.split_whitespace().nth(2))
            .unwrap_or_else(|| panic!("blob hash for {name}: {listing}"))
            .to_string();
        objects.push(
            repo.path()
                .join(".libra/objects")
                .join(&hash[0..2])
                .join(&hash[2..]),
        );
        fs::remove_file(repo.path().join(&name)).unwrap();
        fs::write(
            repo.path().join(format!("new{i}.txt")),
            body(i).replace("line two\n", "line two CHANGED\n"),
        )
        .unwrap();
    }
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the inexact moves");
    for object in &objects {
        mutilate(object);
    }

    let out = run_libra_command(&["--json", "status"], repo.path());
    for object in &objects {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(object, fs::Permissions::from_mode(0o644));
        }
    }
    assert_cli_success(&out, "json status with several broken objects");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert_eq!(
        doc["data"]["renames"],
        serde_json::json!([]),
        "no rename is guessed for any broken candidate: {doc}"
    );
    for i in 0..PAIRS {
        assert!(
            doc["data"]["staged"]["deleted"]
                .as_array()
                .is_some_and(|d| d.iter().any(|p| p == &format!("old{i}.txt")))
                && doc["data"]["staged"]["new"]
                    .as_array()
                    .is_some_and(|n| n.iter().any(|p| p == &format!("new{i}.txt"))),
            "every base D + A row survives: {doc}"
        );
    }
    let warnings = doc["data"]["warnings"].as_array().expect("warnings");
    let metadata: Vec<_> = warnings
        .iter()
        .filter(|w| w["source"] == "metadata")
        .collect();
    assert_eq!(
        metadata.len(),
        1,
        "{PAIRS} candidates with the same fault collapse into ONE metadata warning: {doc}"
    );
    assert_eq!(doc["data"]["base_scan_complete"], true, "{doc}");
    assert_eq!(doc["data"]["rename_detection_complete"], false, "{doc}");
}

/// §B.5 "one warning per {code, source} for the whole run" — the case a
/// single-sided fixture cannot show. Staged AND unstaged detection each drop
/// candidates to the same fault class; because each side's message embeds
/// its own count, emitting per side would produce two warnings with the same
/// code and source and different numbers.
#[test]
fn object_fault_warning_is_deduplicated_across_both_sides() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    enable_rename_untracked(repo.path());
    let body = |i: usize| format!("both sides {i}\nline two\nline three\nline four\n");
    // Two files that will move with a STAGED rename, one with an UNSTAGED
    // (worktree-only) move — so both detection sides run.
    for name in ["s0.txt", "s1.txt", "u0.txt"] {
        fs::write(repo.path().join(name), body(0)).unwrap();
    }
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the fixture");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit the fixture");

    // Break every HEAD blob so both sides hit the same object fault.
    let ls = run_libra_command(&["ls-tree", "HEAD"], repo.path());
    assert_cli_success(&ls, "ls-tree HEAD");
    let listing = String::from_utf8_lossy(&ls.stdout).into_owned();
    let mut objects = Vec::new();
    for line in listing.lines() {
        if let Some(hash) = line.split_whitespace().nth(2) {
            objects.push(
                repo.path()
                    .join(".libra/objects")
                    .join(&hash[0..2])
                    .join(&hash[2..]),
            );
        }
    }

    // Staged moves (two candidates).
    for i in 0..2 {
        fs::remove_file(repo.path().join(format!("s{i}.txt"))).unwrap();
        fs::write(repo.path().join(format!("s{i}-moved.txt")), body(i + 1)).unwrap();
    }
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the moves");
    // Unstaged move (one candidate), left out of the index.
    fs::remove_file(repo.path().join("u0.txt")).unwrap();
    fs::write(repo.path().join("u0-moved.txt"), body(9)).unwrap();

    for object in &objects {
        let _ = fs::remove_file(object);
    }

    let out = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&out, "json status with faults on both sides");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let warnings = doc["data"]["warnings"].as_array().expect("warnings");
    let mut seen: Vec<(String, String)> = warnings
        .iter()
        .map(|w| {
            (
                w["code"].as_str().unwrap_or_default().to_string(),
                w["source"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let before = seen.len();
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen.len(),
        before,
        "no {{code, source}} pair may appear twice across the two detection sides: {doc}"
    );
}

/// §B.3.4 dedup, Missing arm.
#[test]
fn object_read_missing_warning_is_deduplicated() {
    assert_object_fault_warning_is_deduplicated(|object_file| {
        fs::remove_file(object_file).unwrap();
    });
}

/// §B.3.4 dedup, Corrupt arm.
#[test]
fn object_read_corrupt_warning_is_deduplicated() {
    assert_object_fault_warning_is_deduplicated(|object_file| {
        fs::write(object_file, b"this is not a zlib object payload").unwrap();
    });
}

/// §B.3.4 dedup, Unavailable arm.
#[test]
#[cfg(unix)]
fn object_read_unavailable_warning_is_deduplicated() {
    use std::os::unix::fs::PermissionsExt;

    assert_object_fault_warning_is_deduplicated(|object_file| {
        fs::set_permissions(object_file, fs::Permissions::from_mode(0o000)).unwrap();
    });
}

/// §B.4.1 Missing: a deleted loose object only skips its inexact candidate.
#[test]
fn object_read_missing_blob_skips_inexact_only() {
    assert_object_fault_skips_inexact_only(|object_file| {
        fs::remove_file(object_file).unwrap();
    });
}

/// §B.4.1 Corrupt: garbage object bytes only skip the dependent candidate.
#[test]
fn object_read_corrupt_blob_skips_inexact_only() {
    assert_object_fault_skips_inexact_only(|object_file| {
        fs::write(object_file, b"this is not a zlib object payload").unwrap();
    });
}

/// The `raw_base64` encoding itself, pinned on BOTH platforms through the
/// public helper the JSON envelope uses. The end-to-end test above needs an
/// unreadable path with a non-UTF-8 name, which Windows cannot produce
/// without ACL manipulation — so the encoding contract is asserted directly:
/// Unix raw `OsStr` bytes, Windows UTF-16 code units little-endian
/// (including an unpaired surrogate, the case with no UTF-8 form at all).
#[test]
fn raw_base64_encodes_platform_native_units() {
    use base64::Engine as _;
    use libra::command::status::raw_path_base64;

    // A valid-UTF-8 name needs no raw form: `display` is already lossless.
    assert_eq!(raw_path_base64(Path::new("plain.txt")), None);

    #[cfg(unix)]
    {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let name = OsStr::from_bytes(b"bad\xffname");
        assert_eq!(
            raw_path_base64(Path::new(name)),
            Some(base64::engine::general_purpose::STANDARD.encode(b"bad\xffname")),
            "Unix encodes the raw OsStr bytes"
        );
    }
    #[cfg(windows)]
    {
        use std::{
            ffi::OsString,
            os::windows::ffi::{OsStrExt, OsStringExt},
        };
        // b <unpaired surrogate> k — representable as UTF-16, never as UTF-8.
        let units = [0x0062u16, 0xD800, 0x006B];
        let name = OsString::from_wide(&units);
        let mut expected_bytes = Vec::new();
        for unit in name.as_os_str().encode_wide() {
            expected_bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(
            raw_path_base64(Path::new(&name)),
            Some(base64::engine::general_purpose::STANDARD.encode(&expected_bytes)),
            "Windows encodes UTF-16 code units little-endian"
        );
        // And it really round-trips back to the original units.
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(raw_path_base64(Path::new(&name)).expect("encoded"))
            .expect("valid base64");
        let recovered: Vec<u16> = decoded
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(recovered, units, "the exact code units are recoverable");
    }
}

/// §B.3.3 cache fail-closed, across ALL row kinds: a `--check-dirty` run
/// that cannot re-verify a row must leave the cache byte-identical. The
/// existing coverage only pinned a NEW row; DELETED and manual `unknown`
/// marks take different code paths and each had its own way to fabricate a
/// verdict (EACCES read as "gone", or a manual mark pruned as clean).
#[test]
#[cfg(unix)]
fn check_dirty_blocked_rows_never_mutate_the_cache() {
    use std::os::unix::fs::PermissionsExt;

    // DELETED row: committed, then removed, then its PARENT locked so the
    // re-verification cannot even stat it.
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::create_dir_all(repo.path().join("holder")).unwrap();
    fs::write(repo.path().join("holder/gone.txt"), "content\n").unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the fixture");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit the fixture");
    fs::remove_file(repo.path().join("holder/gone.txt")).unwrap();
    let scan = run_libra_command(&["status", "--scan"], repo.path());
    assert_cli_success(&scan, "seed the cache with a deletion");
    let cached_before = status_stdout(repo.path(), &["--json", "status", "--cached"]);

    let holder = repo.path().join("holder");
    fs::set_permissions(&holder, fs::Permissions::from_mode(0o000)).unwrap();
    let checked = run_libra_command(&["--json", "status", "--check-dirty"], repo.path());
    fs::set_permissions(&holder, fs::Permissions::from_mode(0o755)).unwrap();
    assert_cli_success(&checked, "check-dirty reports the partial result");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&checked.stdout)).expect("json");
    assert!(
        !doc["data"]["io_blocked"]
            .as_array()
            .expect("io_blocked")
            .is_empty(),
        "the unverifiable deletion is reported blocked: {doc}"
    );
    assert_eq!(
        doc["stale_paths"],
        serde_json::json!(null),
        "nothing is pruned from a run that could not inspect everything: {doc}"
    );
    let cached_after = status_stdout(repo.path(), &["--json", "status", "--cached"]);
    assert_eq!(
        cached_before, cached_after,
        "the cached snapshot is byte-identical after a blocked re-verification"
    );

    // Manual `unknown` mark: `libra dirty` records a path with no kind, so
    // classification runs the tri-state stat + content confirm.
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::create_dir_all(repo.path().join("manual")).unwrap();
    fs::write(repo.path().join("manual/marked.txt"), "content\n").unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the fixture");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit the fixture");
    let scan = run_libra_command(&["status", "--scan"], repo.path());
    assert_cli_success(&scan, "seed the cache");
    let mark = run_libra_command(&["dirty", "manual/marked.txt"], repo.path());
    if !mark.status.success() {
        eprintln!("skipped (libra dirty mark unavailable)");
        return;
    }
    let cached_before = status_stdout(repo.path(), &["--json", "status", "--cached"]);
    let marked = repo.path().join("manual/marked.txt");
    fs::set_permissions(&marked, fs::Permissions::from_mode(0o000)).unwrap();
    let checked = run_libra_command(&["--json", "status", "--check-dirty"], repo.path());
    fs::set_permissions(&marked, fs::Permissions::from_mode(0o644)).unwrap();
    assert_cli_success(&checked, "check-dirty over a blocked manual mark");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&checked.stdout)).expect("json");
    assert!(
        !doc["data"]["io_blocked"]
            .as_array()
            .expect("io_blocked")
            .is_empty(),
        "the unverifiable manual mark is reported blocked: {doc}"
    );
    let cached_after = status_stdout(repo.path(), &["--json", "status", "--cached"]);
    assert_eq!(
        cached_before, cached_after,
        "a blocked manual mark is never pruned as clean"
    );
}

/// §B.3.2 explicitly FORBIDS applying the probe's budgets to the main
/// display scan: the probe is bounded because it is an optional
/// rename-detection input, while `??` reporting must be complete. A tree
/// wider than the probe's test-injected budget must still list every
/// untracked file, with no `probe_truncated` warning in sight.
#[test]
fn scan_complete_reports_all_untracked() {
    const FILES: usize = 60;

    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    let add = run_libra_command(&["add", "base.txt"], repo.path());
    assert_cli_success(&add, "stage base");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit base");
    for i in 0..FILES {
        fs::write(repo.path().join(format!("untracked-{i:03}.txt")), "x\n").unwrap();
    }

    // The probe budgets are squeezed far below the file count. They bound
    // the PROBE only; the display scan must ignore them entirely.
    let out = run_libra_command_with_stdin_and_env(
        &["--json", "status", "-uall"],
        repo.path(),
        "",
        &[
            ("LIBRA_TEST_STATUS_PROBE_ENUM_BUDGET", "3"),
            ("LIBRA_TEST_STATUS_PROBE_DEST_BUDGET", "1"),
        ],
    );
    assert_cli_success(&out, "status under squeezed probe budgets");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let untracked = doc["data"]["untracked"].as_array().expect("untracked");
    for i in 0..FILES {
        let name = format!("untracked-{i:03}.txt");
        assert!(
            untracked.iter().any(|p| p == &name),
            "the display scan reports EVERY untracked file, budgets or not: {name} missing"
        );
    }
    assert!(
        doc["data"]["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().all(|x| x["code"] != "probe_truncated")),
        "no probe budget applies to the display scan: {doc}"
    );
    assert_eq!(
        doc["data"]["base_scan_complete"], true,
        "the base scan is complete: {doc}"
    );
}

/// §B.6.0.1 completeness matrix: `base_scan_complete` and
/// `rename_detection_complete` report DIFFERENT subsystems, and `complete`
/// is their AND. A base-scan block must not also claim the rename pairing
/// degraded (it was never attempted), and a probe-side degradation must not
/// claim the base scan was incomplete — a consumer uses the two flags to
/// decide whether to trust the `??` list or the `renames[]` list.
#[test]
#[cfg(unix)]
fn json_base_and_rename_completeness_matrix() {
    use std::os::unix::fs::PermissionsExt;

    let flags = |doc: &serde_json::Value| -> (bool, bool, bool) {
        (
            doc["data"]["base_scan_complete"].as_bool().expect("base"),
            doc["data"]["rename_detection_complete"]
                .as_bool()
                .expect("rename"),
            doc["data"]["complete"].as_bool().expect("complete"),
        )
    };

    // (1) Nothing blocked: all three true.
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    let doc: serde_json::Value =
        serde_json::from_str(&status_stdout(repo.path(), &["--json", "status"])).expect("json");
    assert_eq!(
        flags(&doc),
        (true, true, true),
        "a clean run reports everything complete: {doc}"
    );

    // (2) BASE scan blocked only: rename detection was never degraded, so
    // only the base flag flips.
    let repo = create_repo_with_committed_file("tracked.txt", "content\n");
    fs::write(repo.path().join("tracked.txt"), "changed\n").unwrap();
    let locked = repo.path().join("tracked.txt");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let out = run_libra_command(&["--json", "status"], repo.path());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
    assert_cli_success(&out, "json status with a blocked tracked file");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert_eq!(
        flags(&doc),
        (false, true, false),
        "a base-scan block must NOT claim the rename pairing degraded: {doc}"
    );

    // (3) RENAME side degraded only: a rename-limit trip leaves the base
    // scan complete while marking detection incomplete.
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    for i in 0..1001 {
        fs::write(repo.path().join(format!("f{i}.txt")), format!("base {i}\n")).unwrap();
    }
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the wide set");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit the wide set");
    for i in 0..1001 {
        fs::remove_file(repo.path().join(format!("f{i}.txt"))).unwrap();
        fs::write(
            repo.path().join(format!("g{i}.txt")),
            format!("base {i} edited\n"),
        )
        .unwrap();
    }
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the wide move");
    let doc: serde_json::Value =
        serde_json::from_str(&status_stdout(repo.path(), &["--json", "status"])).expect("json");
    assert_eq!(
        flags(&doc),
        (true, false, false),
        "a rename-side degradation leaves the base scan complete: {doc}"
    );
}

/// §B.5 "no stderr-only bypass", the invariant form: under `--json
/// --exit-code-on-warning`, exit 9 ALWAYS corresponds to at least one entry
/// in `data.warnings[]`. A repository-level preflight advisory (raised
/// before the command runs, outside `status`'s own warning list) previously
/// tripped the exit while leaving the structured list empty, which forced
/// JSON consumers to scrape stderr to learn why their exit code changed.
#[test]
fn json_exit_nine_always_has_a_structured_warning() {
    let repo = create_repo_with_committed_file("plain.txt", "content\n");
    fs::write(repo.path().join("plain.txt"), "changed\n").unwrap();

    // Inject a genuine PREFLIGHT warning (raised before the command runs, so
    // it has no command-owned warning list of its own). Without it this test
    // would only ever exercise repositories that happen to be warning-free,
    // and could not catch a stderr-only leak.
    let preflight = [(
        "LIBRA_TEST_PREFLIGHT_WARNING",
        "injected preflight advisory for the delivery-matrix regression",
    )];
    for flags in [
        vec!["--json", "--exit-code-on-warning", "status"],
        vec!["--json", "--exit-code-on-warning", "status", "--exit-code"],
        vec![
            "--json",
            "--exit-code-on-warning",
            "status",
            "--check-dirty",
            "--exit-code",
        ],
    ] {
        let out = run_libra_command_with_stdin_and_env(&flags, repo.path(), "", &preflight);
        assert_eq!(
            out.status.code(),
            Some(9),
            "{flags:?}: an injected preflight warning must trip the warning exit"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).trim().is_empty(),
            "{flags:?}: JSON mode delivers the warning through the envelope, \
             never on stderr: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        let doc: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json envelope");
        assert!(
            doc["data"]["warnings"].as_array().is_some_and(|w| w
                .iter()
                .any(|x| x["code"] == "repository_preflight" && x["source"] == "config")),
            "{flags:?}: the preflight advisory rides in data.warnings[]: {doc}"
        );

        // Text mode still prints it on stderr (that is the whole point of
        // the split).
        let mut text_flags = flags.clone();
        text_flags.retain(|f| *f != "--json");
        let text = run_libra_command_with_stdin_and_env(&text_flags, repo.path(), "", &preflight);
        assert!(
            String::from_utf8_lossy(&text.stderr).contains("injected preflight advisory"),
            "{text_flags:?}: text mode keeps the stderr diagnostic: {:?}",
            String::from_utf8_lossy(&text.stderr)
        );

        let out = run_libra_command(&flags, repo.path());
        let doc: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json envelope");
        let warnings = doc["data"]["warnings"].as_array().expect("warnings");
        if out.status.code() == Some(9) {
            assert!(
                !warnings.is_empty(),
                "{flags:?} exited 9 with an EMPTY structured warning list; \
                 stderr said: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            // Every entry is a real schema member, not a synthesized blob.
            for warning in warnings {
                assert!(
                    warning["code"].as_str().is_some()
                        && warning["source"].as_str().is_some()
                        && warning["message"].as_str().is_some(),
                    "{flags:?} produced a malformed warning: {warning}"
                );
            }
        }
    }
}

/// §B.3.2 relevance test, glob edge: `:(glob)wanted/*.txt` never matches the
/// DIRECTORY `wanted`, only files inside it. Judging an EACCES by whether
/// the directory itself matches would therefore discard the block on the one
/// subtree the caller asked about — reporting "no renames here" for a
/// directory that could not be read at all.
#[test]
#[cfg(unix)]
fn probe_glob_pathspec_keeps_blocked_directory_relevant() {
    use std::os::unix::fs::PermissionsExt;

    let repo = repo_with_worktree_move("wanted/dest.txt");
    enable_rename_untracked(repo.path());
    let wanted = repo.path().join("wanted");
    fs::set_permissions(&wanted, fs::Permissions::from_mode(0o000)).unwrap();

    let out = run_libra_command(
        &[
            "--json",
            "status",
            "--",
            ":(glob)wanted/*.txt",
            "moved-src.txt",
        ],
        repo.path(),
    );
    let text = run_libra_command(
        &[
            "status",
            "--porcelain",
            "--",
            ":(glob)wanted/*.txt",
            "moved-src.txt",
        ],
        repo.path(),
    );
    fs::set_permissions(&wanted, fs::Permissions::from_mode(0o755)).unwrap();

    assert_cli_success(&out, "json reports the partial result");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        !doc["data"]["io_blocked"]
            .as_array()
            .expect("io_blocked")
            .is_empty(),
        "a directory whose CONTENTS match the glob stays relevant when blocked: {doc}"
    );
    assert_eq!(
        doc["data"]["complete"], false,
        "and the run is not claimed complete: {doc}"
    );
    assert!(
        !text.status.success(),
        "text formats fail closed rather than reporting no renames: {}",
        String::from_utf8_lossy(&text.stderr)
    );
}

/// §B.3.1.1: a probe root that is a FILE or a SYMLINK is a candidate in its
/// own right, not a directory to descend into. Probing repository markers
/// under such a root asks the filesystem for `dest.txt/.libra`, which
/// answers `ENOTDIR` — mistaking that for "unreadable" would block a
/// perfectly good destination and fail text status closed. A directory
/// symlink must also stay a leaf: the marker lookup must not dereference it.
#[test]
fn probe_direct_file_and_symlink_roots_are_candidates() {
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());

    // A file named directly as the pathspec.
    let out = run_libra_command(
        &["--json", "status", "--", "dest.txt", "moved-src.txt"],
        repo.path(),
    );
    assert_cli_success(&out, "status narrowed to a file root");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["io_blocked"]
            .as_array()
            .expect("io_blocked")
            .is_empty(),
        "a file root is never 'unreadable' just because it has no markers: {doc}"
    );
    assert!(
        doc["data"]["renames"]
            .as_array()
            .is_some_and(|r| r.iter().any(|x| x["to"] == "dest.txt")),
        "the file root still pairs its rename: {doc}"
    );

    // A directory symlink named directly as the pathspec stays a leaf.
    #[cfg(unix)]
    {
        let repo = repo_with_worktree_move("real/dest.txt");
        enable_rename_untracked(repo.path());
        std::os::unix::fs::symlink("real", repo.path().join("link")).unwrap();
        let out = run_libra_command(&["--json", "status", "--", "link"], repo.path());
        assert_cli_success(&out, "status narrowed to a symlink root");
        let doc: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
        assert!(
            doc["data"]["io_blocked"]
                .as_array()
                .expect("io_blocked")
                .is_empty(),
            "a symlink root is a leaf, not a blocked directory: {doc}"
        );
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("link/dest.txt"),
            "and it is never dereferenced into: {doc}"
        );
    }
}

/// §B.6.1 "a non-UTF-8 name never fails status" — including the cache
/// modes. `--scan` rebuilds the dirty cache from its own snapshot walk, and
/// that walk used to reject an undecodable path outright, making `--scan`
/// the one mode a repository containing such a file could never run.
#[test]
#[cfg(unix)]
fn scan_survives_non_utf8_untracked_path() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let repo = create_repo_with_committed_file("plain.txt", "content\n");
    if !fs_supports_non_utf8_names(repo.path()) {
        return;
    }
    fs::write(
        repo.path().join(OsStr::from_bytes(b"bad\xffname.txt")),
        "x\n",
    )
    .unwrap();

    for flags in [
        vec!["status", "--scan"],
        vec!["--json", "status", "--scan"],
        vec!["--json", "status", "--cached"],
    ] {
        let out = run_libra_command(&flags, repo.path());
        assert!(
            out.status.success(),
            "{flags:?} must not fail on an undecodable untracked name: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // And the base status still lists it (the `??` row is unaffected).
    let short = status_stdout(repo.path(), &["status", "--short"]);
    assert!(
        short.contains("bad") && short.contains("name.txt"),
        "the undecodable path keeps its untracked row: {short:?}"
    );

    // JSON must keep it DISTINGUISHABLE: `Path::display()` would replace the
    // undecodable byte with U+FFFD, collapsing two different real filenames
    // onto one JSON string. The escaped form the docs promise is used
    // instead.
    fs::write(
        repo.path().join(OsStr::from_bytes(b"bad\xfename.txt")),
        "y\n",
    )
    .unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&status_stdout(repo.path(), &["--json", "status", "-uall"]))
            .expect("json");
    let untracked: Vec<&str> = doc["data"]["untracked"]
        .as_array()
        .expect("untracked")
        .iter()
        .filter_map(|p| p.as_str())
        .collect();
    assert!(
        untracked.iter().any(|p| p.contains("\\377")),
        "the 0xFF byte renders as an octal escape: {untracked:?}"
    );
    assert!(
        untracked.iter().any(|p| p.contains("\\376")),
        "and the 0xFE byte renders distinctly: {untracked:?}"
    );
    assert!(
        !untracked.iter().any(|p| p.contains('\u{FFFD}')),
        "no path is lossily replaced: {untracked:?}"
    );
}

/// §B.3.2 escape guard: a pathspec whose intermediate component is a symlink
/// pointing OUTSIDE the worktree must never let the probe read out there.
/// `symlink_metadata` does not follow the final component but does follow
/// every intermediate one, so a lexical containment check would wave
/// `link/child` through. The resolved check catches it, and the escape is
/// REPORTED (`io_blocked`), never silently skipped — a silent skip is
/// indistinguishable from "this directory is empty".
#[test]
#[cfg(unix)]
fn probe_symlinked_escape_is_reported_not_followed() {
    let outside = tempdir().expect("outside dir");
    fs::create_dir_all(outside.path().join("child")).unwrap();
    fs::write(
        outside.path().join("child/decoy.txt"),
        "probe rename content\nline two\n",
    )
    .unwrap();

    // The external directory ALSO holds a repository marker. Without the
    // containment check running first, the marker probe would "exclude a
    // nested repository" and move on — a silent skip that looks identical to
    // "nothing here" while the escape went unreported.
    fs::create_dir_all(outside.path().join("child/.git")).unwrap();

    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    std::os::unix::fs::symlink(outside.path(), repo.path().join("link")).unwrap();

    let out = run_libra_command(&["--json", "status", "--", "link/child"], repo.path());
    assert_cli_success(&out, "status narrowed to a path behind an escaping symlink");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("decoy"),
        "content outside the worktree is never enumerated: {doc}"
    );
    assert!(
        doc["data"]["renames"]
            .as_array()
            .is_some_and(|r| r.is_empty()),
        "and nothing out there becomes a rename destination: {doc}"
    );
    // The escape must be REPORTED, not quietly skipped: a silent skip reads
    // exactly like "this directory is empty" to every consumer, which is the
    // failure mode the guard exists to prevent.
    assert!(
        doc["data"]["io_blocked"]
            .as_array()
            .is_some_and(|events| events.iter().any(|e| e["path"]["display"]
                .as_str()
                .is_some_and(|p| p.contains("link")))),
        "the escaping path is recorded in io_blocked[]: {doc}"
    );
    assert_eq!(
        doc["data"]["complete"], false,
        "and the run is therefore not complete: {doc}"
    );
    // Text formats fail closed on the same request.
    let text = run_libra_command(&["status", "--porcelain", "--", "link/child"], repo.path());
    assert!(
        !text.status.success(),
        "text formats fail closed on an escaping root: {}",
        String::from_utf8_lossy(&text.stderr)
    );
}

/// §B.3.1.2: a case-fold ALIAS of a tracked path never qualifies as a
/// rename destination. On a case-insensitive filesystem `README.md` and
/// `readme.md` are the same file, so pairing a deletion with its own alias
/// would invent a rename out of a case change.
#[test]
fn probe_dest_rejects_casefold_alias() {
    let repo = tempdir().expect("temp repo");
    // The alias filter asks the FILESYSTEM whether two spellings name the
    // same file (a fold-key match alone is not proof). On a case-sensitive
    // filesystem the situation under test cannot be constructed at all, so
    // the case is gated rather than asserted vacuously.
    fs::write(repo.path().join("CaseProbe"), "x").unwrap();
    let case_insensitive = repo.path().join("caseprobe").exists();
    fs::remove_file(repo.path().join("CaseProbe")).unwrap();
    if !case_insensitive {
        eprintln!("skipped (case-sensitive filesystem: a case-fold alias is a distinct file)");
        return;
    }
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let body = "alias content\nline two\n";
    fs::write(repo.path().join("Keep.txt"), body).unwrap();
    fs::write(repo.path().join("gone.txt"), body).unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the fixture");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit the fixture");
    enable_rename_untracked(repo.path());
    let cfg = run_libra_command(&["config", "core.ignorecase", "true"], repo.path());
    assert_cli_success(&cfg, "enable ignorecase");

    // `gone.txt` is deleted (a rename SOURCE). `keep.txt` is a case-fold
    // alias of the still-tracked `Keep.txt`, with identical content — the
    // most tempting possible destination, and exactly the one that must be
    // refused.
    fs::remove_file(repo.path().join("gone.txt")).unwrap();
    fs::write(repo.path().join("keep.txt"), body).unwrap();

    let out = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&out, "json status with a case-fold alias");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["renames"]
            .as_array()
            .is_some_and(|r| r.iter().all(|x| x["to"] != "keep.txt")),
        "a case-fold alias of a tracked path is never a rename destination: {doc}"
    );
    assert!(
        doc["data"]["unstaged"]["deleted"]
            .as_array()
            .is_some_and(|d| d.iter().any(|p| p == "gone.txt")),
        "the deletion is reported plainly instead: {doc}"
    );
}

/// §B.3.2: an IGNORED subtree is pruned BEFORE its contents are enumerated,
/// so its files never consume the enumeration budget. Squeezing the budget
/// below the ignored tree's size must still leave the real candidate
/// pairable — proof the ignored entries were never counted.
#[test]
fn probe_ignored_subtree_pruned_before_enumeration() {
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    fs::create_dir_all(repo.path().join("ignored")).unwrap();
    for i in 0..40 {
        fs::write(repo.path().join(format!("ignored/f{i}.txt")), "x\n").unwrap();
    }
    fs::write(repo.path().join(".libraignore"), "ignored/\n").unwrap();

    let out = run_libra_command_with_stdin_and_env(
        &["--json", "status"],
        repo.path(),
        "",
        // Far below the 40 ignored files; ample for the handful of real ones.
        &[("LIBRA_TEST_STATUS_PROBE_ENUM_BUDGET", "12")],
    );
    assert_cli_success(&out, "status with a large ignored subtree");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["renames"]
            .as_array()
            .is_some_and(|r| r.iter().any(|x| x["to"] == "dest.txt")),
        "the ignored subtree never consumed the budget: {doc}"
    );
    assert!(
        doc["data"]["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().all(|x| x["code"] != "probe_truncated")),
        "and the budget was therefore never tripped: {doc}"
    );
}

/// §B.3.2: the enumeration budget is aggregated ACROSS probe roots, not
/// per-root — two sibling roots that each fit individually must still trip
/// it together, or a wide repository could evade the bound by fanning out.
#[test]
fn probe_budget_aggregates_across_sibling_roots() {
    let repo = repo_with_worktree_move("a/dest.txt");
    enable_rename_untracked(repo.path());
    fs::create_dir_all(repo.path().join("b")).unwrap();
    for i in 0..8 {
        fs::write(repo.path().join(format!("a/noise{i}.txt")), "x\n").unwrap();
        fs::write(repo.path().join(format!("b/noise{i}.txt")), "x\n").unwrap();
    }

    // 12 fits either root alone (9 entries) but not both (18).
    let out = run_libra_command_with_stdin_and_env(
        &["--json", "status", "--", "a", "b", "moved-src.txt"],
        repo.path(),
        "",
        &[("LIBRA_TEST_STATUS_PROBE_ENUM_BUDGET", "12")],
    );
    assert_cli_success(&out, "status across two sibling roots");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|x| x["code"] == "probe_truncated")),
        "the budget is shared across roots, so two fitting roots still trip it: {doc}"
    );
}

/// §B.3.5: a directory whose candidates were only PARTLY consumed keeps its
/// `? dir/` marker — collapsing it would claim the remaining untracked file
/// had been accounted for by a rename.
#[test]
fn probe_marker_kept_when_directory_has_unconsumed_candidates() {
    let repo = repo_with_worktree_move("bucket/dest.txt");
    enable_rename_untracked(repo.path());
    // A second, unrelated untracked file in the same directory.
    fs::write(repo.path().join("bucket/leftover.txt"), "unrelated\n").unwrap();

    let out = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&out, "json status with a partly consumed directory");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["renames"]
            .as_array()
            .is_some_and(|r| r.iter().any(|x| x["to"] == "bucket/dest.txt")),
        "the rename is still detected: {doc}"
    );
    let untracked = doc["data"]["untracked"].as_array().expect("untracked");
    assert!(
        untracked
            .iter()
            .any(|p| p == "bucket/" || p == "bucket/leftover.txt"),
        "the directory is still represented because a candidate remains: {doc}"
    );
}

/// §B.6.0.1 for `--scan` WITH a pathspec: the cache is a whole-repository
/// snapshot, so `--scan docs` still walks the whole tree to rebuild it. A
/// path blocked OUTSIDE the pathspec is therefore discovered by that second
/// pass — and must take the same route as any other block: `--json` reports
/// the partial result with `cache_written: false`, text fails closed. A
/// fatal from inside the snapshot collection would bypass `io_blocked[]`,
/// the warnings and the completeness flags entirely.
#[test]
#[cfg(unix)]
fn json_scan_with_pathspec_reports_blocks_outside_it() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::create_dir_all(repo.path().join("docs")).unwrap();
    fs::write(repo.path().join("docs/readme.md"), "docs\n").unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the fixture");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit the fixture");
    fs::create_dir_all(repo.path().join("elsewhere")).unwrap();
    fs::write(repo.path().join("elsewhere/inner.txt"), "x\n").unwrap();
    let blocked = repo.path().join("elsewhere");
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();

    let out = run_libra_command(&["--json", "status", "--scan", "--", "docs"], repo.path());
    let text = run_libra_command(&["status", "--scan", "--", "docs"], repo.path());
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o755)).unwrap();

    assert_cli_success(&out, "json scan reports the partial result");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert_eq!(
        doc["data"]["cache_written"], false,
        "the cache is untouched: {doc}"
    );
    assert!(
        doc["data"]["io_blocked"]
            .as_array()
            .is_some_and(|events| events.iter().any(|e| e["path"]["display"] == "elsewhere")),
        "a block outside the pathspec is still reported: {doc}"
    );
    assert!(
        doc["data"]["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|x| x["source"] == "worktree")),
        "with its structured warning: {doc}"
    );
    assert_eq!(doc["data"]["base_scan_complete"], false, "{doc}");
    assert!(
        !text.status.success(),
        "text still fails closed: {}",
        String::from_utf8_lossy(&text.stderr)
    );
}

/// §B.6.1 "a non-UTF-8 name never fails status", TRACKED arm: the earlier
/// coverage only exercised untracked names, so a repository that had such a
/// file COMMITTED could still be rejected outright. Every mode must keep
/// answering; only rename candidacy is given up.
#[test]
#[cfg(unix)]
fn tracked_non_utf8_path_never_fails_status() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    if !fs_supports_non_utf8_names(repo.path()) {
        return;
    }
    let name = OsStr::from_bytes(b"tracked\xffname.txt");
    fs::write(repo.path().join(name), "committed\n").unwrap();
    fs::write(repo.path().join("plain.txt"), "plain\n").unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    if !add.status.success() {
        // Staging such a name may itself be refused; the point of this test
        // is the READ path, which is then unreachable.
        eprintln!("skipped (this build refuses to stage a non-UTF-8 path)");
        return;
    }
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit the non-UTF-8 path");

    // Modify it so the tracked comparison actually runs.
    fs::write(repo.path().join(name), "committed and changed\n").unwrap();

    for flags in [
        vec!["status"],
        vec!["status", "--short"],
        vec!["status", "--porcelain"],
        vec!["--json", "status"],
        vec!["status", "--scan"],
        vec!["--json", "status", "--cached"],
    ] {
        let out = run_libra_command(&flags, repo.path());
        assert!(
            out.status.success(),
            "{flags:?} must keep answering with a tracked non-UTF-8 path: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// §B.6.0.1 schema, the branches the permission-denied snapshot cannot
/// reach: a NON-NULL `rename` object (`{from, to, score}`) and the `staged`
/// component of a blocked rename destination. Without these the nested
/// rename shape is unpinned, and a renamed-but-unreadable destination could
/// silently lose its association.
#[test]
#[cfg(unix)]
fn json_io_blocked_rename_branch_schema() {
    use std::os::unix::fs::PermissionsExt;

    let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let repo = create_repo_with_committed_file("orig.txt", &base);
    let mv = run_libra_command(&["mv", "orig.txt", "moved.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");
    let edited = base.replace("line 5\n", "line five changed\n");
    fs::write(repo.path().join("moved.txt"), &edited).unwrap();
    let add = run_libra_command(&["add", "moved.txt"], repo.path());
    assert_cli_success(&add, "restage the inexact rename");
    // Now make the staged rename's DESTINATION unreadable.
    let dest = repo.path().join("moved.txt");
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o000)).unwrap();

    let out = run_libra_command(&["--json", "status"], repo.path());
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o644)).unwrap();
    assert_cli_success(&out, "json status with a blocked rename destination");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let entry = doc["data"]["io_blocked"]
        .as_array()
        .expect("io_blocked")
        .iter()
        .find(|e| e["path"]["display"] == "moved.txt")
        .unwrap_or_else(|| panic!("the blocked destination is reported: {doc}"));

    // This STAGED pairing reads both blobs from the OBJECT STORE
    // (HEAD + index, both KnownObjectId) — the chmod blocks only the
    // worktree stat, so the association can never legitimately drop here.
    // The old null-escape let a regression that loses every blocked-path
    // rename association pass vacuously (2026-08-06 R0-7 review).
    assert!(
        !entry["rename"].is_null(),
        "the object-store-backed staged pairing must survive a blocked \
         worktree destination: {entry}"
    );
    let rename = entry["rename"].as_object().expect("rename object");
    let keys: Vec<&str> = rename.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec!["from", "score", "to"],
        "the nested rename object's field names are frozen: {entry}"
    );
    assert_eq!(rename["from"], "orig.txt", "{entry}");
    assert_eq!(rename["to"], "moved.txt", "{entry}");
    assert!(
        rename["score"].as_u64().is_some_and(|s| s <= 100),
        "score is a 0..=100 integer: {entry}"
    );
    assert_eq!(
        entry["staged"], "R",
        "a blocked rename destination reports its staged component: {entry}"
    );
}

/// §B.6.0.1 / lore.md 1.1: the dirty cache stores paths by KIND and has no
/// row for a rename pair. The snapshot walk therefore runs with rename
/// detection off — otherwise pairing removes both endpoints from
/// `deleted`/`new`, `--scan` persists an EMPTY snapshot, and the next
/// `--cached --exit-code` reports a renamed repository as clean.
#[test]
fn scan_snapshot_keeps_rename_endpoints_as_dirty_rows() {
    let repo = create_repo_with_committed_file("old.txt", "rename roundtrip content\nline two\n");
    let mv = run_libra_command(&["mv", "old.txt", "new.txt"], repo.path());
    assert_cli_success(&mv, "stage the rename");

    // The normal status DOES pair it — that is the behavior under test.
    let doc: serde_json::Value =
        serde_json::from_str(&status_stdout(repo.path(), &["--json", "status"])).expect("json");
    assert!(
        doc["data"]["renames"]
            .as_array()
            .is_some_and(|r| !r.is_empty()),
        "the fixture really is a detected rename: {doc}"
    );

    let scan = run_libra_command(&["status", "--scan"], repo.path());
    assert_cli_success(&scan, "rebuild the cache");

    let cached: serde_json::Value = serde_json::from_str(&status_stdout(
        repo.path(),
        &["--json", "status", "--cached"],
    ))
    .expect("json");
    assert_eq!(
        cached["data"]["is_clean"], false,
        "a renamed repository is not clean through the cache: {cached}"
    );
    assert!(
        cached["data"]["staged"]["deleted"]
            .as_array()
            .is_some_and(|d| d.iter().any(|p| p == "old.txt")),
        "the rename SOURCE survives as a deletion row: {cached}"
    );
    assert!(
        cached["data"]["staged"]["new"]
            .as_array()
            .is_some_and(|n| n.iter().any(|p| p == "new.txt")),
        "and the destination as an addition row: {cached}"
    );

    let exit = run_libra_command(&["status", "--cached", "--exit-code"], repo.path());
    assert_eq!(
        exit.status.code(),
        Some(1),
        "--exit-code over the cache still reports dirty: {}",
        String::from_utf8_lossy(&exit.stderr)
    );
}

/// §B.6.4: porcelain v2 rename records carry repository-root paths AND real
/// metadata when `status` runs from a SUBDIRECTORY. The payload is projected
/// to repo-root paths before rendering, so the writer must use those keys
/// as-is — projecting a second time turned `sub/a.txt` into `sub/sub/a.txt`,
/// missed the HEAD/index lookups, and failed the record closed.
#[test]
fn porcelain_v2_rename_from_subdirectory_keeps_real_metadata() {
    let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::create_dir_all(repo.path().join("sub")).unwrap();
    fs::write(repo.path().join("sub/a.txt"), &base).unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the fixture");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit the fixture");
    let head_oid = {
        let out = run_libra_command(&["rev-parse", "HEAD:sub/a.txt"], repo.path());
        assert_cli_success(&out, "rev-parse");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let mv = run_libra_command(&["mv", "sub/a.txt", "sub/b.txt"], repo.path());
    assert_cli_success(&mv, "stage the rename");

    // Run from INSIDE `sub/`.
    let sub = repo.path().join("sub");
    let out = status_stdout(&sub, &["status", "--porcelain=v2"]);
    let record = out
        .lines()
        .find(|l| l.starts_with("2 "))
        .unwrap_or_else(|| panic!("a v2 rename record is emitted: {out}"));
    let fields: Vec<&str> = record.split_whitespace().collect();
    assert_eq!(fields[1], "R.", "staged-only rename: {out}");
    assert_eq!(fields[3], "100644", "mH is real, not fail-closed: {out}");
    assert_eq!(fields[4], "100644", "mI is real: {out}");
    assert_eq!(fields[5], "100644", "mW is the real worktree mode: {out}");
    assert_eq!(fields[6], head_oid, "hH is the real HEAD blob: {out}");
    assert_ne!(fields[6], "0".repeat(fields[6].len()), "not a zero hash");
    // Paths are repository-root-relative in porcelain, regardless of cwd.
    assert!(
        record.ends_with("sub/b.txt\tsub/a.txt"),
        "repo-root paths, projected exactly once: {out}"
    );
}

/// §B.4.1 exact worktree arm, end to end: an unstaged (index↔worktree)
/// rename whose bytes are unchanged pairs EXACTLY — the worktree OID is
/// streamed this call and the index side is a recorded fact, so the
/// allow-list permits skipping content scoring entirely. Score 100,
/// `exact: true`, on the unstaged side.
#[test]
fn worktree_exact_matches_known_index_oid() {
    let repo = create_repo_with_committed_file("src.txt", "exact worktree content\nline two\n");
    enable_rename_untracked(repo.path());
    // Pure worktree move: the index still records `src.txt` with its OID,
    // and `dst.txt` holds byte-identical content.
    fs::rename(repo.path().join("src.txt"), repo.path().join("dst.txt")).unwrap();

    let json = status_stdout(repo.path(), &["--json", "status"]);
    let doc: serde_json::Value = serde_json::from_str(&json).expect("json status");
    let renames = doc["data"]["renames"].as_array().expect("renames");
    assert_eq!(renames.len(), 1, "one unstaged rename: {json}");
    assert_eq!(renames[0]["from"], "src.txt", "{json}");
    assert_eq!(renames[0]["to"], "dst.txt", "{json}");
    assert_eq!(
        renames[0]["exact"], true,
        "index OID is a recorded fact, so the pair is exact: {json}"
    );
    assert_eq!(renames[0]["score"], 100, "{json}");
    assert_eq!(renames[0]["unstaged"], true, "{json}");
    assert_eq!(renames[0]["staged"], false, "{json}");
}

/// §B.4.1 gitlink arm, end to end over a REAL index: a moved submodule
/// gitlink pairs by (oid, mode) without any content read — the commit object
/// it names need not exist in this repository at all, which is the whole
/// point of the kind check. A regular blob with the same id must not pair
/// with it.
#[test]
fn gitlink_missing_object_exact_by_oid_mode() {
    use git_internal::internal::index::{Index, IndexEntry};

    // A REAL staged gitlink move: HEAD records `old-sub` as a gitlink, the
    // index records `new-sub` with the same commit id, and that commit
    // object does not exist in this repository. The pair must be found by
    // (oid, mode) alone — no content read is possible, which is exactly why
    // the kind check exists.
    let repo = create_repo_with_committed_file("anchor.txt", "anchor\n");
    let phantom = git_internal::internal::object::blob::Blob::from_content("phantom submodule").id;
    let index_path = repo.path().join(".libra/index");

    // Commit `old-sub` as a gitlink so it lands in the HEAD tree.
    {
        let mut index = Index::load(&index_path).expect("load index");
        let mut entry = IndexEntry::new_from_blob("old-sub".to_string(), phantom, 0);
        entry.mode = 0o160000;
        index.add(entry);
        index.save(&index_path).expect("save index");
    }
    // §B.4.1 makes this scenario a hard requirement, so it is asserted, not
    // skipped: a build that cannot commit a gitlink whose object is absent
    // cannot satisfy the card at all.
    let commit = run_libra_command(&["commit", "-m", "add submodule"], repo.path());
    assert_cli_success(
        &commit,
        "committing a gitlink whose commit object is absent is required by B.4.1",
    );

    // Move it in the INDEX: drop `old-sub`, add `new-sub` with the same id.
    {
        let mut index = Index::load(&index_path).expect("reload index");
        index.remove("old-sub", 0);
        let mut entry = IndexEntry::new_from_blob("new-sub".to_string(), phantom, 0);
        entry.mode = 0o160000;
        index.add(entry);
        index.save(&index_path).expect("save index");
    }

    let out = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(
        &out,
        "status answers even though the gitlink's commit object is absent",
    );
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let renames = doc["data"]["renames"].as_array().expect("renames");
    assert_eq!(
        renames.len(),
        1,
        "the moved gitlink is the only rename: {doc}"
    );
    assert_eq!(renames[0]["from"], "old-sub", "{doc}");
    assert_eq!(renames[0]["to"], "new-sub", "{doc}");
    assert_eq!(
        renames[0]["exact"], true,
        "paired by (oid, mode) with NO object read: {doc}"
    );
    assert_eq!(renames[0]["score"], 100, "{doc}");
    // And the absent object never surfaced as a metadata degradation,
    // because it was never requested.
    assert!(
        doc["data"]["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().all(|x| x["source"] != "metadata")),
        "an exact gitlink pair reads no objects: {doc}"
    );
}

/// §B.4 shared-scorer parity: with DUPLICATE content on both sides, which
/// old path pairs with which new path is a choice, not a given. `status` and
/// `diff` must make the SAME choice — same-basename first, then path-byte
/// order — or the same repository reports two different rename sets
/// depending on which command you ask.
#[test]
fn diff_and_status_agree_on_duplicate_content_pairings() {
    let body = "duplicate content\nline two\nline three\n";
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    // Two sources with IDENTICAL content; one destination reuses a source's
    // basename, so the same-basename preference decides the pairing.
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("src/alpha.txt"), body).unwrap();
    fs::write(repo.path().join("src/beta.txt"), body).unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the fixture");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit the fixture");

    fs::create_dir_all(repo.path().join("dst")).unwrap();
    fs::remove_file(repo.path().join("src/alpha.txt")).unwrap();
    fs::remove_file(repo.path().join("src/beta.txt")).unwrap();
    // `dst/beta.txt` shares a basename with `src/beta.txt`; `dst/zzz.txt`
    // shares one with nothing.
    fs::write(repo.path().join("dst/beta.txt"), body).unwrap();
    fs::write(repo.path().join("dst/zzz.txt"), body).unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the moves");

    // status's view.
    let doc: serde_json::Value =
        serde_json::from_str(&status_stdout(repo.path(), &["--json", "status"])).expect("json");
    let mut status_pairs: Vec<(String, String)> = doc["data"]["renames"]
        .as_array()
        .expect("renames")
        .iter()
        .map(|r| {
            (
                r["from"].as_str().unwrap_or_default().to_string(),
                r["to"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    status_pairs.sort();
    assert_eq!(status_pairs.len(), 2, "both moves pair: {doc}");
    // Selection is GLOBAL, not per-source-in-order: every allowed edge is
    // ranked `same_basename DESC, old ASC, new ASC` and consumed greedily.
    // So `src/beta.txt` claims `dst/beta.txt` (the only same-name edge) and
    // `src/alpha.txt` takes what is left — a per-source walk would have let
    // `alpha` grab `dst/beta.txt` first and strand the name match.
    assert_eq!(
        status_pairs,
        vec![
            ("src/alpha.txt".to_string(), "dst/zzz.txt".to_string()),
            ("src/beta.txt".to_string(), "dst/beta.txt".to_string()),
        ],
        "same-basename edges win globally: {status_pairs:?}"
    );

    // diff's view of the same staged state.
    let diff = run_libra_command(&["diff", "--staged", "--name-status"], repo.path());
    assert_cli_success(&diff, "diff --staged --name-status");
    let text = String::from_utf8_lossy(&diff.stdout).into_owned();
    let mut diff_pairs: Vec<(String, String)> = text
        .lines()
        .filter(|l| l.starts_with('R'))
        .filter_map(|l| {
            let mut fields = l.split('\t');
            let _ = fields.next()?;
            Some((fields.next()?.to_string(), fields.next()?.to_string()))
        })
        .collect();
    diff_pairs.sort();
    assert_eq!(
        diff_pairs, status_pairs,
        "diff and status must report the SAME pairings for duplicate content:\n\
         diff: {diff_pairs:?}\nstatus: {status_pairs:?}\ndiff output:\n{text}"
    );
}

/// §B.4.1 in a SHA-256 repository, unstaged arm: the worktree OID is hashed
/// on a pooled I/O worker, and the repository hash kind is thread-local. A
/// worker that started at the SHA-1 default would produce an id that could
/// never equal the SHA-256 index entry, silently degrading an exact pair to
/// an inexact one (and reading objects it did not need).
#[test]
fn rename_sha256_worktree_exact_pairing() {
    let repo = tempdir().expect("temp repo");
    // Asserted, not skipped: SHA-256 support is a hard requirement of the
    // card, so a build that cannot create such a repository fails here
    // rather than reporting a green skip.
    let init = run_libra_command(&["init", "--object-format", "sha256"], repo.path());
    assert_cli_success(&init, "sha256 repositories are required by R0-2");
    configure_identity_via_cli(repo.path());
    let body = "sha256 worktree exact\nline two\nline three\n";
    fs::write(repo.path().join("src.txt"), body).unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the fixture");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit the fixture");
    enable_rename_untracked(repo.path());

    // Pure worktree move, byte-identical content.
    fs::rename(repo.path().join("src.txt"), repo.path().join("dst.txt")).unwrap();

    let json = status_stdout(repo.path(), &["--json", "status"]);
    let doc: serde_json::Value = serde_json::from_str(&json).expect("json status");
    let renames = doc["data"]["renames"].as_array().expect("renames");
    assert_eq!(renames.len(), 1, "the move pairs: {json}");
    assert_eq!(renames[0]["from"], "src.txt", "{json}");
    assert_eq!(renames[0]["to"], "dst.txt", "{json}");
    assert_eq!(
        renames[0]["exact"], true,
        "the worktree OID must be hashed with the REPOSITORY's hash kind: {json}"
    );
    assert_eq!(renames[0]["score"], 100, "{json}");
}

/// §B.4 shared-scorer parity beyond the exact stage. The duplicate-content
/// test above is consumed entirely by exact OID buckets, so it cannot see a
/// divergence in the stages that actually differ between implementations:
/// unique-basename scoring, inexact eligibility, and budget accounting.
#[test]
#[cfg(unix)]
fn diff_and_status_agree_on_inexact_stages() {
    let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::create_dir_all(repo.path().join("src")).unwrap();
    // (a) A unique-basename pair whose content CHANGED — only the basename
    // stage can pair it, and only if the score still clears the threshold.
    fs::write(repo.path().join("src/keep.txt"), &base).unwrap();
    // (b) A symlink pair with "similar" targets: neither command may score
    // symlinks inexactly, so these must NOT be reported as a rename.
    std::os::unix::fs::symlink("target/aaaa", repo.path().join("src/link-a")).unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the fixture");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit the fixture");

    fs::create_dir_all(repo.path().join("dst")).unwrap();
    fs::remove_file(repo.path().join("src/keep.txt")).unwrap();
    fs::write(
        repo.path().join("dst/keep.txt"),
        base.replace("line 7\n", "line seven edited\n"),
    )
    .unwrap();
    fs::remove_file(repo.path().join("src/link-a")).unwrap();
    std::os::unix::fs::symlink("target/aaab", repo.path().join("dst/link-b")).unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the moves");

    let doc: serde_json::Value =
        serde_json::from_str(&status_stdout(repo.path(), &["--json", "status"])).expect("json");
    let mut status_pairs: Vec<(String, String)> = doc["data"]["renames"]
        .as_array()
        .expect("renames")
        .iter()
        .map(|r| {
            (
                r["from"].as_str().unwrap_or_default().to_string(),
                r["to"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    status_pairs.sort();

    let diff = run_libra_command(&["diff", "--staged", "--name-status", "-M40%"], repo.path());
    assert_cli_success(&diff, "diff --staged --name-status -M40%");
    let text = String::from_utf8_lossy(&diff.stdout).into_owned();
    let mut diff_pairs: Vec<(String, String)> = text
        .lines()
        .filter(|l| l.starts_with('R'))
        .filter_map(|l| {
            let mut fields = l.split('\t');
            let _ = fields.next()?;
            Some((fields.next()?.to_string(), fields.next()?.to_string()))
        })
        .collect();
    diff_pairs.sort();

    assert_eq!(
        diff_pairs, status_pairs,
        "diff and status must agree across the basename/inexact stages too:\n\
         diff: {diff_pairs:?}\nstatus: {status_pairs:?}\ndiff output:\n{text}"
    );
    // The basename pair IS found (it is the point of stage 3)…
    assert!(
        status_pairs.contains(&("src/keep.txt".into(), "dst/keep.txt".into())),
        "the scored unique-basename pair is reported: {status_pairs:?}"
    );
    // …and the symlinks are NOT paired by content similarity.
    assert!(
        status_pairs
            .iter()
            .all(|(from, to)| !from.contains("link-") && !to.contains("link-")),
        "symlinks never enter inexact scoring: {status_pairs:?}"
    );
}

/// §B.6.4: an unstaged rename whose SOURCE also carries a staged change
/// renders `MR`, not `.R`. Edit `a`, `add a`, then move it to `b` with
/// `status.renameUntracked` on. Reporting `.R` loses the staged
/// modification entirely (the endpoint row is suppressed) and copies the
/// index hash into `hH`, claiming HEAD and index agree when the user just
/// changed the index.
#[test]
fn porcelain_v2_staged_modify_then_worktree_rename_emits_mr() {
    let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let repo = create_repo_with_committed_file("a.txt", &base);
    enable_rename_untracked(repo.path());
    let head_oid = {
        let out = run_libra_command(&["rev-parse", "HEAD:a.txt"], repo.path());
        assert_cli_success(&out, "rev-parse HEAD:a.txt");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };

    // Staged modification of `a.txt` …
    fs::write(
        repo.path().join("a.txt"),
        base.replace("line 3\n", "line three edited\n"),
    )
    .unwrap();
    let add = run_libra_command(&["add", "a.txt"], repo.path());
    assert_cli_success(&add, "stage the edit");
    let index_oid = {
        let out = run_libra_command(&["ls-files", "--stage"], repo.path());
        assert_cli_success(&out, "ls-files --stage");
        let stage = String::from_utf8(out.stdout).unwrap();
        stage
            .lines()
            .find(|l| l.ends_with("a.txt"))
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or_else(|| panic!("staged entry for a.txt: {stage}"))
            .to_string()
    };
    assert_ne!(head_oid, index_oid, "the staged edit really changed the id");
    // … then a worktree-only move.
    fs::rename(repo.path().join("a.txt"), repo.path().join("b.txt")).unwrap();

    let out = status_stdout(repo.path(), &["status", "--porcelain=v2"]);
    let record = out
        .lines()
        .find(|l| l.starts_with("2 "))
        .unwrap_or_else(|| panic!("a v2 rename record is emitted: {out}"));
    let fields: Vec<&str> = record.split_whitespace().collect();
    assert_eq!(
        fields[1], "MR",
        "the staged modification rides in the X column: {out}"
    );
    assert_eq!(
        fields[6], head_oid,
        "hH is the real HEAD blob, not a copy of the index: {out}"
    );
    assert_eq!(fields[7], index_oid, "hI is the staged blob: {out}");
    assert_ne!(fields[6], fields[7], "HEAD and index differ: {out}");
}

/// §B.4.1 deserialization boundary: an OID of the WRONG hash algorithm
/// parses cleanly (the length is valid for its own kind) and used to reach
/// object loading, where it panicked. A ref carrying such an id must fail
/// closed at the READ, with a message naming the mismatch.
#[test]
fn ref_with_mismatched_hash_kind_fails_closed() {
    // Exercise the READ boundary, not the write one. `update-ref` rejects a
    // 64-character id in a SHA-1 repository up front, so writing one never
    // reaches deserialization. Instead: create the ref legitimately in a
    // SHA-1 repository, then declare the repository SHA-256. The PERSISTED
    // id is now the wrong algorithm, which is exactly the corrupt-ref shape
    // the read path must refuse.
    let repo = create_repo_with_committed_file("a.txt", "content\n");
    let head_oid = {
        let out = run_libra_command(&["rev-parse", "HEAD"], repo.path());
        assert_cli_success(&out, "rev-parse HEAD");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    assert_eq!(head_oid.len(), 40, "the fixture is a SHA-1 repository");

    let flip = run_libra_command(&["config", "core.objectformat", "sha256"], repo.path());
    assert_cli_success(&flip, "declare the repository sha256");

    // Every read path must now refuse the stored SHA-1 id.
    for flags in [
        vec!["status"],
        vec!["--json", "status"],
        vec!["status", "--porcelain"],
    ] {
        let out = run_libra_command(&flags, repo.path());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("panicked"),
            "{flags:?}: a wrong-algorithm ref must fail closed, never panic: {stderr}"
        );
        assert!(
            !out.status.success(),
            "{flags:?}: it must not silently resolve either: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            stderr.contains("Sha1") || stderr.contains("Sha256") || stderr.contains("hash"),
            "{flags:?}: the diagnostic names the hash-kind mismatch: {stderr}"
        );
    }
}

/// §B.4.1 deserialization boundary, index side: a corrupt index — including
/// one whose stage-0 OIDs cannot be decoded — must produce an actionable
/// error rather than a panic deep in object loading. Named by the plan as
/// `malformed_index_oid_rejected_at_decode`.
#[test]
fn malformed_index_oid_rejected_at_decode() {
    // Corrupt the ENTRY OID, not the trailing checksum: the v2 index layout
    // is a 12-byte header followed by entries, each starting with 40 bytes of
    // stat data and then the 20-byte SHA-1. Flipping the tail would only
    // break the file checksum, which is a different (and weaker) check.
    let repo = create_repo_with_committed_file("a.txt", "content\n");
    let index_path = repo.path().join(".libra/index");
    let mut bytes = fs::read(&index_path).expect("read the index");
    const HEADER: usize = 12;
    const STAT_DATA: usize = 40;
    let oid_at = HEADER + STAT_DATA;
    assert!(
        bytes.len() > oid_at + 20,
        "the fixture index really has an entry: {} bytes",
        bytes.len()
    );
    let original: Vec<u8> = bytes[oid_at..oid_at + 20].to_vec();
    for byte in &mut bytes[oid_at..oid_at + 20] {
        *byte ^= 0xFF;
    }
    assert_ne!(
        &bytes[oid_at..oid_at + 20],
        original.as_slice(),
        "the entry OID really changed"
    );
    fs::write(&index_path, &bytes).expect("write the corrupt index");

    for flags in [vec!["status"], vec!["--json", "status"]] {
        let out = run_libra_command(&flags, repo.path());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("panicked"),
            "{flags:?}: a malformed index OID is an error, not a panic: {stderr}"
        );
        assert!(
            !out.status.success(),
            "{flags:?}: a corrupt index must not silently produce a status: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            stderr.contains("LBR-") || stderr.to_lowercase().contains("fatal"),
            "{flags:?}: the failure is actionable: {stderr}"
        );
    }
}

/// §B.6.4: a rename record whose worktree mode cannot be READ (as opposed
/// to being genuinely absent) fails closed rather than emitting a fabricated
/// `100644`. The race window between collection and rendering is too narrow
/// to hit reliably, so a debug-only seam names the path.
#[test]
fn porcelain_v2_rename_unreadable_mode_fails_closed() {
    let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let repo = create_repo_with_committed_file("orig.txt", &base);
    let mv = run_libra_command(&["mv", "orig.txt", "moved.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");
    let edited = base.replace("line 5\n", "line five changed\n");
    fs::write(repo.path().join("moved.txt"), &edited).unwrap();
    let add = run_libra_command(&["add", "moved.txt"], repo.path());
    assert_cli_success(&add, "restage the rename");

    // Sanity: without the seam the record renders normally.
    let ok = status_stdout(repo.path(), &["status", "--porcelain=v2"]);
    assert!(
        ok.lines().any(|l| l.starts_with("2 ")),
        "the fixture really produces a v2 rename record: {ok}"
    );

    let out = run_libra_command_with_stdin_and_env(
        &["status", "--porcelain=v2"],
        repo.path(),
        "",
        &[("LIBRA_TEST_UNREADABLE_MODE_PATH", "moved.txt")],
    );
    assert!(
        !out.status.success(),
        "an unreadable worktree mode must fail closed: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot read the worktree mode"),
        "the error names the actual problem: {stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("100644"),
        "and no partial record with a fabricated mode is printed: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Debug test seams must be IGNORED outside the test harness: with the
/// override variables set but `LIBRA_TEST` removed from the environment,
/// the production defaults stay in effect. Three edited moves need real
/// inexact comparisons, so an honored `LIBRA_TEST_STATUS_COMPARISON_BUDGET=1`
/// would exhaust after the first edge and degrade the run; an honored
/// `LIBRA_TEST_STATUS_PROBE_ENUM_BUDGET=1` would truncate the probe and
/// warn. The I/O-timeout and unreadable-mode overrides have their own
/// negative coverage (`status_probe::tests` and
/// `seam_unreadable_mode_override_is_ignored_without_the_harness_gate`).
#[test]
fn seam_env_overrides_are_ignored_without_the_harness_gate() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    // 40-line bodies score well above the 50% threshold, and the marker
    // line makes the same-index edge the top score in the exhaustive stage.
    for i in 0..3 {
        let body: String = (0..40)
            .map(|l| {
                if l == 20 {
                    format!("marker {i}\n")
                } else {
                    format!("line {l}\n")
                }
                .to_string()
            })
            .collect();
        fs::write(repo.path().join(format!("src{i}.txt")), body).unwrap();
    }
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage base files");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit base files");
    // Three moved AND edited destinations: every pair needs inexact
    // scoring, so the comparison budget is spent per edge.
    for i in 0..3 {
        let body: String = (0..40)
            .map(|l| {
                if l == 20 {
                    format!("marker {i}\n")
                } else if l == 5 {
                    "line five EDITED\n".to_string()
                } else {
                    format!("line {l}\n")
                }
                .to_string()
            })
            .collect();
        fs::rename(
            repo.path().join(format!("src{i}.txt")),
            repo.path().join(format!("dest{i}.txt")),
        )
        .unwrap();
        fs::write(repo.path().join(format!("dest{i}.txt")), body).unwrap();
    }
    enable_rename_untracked(repo.path());

    let out = base_libra_command(&["--json", "status"], repo.path())
        .env_remove("LIBRA_TEST")
        .env("LIBRA_TEST_STATUS_COMPARISON_BUDGET", "1")
        .env("LIBRA_TEST_STATUS_PROBE_ENUM_BUDGET", "1")
        .output()
        .expect("run status without the harness gate");
    assert!(out.status.success(), "status runs: {out:?}");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    for i in 0..3 {
        assert!(
            doc["data"]["renames"]
                .as_array()
                .is_some_and(|r| r.iter().any(|x| x["to"] == format!("dest{i}.txt"))),
            "dest{i}.txt must pair: the ungated overrides must not change the outcome: {doc}"
        );
    }
    assert_eq!(
        doc["data"]["rename_detection_complete"], true,
        "and the run is not degraded by an ignored budget override: {doc}"
    );
    assert_eq!(
        doc["data"]["warnings"],
        serde_json::json!([]),
        "no probe/budget warning from ignored overrides: {doc}"
    );
}

/// The unreadable-mode seam must be ignored without the harness gate: the
/// porcelain v2 record renders the REAL worktree mode instead of failing
/// closed (the positive arm is
/// `porcelain_v2_rename_unreadable_mode_fails_closed`).
#[test]
fn seam_unreadable_mode_override_is_ignored_without_the_harness_gate() {
    let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let repo = create_repo_with_committed_file("orig.txt", &base);
    let mv = run_libra_command(&["mv", "orig.txt", "moved.txt"], repo.path());
    assert_cli_success(&mv, "libra mv");
    let edited = base.replace("line 5\n", "line five changed\n");
    fs::write(repo.path().join("moved.txt"), &edited).unwrap();
    let add = run_libra_command(&["add", "moved.txt"], repo.path());
    assert_cli_success(&add, "restage the rename");

    let out = base_libra_command(&["status", "--porcelain=v2"], repo.path())
        .env_remove("LIBRA_TEST")
        .env("LIBRA_TEST_UNREADABLE_MODE_PATH", "moved.txt")
        .output()
        .expect("run status without the harness gate");
    assert!(
        out.status.success(),
        "with the override ignored the record renders normally: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.lines().any(|l| l.starts_with("2 ")),
        "the v2 rename record is emitted: {stdout}"
    );
    assert!(
        stdout.contains("100644"),
        "the real worktree mode is rendered, not the fail-closed branch: {stdout}"
    );
}

/// §B.4.1: a candidate whose TYPE changes between the snapshot stat and the
/// OID read is dropped, not paired. Otherwise a regular file replaced by a
/// symlink hands the exact gate a symlink-target OID labelled `Regular`.
/// The window is too narrow to hit from a test, so a debug-only seam names
/// the path that "changed".
#[test]
fn worktree_type_race_between_stat_and_hash_drops_candidate() {
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());

    // Without the seam the move pairs normally.
    let doc: serde_json::Value =
        serde_json::from_str(&status_stdout(repo.path(), &["--json", "status"])).expect("json");
    assert!(
        doc["data"]["renames"]
            .as_array()
            .is_some_and(|r| r.iter().any(|x| x["to"] == "dest.txt")),
        "the fixture really pairs without the race: {doc}"
    );

    let out = run_libra_command_with_stdin_and_env(
        &["--json", "status"],
        repo.path(),
        "",
        &[("LIBRA_TEST_TYPE_RACE_PATH", "dest.txt")],
    );
    assert_cli_success(&out, "a type race degrades, it does not fail the command");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert_eq!(
        doc["data"]["renames"],
        serde_json::json!([]),
        "a candidate that changed type is never paired: {doc}"
    );
    // Base status intact: source deletion AND untracked destination.
    assert!(
        doc["data"]["unstaged"]["deleted"]
            .as_array()
            .is_some_and(|d| d.iter().any(|p| p == "moved-src.txt")),
        "the base deletion survives: {doc}"
    );
    assert!(
        doc["data"]["untracked"]
            .as_array()
            .is_some_and(|u| u.iter().any(|p| p == "dest.txt" || p == "dest.txt/")),
        "and the untracked destination survives: {doc}"
    );
    assert_eq!(
        doc["data"]["rename_detection_complete"], false,
        "the dropped candidate is reported as a degradation: {doc}"
    );
    assert!(
        doc["data"]["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|x| x["source"] == "worktree")),
        "with a structured warning: {doc}"
    );
}

/// §B.4.1: a rename candidate that becomes UNUSABLE between the display scan
/// and the snapshot stat costs a candidate, so the run is reported degraded.
/// It used to be dropped silently, leaving `rename_detection_complete: true`
/// for a pairing that was never attempted. This drives the type-race arm;
/// the genuine-`NotFound` arm is
/// `worktree_candidate_not_found_between_scan_and_hash` below — they reach
/// the degradation through different branches, so both are pinned.
#[test]
#[cfg(unix)]
fn worktree_candidate_vanishes_between_scan_and_hash() {
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    // A second untracked file keeps the directory non-empty so the display
    // scan still has something to report after the candidate is removed.
    fs::write(repo.path().join("other.txt"), "unrelated\n").unwrap();

    // Force the type-race branch: the path is treated as having changed type
    // between the stat and the hash, so its OID describes something the
    // recorded kind does not.
    let out = run_libra_command_with_stdin_and_env(
        &["--json", "status"],
        repo.path(),
        "",
        &[("LIBRA_TEST_TYPE_RACE_PATH", "dest.txt")],
    );
    assert_cli_success(&out, "a vanished candidate degrades, it does not fail");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert_eq!(
        doc["data"]["renames"],
        serde_json::json!([]),
        "no rename is claimed for a candidate that could not be read: {doc}"
    );
    assert_eq!(
        doc["data"]["rename_detection_complete"], false,
        "and the run says so rather than claiming the pairing was ruled out: {doc}"
    );
    assert!(
        doc["data"]["unstaged"]["deleted"]
            .as_array()
            .is_some_and(|d| d.iter().any(|p| p == "moved-src.txt")),
        "the base status is unaffected: {doc}"
    );
}

/// §B.4.1 companion to the type-race case above, driving the OTHER branch:
/// the candidate is reported `NotFound` by the time the snapshot stats it.
/// The type-race seam never reaches this arm — it takes the
/// `observed_kind != kind` exit further down — so without this test
/// `NotFound` could be special-cased into a silent skip and nothing would
/// notice. A path that existed when the scan enumerated it and is missing a
/// moment later cost a rename candidate; reporting the run as complete
/// would claim a pairing was ruled out when it was never attempted.
#[test]
#[cfg(unix)]
fn worktree_candidate_not_found_between_scan_and_hash() {
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    fs::write(repo.path().join("other.txt"), "unrelated\n").unwrap();

    // The seam overrides the candidate's stat with a genuine `NotFound`
    // error kind — the same branch an OS-level deletion drives, without
    // the hook mutating the worktree.
    let out = run_libra_command_with_stdin_and_env(
        &["--json", "status"],
        repo.path(),
        "",
        &[("LIBRA_TEST_VANISH_PATH", "dest.txt")],
    );
    assert_cli_success(&out, "a missing candidate degrades, it does not fail");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        repo.path().join("dest.txt").exists(),
        "the seam only simulates the failure — it must never mutate the worktree"
    );
    assert_eq!(
        doc["data"]["renames"],
        serde_json::json!([]),
        "no rename is claimed for a candidate that no longer exists: {doc}"
    );
    assert_eq!(
        doc["data"]["rename_detection_complete"], false,
        "and the run says the pairing was never attempted: {doc}"
    );
    assert!(
        doc["data"]["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|entry| entry["source"] == "worktree")),
        "a worktree-family warning fires instead of silence: {doc}"
    );
    assert!(
        doc["data"]["untracked"]
            .as_array()
            .is_some_and(|u| u.iter().any(|p| p == "dest.txt")),
        "the destination keeps its untracked row through the NotFound degradation: {doc}"
    );
    assert!(
        doc["data"]["unstaged"]["deleted"]
            .as_array()
            .is_some_and(|d| d.iter().any(|p| p == "moved-src.txt")),
        "the base status stays truthful: the source is still reported deleted: {doc}"
    );
}

/// §B.7: the 500k comparison cap is per INVOCATION, not per detection side.
/// Staged and unstaged detection run separately, so building a fresh budget
/// for each let one `status` spend the cap twice while both sides
/// individually looked compliant. The remaining allowance now travels
/// between them, and the second side degrades once the first has spent it.
#[test]
fn comparison_budget_is_shared_across_both_detection_sides() {
    // Both sides must need INEXACT scoring, or the budget is never touched:
    // exact pairs skip content comparison entirely, and a renameLimit trip
    // gates the exhaustive stage before any comparison happens. So: one
    // staged edited move and one unstaged edited move, with a tiny injected
    // budget that only one of them can afford.
    let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::write(repo.path().join("staged-src.txt"), &base).unwrap();
    fs::write(repo.path().join("worktree-src.txt"), &base).unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the fixture");
    let commit = run_libra_command(&["commit", "-m", "base"], repo.path());
    assert_cli_success(&commit, "commit the fixture");
    enable_rename_untracked(repo.path());

    // Staged inexact move (HEAD ↔ index).
    fs::remove_file(repo.path().join("staged-src.txt")).unwrap();
    fs::write(
        repo.path().join("staged-dst.txt"),
        base.replace("line 5\n", "line five edited\n"),
    )
    .unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the inexact move");
    // Unstaged inexact move (index ↔ worktree), left out of the index.
    fs::remove_file(repo.path().join("worktree-src.txt")).unwrap();
    fs::write(
        repo.path().join("worktree-dst.txt"),
        base.replace("line 9\n", "line nine edited\n"),
    )
    .unwrap();

    // Baseline: with room for both, both pair.
    let doc: serde_json::Value =
        serde_json::from_str(&status_stdout(repo.path(), &["--json", "status"])).expect("json");
    let names: Vec<&str> = doc["data"]["renames"]
        .as_array()
        .expect("renames")
        .iter()
        .filter_map(|r| r["to"].as_str())
        .collect();
    assert!(
        names.contains(&"staged-dst.txt") && names.contains(&"worktree-dst.txt"),
        "both sides pair when the budget is ample: {doc}"
    );

    // Budget of 1: whichever side runs first spends it, and the OTHER side
    // must see an exhausted allowance rather than its own fresh one.
    let out = run_libra_command_with_stdin_and_env(
        &["--json", "status"],
        repo.path(),
        "",
        &[("LIBRA_TEST_STATUS_COMPARISON_BUDGET", "1")],
    );
    assert_cli_success(&out, "a spent budget degrades, it does not fail");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let names: Vec<&str> = doc["data"]["renames"]
        .as_array()
        .expect("renames")
        .iter()
        .filter_map(|r| r["to"].as_str())
        .collect();
    assert!(
        names.len() < 2,
        "with a shared budget of 1 the two inexact pairs cannot BOTH be \
         scored — per-side budgets would have found them both: {doc}"
    );
    assert_eq!(
        doc["data"]["rename_detection_complete"], false,
        "and the run reports the degradation: {doc}"
    );
    // One warning per {code, source} for the whole invocation.
    let warnings = doc["data"]["warnings"].as_array().expect("warnings");
    let mut seen: Vec<(String, String)> = warnings
        .iter()
        .map(|w| {
            (
                w["code"].as_str().unwrap_or_default().to_string(),
                w["source"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let before = seen.len();
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen.len(),
        before,
        "no duplicated warning across sides: {doc}"
    );
}

/// §B.4.1: a HEAD whose OID passes ref validation but whose commit or tree
/// object is missing must fail closed. Expanding the HEAD tree went through
/// the panic-only `Commit::load`/`Tree::load`, so a repository with a
/// pruned object took the process down instead of reporting a problem the
/// user can act on.
#[test]
fn missing_head_object_fails_closed_not_panics() {
    let repo = create_repo_with_committed_file("a.txt", "content\n");
    // Give detection something to do, so the HEAD tree really is expanded.
    fs::remove_file(repo.path().join("a.txt")).unwrap();
    fs::write(repo.path().join("b.txt"), "content\n").unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the move");

    // Delete the HEAD commit object.
    let head = {
        let out = run_libra_command(&["rev-parse", "HEAD"], repo.path());
        assert_cli_success(&out, "rev-parse HEAD");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let object = repo
        .path()
        .join(".libra/objects")
        .join(&head[0..2])
        .join(&head[2..]);
    if !object.exists() {
        eprintln!("skipped (HEAD commit is packed, not loose)");
        return;
    }
    fs::remove_file(&object).expect("remove the HEAD commit object");

    for flags in [vec!["status"], vec!["--json", "status"]] {
        let out = run_libra_command(&flags, repo.path());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("panicked"),
            "{flags:?}: a missing HEAD object is an error, not a panic: {stderr}"
        );
        assert!(
            !out.status.success(),
            "{flags:?}: and it does not silently report a status: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            stderr.contains("LBR-") || stderr.to_lowercase().contains("fatal"),
            "{flags:?}: with an actionable message: {stderr}"
        );
    }
}

/// §B.6.0.1 pathspec narrowing removes only the warnings DERIVED from
/// filtered-out `io_blocked[]` entries. The worktree family also carries
/// aggregate rename-scoring warnings that name no path; dropping those by
/// source would hide a real degradation, flip `rename_detection_complete`
/// back to `true`, and silently downgrade `--exit-code-on-warning` from 9.
#[test]
#[cfg(unix)]
fn pathspec_filter_keeps_non_event_worktree_warnings() {
    use std::os::unix::fs::PermissionsExt;

    let repo = repo_with_worktree_move("inside/dest.txt");
    enable_rename_untracked(repo.path());
    // (a) A rename candidate INSIDE the pathspec whose content cannot be
    // hashed → an aggregate `worktree_read_failed` naming no path.
    let dest = repo.path().join("inside/dest.txt");
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o000)).unwrap();
    // (b) A blocked directory OUTSIDE the pathspec → an `io_blocked` event
    // that narrowing must drop, along with its own "cannot inspect" warning.
    let outside = repo.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("inner.txt"), "x\n").unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o000)).unwrap();

    let out = run_libra_command(
        &[
            "--json",
            "--exit-code-on-warning",
            "status",
            "--",
            "inside",
            "moved-src.txt",
        ],
        repo.path(),
    );
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o644)).unwrap();

    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["io_blocked"]
            .as_array()
            .expect("io_blocked")
            .iter()
            .all(|e| e["path"]["display"] != "outside"),
        "the out-of-scope event is filtered away: {doc}"
    );
    let warnings = doc["data"]["warnings"].as_array().expect("warnings");
    assert!(
        warnings.iter().all(|w| !w["message"]
            .as_str()
            .is_some_and(|m| m.contains("cannot inspect 'outside'"))),
        "its derived warning goes with it: {doc}"
    );
    assert!(
        warnings.iter().any(|w| w["source"] == "worktree"
            && !w["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("cannot inspect '"))),
        "the in-scope aggregate rename-read warning SURVIVES narrowing: {doc}"
    );
    assert_eq!(
        doc["data"]["rename_detection_complete"], false,
        "a surviving degradation keeps the completeness flag false: {doc}"
    );
    assert_eq!(
        out.status.code(),
        Some(9),
        "and --exit-code-on-warning still returns 9: {doc}"
    );
}

/// §B.6.0.1: the cache-mode FALLBACK paths take the same fail-closed route
/// as the normal scan. A stale/absent cache degrades to a full status, so it
/// discovers blocked paths exactly like any other run — printing a partial
/// body there (or exiting 0 under `--quiet`) would let "I could not inspect
/// this repository" reach a caller as a clean answer.
#[test]
#[cfg(unix)]
fn cache_fallback_still_fails_closed_on_io_blocked() {
    use std::os::unix::fs::PermissionsExt;

    let repo = create_repo_with_committed_file("locked.txt", "content\n");
    fs::write(repo.path().join("locked.txt"), "content changed\n").unwrap();
    let locked = repo.path().join("locked.txt");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    // No `--scan` has run, so both cache modes take the stale fallback.
    for flags in [
        vec!["status", "--cached"],
        vec!["status", "--check-dirty"],
        vec!["--quiet", "status", "--cached"],
        vec![
            "--quiet",
            "--exit-code-on-warning",
            "status",
            "--check-dirty",
        ],
    ] {
        let out = run_libra_command(&flags, repo.path());
        assert_eq!(
            out.status.code(),
            Some(128),
            "{flags:?} must fail closed on the fallback path: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty(),
            "{flags:?} must not print a partial body: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    // `--json` still reports the partial result rather than failing.
    let json = run_libra_command(&["--json", "status", "--cached"], repo.path());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
    assert_cli_success(&json, "json reports the fallback partial result");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&json.stdout)).expect("json");
    assert!(
        !doc["data"]["io_blocked"]
            .as_array()
            .expect("io_blocked")
            .is_empty(),
        "the blocked path rides in the fallback envelope: {doc}"
    );
    assert_eq!(doc["data"]["is_clean"], false, "{doc}");
}

/// An unreadable top-level UNTRACKED directory fails text modes closed
/// (§B.6.0.1) even though its `?? dir/` marker is emitted — a marker is not
/// an inspection result. `--quiet` suppresses only the body, never the
/// verdict; JSON keeps the partial contract (marker + io_blocked +
/// `base_scan_complete: false`).
#[cfg(unix)]
#[test]
fn quiet_unreadable_untracked_dir_fails_closed() {
    let repo = create_repo_with_committed_file("plain.txt", "content\n");
    let blocked = repo.path().join("blocked");
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("inner.txt"), "y\n").unwrap();
    lock_dir(&blocked);

    let text = run_libra_command(&["status"], repo.path());
    let quiet = run_libra_command(&["--quiet", "status"], repo.path());
    let json = run_libra_command(&["--json", "status"], repo.path());
    unlock_dir(&blocked);

    for (name, out) in [("text", &text), ("quiet", &quiet)] {
        assert!(
            !out.status.success(),
            "{name} must fail closed for an unreadable directory: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("cannot inspect"),
            "{name} names the block with the fatal contract: {stderr}"
        );
    }

    assert_cli_success(&json, "json keeps the partial contract");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&json.stdout)).expect("json");
    assert!(
        doc["data"]["untracked"]
            .as_array()
            .is_some_and(|u| u.iter().any(|p| p == "blocked/")),
        "the marker is still emitted: {doc}"
    );
    assert!(
        doc["data"]["io_blocked"]
            .as_array()
            .is_some_and(|b| !b.is_empty()),
        "{doc}"
    );
    assert_eq!(
        doc["data"]["base_scan_complete"], false,
        "the base scan is not claimed complete: {doc}"
    );
}

/// §B.6.0.1 delivery matrix, `--quiet` arm: quiet suppresses the BODY, never
/// the verdict. A blocked path must still fail closed under `--quiet` — a
/// silent exit 0 on a repository status could not fully inspect is the exact
/// "looks clean" answer the contract exists to prevent. The cache modes take
/// the same path.
#[test]
#[cfg(unix)]
fn quiet_does_not_bypass_io_blocked_fail_closed() {
    use std::os::unix::fs::PermissionsExt;

    let repo = create_repo_with_committed_file("locked.txt", "content\n");
    fs::write(repo.path().join("locked.txt"), "content changed\n").unwrap();
    let locked = repo.path().join("locked.txt");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    for flags in [
        vec!["--quiet", "status"],
        vec!["--quiet", "status", "--exit-code"],
        vec!["--quiet", "--exit-code-on-warning", "status", "--exit-code"],
        vec!["--quiet", "status", "--porcelain"],
    ] {
        let out = run_libra_command(&flags, repo.path());
        assert_eq!(
            out.status.code(),
            Some(128),
            "{flags:?} must fail closed (fatal ≻ 9 ≻ 1), not report a clean/dirty verdict: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("cannot inspect"),
            "{flags:?} names the blocked path: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
}

/// §B.6.0.1: a `--check-dirty` row that cannot be re-verified emits its
/// structured `worktree_*` warning like any other blocked path — the cache
/// modes are not a warning-free shortcut. Without it, `--exit-code-on-warning`
/// silently degrades from 9 to 1 and `data.warnings[]` contradicts
/// `data.io_blocked[]`.
#[test]
#[cfg(unix)]
fn check_dirty_ioblocked_emits_warning_and_exit_nine() {
    use std::os::unix::fs::PermissionsExt;

    let repo = create_repo_with_committed_file("cached.txt", "content\n");
    fs::write(repo.path().join("cached.txt"), "content changed\n").unwrap();
    // Populate the cache while the path is still readable.
    let scan = run_libra_command(&["status", "--scan"], repo.path());
    assert_cli_success(&scan, "seed the dirty cache");

    let locked = repo.path().join("cached.txt");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let out = run_libra_command(
        &[
            "--json",
            "--exit-code-on-warning",
            "status",
            "--check-dirty",
            "--exit-code",
        ],
        repo.path(),
    );
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();

    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let blocked = doc["data"]["io_blocked"].as_array().expect("io_blocked");
    assert!(
        !blocked.is_empty(),
        "the unverifiable row is reported: {doc}"
    );
    let warnings = doc["data"]["warnings"].as_array().expect("warnings");
    assert_eq!(
        warnings.len(),
        blocked.len(),
        "one warning per io_blocked entry: {doc}"
    );
    assert!(
        warnings.iter().all(|w| w["source"] == "worktree"
            && w["message"]
                .as_str()
                .is_some_and(|m| m.contains("cannot inspect"))),
        "the warnings are the documented worktree family: {doc}"
    );
    assert_eq!(
        out.status.code(),
        Some(9),
        "the warning exit beats the dirty exit: {doc}"
    );
}

/// §B.4.1 Unavailable: an object the store cannot open (EACCES on the loose
/// file) is the third fault class alongside missing and corrupt, and takes
/// the same "skip only the dependent inexact candidate" path.
#[test]
#[cfg(unix)]
fn object_read_unavailable_blob_skips_inexact_only() {
    use std::os::unix::fs::PermissionsExt;

    assert_object_fault_skips_inexact_only(|object_file| {
        fs::set_permissions(object_file, fs::Permissions::from_mode(0o000)).unwrap();
    });
}

/// §B.3.4: a blob larger than the per-object cap is a BUDGET fault, not an
/// availability fault — it must degrade with `metadata_budget_exceeded`
/// (never `metadata_unavailable`), keep the base D + A rows, and refuse to
/// guess a rename it could not score.
#[test]
fn object_read_budget_exceeded_skips_inexact_only() {
    // Just over OBJECT_READ_MAX_OBJECT_BYTES (2 MiB): the old blob cannot be
    // read within the per-object cap, so the pair is unscorable.
    let line = "budget cap line with enough bytes to add up quickly\n";
    let big: String = line.repeat((2 * 1024 * 1024 / line.len()) + 64);
    let repo = create_repo_with_committed_file("huge-old.txt", &big);

    fs::remove_file(repo.path().join("huge-old.txt")).unwrap();
    fs::write(
        repo.path().join("huge-new.txt"),
        big.replacen(line, "budget cap line CHANGED\n", 1),
    )
    .unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the oversized move");

    let out = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&out, "json status over the object cap");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let staged = &doc["data"]["staged"];
    assert!(
        staged["deleted"]
            .as_array()
            .is_some_and(|d| d.iter().any(|p| p == "huge-old.txt"))
            && staged["new"]
                .as_array()
                .is_some_and(|n| n.iter().any(|p| p == "huge-new.txt")),
        "base D + A survive the budget cap: {doc}"
    );
    assert_eq!(
        doc["data"]["renames"],
        serde_json::json!([]),
        "an unscorable pair is skipped, never guessed: {doc}"
    );
    let warnings = doc["data"]["warnings"].as_array().expect("warnings");
    assert!(
        warnings
            .iter()
            .any(|w| w["code"] == "metadata_budget_exceeded"),
        "the cap degrades with the BUDGET code: {doc}"
    );
    assert!(
        !warnings.iter().any(|w| w["code"] == "metadata_unavailable"),
        "a size cap is not an availability fault: {doc}"
    );
    assert_eq!(
        warnings
            .iter()
            .filter(|w| w["code"] == "metadata_budget_exceeded")
            .count(),
        1,
        "the budget warning is deduplicated across both detection sides: {doc}"
    );
    assert_eq!(doc["data"]["base_scan_complete"], true, "{doc}");
    assert_eq!(doc["data"]["rename_detection_complete"], false, "{doc}");
}

/// §B.5: the dirty-cache stale-fallback warning obeys the same 9 ≻ 1 exit
/// arbitration as every other path — `--check-dirty` with no cache degrades
/// with `dirty_cache_stale_fallback`, and `--exit-code-on-warning` beats the
/// dirty exit in both text and JSON modes.
#[test]
fn exit_warning_over_dirty_cache_fallback() {
    let repo = create_repo_with_committed_file("plain.txt", "content\n");
    fs::write(repo.path().join("plain.txt"), "content changed\n").unwrap();

    // Text: warning on stderr, exit 9 over dirty 1.
    let text = run_libra_command(
        &[
            "--exit-code-on-warning",
            "status",
            "--check-dirty",
            "--exit-code",
        ],
        repo.path(),
    );
    assert_eq!(
        text.status.code(),
        Some(9),
        "cache-fallback warning exit 9 over dirty 1: {}",
        String::from_utf8_lossy(&text.stderr)
    );
    let stderr = String::from_utf8_lossy(&text.stderr);
    assert!(
        stderr.contains("warning:") && stderr.contains("dirty cache"),
        "stale-fallback warning delivered on stderr: {stderr}"
    );

    // JSON: warning rides in data.warnings[], stderr stays clean, same 9.
    let json = run_libra_command(
        &[
            "--json",
            "--exit-code-on-warning",
            "status",
            "--check-dirty",
            "--exit-code",
        ],
        repo.path(),
    );
    assert_eq!(json.status.code(), Some(9), "json cache-fallback exit 9");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&json.stdout)).expect("json envelope");
    assert!(
        doc["data"]["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|x| x["code"] == "dirty_cache_stale_fallback")),
        "{doc}"
    );
    assert!(
        json.stderr.is_empty(),
        "json mode keeps stderr clean: {:?}",
        String::from_utf8_lossy(&json.stderr)
    );

    // Without --exit-code-on-warning the dirty exit 1 still wins.
    let plain = run_libra_command(&["status", "--check-dirty", "--exit-code"], repo.path());
    assert_eq!(plain.status.code(), Some(1), "dirty exit stays 1");
}

/// §B.6.1 / DEFER-02: a non-UTF-8 name keeps its base `??` row (status must
/// NOT fail) but sits out rename scoring, surfacing one
/// `rename_path_encoding_unsupported` warning and
/// `rename_detection_complete: false`.
#[test]
#[cfg(unix)]
fn non_utf8_path_skips_rename_not_status() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let body = "probe rename content\nline two\n";
    let repo = create_repo_with_committed_file("moved-src.txt", body);
    if !fs_supports_non_utf8_names(repo.path()) {
        return;
    }
    enable_rename_untracked(repo.path());
    fs::remove_file(repo.path().join("moved-src.txt")).unwrap();
    let twin = repo.path().join(OsStr::from_bytes(b"nu\xff-dest.txt"));
    fs::write(&twin, body).unwrap();

    let out = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&out, "json status with non-UTF-8 twin");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["unstaged"]["deleted"]
            .as_array()
            .is_some_and(|d| d.iter().any(|p| p == "moved-src.txt")),
        "base D preserved: {doc}"
    );
    assert_eq!(
        doc["data"]["renames"],
        serde_json::json!([]),
        "non-UTF-8 candidate never pairs in R0: {doc}"
    );
    assert!(
        doc["data"]["warnings"].as_array().is_some_and(|w| w
            .iter()
            .any(|x| x["code"] == "rename_path_encoding_unsupported")),
        "{doc}"
    );
    assert_eq!(doc["data"]["rename_detection_complete"], false, "{doc}");

    // Short format renders the raw bytes octal-escaped (never fails, never
    // replacement-mangled): `?? "nu\377-dest.txt"`.
    let short = run_libra_command(&["status", "--short"], repo.path());
    assert_cli_success(&short, "short status with non-UTF-8 name");
    let text = String::from_utf8_lossy(&short.stdout).into_owned();
    assert!(
        text.contains(r#"?? "nu\377-dest.txt""#),
        "octal-escaped raw bytes in short display: {text}"
    );
}

/// §B.6.7: Unix porcelain v1/v2 `-z` keep the RAW path bytes on the wire —
/// no quoting, no lossy replacement.
#[test]
#[cfg(unix)]
fn porcelain_z_non_utf8_base_status_raw_bytes() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    if !fs_supports_non_utf8_names(repo.path()) {
        return;
    }
    fs::write(
        repo.path().join(OsStr::from_bytes(b"raw\xff.txt")),
        "payload\n",
    )
    .unwrap();

    let v1 = run_libra_command(&["status", "--porcelain", "-z"], repo.path());
    assert_cli_success(&v1, "porcelain v1 -z");
    assert!(
        v1.stdout
            .windows(b"?? raw\xff.txt\0".len())
            .any(|w| w == b"?? raw\xff.txt\0"),
        "v1 -z carries the raw bytes: {:?}",
        String::from_utf8_lossy(&v1.stdout)
    );

    let v2 = run_libra_command(&["status", "--porcelain=v2", "-z"], repo.path());
    assert_cli_success(&v2, "porcelain v2 -z");
    assert!(
        v2.stdout
            .windows(b"? raw\xff.txt\0".len())
            .any(|w| w == b"? raw\xff.txt\0"),
        "v2 -z carries the raw bytes: {:?}",
        String::from_utf8_lossy(&v2.stdout)
    );
}

/// §B.6.0.1: an io_blocked event on a non-UTF-8 path is reversible — the
/// display field carries the octal-escaped readable form and `raw_base64`
/// carries the exact OS bytes. (The Windows encoding is pinned separately by
/// `raw_base64_encodes_windows_utf16_units`, because a Windows fixture
/// cannot make such a directory unreadable without ACL manipulation.)
#[test]
#[cfg(unix)]
fn json_io_blocked_non_utf8_path_reversible() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let repo = create_repo_with_committed_file("plain.txt", "content\n");
    if !fs_supports_non_utf8_names(repo.path()) {
        return;
    }
    let blocked = repo.path().join(OsStr::from_bytes(b"blk\xff"));
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("inner.txt"), "hidden\n").unwrap();
    lock_dir(&blocked);

    // `-uall` descends into untracked directories, so the unreadable
    // non-UTF-8 dir produces a genuine read_dir event.
    let out = run_libra_command(&["--json", "status", "-uall"], repo.path());
    unlock_dir(&blocked);
    assert_cli_success(&out, "json status -uall with non-UTF-8 blocked dir");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let entries = doc["data"]["io_blocked"].as_array().expect("io_blocked");
    let entry = entries
        .iter()
        .find(|e| e["path"]["raw_base64"] != serde_json::Value::Null)
        .unwrap_or_else(|| panic!("one non-UTF-8 io_blocked entry: {doc}"));
    assert_eq!(
        entry["path"]["display"], r#""blk\377""#,
        "octal-escaped display: {doc}"
    );
    // base64("blk\xff") — decodes back to the exact OS bytes.
    assert_eq!(entry["path"]["raw_base64"], "Ymxr/w==", "{doc}");
    assert_eq!(entry["reason"], "permission_denied", "{doc}");
}

// ── R0-2 review follow-ups: exact-evidence allow-list and optional
//    worktree-read degradation (§B.4.1, §B.3.4) ────────────────────────────

/// §B.4.1 fail-closed exact allow-list: two SAME-CALL worktree hashes
/// (`ComputedWorktreeThisCall` on both sides) must NOT be asserted as an
/// exact rename — neither side is anchored in the object store, so the
/// pair goes through the ordinary inexact path instead. The unstaged
/// side is exactly that shape (index→worktree with an untracked
/// destination is C/C only when the OLD side is also a worktree read),
/// so this pins the engine-level predicate.
#[test]
fn blob_evidence_computed_pair_rejects_exact() {
    use libra::command::rename_detect::{
        BlobEvidence, BlobKind, BlobRef, ContentOutcome, RenameContentSource, RenameDetectConfig,
        RenameSnapshot, match_pairs,
    };

    struct NoContent;
    impl RenameContentSource for NoContent {
        fn old_content(&mut self, _path: &Path, _blob: &BlobRef) -> ContentOutcome {
            ContentOutcome::Skipped(libra::command::rename_detect::SkipReason::ObjectUnavailable)
        }
        fn new_content(&mut self, _path: &Path, _blob: &BlobRef) -> ContentOutcome {
            ContentOutcome::Skipped(libra::command::rename_detect::SkipReason::ObjectUnavailable)
        }
    }

    use std::path::PathBuf;

    let oid = git_internal::internal::object::blob::Blob::from_content("same bytes\n").id;
    let computed = |oid| BlobRef {
        kind: BlobKind::Regular,
        mode: 0o100644,
        size: Some(11),
        evidence: BlobEvidence::ComputedWorktreeThisCall { oid },
    };
    let known = |oid| BlobRef {
        kind: BlobKind::Regular,
        mode: 0o100644,
        size: Some(11),
        evidence: BlobEvidence::KnownObjectId { oid },
    };
    let config = RenameDetectConfig {
        threshold: 30_000,
        rename_limit: 1000,
        comparison_budget: None,
    };

    // C/C: refused (content reads then fail, so no pair at all).
    let mut snapshot = RenameSnapshot::default();
    snapshot
        .old_map
        .insert(PathBuf::from("old.txt"), computed(oid));
    snapshot
        .new_map
        .insert(PathBuf::from("new.txt"), computed(oid));
    let outcome = match_pairs(&snapshot, &config, &mut NoContent);
    assert!(
        outcome.matches.is_empty(),
        "computed↔computed must not pair exactly: {:?}",
        outcome.matches
    );

    // K/C and C/K: allowed (one side is a recorded object-store fact).
    for (old, new) in [(known(oid), computed(oid)), (computed(oid), known(oid))] {
        let mut snapshot = RenameSnapshot::default();
        snapshot.old_map.insert(PathBuf::from("old.txt"), old);
        snapshot.new_map.insert(PathBuf::from("new.txt"), new);
        let outcome = match_pairs(&snapshot, &config, &mut NoContent);
        assert_eq!(outcome.matches.len(), 1, "K/C and C/K stay exact");
        assert!(outcome.matches[0].exact, "the pair is exact");
    }
}

/// §B.4.1 fail-closed allow-list, `Unknown` arm: a side with NO blob
/// evidence can never take the exact path, regardless of what the other
/// side carries — an unproven identity must not masquerade as a byte-exact
/// match, because exact pairing skips content reading entirely.
#[test]
fn blob_evidence_unknown_rejects_exact() {
    use std::path::PathBuf;

    use libra::command::rename_detect::{
        BlobEvidence, BlobKind, BlobRef, ContentOutcome, RenameContentSource, RenameDetectConfig,
        RenameSnapshot, SkipReason, match_pairs,
    };

    struct NoContent;
    impl RenameContentSource for NoContent {
        fn old_content(&mut self, _path: &Path, _blob: &BlobRef) -> ContentOutcome {
            ContentOutcome::Skipped(SkipReason::ObjectUnavailable)
        }
        fn new_content(&mut self, _path: &Path, _blob: &BlobRef) -> ContentOutcome {
            ContentOutcome::Skipped(SkipReason::ObjectUnavailable)
        }
    }

    let oid = git_internal::internal::object::blob::Blob::from_content("same bytes\n").id;
    let blob = |evidence| BlobRef {
        kind: BlobKind::Regular,
        mode: 0o100644,
        size: Some(11),
        evidence,
    };
    let config = RenameDetectConfig {
        threshold: 30_000,
        rename_limit: 1000,
        comparison_budget: None,
    };

    for (label, old, new) in [
        (
            "unknown/known",
            blob(BlobEvidence::Unknown),
            blob(BlobEvidence::KnownObjectId { oid }),
        ),
        (
            "known/unknown",
            blob(BlobEvidence::KnownObjectId { oid }),
            blob(BlobEvidence::Unknown),
        ),
        (
            "unknown/unknown",
            blob(BlobEvidence::Unknown),
            blob(BlobEvidence::Unknown),
        ),
        (
            "unknown/computed",
            blob(BlobEvidence::Unknown),
            blob(BlobEvidence::ComputedWorktreeThisCall { oid }),
        ),
    ] {
        let mut snapshot = RenameSnapshot::default();
        snapshot.old_map.insert(PathBuf::from("old.txt"), old);
        snapshot.new_map.insert(PathBuf::from("new.txt"), new);
        let outcome = match_pairs(&snapshot, &config, &mut NoContent);
        assert!(
            outcome.matches.is_empty(),
            "{label} must not produce an exact pair: {:?}",
            outcome.matches
        );
    }
}

/// §B.4.1 gitlink arm, end to end: a submodule gitlink and a regular blob
/// are different object KINDS. Even when their raw ids coincide they must
/// never pair, and a gitlink whose commit object is absent must not become
/// a rename source — `status` still reports the plain add/delete.
#[test]
fn gitlink_never_pairs_with_a_regular_blob() {
    use std::path::PathBuf;

    use libra::command::rename_detect::{
        BlobEvidence, BlobKind, BlobRef, ContentOutcome, RenameContentSource, RenameDetectConfig,
        RenameSnapshot, SkipReason, match_pairs,
    };

    struct NoContent;
    impl RenameContentSource for NoContent {
        fn old_content(&mut self, _path: &Path, _blob: &BlobRef) -> ContentOutcome {
            ContentOutcome::Skipped(SkipReason::ObjectMissing)
        }
        fn new_content(&mut self, _path: &Path, _blob: &BlobRef) -> ContentOutcome {
            ContentOutcome::Skipped(SkipReason::ObjectMissing)
        }
    }

    let oid = git_internal::internal::object::blob::Blob::from_content("shared id\n").id;
    let config = RenameDetectConfig {
        threshold: 30_000,
        rename_limit: 1000,
        comparison_budget: None,
    };
    let entry = |kind, mode| BlobRef {
        kind,
        mode,
        size: Some(9),
        evidence: BlobEvidence::KnownObjectId { oid },
    };

    // Same id, different kind: refused in both directions.
    for (old, new) in [
        (
            entry(BlobKind::Gitlink, 0o160000),
            entry(BlobKind::Regular, 0o100644),
        ),
        (
            entry(BlobKind::Regular, 0o100644),
            entry(BlobKind::Gitlink, 0o160000),
        ),
    ] {
        let mut snapshot = RenameSnapshot::default();
        snapshot.old_map.insert(PathBuf::from("old"), old);
        snapshot.new_map.insert(PathBuf::from("new"), new);
        let outcome = match_pairs(&snapshot, &config, &mut NoContent);
        assert!(
            outcome.matches.is_empty(),
            "a gitlink and a blob are different kinds: {:?}",
            outcome.matches
        );
    }

    // Two gitlinks with the SAME commit id do pair (a moved submodule).
    let mut snapshot = RenameSnapshot::default();
    snapshot
        .old_map
        .insert(PathBuf::from("old"), entry(BlobKind::Gitlink, 0o160000));
    snapshot
        .new_map
        .insert(PathBuf::from("new"), entry(BlobKind::Gitlink, 0o160000));
    let outcome = match_pairs(&snapshot, &config, &mut NoContent);
    assert_eq!(
        outcome.matches.len(),
        1,
        "a moved submodule keeps its identity: {outcome:?}"
    );
    assert!(outcome.matches[0].exact, "identical commit ids are exact");
}

/// §B.4.1 symlink arm, end to end: a symlink's blob content is its TARGET
/// path bytes, never the file it points at. Two links with the same target
/// are an exact rename; a dangling link is still hashable (the target
/// string exists even when the target does not), so a moved dangling
/// symlink is detected rather than dropped.
#[test]
#[cfg(unix)]
fn symlink_rename_hashes_target_bytes_not_referent() {
    let repo = create_repo_with_committed_file("anchor.txt", "anchor\n");
    // A committed symlink pointing at a path that does not exist.
    std::os::unix::fs::symlink("nowhere/at/all", repo.path().join("link-old")).unwrap();
    let add = run_libra_command(&["add", "link-old"], repo.path());
    assert_cli_success(&add, "stage the dangling symlink");
    let commit = run_libra_command(&["commit", "-m", "add dangling link"], repo.path());
    assert_cli_success(&commit, "commit the dangling symlink");

    // Move it: same target bytes, new path.
    fs::remove_file(repo.path().join("link-old")).unwrap();
    std::os::unix::fs::symlink("nowhere/at/all", repo.path().join("link-new")).unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage the moved symlink");

    let out = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&out, "json status with a moved dangling symlink");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let renames = doc["data"]["renames"].as_array().expect("renames");
    assert_eq!(
        renames.len(),
        1,
        "a dangling symlink still has hashable target bytes: {doc}"
    );
    assert_eq!(renames[0]["from"], "link-old", "{doc}");
    assert_eq!(renames[0]["to"], "link-new", "{doc}");
    assert_eq!(
        renames[0]["exact"], true,
        "identical target bytes are an exact match: {doc}"
    );
    assert_eq!(renames[0]["score"], 100, "{doc}");
}

/// §B.4.1 symlink arm on the UNSTAGED side: the staged case above only
/// exercises HEAD↔index, where both blobs are already objects. This is the
/// index↔worktree pairing, so the destination's identity comes from
/// `worktree_blob_oid_and_size`'s symlink branch — `readlink` on the target
/// STRING, never an open of the referent. A dangling link proves it: if
/// detection dereferenced, there would be nothing to hash and the move would
/// be dropped instead of matched exactly.
#[test]
#[cfg(unix)]
fn symlink_worktree_exact_with_rename_untracked() {
    let repo = create_repo_with_committed_file("anchor.txt", "anchor\n");
    std::os::unix::fs::symlink("nowhere/at/all", repo.path().join("link-old")).unwrap();
    let add = run_libra_command(&["add", "link-old"], repo.path());
    assert_cli_success(&add, "stage the dangling symlink");
    let commit = run_libra_command(&["commit", "-m", "add dangling link"], repo.path());
    assert_cli_success(&commit, "commit the dangling symlink");

    enable_rename_untracked(repo.path());
    // Move it in the WORKING TREE ONLY — nothing is re-staged, so the pairing
    // is index (old) against worktree (new).
    fs::remove_file(repo.path().join("link-old")).unwrap();
    std::os::unix::fs::symlink("nowhere/at/all", repo.path().join("link-new")).unwrap();

    let out = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&out, "json status with an unstaged dangling symlink move");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let renames = doc["data"]["renames"].as_array().expect("renames");
    let entry = renames
        .iter()
        .find(|r| r["from"] == "link-old" && r["to"] == "link-new")
        .unwrap_or_else(|| panic!("the unstaged symlink move is detected: {doc}"));
    assert_eq!(
        entry["unstaged"], true,
        "the pairing is index against worktree, not HEAD against index: {doc}"
    );
    assert_eq!(
        entry["exact"], true,
        "identical target bytes match exactly, with no content scoring: {doc}"
    );
    assert_eq!(entry["score"], 100, "{doc}");

    // And the short format agrees, so this is not a JSON-only artifact.
    let short = status_stdout(repo.path(), &["status", "--short"]);
    assert!(
        short.contains("link-old -> link-new"),
        "the short format reports the same move: {short}"
    );
}

/// §B.3.4: an OPTIONAL worktree hash that fails (here: the destination
/// becomes unreadable between the scan and detection) drops only that
/// rename candidate — the base `D` + `??` status survives AND a
/// structured worktree-family warning fires instead of silence.
#[test]
#[cfg(unix)]
fn worktree_read_failure_degrades_with_warning_not_silence() {
    use std::os::unix::fs::PermissionsExt;

    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    // Make the destination unreadable: the probe still enumerates it (the
    // parent directory is readable), but hashing it fails.
    let dest = repo.path().join("dest.txt");
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o000)).unwrap();

    let out = run_libra_command(&["--json", "status"], repo.path());
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o644)).unwrap();
    assert_cli_success(&out, "json status with unreadable rename destination");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert_eq!(
        doc["data"]["renames"],
        serde_json::json!([]),
        "no rename is claimed when the destination cannot be hashed: {doc}"
    );
    assert!(
        doc["data"]["unstaged"]["deleted"]
            .as_array()
            .is_some_and(|d| d.iter().any(|p| p == "moved-src.txt")),
        "the base deletion survives: {doc}"
    );
    assert!(
        doc["data"]["untracked"]
            .as_array()
            .is_some_and(|u| u.iter().any(|p| p == "dest.txt" || p == "dest.txt/")),
        "and so does the untracked DESTINATION — asserting only the deletion \
         would pass even if a premature marker collapse dropped it: {doc}"
    );
    assert!(
        doc["data"]["warnings"].as_array().is_some_and(|w| w
            .iter()
            .any(|x| x["source"] == "worktree" || x["code"] == "worktree_read_failed")),
        "an unreadable candidate produces a structured warning: {doc}"
    );
    assert_eq!(
        doc["data"]["rename_detection_complete"], false,
        "the run reports incomplete rename detection: {doc}"
    );
}

// ── R0-6 review follow-ups: short `-z` rename wire bytes and the full
//    quotePath escape/cascade matrix (§B.6.1, §B.6.6) ─────────────────────

/// §B.6.1: `--short -z` writes a rename as `XY SP <new> NUL <old> NUL`
/// with RAW, unquoted path bytes — even when the paths carry a TAB or a
/// double quote that the non-`-z` form would C-style-escape.
#[test]
fn short_z_rename_record_is_raw_new_then_old() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let old_name = "old\tname.txt";
    let new_name = "new\"quoted.txt";
    fs::write(repo.path().join(old_name), "rename wire payload\n").unwrap();
    let add = run_libra_command(&["add", old_name], repo.path());
    assert_cli_success(&add, "stage odd-named file");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit odd-named file");
    fs::rename(repo.path().join(old_name), repo.path().join(new_name)).unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage rename");

    let out = run_libra_command(&["status", "--short", "-z"], repo.path());
    assert_cli_success(&out, "short -z rename");
    let expected: Vec<u8> = {
        let mut bytes = b"R  ".to_vec();
        bytes.extend_from_slice(new_name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(old_name.as_bytes());
        bytes.push(0);
        bytes
    };
    assert!(
        out.stdout
            .windows(expected.len())
            .any(|w| w == expected.as_slice()),
        "short -z rename record is `R  <new> NUL <old> NUL` with raw bytes: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !out.stdout.contains(&b'"') || !out.stdout.windows(2).any(|w| w == b"\\t"),
        "-z never C-style-quotes: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !out.stdout.contains(&b'\n'),
        "-z records carry no newline: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );

    // The non-`-z` short form DOES quote the same pair.
    let quoted = run_libra_command(&["status", "--short"], repo.path());
    assert_cli_success(&quoted, "short rename");
    let text = String::from_utf8_lossy(&quoted.stdout).into_owned();
    assert!(text.contains(r#""old\tname.txt""#), "{text}");
    assert!(text.contains(r#""new\"quoted.txt""#), "{text}");
}

/// §B.6.6: TAB, LF, CR, backslash and double quote are ALWAYS escaped
/// (independent of `core.quotePath`), and the key itself resolves
/// through the strict local → global → system cascade with the winning
/// scope's invalid value failing closed.
#[test]
fn quote_path_escape_matrix_and_cascade() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    // One file per always-escaped byte (LF/CR are legal in a filename on
    // Unix but not creatable on every FS — use TAB, `"` and `\`).
    for name in ["tab\there.txt", "quote\"here.txt", "back\\slash.txt"] {
        fs::write(repo.path().join(name), "x\n").unwrap();
    }
    // core.quotePath=false must NOT disable the mandatory escapes.
    let cfg = run_libra_command(&["config", "core.quotePath", "false"], repo.path());
    assert_cli_success(&cfg, "set core.quotePath=false");
    let out = run_libra_command(&["status", "--short"], repo.path());
    assert_cli_success(&out, "short with quotePath=false");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains(r#""tab\there.txt""#), "TAB escaped: {text}");
    assert!(
        text.contains(r#""quote\"here.txt""#),
        "double quote escaped: {text}"
    );
    assert!(
        text.contains(r#""back\\slash.txt""#),
        "backslash escaped: {text}"
    );

    // Cascade: a GLOBAL value applies when local is unset; local wins
    // over global; an invalid value in the WINNING scope fails closed.
    let global_repo = tempdir().expect("second repo");
    init_repo_via_cli(global_repo.path());
    configure_identity_via_cli(global_repo.path());
    fs::write(global_repo.path().join("nonascii-é.txt"), "x\n").unwrap();
    let set_global = run_libra_command(
        &["config", "set", "--global", "core.quotePath", "false"],
        global_repo.path(),
    );
    assert_cli_success(&set_global, "set global core.quotePath=false");
    let out = run_libra_command(&["status", "--short"], global_repo.path());
    assert_cli_success(&out, "global cascade applies");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.contains("nonascii-é.txt"),
        "global false leaves non-ASCII unescaped: {text}"
    );
    let set_local = run_libra_command(&["config", "core.quotePath", "true"], global_repo.path());
    assert_cli_success(&set_local, "local overrides global");
    let out = run_libra_command(&["status", "--short"], global_repo.path());
    assert_cli_success(&out, "local wins");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.contains(r"\303\251") || text.contains(r"\351"),
        "local true octal-escapes non-ASCII: {text}"
    );
    let bad = run_libra_command(
        &["config", "core.quotePath", "sometimes"],
        global_repo.path(),
    );
    assert_cli_success(&bad, "store an invalid value");
    let out = run_libra_command(&["status", "--short"], global_repo.path());
    assert!(!out.status.success(), "invalid winning scope fails closed");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-002"),
        "stable code: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "no status output before the refusal: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// ── R0-7 review follow-ups: copy fail-closed matrix, three-scope strict
//    cascade, nested JSON rename compatibility (§B.5, §B.6.5) ─────────────

/// §B.6.5: BOTH spellings (`copy`/`copies`) on BOTH keys
/// (`status.renames` and the `diff.renames` fallback) fail closed —
/// copy detection is not supported and must never degrade to plain
/// rename detection.
#[test]
fn config_copy_fail_closed() {
    for (key, value) in [
        ("status.renames", "copy"),
        ("status.renames", "copies"),
        ("diff.renames", "copy"),
        ("diff.renames", "copies"),
    ] {
        let repo = create_repo_with_committed_file("a.txt", "content\n");
        let cfg = run_libra_command(&["config", key, value], repo.path());
        assert_cli_success(&cfg, "store copy config");
        let out = run_libra_command(&["status"], repo.path());
        assert!(
            !out.status.success(),
            "{key}={value} must fail closed, got: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            stderr.contains("LBR-CLI-002"),
            "{key}={value} carries the stable usage code: {stderr}"
        );
        assert!(
            out.stdout.is_empty(),
            "{key}={value} produces no status output before the refusal: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// §B.5 strict cascade: `status.renameLimit` (and its `diff.renameLimit`
/// fallback) resolve local → global → SYSTEM, and an invalid value in
/// the winning scope fails closed before any output.
#[test]
fn rename_limit_three_scope_cascade_and_invalid_winner() {
    let temp = tempdir().expect("scope temp");
    let global_db = temp.path().join("glob.db").to_string_lossy().into_owned();
    let system_db = temp.path().join("sys.db").to_string_lossy().into_owned();
    let env: [(&str, &str); 2] = [
        ("LIBRA_CONFIG_GLOBAL_DB", global_db.as_str()),
        ("LIBRA_CONFIG_SYSTEM_DB", system_db.as_str()),
    ];
    let repo = repo_with_two_exhaustive_candidates();

    // SYSTEM-only `diff.renameLimit=1`: the fallback key resolves through
    // the system scope and degrades the exhaustive stage with a warning.
    let set_sys = run_libra_command_with_stdin_and_env(
        &["config", "--system", "diff.renameLimit", "1"],
        repo.path(),
        "",
        &env,
    );
    assert_cli_success(&set_sys, "set system diff.renameLimit");
    let out = run_libra_command_with_stdin_and_env(&["--json", "status"], repo.path(), "", &env);
    assert_cli_success(&out, "system-scope cascade applies");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["warnings"].as_array().is_some_and(|w| w
            .iter()
            .any(|x| x["code"] == "rename_limit_product_skipped")),
        "system scope feeds the cascade: {doc}"
    );

    // GLOBAL `status.renameLimit=0` (no cap) outranks the system fallback.
    let set_global = run_libra_command_with_stdin_and_env(
        &["config", "set", "--global", "status.renameLimit", "0"],
        repo.path(),
        "",
        &env,
    );
    assert_cli_success(&set_global, "set global status.renameLimit=0");
    let out = run_libra_command_with_stdin_and_env(&["--json", "status"], repo.path(), "", &env);
    assert_cli_success(&out, "global outranks system");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        !doc["data"]["warnings"].as_array().is_some_and(|w| w
            .iter()
            .any(|x| x["code"] == "rename_limit_product_skipped")),
        "global 0 lifts the cap: {doc}"
    );

    // An invalid value in the WINNING (local) scope fails closed.
    let set_local = run_libra_command_with_stdin_and_env(
        &["config", "set", "status.renameLimit", "--", "-3"],
        repo.path(),
        "",
        &env,
    );
    assert_cli_success(&set_local, "store an invalid local value");
    let out = run_libra_command_with_stdin_and_env(&["status"], repo.path(), "", &env);
    assert!(!out.status.success(), "invalid winning scope fails closed");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-002"),
        "stable code: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty(), "no output before the refusal");
}

/// §B.6.5 compatibility: the nested `unstaged.renamed[]` field keeps
/// carrying unstaged pairs (the top-level `renames[]` array is additive,
/// not a replacement).
#[test]
fn json_nested_unstaged_renamed_survives() {
    let repo = repo_with_worktree_move("dest.txt");
    enable_rename_untracked(repo.path());
    let out = run_libra_command(&["--json", "status"], repo.path());
    assert_cli_success(&out, "json unstaged rename");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let nested = doc["data"]["unstaged"]["renamed"]
        .as_array()
        .expect("nested unstaged.renamed array");
    assert_eq!(nested.len(), 1, "nested unstaged rename present: {doc}");
    assert_eq!(nested[0]["from"], "moved-src.txt", "{doc}");
    assert_eq!(nested[0]["to"], "dest.txt", "{doc}");
    // And the top-level array agrees.
    assert!(
        doc["data"]["renames"].as_array().is_some_and(|r| r
            .iter()
            .any(|x| x["to"] == "dest.txt" && x["unstaged"] == true)),
        "top-level renames[] agrees: {doc}"
    );
}

// ── R0-8 review follow-ups: unified exit arbitration, JSON scan partial,
//    subdirectory schema keys, io_timeout reason (§B.5, §B.6.0.1) ─────────

/// §B.5: an `io_blocked` path's worktree warning takes part in the SAME
/// arbitration as every other warning — `--exit-code-on-warning` exits 9
/// (not the dirty 1) in JSON as well as text.
#[test]
fn exit_warning_over_dirty_ioblocked_json() {
    let repo = repo_with_locked_tracked_dir();
    let locked = repo.path().join("locked");
    let json = run_libra_command(
        &["--json", "--exit-code-on-warning", "status", "--exit-code"],
        repo.path(),
    );
    let text = run_libra_command(
        &["--exit-code-on-warning", "status", "--exit-code"],
        repo.path(),
    );
    unlock_dir(&locked);
    assert_eq!(
        json.status.code(),
        Some(9),
        "json: warning exit 9 over dirty 1: {:?}",
        String::from_utf8_lossy(&json.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&json.stdout)).expect("json envelope");
    assert!(
        doc["data"]["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|x| x["source"] == "worktree")),
        "the blocked path's warning rides in data.warnings[]: {doc}"
    );
    // Text mode fails closed (LBR-IO-001), never the silent dirty exit.
    assert_eq!(text.status.code(), Some(128), "text still fails closed");
}

/// §B.3.3 + §B.6.0.1: `--json status --scan` with a blocked path keeps
/// the partial contract — it succeeds with `io_blocked[]`, reports that
/// no cache was written, and leaves the previous snapshot intact.
#[test]
fn json_scan_ioblocked_reports_partial_without_writing_cache() {
    let repo = repo_with_locked_tracked_dir();
    // Seed a good cache first (nothing blocked yet).
    unlock_dir(&repo.path().join("locked"));
    let seed = run_libra_command(&["status", "--scan"], repo.path());
    assert_cli_success(&seed, "seed cache");
    lock_dir(&repo.path().join("locked"));

    let out = run_libra_command(&["--json", "status", "--scan"], repo.path());
    unlock_dir(&repo.path().join("locked"));
    // Exactly 0 — the documented default. Accepting 1 as well would let a
    // regression that reports dirty WITHOUT `--exit-code` slip through, and
    // stderr must stay clean because JSON delivers warnings in the envelope.
    assert_eq!(
        out.status.code(),
        Some(0),
        "json scan reports a partial result and exits 0 by default: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).trim().is_empty(),
        "JSON mode keeps stderr clean: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json envelope");
    assert_eq!(doc["data"]["mode"], "scan", "{doc}");
    assert_eq!(doc["data"]["cache_written"], false, "{doc}");
    assert!(
        doc["data"]["io_blocked"]
            .as_array()
            .is_some_and(|b| !b.is_empty()),
        "the partial result names the blocked paths: {doc}"
    );
    assert_eq!(doc["data"]["base_scan_complete"], false, "{doc}");
    // Text mode still fails closed with the actionable hint.
    lock_dir(&repo.path().join("locked"));
    let text = run_libra_command(&["status", "--scan"], repo.path());
    unlock_dir(&repo.path().join("locked"));
    assert!(!text.status.success(), "text scan fails closed");
    assert!(
        String::from_utf8_lossy(&text.stderr).contains("--json"),
        "the refusal points at the partial view"
    );
}

/// §B.6.0.1: the `staged` / `rename` fields of an `io_blocked[]` entry
/// are computed on REPO-RELATIVE keys, so they stay correct when status
/// runs from a subdirectory.
#[test]
fn json_io_blocked_staged_field_survives_subdirectory() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::create_dir_all(repo.path().join("sub/locked")).unwrap();
    fs::write(repo.path().join("sub/locked/inside.txt"), "tracked\n").unwrap();
    fs::write(repo.path().join("sub/other.txt"), "other\n").unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage fixture");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit fixture");
    // Stage a modification to the path that will then become unreadable.
    fs::write(repo.path().join("sub/locked/inside.txt"), "tracked v2\n").unwrap();
    let add = run_libra_command(&["add", "sub/locked/inside.txt"], repo.path());
    assert_cli_success(&add, "stage modification");
    lock_dir(&repo.path().join("sub/locked"));

    let out = run_libra_command(&["--json", "status"], &repo.path().join("sub"));
    unlock_dir(&repo.path().join("sub/locked"));
    assert_cli_success(&out, "json status from a subdirectory");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let entry = doc["data"]["io_blocked"]
        .as_array()
        .expect("io_blocked")
        .iter()
        .find(|e| {
            e["path"]["display"]
                .as_str()
                .is_some_and(|d| d.contains("inside.txt"))
        })
        .unwrap_or_else(|| panic!("the staged blocked path is reported: {doc}"));
    assert_eq!(
        entry["staged"], "M",
        "the staged component resolves from a subdirectory: {doc}"
    );
}

/// §B.3.3: a hung worktree read is reclaimed and reported as
/// `io_timeout` instead of hanging `status` forever. The deadline is
/// injected (debug-only) and tripped with a FIFO, which blocks
/// `read_dir`-adjacent metadata reads indefinitely.
#[test]
#[cfg(unix)]
fn tracked_scan_io_timeout_is_reported_not_hung() {
    // A tracked regular file replaced by a FIFO: opening it to hash the
    // content blocks forever because nothing ever opens the write end. This
    // is the real "hung NFS/FUSE mount" shape — no fault injection beyond
    // shortening the deadline, so the test proves the operation is actually
    // reclaimed rather than merely that a flag exists.
    let repo = create_repo_with_committed_file("hangs.txt", "committed content\n");
    let fifo = repo.path().join("hangs.txt");
    fs::remove_file(&fifo).expect("remove the committed regular file");
    let created = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !created {
        eprintln!("skipped (mkfifo unavailable)");
        return;
    }

    let started = std::time::Instant::now();
    let out = run_libra_command_with_stdin_and_env(
        &["--json", "status"],
        repo.path(),
        "",
        &[("LIBRA_TEST_STATUS_IO_TIMEOUT_MS", "300")],
    );
    let elapsed = started.elapsed();
    assert_cli_success(&out, "status survives a permanently blocking read");
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "the blocked read must be reclaimed, not waited out: took {elapsed:?}"
    );

    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let blocked = doc["data"]["io_blocked"].as_array().expect("io_blocked");
    assert!(
        !blocked.is_empty(),
        "an unreadable path must be REPORTED, never silently dropped: {doc}"
    );
    let entry = blocked
        .iter()
        .find(|e| e["path"]["display"] == "hangs.txt")
        .unwrap_or_else(|| panic!("the FIFO must appear in io_blocked: {doc}"));
    assert_eq!(
        entry["reason"], "io_timeout",
        "a deadline hit is reported as io_timeout, not a generic io_error: {doc}"
    );
    assert!(
        doc["data"]["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|x| x["code"] == "worktree_io_timeout")),
        "a timeout maps to the documented warning code: {doc}"
    );
    // "Cannot inspect" is never clean, and the tracked file must not be
    // rendered as deleted or unchanged.
    assert_eq!(doc["data"]["is_clean"], false, "{doc}");
    assert!(
        !doc["data"]["unstaged"]["deleted"]
            .as_array()
            .expect("deleted")
            .iter()
            .any(|p| p == "hangs.txt"),
        "a blocked read must never be reported as a deletion: {doc}"
    );

    // Text formats fail closed on the same repository (§B.6.0.1).
    let text = run_libra_command_with_stdin_and_env(
        &["status", "--porcelain"],
        repo.path(),
        "",
        &[("LIBRA_TEST_STATUS_IO_TIMEOUT_MS", "300")],
    );
    assert!(
        !text.status.success(),
        "porcelain must fail closed instead of printing a partial view"
    );
}

// ── R0-5 review follow-up: porcelain v2 `-z` rename record raw bytes ─────

/// §B.6.4/§B.6.7: the v2 rename record under `-z` carries RAW path bytes
/// in `<new> NUL <old> NUL` order — even when a path holds a TAB that the
/// non-`-z` form would C-style-escape (and which is also the field
/// separator in the tabbed form).
#[test]
fn porcelain_v2_z_rename_raw_bytes_with_tab_paths() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let old_name = "old\tv2.txt";
    let new_name = "new\tv2.txt";
    fs::write(repo.path().join(old_name), "v2 rename payload\n").unwrap();
    let add = run_libra_command(&["add", old_name], repo.path());
    assert_cli_success(&add, "stage tabbed file");
    let commit = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit tabbed file");
    fs::rename(repo.path().join(old_name), repo.path().join(new_name)).unwrap();
    let add = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&add, "stage rename");

    let out = run_libra_command(&["status", "--porcelain=v2", "-z"], repo.path());
    assert_cli_success(&out, "porcelain v2 -z rename");
    let mut expected = new_name.as_bytes().to_vec();
    expected.push(0);
    expected.extend_from_slice(old_name.as_bytes());
    expected.push(0);
    assert!(
        out.stdout
            .windows(expected.len())
            .any(|w| w == expected.as_slice()),
        "v2 -z rename tail is `<new> NUL <old> NUL` with raw bytes: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        out.stdout.starts_with(b"2 R"),
        "the record is a single `2 R…` row: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !out.stdout.windows(2).any(|w| w == b"\\t"),
        "-z never escapes the TAB: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    // The tabbed (non-`-z`) form quotes instead, so the separator stays
    // unambiguous.
    let tabbed = run_libra_command(&["status", "--porcelain=v2"], repo.path());
    assert_cli_success(&tabbed, "porcelain v2 rename");
    let text = String::from_utf8_lossy(&tabbed.stdout).into_owned();
    assert!(text.contains(r#""new\tv2.txt""#), "{text}");
    assert!(text.contains(r#""old\tv2.txt""#), "{text}");
}

// ── R0-3 review follow-ups: probe relevance filtering and per-root
//    marker三态 (§B.3.2, §B.3.5) ────────────────────────────────────────

/// §B.3.2: an EACCES on a path the pathspec EXCLUDES is not this run's
/// problem — a narrowed `status` must neither fail closed nor leak the
/// unrelated path through `io_blocked[]`.
#[test]
#[cfg(unix)]
fn probe_pathspec_outside_eacces_not_propagated() {
    use std::os::unix::fs::PermissionsExt;

    let repo = repo_with_worktree_move("wanted/dest.txt");
    enable_rename_untracked(repo.path());
    // An unreadable directory OUTSIDE the pathspec.
    let unrelated = repo.path().join("elsewhere");
    fs::create_dir_all(&unrelated).unwrap();
    fs::write(unrelated.join("payload.txt"), "x\n").unwrap();
    fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o000)).unwrap();

    let out = run_libra_command(
        &["--json", "status", "--", "wanted", "moved-src.txt"],
        repo.path(),
    );
    fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o755)).unwrap();
    assert_cli_success(&out, "narrowed status ignores an unrelated unreadable dir");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["io_blocked"]
            .as_array()
            .is_some_and(|b| b.iter().all(|e| {
                !e["path"]["display"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("elsewhere")
            })),
        "an out-of-pathspec block never leaks into the report: {doc}"
    );
}

/// §B.3.5 per-root三态: a truncated/blocked root must not freeze the
/// markers of a DIFFERENT root that was walked to completion.
#[test]
#[cfg(unix)]
fn probe_marker_collapse_is_per_root() {
    use std::os::unix::fs::PermissionsExt;

    let repo = create_repo_with_committed_file("moved-src.txt", "probe rename content\nline two\n");
    enable_rename_untracked(repo.path());
    // Root A: a clean, fully walkable destination directory.
    fs::create_dir_all(repo.path().join("clean")).unwrap();
    fs::rename(
        repo.path().join("moved-src.txt"),
        repo.path().join("clean/dest.txt"),
    )
    .unwrap();
    // Root B: an unreadable directory (blocks its own root only).
    let blocked = repo.path().join("blockedroot");
    fs::create_dir_all(&blocked).unwrap();
    fs::write(blocked.join("inner.txt"), "y\n").unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();

    // BOTH directories are positive pathspecs, so both become probe roots —
    // that is the whole point: one root being blocked must not suppress the
    // other root's marker collapse, and the clean root's success must not
    // hide the blocked one.
    let out = run_libra_command(
        &[
            "--json",
            "status",
            "--",
            "clean",
            "blockedroot",
            "moved-src.txt",
        ],
        repo.path(),
    );
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_cli_success(&out, "narrowed status across a clean and a blocked root");
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(
        doc["data"]["renames"]
            .as_array()
            .is_some_and(|r| r.iter().any(|x| x["to"] == "clean/dest.txt")),
        "the complete root still pairs its rename: {doc}"
    );
    let untracked = doc["data"]["untracked"].as_array().expect("untracked");
    assert!(
        untracked.iter().all(|p| p != "clean/"),
        "the complete root's consumed marker collapses: {doc}"
    );
    // The blocked root is the negative half of the contract: its probe could
    // not enumerate, so its marker is conservatively RETAINED (collapsing it
    // would claim every candidate underneath was accounted for).
    assert!(
        untracked.iter().any(|p| p == "blockedroot/"),
        "a blocked root keeps its directory marker: {doc}"
    );
    let blocked_events = doc["data"]["io_blocked"].as_array().expect("io_blocked");
    assert!(
        blocked_events
            .iter()
            .any(|e| e["path"]["display"] == "blockedroot"),
        "the blocked root is reported, not silently skipped: {doc}"
    );
    assert_eq!(
        doc["data"]["rename_detection_complete"], false,
        "one blocked root degrades the completeness flag: {doc}"
    );
}

/// §B.6.6 escape matrix, platform-independent half: TAB, LF, CR, `"`
/// and `\` are ALWAYS escaped under BOTH `core.quotePath` settings (LF
/// and CR cannot be created as filenames everywhere, so they are pinned
/// through the public quoting helper the renderers use).
#[test]
fn quote_path_always_escapes_control_bytes() {
    use libra::command::status::quote_pathname;

    for quote_path in [true, false] {
        for (raw, expected) in [
            ("tab\there.txt", r#""tab\there.txt""#),
            ("lf\nhere.txt", r#""lf\nhere.txt""#),
            ("cr\rhere.txt", r#""cr\rhere.txt""#),
            ("quote\"here.txt", r#""quote\"here.txt""#),
            ("back\\slash.txt", r#""back\\slash.txt""#),
        ] {
            assert_eq!(
                quote_pathname(Path::new(raw), quote_path),
                expected,
                "core.quotePath={quote_path} must always escape {raw:?}"
            );
        }
        // A plain ASCII path is never quoted under either setting.
        assert_eq!(
            quote_pathname(Path::new("plain.txt"), quote_path),
            "plain.txt"
        );
    }
    // Non-ASCII follows the setting: escaped by default, raw when off.
    assert_eq!(
        quote_pathname(Path::new("café.txt"), true),
        r#""caf\303\251.txt""#
    );
    assert_eq!(quote_pathname(Path::new("café.txt"), false), "café.txt");
}

/// §B.4.3: the four VALID short clusters all work, and every one of them
/// selects the format its letters name.
///
/// The existing cluster tests all exercise argv that clap REJECTS (`-bus`,
/// `-buz` fail on `-u`'s value), so they prove the arity table is consulted
/// but never that a successful cluster reaches the right renderer. These do:
/// each of `-bz`, `-zb`, `-sz`, `-zs` must produce NUL-separated short
/// output, in either letter order.
#[test]
fn valid_short_clusters_select_short_and_nul() {
    let repo = create_repo_with_committed_file("a.txt", "content\n");
    fs::write(repo.path().join("b.txt"), "new\n").unwrap();

    for cluster in ["-bz", "-zb", "-sz", "-zs"] {
        let out = run_libra_command(&["status", cluster], repo.path());
        assert!(
            out.status.success(),
            "`status {cluster}` must be accepted: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains('\0'),
            "`{cluster}` includes -z, so records are NUL-separated: {stdout:?}"
        );
        assert!(
            !stdout.contains('\n'),
            "`{cluster}` must not also emit newlines: {stdout:?}"
        );
        assert!(
            stdout.contains("?? b.txt"),
            "`{cluster}` renders the short format: {stdout:?}"
        );
    }
}

/// §B.4.3: after `--`, a token that LOOKS like `--find-renames=…` is a
/// pathspec and must be neither collected as an occurrence nor rewritten.
#[test]
fn find_renames_after_double_dash_is_a_pathspec() {
    let repo = create_repo_with_committed_file("a.txt", "content\n");
    // A file whose name is exactly the flag spelling.
    let literal = "--find-renames=505";
    fs::write(repo.path().join(literal), "literal\n").unwrap();

    // Untracked, so `status -- <literal>` reports it and nothing else.
    let out = run_libra_command(&["status", "--porcelain", "--", literal], repo.path());
    assert!(
        out.status.success(),
        "a pathspec after `--` must not be parsed as a flag: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(literal),
        "the literal path is reported: {stdout:?}"
    );

    // And an INVALID raw value after `--` is still just a path — it must not
    // reach the threshold parser and fail the command.
    fs::write(repo.path().join("--find-renames=bogus"), "literal\n").unwrap();
    let out = run_libra_command(
        &["status", "--porcelain", "--", "--find-renames=bogus"],
        repo.path(),
    );
    assert!(
        out.status.success(),
        "an invalid raw value after `--` is a path, not a flag: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// §B.4.3: argv is `OsString`, so a non-UTF-8 argument does not abort the
/// process — and an invalid raw threshold that a LATER flag overrides is
/// never interpreted at all.
///
/// `env::args()` panics on invalid Unicode, so before this the command died
/// with a Rust panic message for an ordinary pathspec naming a file whose
/// name is not UTF-8.
#[cfg(unix)]
#[test]
fn non_utf8_arguments_do_not_abort_the_process() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let repo = create_repo_with_committed_file("a.txt", "content\n");
    let bad = OsString::from_vec(vec![0xff, 0xfe]);

    // A non-UTF-8 `--find-renames` value that a later occurrence overrides:
    // last-one-wins means it is never interpreted, so this must SUCCEED.
    let mut find = OsString::from("--find-renames=");
    find.push(&bad);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .arg("status")
        .arg(&find)
        .arg("--find-renames=80")
        .arg("--porcelain")
        .current_dir(repo.path())
        .output()
        .expect("run libra");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "a non-UTF-8 argument must not panic: {stderr}"
    );
    assert!(
        out.status.success(),
        "an overridden invalid value is never interpreted: {stderr}"
    );

    // The same value as the WINNER is a clean usage error, not a panic.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .arg("status")
        .arg("--find-renames=80")
        .arg(&find)
        .current_dir(repo.path())
        .output()
        .expect("run libra");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && !stderr.contains("panicked"),
        "the winning invalid value fails cleanly: {stderr}"
    );
    assert!(stderr.contains("not valid UTF-8"), "and says why: {stderr}");

    // A non-UTF-8 PATHSPEC is refused by the parser — `StatusArgs.pathspec`
    // is `String` and stays that way, because the card requires the public
    // type to keep source compatibility. What matters is that the refusal is
    // a clean usage error and NOT a panic, which is what it used to be.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .arg("status")
        .arg("--porcelain")
        .arg("--")
        .arg(&bad)
        .current_dir(repo.path())
        .output()
        .expect("run libra");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "a non-UTF-8 pathspec must not panic: {stderr}"
    );
    assert!(
        stderr.contains("LBR-CLI-002"),
        "it is a usage error, with a stable code: {stderr}"
    );
}

/// A root global AFTER the subcommand is accepted, and its attached value is
/// not read as a short cluster (`libra status -J=ndjson`).
///
/// The argv scan interprets clusters from an arity table; when that table
/// omitted the root globals, `ndjson` scanned as a cluster and its `s` looked
/// like `--short`, which the scan-vs-parser agreement check then refused.
#[test]
fn global_after_subcommand_is_not_scanned_as_a_cluster() {
    let repo = create_repo_with_committed_file("a.txt", "content\n");
    let out = run_libra_command(&["status", "-J=ndjson"], repo.path());
    assert!(
        out.status.success(),
        "`status -J=ndjson` must be accepted: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value =
        serde_json::from_str(stdout.lines().next().expect("ndjson line")).expect("ndjson envelope");
    assert_eq!(doc["command"], "status");
    assert!(
        !stdout.contains('\0'),
        "no NUL records — nothing selected the short format: {stdout:?}"
    );
}
