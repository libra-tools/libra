//! Machine-readable porcelain compatibility guards for plan-20260708 P1-03.

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
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("create tempdir");
        let root = temp.path().to_path_buf();
        let home = root.join("home");
        let repo = root.join("repo");
        fs::create_dir_all(&home).expect("create isolated home");
        fs::create_dir_all(&repo).expect("create repo");
        let fixture = Self {
            _temp: temp,
            root,
            home,
            repo,
        };
        fixture.success(
            &fixture.root,
            &["init", "--vault", "false", repo_str(&fixture.repo)],
        );
        fixture.success(
            &fixture.repo,
            &["config", "set", "user.name", "Porcelain Test"],
        );
        fixture.success(
            &fixture.repo,
            &["config", "set", "user.email", "porcelain@example.com"],
        );
        fixture.write("tracked.txt", "base\n");
        fixture.success(&fixture.repo, &["add", "tracked.txt"]);
        fixture.success(
            &fixture.repo,
            &["commit", "--no-gpg-sign", "--no-verify", "-m", "base"],
        );
        fixture
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

    fn failure(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.run(cwd, args);
        assert!(
            !output.status.success(),
            "{} unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn stdout(&self, cwd: &Path, args: &[&str]) -> String {
        String::from_utf8(self.success(cwd, args).stdout).expect("stdout is utf8")
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.repo.join(path);
        fs::create_dir_all(path.parent().expect("file has parent")).expect("create parent");
        fs::write(path, contents).expect("write fixture file");
    }
}

fn repo_str(path: &Path) -> &str {
    path.to_str().expect("repo path is utf8")
}

fn status_code(output: &Output) -> i32 {
    output.status.code().expect("process exited normally")
}

#[test]
fn status_porcelain_z_records_are_nul_terminated_without_rename_arrow() {
    let fixture = Fixture::new();
    fs::rename(
        fixture.repo.join("tracked.txt"),
        fixture.repo.join("renamed.txt"),
    )
    .expect("rename tracked fixture");
    fixture.success(&fixture.repo, &["add", "."]);

    let v1 = fixture.success(
        &fixture.repo,
        &["status", "--porcelain=v1", "-z", "--renames"],
    );
    assert!(
        v1.stdout.ends_with(b"\0"),
        "porcelain v1 -z should terminate records with NUL: {:?}",
        v1.stdout
    );
    assert!(
        !v1.stdout.contains(&b'\n'),
        "porcelain v1 -z must not contain newlines: {:?}",
        v1.stdout
    );
    let v1_text = String::from_utf8_lossy(&v1.stdout);
    assert!(
        !v1_text.contains(" -> "),
        "porcelain v1 -z rename output must not use arrow syntax: {v1_text:?}"
    );
    let v1_records = v1
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    assert!(
        v1_records.len() >= 2,
        "porcelain v1 -z should expose path fields as NUL records: {:?}",
        v1.stdout
    );

    let v2 = fixture.success(
        &fixture.repo,
        &["status", "--porcelain=v2", "-z", "--renames"],
    );
    assert!(
        v2.stdout.ends_with(b"\0"),
        "porcelain v2 -z should terminate records with NUL: {:?}",
        v2.stdout
    );
    assert!(
        !v2.stdout.contains(&b'\n'),
        "porcelain v2 -z must not contain newlines: {:?}",
        v2.stdout
    );
}

#[test]
fn diff_porcelain_modes_ignore_untracked_by_default() {
    let fixture = Fixture::new();
    fixture.write(".libraignore", "*.ignored\n");
    fixture.write("untracked.txt", "untracked\n");

    let quiet_clean = fixture.success(&fixture.repo, &["diff", "--quiet"]);
    assert_eq!(
        status_code(&quiet_clean),
        0,
        "untracked files must not make diff --quiet dirty"
    );
    let exit_clean = fixture.success(&fixture.repo, &["diff", "--exit-code"]);
    assert_eq!(
        status_code(&exit_clean),
        0,
        "untracked files must not make diff --exit-code dirty"
    );
    assert_eq!(
        fixture.stdout(&fixture.repo, &["diff", "--name-status"]),
        "",
        "name-status should not list untracked files by default"
    );
    assert_eq!(
        fixture.stdout(&fixture.repo, &["diff", "--numstat"]),
        "",
        "numstat should not list untracked files by default"
    );
    assert_eq!(
        fixture.stdout(&fixture.repo, &["diff", "--shortstat"]),
        "",
        "shortstat should not count untracked files by default"
    );

    fixture.write("tracked.txt", "base\nchanged\n");
    fixture.write("another-untracked.txt", "still untracked\n");

    let quiet_dirty = fixture.failure(&fixture.repo, &["diff", "--quiet"]);
    assert_eq!(status_code(&quiet_dirty), 1);
    let exit_dirty = fixture.failure(&fixture.repo, &["diff", "--exit-code"]);
    assert_eq!(status_code(&exit_dirty), 1);

    let name_status = fixture.stdout(&fixture.repo, &["diff", "--name-status"]);
    assert_eq!(name_status, "M\ttracked.txt\n");

    let numstat = fixture.stdout(&fixture.repo, &["diff", "--numstat"]);
    assert_eq!(numstat, "1\t0\ttracked.txt\n");

    let shortstat = fixture.stdout(&fixture.repo, &["diff", "--shortstat"]);
    assert_eq!(shortstat.trim(), "1 file changed, 1 insertion(+)");
}

#[test]
fn ls_files_error_unmatch_exits_one() {
    let fixture = Fixture::new();

    let output = fixture.failure(
        &fixture.repo,
        &["ls-files", "--error-unmatch", "missing.txt"],
    );
    assert_eq!(status_code(&output), 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("missing.txt"),
        "diagnostic should name the unmatched pathspec: {stderr}"
    );
}

#[test]
fn grep_exit_codes_follow_git_contract() {
    let fixture = Fixture::new();

    let matched = fixture.success(&fixture.repo, &["grep", "base"]);
    assert_eq!(status_code(&matched), 0);

    let no_match = fixture.failure(&fixture.repo, &["grep", "definitely-not-present"]);
    assert_eq!(status_code(&no_match), 1);

    let bad_pattern = fixture.failure(&fixture.repo, &["grep", "["]);
    assert_eq!(status_code(&bad_pattern), 2);
}
