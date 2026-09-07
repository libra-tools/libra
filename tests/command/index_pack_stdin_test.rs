use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use libra::utils::pager::LIBRA_TEST_ENV;
use serial_test::serial;
use tempfile::tempdir;

use super::{assert_cli_success, init_repo_via_cli, parse_cli_error_stderr, parse_json_stdout};

fn find_pack(prefix: &str) -> PathBuf {
    let packs_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs");
    let mut matches = Vec::new();
    for entry in fs::read_dir(&packs_dir).expect("read packs dir failed") {
        let entry = entry.expect("dir entry error");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(prefix) && name.ends_with(".pack") {
            matches.push(entry.path());
        }
    }
    match matches.len() {
        0 => panic!("pack with prefix `{prefix}` not found in {:?}", packs_dir),
        1 => matches.remove(0),
        _ => panic!(
            "multiple packs with prefix `{prefix}` found in {:?}",
            packs_dir
        ),
    }
}

fn run_libra_command_with_stdin_bytes(args: &[&str], cwd: &Path, stdin_body: &[u8]) -> Output {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    let global_db = home.join(".libra").join("config.db");
    fs::create_dir_all(&config_home).expect("failed to create isolated config directory");

    let mut child = Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("LIBRA_CONFIG_GLOBAL_DB", &global_db)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env(LIBRA_TEST_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to execute libra binary");

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_body)
            .expect("failed to write stdin to libra process");
    }

    child
        .wait_with_output()
        .expect("failed to collect libra command output")
}

#[test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
fn index_pack_stdin_requires_explicit_output_path() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    let pack_bytes = fs::read(find_pack("small-sha1")).expect("pack fixture should be readable");

    let output =
        run_libra_command_with_stdin_bytes(&["index-pack", "--stdin"], repo.path(), &pack_bytes);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert_eq!(
        stderr,
        "fatal: index-pack --stdin requires -o <INDEX_FILE>\nError-Code: LBR-CLI-002"
    );
}

#[test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
fn index_pack_stdin_writes_pack_and_index_and_reports_json_paths() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    let pack_bytes = fs::read(find_pack("small-sha1")).expect("pack fixture should be readable");
    let output_dir = tempdir().unwrap();
    let index_path = output_dir.path().join("stdin-pack.idx");
    let pack_path = output_dir.path().join("stdin-pack.pack");

    let output = run_libra_command_with_stdin_bytes(
        &[
            "index-pack",
            "--stdin",
            "-o",
            index_path.to_str().unwrap(),
            "--json",
        ],
        repo.path(),
        &pack_bytes,
    );

    assert_cli_success(&output, "index-pack --stdin should succeed");
    assert_eq!(
        fs::read(&pack_path).expect("stdin pack should be persisted"),
        pack_bytes
    );
    assert!(index_path.exists(), "stdin index should be generated");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "index-pack");
    assert_eq!(
        json["data"]["pack_file"],
        pack_path.to_string_lossy().as_ref()
    );
    assert_eq!(
        json["data"]["index_file"],
        index_path.to_string_lossy().as_ref()
    );
    assert_eq!(json["data"]["index_version"], 1);
}
