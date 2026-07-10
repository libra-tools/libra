//! Conflict-aware status/diff compatibility contracts for plan-20260708 P0-01.

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use tempfile::{TempDir, tempdir};

#[derive(Copy, Clone, Debug)]
enum ConflictFlow {
    Merge,
    Rebase,
    CherryPick,
}

impl ConflictFlow {
    fn label(self) -> &'static str {
        match self {
            ConflictFlow::Merge => "merge",
            ConflictFlow::Rebase => "rebase",
            ConflictFlow::CherryPick => "cherry-pick",
        }
    }
}

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
        repo.success(&["config", "user.name", "Conflict Test"]);
        repo.success(&["config", "user.email", "conflict@example.com"]);
        repo.write("conflict.txt", "base\n");
        repo.success(&["add", ".libraignore", "conflict.txt"]);
        repo.success(&["commit", "-s", "-m", "base"]);
        repo
    }

    fn command(&self, args: &[&str]) -> Command {
        self.command_in(&self.repo, args)
    }

    fn command_in(&self, cwd: &PathBuf, args: &[&str]) -> Command {
        let home = self.repo.join(".libra-test-home");
        let config_home = home.join(".config");
        let global_db = home.join(".libra").join("config.db");
        fs::create_dir_all(&config_home).expect("create isolated config dir");

        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .args(args)
            .current_dir(cwd)
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

    fn run_in(&self, cwd: &PathBuf, args: &[&str]) -> Output {
        self.command_in(cwd, args).output().expect("spawn libra")
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

    fn stdout(&self, args: &[&str]) -> String {
        let output = self.success(args);
        String::from_utf8(output.stdout).expect("stdout is utf8")
    }

    fn write(&self, path: &str, contents: &str) {
        fs::write(self.repo.join(path), contents).expect("write file");
    }

    fn create_conflict(&self, flow: ConflictFlow) {
        match flow {
            ConflictFlow::Merge => self.create_merge_conflict(),
            ConflictFlow::Rebase => self.create_rebase_conflict(),
            ConflictFlow::CherryPick => self.create_cherry_pick_conflict(),
        }
    }

    fn create_merge_conflict(&self) {
        self.success(&["switch", "-c", "side"]);
        self.commit_conflict_text("side", "side");
        self.success(&["switch", "main"]);
        self.commit_conflict_text("main", "main");
        self.expect_conflict(&["merge", "side"]);
    }

    fn create_rebase_conflict(&self) {
        self.success(&["switch", "-c", "topic"]);
        self.commit_conflict_text("topic", "topic");
        self.success(&["switch", "main"]);
        self.commit_conflict_text("main", "main");
        self.success(&["switch", "topic"]);
        self.expect_conflict(&["rebase", "main"]);
    }

    fn create_cherry_pick_conflict(&self) {
        self.success(&["switch", "-c", "topic"]);
        self.commit_conflict_text("topic", "topic");
        let topic = self.stdout(&["rev-parse", "HEAD"]);
        self.success(&["switch", "main"]);
        self.commit_conflict_text("main", "main");
        self.expect_conflict(&["cherry-pick", topic.trim()]);
    }

    fn commit_conflict_text(&self, body: &str, message: &str) {
        self.write("conflict.txt", &format!("{body}\n"));
        self.success(&["add", "conflict.txt"]);
        self.success(&["commit", "-s", "-m", message]);
    }

    fn expect_conflict(&self, args: &[&str]) {
        let output = self.run(args);
        assert!(
            !output.status.success(),
            "{} should stop on conflict",
            args.join(" ")
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("LBR-CONFLICT-"),
            "{} should report a conflict error\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn conflict_paths_are_unmerged_across_status_ls_files_and_diff() {
    for flow in [
        ConflictFlow::Merge,
        ConflictFlow::Rebase,
        ConflictFlow::CherryPick,
    ] {
        let repo = TestRepo::new();
        repo.create_conflict(flow);
        assert_unmerged_contract(&repo, flow.label());
    }
}

#[test]
fn status_pathspec_preserves_global_merge_state() {
    let repo = TestRepo::new();
    repo.create_conflict(ConflictFlow::Merge);

    let status = repo.run(&["status", "--exit-code", ".libraignore"]);
    assert_eq!(
        status.status.code(),
        Some(1),
        "merge-in-progress must keep --exit-code dirty even when the conflict path is filtered"
    );
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        stdout.contains("You are in the middle of a merge"),
        "pathspec-filtered status should keep the merge recovery prompt:\n{stdout}"
    );
    assert!(
        stdout.contains("conflicts remain outside the selected pathspec"),
        "pathspec-filtered status should not claim all conflicts are fixed:\n{stdout}"
    );
    assert!(
        !stdout.contains("all conflicts fixed"),
        "hidden conflicts must not be reported as resolved:\n{stdout}"
    );
    assert!(
        !stdout.contains("working tree clean"),
        "hidden conflicts must not be followed by a clean-tree summary:\n{stdout}"
    );
    assert!(
        !stdout.contains("UU conflict.txt"),
        "pathspec should still filter the conflict path rows:\n{stdout}"
    );
}

#[test]
fn status_top_pathspec_from_subdir_keeps_merge_conflict_paths() {
    let repo = TestRepo::new();
    repo.create_conflict(ConflictFlow::Merge);
    let subdir = repo.repo.join("subdir");
    fs::create_dir_all(&subdir).expect("create subdir");

    let status = repo.run_in(&subdir, &["--json", "status", ":(top)conflict.txt"]);
    assert!(
        status.status.success(),
        "status from subdir should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("status stdout is json");
    let conflicted_paths = json["data"]["merge_state"]["conflicted_paths"]
        .as_array()
        .expect("merge_state conflicted_paths is an array");
    assert!(
        conflicted_paths
            .iter()
            .any(|path| path.as_str() == Some("conflict.txt")),
        "top pathspec from a subdir should keep repo-root merge conflict paths: {json}"
    );
}

fn assert_unmerged_contract(repo: &TestRepo, label: &str) {
    let porcelain = repo.stdout(&["status", "--porcelain"]);
    assert!(
        porcelain.lines().any(|line| line == "UU conflict.txt"),
        "{label}: porcelain v1 must report UU, got:\n{porcelain}"
    );
    assert!(
        !porcelain.lines().any(|line| line == "?? conflict.txt"),
        "{label}: conflict path must not be untracked, got:\n{porcelain}"
    );

    let porcelain_v2 = repo.stdout(&["status", "--porcelain=v2"]);
    let unmerged = porcelain_v2
        .lines()
        .find(|line| line.ends_with(" conflict.txt"))
        .unwrap_or_else(|| panic!("{label}: missing porcelain v2 conflict row:\n{porcelain_v2}"));
    let fields: Vec<_> = unmerged.split_whitespace().collect();
    assert_eq!(fields.len(), 11, "{label}: malformed v2 row {unmerged}");
    assert_eq!(fields[0], "u", "{label}: v2 row must be unmerged");
    assert_eq!(fields[1], "UU", "{label}: v2 XY must be UU");
    assert_eq!(fields[2], "N...", "{label}: v2 submodule field");
    assert_eq!(&fields[3..7], &["100644", "100644", "100644", "100644"]);
    assert_eq!(fields[10], "conflict.txt");

    let ls_unmerged = repo.stdout(&["ls-files", "-u"]);
    for stage in [" 1\tconflict.txt", " 2\tconflict.txt", " 3\tconflict.txt"] {
        assert!(
            ls_unmerged.contains(stage),
            "{label}: ls-files -u missing stage {stage:?}, got:\n{ls_unmerged}"
        );
    }

    let ls_tagged = repo.stdout(&["ls-files", "-t"]);
    let tagged_conflict_rows = ls_tagged
        .lines()
        .filter(|line| *line == "M conflict.txt")
        .count();
    assert_eq!(
        tagged_conflict_rows, 3,
        "{label}: ls-files -t must show all unmerged stages, got:\n{ls_tagged}"
    );

    let diff = repo.stdout(&["diff"]);
    assert!(
        diff.contains("diff --cc conflict.txt"),
        "{label}: diff must use a conflict-aware combined header, got:\n{diff}"
    );
    assert!(
        !diff.contains("--- /dev/null\n+++ b/conflict.txt"),
        "{label}: conflict diff must not be rendered as a /dev/null add:\n{diff}"
    );
}
