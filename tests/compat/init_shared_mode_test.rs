//! `init --shared` safety and config persistence contracts for plan-20260708 P0-10.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
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

    fn success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.run(cwd, args);
        assert_success(args, &output);
        output
    }

    fn init_with_shared(&self, mode: &str) {
        self.success(
            &self.root,
            &[
                "init",
                "--vault",
                "false",
                "--shared",
                mode,
                self.repo.to_str().expect("utf8 repo"),
            ],
        );
    }

    fn shared_repository_config(&self) -> String {
        stdout_trim(&self.success(&self.repo, &["config", "get", "core.sharedRepository"]))
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

#[test]
fn init_rejects_non_traversable_numeric_shared_mode_without_partial_repo() {
    let fixture = CliFixture::new();
    fs::create_dir_all(&fixture.repo).expect("create target dir");

    let output = fixture.run(
        &fixture.root,
        &[
            "init",
            "--vault",
            "false",
            "--shared",
            "0660",
            fixture.repo.to_str().expect("utf8 repo"),
        ],
    );

    assert!(
        !output.status.success(),
        "init --shared=0660 unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LBR-CLI-002"),
        "expected invalid-arguments code, got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("directories traversable"),
        "diagnostic should name the directory traversal contract:\n{stderr}"
    );
    assert!(
        !fixture.repo.join(".libra").exists(),
        "invalid numeric --shared must not leave a partial .libra directory"
    );
}

#[test]
fn init_persists_directory_safe_numeric_shared_mode() {
    let fixture = CliFixture::new();
    fixture.init_with_shared("0770");

    assert_eq!(fixture.shared_repository_config(), "0770");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(fixture.repo.join(".libra"))
            .expect("stat .libra")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o770, "numeric shared mode should apply to .libra");
    }
}

#[test]
fn init_persists_group_and_all_shared_modes() {
    let group = CliFixture::new();
    group.init_with_shared("group");
    assert_eq!(group.shared_repository_config(), "group");

    let all = CliFixture::new();
    all.init_with_shared("all");
    assert_eq!(all.shared_repository_config(), "all");
}

#[test]
fn reinit_updates_shared_repository_config() {
    let fixture = CliFixture::new();
    fixture.success(
        &fixture.root,
        &[
            "init",
            "--vault",
            "false",
            fixture.repo.to_str().expect("utf8 repo"),
        ],
    );

    fixture.success(
        &fixture.root,
        &[
            "init",
            "--vault",
            "false",
            "--shared",
            "group",
            fixture.repo.to_str().expect("utf8 repo"),
        ],
    );

    assert_eq!(fixture.shared_repository_config(), "group");
}
