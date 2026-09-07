//! Tests for `verify-pack`, covering generated v1/v2 indexes, verbose output,
//! JSON output, and corrupt/missing pack failures.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serial_test::serial;
use sha1::{Digest, Sha1};

use super::{
    assert_cli_success, init_repo_via_cli, parse_cli_error_stderr, parse_json_stdout,
    run_libra_command,
};

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

fn init_sha256_repo_via_cli(repo: &Path) {
    fs::create_dir_all(repo).expect("create repo dir");
    let output = run_libra_command(&["init", "--object-format", "sha256"], repo);
    assert_cli_success(&output, "failed to initialize sha256 repository");
}

fn corrupt_v1_index_with_duplicate_first_entry(idx_path: &Path) {
    const FANOUT_LEN: usize = 256 * 4;
    const HASH_LEN: usize = 20;
    const ENTRY_LEN: usize = 4 + HASH_LEN;

    let mut bytes = fs::read(idx_path).expect("read generated idx");
    let object_count = u32::from_be_bytes(
        bytes[FANOUT_LEN - 4..FANOUT_LEN]
            .try_into()
            .expect("fanout[255] is present"),
    ) as usize;
    assert!(
        object_count >= 2,
        "fixture index needs at least two objects"
    );

    let entries_start = FANOUT_LEN;
    let first_entry = bytes[entries_start..entries_start + ENTRY_LEN].to_vec();
    bytes[entries_start + ENTRY_LEN..entries_start + ENTRY_LEN * 2].copy_from_slice(&first_entry);

    let mut fanout = [0u32; 256];
    for idx in 0..object_count {
        let hash_start = entries_start + idx * ENTRY_LEN + 4;
        fanout[bytes[hash_start] as usize] += 1;
    }
    for idx in 1..fanout.len() {
        fanout[idx] += fanout[idx - 1];
    }
    for (idx, count) in fanout.iter().enumerate() {
        let start = idx * 4;
        bytes[start..start + 4].copy_from_slice(&count.to_be_bytes());
    }

    let checksum_start = bytes.len() - HASH_LEN;
    let checksum: [u8; HASH_LEN] = Sha1::digest(&bytes[..checksum_start]).into();
    bytes[checksum_start..].copy_from_slice(&checksum);
    fs::write(idx_path, bytes).expect("write duplicate idx");
}

#[test]
fn verify_pack_accepts_generated_v1_index() {
    let repo = tempfile::tempdir().expect("create repo");
    init_repo_via_cli(repo.path());
    let (_pack_dir, pack_path) = copy_pack_to_temp("small-sha1");
    let idx_path = build_index(repo.path(), &pack_path, "1");

    let output = run_libra_command(
        &["verify-pack", idx_path.to_str().expect("idx path UTF-8")],
        repo.path(),
    );
    assert_cli_success(&output, "verify-pack should accept generated v1 index");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(": ok"),
        "human output should confirm success: {stdout}"
    );
    assert!(
        output.stderr.is_empty(),
        "stderr should be clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn verify_pack_uses_repository_hash_kind_for_sha256_indexes() {
    let repo = tempfile::tempdir().expect("create repo");
    init_sha256_repo_via_cli(repo.path());
    let (_pack_dir, pack_path) = copy_pack_to_temp("small-sha256");
    let idx_path = build_index(repo.path(), &pack_path, "2");

    let output = run_libra_command(
        &[
            "verify-pack",
            idx_path.to_str().expect("idx path UTF-8"),
            "--json",
        ],
        repo.path(),
    );
    assert_cli_success(
        &output,
        "verify-pack should use repository sha256 object format",
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "verify-pack");
    assert_eq!(json["data"]["index_version"], 2);
    assert_eq!(
        json["data"]["pack_hash"]
            .as_str()
            .expect("pack_hash should be string")
            .len(),
        64
    );
}

#[test]
fn verify_pack_accepts_absolute_index_path_outside_repository() {
    let repo = tempfile::tempdir().expect("create repo");
    init_repo_via_cli(repo.path());
    let (_pack_dir, pack_path) = copy_pack_to_temp("small-sha1");
    let idx_path = build_index(repo.path(), &pack_path, "1");
    let outside = tempfile::tempdir().expect("create non-repo cwd");

    let output = run_libra_command(
        &["verify-pack", idx_path.to_str().expect("idx path UTF-8")],
        outside.path(),
    );
    assert_cli_success(
        &output,
        "verify-pack should accept absolute index paths outside a repo",
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(": ok"),
        "outside-repo verification should confirm success: {stdout}"
    );
}

#[test]
fn verify_pack_accepts_sha256_index_path_outside_repository() {
    let repo = tempfile::tempdir().expect("create repo");
    init_sha256_repo_via_cli(repo.path());
    let (_pack_dir, pack_path) = copy_pack_to_temp("small-sha256");
    let idx_path = build_index(repo.path(), &pack_path, "2");
    let outside = tempfile::tempdir().expect("create non-repo cwd");

    let output = run_libra_command(
        &[
            "verify-pack",
            idx_path.to_str().expect("idx path UTF-8"),
            "--json",
        ],
        outside.path(),
    );
    assert_cli_success(
        &output,
        "verify-pack should infer sha256 index format outside a repo",
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["index_version"], 2);
    assert_eq!(
        json["data"]["pack_hash"]
            .as_str()
            .expect("pack_hash should be string")
            .len(),
        64
    );
}

#[test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
fn verify_pack_rejects_duplicate_object_ids_even_with_valid_index_checksum() {
    let repo = tempfile::tempdir().expect("create repo");
    init_repo_via_cli(repo.path());
    let (_pack_dir, pack_path) = copy_pack_to_temp("small-sha1");
    let idx_path = build_index(repo.path(), &pack_path, "1");
    corrupt_v1_index_with_duplicate_first_entry(&idx_path);

    let output = run_libra_command(
        &["verify-pack", idx_path.to_str().expect("idx path UTF-8")],
        repo.path(),
    );
    assert!(
        !output.status.success(),
        "duplicate index object IDs should fail verification"
    );

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        human.contains("not strictly sorted"),
        "error should identify duplicate or unsorted object IDs: {human}"
    );
}

#[test]
fn verify_pack_accepts_generated_v2_index_with_verbose_objects() {
    let repo = tempfile::tempdir().expect("create repo");
    init_repo_via_cli(repo.path());
    let (_pack_dir, pack_path) = copy_pack_to_temp("small-sha1");
    let idx_path = build_index(repo.path(), &pack_path, "2");

    let output = run_libra_command(
        &[
            "verify-pack",
            "-v",
            idx_path.to_str().expect("idx path UTF-8"),
        ],
        repo.path(),
    );
    assert_cli_success(&output, "verify-pack should accept generated v2 index");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let object_lines = stdout
        .lines()
        .filter(|line| !line.ends_with(": ok"))
        .collect::<Vec<_>>();
    assert!(
        !object_lines.is_empty(),
        "expected verbose object rows: {stdout}"
    );
    for line in object_lines {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        assert_eq!(
            fields.len(),
            5,
            "verbose rows should match Git's '<oid> <type> <size> <size-in-pack> <offset>' shape: {line}"
        );
        assert!(
            matches!(fields[1], "blob" | "tree" | "commit" | "tag"),
            "verbose type field should be a Git object type: {line}"
        );
        for field in &fields[2..] {
            field
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("verbose numeric field should parse: {line}"));
        }
    }
    assert!(
        stdout.trim_end().ends_with(": ok"),
        "verbose output should end with success line: {stdout}"
    );
}

#[test]
fn verify_pack_json_reports_counts_and_hashes() {
    let repo = tempfile::tempdir().expect("create repo");
    init_repo_via_cli(repo.path());
    let (_pack_dir, pack_path) = copy_pack_to_temp("small-sha1");
    let idx_path = build_index(repo.path(), &pack_path, "1");

    let output = run_libra_command(
        &[
            "verify-pack",
            idx_path.to_str().expect("idx path UTF-8"),
            "--json",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "verify-pack --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "verify-pack");
    assert_eq!(json["data"]["verified"], true);
    assert_eq!(json["data"]["index_version"], 1);
    assert!(
        json["data"]["object_count"].as_u64().unwrap_or_default() > 0,
        "object_count should be positive: {json}"
    );
    assert!(
        json["data"]["pack_hash"].as_str().unwrap_or_default().len() >= 40,
        "pack_hash should be present: {json}"
    );
    assert!(
        json["data"].get("objects").is_none(),
        "non-verbose JSON should not include per-object payloads: {json}"
    );
}

#[test]
fn verify_pack_rejects_corrupt_index_entry() {
    let repo = tempfile::tempdir().expect("create repo");
    init_repo_via_cli(repo.path());
    let (_pack_dir, pack_path) = copy_pack_to_temp("small-sha1");
    let idx_path = build_index(repo.path(), &pack_path, "1");

    let mut bytes = fs::read(&idx_path).expect("read generated idx");
    let first_hash_byte = 256 * 4 + 4;
    bytes[first_hash_byte] ^= 0x80;
    fs::write(&idx_path, bytes).expect("write corrupt idx");

    let output = run_libra_command(
        &["verify-pack", idx_path.to_str().expect("idx path UTF-8")],
        repo.path(),
    );
    assert!(
        !output.status.success(),
        "corrupt index should fail verification"
    );

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        human.contains("invalid pack index") || human.contains("pack verification failed"),
        "error should explain verification failure: {human}"
    );
}

#[test]
fn verify_pack_reports_missing_pack_as_read_error() {
    let repo = tempfile::tempdir().expect("create repo");
    init_repo_via_cli(repo.path());
    let (_pack_dir, pack_path) = copy_pack_to_temp("small-sha1");
    let idx_path = build_index(repo.path(), &pack_path, "1");
    fs::remove_file(&pack_path).expect("remove pack fixture");

    let output = run_libra_command(
        &["verify-pack", idx_path.to_str().expect("idx path UTF-8")],
        repo.path(),
    );
    assert!(!output.status.success(), "missing pack should fail");

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-IO-001");
    assert!(
        human.contains("could not open pack file"),
        "error should identify missing pack: {human}"
    );
}
