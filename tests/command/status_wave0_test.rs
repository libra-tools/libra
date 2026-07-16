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
