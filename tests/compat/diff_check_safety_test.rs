//! `diff --check` compatibility contracts for plan-20260708 P0-02.

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use tempfile::{TempDir, tempdir};

struct TestRepo {
    _temp: TempDir,
    repo: PathBuf,
}

impl TestRepo {
    fn new() -> Self {
        let temp = tempdir().expect("create tempdir");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).expect("create repo dir");
        let repo = Self { _temp: temp, repo };
        repo.success(&["init"]);
        repo.success(&["config", "user.name", "Diff Check Test"]);
        repo.success(&["config", "user.email", "diff-check@example.com"]);
        repo.write("trailing.txt", "base\n");
        repo.write("blank.txt", "base\n");
        repo.write("middle.txt", "top\nbottom\nend\n");
        repo.success(&[
            "add",
            ".libraignore",
            "trailing.txt",
            "blank.txt",
            "middle.txt",
        ]);
        repo.success(&["commit", "-s", "-m", "base"]);
        repo
    }

    fn command(&self, args: &[&str]) -> Command {
        let home = self.repo.join(".libra-test-home");
        let config_home = home.join(".config");
        let global_db = home.join(".libra").join("config.db");
        fs::create_dir_all(&config_home).expect("create isolated config dir");

        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &home)
            .env("USERPROFILE", &home)
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

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("spawn libra")
    }

    fn success(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn write(&self, path: &str, contents: &str) {
        fs::write(self.repo.join(path), contents).expect("write file");
    }
}

#[test]
fn diff_check_reports_all_git_safety_classes() {
    let repo = TestRepo::new();
    repo.write("trailing.txt", "base\ntrailing   \n");
    repo.write("blank.txt", "base\n\n");
    repo.write(
        "markers.txt",
        "before\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> side\n",
    );
    repo.success(&["add", "trailing.txt", "blank.txt", "markers.txt"]);

    let output = repo.run(&["diff", "--cached", "--check"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "diff --check must exit 2 when safety problems are found\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("trailing.txt:2: trailing whitespace"),
        "missing trailing whitespace report:\n{stdout}"
    );
    assert!(
        stdout.contains("blank.txt:2: new blank line at EOF."),
        "missing blank-at-eof report:\n{stdout}"
    );
    for line in [2, 4, 6] {
        assert!(
            stdout.contains(&format!("markers.txt:{line}: leftover conflict marker")),
            "missing leftover conflict marker report for line {line}:\n{stdout}"
        );
    }
}

#[test]
fn diff_check_exits_zero_for_clean_staged_diff() {
    let repo = TestRepo::new();
    repo.write("trailing.txt", "base\ntidy\n");
    repo.write("middle.txt", "top\n\nbottom\nend\n");
    repo.success(&["add", "trailing.txt", "middle.txt"]);

    let output = repo.run(&["diff", "--cached", "--check"]);
    assert!(
        output.status.success(),
        "clean diff --check must exit 0\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "clean diff --check must print nothing, got:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
