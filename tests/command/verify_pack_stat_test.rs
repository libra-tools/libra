use std::{
    fs,
    path::{Path, PathBuf},
};

use super::{assert_cli_success, init_repo_via_cli, run_libra_command};

fn packs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs")
}

fn copy_pack_to_temp(prefix: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let pack_src = fs::read_dir(packs_dir())
        .expect("read packs dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".pack"))
        })
        .unwrap_or_else(|| panic!("pack fixture with prefix {prefix:?} not found"));
    let pack_dst = dir.path().join(
        pack_src
            .file_name()
            .expect("pack fixture should have file name"),
    );
    fs::copy(&pack_src, &pack_dst).expect("copy pack fixture");
    (dir, pack_dst)
}

fn build_index(repo: &Path, pack_path: &Path, version: &str) -> PathBuf {
    let output = run_libra_command(
        &[
            "index-pack",
            pack_path.to_str().expect("pack path should be UTF-8"),
            "--index-version",
            version,
        ],
        repo,
    );
    assert_cli_success(&output, "index-pack should build fixture index");
    pack_path.with_extension("idx")
}

#[test]
fn verify_pack_stat_only_reports_non_delta_summary() {
    let repo = tempfile::tempdir().expect("create repo");
    init_repo_via_cli(repo.path());
    let (_pack_dir, pack_path) = copy_pack_to_temp("small-sha1");
    let idx_path = build_index(repo.path(), &pack_path, "2");

    let output = run_libra_command(
        &[
            "verify-pack",
            "--stat-only",
            idx_path.to_str().expect("idx path UTF-8"),
        ],
        repo.path(),
    );
    assert_cli_success(&output, "verify-pack --stat-only should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("non delta:"),
        "stat-only output should summarize non-delta objects: {stdout}"
    );
    assert!(
        !stdout.contains(": ok"),
        "stat-only output should not print the trailing ok line: {stdout}"
    );
}
