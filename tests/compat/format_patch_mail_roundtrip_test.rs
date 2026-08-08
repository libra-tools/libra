//! P2-03 mail-output compatibility and Git/Libra round trips.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use tempfile::{TempDir, tempdir};

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("create tempdir");
        let root = temp.path().to_path_buf();
        let home = root.join("home");
        fs::create_dir_all(home.join(".config")).expect("create isolated config home");
        Self {
            _temp: temp,
            root,
            home,
        }
    }

    fn libra(&self, cwd: &Path, args: &[&str]) -> Output {
        let global_db = self.home.join(".libra").join("config.db");
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("LIBRA_CONFIG_GLOBAL_DB", global_db)
            .env("LIBRA_TEST", "1")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .output()
            .expect("spawn libra")
    }

    fn libra_success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.libra(cwd, args);
        assert_success("libra", args, &output);
        output
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .output()
            .expect("spawn git")
    }

    fn git_success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.git(cwd, args);
        assert_success("git", args, &output);
        output
    }

    fn init_libra(&self, name: &str) -> PathBuf {
        let repo = self.root.join(name);
        fs::create_dir_all(&repo).expect("create Libra repo");
        self.libra_success(&repo, &["init", "--vault", "false"]);
        self.libra_success(&repo, &["config", "set", "user.name", "Mail Tester"]);
        self.libra_success(
            &repo,
            &["config", "set", "user.email", "mail-tester@example.com"],
        );
        repo
    }

    fn init_git(&self, name: &str) -> PathBuf {
        let repo = self.root.join(name);
        fs::create_dir_all(&repo).expect("create Git repo");
        self.git_success(&repo, &["init", "-q"]);
        self.git_success(&repo, &["config", "user.name", "Mail Tester"]);
        self.git_success(&repo, &["config", "user.email", "mail-tester@example.com"]);
        repo
    }

    fn libra_commit(&self, repo: &Path, content: &str, subject: &str) -> String {
        fs::write(repo.join("file.txt"), content).expect("write Libra fixture");
        self.libra_success(repo, &["add", "file.txt"]);
        self.libra_success(repo, &["commit", "-m", subject, "--no-verify"]);
        stdout_trim(&self.libra_success(repo, &["rev-parse", "HEAD"]))
    }

    fn git_commit(&self, repo: &Path, content: &str, subject: &str) {
        fs::write(repo.join("file.txt"), content).expect("write Git fixture");
        self.git_success(repo, &["add", "file.txt"]);
        self.git_success(repo, &["commit", "-q", "-m", subject]);
    }
}

fn assert_success(tool: &str, args: &[&str], output: &Output) {
    assert!(
        output.status.success(),
        "{tool} {} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout_trim(output: &Output) -> String {
    String::from_utf8(output.stdout.clone())
        .expect("stdout is UTF-8")
        .trim()
        .to_string()
}

fn sorted_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .expect("read output directory")
        .map(|entry| entry.expect("read output entry").path())
        .collect();
    files.sort();
    files
}

#[test]
fn libra_last_one_stdout_applies_with_git_am() {
    let fixture = Fixture::new();
    let source = fixture.init_libra("libra-source");
    fixture.libra_commit(&source, "base\n", "base");
    fixture.libra_commit(&source, "base\nfrom libra\n", "libra mail change");

    let patch = fixture.libra_success(&source, &["format-patch", "-1", "--stdout"]);
    let mail = String::from_utf8(patch.stdout.clone()).expect("Libra mail is UTF-8");
    assert_eq!(mail.matches("Subject:").count(), 1, "{mail}");
    assert!(
        mail.contains("Subject: [PATCH] libra mail change"),
        "{mail}"
    );

    let target = fixture.init_git("git-target");
    fixture.git_commit(&target, "base\n", "base");
    let patch_path = fixture.root.join("libra.patch");
    fs::write(&patch_path, patch.stdout).expect("write Libra patch");
    fixture.git_success(
        &target,
        &["am", patch_path.to_str().expect("UTF-8 patch path")],
    );
    assert_eq!(
        fs::read_to_string(target.join("file.txt")).expect("read Git result"),
        "base\nfrom libra\n"
    );
    assert_eq!(
        stdout_trim(&fixture.git_success(&target, &["log", "-1", "--format=%s"])),
        "libra mail change"
    );
}

#[test]
fn git_format_patch_applies_with_libra_am() {
    let fixture = Fixture::new();
    let source = fixture.init_git("git-source");
    fixture.git_commit(&source, "base\n", "base");
    fixture.git_commit(&source, "base\nfrom git\n", "git mail change");
    let patch = fixture.git_success(&source, &["format-patch", "-1", "--stdout"]);
    let patch_path = fixture.root.join("git.patch");
    fs::write(&patch_path, patch.stdout).expect("write Git patch");

    let target = fixture.init_libra("libra-target");
    fixture.libra_commit(&target, "base\n", "base");
    fixture.libra_success(
        &target,
        &["am", patch_path.to_str().expect("UTF-8 patch path")],
    );
    assert_eq!(
        fs::read_to_string(target.join("file.txt")).expect("read Libra result"),
        "base\nfrom git\n"
    );
    assert!(
        String::from_utf8_lossy(&fixture.libra_success(&target, &["log", "-1"]).stdout)
            .contains("git mail change")
    );
}

#[test]
fn format_config_defaults_apply_and_cli_options_override_them() {
    let fixture = Fixture::new();
    let repo = fixture.init_libra("config-source");
    fixture.libra_commit(&repo, "base\n", "base");
    fixture.libra_commit(&repo, "base\nconfigured\n", "configured mail");
    let configured_dir = fixture.root.join("configured-output");
    fixture.libra_success(&repo, &["config", "set", "format.subjectPrefix", "RFC"]);
    fixture.libra_success(&repo, &["config", "set", "format.signOff", "true"]);
    fixture.libra_success(
        &repo,
        &[
            "config",
            "set",
            "format.outputDirectory",
            configured_dir.to_str().expect("UTF-8 output dir"),
        ],
    );
    fixture.libra_success(&repo, &["config", "set", "format.suffix", ".mail"]);

    fixture.libra_success(&repo, &["format-patch", "HEAD~1"]);
    let configured_files = sorted_files(&configured_dir);
    assert_eq!(configured_files.len(), 1);
    assert_eq!(
        configured_files[0]
            .extension()
            .and_then(|value| value.to_str()),
        Some("mail")
    );
    let configured = fs::read_to_string(&configured_files[0]).expect("read configured mail");
    assert!(
        configured.contains("Subject: [RFC] configured mail"),
        "{configured}"
    );
    assert!(
        configured.contains("Signed-off-by: Mail Tester <mail-tester@example.com>"),
        "{configured}"
    );

    let explicit_dir = fixture.root.join("explicit-output");
    fixture.libra_success(
        &repo,
        &[
            "format-patch",
            "--subject-prefix",
            "PATCH",
            "--no-signoff",
            "--suffix",
            ".patch",
            "-o",
            explicit_dir.to_str().expect("UTF-8 explicit dir"),
            "HEAD~1",
        ],
    );
    let explicit = fs::read_to_string(&sorted_files(&explicit_dir)[0]).expect("read explicit mail");
    assert!(
        explicit.contains("Subject: [PATCH] configured mail"),
        "{explicit}"
    );
    assert!(!explicit.contains("Signed-off-by:"), "{explicit}");

    // Defaults made irrelevant by explicit output/signoff choices must not
    // break a pipeline, even when the stored values are invalid.
    fixture.libra_success(&repo, &["config", "set", "format.outputDirectory", ""]);
    fixture.libra_success(
        &repo,
        &["config", "set", "format.signOff", "definitely-not-a-bool"],
    );
    let bypassed =
        fixture.libra_success(&repo, &["format-patch", "-1", "--stdout", "--no-signoff"]);
    assert!(String::from_utf8_lossy(&bypassed.stdout).contains("Subject: [RFC] configured mail"));
    let invalid = fixture.libra(&repo, &["format-patch", "-1", "--stdout"]);
    assert!(
        !invalid.status.success(),
        "invalid format.signOff was accepted"
    );
    assert!(
        String::from_utf8_lossy(&invalid.stderr)
            .contains("bad config value 'definitely-not-a-bool' for 'format.signOff'"),
        "{}",
        String::from_utf8_lossy(&invalid.stderr)
    );
}

#[test]
fn cover_letter_and_external_reply_headers_form_a_valid_thread() {
    let fixture = Fixture::new();
    let repo = fixture.init_libra("thread-source");
    fixture.libra_commit(&repo, "base\n", "base");
    fixture.libra_commit(&repo, "base\none\n", "mail one");
    fixture.libra_commit(&repo, "base\none\ntwo\n", "mail two");
    let out = fixture.root.join("thread-output");
    fixture.libra_success(
        &repo,
        &[
            "format-patch",
            "--cover-letter",
            "--thread",
            "--in-reply-to",
            "<parent@example.com>",
            "-o",
            out.to_str().expect("UTF-8 thread dir"),
            "HEAD~2..HEAD",
        ],
    );
    let files = sorted_files(&out);
    assert_eq!(files.len(), 3);
    let cover = fs::read_to_string(&files[0]).expect("read cover letter");
    let first = fs::read_to_string(&files[1]).expect("read first patch");
    let second = fs::read_to_string(&files[2]).expect("read second patch");
    assert!(cover.contains("Subject: [PATCH 0/2]"), "{cover}");
    assert!(
        cover.contains("In-Reply-To: <parent@example.com>"),
        "{cover}"
    );
    assert!(!cover.contains("<<parent@example.com>>"), "{cover}");
    let cover_id = cover
        .lines()
        .find_map(|line| line.strip_prefix("Message-ID: "))
        .expect("cover Message-ID");
    assert!(
        first.contains(&format!("In-Reply-To: {cover_id}")),
        "{first}"
    );
    assert!(
        second.contains(&format!("In-Reply-To: {cover_id}")),
        "{second}"
    );
    assert!(
        first.contains("References: <parent@example.com>"),
        "{first}"
    );
    assert!(
        second.contains("References: <parent@example.com>"),
        "{second}"
    );
}

#[test]
fn mime_attachment_from_libra_applies_with_git_am() {
    let fixture = Fixture::new();
    let source = fixture.init_libra("mime-source");
    fixture.libra_commit(&source, "base\n", "base");
    fixture.libra_commit(&source, "base\nmime\n", "mime mail");
    let patch = fixture.libra_success(&source, &["format-patch", "-1", "--attach", "--stdout"]);
    let mail = String::from_utf8(patch.stdout.clone()).expect("MIME mail is UTF-8");
    let boundary = mail
        .lines()
        .find_map(|line| line.strip_prefix("Content-Type: multipart/mixed; boundary=\""))
        .and_then(|value| value.strip_suffix('"'))
        .expect("MIME boundary");
    assert_eq!(mail.matches(&format!("--{boundary}")).count(), 3, "{mail}");
    assert!(mail.contains(&format!("--{boundary}--")), "{mail}");

    let target = fixture.init_git("mime-git-target");
    fixture.git_commit(&target, "base\n", "base");
    let patch_path = fixture.root.join("mime.patch");
    fs::write(&patch_path, patch.stdout).expect("write MIME patch");
    fixture.git_success(
        &target,
        &["am", patch_path.to_str().expect("UTF-8 MIME patch path")],
    );
    assert_eq!(
        fs::read_to_string(target.join("file.txt")).expect("read MIME result"),
        "base\nmime\n"
    );
}

#[test]
fn root_algorithms_full_index_and_custom_prefixes_are_effective() {
    let fixture = Fixture::new();
    let repo = fixture.init_libra("diff-source");
    // Repeated noise with sparse inserted anchors makes Histogram choose a
    // different valid hunk layout than Myers (the similar backend's intended
    // low-frequency-anchor case).
    let old = "A\nA\nA\nA\nA\nA\nA\n";
    let new = "B\nA\nB\nA\nA\nB\nA\n";
    fixture.libra_commit(&repo, old, "root mail");
    fixture.libra_commit(&repo, new, "reordered mail");

    let root = fixture.libra_success(&repo, &["format-patch", "--root", "--stdout"]);
    assert_eq!(
        String::from_utf8_lossy(&root.stdout)
            .matches("Subject:")
            .count(),
        2
    );

    let default = fixture.libra_success(&repo, &["format-patch", "-1", "--stdout"]);
    let minimal = fixture.libra_success(&repo, &["format-patch", "-1", "--minimal", "--stdout"]);
    assert_eq!(
        minimal.stdout, default.stdout,
        "minimal Myers is shortest by default"
    );
    let histogram =
        fixture.libra_success(&repo, &["format-patch", "-1", "--histogram", "--stdout"]);
    assert_ne!(
        histogram.stdout, default.stdout,
        "histogram must change this hunk layout"
    );

    fixture.libra_commit(
        &repo,
        "B\nA\nB\nA\nA\nB\nA\ndiff --git a/file.txt b/file.txt\n--- a/file.txt\n+++ b/file.txt\n",
        "header-like payload",
    );

    let prefixed = fixture.libra_success(
        &repo,
        &[
            "format-patch",
            "-1",
            "--full-index",
            "--src-prefix",
            "old/",
            "--dst-prefix",
            "new/",
            "--stdout",
        ],
    );
    let mail = String::from_utf8(prefixed.stdout).expect("prefixed mail is UTF-8");
    assert!(
        mail.contains("diff --git old/file.txt new/file.txt"),
        "{mail}"
    );
    assert!(
        mail.contains("--- old/file.txt\n+++ new/file.txt"),
        "{mail}"
    );
    assert!(
        mail.contains("+diff --git a/file.txt b/file.txt\n+--- a/file.txt\n++++ b/file.txt"),
        "custom prefixes must not rewrite header-looking payload lines:\n{mail}"
    );
    let ids = mail
        .lines()
        .find_map(|line| line.strip_prefix("index "))
        .and_then(|line| line.split_whitespace().next())
        .expect("index ids");
    let (old_id, new_id) = ids.split_once("..").expect("old..new ids");
    assert_eq!(old_id.len(), 40, "{mail}");
    assert_eq!(new_id.len(), 40, "{mail}");
}

#[test]
fn ignore_if_in_upstream_suppresses_patch_equivalent_commits() {
    let fixture = Fixture::new();
    let repo = fixture.init_libra("upstream-source");
    let base = fixture.libra_commit(&repo, "base\n", "base");
    fixture.libra_commit(&repo, "base\nsame\n", "upstream change");
    fixture.libra_success(&repo, &["branch", "upstream"]);
    fixture.libra_success(&repo, &["checkout", "-b", "topic", &base]);
    fixture.libra_commit(&repo, "base\nsame\n", "duplicate change");

    let output = fixture.libra_success(
        &repo,
        &[
            "format-patch",
            "--ignore-if-in-upstream",
            "--stdout",
            "upstream..topic",
        ],
    );
    assert!(
        output.stdout.is_empty(),
        "equivalent patch was not suppressed"
    );
}

/// PD-09 ① gate: real `git format-patch --stdout` mbox piped into
/// `libra am -`. Skips (never fails) when git is not installed.
#[test]
fn git_format_patch_stdout_mbox_applies_with_libra_am_stdin() {
    if Command::new("git")
        .arg("--version")
        .output()
        .map(|out| !out.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipped (install git to run the format-patch mbox round-trip gate)");
        return;
    }
    let fixture = Fixture::new();
    let source = fixture.init_git("git-mbox-source");
    fixture.git_commit(&source, "base\n", "base");
    fixture.git_commit(&source, "base\none\n", "mbox change one");
    fixture.git_commit(&source, "base\none\ntwo\n", "mbox change two");
    let mbox = fixture.git_success(&source, &["format-patch", "--stdout", "HEAD~2..HEAD"]);

    let target = fixture.init_libra("libra-mbox-target");
    fixture.libra_commit(&target, "base\n", "base");

    use std::{io::Write, process::Stdio};
    let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
    command
        .args(["am", "-"])
        .current_dir(&target)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &fixture.home)
        .env("USERPROFILE", &fixture.home)
        .env("XDG_CONFIG_HOME", fixture.home.join(".config"))
        .env(
            "LIBRA_CONFIG_GLOBAL_DB",
            fixture.home.join(".libra").join("config.db"),
        )
        .env("LIBRA_TEST", "1")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn libra am -");
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(&mbox.stdout)
        .expect("write mbox to stdin");
    let out = child.wait_with_output().expect("wait for libra am");
    assert_success("libra", &["am", "-"], &out);

    assert_eq!(
        fs::read_to_string(target.join("file.txt")).expect("read result"),
        "base\none\ntwo\n"
    );
    let log = String::from_utf8_lossy(&fixture.libra_success(&target, &["log", "-2"]).stdout)
        .into_owned();
    assert!(log.contains("mbox change one"), "{log}");
    assert!(log.contains("mbox change two"), "{log}");
}

/// PD-09 ② gate: real `git format-patch --binary` output — binary add,
/// rename, and a mode flip — applies with `libra am`. Skips when git is
/// not installed.
#[test]
fn git_binary_rename_mode_patches_apply_with_libra_am() {
    if Command::new("git")
        .arg("--version")
        .output()
        .map(|out| !out.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipped (install git to run the extended-patch round-trip gate)");
        return;
    }
    let fixture = Fixture::new();
    let source = fixture.init_git("git-ext-source");
    fixture.git_commit(&source, "base\n", "base");

    // Commit 1: add a binary blob (NUL bytes force binary detection).
    let payload: Vec<u8> = (0u8..=255).cycle().take(600).collect();
    fs::write(source.join("blob.bin"), &payload).expect("write binary blob");
    fixture.git_success(&source, &["add", "blob.bin"]);
    fixture.git_success(&source, &["commit", "-q", "-m", "add binary"]);
    // Commit 2: rename the text file.
    fixture.git_success(&source, &["mv", "file.txt", "renamed.txt"]);
    fixture.git_success(&source, &["commit", "-q", "-m", "rename text"]);
    // Commit 3: chmod-only.
    let sh = source.join("run.sh");
    fs::write(&sh, "#!/bin/sh\n").expect("write script");
    fixture.git_success(&source, &["add", "run.sh"]);
    fixture.git_success(&source, &["commit", "-q", "-m", "add script"]);
    fixture.git_success(&source, &["update-index", "--chmod=+x", "run.sh"]);
    fixture.git_success(&source, &["commit", "-q", "-m", "make script executable"]);

    let out_dir = fixture.root.join("ext-patches");
    fixture.git_success(
        &source,
        &[
            "format-patch",
            "--binary",
            "-o",
            out_dir.to_str().expect("utf8 out dir"),
            "HEAD~4..HEAD",
        ],
    );
    let patches = sorted_files(&out_dir);
    assert_eq!(patches.len(), 4, "four exported mails");

    let target = fixture.init_libra("libra-ext-target");
    fixture.libra_commit(&target, "base\n", "base");
    let mut args: Vec<&str> = vec!["am"];
    let paths: Vec<String> = patches
        .iter()
        .map(|path| path.to_str().expect("utf8 patch").to_string())
        .collect();
    args.extend(paths.iter().map(String::as_str));
    fixture.libra_success(&target, &args);

    assert_eq!(
        fs::read(target.join("blob.bin")).expect("binary applied"),
        payload
    );
    assert!(!target.join("file.txt").exists(), "rename removed source");
    assert_eq!(
        fs::read_to_string(target.join("renamed.txt")).expect("rename dest"),
        "base\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(target.join("run.sh"))
            .expect("script stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "mode-only patch applied: {mode:o}");
    }
}

/// PD-09 ④ gate: real `git format-patch --attach` multipart output
/// applies with `libra am`. Skips when git is not installed.
#[test]
fn git_format_patch_attach_applies_with_libra_am() {
    if Command::new("git")
        .arg("--version")
        .output()
        .map(|out| !out.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipped (install git to run the --attach round-trip gate)");
        return;
    }
    let fixture = Fixture::new();
    let source = fixture.init_git("git-attach-source");
    fixture.git_commit(&source, "base\n", "base");
    fixture.git_commit(&source, "base\nattached from git\n", "git attach change");
    let patch = fixture.git_success(&source, &["format-patch", "-1", "--attach", "--stdout"]);
    let patch_path = fixture.root.join("git-attach.patch");
    fs::write(&patch_path, &patch.stdout).expect("write git attach mail");

    let target = fixture.init_libra("libra-attach-target");
    fixture.libra_commit(&target, "base\n", "base");
    fixture.libra_success(
        &target,
        &["am", patch_path.to_str().expect("utf8 mail path")],
    );
    assert_eq!(
        fs::read_to_string(target.join("file.txt")).expect("result"),
        "base\nattached from git\n"
    );
    assert!(
        String::from_utf8_lossy(&fixture.libra_success(&target, &["log", "-1"]).stdout)
            .contains("git attach change")
    );
}
