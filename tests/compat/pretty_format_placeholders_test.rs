//! Pretty-format placeholder compatibility guards for plan-20260708 P1-04.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use tempfile::{TempDir, tempdir};

const PATH_ENV: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const MESSAGE: &str = "Subject line\n\nBody line 1\n\nBody line 2";
const AUTHOR_DATE: &str = "1600000000 +0200";
const COMMITTER_DATE: &str = "1700000000 -0330";

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    git_home: PathBuf,
    libra_home: PathBuf,
    git_repo: PathBuf,
    libra_repo: PathBuf,
}

impl Fixture {
    fn new() -> Option<Self> {
        if !git_available() {
            eprintln!("skipping pretty-format Git parity test: git binary not found");
            return None;
        }

        let temp = tempdir().expect("create tempdir");
        let root = temp.path().to_path_buf();
        let git_home = root.join("git-home");
        let libra_home = root.join("libra-home");
        let git_repo = root.join("git-repo");
        let libra_repo = root.join("libra-repo");
        fs::create_dir_all(&git_home).expect("create git home");
        fs::create_dir_all(&libra_home).expect("create libra home");

        let fixture = Self {
            _temp: temp,
            root,
            git_home,
            libra_home,
            git_repo,
            libra_repo,
        };
        fixture.init_git_repo();
        fixture.init_libra_repo();
        Some(fixture)
    }

    fn init_git_repo(&self) {
        self.git_success(
            &self.root,
            &[
                "-c",
                "init.defaultBranch=main",
                "init",
                "-q",
                path_str(&self.git_repo),
            ],
        );
        self.git_success(&self.git_repo, &["config", "user.name", "A"]);
        self.git_success(&self.git_repo, &["config", "user.email", "a@example.com"]);
        self.git_success(&self.git_repo, &["config", "commit.gpgSign", "false"]);
        self.git_success(&self.git_repo, &["config", "tag.gpgSign", "false"]);
        fs::write(self.git_repo.join("a"), "a\n").expect("write git fixture");
        fs::write(self.git_repo.join("message.txt"), MESSAGE).expect("write git message");
        self.git_success(&self.git_repo, &["add", "a"]);
        self.git_success_with_dates(
            &self.git_repo,
            &["commit", "-q", "--no-gpg-sign", "-F", "message.txt"],
        );
        self.git_success(&self.git_repo, &["tag", "--no-sign", "v1"]);
    }

    fn init_libra_repo(&self) {
        fs::create_dir_all(&self.libra_repo).expect("create libra repo dir");
        self.libra_success(
            &self.root,
            &["init", "--vault", "false", path_str(&self.libra_repo)],
        );
        self.libra_success(&self.libra_repo, &["config", "set", "user.name", "A"]);
        self.libra_success(
            &self.libra_repo,
            &["config", "set", "user.email", "a@example.com"],
        );
        fs::write(self.libra_repo.join("a"), "a\n").expect("write libra fixture");
        self.libra_success(&self.libra_repo, &["add", "a"]);
        self.libra_success_with_dates(
            &self.libra_repo,
            &["commit", "--no-gpg-sign", "--no-verify", "-m", MESSAGE],
        );
        self.libra_success(&self.libra_repo, &["tag", "v1"]);
    }

    fn git_command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", PATH_ENV)
            .env("HOME", &self.git_home)
            .env("USERPROFILE", &self.git_home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("VISUAL", "true")
            .env("LANG", "C")
            .env("LC_ALL", "C");
        command
    }

    fn libra_command(&self, cwd: &Path, args: &[&str]) -> Command {
        let config_home = self.libra_home.join(".config");
        let global_db = self.libra_home.join(".libra").join("config.db");
        fs::create_dir_all(&config_home).expect("create isolated config dir");

        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", PATH_ENV)
            .env("HOME", &self.libra_home)
            .env("USERPROFILE", &self.libra_home)
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

    fn git_success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.git_command(cwd, args).output().expect("spawn git");
        assert_success("git", args, &output);
        output
    }

    fn git_success_with_dates(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut command = self.git_command(cwd, args);
        command
            .env("GIT_AUTHOR_DATE", AUTHOR_DATE)
            .env("GIT_COMMITTER_DATE", COMMITTER_DATE);
        let output = command.output().expect("spawn git");
        assert_success("git", args, &output);
        output
    }

    fn libra_success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.libra_command(cwd, args).output().expect("spawn libra");
        assert_success("libra", args, &output);
        output
    }

    fn libra_success_with_dates(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut command = self.libra_command(cwd, args);
        command
            .env("GIT_AUTHOR_DATE", AUTHOR_DATE)
            .env("GIT_COMMITTER_DATE", COMMITTER_DATE);
        let output = command.output().expect("spawn libra");
        assert_success("libra", args, &output);
        output
    }
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .env_clear()
        .env("PATH", PATH_ENV)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("fixture path is utf8")
}

fn assert_success(program: &str, args: &[&str], output: &Output) {
    assert!(
        output.status.success(),
        "{} {} failed\nstdout:\n{}\nstderr:\n{}",
        program,
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_bytes_eq(label: &str, libra: &[u8], git: &[u8]) {
    assert_eq!(
        libra,
        git,
        "{label}\nlibra bytes: {}\n git bytes: {}",
        hex_dump(libra),
        hex_dump(git)
    );
}

fn hex_dump(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn count_subslice(bytes: &[u8], needle: &[u8]) -> usize {
    bytes
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

#[test]
fn log_pretty_placeholders_match_git_bytes() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let format = "%s|%%|%b|%B|%n|%x09|%x00|%x7f|%aI|%cI|%at|%ct|%d|%D|%m|%q|%xGG";

    let git = fixture.git_success(
        &fixture.git_repo,
        &[
            "log",
            "-1",
            "--decorate=short",
            &format!("--format={format}"),
        ],
    );
    let libra = fixture.libra_success(
        &fixture.libra_repo,
        &[
            "log",
            "-1",
            "--decorate=short",
            &format!("--format={format}"),
        ],
    );

    assert_bytes_eq(
        "log custom pretty placeholders must match Git",
        &libra.stdout,
        &git.stdout,
    );
}

#[test]
fn log_name_output_separators_and_null_mode_match_git() {
    let Some(fixture) = Fixture::new() else {
        return;
    };

    let git_names = fixture.git_success(
        &fixture.git_repo,
        &["log", "-1", "--name-only", "--format=%s"],
    );
    let libra_names = fixture.libra_success(
        &fixture.libra_repo,
        &["log", "-1", "--name-only", "--format=%s"],
    );
    assert_bytes_eq(
        "log --name-only --format separator must match Git",
        &libra_names.stdout,
        &git_names.stdout,
    );

    let git_z = fixture.git_success(
        &fixture.git_repo,
        &["log", "-1", "-z", "--name-status", "--format=%s"],
    );
    let libra_z = fixture.libra_success(
        &fixture.libra_repo,
        &["log", "-1", "-z", "--name-status", "--format=%s"],
    );
    assert_bytes_eq(
        "log -z --name-status records must match Git",
        &libra_z.stdout,
        &git_z.stdout,
    );
}

#[test]
fn log_color_placeholders_match_git_default_and_forced_modes() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let format = "%CredRED%Creset|%C(red)RED%Creset|%C(always,red)RED%Creset";

    let git_default = fixture.git_success(
        &fixture.git_repo,
        &["log", "-1", &format!("--format={format}")],
    );
    let libra_default = fixture.libra_success(
        &fixture.libra_repo,
        &["log", "-1", &format!("--format={format}")],
    );
    assert_bytes_eq(
        "default color placeholders must match Git",
        &libra_default.stdout,
        &git_default.stdout,
    );

    let git_forced = fixture.git_success(
        &fixture.git_repo,
        &["log", "--color=always", "-1", &format!("--format={format}")],
    );
    let libra_forced = fixture.libra_success(
        &fixture.libra_repo,
        &["--color=always", "log", "-1", &format!("--format={format}")],
    );
    assert_bytes_eq(
        "forced color placeholders must match Git",
        &libra_forced.stdout,
        &git_forced.stdout,
    );

    let git_show_forced = fixture.git_success(
        &fixture.git_repo,
        &[
            "show",
            "--color=always",
            "-s",
            &format!("--format={format}"),
        ],
    );
    let libra_show_forced = fixture.libra_success(
        &fixture.libra_repo,
        &[
            "--color=always",
            "show",
            "-s",
            &format!("--format={format}"),
            "HEAD",
        ],
    );
    assert_bytes_eq(
        "show forced color placeholders must match Git",
        &libra_show_forced.stdout,
        &git_show_forced.stdout,
    );

    let shortlog_default = fixture.libra_success(
        &fixture.libra_repo,
        &["shortlog", &format!("--format={format}")],
    );
    assert_eq!(
        count_subslice(&shortlog_default.stdout, b"\x1b[31m"),
        1,
        "shortlog default color should only honor %C(always,...): {}",
        hex_dump(&shortlog_default.stdout)
    );
    assert_eq!(
        count_subslice(&shortlog_default.stdout, b"\x1b[m"),
        0,
        "shortlog default color should match Git: %Creset is policy-gated, use %C(always,reset) to force a reset: {}",
        hex_dump(&shortlog_default.stdout)
    );

    let shortlog_forced = fixture.libra_success(
        &fixture.libra_repo,
        &["--color=always", "shortlog", &format!("--format={format}")],
    );
    assert_eq!(
        count_subslice(&shortlog_forced.stdout, b"\x1b[31m"),
        3,
        "shortlog forced color should emit ANSI red for every color placeholder: {}",
        hex_dump(&shortlog_forced.stdout)
    );
    assert_eq!(
        count_subslice(&shortlog_forced.stdout, b"\x1b[m"),
        3,
        "shortlog forced color should emit ANSI reset for every %Creset: {}",
        hex_dump(&shortlog_forced.stdout)
    );
}

#[test]
fn show_and_shortlog_reuse_pretty_placeholder_renderer() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let format = "%b%x09%aI";

    let git_show = fixture.git_success(
        &fixture.git_repo,
        &["show", "-s", &format!("--format={format}"), "HEAD"],
    );
    let libra_show = fixture.libra_success(
        &fixture.libra_repo,
        &["show", "-s", &format!("--format={format}"), "HEAD"],
    );
    assert_bytes_eq(
        "show --format must reuse Git-like pretty placeholders",
        &libra_show.stdout,
        &git_show.stdout,
    );

    let shortlog = fixture.libra_success(
        &fixture.libra_repo,
        &["shortlog", &format!("--format={format}")],
    );
    let shortlog_text = String::from_utf8_lossy(&shortlog.stdout);
    assert!(
        shortlog_text.contains("Body line 1"),
        "shortlog should render %b via the shared formatter: {shortlog_text}"
    );
    assert!(
        shortlog_text.contains('\t') && shortlog_text.contains("2020-09-13T14:26:40+02:00"),
        "shortlog should render %x09 and %aI via the shared formatter: {shortlog_text}"
    );
    assert!(
        !shortlog_text.contains("%b") && !shortlog_text.contains("%x09"),
        "shortlog must not leak supported placeholders literally: {shortlog_text}"
    );
}
