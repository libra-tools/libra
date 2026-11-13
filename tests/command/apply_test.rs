//! Integration tests for `libra apply --check`.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::fs;

use tempfile::{TempDir, tempdir};

use super::{
    assert_cli_success, parse_json_stdout, run_libra_command, run_libra_command_with_stdin,
};

fn init_repo() -> TempDir {
    let repo = tempdir().unwrap();
    assert!(run_libra_command(&["init"], repo.path()).status.success());
    repo
}

/// A clean single-file modification: change line 2 of `f.txt`.
const MODIFY_PATCH: &str = "\
--- a/f.txt
+++ b/f.txt
@@ -1,3 +1,3 @@
 a
-b
+B
 c
";

#[test]
fn apply_check_clean_modification_exits_0() {
    let repo = init_repo();
    fs::write(repo.path().join("f.txt"), "a\nb\nc\n").unwrap();
    fs::write(repo.path().join("p.diff"), MODIFY_PATCH).unwrap();
    let out = run_libra_command(&["apply", "--check", "p.diff"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "clean patch should apply: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // --check must not modify the file.
    assert_eq!(
        fs::read_to_string(repo.path().join("f.txt")).unwrap(),
        "a\nb\nc\n"
    );
}

#[test]
fn apply_check_non_matching_context_exits_1() {
    let repo = init_repo();
    fs::write(repo.path().join("f.txt"), "x\ny\nz\n").unwrap();
    fs::write(repo.path().join("p.diff"), MODIFY_PATCH).unwrap();
    let out = run_libra_command(&["apply", "--check", "p.diff"], repo.path());
    assert_eq!(out.status.code(), Some(1), "a non-applying patch exits 1");
}

#[test]
fn apply_check_new_file_exits_0() {
    let repo = init_repo();
    let patch = "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+hello\n+world\n";
    fs::write(repo.path().join("p.diff"), patch).unwrap();
    let out = run_libra_command(&["apply", "--check", "p.diff"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "a new-file patch should apply: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn apply_check_multi_file_exits_0() {
    let repo = init_repo();
    fs::write(repo.path().join("f.txt"), "a\nb\nc\n").unwrap();
    fs::write(repo.path().join("g.txt"), "1\n2\n3\n").unwrap();
    let patch =
        format!("{MODIFY_PATCH}--- a/g.txt\n+++ b/g.txt\n@@ -1,3 +1,3 @@\n 1\n-2\n+TWO\n 3\n");
    fs::write(repo.path().join("p.diff"), patch).unwrap();
    let out = run_libra_command(&["apply", "--check", "p.diff"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "a clean multi-file patch should apply: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn apply_check_respects_strip_p0() {
    let repo = init_repo();
    fs::write(repo.path().join("f.txt"), "a\nb\nc\n").unwrap();
    // No a/ b/ prefixes; -p0 keeps the path as-is.
    let patch = "--- f.txt\n+++ f.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n";
    fs::write(repo.path().join("p.diff"), patch).unwrap();
    let out = run_libra_command(&["apply", "--check", "-p0", "p.diff"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "-p0 patch should apply: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn apply_check_reads_stdin() {
    let repo = init_repo();
    fs::write(repo.path().join("f.txt"), "a\nb\nc\n").unwrap();
    let out = run_libra_command_with_stdin(&["apply", "--check"], repo.path(), MODIFY_PATCH);
    assert_eq!(
        out.status.code(),
        Some(0),
        "patch from stdin should apply: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn apply_security_rejects_path_traversal() {
    let repo = init_repo();
    let patch = "--- a/x\n+++ b/../escape.txt\n@@ -0,0 +1,1 @@\n+x\n";
    fs::write(repo.path().join("p.diff"), patch).unwrap();
    let out = run_libra_command(&["apply", "--check", "p.diff"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(128),
        "`..` traversal must be rejected"
    );
}

#[test]
fn apply_security_rejects_noncanonical_alias_components() {
    let repo = init_repo();
    for (name, target) in [("double.diff", "b/dir//file"), ("dot.diff", "b/dir/./file")] {
        let patch = format!("--- /dev/null\n+++ {target}\n@@ -0,0 +1 @@\n+x\n");
        fs::write(repo.path().join(name), patch).unwrap();
        let out = run_libra_command(&["apply", "--check", name], repo.path());
        assert_eq!(
            out.status.code(),
            Some(128),
            "{target} must not alias a canonical worktree path: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn apply_security_rejects_libra_internal_path() {
    let repo = init_repo();
    let patch = "--- /dev/null\n+++ b/.libra/evil\n@@ -0,0 +1,1 @@\n+x\n";
    fs::write(repo.path().join("p.diff"), patch).unwrap();
    let out = run_libra_command(&["apply", "--check", "p.diff"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(128),
        "writing inside .libra must be rejected"
    );
}

#[test]
fn apply_security_rejects_absolute_path_after_strip() {
    let repo = init_repo();
    // An absolute target must be rejected even though -p1 would turn it relative.
    let patch = "--- a/x\n+++ /abs/evil.txt\n@@ -0,0 +1,1 @@\n+x\n";
    fs::write(repo.path().join("p.diff"), patch).unwrap();
    let out = run_libra_command(&["apply", "--check", "p.diff"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(128),
        "an absolute patch path must be rejected before stripping: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(unix)]
#[test]
fn apply_security_rejects_symlink_path_components() {
    use std::os::unix::fs::symlink;

    let repo = init_repo();
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("victim.txt"), "old\n").unwrap();
    symlink(outside.path(), repo.path().join("link")).unwrap();
    let patch = "--- a/link/victim.txt\n+++ b/link/victim.txt\n@@ -1 +1 @@\n-old\n+new\n";
    fs::write(repo.path().join("p.diff"), patch).unwrap();

    let out = run_libra_command(&["apply", "--check", "p.diff"], repo.path());
    assert_eq!(out.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("symlink patch path"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(outside.path().join("victim.txt")).unwrap(),
        "old\n"
    );
}

#[test]
fn apply_check_clean_deletion_exits_0() {
    let repo = init_repo();
    fs::write(repo.path().join("f.txt"), "a\nb\n").unwrap();
    let patch = "--- a/f.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n";
    fs::write(repo.path().join("p.diff"), patch).unwrap();
    let out = run_libra_command(&["apply", "--check", "p.diff"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "a clean full-file deletion should apply: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn apply_without_check_writes_the_patch() {
    let repo = init_repo();
    fs::write(repo.path().join("f.txt"), "a\nb\nc\n").unwrap();
    fs::write(repo.path().join("p.diff"), MODIFY_PATCH).unwrap();
    let out = run_libra_command(&["apply", "p.diff"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(repo.path().join("f.txt")).unwrap(),
        "a\nB\nc\n"
    );
}

#[test]
fn apply_check_malformed_patch_is_an_error() {
    let repo = init_repo();
    fs::write(repo.path().join("p.diff"), "this is not a patch\n").unwrap();
    let out = run_libra_command(&["apply", "--check", "p.diff"], repo.path());
    assert_eq!(out.status.code(), Some(128), "a malformed patch exits 128");
}

#[test]
fn apply_check_json_reports_result() {
    let repo = init_repo();
    fs::write(repo.path().join("f.txt"), "a\nb\nc\n").unwrap();
    fs::write(repo.path().join("p.diff"), MODIFY_PATCH).unwrap();
    let out = run_libra_command(&["--json", "apply", "--check", "p.diff"], repo.path());
    assert_eq!(out.status.code(), Some(0));
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["applies"].as_bool(), Some(true));
    assert_eq!(json["data"]["files"][0].as_str(), Some("f.txt"));
}

#[test]
fn apply_outside_repository_is_an_error() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("p.diff"), MODIFY_PATCH).unwrap();
    let out = run_libra_command(&["apply", "--check", "p.diff"], dir.path());
    assert_eq!(out.status.code(), Some(128));
}

/// LG-03 (M-HUNK H7): `apply --check` accepts legacy non-canonical hunk headers.
#[test]
fn test_apply_check_accepts_legacy_hunk_headers() {
    let repo = init_repo();
    assert!(
        run_libra_command(&["config", "user.name", "T"], repo.path())
            .status
            .success()
    );
    assert!(
        run_libra_command(&["config", "user.email", "t@e"], repo.path())
            .status
            .success()
    );
    fs::write(repo.path().join("f.txt"), "a\n").unwrap();
    assert!(
        run_libra_command(&["add", "f.txt"], repo.path())
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path())
            .status
            .success()
    );

    // Legacy form: explicit `,0`/`,1` counts on both sides.
    let legacy = "\
--- a/f.txt
+++ b/f.txt
@@ -1,0 +2,1 @@
+b
";
    let patch = repo.path().join("legacy.patch");
    fs::write(&patch, legacy).unwrap();
    let out = run_libra_command(&["apply", "--check", patch.to_str().unwrap()], repo.path());
    assert_cli_success(&out, "apply --check legacy header");
}

/// LG-11 (M-APPLY A1–A5): report modes, worktree application, reverse, and
/// atomic failure. Ports `t4106:15` and `t4102:38`.
#[test]
fn test_apply_worktree_and_reports_matrix() {
    let repo = init_repo();
    assert!(
        run_libra_command(&["config", "user.name", "T"], repo.path())
            .status
            .success()
    );
    assert!(
        run_libra_command(&["config", "user.email", "t@e"], repo.path())
            .status
            .success()
    );
    fs::write(repo.path().join("a.txt"), "a\nb\n").unwrap();
    assert!(
        run_libra_command(&["add", "a.txt"], repo.path())
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path())
            .status
            .success()
    );

    let patch = "\
diff --git a/a.txt b/a.txt
index 422c2b7..de98044 100644
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,3 @@
 a
 b
+c
";
    let patch_path = repo.path().join("p.patch");
    fs::write(&patch_path, patch).unwrap();
    let p = |args: &[&str]| run_libra_command(args, repo.path());

    // A1: `--stat` reports without touching the file (t4106:15 port).
    let stat = p(&["apply", "--stat", patch_path.to_str().unwrap()]);
    assert_cli_success(&stat, "apply --stat");
    assert_eq!(
        String::from_utf8_lossy(&stat.stdout),
        " a.txt |    1 +\n 1 file changed, 1 insertion(+)\n"
    );
    assert_eq!(
        fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "a\nb\n"
    );

    // A2: `--numstat` and `--summary` (new-file creation).
    let numstat = p(&["apply", "--numstat", patch_path.to_str().unwrap()]);
    assert_eq!(String::from_utf8_lossy(&numstat.stdout), "1\t0\ta.txt\n");
    let new_patch = "diff --git a/n.txt b/n.txt\nnew file mode 100644\nindex 0000000..422c2b7\n--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1 @@\n+x\n";
    let new_path = repo.path().join("new.patch");
    fs::write(&new_path, new_patch).unwrap();
    let summary = p(&["apply", "--summary", new_path.to_str().unwrap()]);
    assert_eq!(
        String::from_utf8_lossy(&summary.stdout),
        " create mode 100644 n.txt\n"
    );
    assert!(
        !repo.path().join("n.txt").exists(),
        "report modes never write"
    );

    // A3: default apply writes only the worktree; the index keeps HEAD's blob.
    let applied = p(&["apply", patch_path.to_str().unwrap()]);
    assert_cli_success(&applied, "apply");
    assert_eq!(
        fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "a\nb\nc\n"
    );
    let status = p(&["status", "--short"]);
    let status_text = String::from_utf8_lossy(&status.stdout);
    assert!(status_text.contains(" M a.txt"), "{status_text}");

    // A4: `-R` reverses the same patch (t4102:38 port).
    let reversed = p(&["apply", "-R", patch_path.to_str().unwrap()]);
    assert_cli_success(&reversed, "apply -R");
    assert_eq!(
        fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "a\nb\n"
    );

    // A5: a multi-file patch whose second file does not apply writes nothing.
    let bad = "\
diff --git a/a.txt b/a.txt
index 422c2b7..de98044 100644
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,3 @@
 a
 b
+c
diff --git a/missing.txt b/missing.txt
--- a/missing.txt
+++ b/missing.txt
@@ -1 +1 @@
-nope
+yep
";
    let bad_path = repo.path().join("bad.patch");
    fs::write(&bad_path, bad).unwrap();
    let failed = p(&["apply", bad_path.to_str().unwrap()]);
    assert!(!failed.status.success(), "non-applying patch must fail");
    assert_eq!(
        fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "a\nb\n",
        "atomic failure must not write the applying file"
    );
}

/// LG-12 (M-AIDX B1–B5): `--index`, `--cached`, and ordered path filters.
#[test]
fn test_apply_index_cached_and_filters_matrix() {
    let make_repo = || -> (tempfile::TempDir, String) {
        let repo = init_repo();
        let p = repo.path();
        assert!(
            run_libra_command(&["config", "user.name", "T"], p)
                .status
                .success()
        );
        assert!(
            run_libra_command(&["config", "user.email", "t@e"], p)
                .status
                .success()
        );
        fs::write(p.join("a.txt"), "a\nb\n").unwrap();
        assert!(run_libra_command(&["add", "a.txt"], p).status.success());
        assert!(
            run_libra_command(&["commit", "-m", "base", "--no-verify"], p)
                .status
                .success()
        );
        let patch = p.join("p.patch");
        fs::write(
            &patch,
            "diff --git a/a.txt b/a.txt\nindex 422c2b7..de98044 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,3 @@\n a\n b\n+c\n",
        )
        .unwrap();
        (repo, patch.to_str().unwrap().to_string())
    };
    let p = |repo: &tempfile::TempDir, args: &[&str]| run_libra_command(args, repo.path());

    // B1: `--index` stages both the index and the working tree.
    let (repo, patch) = make_repo();
    let out = p(&repo, &["apply", "--index", &patch]);
    assert_cli_success(&out, "apply --index");
    assert_eq!(
        fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "a\nb\nc\n"
    );
    let status = String::from_utf8_lossy(&p(&repo, &["status", "--short"]).stdout).into_owned();
    assert!(status.contains("M  a.txt"), "{status}");

    // B2: a dirty worktree that does not match the index is refused before
    // any write.
    let (repo, patch) = make_repo();
    fs::write(repo.path().join("a.txt"), "dirty\n").unwrap();
    let mismatch = p(&repo, &["apply", "--index", &patch]);
    assert_eq!(mismatch.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&mismatch.stderr).contains("does not match index"),
        "{}",
        String::from_utf8_lossy(&mismatch.stderr)
    );
    assert_eq!(
        fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "dirty\n"
    );

    // B3: `--cached` updates only the index.
    let (repo, patch) = make_repo();
    let cached = p(&repo, &["apply", "--cached", &patch]);
    assert_cli_success(&cached, "apply --cached");
    assert_eq!(
        fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "a\nb\n"
    );
    let status = String::from_utf8_lossy(&p(&repo, &["status", "--short"]).stdout).into_owned();
    assert!(status.contains("MM a.txt"), "{status}");

    // B4: the first matching include/exclude rule decides.
    let (repo, patch) = make_repo();
    let excluded = p(&repo, &["apply", &patch, "--exclude=a.txt"]);
    assert_cli_success(&excluded, "apply --exclude");
    assert_eq!(
        fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "a\nb\n"
    );
    let included = p(&repo, &["apply", "--include=a.txt", "--exclude=*", &patch]);
    assert_cli_success(&included, "apply --include then --exclude");
    assert_eq!(
        fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "a\nb\nc\n"
    );
    let (repo, patch) = make_repo();
    let skipped = p(&repo, &["apply", "--exclude=*", "--include=a.txt", &patch]);
    assert_cli_success(&skipped, "apply --exclude then --include");
    assert_eq!(
        fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "a\nb\n"
    );

    // B5: new-file, delete, and mode-change patches with `--index`.
    let (repo, _) = make_repo();
    let new_patch = repo.path().join("new.patch");
    fs::write(
        &new_patch,
        "diff --git a/n.txt b/n.txt\nnew file mode 100644\nindex 0000000..422c2b7\n--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1 @@\n+x\n",
    )
    .unwrap();
    assert_cli_success(
        &p(&repo, &["apply", "--index", new_patch.to_str().unwrap()]),
        "apply --index new file",
    );
    assert!(repo.path().join("n.txt").exists());
    let status = String::from_utf8_lossy(&p(&repo, &["status", "--short"]).stdout).into_owned();
    assert!(status.contains("A  n.txt"), "{status}");
    // Commit the new file so its later deletion is observable against HEAD.
    assert!(
        run_libra_command(&["commit", "-m", "add n", "--no-verify"], repo.path())
            .status
            .success()
    );

    let delete_patch = repo.path().join("del.patch");
    fs::write(
        &delete_patch,
        "diff --git a/n.txt b/n.txt\ndeleted file mode 100644\nindex 422c2b7..0000000\n--- a/n.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n",
    )
    .unwrap();
    assert_cli_success(
        &p(&repo, &["apply", "--index", delete_patch.to_str().unwrap()]),
        "apply --index delete",
    );
    assert!(!repo.path().join("n.txt").exists());
    let status = String::from_utf8_lossy(&p(&repo, &["status", "--short"]).stdout).into_owned();
    assert!(status.contains("D  n.txt"), "{status}");

    let mode_patch = repo.path().join("mode.patch");
    fs::write(
        &mode_patch,
        "diff --git a/a.txt b/a.txt\nold mode 100644\nnew mode 100755\n",
    )
    .unwrap();
    assert_cli_success(
        &p(&repo, &["apply", "--index", mode_patch.to_str().unwrap()]),
        "apply --index mode change",
    );
    let summary = p(&repo, &["diff", "--cached", "--summary"]);
    assert!(
        String::from_utf8_lossy(&summary.stdout).contains("mode change 100644 => 100755 a.txt"),
        "{}",
        String::from_utf8_lossy(&summary.stdout)
    );
}
