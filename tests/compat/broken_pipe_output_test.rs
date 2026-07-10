//! Stdout broken-pipe contracts for plan-20260708 P0-06.

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
            .env("LIBRA_PAGER", "never")
            .env("LIBRA_TEST", "1")
            .env("RUST_BACKTRACE", "1")
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

    fn run_with_closed_stdout(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut child = self
            .command(cwd, args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("spawn libra {}: {error}", args.join(" ")));
        drop(child.stdout.take());
        child
            .wait_with_output()
            .unwrap_or_else(|error| panic!("wait for libra {}: {error}", args.join(" ")))
    }

    fn init_repo(&self) {
        fs::create_dir_all(&self.repo).expect("create repo dir");
        self.success(
            &self.root,
            &["init", self.repo.to_str().expect("utf8 repo")],
        );
        self.success(&self.repo, &["config", "set", "user.name", "Pipe Test"]);
        self.success(
            &self.repo,
            &["config", "set", "user.email", "pipe@example.com"],
        );
    }

    fn commit_base(&self) {
        fs::write(self.repo.join("tracked.txt"), "needle\nbase\n").expect("write tracked file");
        self.success(&self.repo, &["add", "tracked.txt"]);
        self.success(&self.repo, &["commit", "-s", "-m", "base"]);
        fs::write(self.repo.join("tracked.txt"), "needle\nchanged\n").expect("modify file");
    }
}

fn assert_stdout_broken_pipe_is_quiet(fixture: &CliFixture, args: &[&str]) {
    let output = fixture.run_with_closed_stdout(&fixture.repo, args);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{} should treat stdout BrokenPipe as a normal early-closed pipeline\nstatus: {:?}\nstderr:\n{}",
        args.join(" "),
        output.status,
        stderr
    );
    let lower = stderr.to_ascii_lowercase();
    assert!(
        !lower.contains("panicked")
            && !lower.contains("backtrace")
            && !lower.contains("broken pipe")
            && !lower.contains("cli thread panicked"),
        "{} should not print panic/backtrace/BrokenPipe noise\nstderr:\n{}",
        args.join(" "),
        stderr
    );
}

#[test]
fn large_stdout_commands_exit_quietly_when_downstream_closes() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.commit_base();

    let huge_format = format!("{}%(refname)", "x".repeat(64 * 1024));
    let cases: Vec<Vec<String>> = vec![
        vec!["log".into(), "--oneline".into()],
        vec!["diff".into()],
        vec!["grep".into(), "needle".into()],
        vec!["ls-files".into()],
        vec!["show".into(), "--stat".into(), "HEAD".into()],
        vec!["for-each-ref".into(), format!("--format={huge_format}")],
        vec!["cat-file".into(), "-p".into(), "HEAD".into()],
        vec!["--json".into(), "status".into()],
    ];

    for case in cases {
        let args = case.iter().map(String::as_str).collect::<Vec<_>>();
        assert_stdout_broken_pipe_is_quiet(&fixture, &args);
    }
}
