//! Integration tests for per-worktree HEAD/index/HEAD-reflog isolation
//! (lore.md 2.1).
//!
//! Verifies: a linked worktree gets its own HEAD, index, and HEAD-reflog while
//! sharing the object store + shared branches; a commit/switch in one worktree
//! never moves another's HEAD; the same-branch guard; the linked-worktree
//! sequencer refusal; and `worktree remove` GCs the private rows. A
//! single-worktree repo is unchanged.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::fs;

use super::{assert_cli_success, run_libra_command, run_libra_command_with_stdin};

/// A committed repo (a.txt @ c1) with a `feature` branch. Returns its dir.
fn repo_with_feature() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init", "--vault=false"], p), "init");
    assert_cli_success(&run_libra_command(&["config", "user.name", "t"], p), "name");
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "t@t"], p),
        "email",
    );
    fs::write(p.join("a.txt"), "a\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    repo
}

fn abbrev_head(dir: &std::path::Path) -> String {
    String::from_utf8_lossy(&run_libra_command(&["rev-parse", "--abbrev-ref", "HEAD"], dir).stdout)
        .trim()
        .to_string()
}

#[test]
fn linked_worktree_has_isolated_head_and_index() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // The new worktree is DETACHED at c1 (its own HEAD), with a real .libra.
    assert_eq!(abbrev_head(&wt), "HEAD", "new worktree is detached");
    assert!(wt.join(".libra/commondir").exists(), "commondir pointer");
    assert!(
        wt.join(".libra/worktree_id").exists(),
        "private worktree id"
    );
    assert!(wt.join(".libra/index").exists(), "private index");
    // db/objects are NOT duplicated into the linked worktree.
    assert!(
        !wt.join(".libra/libra.db").exists(),
        "db is shared, not copied"
    );

    // Switch the worktree to `feature` and commit there.
    assert_cli_success(&run_libra_command(&["switch", "feature"], &wt), "wt switch");
    fs::write(wt.join("b.txt"), "b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], &wt), "wt add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2-in-wt", "--no-verify"], &wt),
        "wt commit",
    );

    // HEAD isolation: main is still on `main`; the wt commit did NOT move it.
    assert_eq!(
        abbrev_head(main),
        "main",
        "main HEAD unmoved by the wt commit"
    );
    assert_eq!(abbrev_head(&wt), "feature", "wt on its own branch");

    // Index isolation: b.txt is not staged/known in the main worktree.
    let main_status = run_libra_command(&["status", "--porcelain"], main);
    assert!(
        !String::from_utf8_lossy(&main_status.stdout).contains("b.txt"),
        "main index does not see the wt's staged file"
    );

    // HEAD-reflog isolation: the wt commit is not in main's HEAD reflog.
    let main_reflog = run_libra_command(&["reflog"], main);
    assert!(
        !String::from_utf8_lossy(&main_reflog.stdout).contains("c2-in-wt"),
        "main HEAD reflog is independent of the wt"
    );

    // Shared object store: main can resolve the branch tip the wt advanced.
    let feat = run_libra_command(&["log", "feature", "--oneline"], main);
    assert!(
        String::from_utf8_lossy(&feat.stdout).contains("c2-in-wt"),
        "objects + shared branch are visible from main"
    );
}

/// `worktree list --porcelain` reports each worktree's OWN HEAD (Part C
/// §C.3.3): the main worktree on a branch, the linked worktree detached at its
/// own commit — never one shared HEAD stamped onto both entries.
#[test]
fn porcelain_reports_per_worktree_head() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    let out = run_libra_command(&["worktree", "list", "--porcelain"], main);
    assert_cli_success(&out, "worktree list --porcelain");
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    // The main worktree entry carries a branch line...
    assert!(
        text.lines().any(|l| l == "branch refs/heads/main"),
        "main entry reports its branch: {text:?}"
    );
    // ...and the linked worktree entry is detached (its own HEAD), so a
    // `detached` line must appear too.
    assert!(
        text.lines().any(|l| l == "detached"),
        "linked worktree entry reports detached HEAD: {text:?}"
    );
    // Two distinct `worktree <path>` entries, each with its own HEAD line.
    let head_lines = text.lines().filter(|l| l.starts_with("HEAD ")).count();
    assert_eq!(
        head_lines, 2,
        "each worktree has its own HEAD line: {text:?}"
    );
}

/// Part C §C.4.1: a linked worktree whose `commondir` pointer is corrupt
/// (emptied) must FAIL CLOSED rather than silently treating its library-less
/// local gitdir as the shared storage (a "phantom repository" that routes
/// db/objects lookups at an empty dir).
#[test]
fn corrupt_commondir_fails_closed_not_phantom_repo() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Corrupt the commondir pointer (empty it) — the shared-storage link is now
    // unresolvable.
    fs::write(wt.join(".libra/commondir"), "").unwrap();

    let out = run_libra_command(&["status"], &wt);
    assert_ne!(
        out.status.code(),
        Some(0),
        "a corrupt commondir must fail closed, not operate on a phantom repo"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The failure happens at path resolution (repo-not-found), NOT by routing
    // the DB lookup at a phantom `<wt>/.libra/libra.db` — the pre-fix symptom.
    assert!(
        !stderr.contains(".libra/libra.db"),
        "must not route db lookups at the phantom local gitdir: {stderr}"
    );
    assert!(
        stderr.contains("LBR-REPO-001") || stderr.contains("not a libra repository"),
        "fails closed at repo resolution: {stderr}"
    );
}

/// Part C §C.5: `rev-parse --git-dir`/`--absolute-git-dir` return the LINKED
/// worktree's own local gitdir, and `--is-inside-git-dir` tests it — not the
/// shared common storage. Scripts locating the index/EDITMSG via `--git-dir`
/// must hit the per-worktree gitdir.
#[test]
fn rev_parse_git_dir_is_worktree_local() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    let git_dir =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "--git-dir"], &wt).stdout)
            .trim()
            .to_string();
    let wt_libra = wt.join(".libra");
    // The linked worktree's --git-dir must be ITS OWN .libra, not the main's.
    assert!(
        std::fs::canonicalize(&git_dir).ok() == std::fs::canonicalize(&wt_libra).ok(),
        "linked --git-dir should be the worktree-local gitdir: got {git_dir}, want {}",
        wt_libra.display()
    );
    assert!(
        !git_dir.contains(main.file_name().unwrap().to_str().unwrap()),
        "linked --git-dir must not point at the main worktree's storage: {git_dir}"
    );

    // --is-inside-git-dir from inside the linked .libra is true.
    let inside = String::from_utf8_lossy(
        &run_libra_command(&["rev-parse", "--is-inside-git-dir"], &wt_libra).stdout,
    )
    .trim()
    .to_string();
    assert_eq!(
        inside, "true",
        "cwd inside the linked .libra is inside GIT_DIR"
    );
}

#[test]
fn same_branch_is_refused_across_worktrees() {
    let repo = repo_with_feature();
    let main = repo.path();
    // main checks out `feature`.
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], main),
        "main->feature",
    );
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    // The wt cannot switch to `feature` (checked out in main).
    let refused = run_libra_command(&["switch", "feature"], &wt);
    assert_ne!(refused.status.code(), Some(0), "same-branch switch refused");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("already checked out"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    // But it can switch to a free branch.
    assert_cli_success(
        &run_libra_command(&["switch", "main"], &wt),
        "free branch ok",
    );
}

/// Part C W0 (§C.11 transition guards): the states whose stores are still
/// repository-global — the stash stack, the dirty cache, the layer/sparse
/// tables, and the composite `pull` (shared merge/rebase state) — must fail
/// closed in a linked worktree until W1/W2 make them worktree-scoped. The
/// guard fires before any side effect, so no remote/network is needed.
/// (`fetch` was un-guarded in W1 once `FETCH_HEAD` became worktree-local — see
/// `fetch_uses_worktree_local_fetch_head`.)
#[test]
fn repository_global_state_commands_refused_in_linked_worktree() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    let cases: &[&[&str]] = &[
        &["stash", "list"],
        &["layer", "list"],
        &["sparse-view", "status"],
        &["dirty", "--list"],
        &["pull"],
    ];
    for argv in cases {
        let out = run_libra_command(argv, &wt);
        assert_ne!(
            out.status.code(),
            Some(0),
            "{argv:?} must fail closed in a linked worktree"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("linked worktree"),
            "{argv:?} should fail with the linked-worktree guard, got: {stderr}"
        );
    }

    // The SAME commands succeed in the main worktree (guard is main-only).
    assert_cli_success(
        &run_libra_command(&["stash", "list"], main),
        "stash list works in main",
    );
    assert_cli_success(
        &run_libra_command(&["layer", "list"], main),
        "layer list works in main",
    );
}

/// Part C W0 (§C.11 line 1507a): plain `status` works in a linked worktree
/// (it never consults the shared dirty cache), but the cache-semantic modes
/// `--scan`/`--cached`/`--check-dirty` fail closed until W1 scopes the cache.
#[test]
fn status_cache_modes_refused_in_linked_but_plain_status_works() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Plain status must succeed in the linked worktree.
    assert_cli_success(
        &run_libra_command(&["status"], &wt),
        "plain status works in a linked worktree",
    );
    assert_cli_success(
        &run_libra_command(&["status", "--porcelain"], &wt),
        "porcelain status works in a linked worktree",
    );

    // The dirty-cache modes fail closed.
    for mode in [
        vec!["status", "--scan"],
        vec!["status", "--cached"],
        vec!["status", "--check-dirty"],
    ] {
        let out = run_libra_command(&mode, &wt);
        assert_ne!(
            out.status.code(),
            Some(0),
            "{mode:?} must fail closed in a linked worktree"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("linked worktree"),
            "{mode:?} should hit the linked-worktree guard: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Part C W0 (§C.11): destructive branch writers (`branch -d`, `branch -m`,
/// `branch reset`) refuse to touch a branch that is checked out in ANOTHER
/// worktree — otherwise that worktree's HEAD would dangle or its working tree
/// would silently diverge (Git parity).
#[test]
fn branch_writers_refuse_branch_checked_out_in_another_worktree() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    // The linked worktree checks out `feature`.
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );

    // From the main worktree, deleting/renaming/resetting `feature` is refused.
    for argv in [
        vec!["branch", "-D", "feature"],
        vec!["branch", "-m", "feature", "feature2"],
        vec!["branch", "reset", "feature", "main"],
    ] {
        let out = run_libra_command(&argv, main);
        assert_ne!(
            out.status.code(),
            Some(0),
            "{argv:?} must be refused while feature is checked out elsewhere"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("checked out"),
            "{argv:?} should name the other worktree: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // A branch checked out NOWHERE else is still freely mutable.
    assert_cli_success(
        &run_libra_command(&["branch", "spare"], main),
        "create spare branch",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "-D", "spare"], main),
        "delete a free branch works",
    );
}

/// Part C W0 (§C.11): `update-ref` refuses to move or delete a branch that is
/// checked out in another worktree, but may still update this worktree's own
/// current branch.
#[test]
fn update_ref_refuses_branch_checked_out_elsewhere() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );
    // main HEAD commit, to use as an update target.
    let main_oid = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], main).stdout)
        .trim()
        .to_string();

    // From main, update-ref on `feature` (checked out in wt) is refused.
    let refused = run_libra_command(&["update-ref", "refs/heads/feature", &main_oid], main);
    assert_ne!(
        refused.status.code(),
        Some(0),
        "update-ref on wt branch refused"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("checked out"),
        "names the other worktree: {}",
        String::from_utf8_lossy(&refused.stderr)
    );

    // update-ref on main's OWN current branch is still allowed.
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/main", &main_oid], main),
        "update-ref on own branch works",
    );
}

/// Part C W0 (§C.11): `symbolic-ref HEAD refs/heads/<b>` refuses to point HEAD
/// at a branch already checked out in another worktree (would create a
/// duplicate checkout).
#[test]
fn symbolic_ref_refuses_branch_checked_out_elsewhere() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );

    // From main (on `main`), pointing HEAD at `feature` is refused.
    let refused = run_libra_command(&["symbolic-ref", "HEAD", "refs/heads/feature"], main);
    assert_ne!(
        refused.status.code(),
        Some(0),
        "symbolic-ref to wt branch refused"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("checked out"),
        "names the collision: {}",
        String::from_utf8_lossy(&refused.stderr)
    );

    // Re-pointing at main's own current branch is allowed.
    assert_cli_success(
        &run_libra_command(&["symbolic-ref", "HEAD", "refs/heads/main"], main),
        "symbolic-ref to own branch works",
    );
}

/// Part C W0 (§C.11, intentionally-different from Git): `--ignore-other-worktrees`
/// does NOT bypass the same-branch guard in a multi-worktree repo. Libra never
/// allows the same branch checked out in two worktrees.
#[test]
fn ignore_other_worktrees_flag_cannot_bypass_in_multi_worktree() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    // main is on `main`; the linked worktree takes `feature`.
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );

    // From main, `checkout --ignore-other-worktrees feature` is STILL refused.
    let co = run_libra_command(&["checkout", "--ignore-other-worktrees", "feature"], main);
    assert_ne!(co.status.code(), Some(0), "checkout flag cannot bypass");
    let co_err = String::from_utf8_lossy(&co.stderr);
    assert!(
        co_err.contains("already checked out") && co_err.contains("ignore-other-worktrees"),
        "error explains the flag is not honored: {co_err}"
    );

    // Plain `switch feature` is also refused (the same-branch guard).
    let sw = run_libra_command(&["switch", "feature"], main);
    assert_ne!(sw.status.code(), Some(0), "switch to wt branch refused");
    assert!(
        String::from_utf8_lossy(&sw.stderr).contains("already checked out"),
        "switch refused: {}",
        String::from_utf8_lossy(&sw.stderr)
    );
}

/// Part C W0 (§C.11): `reflog expire --updateref` moves a branch tip; it
/// refuses a branch checked out in another worktree (before any write).
#[test]
fn reflog_expire_updateref_refuses_branch_checked_out_elsewhere() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );
    // Commit on `feature` in the linked worktree so it has a (shared) branch
    // reflog for `reflog expire` to resolve — otherwise expire errors with
    // "reflog not found" before the cross-worktree guard runs.
    fs::write(wt.join("f.txt"), "f\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], &wt), "wt add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "on-feature", "--no-verify"], &wt),
        "wt commit on feature",
    );

    // From main, `reflog expire --updateref feature` is refused.
    let out = run_libra_command(
        &["reflog", "expire", "--updateref", "--expire=all", "feature"],
        main,
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "reflog expire --updateref on a wt branch refused"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("checked out"),
        "names the collision: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `--updateref` on main's own branch is allowed (no other-worktree conflict).
    assert_cli_success(
        &run_libra_command(&["reflog", "expire", "--updateref", "main"], main),
        "reflog expire --updateref on own branch works",
    );
}

/// Part C W0 (§C.11): `fast-import`'s batch flush rewrites shared branch refs;
/// it refuses (before the transaction) to import into a branch checked out in
/// another worktree.
#[test]
fn fast_import_refuses_branch_checked_out_elsewhere() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );

    // From main, import a commit onto `feature` (checked out in wt) — refused.
    let stream = "blob\nmark :1\ndata 6\nhello\n\n\
        commit refs/heads/feature\nmark :2\n\
        committer Tester <t@example.com> 1700000000 +0000\ndata 8\nimported\n\n\
        M 100644 :1 g.txt\n\ndone\n";
    let out = run_libra_command_with_stdin(&["fast-import", "--quiet"], main, stream);
    assert_ne!(
        out.status.code(),
        Some(0),
        "fast-import into a wt branch must be refused"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("checked out"),
        "names the collision: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Part C W0 release gate (§C.11): GC's reachability walk reads only the
/// CURRENT worktree's index, so a blob staged (but not committed) in a LINKED
/// worktree is not yet a root. Until the typed `GcObjectSource` inventory
/// lands, `maintenance run --task gc` must skip the loose-object prune in a
/// multi-worktree repository rather than delete objects it cannot see.
#[test]
fn gc_skips_prune_in_multi_worktree_repo() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Stage a blob ONLY in the linked worktree (never committed). Its object is
    // reachable only from that worktree's private index.
    fs::write(wt.join("staged-only.txt"), "precious\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "staged-only.txt"], &wt),
        "stage blob in wt",
    );
    let oid = String::from_utf8_lossy(
        &run_libra_command(&["hash-object", "staged-only.txt"], &wt).stdout,
    )
    .trim()
    .to_string();
    assert!(!oid.is_empty(), "hashed the staged blob");

    // GC from the MAIN worktree must skip the prune (not delete the blob).
    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], main);
    assert_cli_success(&gc, "maintenance gc");
    let text = String::from_utf8_lossy(&gc.stdout) + String::from_utf8_lossy(&gc.stderr);
    assert!(
        text.contains("linked worktree"),
        "gc should report skipping the prune for linked worktrees: {text}"
    );

    // The staged-only blob must still be readable (no data loss).
    let cat = run_libra_command(&["cat-file", "-p", &oid], main);
    assert_cli_success(&cat, "staged-only blob survives gc");
    assert!(
        String::from_utf8_lossy(&cat.stdout).contains("precious"),
        "the linked worktree's staged blob was pruned by gc"
    );

    // Part C §C.9: every worktree's private index is a reachability root, so
    // `fsck --unreachable` must NOT report the linked worktree's staged blob as
    // garbage (fsck only reports, but a false "unreachable" invites a manual
    // delete).
    let fsck = run_libra_command(&["fsck", "--unreachable"], main);
    let fsck_text = String::from_utf8_lossy(&fsck.stdout) + String::from_utf8_lossy(&fsck.stderr);
    assert!(
        !fsck_text.contains(&oid),
        "the linked worktree's staged blob must not be reported unreachable: {fsck_text}"
    );

    // The incremental-repack task has the same gap (it rebuilds one pack from
    // the reachable set and deletes the old packs), so it must skip too.
    let repack = run_libra_command(
        &["maintenance", "run", "--task", "incremental-repack"],
        main,
    );
    assert_cli_success(&repack, "maintenance incremental-repack");
    let repack_text =
        String::from_utf8_lossy(&repack.stdout) + String::from_utf8_lossy(&repack.stderr);
    assert!(
        repack_text.contains("linked worktree"),
        "incremental-repack should skip in a multi-worktree repo: {repack_text}"
    );
}

/// Part C §C.4.3: transient editor buffers live in each worktree's OWN gitdir.
/// `tag` is a Repository-scope command allowed in ANY worktree, so a shared
/// `TAG_EDITMSG` would let two worktrees composing a message concurrently
/// truncate each other's buffer.
#[test]
fn editor_buffers_are_worktree_local_not_shared() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Drive the editor via a script that records WHICH file it was handed, then
    // writes a message. Each worktree must be handed its own gitdir's buffer.
    let probe = parent.path().join("probe.sh");
    let seen = parent.path().join("seen.txt");
    fs::write(
        &probe,
        format!(
            "#!/bin/sh\necho \"$1\" >> {}\necho 'the tag message' > \"$1\"\n",
            seen.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).unwrap();
    }

    // `-e` is Libra's editor-driven annotated-tag flow (there is no `-a`), and
    // `GIT_EDITOR` is the highest-precedence explicit editor (runs without a
    // TTY). The probe records which TAG_EDITMSG path it was handed.
    for (dir, tag) in [(main, "t-main"), (wt.as_path(), "t-wt")] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
            .args(["tag", "-e", tag])
            .current_dir(dir)
            .env("GIT_EDITOR", probe.to_str().unwrap())
            .output()
            .expect("run libra tag -e");
        assert!(
            out.status.success(),
            "tag -e in {dir:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let seen_text = fs::read_to_string(&seen).unwrap_or_default();
    let paths: Vec<&str> = seen_text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        paths.len(),
        2,
        "the editor ran once per worktree: {paths:?}"
    );
    assert_ne!(
        paths[0], paths[1],
        "each worktree must get its OWN TAG_EDITMSG, not a shared one: {paths:?}"
    );
    // The linked worktree's buffer lives under ITS gitdir. Compare against the
    // canonicalized worktree path rather than a raw prefix, which `/tmp` →
    // `/private/tmp` symlink resolution would otherwise break.
    let wt_canon = wt.canonicalize().expect("canonicalize wt");
    let expected = wt_canon.join(".libra").join("TAG_EDITMSG");
    assert_eq!(
        std::path::Path::new(paths[1]),
        expected,
        "the linked worktree's buffer lives in its own gitdir: {paths:?}"
    );
}

/// Part C W1 (§C.4.2): `fetch` is no longer refused in a linked worktree, and
/// its `FETCH_HEAD` is written to that worktree's OWN gitdir — a fetch there
/// never overwrites the main worktree's `FETCH_HEAD`.
#[test]
fn fetch_uses_worktree_local_fetch_head() {
    // An upstream repo to fetch FROM (a plain local path remote).
    let upstream = repo_with_feature();
    let up = upstream.path();

    // A clone that will host the linked worktree.
    let clone_parent = tempfile::tempdir().expect("clone parent");
    let clone_dir = clone_parent.path().join("clone");
    assert_cli_success(
        &run_libra_command(
            &["clone", up.to_str().unwrap(), clone_dir.to_str().unwrap()],
            clone_parent.path(),
        ),
        "clone upstream",
    );
    let main = clone_dir.as_path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Fetch from the LINKED worktree — must NOT hit the linked-worktree guard.
    let out = run_libra_command(&["fetch", "origin"], &wt);
    assert!(
        out.status.success(),
        "fetch from a linked worktree should work: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("linked worktree"),
        "fetch must no longer be refused in a linked worktree"
    );

    // The FETCH_HEAD it wrote lives in the LINKED worktree's gitdir, not main's.
    assert!(
        wt.join(".libra/FETCH_HEAD").exists(),
        "the linked worktree's fetch wrote its own FETCH_HEAD"
    );
    assert!(
        !main.join(".libra/FETCH_HEAD").exists(),
        "the linked worktree's fetch must not write the main worktree's FETCH_HEAD"
    );
}

/// Part C W1 (§C.4.2): cherry-pick is now allowed in a linked worktree, and
/// two worktrees can each cherry-pick onto their OWN branch without their
/// sequencer state or `CHERRY_PICK_MSG` colliding.
#[test]
fn cherry_pick_runs_concurrently_in_worktrees() {
    // main repo on `main`; make a `pick` commit on a side branch to cherry-pick.
    let repo = repo_with_feature();
    let main = repo.path();
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "src"], main),
        "branch src",
    );
    fs::write(main.join("p.txt"), "picked\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "p.txt"], main), "add p");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "the-pick", "--no-verify"], main),
        "commit pick",
    );
    let pick = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], main).stdout)
        .trim()
        .to_string();
    assert_cli_success(
        &run_libra_command(&["switch", "main"], main),
        "back to main",
    );

    // A linked worktree checked out on `feature`.
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );

    // Cherry-pick the same commit in BOTH worktrees. Neither must be refused,
    // and each lands on its own branch.
    let co_wt = run_libra_command(&["cherry-pick", &pick], &wt);
    assert!(
        co_wt.status.success(),
        "cherry-pick in the linked worktree should work: {}",
        String::from_utf8_lossy(&co_wt.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&co_wt.stderr).contains("linked worktree"),
        "cherry-pick must no longer be refused in a linked worktree"
    );
    let co_main = run_libra_command(&["cherry-pick", &pick], main);
    assert!(
        co_main.status.success(),
        "cherry-pick in main should work: {}",
        String::from_utf8_lossy(&co_main.stderr)
    );

    // Each worktree's branch now carries the picked file; HEADs are independent.
    assert!(main.join("p.txt").exists(), "main picked p.txt onto `main`");
    assert!(wt.join("p.txt").exists(), "wt picked p.txt onto `feature`");
    assert_eq!(abbrev_head(main), "main", "main still on its branch");
    assert_eq!(abbrev_head(&wt), "feature", "wt still on its branch");
}

/// Part C W1 (§C.4.2): `am` is allowed in a linked worktree — its state is the
/// worktree-scoped `sequence_state` row, and it applies onto that worktree's
/// own branch.
#[test]
fn am_applies_in_linked_worktree() {
    let repo = repo_with_feature();
    let main = repo.path();

    // Build a one-patch series on a side branch, then format-patch it.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "src"], main),
        "branch src",
    );
    fs::write(main.join("mailed.txt"), "from a patch\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "mailed.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "mailed change", "--no-verify"], main),
        "commit",
    );
    let patch_dir = repo.path().join("patches");
    assert_cli_success(
        &run_libra_command(
            &[
                "format-patch",
                "-o",
                patch_dir.to_str().unwrap(),
                "main..HEAD",
            ],
            main,
        ),
        "format-patch",
    );
    let patch = fs::read_dir(&patch_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|x| x == "patch"))
        .expect("a .patch file");
    assert_cli_success(
        &run_libra_command(&["switch", "main"], main),
        "back to main",
    );

    // A linked worktree on `feature` applies the patch via `am`.
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );

    let out = run_libra_command(&["am", patch.to_str().unwrap()], &wt);
    assert!(
        out.status.success(),
        "am in a linked worktree should work: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("linked worktree"),
        "am must no longer be refused in a linked worktree"
    );
    // The patch landed on `feature`, in the linked worktree only.
    assert!(wt.join("mailed.txt").exists(), "am applied onto feature");
    assert!(
        !main.join("mailed.txt").exists(),
        "main worktree is untouched by the linked am"
    );
    assert_eq!(abbrev_head(&wt), "feature", "wt still on its branch");
}

/// Part C W1 (§C.4.2): `revert` is allowed in a linked worktree — its
/// `revert-state.json` and `REVERT_EDITMSG` live in that worktree's own gitdir,
/// and it replays onto that worktree's own branch.
#[test]
fn revert_runs_in_linked_worktree() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    // Give the linked worktree its own branch with a commit to revert.
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );
    fs::write(wt.join("r.txt"), "to be reverted\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "r.txt"], &wt), "wt add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add r.txt", "--no-verify"], &wt),
        "wt commit",
    );

    // Revert that commit from the linked worktree — must not be refused.
    let out = run_libra_command(&["revert", "HEAD", "--no-edit"], &wt);
    assert!(
        out.status.success(),
        "revert in a linked worktree should work: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("linked worktree"),
        "revert must no longer be refused in a linked worktree"
    );
    // The revert removed r.txt in the linked worktree; main never had it.
    assert!(
        !wt.join("r.txt").exists(),
        "the revert undid the change in the linked worktree"
    );
    assert!(!main.join("r.txt").exists(), "main is untouched");
    assert_eq!(abbrev_head(&wt), "feature", "wt still on its branch");
}

/// Part C W1 (§C.4.2/§C.4.3): `merge` is allowed in a linked worktree — its
/// state (`merge-state.json`/`merge-autostash.json`) lives in that worktree's
/// gitdir, and it merges into that worktree's own branch.
#[test]
fn merge_runs_in_linked_worktree() {
    let repo = repo_with_feature();
    let main = repo.path();

    // Advance `main` with a commit that `feature` does not have.
    fs::write(main.join("m.txt"), "on main\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "m.txt"], main), "add m");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main change", "--no-verify"], main),
        "commit main",
    );

    // A linked worktree on `feature`, with its own divergent commit.
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );
    fs::write(wt.join("f.txt"), "on feature\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], &wt), "wt add f");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feature change", "--no-verify"], &wt),
        "wt commit",
    );

    // Merge `main` into `feature` FROM the linked worktree (no conflict — the
    // two touched different files) — must not be refused.
    let out = run_libra_command(&["merge", "main", "--no-edit"], &wt);
    assert!(
        out.status.success(),
        "merge in a linked worktree should work: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("linked worktree"),
        "merge must no longer be refused in a linked worktree"
    );
    // The merge brought main's file into feature; main is untouched.
    assert!(wt.join("m.txt").exists(), "merge pulled m.txt into feature");
    assert!(wt.join("f.txt").exists(), "feature keeps its own file");
    assert!(
        !main.join("f.txt").exists(),
        "the main worktree is untouched by the linked merge"
    );
    assert_eq!(abbrev_head(&wt), "feature", "wt still on its branch");
}

#[test]
fn sequencer_ops_refused_in_linked_worktree() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    // rebase is still refused in a linked worktree (its state is
    // repository-global: lazy-DDL `rebase_state` + common sidecars).
    // cherry-pick/am/revert/merge were lifted in W1 once their state became
    // fully worktree-scoped.
    let rebase = run_libra_command(&["rebase", "feature"], &wt);
    assert_ne!(
        rebase.status.code(),
        Some(0),
        "rebase is still refused in a linked worktree"
    );
    assert!(
        String::from_utf8_lossy(&rebase.stderr).contains("linked worktree"),
        "rebase: {}",
        String::from_utf8_lossy(&rebase.stderr)
    );
    // rebase still works in the main worktree.
    assert_cli_success(
        &run_libra_command(&["rebase", "feature"], main),
        "rebase in main",
    );
}

#[test]
fn remove_gcs_private_head_rows() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let id = fs::read_to_string(wt.join(".libra/worktree_id"))
        .unwrap()
        .trim()
        .to_string();
    assert!(!id.is_empty(), "worktree id present");

    // Remove the worktree (and its dir); its private HEAD row is GC'd.
    assert_cli_success(
        &run_libra_command(
            &["worktree", "remove", wt.to_str().unwrap(), "--delete-dir"],
            main,
        ),
        "worktree remove",
    );
    // Re-adding at the SAME path (same id) starts clean — detached at HEAD,
    // not inheriting a stale HEAD row.
    fs::create_dir_all(&wt).ok();
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "re-add worktree",
    );
    assert_eq!(
        abbrev_head(&wt),
        "HEAD",
        "re-added worktree is cleanly detached"
    );
}
