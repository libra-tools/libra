//! Shared pathspec magic compatibility guards for plan-20260708 P1-01.

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
            &["config", "set", "user.name", "Pathspec Test"],
        );
        fixture.success(
            &fixture.repo,
            &["config", "set", "user.email", "pathspec@example.com"],
        );
        fixture.write("README.md", "root\n");
        fixture.write("src/main.rs", "NEEDLE main\n");
        fixture.write("src/generated.rs", "NEEDLE generated\n");
        fixture.write("src/Case.TXT", "NEEDLE case\n");
        fixture.write("docs/readme.md", "NEEDLE docs\n");
        fixture.write("literal/[abc].txt", "NEEDLE literal\n");
        fixture.write("literal/[abc]/child.txt", "NEEDLE literal child\n");
        fixture.success(&fixture.repo, &["add", "."]);
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

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.repo.join(path)).expect("read fixture file")
    }
}

fn repo_str(path: &Path) -> &str {
    path.to_str().expect("repo path is utf8")
}

#[test]
fn ls_files_honors_shared_pathspec_magic() {
    let fixture = Fixture::new();

    let glob_exclude = fixture.stdout(
        &fixture.repo,
        &["ls-files", ":(glob)src/*.rs", ":(exclude)src/generated.rs"],
    );
    assert_eq!(glob_exclude, "src/main.rs\n");

    let case = fixture.stdout(&fixture.repo, &["ls-files", ":(icase)src/case.txt"]);
    assert_eq!(case, "src/Case.TXT\n");

    let literal = fixture.stdout(&fixture.repo, &["ls-files", ":(literal)literal/[abc].txt"]);
    assert_eq!(literal, "literal/[abc].txt\n");

    let src_dir = fixture.repo.join("src");
    let top = fixture.stdout(&src_dir, &["ls-files", ":(top)README.md"]);
    assert_eq!(top, "README.md\n");

    let relative = fixture.stdout(&src_dir, &["ls-files", "*.rs"]);
    assert_eq!(relative, "src/generated.rs\nsrc/main.rs\n");
}

#[test]
fn grep_honors_shared_pathspec_magic() {
    let fixture = Fixture::new();

    let output = fixture.stdout(
        &fixture.repo,
        &[
            "grep",
            "-n",
            "NEEDLE",
            ":(glob)src/*.rs",
            ":(exclude)src/generated.rs",
        ],
    );
    assert!(
        output.contains("src/main.rs:1:NEEDLE main"),
        "grep output should include main.rs:\n{output}"
    );
    assert!(
        !output.contains("generated.rs"),
        "exclude pathspec should remove generated.rs:\n{output}"
    );

    let case = fixture.stdout(
        &fixture.repo,
        &["grep", "-n", "NEEDLE", ":(icase)src/case.txt"],
    );
    assert_eq!(case, "src/Case.TXT:1:NEEDLE case\n");

    let max_depth = fixture.stdout(
        &fixture.repo,
        &[
            "grep",
            "-n",
            "--max-depth",
            "0",
            "NEEDLE",
            ":(glob)src/*.rs",
            ":(exclude)src/generated.rs",
        ],
    );
    assert_eq!(max_depth, "src/main.rs:1:NEEDLE main\n");

    let icase_max_depth = fixture.stdout(
        &fixture.repo,
        &[
            "grep",
            "-n",
            "--max-depth",
            "0",
            "NEEDLE",
            ":(icase)src/case.txt",
        ],
    );
    assert_eq!(icase_max_depth, "src/Case.TXT:1:NEEDLE case\n");
}

#[test]
fn diff_and_status_honor_shared_pathspec_magic() {
    let fixture = Fixture::new();
    fixture.write("src/main.rs", "NEEDLE main\nchanged\n");
    fixture.write("src/generated.rs", "NEEDLE generated\nchanged\n");
    fixture.write("docs/readme.md", "NEEDLE docs\nchanged\n");

    let diff = fixture.stdout(
        &fixture.repo,
        &[
            "diff",
            "--",
            ":(glob)src/*.rs",
            ":(exclude)src/generated.rs",
        ],
    );
    assert!(
        diff.contains("diff --git a/src/main.rs b/src/main.rs"),
        "diff should include src/main.rs:\n{diff}"
    );
    assert!(
        !diff.contains("generated.rs") && !diff.contains("docs/readme.md"),
        "diff should apply exclude and positive filters:\n{diff}"
    );

    let status = fixture.stdout(
        &fixture.repo,
        &[
            "status",
            "--short",
            ":(glob)src/*.rs",
            ":(exclude)src/generated.rs",
        ],
    );
    assert_eq!(status, " M src/main.rs\n");

    let src_dir = fixture.repo.join("src");
    let relative_status = fixture.stdout(&src_dir, &["status", "--short", "*.rs"]);
    assert_eq!(
        relative_status, " M generated.rs\n M main.rs\n",
        "status pathspecs from a subdirectory should match repo-root paths and render cwd-relative entries"
    );
}

#[test]
fn diff_accepts_magic_pathspecs_without_dashdash() {
    let fixture = Fixture::new();
    fixture.write("README.md", "root\nchanged\n");
    fixture.write("src/Case.TXT", "NEEDLE case\nchanged\n");
    fixture.write("docs/readme.md", "NEEDLE docs\nchanged\n");
    fixture.write("literal/[abc].txt", "NEEDLE literal\nchanged\n");

    let top = fixture.stdout(&fixture.repo, &["diff", "--name-only", ":(top)README.md"]);
    assert_eq!(top, "README.md\n");

    let exclude = fixture.stdout(
        &fixture.repo,
        &["diff", "--name-only", ":(exclude)docs/readme.md"],
    );
    assert!(
        exclude.contains("README.md") && !exclude.contains("docs/readme.md"),
        "exclude magic should be parsed as a pathspec without --:\n{exclude}"
    );

    let case = fixture.stdout(
        &fixture.repo,
        &["diff", "--name-only", ":(icase)src/case.txt"],
    );
    assert_eq!(case, "src/Case.TXT\n");

    let literal = fixture.stdout(
        &fixture.repo,
        &["diff", "--name-only", ":(literal)literal/[abc].txt"],
    );
    assert_eq!(literal, "literal/[abc].txt\n");
}

#[test]
fn add_honors_shared_pathspec_magic() {
    let fixture = Fixture::new();
    fixture.write("src/main.rs", "NEEDLE main\nchanged\n");
    fixture.write("src/generated.rs", "NEEDLE generated\nchanged\n");
    fixture.write("docs/readme.md", "NEEDLE docs\nchanged\n");
    fixture.write("src/extra.rs", "NEEDLE extra\n");
    fixture.write("literal/[abc].txt", "NEEDLE literal\nchanged\n");
    fixture.write("literal/[abc]/child.txt", "NEEDLE literal child\nchanged\n");

    fixture.success(
        &fixture.repo,
        &[
            "add",
            ":(glob)src/*.rs",
            ":(exclude)src/generated.rs",
            ":(literal)literal/[abc].txt",
            "literal/[abc]",
        ],
    );

    let staged = fixture.stdout(&fixture.repo, &["diff", "--cached", "--name-only"]);
    assert!(
        staged.contains("src/main.rs\n"),
        "glob pathspec should stage src/main.rs:\n{staged}"
    );
    assert!(
        staged.contains("src/extra.rs\n"),
        "glob pathspec should stage new src/extra.rs:\n{staged}"
    );
    assert!(
        staged.contains("literal/[abc].txt\n"),
        "literal magic should stage the literal bracket path:\n{staged}"
    );
    assert!(
        staged.contains("literal/[abc]/child.txt\n"),
        "wildcard-looking pathspec should also match the literal bracket directory prefix:\n{staged}"
    );
    assert!(
        !staged.contains("src/generated.rs") && !staged.contains("docs/readme.md"),
        "exclude and positive pathspecs should restrict staged paths:\n{staged}"
    );

    fixture.write("src/Case.TXT", "NEEDLE case\nchanged\n");
    fixture.success(&fixture.repo, &["add", ":(icase)src/case.txt"]);
    let staged_case = fixture.stdout(&fixture.repo, &["diff", "--cached", "--name-only"]);
    assert!(
        staged_case.contains("src/Case.TXT\n"),
        "icase pathspec should stage the differently cased path:\n{staged_case}"
    );
}

#[test]
fn add_pathspec_from_file_honors_shared_magic() {
    let fixture = Fixture::new();
    fixture.write("src/main.rs", "NEEDLE main\nchanged\n");
    fixture.write("src/generated.rs", "NEEDLE generated\nchanged\n");
    fixture.write("docs/readme.md", "NEEDLE docs\nchanged\n");
    fixture.write("paths.txt", ":(glob)src/*.rs\n:(exclude)src/generated.rs\n");

    fixture.success(&fixture.repo, &["add", "--pathspec-from-file", "paths.txt"]);

    let staged = fixture.stdout(&fixture.repo, &["diff", "--cached", "--name-only"]);
    assert!(
        staged.contains("src/main.rs\n"),
        "pathspec-from-file glob should stage src/main.rs:\n{staged}"
    );
    assert!(
        !staged.contains("src/generated.rs") && !staged.contains("docs/readme.md"),
        "pathspec-from-file exclude should restrict staged paths:\n{staged}"
    );
}

#[test]
fn rm_honors_shared_pathspec_magic() {
    let fixture = Fixture::new();

    let dry_run = fixture.stdout(
        &fixture.repo,
        &[
            "rm",
            "--dry-run",
            "--cached",
            ":(glob)src/*.rs",
            ":(exclude)src/generated.rs",
        ],
    );
    assert!(
        dry_run.contains("rm 'src/main.rs'"),
        "glob pathspec should select src/main.rs:\n{dry_run}"
    );
    assert!(
        !dry_run.contains("src/generated.rs"),
        "exclude pathspec should remove generated.rs from rm candidates:\n{dry_run}"
    );

    fixture.success(
        &fixture.repo,
        &[
            "rm",
            "--cached",
            ":(glob)src/*.rs",
            ":(exclude)src/generated.rs",
        ],
    );
    let tracked = fixture.stdout(&fixture.repo, &["ls-files"]);
    assert!(
        !tracked.contains("src/main.rs\n"),
        "rm --cached should remove the matched path from the index:\n{tracked}"
    );
    assert!(
        tracked.contains("src/generated.rs\n"),
        "exclude pathspec should keep generated.rs tracked:\n{tracked}"
    );

    fixture.success(&fixture.repo, &["rm", "--cached", ":(icase)src/case.txt"]);
    let tracked_after_case = fixture.stdout(&fixture.repo, &["ls-files"]);
    assert!(
        !tracked_after_case.contains("src/Case.TXT\n"),
        "icase pathspec should remove the differently cased tracked path:\n{tracked_after_case}"
    );

    fixture.success(&fixture.repo, &["rm", "--cached", "literal/[abc]"]);
    let tracked_after_literal_dir = fixture.stdout(&fixture.repo, &["ls-files"]);
    assert!(
        !tracked_after_literal_dir.contains("literal/[abc]/child.txt\n"),
        "wildcard-looking pathspec should remove the literal bracket directory prefix:\n{tracked_after_literal_dir}"
    );
}

#[test]
fn rm_recursive_does_not_delete_excluded_paths() {
    let fixture = Fixture::new();

    fixture.success(
        &fixture.repo,
        &["rm", "-r", "src", ":(exclude)src/generated.rs"],
    );

    assert!(
        !fixture.repo.join("src/main.rs").exists(),
        "matched file should be deleted from disk"
    );
    assert!(
        fixture.repo.join("src/generated.rs").exists(),
        "exclude pathspec must prevent recursive directory deletion from removing generated.rs"
    );
    let tracked = fixture.stdout(&fixture.repo, &["ls-files"]);
    assert!(
        !tracked.contains("src/main.rs\n"),
        "matched file should be removed from the index:\n{tracked}"
    );
    assert!(
        tracked.contains("src/generated.rs\n"),
        "excluded file should remain tracked:\n{tracked}"
    );
}

#[test]
fn rm_recursive_preserves_untracked_files_in_matched_directory() {
    let fixture = Fixture::new();
    fixture.write("src/untracked.log", "local only\n");

    fixture.success(&fixture.repo, &["rm", "-r", "src"]);

    assert!(
        fixture.repo.join("src/untracked.log").exists(),
        "recursive rm should preserve untracked files under matched directories"
    );
    assert!(
        !fixture.repo.join("src/main.rs").exists(),
        "matched tracked file should be deleted from disk"
    );
    let tracked = fixture.stdout(&fixture.repo, &["ls-files"]);
    assert!(
        !tracked.contains("src/main.rs\n")
            && !tracked.contains("src/generated.rs\n")
            && !tracked.contains("src/Case.TXT\n"),
        "all tracked files under src should be removed from the index:\n{tracked}"
    );
}

#[test]
fn restore_honors_shared_pathspec_magic() {
    let fixture = Fixture::new();
    fixture.write("src/main.rs", "NEEDLE main\nchanged\n");
    fixture.write("src/generated.rs", "NEEDLE generated\nchanged\n");
    fixture.write("docs/readme.md", "NEEDLE docs\nchanged\n");
    fixture.write("src/Case.TXT", "NEEDLE case\nchanged\n");
    fixture.write("README.md", "root\nchanged\n");
    fixture.write("literal/[abc]/child.txt", "NEEDLE literal child\nchanged\n");

    fixture.success(
        &fixture.repo,
        &["restore", ":(glob)src/*.rs", ":(exclude)src/generated.rs"],
    );
    assert_eq!(fixture.read("src/main.rs"), "NEEDLE main\n");
    assert_eq!(
        fixture.read("src/generated.rs"),
        "NEEDLE generated\nchanged\n"
    );
    assert_eq!(fixture.read("docs/readme.md"), "NEEDLE docs\nchanged\n");

    fixture.success(&fixture.repo, &["restore", ":(icase)src/case.txt"]);
    assert_eq!(fixture.read("src/Case.TXT"), "NEEDLE case\n");

    let src_dir = fixture.repo.join("src");
    fixture.success(&src_dir, &["restore", ":(top)README.md"]);
    assert_eq!(fixture.read("README.md"), "root\n");

    fixture.success(&fixture.repo, &["restore", "literal/[abc]"]);
    assert_eq!(
        fixture.read("literal/[abc]/child.txt"),
        "NEEDLE literal child\n"
    );
}

#[test]
fn restore_empty_pathspec_file_errors_without_restoring_everything() {
    let fixture = Fixture::new();
    fixture.write("README.md", "root\nchanged\n");
    fixture.write("src/main.rs", "NEEDLE main\nchanged\n");
    fixture.write("empty-pathspecs.txt", "");

    let output = fixture.failure(
        &fixture.repo,
        &["restore", "--pathspec-from-file", "empty-pathspecs.txt"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no pathspec was given"),
        "empty pathspec file should be a usage error, got:\n{stderr}"
    );
    assert_eq!(fixture.read("README.md"), "root\nchanged\n");
    assert_eq!(fixture.read("src/main.rs"), "NEEDLE main\nchanged\n");
}

#[test]
fn checkout_path_mode_honors_shared_pathspec_magic() {
    let fixture = Fixture::new();
    fixture.write("src/main.rs", "NEEDLE main\nchanged\n");
    fixture.write("src/generated.rs", "NEEDLE generated\nchanged\n");
    fixture.write("docs/readme.md", "NEEDLE docs\nchanged\n");
    fixture.write("literal/[abc]/child.txt", "NEEDLE literal child\nchanged\n");

    fixture.success(
        &fixture.repo,
        &[
            "checkout",
            "--",
            ":(glob)src/*.rs",
            ":(exclude)src/generated.rs",
        ],
    );

    assert_eq!(fixture.read("src/main.rs"), "NEEDLE main\n");
    assert_eq!(
        fixture.read("src/generated.rs"),
        "NEEDLE generated\nchanged\n"
    );
    assert_eq!(fixture.read("docs/readme.md"), "NEEDLE docs\nchanged\n");

    fixture.success(&fixture.repo, &["checkout", "--", "literal/[abc]"]);
    assert_eq!(
        fixture.read("literal/[abc]/child.txt"),
        "NEEDLE literal child\n"
    );
}

// ---- PD-07: `clean` joins the shared pathspec engine ---------------------

/// Extract the workdir-relative paths from `clean -n` ("Would remove …") or
/// `clean -f` ("Removing …") human output, order-insensitively.
fn clean_reported_paths(stdout: &str) -> Vec<String> {
    let mut paths: Vec<String> = stdout
        .lines()
        .filter_map(|line| {
            line.strip_prefix("Would remove ")
                .or_else(|| line.strip_prefix("Removing "))
                .map(str::to_string)
        })
        .collect();
    paths.sort();
    paths
}

/// Extract the untracked (`?? `) paths from `status --porcelain` output.
fn porcelain_untracked_paths(stdout: &str) -> Vec<String> {
    let mut paths: Vec<String> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("?? ").map(str::to_string))
        .collect();
    paths.sort();
    paths
}

/// PD-07: glob magic selects deletion candidates with the same matcher as
/// ls-files/status, and the `-n` preview set is byte-identical to the `-f`
/// deletion set. Tracked files never qualify.
#[test]
fn clean_honors_shared_pathspec_magic() {
    let fixture = Fixture::new();
    fixture.write("src/tmp.log", "untracked\n");
    fixture.write("docs/tmp.log", "untracked\n");
    fixture.write("src/scratch.txt", "untracked survivor\n");

    // Cross-command parity: clean's candidate set for a spec is exactly the
    // untracked set status reports for the same spec (same shared matcher).
    let status = fixture.stdout(
        &fixture.repo,
        &["status", "--porcelain=v1", "--", ":(glob)**/*.log"],
    );
    let preview = fixture.stdout(&fixture.repo, &["clean", "-n", ":(glob)**/*.log"]);
    let previewed = clean_reported_paths(&preview);
    assert_eq!(
        previewed,
        porcelain_untracked_paths(&status),
        "clean must select the same shared-engine set as status:\nstatus:\n{status}\nclean:\n{preview}"
    );
    assert_eq!(
        previewed,
        vec!["docs/tmp.log", "src/tmp.log"],
        "glob magic must match the shared-engine set:\n{preview}"
    );

    let removal = fixture.stdout(&fixture.repo, &["clean", "-f", ":(glob)**/*.log"]);
    assert_eq!(
        clean_reported_paths(&removal),
        previewed,
        "-n preview set must equal the -f deletion set"
    );
    assert!(!fixture.repo.join("src/tmp.log").exists());
    assert!(!fixture.repo.join("docs/tmp.log").exists());
    assert!(fixture.repo.join("src/scratch.txt").exists());
    // Tracked files are untouched even though the pattern shape could match.
    assert_eq!(fixture.read("src/main.rs"), "NEEDLE main\n");
}

/// PD-07: `:(exclude)` magic can only NARROW the deletion set.
#[test]
fn clean_exclude_magic_narrows_only() {
    let fixture = Fixture::new();
    fixture.write("logs/a.log", "untracked\n");
    fixture.write("logs/keep.log", "untracked keeper\n");

    fixture.success(
        &fixture.repo,
        &["clean", "-f", "logs", ":(exclude)logs/keep.log"],
    );
    assert!(!fixture.repo.join("logs/a.log").exists());
    assert!(
        fixture.repo.join("logs/keep.log").exists(),
        ":(exclude)-hit paths must never be deleted"
    );
}

/// PD-07: pathspecs are subdirectory-relative like every other shared-engine
/// consumer, and `:(top)` re-anchors at the repository root.
#[test]
fn clean_subdir_relative_and_top_magic() {
    let fixture = Fixture::new();
    fixture.write("root.log", "untracked\n");
    fixture.write("src/sub.log", "untracked\n");
    let subdir = fixture.repo.join("src");

    let preview = fixture.stdout(&subdir, &["clean", "-n", "sub.log"]);
    assert_eq!(
        clean_reported_paths(&preview),
        vec!["src/sub.log"],
        "bare pathspec must resolve relative to the invocation subdirectory"
    );

    // Default (non-glob) fnmatch follows Git: `*` also crosses `/`, so a
    // top-anchored `*.log` selects both files from anywhere in the tree.
    let top = fixture.stdout(&subdir, &["clean", "-n", ":(top)*.log"]);
    assert_eq!(
        clean_reported_paths(&top),
        vec!["root.log", "src/sub.log"],
        ":(top) must re-anchor at the repository root with Git fnmatch semantics"
    );
}

/// PD-07: `:(icase)` magic matches case-insensitively.
#[test]
fn clean_icase_magic() {
    let fixture = Fixture::new();
    fixture.write("src/UPPER.LOG", "untracked\n");

    let preview = fixture.stdout(&fixture.repo, &["clean", "-n", ":(icase)src/upper.log"]);
    assert_eq!(clean_reported_paths(&preview), vec!["src/UPPER.LOG"]);
}

/// PD-07: bracket filenames keep P1-01 write-command parity — a
/// wildcard-looking `[abc].tmp` matches the character class AND retains the
/// literal-path fallback (union), while `:(literal)` selects only the exact
/// bracket file.
#[test]
fn clean_literal_bracket_fallback() {
    let fixture = Fixture::new();
    fixture.write("literal/[abc].tmp", "untracked bracket file\n");
    fixture.write("literal/a.tmp", "untracked class member\n");
    fixture.write("literal/z.tmp", "untracked non-member\n");

    // Union semantics (shared-engine parity with add/rm): the class member
    // and the literal bracket file both match; a non-member never does.
    let preview = fixture.stdout(&fixture.repo, &["clean", "-n", "literal/[abc].tmp"]);
    assert_eq!(
        clean_reported_paths(&preview),
        vec!["literal/[abc].tmp", "literal/a.tmp"],
        "bracket pattern must keep the literal fallback alongside the class"
    );

    // The explicit :(literal) form protects class members: only the exact
    // bracket file is deleted.
    fixture.success(
        &fixture.repo,
        &["clean", "-f", ":(literal)literal/[abc].tmp"],
    );
    assert!(!fixture.repo.join("literal/[abc].tmp").exists());
    assert!(
        fixture.repo.join("literal/a.tmp").exists(),
        ":(literal) must not expand the character class"
    );
    assert!(fixture.repo.join("literal/z.tmp").exists());
}

/// PD-07 mis-deletion guards: tracked and ignored paths stay protected even
/// when a pathspec matches them, and the empty string stays rejected.
#[test]
fn clean_pathspec_protects_tracked_and_ignored() {
    let fixture = Fixture::new();
    fixture.write(".libraignore", "*.ign\n");
    fixture.write("junk.ign", "ignored untracked\n");

    // Ignored files are not candidates without -x.
    fixture.success(&fixture.repo, &["clean", "-f", "junk.ign"]);
    assert!(
        fixture.repo.join("junk.ign").exists(),
        "ignored files must survive clean without -x"
    );

    // Tracked files are never candidates.
    fixture.success(&fixture.repo, &["clean", "-f", "README.md"]);
    assert_eq!(fixture.read("README.md"), "root\n");

    // The empty string must not silently widen the deletion set.
    let empty = fixture.failure(&fixture.repo, &["clean", "-f", ""]);
    assert!(
        String::from_utf8_lossy(&empty.stderr).contains("empty string is not a valid pathspec"),
        "empty pathspec must be rejected:\n{}",
        String::from_utf8_lossy(&empty.stderr)
    );

    // Unsupported magic fails closed before any filesystem work.
    let bogus = fixture.failure(&fixture.repo, &["clean", "-f", ":(bogus)x"]);
    assert!(
        String::from_utf8_lossy(&bogus.stderr).contains("pathspec magic"),
        "unsupported magic must fail closed with the magic hint:\n{}",
        String::from_utf8_lossy(&bogus.stderr)
    );
}
