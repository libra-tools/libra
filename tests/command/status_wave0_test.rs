use std::{fs, path::Path};

use git_internal::internal::{
    index::{Index, IndexEntry},
    object::blob::Blob,
};
use libra::{command::save_object, utils::path};
use serde_json::Value;

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

fn stage_all(repo: &Path) {
    let add = run_libra_command(&["add", "-A"], repo);
    assert_cli_success(&add, "stage all changes");
}

fn status_stdout(repo: &Path, args: &[&str]) -> String {
    let output = run_libra_command(args, repo);
    assert_cli_success(&output, "status command");
    String::from_utf8(output.stdout).expect("status stdout should be utf-8")
}

fn status_json(repo: &Path) -> Value {
    let output = run_libra_command(&["--json", "status"], repo);
    assert_cli_success(&output, "json status command");
    parse_json_stdout(&output)
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
fn porcelain_v2_rename_line_emits_r100() {
    let repo = create_repo_with_committed_file("a.txt", "same\ncontent\n");
    fs::rename(repo.path().join("a.txt"), repo.path().join("b.txt"))
        .expect("failed to rename fixture");
    stage_all(repo.path());

    let output = status_stdout(repo.path(), &["status", "--porcelain", "v2"]);

    assert!(
        output.lines().any(|line| {
            line.starts_with("2 R. ") && line.contains(" R100 ") && line.ends_with("b.txt\ta.txt")
        }),
        "expected porcelain v2 R100 rename row, got:\n{output}"
    );
}

#[test]
#[serial]
fn porcelain_v2_rename_partial_score() {
    let repo = create_repo_with_committed_file("a.txt", "one\ntwo\nthree\nfour\n");
    fs::remove_file(repo.path().join("a.txt")).expect("failed to remove source fixture");
    fs::write(repo.path().join("b.txt"), "one\ntwo\nchanged\nfour\n")
        .expect("failed to write renamed fixture");
    stage_all(repo.path());

    let output = status_stdout(repo.path(), &["status", "--porcelain", "v2"]);
    let rename_line = output
        .lines()
        .find(|line| line.starts_with("2 R. "))
        .expect("expected porcelain v2 rename row");
    // Porcelain v2 fixed positions: xy at [1] is "R." and must not be mistaken
    // for the similarity field at [8] ("R75").
    let fields: Vec<&str> = rename_line.split_whitespace().collect();
    assert!(
        fields.len() >= 9,
        "expected ≥9 whitespace fields on v2 rename row, got {}: {rename_line}",
        fields.len()
    );
    let score_field = fields[8];
    let score: u32 = score_field
        .strip_prefix('R')
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .expect("fields[8] should be R<digits>")
        .parse()
        .expect("score should be numeric");

    assert!(
        (50..100).contains(&score),
        "expected partial rename score in 50..99, got {score}: {rename_line}"
    );
    assert!(
        rename_line.ends_with("b.txt\ta.txt"),
        "rename paths should be TAB-separated new then old: {rename_line}"
    );
}

#[test]
#[serial]
fn short_rename_arrow_format() {
    let repo = create_repo_with_committed_file("a.txt", "same\ncontent\n");
    fs::rename(repo.path().join("a.txt"), repo.path().join("b.txt"))
        .expect("failed to rename fixture");
    stage_all(repo.path());

    let output = status_stdout(repo.path(), &["status", "--short"]);

    assert!(
        output.lines().any(|line| line == "R  a.txt -> b.txt"),
        "expected short rename arrow row, got:\n{output}"
    );
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
fn json_includes_renames_array() {
    let clean_repo = create_repo_with_committed_file("base.txt", "base\n");
    let clean = status_json(clean_repo.path());
    assert_eq!(
        clean["data"]["renames"],
        Value::Array(Vec::new()),
        "clean status should expose an empty renames array"
    );

    let repo = create_repo_with_committed_file("a.txt", "same\ncontent\n");
    fs::rename(repo.path().join("a.txt"), repo.path().join("b.txt"))
        .expect("failed to rename fixture");
    stage_all(repo.path());

    let json = status_json(repo.path());
    let renames = json["data"]["renames"]
        .as_array()
        .expect("renames should be an array");

    assert_eq!(renames.len(), 1, "expected one rename entry: {json}");
    assert_eq!(renames[0]["from"], "a.txt");
    assert_eq!(renames[0]["to"], "b.txt");
    assert_eq!(renames[0]["score"], 100);
}

#[test]
#[serial]
fn rename_large_file_staged_exact_r100() {
    let content = format!("{}\n", "x".repeat(2 * 1024 * 1024 + 1));
    let repo = create_repo_with_committed_file("a.txt", &content);
    fs::rename(repo.path().join("a.txt"), repo.path().join("b.txt"))
        .expect("failed to rename large fixture");
    stage_all(repo.path());

    let output = status_stdout(repo.path(), &["status", "--porcelain", "v2"]);

    assert!(
        output.lines().any(|line| {
            line.starts_with("2 R. ") && line.contains(" R100 ") && line.ends_with("b.txt\ta.txt")
        }),
        "staged large-file rename with known OID must emit R100, got:\n{output}"
    );
}

#[test]
#[serial]
fn rename_large_file_worktree_inexact_skips() {
    let content = format!("{}\n", "x".repeat(2 * 1024 * 1024 + 1));
    let repo = create_repo_with_committed_file("a.txt", &content);
    fs::remove_file(repo.path().join("a.txt")).expect("failed to remove source fixture");
    // Dest is untracked and content differs so Exact OID cannot pair; >2MiB
    // forces worktree/inexact skip. Must enable renameUntracked so probe runs.
    let mut altered = content.into_bytes();
    if let Some(last) = altered.last_mut() {
        *last = b'y';
    }
    fs::write(repo.path().join("b.txt"), altered).expect("failed to write large dest");
    let cfg = run_libra_command(&["config", "set", "status.renameUntracked", "true"], repo.path());
    assert_cli_success(&cfg, "enable renameUntracked");

    let output = status_stdout(repo.path(), &["status", "--porcelain", "v2", "--renames"]);

    assert!(
        !output.lines().any(|line| line.starts_with("2 ")),
        "worktree/inexact large rename should not emit 2-line, got:\n{output}"
    );
    assert!(
        output.lines().any(|line| line.starts_with("1 .D")),
        "deleted source should remain .D when rename fails, got:\n{output}"
    );
    assert!(
        output.lines().any(|line| line == "? b.txt"),
        "untracked dest must stay ?, not staged A, got:\n{output}"
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn typechange_emits_t() {
    let repo = create_repo_with_committed_file("target.txt", "target\n");
    fs::remove_file(repo.path().join("target.txt")).expect("failed to remove regular file");
    fs::write(repo.path().join("elsewhere.txt"), "target\n")
        .expect("failed to write symlink target");
    std::os::unix::fs::symlink("elsewhere.txt", repo.path().join("target.txt"))
        .expect("failed to create symlink");

    let short = status_stdout(repo.path(), &["status", "--short"]);
    assert!(
        short.lines().any(|line| line == " T target.txt"),
        "unstaged file-to-symlink change should emit T, got:\n{short}"
    );

    let v2 = status_stdout(repo.path(), &["status", "--porcelain", "v2"]);
    assert!(
        v2.lines().any(|line| line.starts_with("1  T")),
        "unstaged file-to-symlink change should emit T in porcelain v2, got:\n{v2}"
    );
}

#[test]
fn unmerged_stage_presence_to_xy_mapping() {
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
        assert_eq!(
            libra::command::status::unmerged_xy_for_stage_presence(base, ours, theirs),
            Some(expected),
            "unexpected XY mapping for base={base} ours={ours} theirs={theirs}"
        );
    }

    assert_eq!(
        libra::command::status::unmerged_xy_for_stage_presence(false, false, false),
        None
    );
}

#[test]
fn porcelain_v1_rename_output_stays_add_delete() {
    let staged = libra::command::status::Changes {
        new: vec!["b.txt".into()],
        modified: vec![],
        deleted: vec!["a.txt".into()],
    };
    let unstaged = libra::command::status::Changes::default();
    let mut output = Vec::new();

    libra::command::status::output_porcelain(&staged, &unstaged, &mut output)
        .expect("porcelain v1 output should succeed");

    let rendered = String::from_utf8(output).expect("porcelain v1 should be utf-8");
    assert_eq!(rendered, "D  a.txt\nA  b.txt\n");
}
