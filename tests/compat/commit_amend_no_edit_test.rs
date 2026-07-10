//! Clean amend contracts for plan-20260708 P0-07.

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
        assert!(
            output.status.success(),
            "{} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn success_with_stdin(&self, cwd: &Path, args: &[&str], stdin: &str) -> Output {
        let output = self.run_with_stdin(cwd, args, stdin);
        assert!(
            output.status.success(),
            "{} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
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
        self.success(&self.repo, &["config", "set", "user.name", "Amend Test"]);
        self.success(
            &self.repo,
            &["config", "set", "user.email", "amend@example.com"],
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

fn signature_timestamp(line: &str) -> u64 {
    let (without_zone, _zone) = line
        .rsplit_once(' ')
        .unwrap_or_else(|| panic!("signature line has no timezone: {line}"));
    let (_identity, timestamp) = without_zone
        .rsplit_once(' ')
        .unwrap_or_else(|| panic!("signature line has no timestamp: {line}"));
    timestamp
        .parse()
        .unwrap_or_else(|error| panic!("invalid timestamp in {line}: {error}"))
}

fn rewrite_signature_timestamp(line: &str, timestamp: u64) -> String {
    let (without_zone, zone) = line
        .rsplit_once(' ')
        .unwrap_or_else(|| panic!("signature line has no timezone: {line}"));
    let (identity, _old_timestamp) = without_zone
        .rsplit_once(' ')
        .unwrap_or_else(|| panic!("signature line has no timestamp: {line}"));
    format!("{identity} {timestamp} {zone}")
}

#[test]
fn clean_amend_no_edit_rewrites_head_and_refreshes_committer_date() {
    let fixture = CliFixture::new();
    fixture.init_repo();

    fs::write(fixture.repo.join("file.txt"), "base\n").expect("write base file");
    fixture.success(&fixture.repo, &["add", "file.txt"]);
    fixture.success(
        &fixture.repo,
        &[
            "commit",
            "--no-gpg-sign",
            "--no-verify",
            "-m",
            "base message",
        ],
    );

    let base_raw = fixture.cat_file("HEAD");
    let tree_line = first_line_with(&base_raw, "tree ");
    let tree = tree_line.strip_prefix("tree ").expect("tree prefix");
    let author = first_line_with(&base_raw, "author ");
    let committer = first_line_with(&base_raw, "committer ");
    let future_timestamp = signature_timestamp(committer) + 3_600;
    let future_author = rewrite_signature_timestamp(author, future_timestamp);
    let future_committer = rewrite_signature_timestamp(committer, future_timestamp);
    let future_commit =
        format!("{tree_line}\n{future_author}\n{future_committer}\n\nbase message\n");

    let parent_output = fixture.success_with_stdin(
        &fixture.repo,
        &["hash-object", "-w", "-t", "commit", "--stdin"],
        &future_commit,
    );
    let parent_oid = stdout_trim(&parent_output);
    fixture.success(
        &fixture.repo,
        &["update-ref", "refs/heads/main", &parent_oid],
    );
    assert_eq!(fixture.rev_parse("HEAD"), parent_oid);

    let amend = fixture.success(
        &fixture.repo,
        &[
            "commit",
            "--amend",
            "--no-edit",
            "--no-gpg-sign",
            "--no-verify",
        ],
    );
    assert!(
        String::from_utf8_lossy(&amend.stdout).contains("[main "),
        "amend should print a real commit summary for the rewritten HEAD\nstdout:\n{}",
        String::from_utf8_lossy(&amend.stdout)
    );

    let amended_oid = fixture.rev_parse("HEAD");
    assert_ne!(
        amended_oid, parent_oid,
        "clean commit --amend --no-edit must rewrite HEAD, not report success for an unchanged ref"
    );

    let amended_raw = fixture.cat_file("HEAD");
    let amended_tree = first_line_with(&amended_raw, "tree ")
        .strip_prefix("tree ")
        .expect("tree prefix");
    assert_eq!(
        amended_tree, tree,
        "clean amend should preserve the unchanged tree"
    );
    assert_eq!(
        stdout_trim(&fixture.success(&fixture.repo, &["log", "--pretty=%P", "-1"])),
        "",
        "amending a root commit should keep the replacement commit root"
    );
    assert_eq!(
        stdout_trim(&fixture.success(&fixture.repo, &["log", "-1", "--format=%s"])),
        "base message",
        "--no-edit should preserve the original subject"
    );

    let amended_committer = first_line_with(&amended_raw, "committer ");
    assert!(
        signature_timestamp(amended_committer) > future_timestamp,
        "clean amend should refresh the committer date beyond the replaced commit\nparent: {future_committer}\namended: {amended_committer}"
    );
}
