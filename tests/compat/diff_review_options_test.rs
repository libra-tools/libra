//! Script-facing diff review controls for plan-20260708 P1-08a.

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
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("create temp directory");
        let root = temp.path().to_path_buf();
        let home = root.join("home");
        fs::create_dir_all(&home).expect("create isolated home");
        Self {
            _temp: temp,
            root,
            home,
        }
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("LIBRA_CONFIG_GLOBAL_DB", self.home.join(".libra/config.db"))
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

    fn init_repo(&self, name: &str) -> PathBuf {
        let repo = self.root.join(name);
        fs::create_dir_all(&repo).expect("create repository directory");
        self.success(&self.root, &["init", repo.to_str().expect("utf8 repo")]);
        self.success(&repo, &["config", "set", "user.name", "Diff Test"]);
        self.success(&repo, &["config", "set", "user.email", "diff@example.com"]);
        repo
    }

    fn commit_all(&self, repo: &Path, message: &str) {
        self.success(repo, &["add", "-A"]);
        self.success(repo, &["commit", "--no-gpg-sign", "-s", "-m", message]);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("diff output is utf8")
}

fn staged_change_matrix(fixture: &Fixture, name: &str) -> PathBuf {
    let repo = fixture.init_repo(name);
    for (path, content) in [
        ("modified.txt", "old modified\n"),
        ("deleted.txt", "old deleted\n"),
        ("rename-old.txt", "rename payload\n"),
    ] {
        fs::write(repo.join(path), content).expect("write base fixture");
    }
    fixture.commit_all(&repo, "base");
    fs::write(repo.join("modified.txt"), "new modified\n").expect("modify fixture");
    fs::remove_file(repo.join("deleted.txt")).expect("delete fixture");
    fs::rename(repo.join("rename-old.txt"), repo.join("rename-new.txt")).expect("rename fixture");
    fs::write(repo.join("added.txt"), "new file\n").expect("add fixture");
    fixture.success(&repo, &["add", "-A"]);
    repo
}

#[test]
fn algorithm_selectors_execute_real_backends_and_obey_precedence() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("algorithms");
    let old = "void alpha() {\n    one();\n}\n\nvoid beta() {\n    two();\n}\n";
    let new = "void beta() {\n    two();\n}\n\nvoid alpha() {\n    one();\n}\n";
    fs::write(repo.join("code.c"), old).expect("write algorithm base");
    fixture.commit_all(&repo, "algorithm base");
    fs::write(repo.join("code.c"), new).expect("reorder functions");

    let default = stdout(&fixture.success(&repo, &["diff", "--", "code.c"]));
    let myers = stdout(&fixture.success(&repo, &["diff", "--algorithm=myers", "--", "code.c"]));
    let minimal = stdout(&fixture.success(&repo, &["diff", "--minimal", "--", "code.c"]));
    let named_minimal =
        stdout(&fixture.success(&repo, &["diff", "--algorithm=myersMinimal", "--", "code.c"]));
    let patience = stdout(&fixture.success(&repo, &["diff", "--patience", "--", "code.c"]));
    let named_patience =
        stdout(&fixture.success(&repo, &["diff", "--algorithm=patience", "--", "code.c"]));
    let histogram = stdout(&fixture.success(&repo, &["diff", "--histogram", "--", "code.c"]));
    let named_histogram =
        stdout(&fixture.success(&repo, &["diff", "--algorithm=histogram", "--", "code.c"]));
    let anchored_alpha =
        stdout(&fixture.success(&repo, &["diff", "--anchored=void alpha", "--", "code.c"]));
    let anchored_beta =
        stdout(&fixture.success(&repo, &["diff", "--anchored=void beta", "--", "code.c"]));

    assert_eq!(default, myers, "Myers is the truthful default");
    assert_eq!(minimal, myers, "Myers already computes a shortest script");
    assert_eq!(named_minimal, minimal, "named and shorthand minimal agree");
    assert_ne!(patience, myers, "Patience uses different anchors here");
    assert_eq!(
        named_patience, patience,
        "named and shorthand Patience agree"
    );
    assert_eq!(
        named_histogram, histogram,
        "named and shorthand Histogram agree"
    );
    assert!(
        anchored_alpha.lines().any(|line| line == " void alpha() {"),
        "the unique alpha line stays context:\n{anchored_alpha}"
    );
    assert!(
        !anchored_alpha
            .lines()
            .any(|line| line == "-void alpha() {" || line == "+void alpha() {"),
        "the selected anchor must not surface as delete+insert:\n{anchored_alpha}"
    );

    let retained_across_histogram = stdout(&fixture.success(
        &repo,
        &[
            "diff",
            "--anchored=void alpha",
            "--histogram",
            "--anchored=void beta",
            "--",
            "code.c",
        ],
    ));
    let cleared_by_patience = stdout(&fixture.success(
        &repo,
        &[
            "diff",
            "--anchored=void alpha",
            "--patience",
            "--anchored=void beta",
            "--",
            "code.c",
        ],
    ));
    assert_eq!(
        retained_across_histogram, anchored_alpha,
        "histogram leaves the earlier alpha anchor available when anchored is reselected"
    );
    assert_eq!(
        cleared_by_patience, anchored_beta,
        "the patience shorthand clears the earlier alpha anchor"
    );

    let histogram_last = stdout(&fixture.success(
        &repo,
        &["diff", "--patience", "--histogram", "--", "code.c"],
    ));
    let patience_last = stdout(&fixture.success(
        &repo,
        &["diff", "--histogram", "--patience", "--", "code.c"],
    ));
    assert_eq!(histogram_last, histogram, "last algorithm selector wins");
    assert_eq!(patience_last, patience, "last algorithm selector wins");

    let myers_filtered = stdout(&fixture.success(
        &repo,
        &[
            "diff",
            "--algorithm=myers",
            "-w",
            "--ignore-blank-lines",
            "--",
            "code.c",
        ],
    ));
    let patience_filtered = stdout(&fixture.success(
        &repo,
        &[
            "diff",
            "--patience",
            "-w",
            "--ignore-blank-lines",
            "--",
            "code.c",
        ],
    ));
    assert_ne!(
        patience_filtered, myers_filtered,
        "whitespace/blank re-diff must retain the selected backend"
    );
    let anchored_filtered = stdout(&fixture.success(
        &repo,
        &[
            "diff",
            "--anchored=void alpha",
            "-w",
            "--ignore-blank-lines",
            "--",
            "code.c",
        ],
    ));
    assert!(
        anchored_filtered
            .lines()
            .any(|line| line == " void alpha() {"),
        "whitespace/blank re-diff must retain anchors:\n{anchored_filtered}"
    );

    fs::write(repo.join(".libra_attributes"), "*.c diff=identity\n")
        .expect("write textconv attributes");
    fixture.success(&repo, &["config", "set", "diff.identity.textconv", "cat"]);
    let myers_textconv =
        stdout(&fixture.success(&repo, &["diff", "--algorithm=myers", "--", "code.c"]));
    let patience_textconv =
        stdout(&fixture.success(&repo, &["diff", "--patience", "--", "code.c"]));
    assert_ne!(
        patience_textconv, myers_textconv,
        "textconv re-diff must retain the selected backend"
    );
    let anchored_textconv =
        stdout(&fixture.success(&repo, &["diff", "--anchored=void alpha", "--", "code.c"]));
    assert!(
        anchored_textconv
            .lines()
            .any(|line| line == " void alpha() {"),
        "textconv re-diff must retain anchors:\n{anchored_textconv}"
    );

    fs::rename(repo.join("code.c"), repo.join("moved.c")).expect("rename changed file");
    fixture.success(&repo, &["add", "-A"]);
    let myers_rename = stdout(&fixture.success(
        &repo,
        &[
            "diff",
            "--cached",
            "--algorithm=myers",
            "--no-textconv",
            "--",
            "code.c",
            "moved.c",
        ],
    ));
    let patience_rename = stdout(&fixture.success(
        &repo,
        &[
            "diff",
            "--cached",
            "--patience",
            "--no-textconv",
            "--",
            "code.c",
            "moved.c",
        ],
    ));
    assert!(
        patience_rename.contains("rename from code.c"),
        "{patience_rename}"
    );
    assert_ne!(
        patience_rename, myers_rename,
        "rename body must retain the selected backend"
    );
    let anchored_rename = stdout(&fixture.success(
        &repo,
        &[
            "diff",
            "--cached",
            "--anchored=void alpha",
            "--no-textconv",
            "--",
            "code.c",
            "moved.c",
        ],
    ));
    assert!(anchored_rename.contains("rename from code.c"));
    assert!(
        anchored_rename
            .lines()
            .any(|line| line == " void alpha() {"),
        "rename body must retain anchors:\n{anchored_rename}"
    );

    let invalid = fixture.run(&repo, &["diff", "--algorithm=bogus", "--", "code.c"]);
    assert_eq!(invalid.status.code(), Some(129));
    assert!(invalid.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&invalid.stderr);
    assert!(
        stderr.contains("invalid diff algorithm 'bogus'"),
        "{stderr}"
    );
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
    assert!(!stderr.contains("Scanning working tree"), "{stderr}");
}

#[test]
fn raw_records_cover_add_delete_modify_rename_and_nul_mode() {
    let fixture = Fixture::new();
    let repo = staged_change_matrix(&fixture, "raw-matrix");

    let raw = stdout(&fixture.success(&repo, &["diff", "--cached", "--raw"]));
    assert!(
        raw.lines().any(|line| line.ends_with(" A\tadded.txt")),
        "{raw}"
    );
    assert!(
        raw.lines().any(|line| line.ends_with(" D\tdeleted.txt")),
        "{raw}"
    );
    assert!(
        raw.lines().any(|line| line.ends_with(" M\tmodified.txt")),
        "{raw}"
    );
    assert!(
        raw.lines()
            .any(|line| line.contains(" R100\trename-old.txt\trename-new.txt")),
        "{raw}"
    );
    assert!(raw.lines().all(|line| line.starts_with(':')), "{raw}");

    let compact = stdout(&fixture.success(&repo, &["diff", "--cached", "--compact-summary"]));
    assert!(compact.contains("added.txt (new) |"), "{compact}");
    assert!(compact.contains("deleted.txt (gone) |"), "{compact}");

    let nul = fixture.success(&repo, &["diff", "--cached", "--raw", "-z"]);
    assert!(!nul.stdout.ends_with(b"\n"), "NUL output gained a newline");
    assert!(
        nul.stdout
            .windows(b"R100\0rename-old.txt\0rename-new.txt\0".len())
            .any(|window| window == b"R100\0rename-old.txt\0rename-new.txt\0"),
        "NUL rename record missing: {:?}",
        nul.stdout
    );
}

#[test]
fn diff_filter_supports_include_exclude_all_or_none_and_validation() {
    let fixture = Fixture::new();
    let repo = staged_change_matrix(&fixture, "diff-filter");

    let included = stdout(&fixture.success(
        &repo,
        &["diff", "--cached", "--name-status", "--diff-filter=AM"],
    ));
    assert!(included.contains("A\tadded.txt"), "{included}");
    assert!(included.contains("M\tmodified.txt"), "{included}");
    assert!(!included.contains("deleted.txt"), "{included}");
    assert!(!included.contains("rename-new.txt"), "{included}");

    let excluded = stdout(&fixture.success(
        &repo,
        &["diff", "--cached", "--name-only", "--diff-filter=ad"],
    ));
    assert!(excluded.contains("modified.txt"), "{excluded}");
    assert!(excluded.contains("rename-new.txt"), "{excluded}");
    assert!(!excluded.contains("added.txt"), "{excluded}");
    assert!(!excluded.contains("deleted.txt"), "{excluded}");

    let all = stdout(&fixture.success(
        &repo,
        &["diff", "--cached", "--name-only", "--diff-filter=R*"],
    ));
    assert_eq!(all.lines().count(), 4, "{all}");
    let all_with_exclusions = stdout(&fixture.success(
        &repo,
        &["diff", "--cached", "--name-only", "--diff-filter=ad*"],
    ));
    assert_eq!(
        all_with_exclusions.lines().count(),
        4,
        "`*` must retain the whole set once a non-A/D path matches: {all_with_exclusions}"
    );
    let none = stdout(&fixture.success(
        &repo,
        &["diff", "--cached", "--name-only", "--diff-filter=T*"],
    ));
    assert!(none.is_empty(), "{none}");

    let invalid = fixture.run(&repo, &["diff", "--diff-filter=Q"]);
    assert_eq!(invalid.status.code(), Some(129));
    assert!(invalid.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&invalid.stderr);
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
    assert!(!stderr.contains("Scanning working tree"), "{stderr}");
}

#[test]
fn full_index_and_explicit_prefixes_rewrite_patch_headers() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("full-index-prefixes");
    fs::write(repo.join("tracked.txt"), "old\n").expect("write base");
    fixture.commit_all(&repo, "base");
    fs::write(repo.join("tracked.txt"), "new\n").expect("modify file");
    fixture.success(&repo, &["add", "tracked.txt"]);

    let output = stdout(&fixture.success(
        &repo,
        &[
            "diff",
            "--cached",
            "--full-index",
            "--src-prefix=OLD/",
            "--dst-prefix=NEW/",
        ],
    ));
    assert!(
        output.contains("diff --git OLD/tracked.txt NEW/tracked.txt"),
        "{output}"
    );
    assert!(output.contains("--- OLD/tracked.txt"), "{output}");
    assert!(output.contains("+++ NEW/tracked.txt"), "{output}");
    let index = output
        .lines()
        .find(|line| line.starts_with("index "))
        .expect("full index line");
    let ids = index
        .split_whitespace()
        .nth(1)
        .expect("index ids")
        .split("..")
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2, "{index}");
    assert!(ids.iter().all(|id| id.len() == 40), "{index}");

    let reversed = stdout(&fixture.success(
        &repo,
        &[
            "diff",
            "--cached",
            "-R",
            "--src-prefix=OLD/",
            "--dst-prefix=NEW/",
        ],
    ));
    assert!(
        reversed.contains("diff --git NEW/tracked.txt OLD/tracked.txt"),
        "{reversed}"
    );

    fs::write(repo.join("binary.bin"), b"old\0binary").expect("write base binary");
    fixture.commit_all(&repo, "binary base");
    fs::write(repo.join("binary.bin"), b"new\0binary payload").expect("modify binary");
    fixture.success(&repo, &["add", "binary.bin"]);
    let binary = stdout(&fixture.success(
        &repo,
        &["diff", "--cached", "--full-index", "--", "binary.bin"],
    ));
    assert!(binary.contains("Binary files"), "{binary}");
    let binary_index = binary
        .lines()
        .find(|line| line.starts_with("index "))
        .expect("binary full-index line");
    assert!(
        binary_index
            .split_whitespace()
            .nth(1)
            .expect("binary ids")
            .split("..")
            .all(|id| id.len() == 40),
        "{binary_index}"
    );
}

#[test]
fn explicit_prefix_pair_bypasses_irrelevant_broken_prefix_defaults() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("prefix-override");
    fs::write(repo.join("tracked.txt"), "old\n").expect("write base");
    fixture.commit_all(&repo, "base");
    fs::write(repo.join("tracked.txt"), "new\n").expect("modify file");
    fixture.success(&repo, &["config", "diff.noPrefix", "sideways"]);

    let output = stdout(&fixture.success(&repo, &["diff", "--src-prefix=S/", "--dst-prefix=D/"]));
    assert!(
        output.contains("diff --git S/tracked.txt D/tracked.txt"),
        "{output}"
    );
}

#[test]
fn raw_worktree_records_use_zero_postimage_object_id() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("raw-worktree");
    fs::write(repo.join("tracked.txt"), "old\n").expect("write base");
    fixture.commit_all(&repo, "base");
    fs::write(repo.join("tracked.txt"), "new\n").expect("modify file");

    let raw = stdout(&fixture.success(&repo, &["diff", "--raw"]));
    let fields = raw.split_whitespace().collect::<Vec<_>>();
    assert_eq!(fields.get(3), Some(&"0000000"), "{raw}");
    assert!(raw.trim_end().ends_with("M\ttracked.txt"), "{raw}");
}

#[cfg(unix)]
#[test]
fn mode_only_changes_feed_raw_summary_and_compact_summary() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let repo = fixture.init_repo("mode-only");
    fs::write(repo.join("script.sh"), "#!/bin/sh\nexit 0\n").expect("write script");
    fixture.commit_all(&repo, "base");
    let mut permissions = fs::metadata(repo.join("script.sh"))
        .expect("stat script")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(repo.join("script.sh"), permissions).expect("chmod script");

    let worktree_raw = stdout(&fixture.success(&repo, &["diff", "--raw"]));
    assert!(worktree_raw.contains(":100644 100755 "), "{worktree_raw}");
    let worktree_fields = worktree_raw.split_whitespace().collect::<Vec<_>>();
    assert_ne!(worktree_fields.get(2), Some(&"0000000"), "{worktree_raw}");
    assert_eq!(worktree_fields.get(3), Some(&"0000000"), "{worktree_raw}");

    let driver = repo.join("mode-driver.sh");
    fs::write(
        &driver,
        "#!/bin/sh\nprintf '%s %s %s %s\\n' \"$3\" \"$4\" \"$6\" \"$7\"\n",
    )
    .expect("write external diff driver");
    let mut driver_permissions = fs::metadata(&driver)
        .expect("stat external diff driver")
        .permissions();
    driver_permissions.set_mode(0o755);
    fs::set_permissions(&driver, driver_permissions).expect("chmod external diff driver");
    fixture.success(
        &repo,
        &[
            "config",
            "diff.external",
            driver.to_str().expect("utf8 driver path"),
        ],
    );
    let filtered_external = stdout(&fixture.success(&repo, &["diff", "--diff-filter=T"]));
    assert!(
        filtered_external.is_empty(),
        "filtered files must not invoke diff.external: {filtered_external}"
    );
    let external = stdout(&fixture.success(&repo, &["diff"]));
    let external_fields = external.split_whitespace().collect::<Vec<_>>();
    assert_eq!(external_fields.len(), 4, "{external}");
    assert_ne!(external_fields[0], "0".repeat(40), "{external}");
    assert_eq!(external_fields[1], "100644", "{external}");
    assert_eq!(external_fields[2], "0".repeat(40), "{external}");
    assert_eq!(external_fields[3], "100755", "{external}");
    fixture.success(&repo, &["config", "--unset", "diff.external"]);

    fixture.success(&repo, &["add", "--chmod=+x", "script.sh"]);

    let raw = stdout(&fixture.success(&repo, &["diff", "--cached", "--raw"]));
    assert!(raw.contains(":100644 100755 "), "{raw}");
    let staged_fields = raw.split_whitespace().collect::<Vec<_>>();
    assert_ne!(staged_fields.get(2), Some(&"0000000"), "{raw}");
    assert_eq!(staged_fields.get(2), staged_fields.get(3), "{raw}");
    assert!(raw.trim_end().ends_with("M\tscript.sh"), "{raw}");

    let summary = stdout(&fixture.success(&repo, &["diff", "--cached", "--summary"]));
    assert_eq!(summary.trim(), "mode change 100644 => 100755 script.sh");

    let compact = stdout(&fixture.success(&repo, &["diff", "--cached", "--compact-summary"]));
    assert!(compact.contains("script.sh (+x) | 0"), "{compact}");

    fs::write(repo.join("script.sh"), "#!/bin/sh\necho changed\n").expect("modify script");
    fixture.success(&repo, &["add", "script.sh"]);
    let content_and_mode = stdout(&fixture.success(&repo, &["diff", "--cached", "--full-index"]));
    assert!(
        content_and_mode.contains("old mode 100644"),
        "{content_and_mode}"
    );
    assert!(
        content_and_mode.contains("new mode 100755"),
        "{content_and_mode}"
    );
    let changed_index = content_and_mode
        .lines()
        .find(|line| line.starts_with("index "))
        .expect("content+mode index line");
    assert_eq!(
        changed_index.split_whitespace().count(),
        2,
        "{changed_index}"
    );
    assert!(
        changed_index
            .split_whitespace()
            .nth(1)
            .expect("content+mode ids")
            .split("..")
            .all(|id| id.len() == 40),
        "{changed_index}"
    );

    fixture.commit_all(&repo, "make script executable");
    fs::write(repo.join("script.sh"), "#!/bin/sh\necho changed again\n")
        .expect("modify executable script");
    fixture.success(&repo, &["add", "script.sh"]);
    let executable_content = stdout(&fixture.success(&repo, &["diff", "--cached", "--full-index"]));
    let executable_index = executable_content
        .lines()
        .find(|line| line.starts_with("index "))
        .expect("executable index line");
    assert!(executable_index.ends_with(" 100755"), "{executable_index}");
}

#[cfg(unix)]
#[test]
fn mode_changes_survive_textconv_and_whitespace_body_suppression() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let repo = fixture.init_repo("mode-body-suppression");
    fs::write(repo.join("script.sh"), "echo x\n").expect("write script");
    fs::write(repo.join(".gitattributes"), "script.sh diff=constant\n").expect("write attributes");
    fixture.commit_all(&repo, "base");

    let driver = repo.join("constant-textconv.sh");
    fs::write(&driver, "#!/bin/sh\nprintf 'constant\\n'\n").expect("write textconv driver");
    let mut driver_permissions = fs::metadata(&driver)
        .expect("stat textconv driver")
        .permissions();
    driver_permissions.set_mode(0o755);
    fs::set_permissions(&driver, driver_permissions).expect("chmod textconv driver");
    fixture.success(
        &repo,
        &[
            "config",
            "diff.constant.textconv",
            driver.to_str().expect("utf8 driver path"),
        ],
    );

    let mut permissions = fs::metadata(repo.join("script.sh"))
        .expect("stat script")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(repo.join("script.sh"), permissions).expect("chmod script");
    fs::write(repo.join("script.sh"), "echo  x\n").expect("change whitespace");
    fixture.success(&repo, &["add", "--chmod=+x", "script.sh"]);
    fixture.success(&repo, &["add", "script.sh"]);

    let textconv_summary = stdout(&fixture.success(&repo, &["diff", "--cached", "--summary"]));
    assert_eq!(
        textconv_summary.trim(),
        "mode change 100644 => 100755 script.sh"
    );
    let whitespace_summary = stdout(&fixture.success(
        &repo,
        &["diff", "--cached", "--no-textconv", "-w", "--summary"],
    ));
    assert_eq!(
        whitespace_summary.trim(),
        "mode change 100644 => 100755 script.sh"
    );
}

#[cfg(unix)]
#[test]
fn diff_filter_all_or_none_is_recomputed_after_sparse_view_projection() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let repo = fixture.init_repo("filter-sparse-view");
    fs::create_dir_all(repo.join("src")).expect("create in-view directory");
    fs::create_dir_all(repo.join("docs")).expect("create hidden directory");
    fs::write(repo.join("src/in.txt"), "old\n").expect("write in-view file");
    fs::write(repo.join("docs/hidden.sh"), "#!/bin/sh\n").expect("write hidden file");
    fixture.commit_all(&repo, "base");
    fixture.success(&repo, &["sparse-view", "set", "src/**"]);

    fs::write(repo.join("src/in.txt"), "new\n").expect("modify in-view file");
    fs::remove_file(repo.join("docs/hidden.sh")).expect("remove hidden regular file");
    symlink("hidden-target", repo.join("docs/hidden.sh"))
        .expect("replace hidden file with symlink");

    let output = stdout(&fixture.success(&repo, &["diff", "--name-only", "--diff-filter=T*"]));
    assert!(
        output.is_empty(),
        "hidden T must not make visible M pass all-or-none: {output}"
    );
    fixture.success(&repo, &["sparse-view", "disable"]);
    let hidden_type_change =
        stdout(&fixture.success(&repo, &["diff", "--name-status", "--diff-filter=T"]));
    assert_eq!(hidden_type_change.trim(), "T\tdocs/hidden.sh");
}

#[cfg(unix)]
#[test]
fn rename_detection_does_not_pair_different_file_types() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let repo = fixture.init_repo("rename-file-type");
    fs::write(repo.join("old-regular"), "link-target").expect("write regular file");
    fixture.commit_all(&repo, "base");

    fs::remove_file(repo.join("old-regular")).expect("remove regular file");
    symlink("link-target", repo.join("new-symlink")).expect("create symlink");
    fixture.success(&repo, &["add", "-A"]);

    let output = stdout(&fixture.success(&repo, &["diff", "--cached", "--name-status", "-M100%"]));
    assert!(output.contains("D\told-regular"), "{output}");
    assert!(output.contains("A\tnew-symlink"), "{output}");
    assert!(
        !output.lines().any(|line| line.starts_with('R')),
        "{output}"
    );
}

#[test]
fn pickaxe_string_and_regex_filter_file_pairs_and_compose_with_status_filter() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("pickaxe-file-pairs");
    for (path, content) in [
        ("count.txt", "needle\n"),
        ("moved.txt", "needle old\n"),
        ("regex.txt", "handler_v1\n"),
        ("unrelated.txt", "old\n"),
        ("rename-old.txt", "stable rename needle\n"),
    ] {
        fs::write(repo.join(path), content).expect("write pickaxe base");
    }
    fs::write(repo.join("binary.bin"), b"\0binary-only").expect("write binary base");
    fixture.commit_all(&repo, "base");

    fs::write(repo.join("count.txt"), "needle\nneedle\n").expect("change count");
    fs::write(repo.join("moved.txt"), "needle new\n").expect("edit around stable literal");
    fs::write(repo.join("regex.txt"), "handler_v2\n").expect("change regex line");
    fs::write(repo.join("unrelated.txt"), "new\n").expect("change unrelated file");
    fs::write(repo.join("binary.bin"), b"\0binary-only binary-only")
        .expect("change binary literal count");
    fs::rename(repo.join("rename-old.txt"), repo.join("rename-new.txt"))
        .expect("rename stable literal");
    fs::write(repo.join("added.txt"), "new needle\n").expect("add literal");
    fixture.success(&repo, &["add", "-A"]);

    let string =
        stdout(&fixture.success(&repo, &["diff", "--cached", "-S", "needle", "--name-only"]));
    let selected = string.lines().collect::<Vec<_>>();
    assert_eq!(selected.len(), 2, "{string}");
    assert!(selected.contains(&"added.txt"), "{string}");
    assert!(selected.contains(&"count.txt"), "{string}");
    assert!(
        !string.contains("moved.txt"),
        "equal counts must not match: {string}"
    );
    assert!(
        !string.contains("rename-new.txt"),
        "exact rename must not match: {string}"
    );

    let raw = stdout(&fixture.success(&repo, &["diff", "--cached", "-Sneedle", "--raw"]));
    assert_eq!(raw.lines().count(), 2, "{raw}");
    assert!(raw.contains(" A\tadded.txt"), "{raw}");
    assert!(raw.contains(" M\tcount.txt"), "{raw}");

    let regex = stdout(&fixture.success(
        &repo,
        &["diff", "--cached", "-Ghandler_v[0-9]", "--name-only"],
    ));
    assert_eq!(regex.trim(), "regex.txt");

    let changed_line =
        stdout(&fixture.success(&repo, &["diff", "--cached", "-G", "needle", "--name-only"]));
    assert!(changed_line.contains("moved.txt"), "{changed_line}");

    let modified_only = stdout(&fixture.success(
        &repo,
        &[
            "diff",
            "--cached",
            "-Sneedle",
            "--diff-filter=M",
            "--name-only",
        ],
    ));
    assert_eq!(modified_only.trim(), "count.txt");

    let binary =
        stdout(&fixture.success(&repo, &["diff", "--cached", "-Sbinary-only", "--name-only"]));
    assert_eq!(binary.trim(), "binary.bin", "{binary}");
}

#[test]
fn pickaxe_validation_fails_before_worktree_progress() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("pickaxe-validation");
    fs::write(repo.join("file.txt"), "content\n").expect("write fixture");
    fixture.commit_all(&repo, "base");
    fs::write(repo.join("file.txt"), "changed\n").expect("change fixture");

    let invalid = fixture.run(&repo, &["diff", "-G", "["]);
    assert_eq!(invalid.status.code(), Some(129));
    assert!(invalid.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&invalid.stderr);
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
    assert!(stderr.contains("invalid -G regex"), "{stderr}");
    assert!(!stderr.contains("Scanning working tree"), "{stderr}");

    let conflicting = fixture.run(&repo, &["diff", "-S", "x", "-G", "x"]);
    assert!(!conflicting.status.success());

    let empty = stdout(&fixture.success(&repo, &["diff", "-S", "", "--name-only"]));
    assert!(empty.is_empty(), "an empty literal never matches: {empty}");
}

#[cfg(unix)]
#[test]
fn pickaxe_reuses_textconv_and_filters_before_external_driver() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let repo = fixture.init_repo("pickaxe-drivers");
    fs::write(repo.join(".gitattributes"), "*.dat diff=upper\n").expect("write attributes");
    fs::write(repo.join("pick.dat"), "token\n").expect("write textconv base");
    fs::write(repo.join("match.txt"), "old\n").expect("write match base");
    fs::write(repo.join("other.txt"), "before\n").expect("write other base");
    fixture.commit_all(&repo, "base");

    fs::write(repo.join("pick.dat"), "token token\n").expect("change textconv file");
    fs::write(repo.join("match.txt"), "new needle\n").expect("change matching file");
    fs::write(repo.join("other.txt"), "after\n").expect("change other file");
    fixture.success(&repo, &["add", "-A"]);
    fixture.success(
        &repo,
        &["config", "set", "diff.upper.textconv", "tr a-z A-Z <"],
    );

    let converted =
        stdout(&fixture.success(&repo, &["diff", "--cached", "-S", "TOKEN", "--name-only"]));
    assert_eq!(converted.trim(), "pick.dat", "{converted}");
    let raw = stdout(&fixture.success(
        &repo,
        &[
            "diff",
            "--cached",
            "--no-textconv",
            "-S",
            "TOKEN",
            "--name-only",
        ],
    ));
    assert!(
        raw.is_empty(),
        "raw lowercase content must not match TOKEN: {raw}"
    );

    let driver = repo.join("path-driver.sh");
    fs::write(&driver, "#!/bin/sh\nprintf '%s\\n' \"$1\"\n").expect("write external driver");
    let mut permissions = fs::metadata(&driver)
        .expect("stat external driver")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&driver, permissions).expect("chmod external driver");
    fixture.success(
        &repo,
        &[
            "config",
            "set",
            "diff.external",
            driver.to_str().expect("utf8 driver path"),
        ],
    );

    let external = stdout(&fixture.success(
        &repo,
        &["diff", "--cached", "--no-textconv", "-S", "needle"],
    ));
    assert_eq!(external.trim(), "match.txt", "{external}");
}
