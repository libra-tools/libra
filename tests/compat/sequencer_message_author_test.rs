//! Sequencer author/message contracts for plan-20260708 P0-08.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use tempfile::{TempDir, tempdir};

struct CliFixture {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
    repo: PathBuf,
}

impl CliFixture {
    fn new() -> Self {
        let temp = tempdir().expect("create tempdir");
        let root = temp.path().to_path_buf();
        let home = root.join("home");
        let repo = root.join("repo");
        fs::create_dir_all(&home).expect("create isolated home");
        Self {
            _temp: temp,
            root,
            home,
            repo,
        }
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let config_home = self.home.join(".config");
        let global_db = self.home.join(".libra").join("config.db");
        fs::create_dir_all(&config_home).expect("create isolated config dir");

        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("LIBRA_CONFIG_GLOBAL_DB", &global_db)
            .env("LIBRA_TEST", "1")
            .env("LANG", "C")
            .env("LC_ALL", "C");
        if let Some(profile_file) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile_file);
        }
        command
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd, args).output().expect("spawn libra")
    }

    fn run_env(&self, cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut command = self.command(cwd, args);
        for (key, value) in envs {
            command.env(key, value);
        }
        command.output().expect("spawn libra with env")
    }

    fn run_with_stdin(&self, cwd: &Path, args: &[&str], stdin: &str) -> Output {
        let mut child = self
            .command(cwd, args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("spawn libra {}: {error}", args.join(" ")));
        if let Some(mut child_stdin) = child.stdin.take() {
            use std::io::Write;
            child_stdin
                .write_all(stdin.as_bytes())
                .expect("write stdin to libra");
        }
        child
            .wait_with_output()
            .unwrap_or_else(|error| panic!("wait for libra {} with stdin: {error}", args.join(" ")))
    }

    fn success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.run(cwd, args);
        assert_success(args, &output);
        output
    }

    fn success_env(&self, cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let output = self.run_env(cwd, args, envs);
        assert_success(args, &output);
        output
    }

    fn success_with_stdin(&self, cwd: &Path, args: &[&str], stdin: &str) -> Output {
        let output = self.run_with_stdin(cwd, args, stdin);
        assert_success(args, &output);
        output
    }

    fn init_repo(&self) {
        fs::create_dir_all(&self.repo).expect("create repo dir");
        self.success(
            &self.root,
            &[
                "init",
                "--vault",
                "false",
                self.repo.to_str().expect("utf8 repo"),
            ],
        );
        self.success(&self.repo, &["config", "set", "user.name", "Current User"]);
        self.success(
            &self.repo,
            &["config", "set", "user.email", "current@example.com"],
        );
    }

    fn rev_parse(&self, spec: &str) -> String {
        stdout_trim(&self.success(&self.repo, &["rev-parse", spec]))
    }

    fn cat_file(&self, spec: &str) -> String {
        String::from_utf8(self.success(&self.repo, &["cat-file", "-p", spec]).stdout)
            .expect("cat-file output is utf8")
    }
}

fn assert_success(args: &[&str], output: &Output) {
    assert!(
        output.status.success(),
        "{} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout_trim(output: &Output) -> String {
    String::from_utf8(output.stdout.clone())
        .expect("stdout is utf8")
        .trim()
        .to_string()
}

fn first_line_with<'a>(text: &'a str, prefix: &str) -> &'a str {
    text.lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("missing {prefix:?} line in commit:\n{text}"))
}

fn message_body(raw_commit: &str) -> &str {
    raw_commit
        .split_once("\n\n")
        .map(|(_, body)| body)
        .unwrap_or("")
}

#[test]
fn cherry_pick_preserves_original_author_and_uses_current_committer() {
    let fixture = CliFixture::new();
    fixture.init_repo();

    fs::write(fixture.repo.join("base.txt"), "base\n").expect("write base");
    fixture.success(&fixture.repo, &["add", "base.txt"]);
    fixture.success(
        &fixture.repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "base"],
    );

    fixture.success(&fixture.repo, &["switch", "-c", "feature"]);
    fs::write(fixture.repo.join("feature.txt"), "feature\n").expect("write feature");
    fixture.success(&fixture.repo, &["add", "feature.txt"]);
    fixture.success(
        &fixture.repo,
        &[
            "commit",
            "--no-gpg-sign",
            "--no-verify",
            "--author",
            "Picked Author <picked@example.com>",
            "--date",
            "1700000100 +0200",
            "-m",
            "picked subject",
        ],
    );
    let picked = fixture.rev_parse("HEAD");

    fixture.success(&fixture.repo, &["switch", "main"]);
    fixture.success_env(
        &fixture.repo,
        &["cherry-pick", &picked],
        &[
            ("GIT_COMMITTER_NAME", "Cherry Committer"),
            ("GIT_COMMITTER_EMAIL", "cherry@example.com"),
            ("GIT_COMMITTER_DATE", "1700000200 +0000"),
        ],
    );

    let raw = fixture.cat_file("HEAD");
    assert_eq!(
        first_line_with(&raw, "author "),
        "author Picked Author <picked@example.com> 1700000100 +0200"
    );
    assert_eq!(
        first_line_with(&raw, "committer "),
        "committer Cherry Committer <cherry@example.com> 1700000200 +0000"
    );
    assert_eq!(message_body(&raw).trim(), "picked subject");
}

#[test]
fn revert_uses_current_identity_and_strips_signed_subject() {
    let fixture = CliFixture::new();
    fixture.init_repo();

    fs::write(fixture.repo.join("base.txt"), "base\n").expect("write base");
    fixture.success(&fixture.repo, &["add", "base.txt"]);
    fixture.success(
        &fixture.repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "base"],
    );

    fs::write(fixture.repo.join("signed.txt"), "signed\n").expect("write signed");
    fixture.success(&fixture.repo, &["add", "signed.txt"]);
    fixture.success(
        &fixture.repo,
        &[
            "commit",
            "--no-gpg-sign",
            "--no-verify",
            "-m",
            "signed subject",
        ],
    );
    let raw_unsigned = fixture.cat_file("HEAD");
    let headers = raw_unsigned
        .split_once("\n\n")
        .map(|(headers, _)| headers)
        .expect("commit object has headers and body");
    let signed_commit = format!(
        "{headers}\ngpgsig -----BEGIN PGP SIGNATURE-----\n iQfixture\n -----END PGP SIGNATURE-----\n\nsigned subject\n\nsigned body\n"
    );
    let signed_oid = stdout_trim(&fixture.success_with_stdin(
        &fixture.repo,
        &["hash-object", "-w", "-t", "commit", "--stdin"],
        &signed_commit,
    ));
    fixture.success(
        &fixture.repo,
        &["update-ref", "refs/heads/main", &signed_oid],
    );
    assert_eq!(fixture.rev_parse("HEAD"), signed_oid);

    fixture.success_env(
        &fixture.repo,
        &["revert", "HEAD"],
        &[
            ("GIT_AUTHOR_NAME", "Revert Author"),
            ("GIT_AUTHOR_EMAIL", "revert-author@example.com"),
            ("GIT_AUTHOR_DATE", "1700000300 +0000"),
            ("GIT_COMMITTER_NAME", "Revert Committer"),
            ("GIT_COMMITTER_EMAIL", "revert-committer@example.com"),
            ("GIT_COMMITTER_DATE", "1700000400 +0000"),
        ],
    );

    let raw = fixture.cat_file("HEAD");
    assert_eq!(
        first_line_with(&raw, "author "),
        "author Revert Author <revert-author@example.com> 1700000300 +0000"
    );
    assert_eq!(
        first_line_with(&raw, "committer "),
        "committer Revert Committer <revert-committer@example.com> 1700000400 +0000"
    );
    let body = message_body(&raw);
    assert!(
        body.starts_with("Revert \"signed subject\""),
        "revert should use the original subject, got:\n{body}"
    );
    assert!(
        !body.contains("gpgsig"),
        "revert subject/message must not be derived from a signature block:\n{body}"
    );
}
