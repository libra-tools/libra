//! Minimal `mailinfo` plumbing contracts for plan-20260708 P2-02.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::Value;
use tempfile::tempdir;

const MAIL: &str = "From 0123456789 Mon Sep 17 00:00:00 2001\n\
From: Alice Example <alice@example.com>\n\
Date: Tue, 14 Jul 2026 10:00:00 +0800\n\
Subject: [PATCH v2 1/1] fix greeting\n\
Content-Type: text/plain; charset=UTF-8\n\
Content-Transfer-Encoding: 8bit\n\
\n\
Explain why.\n\
\n\
Second paragraph.\n\
---\n\
\x20file.txt | 2 +-\n\
\x201 file changed, 1 insertion(+), 1 deletion(-)\n\
\n\
diff --git a/file.txt b/file.txt\n\
index 3367afd..3e75765 100644\n\
--- a/file.txt\n\
+++ b/file.txt\n\
@@ -1 +1 @@\n\
-old\n\
+new\n\
-- \n\
libra 0.18.84\n";

fn run_mailinfo(cwd: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
    command
        .args(args)
        .current_dir(cwd)
        .env("LIBRA_TEST", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn libra mailinfo");
    child
        .stdin
        .take()
        .expect("mailinfo stdin")
        .write_all(stdin)
        .expect("write mailinfo stdin");
    child.wait_with_output().expect("wait for libra mailinfo")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "mailinfo failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failure(output: &Output, needle: &str) {
    assert!(!output.status.success(), "mailinfo unexpectedly succeeded");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(needle),
        "stderr must contain {needle:?}:\n{stderr}"
    );
}

#[test]
fn extracts_git_shaped_metadata_message_and_patch_outside_a_repo() {
    let temp = tempdir().expect("create tempdir");
    let message = temp.path().join("message");
    let patch = temp.path().join("patch");
    let output = run_mailinfo(
        temp.path(),
        &["mailinfo", "message", "patch"],
        MAIL.as_bytes(),
    );
    assert_success(&output);

    assert_eq!(
        String::from_utf8(output.stdout).expect("metadata is utf8"),
        "Author: Alice Example\nEmail: alice@example.com\nSubject: fix greeting\nDate: Tue, 14 Jul 2026 10:00:00 +0800\n"
    );
    assert_eq!(
        fs::read_to_string(message).expect("read message output"),
        "Explain why.\n\nSecond paragraph.\n"
    );
    let patch = fs::read_to_string(patch).expect("read patch output");
    assert!(patch.starts_with("---\n file.txt | 2 +-\n"), "{patch}");
    assert!(
        patch.contains("diff --git a/file.txt b/file.txt"),
        "{patch}"
    );
    assert!(patch.ends_with("-- \nlibra 0.18.84\n"), "{patch}");
}

#[test]
fn decodes_folded_headers_quoted_printable_and_in_body_author() {
    let temp = tempdir().expect("create tempdir");
    let mail = MAIL
        .replace(
            "From: Alice Example <alice@example.com>",
            "From: Envelope Author <envelope@example.com>",
        )
        .replace(
            "Subject: [PATCH v2 1/1] fix greeting",
            "Subject: =?UTF-8?Q?[PATCH]_fix=3A?=\n =?UTF-8?Q?_caf=C3=A9?=",
        )
        .replace(
            "Content-Transfer-Encoding: 8bit\n\nExplain why.",
            "Content-Transfer-Encoding: quoted-printable\n\nFrom: Body Author <body@example.com>\n\nExplain=20why.",
        );
    let output = run_mailinfo(temp.path(), &["mailinfo", "msg", "patch"], mail.as_bytes());
    assert_success(&output);

    let stdout = String::from_utf8(output.stdout).expect("metadata is utf8");
    assert!(stdout.contains("Author: Body Author\nEmail: body@example.com\n"));
    assert!(stdout.contains("Subject: fix: café\n"), "{stdout}");
    assert!(
        fs::read_to_string(temp.path().join("msg"))
            .expect("read decoded message")
            .starts_with("Explain why.\n")
    );
}

#[test]
fn json_and_quiet_modes_keep_file_outputs_and_shape_stdout() {
    let temp = tempdir().expect("create tempdir");
    let json = run_mailinfo(
        temp.path(),
        &["--json=compact", "mailinfo", "json.msg", "json.patch"],
        MAIL.as_bytes(),
    );
    assert_success(&json);
    let value: Value = serde_json::from_slice(&json.stdout).expect("parse JSON output");
    assert_eq!(value["command"], "mailinfo");
    assert_eq!(value["data"]["author"], "Alice Example");
    assert_eq!(value["data"]["email"], "alice@example.com");
    assert_eq!(value["data"]["subject"], "fix greeting");
    assert_eq!(value["data"]["message_path"], "json.msg");
    assert!(value["data"]["patch_bytes"].as_u64().is_some());

    let quiet = run_mailinfo(
        temp.path(),
        &["--quiet", "mailinfo", "quiet.msg", "quiet.patch"],
        MAIL.as_bytes(),
    );
    assert_success(&quiet);
    assert!(quiet.stdout.is_empty());
    assert!(temp.path().join("quiet.msg").is_file());
    assert!(temp.path().join("quiet.patch").is_file());
}

#[test]
fn aliased_output_paths_fail_before_overwriting_either_file() {
    let temp = tempdir().expect("create tempdir");
    let target = temp.path().join("out");
    fs::write(&target, "preserve\n").expect("seed output");
    let output = run_mailinfo(temp.path(), &["mailinfo", "out", "./out"], MAIL.as_bytes());
    assert_failure(&output, "must be different files");
    assert_eq!(
        fs::read_to_string(target).expect("read preserved output"),
        "preserve\n"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_parent_aliases_cannot_hide_the_same_output() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("create tempdir");
    let real = temp.path().join("real");
    fs::create_dir(&real).expect("create real output directory");
    symlink(&real, temp.path().join("alias")).expect("create parent symlink");
    fs::write(real.join("out"), "preserve\n").expect("seed output");

    let output = run_mailinfo(
        temp.path(),
        &["mailinfo", "real/out", "alias/out"],
        MAIL.as_bytes(),
    );
    assert_failure(&output, "must be different files");
    assert_eq!(
        fs::read_to_string(real.join("out")).expect("read preserved output"),
        "preserve\n"
    );
}

#[test]
fn invalid_mail_and_invalid_second_destination_preserve_existing_outputs() {
    let temp = tempdir().expect("create tempdir");
    let message = temp.path().join("message");
    let patch = temp.path().join("patch");
    fs::write(&message, "old message\n").expect("seed message");
    fs::write(&patch, "old patch\n").expect("seed patch");

    let invalid = run_mailinfo(
        temp.path(),
        &["mailinfo", "message", "patch"],
        b"not a mail",
    );
    assert_failure(&invalid, "invalid mail patch 'stdin'");
    assert_eq!(
        fs::read_to_string(&message).expect("read message"),
        "old message\n"
    );
    assert_eq!(
        fs::read_to_string(&patch).expect("read patch"),
        "old patch\n"
    );

    fs::create_dir(temp.path().join("directory")).expect("create directory output");
    let bad_patch = run_mailinfo(
        temp.path(),
        &["mailinfo", "message", "directory"],
        MAIL.as_bytes(),
    );
    assert_failure(&bad_patch, "patch output 'directory' is a directory");
    assert_eq!(
        fs::read_to_string(message).expect("read message"),
        "old message\n"
    );

    let missing_parent = run_mailinfo(
        temp.path(),
        &["mailinfo", "message", "missing/patch"],
        MAIL.as_bytes(),
    );
    assert_failure(&missing_parent, "cannot access parent directory 'missing'");
    assert_eq!(
        fs::read_to_string(temp.path().join("message")).expect("read message"),
        "old message\n"
    );
}

#[test]
fn unsupported_multipart_and_non_utf8_input_fail_closed() {
    let temp = tempdir().expect("create tempdir");
    // PD-09 ④: well-formed multipart is now SUPPORTED; a mail that
    // declares a boundary but carries no parts for it still fails
    // closed rather than silently parsing garbage.
    let multipart = MAIL.replace(
        "Content-Type: text/plain; charset=UTF-8",
        "Content-Type: multipart/mixed; boundary=x",
    );
    assert_failure(
        &run_mailinfo(temp.path(), &["mailinfo", "a", "b"], multipart.as_bytes()),
        "multipart mail has no parts for its declared boundary",
    );
    assert!(!temp.path().join("a").exists());
    assert!(!temp.path().join("b").exists());

    assert_failure(
        &run_mailinfo(temp.path(), &["mailinfo", "a", "b"], &[0xff, 0xfe]),
        "not valid UTF-8",
    );
    assert!(!temp.path().join("a").exists());
    assert!(!temp.path().join("b").exists());
}

#[test]
fn help_documents_the_minimal_stdin_contract() {
    let temp = tempdir().expect("create tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(["mailinfo", "--help"])
        .current_dir(temp.path())
        .output()
        .expect("run mailinfo help");
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).expect("help is utf8");
    assert!(
        stdout.contains("Usage: libra mailinfo [OPTIONS] <MSG> <PATCH>"),
        "{stdout}"
    );
    assert!(stdout.contains("EXAMPLES:"), "{stdout}");
    assert!(
        stdout.contains("beginning at the `---` separator"),
        "{stdout}"
    );
}
