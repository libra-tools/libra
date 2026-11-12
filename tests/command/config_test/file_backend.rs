//! CF-13 (M-FILEW X1..X8): `--file` write backend tests.

use std::fs;

use serial_test::serial;
use tempfile::tempdir;

use crate::command::{assert_cli_success, run_libra_command};

/// CF-13 (M-FILEW X1..X7): `--file` writes follow Git placement/quoting,
/// replace atomically, and refuse vault/stdin writes.
#[test]
#[serial(cwd, env)]
fn test_file_write_bytes_match_git_matrix() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "init",
    );
    let cfg = temp.path().join("explicit.config");
    let cfg_str = cfg.to_str().unwrap().to_string();

    // X1/X2: a fresh file gets a section and the variable.
    assert_cli_success(
        &run_libra_command(&["config", "--file", &cfg_str, "core.editor", "vim"], &repo),
        "file set creates",
    );
    assert_eq!(
        fs::read_to_string(&cfg).unwrap(),
        "[core]\n\teditor = vim\n"
    );

    // Existing variable updated in place, new variable appended to the section.
    assert_cli_success(
        &run_libra_command(
            &["config", "--file", &cfg_str, "core.editor", "nano"],
            &repo,
        ),
        "file set updates",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--file", &cfg_str, "core.bare", "false"], &repo),
        "file set appends",
    );
    assert_eq!(
        fs::read_to_string(&cfg).unwrap(),
        "[core]\n\teditor = nano\n\tbare = false\n"
    );

    // Quoting: a value with a semicolon must be quoted.
    assert_cli_success(
        &run_libra_command(&["config", "--file", &cfg_str, "core.note", "a;b"], &repo),
        "file set quotes",
    );
    assert!(
        fs::read_to_string(&cfg)
            .unwrap()
            .contains("\tnote = \"a;b\"\n"),
        "value with ';' must be quoted: {}",
        fs::read_to_string(&cfg).unwrap()
    );

    // X3: unset / unset-all.
    assert_cli_success(
        &run_libra_command(
            &["config", "--file", &cfg_str, "--unset", "core.bare"],
            &repo,
        ),
        "file unset",
    );
    assert!(!fs::read_to_string(&cfg).unwrap().contains("bare"));

    assert_cli_success(
        &run_libra_command(
            &["config", "--file", &cfg_str, "--add", "multi.k", "1"],
            &repo,
        ),
        "file add 1",
    );
    assert_cli_success(
        &run_libra_command(
            &["config", "--file", &cfg_str, "--add", "multi.k", "2"],
            &repo,
        ),
        "file add 2",
    );
    assert_cli_success(
        &run_libra_command(
            &["config", "--file", &cfg_str, "--unset-all", "multi.k"],
            &repo,
        ),
        "file unset-all",
    );
    assert!(!fs::read_to_string(&cfg).unwrap().contains("multi.k"));

    // X3: replace-all collapses multiple values.
    assert_cli_success(
        &run_libra_command(&["config", "--file", &cfg_str, "--add", "r.k", "1"], &repo),
        "file add r1",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--file", &cfg_str, "--add", "r.k", "2"], &repo),
        "file add r2",
    );
    assert_cli_success(
        &run_libra_command(
            &["config", "--file", &cfg_str, "--replace-all", "r.k", "9"],
            &repo,
        ),
        "file replace-all",
    );
    let text = fs::read_to_string(&cfg).unwrap();
    assert_eq!(text.matches("k = 9").count(), 1, "{text}");
    assert!(!text.contains("k = 1") && !text.contains("k = 2"), "{text}");

    // X3: remove-section and rename-section.
    assert_cli_success(
        &run_libra_command(
            &["config", "--file", &cfg_str, "--remove-section", "multi"],
            &repo,
        ),
        "file remove-section",
    );
    assert!(!fs::read_to_string(&cfg).unwrap().contains("[multi]"));

    assert_cli_success(
        &run_libra_command(
            &[
                "config",
                "--file",
                &cfg_str,
                "--rename-section",
                "r",
                "renamed",
            ],
            &repo,
        ),
        "file rename-section",
    );
    assert!(fs::read_to_string(&cfg).unwrap().contains("[renamed]"));

    // X6: vault/stdin writes are refused and write nothing.
    let before = fs::read_to_string(&cfg).unwrap();
    let stdin_write = run_libra_command(
        &["config", "--file", &cfg_str, "--stdin", "vault.new"],
        &repo,
    );
    assert_eq!(stdin_write.status.code(), Some(129));
    assert_eq!(fs::read_to_string(&cfg).unwrap(), before);

    let encrypt_write = run_libra_command(
        &["config", "--file", &cfg_str, "--encrypt", "secret.k", "v"],
        &repo,
    );
    assert_eq!(encrypt_write.status.code(), Some(129));
    assert_eq!(fs::read_to_string(&cfg).unwrap(), before);

    // X5: a leftover lock file blocks the write and is not clobbered.
    let lock = std::path::PathBuf::from(format!("{}.lock", cfg.display()));
    fs::write(&lock, b"held").unwrap();
    let blocked = run_libra_command(
        &["config", "--file", &cfg_str, "core.editor", "emacs"],
        &repo,
    );
    assert_eq!(blocked.status.code(), Some(128), "lock conflict exits 128");
    assert_eq!(fs::read_to_string(&cfg).unwrap(), before);
    assert_eq!(fs::read_to_string(&lock).unwrap(), "held");
    fs::remove_file(&lock).unwrap();

    // X4: unset on a missing key exits 5 with no rewrite.
    let missing = run_libra_command(&["config", "--file", &cfg_str, "--unset", "no.such"], &repo);
    assert_eq!(missing.status.code(), Some(5));
    assert_eq!(fs::read_to_string(&cfg).unwrap(), before);
}

/// CF-13 (M-FILEW X8): the `--file` form round-trips through the reader.
#[test]
#[serial(cwd, env)]
fn test_file_write_roundtrip_through_reader() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "init",
    );
    let cfg = temp.path().join("roundtrip.config");
    let cfg_str = cfg.to_str().unwrap().to_string();

    assert_cli_success(
        &run_libra_command(
            &["config", "--file", &cfg_str, "user.name", "Jane Doe"],
            &repo,
        ),
        "write",
    );
    let read = run_libra_command(&["config", "--file", &cfg_str, "--get", "user.name"], &repo);
    assert_cli_success(&read, "read");
    assert_eq!(String::from_utf8_lossy(&read.stdout).trim(), "Jane Doe");

    let list = run_libra_command(&["config", "--file", &cfg_str, "--list"], &repo);
    assert_cli_success(&list, "list");
    assert!(String::from_utf8_lossy(&list.stdout).contains("user.name=Jane Doe"));
}
