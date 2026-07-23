//! Integration tests for per-worktree HEAD/index/HEAD-reflog isolation
//! (lore.md 2.1).
//!
//! Verifies: a linked worktree gets its own HEAD, index, and HEAD-reflog while
//! sharing the object store + shared branches; a commit/switch in one worktree
//! never moves another's HEAD; the same-branch guard; per-worktree
//! sequencer state (all six ops run in linked worktrees); and
//! `worktree remove` GCs the private rows. A
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
/// repository-global — the stash stack, the dirty cache, and the layer/sparse
/// tables — must fail closed in a linked worktree until W1/W2 make them
/// worktree-scoped. The guard fires before any side effect, so no
/// remote/network is needed. (`fetch` was un-guarded in W1 once `FETCH_HEAD`
/// became worktree-local — see `fetch_uses_worktree_local_fetch_head`; `pull`
/// in merge mode was un-guarded once merge state was scoped — only its
/// `--rebase` mode still refuses, asserted below on a branch-attached
/// worktree since the mode is resolved after HEAD.)
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

    // Part C W1 final lift: `pull --rebase` itself is no longer refused in a
    // linked worktree (rebase state is scoped). Only the `--autostash` combo
    // stays guarded — its legacy wrap uses the repository-global stash stack.
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );
    let rebase_pull = run_libra_command(&["pull", "--rebase"], &wt);
    assert!(
        !String::from_utf8_lossy(&rebase_pull.stderr).contains("linked worktree"),
        "pull --rebase must not hit the linked-worktree guard anymore: {}",
        String::from_utf8_lossy(&rebase_pull.stderr)
    );
    let autostash_pull = run_libra_command(&["pull", "--rebase", "--autostash"], &wt);
    assert_ne!(
        autostash_pull.status.code(),
        Some(0),
        "pull --rebase --autostash must fail closed in a linked worktree"
    );
    assert!(
        String::from_utf8_lossy(&autostash_pull.stderr).contains("linked worktree"),
        "the autostash combo fails with the linked-worktree guard: {}",
        String::from_utf8_lossy(&autostash_pull.stderr)
    );

    let cases: &[&[&str]] = &[
        &["stash", "list"],
        &["layer", "list"],
        &["sparse-view", "status"],
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
    // W1 §C.4.1.1: the dirty cache is worktree-scoped now — `dirty` runs in
    // a linked worktree against its own rows.
    assert_cli_success(
        &run_libra_command(&["dirty", "--list"], &wt),
        "dirty --list runs in a linked worktree since W1",
    );
}

/// W1 §C.4.1.1: plain `status` and ALL cache-semantic modes run in a
/// linked worktree — the dirty cache is scoped per worktree. (Formerly the
/// `--scan`/`--cached`/`--check-dirty` fail closed until W1 scopes the cache.
#[test]
fn status_cache_modes_run_in_linked_worktree() {
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

    // W1 §C.4.1.1: the cache-semantic modes run in a linked worktree against
    // their own scoped rows.
    for mode in [
        vec!["status", "--scan"],
        vec!["status", "--cached"],
        vec!["status", "--check-dirty"],
    ] {
        let out = run_libra_command(&mode, &wt);
        assert_cli_success(&out, "cache-semantic mode runs in a linked worktree");
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

/// Part C W1 (§C.4.4): `pull` in MERGE mode runs in a linked worktree — its
/// fetch resolves worktree-local paths and its merge integrates on that
/// worktree's own scoped HEAD/index/tree; the main worktree is untouched.
/// (The rebase mode stays refused — see
/// `repository_global_state_commands_refused_in_linked_worktree`. Note:
/// libra's pull-internal fetch does not write a FETCH_HEAD at all — only the
/// public `fetch` command does — so the assertion here is only that MAIN's
/// gitdir gains none.)
#[test]
fn pull_merges_in_linked_worktree() {
    // An upstream repo to pull FROM (a plain local path remote).
    let upstream = repo_with_feature();
    let up = upstream.path();

    // A clone hosting the linked worktree.
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
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );

    // Advance the UPSTREAM's `feature` so the pull has something to merge.
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], up),
        "upstream switch feature",
    );
    fs::write(up.join("b2.txt"), "b2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b2.txt"], up), "upstream add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2-upstream", "--no-verify"], up),
        "upstream commit",
    );

    let main_head_before = abbrev_head(main);

    // Pull (merge mode) in the LINKED worktree — must not be refused.
    let pull = run_libra_command(&["pull", "origin", "feature"], &wt);
    assert!(
        pull.status.success(),
        "pull (merge mode) in a linked worktree should work: {}",
        String::from_utf8_lossy(&pull.stderr)
    );

    // The merge landed in the LINKED worktree only.
    assert!(wt.join("b2.txt").exists(), "pulled file present in the wt");
    assert_eq!(abbrev_head(&wt), "feature", "wt still on its branch");
    assert!(
        !main.join("b2.txt").exists(),
        "the pull integrated into the LINKED worktree, not main"
    );
    assert_eq!(abbrev_head(main), main_head_before, "main HEAD untouched");
    assert!(
        !main.join(".libra/FETCH_HEAD").exists(),
        "the linked worktree's pull must not write into main's gitdir"
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

fn head_sha(dir: &std::path::Path) -> String {
    String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], dir).stdout)
        .trim()
        .to_string()
}

/// In `dir`: switch to `feature` and add commits c2 (+b2.txt) and c3 (+b3.txt)
/// on top of c1, returning `(c1_sha, c2_sha, c3_sha)` — a bisect range.
fn grow_feature_history(dir: &std::path::Path) -> (String, String, String) {
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], dir),
        "switch feature",
    );
    let c1 = head_sha(dir);
    let mut shas = Vec::new();
    for n in [2, 3] {
        fs::write(dir.join(format!("b{n}.txt")), format!("b{n}\n")).unwrap();
        assert_cli_success(
            &run_libra_command(&["add", &format!("b{n}.txt")], dir),
            "add",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", &format!("c{n}"), "--no-verify"], dir),
            "commit",
        );
        shas.push(head_sha(dir));
    }
    (c1, shas[0].clone(), shas[1].clone())
}

/// Part C W1 (§C.4.2): `bisect` is allowed in a linked worktree — its
/// `bisect_state` row is keyed by `worktree_id`, its checkouts materialize into
/// that worktree's OWN working directory AND index (no phantom `status`
/// modifications), and `reset` restores only that worktree's HEAD. The main
/// worktree's HEAD and files stay untouched throughout.
#[test]
fn bisect_runs_in_linked_worktree() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let (c1, c2, c3) = grow_feature_history(&wt);
    let main_head_before = abbrev_head(main);

    // Start a bisect in the LINKED worktree — must not be refused.
    let start = run_libra_command(&["bisect", "start", "HEAD", "--good", &c1], &wt);
    assert!(
        start.status.success(),
        "bisect start in a linked worktree should work: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    // The bisect checkout detached the LINKED worktree's HEAD at a candidate
    // in (c1..c3] and materialized THAT candidate's files into the linked
    // worktree — with the index rewritten in step, so `status` is clean.
    assert_eq!(abbrev_head(&wt), "HEAD", "wt HEAD detached at bisect point");
    let candidate = head_sha(&wt);
    assert!(
        candidate == c2 || candidate == c3,
        "wt detached at a bisect candidate (got {candidate})"
    );
    assert!(wt.join("b2.txt").exists(), "candidate tree materialized");
    assert_eq!(
        wt.join("b3.txt").exists(),
        candidate == c3,
        "b3.txt present exactly when the candidate is c3"
    );
    let wt_status = run_libra_command(&["status", "--porcelain"], &wt);
    assert_eq!(
        String::from_utf8_lossy(&wt_status.stdout).trim(),
        "",
        "bisect checkout rewrites the per-worktree index in step with the \
         worktree — no phantom modifications"
    );

    // The MAIN worktree is untouched: HEAD, files, and status.
    assert_eq!(
        abbrev_head(main),
        main_head_before,
        "main HEAD untouched by the linked worktree's bisect"
    );
    assert!(
        !main.join("b2.txt").exists() && !main.join("b3.txt").exists(),
        "the bisect checkout materialized into the LINKED worktree, not main"
    );
    assert!(main.join("a.txt").exists(), "main's own files survive");

    // Reset ends the session and restores the linked worktree's branch + tree.
    assert_cli_success(
        &run_libra_command(&["bisect", "reset"], &wt),
        "bisect reset",
    );
    assert_eq!(abbrev_head(&wt), "feature", "wt restored to its branch");
    assert!(
        wt.join("b2.txt").exists() && wt.join("b3.txt").exists(),
        "wt tree restored to the feature tip"
    );
    assert_eq!(abbrev_head(main), main_head_before, "main still untouched");
}

/// Part C W1 (§C.4.2): worktree ids are deterministic (hash of the canonical
/// path), so `worktree remove` must GC the removed worktree's scoped
/// `bisect_state` row — otherwise a worktree re-added at the SAME path would
/// silently inherit (and resume) the dead bisect session.
#[test]
fn readded_worktree_does_not_inherit_bisect_session() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let (c1, _c2, _c3) = grow_feature_history(&wt);
    assert_cli_success(
        &run_libra_command(&["bisect", "start", "HEAD", "--good", &c1], &wt),
        "bisect start",
    );

    // Remove the worktree MID-BISECT, clear its directory, and re-add at the
    // same path (same deterministic worktree id).
    assert_cli_success(
        &run_libra_command(&["worktree", "remove", wt.to_str().unwrap()], main),
        "worktree remove",
    );
    fs::remove_dir_all(&wt).expect("clear removed worktree dir");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree re-add",
    );

    // The fresh worktree must NOT see the dead session: a new bisect starts
    // cleanly instead of being refused (or worse, resumed).
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "re-added wt switch feature",
    );
    let restart = run_libra_command(&["bisect", "start", "HEAD", "--good", &c1], &wt);
    assert!(
        restart.status.success(),
        "re-added worktree starts a FRESH bisect (stale row must be GC'd): {}",
        String::from_utf8_lossy(&restart.stderr)
    );
}

/// Part C W1 (§C.4.2): while a worktree bisects (detached), its original
/// branch looks free and another worktree may legitimately check it out.
/// `bisect reset` must then NOT re-attach that branch (one branch on two
/// HEADs is the state `switch`/`checkout` categorically refuse) — it warns
/// and ends the session detached at the original tip instead.
#[test]
fn bisect_reset_does_not_steal_branch_attached_elsewhere() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let (c1, _c2, c3) = grow_feature_history(&wt);
    assert_cli_success(
        &run_libra_command(&["bisect", "start", "HEAD", "--good", &c1], &wt),
        "bisect start",
    );

    // The bisecting worktree is detached, so `feature` is free: MAIN takes it.
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], main),
        "main takes the branch while wt is detached",
    );

    // Reset must not create a second attachment of `feature`.
    let reset = run_libra_command(&["bisect", "reset"], &wt);
    assert!(
        reset.status.success(),
        "bisect reset still succeeds: {}",
        String::from_utf8_lossy(&reset.stderr)
    );
    assert!(
        String::from_utf8_lossy(&reset.stderr).contains("not re-attaching branch 'feature'"),
        "reset warns that the branch is taken: {}",
        String::from_utf8_lossy(&reset.stderr)
    );
    assert_eq!(
        abbrev_head(&wt),
        "HEAD",
        "wt ends DETACHED instead of double-attaching the branch"
    );
    assert_eq!(head_sha(&wt), c3, "wt detached at the original tip");
    assert_eq!(abbrev_head(main), "feature", "main keeps the branch");
}

/// plan-20260714 §C.9 item 10: an in-progress sequencer/rebase/bisect row's
/// OID columns are GC reachability roots — across EVERY worktree scope, not
/// just the scope gc runs from. A commit anchored ONLY by a (foreign-scope)
/// `rebase_state` row must survive `gc`; once the row is gone, the same
/// commit is pruned (proving the positive case was not vacuous).
#[test]
fn sequencer_state_rows_are_gc_roots_across_scopes() {
    // Environment gate: this fixture shells out to `sqlite3`; print skipped
    // instead of hard-failing where the tool is absent (repo test convention).
    if std::process::Command::new("sqlite3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipped (sqlite3 not installed)");
        return;
    }

    let repo = repo_with_feature();
    let main = repo.path();

    // A commit reachable from nothing but the state row we are about to
    // plant: commit on a temp branch, delete the branch, purge the reflog.
    assert_cli_success(&run_libra_command(&["switch", "-c", "tmp"], main), "tmp");
    fs::write(main.join("t.txt"), "t\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "t.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "tmp-commit", "--no-verify"], main),
        "commit",
    );
    let oid = head_sha(main);
    assert_cli_success(&run_libra_command(&["switch", "main"], main), "back");
    assert_cli_success(
        &run_libra_command(&["branch", "-D", "tmp"], main),
        "drop tmp",
    );
    let sqlite = |sql: &str| {
        let out = std::process::Command::new("/usr/bin/sqlite3")
            .arg(main.join(".libra/libra.db"))
            .arg(sql)
            .output()
            .expect("run sqlite3");
        assert!(
            out.status.success(),
            "sqlite3 {sql}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    sqlite("DELETE FROM reflog;");

    // Plant a FOREIGN-scope rebase_state row anchoring the commit.
    sqlite(&format!(
        "INSERT INTO rebase_state (worktree_id, head_name, onto, orig_head, current_head, \
         todo, done, stopped_sha) VALUES ('wt-alien', 'refs/heads/x', '{oid}', '{oid}', \
         '{oid}', '', '', '{oid}');"
    ));

    assert_cli_success(
        &run_libra_command(&["maintenance", "run", "--task", "gc"], main),
        "gc with state row",
    );
    let survives = run_libra_command(&["cat-file", "-t", &oid], main);
    assert!(
        survives.status.success(),
        "a commit anchored only by a foreign-scope rebase_state row survives gc: {}",
        String::from_utf8_lossy(&survives.stderr)
    );

    // Negative control: drop the row — the same commit is now garbage.
    sqlite("DELETE FROM rebase_state;");
    assert_cli_success(
        &run_libra_command(&["maintenance", "run", "--task", "gc"], main),
        "gc without state row",
    );
    let pruned = run_libra_command(&["cat-file", "-t", &oid], main);
    assert!(
        !pruned.status.success(),
        "without the state row the commit is pruned (positive case was real)"
    );
}

/// Part C W1 (§C.4.2 ambiguous-common-sidecar rule): the legacy common
/// `.libra/rebase-merge/` crash-state directory is never auto-adopted (and
/// destroyed) while linked worktrees are registered — its owner is ambiguous.
/// The main worktree fails closed with an actionable error; a linked
/// worktree's probes simply do not see it (it is not that worktree's rebase).
#[test]
fn legacy_rebase_merge_dir_not_auto_adopted_with_linked_worktrees() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Plant a legacy crash-state dir in COMMON storage.
    fs::create_dir_all(main.join(".libra/rebase-merge")).unwrap();

    // Main: `rebase --continue` fails CLOSED mentioning the ambiguous legacy
    // dir, and must NOT consume it.
    let cont = run_libra_command(&["rebase", "--continue"], main);
    assert_ne!(
        cont.status.code(),
        Some(0),
        "adoption is refused while linked worktrees exist"
    );
    let stderr = String::from_utf8_lossy(&cont.stderr);
    assert!(
        stderr.contains("legacy rebase state"),
        "error names the legacy dir and why: {stderr}"
    );
    assert!(
        main.join(".libra/rebase-merge").exists(),
        "the legacy dir is preserved, not consumed"
    );

    // Linked worktree: status still works and does not adopt it either.
    assert_cli_success(
        &run_libra_command(&["status"], &wt),
        "status works in the linked worktree",
    );
    assert!(
        main.join(".libra/rebase-merge").exists(),
        "still preserved after linked-worktree probes"
    );
}

/// Part C W1 (§C.4.2, the final lift): `rebase` runs in a LINKED worktree on
/// fully worktree-scoped state. A conflicted rebase stopped in the linked
/// worktree does not block the MAIN worktree's own sequencer op (scoped
/// mutex), and the linked `--abort` restores only that worktree. Covers the
/// plan-named `linked_rebase_conflict_does_not_block_main_cherry_pick` and
/// the abort half of `two_linked_rebases_keep_independent_todo_and_abort`.
#[test]
fn rebase_runs_in_linked_worktree_and_conflict_does_not_block_main() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Diverge: main edits a.txt on `main`; the wt edits a.txt on `feature`.
    fs::write(main.join("a.txt"), "main-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "main add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-edit", "--no-verify"], main),
        "main commit",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );
    fs::write(wt.join("a.txt"), "feature-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], &wt), "wt add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feature-edit", "--no-verify"], &wt),
        "wt commit",
    );
    let wt_tip = head_sha(&wt);
    let main_head_before = head_sha(main);

    // Rebase `feature` onto main IN THE LINKED WORKTREE — allowed, and it
    // stops on the content conflict with worktree-scoped state.
    let rebase = run_libra_command(&["rebase", "main"], &wt);
    assert!(
        !String::from_utf8_lossy(&rebase.stderr).contains("not yet supported inside a linked"),
        "rebase must no longer be refused in a linked worktree: {}",
        String::from_utf8_lossy(&rebase.stderr)
    );
    assert_ne!(
        rebase.status.code(),
        Some(0),
        "the conflicting rebase stops for resolution"
    );

    // The MAIN worktree is not blocked by the linked worktree's stopped
    // rebase: its own cherry-pick of the wt's commit proceeds (it conflicts
    // in MAIN too — a same-file change — but the point is the scoped MUTEX
    // let it START; abort it right away).
    let cp = run_libra_command(&["cherry-pick", &wt_tip], main);
    assert!(
        !String::from_utf8_lossy(&cp.stderr).contains("rebase in progress"),
        "main's sequencer mutex must not see the linked worktree's rebase: {}",
        String::from_utf8_lossy(&cp.stderr)
    );
    if !cp.status.success() {
        assert_cli_success(
            &run_libra_command(&["cherry-pick", "--abort"], main),
            "abort main cherry-pick",
        );
    }

    // Abort the linked worktree's rebase: only ITS state restores.
    assert_cli_success(
        &run_libra_command(&["rebase", "--abort"], &wt),
        "wt rebase --abort",
    );
    assert_eq!(head_sha(&wt), wt_tip, "wt restored to its pre-rebase tip");
    assert_eq!(abbrev_head(&wt), "feature", "wt back on its branch");
    assert_eq!(head_sha(main), main_head_before, "main HEAD untouched");

    // Full conflict flow in the linked worktree: rebase again, resolve, and
    // `--continue` to completion — the continue path reads/clears only THIS
    // worktree's scoped state.
    let rerebase = run_libra_command(&["rebase", "main"], &wt);
    assert_ne!(
        rerebase.status.code(),
        Some(0),
        "stops on the conflict again"
    );
    fs::write(wt.join("a.txt"), "main-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], &wt), "resolve");
    assert_cli_success(
        &run_libra_command(&["rebase", "--continue"], &wt),
        "linked rebase --continue completes",
    );
    assert_eq!(abbrev_head(&wt), "feature", "wt still on feature");
    assert_eq!(head_sha(main), main_head_before, "main still untouched");
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

/// W1 §C.4.1.1: removing a worktree purges its dirty-cache rows AND meta —
/// a later re-add (fresh worktree_id) never inherits or leaks stale scope
/// rows.
#[test]
#[serial_test::serial]
fn worktree_remove_purges_dirty_scope_rows() {
    let repo = repo_with_feature();
    let main = repo.path();
    let wt_root = tempfile::tempdir().expect("wt root");
    let wt = wt_root.path().join("purge-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    std::fs::write(wt.join("dirt.txt"), "x\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["status", "--scan"], &wt),
        "linked scan",
    );
    assert_cli_success(
        &run_libra_command(&["dirty", "dirt.txt"], &wt),
        "linked manual mark",
    );

    assert_cli_success(
        &run_libra_command(&["worktree", "remove", wt.to_str().unwrap()], main),
        "worktree remove",
    );

    // In-process: no linked-scope rows survive in either dirty table.
    let _guard = libra::utils::test::ChangeDirGuard::new(main);
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        use sea_orm::{ConnectionTrait, Statement};
        let db = libra::internal::db::get_db_conn_instance().await;
        for table in ["working_dirty", "working_dirty_meta"] {
            let row = db
                .query_one(Statement::from_string(
                    db.get_database_backend(),
                    format!("SELECT COUNT(*) FROM {table} WHERE worktree_id <> '';"),
                ))
                .await
                .expect("count")
                .expect("row");
            let count: i64 = row.try_get_by_index(0).expect("count value");
            assert_eq!(count, 0, "{table} keeps no removed-scope rows");
        }
    });
}
