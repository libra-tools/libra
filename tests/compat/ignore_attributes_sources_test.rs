//! Git/Libra ignore and attributes source compatibility guards for P1-02.

use std::{
    fs,
    io::Cursor,
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
        fixture.success(&fixture.repo, &["config", "set", "user.name", "P1 Test"]);
        fixture.success(
            &fixture.repo,
            &["config", "set", "user.email", "p1@example.com"],
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

    fn stdout(&self, cwd: &Path, args: &[&str]) -> String {
        String::from_utf8(self.success(cwd, args).stdout).expect("stdout is utf8")
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.repo.join(path);
        fs::create_dir_all(path.parent().expect("file has parent")).expect("create parent");
        fs::write(path, contents).expect("write fixture file");
    }

    fn commit_all(&self, message: &str) {
        self.success(&self.repo, &["add", "-A"]);
        self.success(
            &self.repo,
            &["commit", "--no-gpg-sign", "--no-verify", "-m", message],
        );
    }
}

fn repo_str(path: &Path) -> &str {
    path.to_str().expect("repo path is utf8")
}

#[test]
fn gitignore_sources_feed_status_add_clean_and_check_ignore() {
    let fixture = Fixture::new();
    fixture.write(".gitignore", "*.log\n");
    fixture.write("ignored.log", "ignored\n");
    fixture.write("visible.txt", "visible\n");

    let status = fixture.stdout(&fixture.repo, &["status", "--short"]);
    assert!(
        status.contains("?? visible.txt"),
        "visible file should be untracked:\n{status}"
    );
    assert!(
        !status.contains("ignored.log"),
        ".gitignore should hide ignored.log from default status:\n{status}"
    );

    let check = fixture.stdout(&fixture.repo, &["check-ignore", "-v", "ignored.log"]);
    assert_eq!(check, ".gitignore:1:*.log\tignored.log\n");

    fixture.success(&fixture.repo, &["add", "."]);
    let tracked = fixture.stdout(&fixture.repo, &["ls-files"]);
    assert!(
        tracked.contains(".gitignore\n") && tracked.contains("visible.txt\n"),
        "add . should stage visible files:\n{tracked}"
    );
    assert!(
        !tracked.contains("ignored.log"),
        "add . must not stage .gitignore-matched files:\n{tracked}"
    );

    let clean_default = fixture.stdout(&fixture.repo, &["clean", "-n"]);
    assert!(
        !clean_default.contains("ignored.log"),
        "plain clean must respect .gitignore:\n{clean_default}"
    );
    let clean_ignored = fixture.stdout(&fixture.repo, &["clean", "-nX"]);
    assert!(
        clean_ignored.contains("Would remove ignored.log"),
        "clean -X should target ignored files:\n{clean_ignored}"
    );
}

#[test]
fn git_info_exclude_and_core_excludesfile_are_standard_ignore_sources() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.repo.join(".git/info")).expect("create .git/info");
    fs::write(fixture.repo.join(".git/info/exclude"), "info.tmp\n").expect("write info exclude");
    fixture.write("info.tmp", "ignored by info\n");

    let info = fixture.stdout(&fixture.repo, &["check-ignore", "-v", "info.tmp"]);
    assert_eq!(info, ".git/info/exclude:1:info.tmp\tinfo.tmp\n");

    let global_ignore = fixture.root.join("global-ignore");
    fs::write(&global_ignore, "global.tmp\n").expect("write global ignore");
    fixture.success(
        &fixture.repo,
        &[
            "config",
            "set",
            "core.excludesFile",
            repo_str(&global_ignore),
        ],
    );
    fixture.write("global.tmp", "ignored by config\n");

    let global = fixture.stdout(&fixture.repo, &["check-ignore", "-v", "global.tmp"]);
    assert!(
        global.ends_with(":1:global.tmp\tglobal.tmp\n"),
        "core.excludesFile should be the verbose source:\n{global}"
    );
}

#[test]
fn libraignore_can_override_gitignore_in_the_same_directory() {
    let fixture = Fixture::new();
    fixture.write(".gitignore", "*.tmp\n");
    fixture.write(".libraignore", "!keep.tmp\n");
    fixture.write("keep.tmp", "must stay visible\n");

    let output = fixture.run(&fixture.repo, &["check-ignore", "keep.tmp"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        ".libraignore whitelist should override sibling .gitignore ignore"
    );
}

#[test]
fn attributes_sources_feed_check_attr_lfs_diff_and_archive() {
    let fixture = Fixture::new();
    fixture.write(
        ".gitattributes",
        "*.bin filter=lfs\n*.upper diff=upper\nsecret.txt export-ignore\n",
    );
    fixture.write("data.bin", "binary payload\n");
    fixture.write("word.upper", "alpha\n");
    fixture.write("secret.txt", "do not archive\n");
    fixture.write("public.txt", "archive me\n");
    fixture.success(
        &fixture.repo,
        &["config", "set", "diff.upper.textconv", "tr a-z A-Z <"],
    );
    fixture.commit_all("base with attributes");

    let attr = fixture.stdout(&fixture.repo, &["check-attr", "filter", "data.bin"]);
    assert_eq!(attr, "data.bin: filter: lfs\n");

    let lfs = fixture.stdout(&fixture.repo, &["lfs", "ls-files", "--name-only"]);
    assert!(
        lfs.lines().any(|line| line == "data.bin"),
        "LFS listing should use .gitattributes filter=lfs:\n{lfs}"
    );

    fixture.write("word.upper", "bravo\n");
    let diff = fixture.stdout(&fixture.repo, &["diff", "--", "word.upper"]);
    assert!(
        diff.contains("-ALPHA") && diff.contains("+BRAVO"),
        "diff --textconv should use .gitattributes diff driver:\n{diff}"
    );

    fixture.write(".gitattributes", "*.bin filter=lfs\n*.upper diff=upper\n");
    let archive_path = fixture.repo.join("out.tar");
    fixture.success(
        &fixture.repo,
        &["archive", "-o", repo_str(&archive_path), "HEAD"],
    );
    let archive_bytes = fs::read(&archive_path).expect("read archive");
    let mut archive = tar::Archive::new(Cursor::new(archive_bytes));
    let names = archive
        .entries()
        .expect("tar entries")
        .map(|entry| {
            entry
                .expect("tar entry")
                .path()
                .expect("entry path")
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    assert!(
        names.iter().any(|name| name == "public.txt"),
        "archive should keep public file: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "secret.txt"),
        "archive should honor export-ignore: {names:?}"
    );
}

#[test]
fn git_info_attributes_and_core_attributesfile_are_standard_attribute_sources() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.repo.join(".git/info")).expect("create .git/info");
    fixture.write(".gitattributes", "info.dat filter=from_tree\n");
    fs::write(
        fixture.repo.join(".git/info/attributes"),
        "info.dat filter=lfs\n",
    )
    .expect("write info attributes");

    let info = fixture.stdout(&fixture.repo, &["check-attr", "filter", "info.dat"]);
    assert_eq!(info, "info.dat: filter: lfs\n");

    let global_attrs = fixture.root.join("global-attributes");
    fs::write(&global_attrs, "core.dat filter=lfs\n").expect("write global attributes");
    fixture.success(
        &fixture.repo,
        &[
            "config",
            "set",
            "core.attributesFile",
            repo_str(&global_attrs),
        ],
    );

    let core = fixture.stdout(&fixture.repo, &["check-attr", "filter", "core.dat"]);
    assert_eq!(core, "core.dat: filter: lfs\n");

    fixture.write(".gitattributes", "priority.dat filter=from_tree\n");
    fixture.write(".libra_attributes", "priority.dat filter=from_libra\n");
    fs::write(
        fixture.repo.join(".git/info/attributes"),
        "info.dat filter=lfs\npriority.dat filter=from_info\n",
    )
    .expect("rewrite info attributes");
    let priority = fixture.stdout(&fixture.repo, &["check-attr", "filter", "priority.dat"]);
    assert_eq!(
        priority, "priority.dat: filter: from_info\n",
        ".git/info/attributes should override worktree attributes sources"
    );

    fs::write(
        fixture.repo.join(".git/info/attributes"),
        "info.dat filter=lfs\n",
    )
    .expect("remove priority rule from info attributes");
    let libra = fixture.stdout(&fixture.repo, &["check-attr", "filter", "priority.dat"]);
    assert_eq!(
        libra, "priority.dat: filter: from_libra\n",
        ".libra_attributes should override a sibling .gitattributes rule"
    );
}
