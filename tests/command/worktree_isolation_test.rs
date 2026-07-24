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

use super::{
    assert_cli_success, base_libra_command, parse_json_stdout, run_libra_command,
    run_libra_command_with_stdin,
};

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

/// Part C §C.11 transition-guard retirement ledger: every store that W0
/// fail-closed in linked worktrees has been scoped (dirty/layer/sparse in
/// W1, the stash stack protocol + pull's autostash wrap in W2), so ALL the
/// formerly guarded commands now run in a linked worktree. This test pins
/// the lifted contract — none of them may hit a linked-worktree guard.
#[test]
fn formerly_guarded_commands_run_in_linked_worktree() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Part C W2 final lift: `pull --rebase` AND its `--autostash` combo run
    // in a linked worktree — the autostash wrap uses the W2 stack-lock +
    // CAS protocol on the shared stash stack.
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );
    for argv in [
        vec!["pull", "--rebase"],
        vec!["pull", "--rebase", "--autostash"],
        vec!["stash", "list"],
    ] {
        let out = run_libra_command(&argv, &wt);
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("linked worktree"),
            "{argv:?} must not hit the linked-worktree guard anymore: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // W1/W2 §C.4: the dirty cache, the layer registry, the sparse view, and
    // the stash stack protocol are worktree-aware now — all run in a linked
    // worktree.
    assert_cli_success(
        &run_libra_command(&["dirty", "--list"], &wt),
        "dirty --list runs in a linked worktree since W1",
    );
    assert_cli_success(
        &run_libra_command(&["layer", "list"], &wt),
        "layer list runs in a linked worktree since W1",
    );
    assert_cli_success(
        &run_libra_command(&["sparse-view", "status"], &wt),
        "sparse-view status runs in a linked worktree since W1",
    );
    assert_cli_success(
        &run_libra_command(&["stash", "list"], &wt),
        "stash list runs in a linked worktree since W2",
    );
}

/// W2 §C.4.3: the stash STACK is deliberately repository-shared (an entry
/// pushed in one worktree lists and applies in another), while push/pop
/// snapshot and mutate only the ACTING worktree's index/workdir; `stash
/// branch` preflights the branch collision before touching anything.
#[test]
fn stash_stack_is_shared_with_scoped_snapshots() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("stash-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Dirty a TRACKED file in the linked worktree and stash there.
    let tracked = std::fs::read_dir(&wt)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.path().is_file())
        .expect("tracked file in wt")
        .path();
    let original = std::fs::read_to_string(&tracked).unwrap();
    std::fs::write(&tracked, "stashed-from-wt\n").unwrap();
    assert_cli_success(&run_libra_command(&["stash", "push"], &wt), "wt stash push");
    assert_eq!(
        std::fs::read_to_string(&tracked).unwrap(),
        original,
        "push restores the LINKED worktree's file"
    );

    // The shared stack lists the entry from MAIN...
    let listed = run_libra_command(&["stash", "list"], main);
    assert_cli_success(&listed, "main stash list");
    assert!(
        !String::from_utf8_lossy(&listed.stdout).trim().is_empty(),
        "the stack is repository-shared"
    );
    // ...and `stash branch` with a COLLIDING name refuses up front, keeping
    // the entry and both worktrees untouched.
    let collided = run_libra_command(&["stash", "branch", "feature"], main);
    assert_ne!(
        collided.status.code(),
        Some(0),
        "stash branch preflights the existing-branch collision"
    );
    let listed = run_libra_command(&["stash", "list"], main);
    assert!(
        !String::from_utf8_lossy(&listed.stdout).trim().is_empty(),
        "the refused branch kept the entry"
    );

    // Pop in MAIN: the change materializes in MAIN's workdir (the acting
    // scope), the linked worktree stays clean, and the entry is CAS-dropped.
    let main_file = main.join(
        tracked
            .file_name()
            .and_then(|n| n.to_str())
            .expect("file name"),
    );
    assert_cli_success(&run_libra_command(&["stash", "pop"], main), "main pop");
    assert_eq!(
        std::fs::read_to_string(&main_file).unwrap(),
        "stashed-from-wt\n",
        "pop applies to the ACTING worktree"
    );
    assert_eq!(
        std::fs::read_to_string(&tracked).unwrap(),
        original,
        "the linked worktree is untouched by main's pop"
    );
    let listed = run_libra_command(&["stash", "list"], &wt);
    assert_cli_success(&listed, "wt stash list");
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout).trim(),
        "",
        "the CAS pop removed the entry from the shared stack"
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
fn gc_and_repack_run_in_multi_worktree_repo_keeping_private_roots() {
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

    // W2 §C.4.3: gc RUNS in a multi-worktree repository (the W0 skip is
    // lifted) — the linked worktree's private index is a reachability root.
    // Age every loose object past the prune grace window first, so survival
    // below proves the ROOT, not the freshness belt.
    backdate_loose_objects(main);
    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], main);
    assert_cli_success(&gc, "maintenance gc");
    let text = String::from_utf8_lossy(&gc.stdout) + String::from_utf8_lossy(&gc.stderr);
    assert!(
        !text.contains("skipped loose-object prune"),
        "the multi-worktree gc skip is lifted: {text}"
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

    // incremental-repack runs too (same lifted skip); the staged-only blob
    // must still be readable afterwards (it is in the consolidated root set).
    let repack = run_libra_command(
        &["maintenance", "run", "--task", "incremental-repack"],
        main,
    );
    assert_cli_success(&repack, "maintenance incremental-repack");
    let repack_text =
        String::from_utf8_lossy(&repack.stdout) + String::from_utf8_lossy(&repack.stderr);
    assert!(
        !repack_text.contains("skipped repack: this repository has linked worktrees"),
        "the multi-worktree repack skip is lifted: {repack_text}"
    );
    let cat = run_libra_command(&["cat-file", "-p", &oid], main);
    assert_cli_success(&cat, "staged-only blob survives repack");
}

/// Age every loose object file past the gc prune grace window so a test can
/// prove ROOT-based survival rather than freshness-based survival.
pub(crate) fn backdate_loose_objects(repo: &std::path::Path) {
    // POSIX `touch -t [[CC]YY]MMDDhhmm` (portable, unlike GNU `-d`).
    let stamp = (chrono::Utc::now() - chrono::Duration::hours(2))
        .format("%Y%m%d%H%M")
        .to_string();
    let objects = repo.join(".libra/objects");
    let shards = std::fs::read_dir(&objects).expect("read objects dir");
    for shard in shards {
        let shard = shard.expect("objects shard entry");
        if !shard.path().is_dir() || shard.file_name() == "pack" {
            continue;
        }
        let files = std::fs::read_dir(shard.path()).expect("read objects shard");
        for file in files {
            let file = file.expect("loose object entry");
            let status = std::process::Command::new("touch")
                .arg("-t")
                .arg(&stamp)
                .arg(file.path())
                .status()
                .expect("spawn touch");
            assert!(
                status.success(),
                "backdating '{}' must succeed (a silently-fresh object would let the \
                 grace window mask a missing root)",
                file.path().display()
            );
        }
    }
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

/// W1 §C.4.1.1: the layer registry is worktree-scoped — the same layer name
/// registers/applies independently per worktree, each scope's overlay is
/// excluded from its own `status`/`add`, and one scope's unapply never
/// touches another worktree's materialized files.
#[test]
fn layer_registry_is_worktree_scoped() {
    let repo = repo_with_feature();
    let main = repo.path();
    let wt_root = tempfile::tempdir().expect("wt root");
    let wt = wt_root.path().join("layer-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Two external source dirs, same overlay filename, different content.
    let sources = tempfile::tempdir().expect("sources");
    let src_main = sources.path().join("src-main");
    let src_linked = sources.path().join("src-linked");
    std::fs::create_dir_all(&src_main).unwrap();
    std::fs::create_dir_all(&src_linked).unwrap();
    std::fs::write(src_main.join("ov.txt"), "from-main\n").unwrap();
    std::fs::write(src_linked.join("ov.txt"), "from-linked\n").unwrap();

    // The SAME layer name registers independently in each worktree.
    assert_cli_success(
        &run_libra_command(
            &["layer", "add", "ov", "--source", src_main.to_str().unwrap()],
            main,
        ),
        "main layer add",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "ov",
                "--source",
                src_linked.to_str().unwrap(),
            ],
            &wt,
        ),
        "linked layer add (same name)",
    );
    assert_cli_success(&run_libra_command(&["layer", "apply"], main), "main apply");
    assert_cli_success(&run_libra_command(&["layer", "apply"], &wt), "wt apply");
    assert_eq!(
        std::fs::read_to_string(main.join("ov.txt")).unwrap(),
        "from-main\n"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("ov.txt")).unwrap(),
        "from-linked\n"
    );

    // Each scope lists only its own registration.
    let listed = run_libra_command(&["layer", "list"], &wt);
    assert_cli_success(&listed, "wt layer list");
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(
        stdout.contains("src-linked") && !stdout.contains("src-main"),
        "linked list shows only its own layer: {stdout}"
    );

    // The overlay is excluded from the linked worktree's status…
    let status = run_libra_command(&["status", "--porcelain=v1"], &wt);
    assert_cli_success(&status, "wt status");
    assert!(
        !String::from_utf8_lossy(&status.stdout).contains("ov.txt"),
        "layer overlay excluded from linked status"
    );
    // …and the linked add guard refuses to stage it even under --force.
    let forced = run_libra_command(&["add", "-f", "ov.txt"], &wt);
    assert_ne!(
        forced.status.code(),
        Some(0),
        "layer-owned path must not stage in the linked scope"
    );

    // Unapply in the linked scope removes ITS file only.
    assert_cli_success(&run_libra_command(&["layer", "unapply"], &wt), "wt unapply");
    assert!(!wt.join("ov.txt").exists(), "linked overlay removed");
    assert_eq!(
        std::fs::read_to_string(main.join("ov.txt")).unwrap(),
        "from-main\n",
        "main's materialized overlay is untouched"
    );
}

/// W1 §C.4.1.1: the sparse view is per-worktree — the same repo filters
/// `ls-files` differently per worktree, and one scope's disable/clear never
/// leaks into another's view.
#[test]
fn sparse_view_is_worktree_scoped() {
    let repo = repo_with_feature();
    let main = repo.path();
    let wt_root = tempfile::tempdir().expect("wt root");
    let wt = wt_root.path().join("sparse-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    // Two tracked files exist from the fixture; add distinct view scopes.
    let main_ls_all = run_libra_command(&["ls-files"], main);
    assert_cli_success(&main_ls_all, "main ls-files baseline");
    let baseline = String::from_utf8_lossy(&main_ls_all.stdout).lines().count();
    assert!(baseline >= 1, "fixture has tracked files");

    // Main scopes to a never-matching pattern; linked keeps everything.
    assert_cli_success(
        &run_libra_command(&["sparse-view", "set", "nothing-matches/**"], main),
        "main sparse-view set",
    );
    let main_ls = run_libra_command(&["ls-files"], main);
    assert_cli_success(&main_ls, "main ls-files filtered");
    assert_eq!(
        String::from_utf8_lossy(&main_ls.stdout).trim(),
        "",
        "main view filters everything out"
    );
    let wt_ls = run_libra_command(&["ls-files"], &wt);
    assert_cli_success(&wt_ls, "wt ls-files unfiltered");
    assert_eq!(
        String::from_utf8_lossy(&wt_ls.stdout).lines().count(),
        baseline,
        "linked worktree is NOT filtered by main's view"
    );

    // The linked worktree sets its own view; disabling it does not disable
    // main's, and clearing main's leaves linked's patterns intact.
    assert_cli_success(
        &run_libra_command(&["sparse-view", "set", "also-nothing/**"], &wt),
        "wt sparse-view set",
    );
    assert_cli_success(
        &run_libra_command(&["sparse-view", "disable"], &wt),
        "wt disable",
    );
    let main_status = run_libra_command(&["--json", "sparse-view", "status"], main);
    assert_cli_success(&main_status, "main sparse-view status");
    let json = parse_json_stdout(&main_status);
    assert_eq!(
        json["data"]["enabled"].as_bool(),
        Some(true),
        "main stays enabled after linked disable"
    );
    assert_cli_success(
        &run_libra_command(&["sparse-view", "clear"], main),
        "main clear",
    );
    let wt_status = run_libra_command(&["--json", "sparse-view", "status"], &wt);
    assert_cli_success(&wt_status, "wt sparse-view status");
    let json = parse_json_stdout(&wt_status);
    assert_eq!(
        json["data"]["pattern_count"].as_i64(),
        Some(1),
        "linked patterns survive main's clear"
    );
}

/// W1 §C.4.1.1: every registry mutator serializes on `worktrees.lock`. A
/// held lock BLOCKS a concurrent `worktree add` (it queues rather than
/// fails) and the add proceeds once the lock is released; concurrent adds
/// therefore both land in the registry (no load-modify-write lost update,
/// and a second add's strict pre-seed sweep can never run between another
/// add's seed and registry commit).
#[test]
fn registry_mutators_serialize_on_worktrees_lock() {
    /// Kill-and-reap on every exit path — an assertion failure must never
    /// leave a spawned add running against a removed temp repository.
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let repo = repo_with_feature();
    let main = repo.path();
    let wt_root = tempfile::tempdir().expect("wt root");

    // Take the registry lock, THEN spawn all three adds: the held lock is a
    // start barrier — every child must queue on the flock (add's FIRST
    // operation) before any of them can proceed, so the contention below is
    // guaranteed, not timing-dependent.
    let lock_path = main.join(".libra/worktrees.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open registry lock");
    // std file locking mirrors the production guard cross-platform (flock
    // on Unix, LockFileEx on Windows) — the test itself needs no cfg gate.
    lock_file.lock().expect("test takes the registry lock");
    let spawn_add = |wt: &std::path::Path| {
        ChildGuard(
            base_libra_command(&["worktree", "add", wt.to_str().unwrap()], main)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn add"),
        )
    };
    let targets = [
        wt_root.path().join("lock-wt-a"),
        wt_root.path().join("lock-wt-b"),
        wt_root.path().join("lock-wt-c"),
    ];
    let mut children: Vec<(std::path::PathBuf, ChildGuard)> = targets
        .iter()
        .map(|wt| (wt.clone(), spawn_add(wt)))
        .collect();

    std::thread::sleep(std::time::Duration::from_millis(1500));
    for (wt, child) in &mut children {
        assert!(
            child.0.try_wait().expect("try_wait").is_none(),
            "add for {} queues on the held registry lock instead of finishing",
            wt.display()
        );
        // STRONGER than liveness (which a slow start could fake): the lock
        // is add's first operation, before the target directory is even
        // created — zero side effects prove the child is parked ON the
        // flock, not merely slow.
        assert!(
            !wt.exists(),
            "no side effect for {} while the lock is held (add parks on the flock \
             before creating anything)",
            wt.display()
        );
    }

    lock_file.unlock().expect("test releases the registry lock");
    for (wt, mut child) in children {
        let status = child.0.wait().expect("wait add");
        assert!(
            status.success(),
            "add for {} succeeds once the lock is released",
            wt.display()
        );
        assert!(wt.join(".libra").exists(), "worktree materialized");
    }

    // All three serialized through the lock — none lost the others' entry.
    let registry =
        std::fs::read_to_string(main.join(".libra/worktrees.json")).expect("registry file");
    for name in ["lock-wt-a", "lock-wt-b", "lock-wt-c"] {
        assert!(
            registry.contains(name),
            "{name} survives concurrent registry writes: {registry}"
        );
    }
}

/// W1 §C.4.1.1: instance ids are deterministic (path-derived), and the
/// remove/prune GC is best-effort — so `worktree add` STRICTLY sweeps its
/// instance id's scoped rows before seeding. Stale rows a failed GC left
/// behind (planted here directly) must never be inherited by a new
/// worktree at the same path: its sparse view starts disabled/empty and
/// its layer registry starts empty.
#[test]
#[serial_test::serial]
fn worktree_add_sweeps_stale_scope_rows() {
    let repo = repo_with_feature();
    let main = repo.path();
    let wt_root = tempfile::tempdir().expect("wt root");
    let wt = wt_root.path().join("swept-wt");
    // Pre-create the (empty) directory so its canonical path — and thus the
    // deterministic instance id — can be computed before the add.
    std::fs::create_dir_all(&wt).unwrap();
    let canonical = std::fs::canonicalize(&wt).unwrap();
    let stale_id = libra::utils::util::worktree_instance_id(&canonical);

    // Plant "leaked" rows for that id, as if a prior remove's GC failed.
    {
        let _guard = libra::utils::test::ChangeDirGuard::new(main);
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            use sea_orm::{ConnectionTrait, Statement};
            let db = libra::internal::db::get_db_conn_instance().await;
            for sql in [
                format!(
                    "INSERT INTO sparse_view (worktree_id, pattern, ordinal) \
                     VALUES ('{stale_id}', 'stale/**', 0);"
                ),
                format!(
                    "INSERT INTO sparse_view_meta (worktree_id, enabled) \
                     VALUES ('{stale_id}', 1);"
                ),
                format!(
                    "INSERT INTO layer (worktree_id, name, source) \
                     VALUES ('{stale_id}', 'stale-ov', '/nonexistent');"
                ),
                format!(
                    "INSERT INTO layer_path (worktree_id, layer_name, path, content_hash) \
                     VALUES ('{stale_id}', 'stale-ov', 'stale.txt', 'h0');"
                ),
            ] {
                db.execute(Statement::from_string(db.get_database_backend(), sql))
                    .await
                    .expect("plant stale row");
            }
        });
    }

    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add sweeps the stale scope",
    );

    // The new worktree inherits NOTHING: sparse disabled/empty, no layers.
    let status = run_libra_command(&["--json", "sparse-view", "status"], &wt);
    assert_cli_success(&status, "wt sparse-view status");
    let json = parse_json_stdout(&status);
    assert_eq!(json["data"]["enabled"].as_bool(), Some(false));
    assert_eq!(json["data"]["pattern_count"].as_i64(), Some(0));
    let layers = run_libra_command(&["layer", "list"], &wt);
    assert_cli_success(&layers, "wt layer list");
    assert!(
        !String::from_utf8_lossy(&layers.stdout).contains("stale-ov"),
        "stale layer registration not inherited"
    );
}

/// W1 §C.4.1.1: `worktree remove` purges the removed scope's layer rows ONLY
/// when the directory is deleted too. A default (directory-retaining) remove
/// keeps the ownership rows — the retained `.libra` still operates as a
/// repository, so the still-materialized overlay files must stay
/// un-stageable (never-enters-commit).
#[test]
#[serial_test::serial]
fn worktree_remove_purges_layer_scope_rows() {
    let repo = repo_with_feature();
    let main = repo.path();
    let wt_root = tempfile::tempdir().expect("wt root");
    let sources = tempfile::tempdir().expect("sources");
    let src = sources.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("ov.txt"), "x\n").unwrap();

    let add_layer_and_apply = |wt: &std::path::Path| {
        assert_cli_success(
            &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
            "worktree add",
        );
        assert_cli_success(
            &run_libra_command(
                &["layer", "add", "ov", "--source", src.to_str().unwrap()],
                wt,
            ),
            "linked layer add",
        );
        assert_cli_success(&run_libra_command(&["layer", "apply"], wt), "wt apply");
        assert_cli_success(
            &run_libra_command(&["sparse-view", "set", "scoped/**"], wt),
            "wt sparse-view set",
        );
    };
    let linked_rows = |table: &str| -> i64 {
        let _guard = libra::utils::test::ChangeDirGuard::new(main);
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let table = table.to_string();
        rt.block_on(async {
            use sea_orm::{ConnectionTrait, Statement};
            let db = libra::internal::db::get_db_conn_instance().await;
            let row = db
                .query_one(Statement::from_string(
                    db.get_database_backend(),
                    format!("SELECT COUNT(*) FROM {table} WHERE worktree_id <> '';"),
                ))
                .await
                .expect("count")
                .expect("row");
            row.try_get_by_index(0).expect("count value")
        })
    };

    // Branch 1 — default remove RETAINS the directory: ownership rows
    // survive, and the retained directory still refuses to stage the
    // overlay (never-enters-commit holds for the files left on disk).
    let wt_kept = wt_root.path().join("layer-kept-wt");
    add_layer_and_apply(&wt_kept);
    assert_cli_success(
        &run_libra_command(&["worktree", "remove", wt_kept.to_str().unwrap()], main),
        "default worktree remove",
    );
    assert!(wt_kept.join("ov.txt").exists(), "overlay file retained");
    assert!(
        linked_rows("layer") > 0 && linked_rows("layer_path") > 0,
        "retained directory keeps its layer ownership rows"
    );
    assert!(
        linked_rows("sparse_view") > 0 && linked_rows("sparse_view_meta") > 0,
        "retained directory keeps its sparse view rows"
    );
    let forced = run_libra_command(&["add", "-f", "ov.txt"], &wt_kept);
    assert_ne!(
        forced.status.code(),
        Some(0),
        "retained overlay stays un-stageable after a directory-keeping remove"
    );

    // Branch 2 — `--delete-dir` removes the files WITH the directory, so the
    // scope rows are purged (nothing left on disk to guard). An applied
    // overlay alone does NOT count as dirty, but a REAL uncommitted file
    // still refuses — the explicit overlay subtraction must not fail open.
    let wt_gone = wt_root.path().join("layer-gone-wt");
    add_layer_and_apply(&wt_gone);
    std::fs::write(wt_gone.join("real-work.txt"), "uncommitted\n").unwrap();
    let refused = run_libra_command(
        &[
            "worktree",
            "remove",
            wt_gone.to_str().unwrap(),
            "--delete-dir",
        ],
        main,
    );
    assert_ne!(
        refused.status.code(),
        Some(0),
        "a real uncommitted file still refuses --delete-dir"
    );
    std::fs::remove_file(wt_gone.join("real-work.txt")).unwrap();
    assert_cli_success(
        &run_libra_command(
            &[
                "worktree",
                "remove",
                wt_gone.to_str().unwrap(),
                "--delete-dir",
            ],
            main,
        ),
        "worktree remove --delete-dir",
    );
    for table in ["layer", "layer_path", "sparse_view", "sparse_view_meta"] {
        assert_eq!(
            linked_rows(table),
            1,
            "{table} keeps only the retained (kept-dir) scope's row"
        );
    }

    // Branch 3 — `worktree prune` GCs the scoped rows of an externally
    // deleted worktree the same way (nothing on disk left to guard).
    let wt_pruned = wt_root.path().join("layer-pruned-wt");
    add_layer_and_apply(&wt_pruned);
    std::fs::remove_dir_all(&wt_pruned).unwrap();
    assert_cli_success(
        &run_libra_command(&["worktree", "prune"], main),
        "worktree prune",
    );
    for table in ["layer", "layer_path", "sparse_view", "sparse_view_meta"] {
        assert_eq!(
            linked_rows(table),
            1,
            "{table} keeps only the retained (kept-dir) scope's row after prune"
        );
    }
}

/// Registry v2 (plan-20260714 §C.7): a legacy v1 `{ worktrees: [...] }` file
/// is durably upgraded on first touch — rewritten as
/// `{ schema_version: 2, entries: [...] }` with each linked entry's STABLE id
/// backfilled from its gitdir — while preserving every v1 field.
#[test]
fn registry_v1_file_upgrades_to_v2_with_backfilled_ids() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-v1up");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let gitdir_id = std::fs::read_to_string(wt.join(".libra").join("worktree_id"))
        .expect("linked gitdir id")
        .trim()
        .to_string();

    // Downgrade the registry file to the v1 shape by hand.
    let registry = main.join(".libra").join("worktrees.json");
    let v2: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).expect("read registry")).expect("v2 json");
    assert_eq!(v2["schema_version"], 2, "fresh registry is v2");
    let v1_entries: Vec<serde_json::Value> = v2["entries"]
        .as_array()
        .expect("entries array")
        .iter()
        .map(|entry| {
            serde_json::json!({
                "path": entry["path"],
                "is_main": entry["is_main"],
                "locked": entry["locked"],
                "lock_reason": entry["lock_reason"],
            })
        })
        .collect();
    std::fs::write(
        &registry,
        serde_json::to_vec_pretty(&serde_json::json!({ "worktrees": v1_entries }))
            .expect("serialize v1"),
    )
    .expect("write v1 registry");

    // A LOCKLESS reader (list) reads the v1 file through the in-memory
    // upgrade — with correct ids via the gitdir fallback — but must NOT
    // rewrite it (an unlocked writer could overwrite a concurrent locked
    // mutation).
    let v1_bytes = std::fs::read(&registry).expect("v1 bytes");
    let list = run_libra_command(&["worktree", "list", "--json"], main);
    assert_cli_success(&list, "worktree list after v1 downgrade");
    let listed = parse_json_stdout(&list);
    let entries = listed["data"]["worktrees"]
        .as_array()
        .expect("list entries");
    let linked = entries
        .iter()
        .find(|entry| entry["is_main"] == false)
        .expect("linked entry listed");
    assert_eq!(
        linked["worktree_id"].as_str(),
        Some(gitdir_id.as_str()),
        "listed id survives the v1 round-trip"
    );
    assert_eq!(
        std::fs::read(&registry).expect("registry after list"),
        v1_bytes,
        "a lockless reader never rewrites the registry"
    );

    // The first MUTATING command (here: no-arg repair, which loads under the
    // registry lock) performs the durable upgrade.
    assert_cli_success(
        &run_libra_command(&["worktree", "repair"], main),
        "repair drives the durable upgrade",
    );

    let upgraded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).expect("read upgraded registry"))
            .expect("upgraded json");
    assert_eq!(upgraded["schema_version"], 2, "file rewritten as v2");
    let upgraded_entries = upgraded["entries"].as_array().expect("v2 entries");
    assert_eq!(upgraded_entries.len(), 2, "both entries preserved");
    let upgraded_linked = upgraded_entries
        .iter()
        .find(|entry| entry["is_main"] == false)
        .expect("linked entry persisted");
    assert_eq!(
        upgraded_linked["worktree_id"].as_str(),
        Some(gitdir_id.as_str()),
        "stable id backfilled from the gitdir during the upgrade"
    );
    assert!(
        upgraded.get("worktrees").is_none(),
        "legacy top-level key does not survive"
    );
}

/// `worktree repair <path>` (§C.7): restores a linked worktree's deleted
/// `.libra/worktree_id` and `commondir` from the registry's PERSISTED id, so
/// the worktree maps back to ITS OWN scoped rows (never a fresh synthesized
/// scope and never main's).
#[test]
fn worktree_repair_path_restores_identity_from_registry() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-repair");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "switch in linked worktree",
    );
    let gitdir = wt.join(".libra");
    let original_id = std::fs::read_to_string(gitdir.join("worktree_id"))
        .expect("original id")
        .trim()
        .to_string();
    let original_commondir =
        std::fs::read_to_string(gitdir.join("commondir")).expect("original commondir");

    // Simulate identity loss: both gitdir pointer files vanish.
    std::fs::remove_file(gitdir.join("worktree_id")).expect("drop id file");
    std::fs::remove_file(gitdir.join("commondir")).expect("drop commondir");

    let repaired = run_libra_command(
        &["worktree", "repair", wt.to_str().unwrap(), "--json"],
        main,
    );
    assert_cli_success(&repaired, "worktree repair <path>");
    let payload = parse_json_stdout(&repaired);
    assert_eq!(
        payload["data"]["worktree_id"].as_str(),
        Some(original_id.as_str()),
        "repair restores the persisted id, not a fresh synthesis"
    );
    assert_eq!(payload["data"]["worktree_id_restored"], true);
    assert_eq!(payload["data"]["commondir_restored"], true);

    let restored_id = std::fs::read_to_string(gitdir.join("worktree_id"))
        .expect("restored id")
        .trim()
        .to_string();
    assert_eq!(restored_id, original_id);
    let restored_commondir =
        std::fs::read_to_string(gitdir.join("commondir")).expect("restored commondir");
    assert_eq!(
        restored_commondir.trim(),
        original_commondir.trim(),
        "commondir points back at the shared storage"
    );

    // The repaired worktree still resolves ITS OWN scope: HEAD stays on
    // `feature`, proving the id did not silently change.
    let head = String::from_utf8_lossy(
        &run_libra_command(&["rev-parse", "--abbrev-ref", "HEAD"], &wt).stdout,
    )
    .trim()
    .to_string();
    assert_eq!(
        head, "feature",
        "repaired worktree keeps its own HEAD scope"
    );

    // Idempotent second run: nothing left to restore.
    let second = run_libra_command(
        &["worktree", "repair", wt.to_str().unwrap(), "--json"],
        main,
    );
    assert_cli_success(&second, "second repair run");
    let payload = parse_json_stdout(&second);
    assert_eq!(payload["data"]["worktree_id_restored"], false);
    assert_eq!(payload["data"]["commondir_restored"], false);

    // A CORRUPT (empty) commondir — the exact state the storage resolver
    // fails closed on — is restored too, not just a missing file.
    std::fs::write(gitdir.join("commondir"), "").expect("corrupt commondir");
    let third = run_libra_command(
        &["worktree", "repair", wt.to_str().unwrap(), "--json"],
        main,
    );
    assert_cli_success(&third, "repair of a corrupt commondir");
    let payload = parse_json_stdout(&third);
    assert_eq!(payload["data"]["commondir_restored"], true);
    let healed = std::fs::read_to_string(gitdir.join("commondir")).expect("healed commondir");
    assert_eq!(healed.trim(), original_commondir.trim());

    // A RELATIVE pointer that resolves (against the gitdir) to THIS
    // repository's storage is recognized as correct — not misclassified as
    // foreign against the caller's cwd.
    std::fs::write(gitdir.join("commondir"), "../../.libra\n").expect("relative commondir");
    let relative = run_libra_command(
        &["worktree", "repair", wt.to_str().unwrap(), "--json"],
        main,
    );
    assert_cli_success(&relative, "repair with a valid relative commondir");
    let payload = parse_json_stdout(&relative);
    assert_eq!(
        payload["data"]["commondir_restored"], false,
        "a valid relative pointer is not foreign and needs no restore"
    );

    // A VALID pointer at a DIFFERENT storage is refused — and the refusal
    // must be side-effect free: NEITHER gitdir file may change, even when
    // the worktree_id also needs restoring.
    let other = tempfile::tempdir().expect("other storage");
    let foreign_pointer = format!("{}\n", other.path().display());
    std::fs::write(gitdir.join("commondir"), &foreign_pointer).expect("foreign commondir");
    std::fs::write(gitdir.join("worktree_id"), "stale-or-corrupt\n").expect("stale id");
    let refused = run_libra_command(&["worktree", "repair", wt.to_str().unwrap()], main);
    assert!(
        !refused.status.success(),
        "repair must refuse to re-home a worktree pointing at another storage"
    );
    assert_eq!(
        std::fs::read_to_string(gitdir.join("commondir")).expect("commondir after refusal"),
        foreign_pointer,
        "refusal leaves commondir byte-for-byte unchanged"
    );
    assert_eq!(
        std::fs::read_to_string(gitdir.join("worktree_id")).expect("id after refusal"),
        "stale-or-corrupt\n",
        "refusal leaves worktree_id byte-for-byte unchanged"
    );
}

/// `worktree repair <path>` refuses unregistered paths and the main worktree
/// instead of guessing identities (§C.7 fail-closed).
#[test]
fn worktree_repair_path_refuses_main_and_unregistered() {
    let dir = repo_with_feature();
    let main = dir.path();

    let main_refused = run_libra_command(&["worktree", "repair", main.to_str().unwrap()], main);
    assert!(
        !main_refused.status.success(),
        "repair <main> must be refused"
    );

    let stranger = main.join("never-registered");
    std::fs::create_dir_all(&stranger).expect("mkdir");
    let unregistered = run_libra_command(&["worktree", "repair", stranger.to_str().unwrap()], main);
    assert!(
        !unregistered.status.success(),
        "repair on an unregistered path must be refused"
    );
}

/// §C.7 ordering: every worktree command applies pending repository
/// migrations — including the registry-v2 capability marker (2026072401) —
/// BEFORE any `worktrees.json` read or rewrite. A repo whose database predates
/// the marker gains it from a plain `worktree list`, so an old binary is
/// refused at connect time no matter which command first touches the v2 file.
#[tokio::test]
async fn worktree_commands_apply_capability_marker_before_registry_io() {
    use libra::internal::db::migration::builtin_runner;
    use sea_orm::{ConnectionTrait, Database, Statement};

    let dir = repo_with_feature();
    let main = dir.path();
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        main.join(".libra/libra.db").display()
    );

    // Re-open the pre-v2 window: roll back ONLY the capability marker.
    {
        let conn = Database::connect(&db_url).await.expect("connect repo db");
        let rolled = builtin_runner()
            .expect("builtin runner")
            .rollback_to(&conn, 2026072304)
            .await
            .expect("roll back capability marker");
        assert_eq!(rolled, vec![2026072401]);
        conn.close().await.expect("close");
    }

    assert_cli_success(
        &run_libra_command(&["worktree", "list"], main),
        "worktree list on a pre-marker database",
    );

    let conn = Database::connect(&db_url).await.expect("reconnect repo db");
    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' \
             AND name = 'worktree_registry_capability'"
                .to_string(),
        ))
        .await
        .expect("query")
        .expect("count row");
    let count: i32 = row.try_get_by_index(0).expect("count");
    assert_eq!(
        count, 1,
        "the preflight re-applied the capability marker before registry IO"
    );
}

/// v2 identity invariants (§C.7): a v2 registry whose linked entry lost its
/// persisted id is CORRUPT — readers and mutators refuse it (never silently
/// falling back to the mutable gitdir) until the explicit no-arg
/// `worktree repair` deterministically heals and persists it.
#[test]
fn v2_identity_invariant_violations_refuse_until_explicit_repair() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-invariant");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let gitdir_id = std::fs::read_to_string(wt.join(".libra").join("worktree_id"))
        .expect("linked gitdir id")
        .trim()
        .to_string();

    // Corrupt the v2 registry: strip the linked entry's persisted id.
    let registry = main.join(".libra").join("worktrees.json");
    let mut doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).expect("read registry"))
            .expect("registry json");
    for entry in doc["entries"].as_array_mut().expect("entries") {
        if entry["is_main"] == false {
            entry.as_object_mut().expect("entry").remove("worktree_id");
        }
    }
    std::fs::write(
        &registry,
        serde_json::to_vec_pretty(&doc).expect("serialize"),
    )
    .expect("write corrupt registry");

    // Both a lockless reader and a locked mutator refuse, pointing at repair.
    let list = run_libra_command(&["worktree", "list"], main);
    assert!(
        !list.status.success(),
        "list refuses the corrupt v2 registry"
    );
    let lock = run_libra_command(
        &["worktree", "lock", wt.to_str().unwrap(), "--reason", "x"],
        main,
    );
    assert!(
        !lock.status.success(),
        "mutators refuse the corrupt v2 registry"
    );
    let stderr = String::from_utf8_lossy(&lock.stderr);
    assert!(
        stderr.contains("worktree repair"),
        "refusal directs at the explicit repair: {stderr}"
    );

    // The explicit no-arg repair heals deterministically (gitdir backfill).
    let repaired = run_libra_command(&["--json", "worktree", "repair"], main);
    assert_cli_success(&repaired, "no-arg repair heals the invariants");
    let payload = parse_json_stdout(&repaired);
    assert_eq!(
        payload["data"]["changed"], true,
        "heal is reported as a change"
    );

    let healed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).expect("read healed registry"))
            .expect("healed json");
    let linked = healed["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["is_main"] == false)
        .expect("linked entry");
    assert_eq!(
        linked["worktree_id"].as_str(),
        Some(gitdir_id.as_str()),
        "heal backfills the id from the gitdir"
    );

    // Mutators work again.
    assert_cli_success(
        &run_libra_command(
            &["worktree", "lock", wt.to_str().unwrap(), "--reason", "x"],
            main,
        ),
        "mutators run after the heal",
    );
}

/// A zero-byte registry is a torn write, not a fresh repository: readers and
/// mutators fail closed and NOTHING reinitializes or overwrites it — a silent
/// main-only rewrite would drop every linked entry.
#[test]
fn zero_byte_registry_fails_closed_everywhere() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-torn");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let registry = main.join(".libra").join("worktrees.json");
    std::fs::write(&registry, b"").expect("truncate registry");

    for argv in [
        vec!["worktree", "list"],
        vec!["worktree", "lock", wt.to_str().unwrap(), "--reason", "x"],
        vec!["worktree", "repair"],
        vec!["worktree", "repair", wt.to_str().unwrap()],
    ] {
        let out = run_libra_command(&argv, main);
        assert!(
            !out.status.success(),
            "{argv:?} must fail closed on a zero-byte registry"
        );
    }
    assert_eq!(
        std::fs::metadata(&registry)
            .expect("registry still present")
            .len(),
        0,
        "nothing may reinitialize or overwrite the torn registry"
    );
}

/// `worktree repair <path>` refuses a legacy v1 registry outright: v1 carries
/// no persisted identities, so restoring from it would launder a freshly
/// synthesized id into the gitdir. The explicit no-arg repair upgrade comes
/// first, then the path form works.
#[test]
fn worktree_repair_path_refuses_v1_registry_until_upgrade() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-v1-repair");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Downgrade the registry to the v1 shape.
    let registry = main.join(".libra").join("worktrees.json");
    let v2: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).expect("read registry")).expect("v2 json");
    let v1_entries: Vec<serde_json::Value> = v2["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| {
            serde_json::json!({
                "path": entry["path"],
                "is_main": entry["is_main"],
                "locked": entry["locked"],
                "lock_reason": entry["lock_reason"],
            })
        })
        .collect();
    std::fs::write(
        &registry,
        serde_json::to_vec_pretty(&serde_json::json!({ "worktrees": v1_entries }))
            .expect("serialize v1"),
    )
    .expect("write v1 registry");

    let refused = run_libra_command(&["worktree", "repair", wt.to_str().unwrap()], main);
    assert!(
        !refused.status.success(),
        "path repair must refuse a v1 registry"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("legacy v1"),
        "refusal explains the v1 state: {stderr}"
    );

    // The explicit upgrade, then the path form works.
    assert_cli_success(
        &run_libra_command(&["worktree", "repair"], main),
        "no-arg repair upgrades the registry",
    );
    assert_cli_success(
        &run_libra_command(&["worktree", "repair", wt.to_str().unwrap()], main),
        "path repair works on the upgraded registry",
    );
}

/// §C.7: the repository root is the AUTHORITATIVE main. A malformed v1
/// registry that marks a LINKED entry as main (or omits the main entirely)
/// must never durably crown the linked worktree during the upgrade — the
/// root is restored as main and the linked entry stays linked with its id.
#[test]
fn v1_upgrade_never_crowns_a_linked_entry_as_main() {
    let dir = repo_with_feature();
    let main = dir.path();
    let canonical_main = main.canonicalize().expect("canonical main");
    let wt = main.join("wt-crown");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let gitdir_id = std::fs::read_to_string(wt.join(".libra").join("worktree_id"))
        .expect("linked gitdir id")
        .trim()
        .to_string();
    let canonical_wt = wt.canonicalize().expect("canonical wt");
    let registry = main.join(".libra").join("worktrees.json");

    // Case 1: multi-main v1 — the linked entry is (wrongly) marked main too.
    // Case 2: mainless v1 — ONLY the linked entry exists.
    let multi_main = serde_json::json!({ "worktrees": [
        {"path": canonical_main.to_string_lossy(), "is_main": true,
         "locked": false, "lock_reason": null},
        {"path": canonical_wt.to_string_lossy(), "is_main": true,
         "locked": false, "lock_reason": null},
    ]});
    let mainless = serde_json::json!({ "worktrees": [
        {"path": canonical_wt.to_string_lossy(), "is_main": false,
         "locked": false, "lock_reason": null},
    ]});
    for (label, doc) in [("multi-main", multi_main), ("mainless", mainless)] {
        std::fs::write(
            &registry,
            serde_json::to_vec_pretty(&doc).expect("serialize"),
        )
        .expect("write malformed v1");
        assert_cli_success(
            &run_libra_command(&["worktree", "repair"], main),
            "upgrade via no-arg repair",
        );
        let upgraded: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry).expect("read upgraded"))
                .expect("upgraded json");
        let entries = upgraded["entries"].as_array().expect("entries");
        let mains: Vec<_> = entries.iter().filter(|e| e["is_main"] == true).collect();
        assert_eq!(mains.len(), 1, "{label}: exactly one main");
        assert_eq!(
            mains[0]["path"].as_str(),
            Some(canonical_main.to_string_lossy().as_ref()),
            "{label}: the repository root is main, never the linked path"
        );
        let linked = entries
            .iter()
            .find(|e| e["path"].as_str() == Some(canonical_wt.to_string_lossy().as_ref()))
            .expect("linked entry survives");
        assert_eq!(linked["is_main"], false, "{label}: linked stays linked");
        assert_eq!(
            linked["worktree_id"].as_str(),
            Some(gitdir_id.as_str()),
            "{label}: linked id backfilled from ITS OWN gitdir"
        );
    }
}

/// Part C bare boundary (§C.4.1): a bare repository has no working trees —
/// the entire worktree family refuses with the stable `LBR-REPO-003` before
/// any registry IO (no worktrees.json may appear).
#[test]
fn bare_repository_refuses_worktree_family() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bare = dir.path().join("repo.git");
    assert_cli_success(
        &run_libra_command(&["init", "--bare", bare.to_str().unwrap()], dir.path()),
        "init --bare",
    );

    let wt_target = dir.path().join("wt-from-bare");
    for argv in [
        vec!["worktree", "list"],
        vec!["worktree", "add", wt_target.to_str().unwrap()],
        vec!["worktree", "repair"],
    ] {
        let out = run_libra_command(&argv, &bare);
        assert!(
            !out.status.success(),
            "{argv:?} must be refused in a bare repository"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr
                .lines()
                .any(|line| line.trim() == "Error-Code: LBR-REPO-003"),
            "stable LBR-REPO-003 refusal for {argv:?}: {stderr}"
        );
    }
    assert!(
        !bare.join("worktrees.json").exists(),
        "no registry may be created in a bare repository"
    );
    assert!(!wt_target.exists(), "no worktree directory may be created");

    // Adversarial layout: a bare repository whose directory is literally
    // named `.libra` defeats any basename heuristic — the recorded
    // `core.bare` config must still refuse it.
    let disguised_parent = dir.path().join("disguised");
    std::fs::create_dir_all(&disguised_parent).expect("mkdir");
    let disguised = disguised_parent.join(".libra");
    assert_cli_success(
        &run_libra_command(&["init", "--bare", disguised.to_str().unwrap()], dir.path()),
        "init --bare .libra",
    );
    for cwd in [&disguised, &disguised_parent] {
        let out = run_libra_command(&["worktree", "list"], cwd);
        assert!(
            !out.status.success(),
            "worktree list from {cwd:?} must be refused for a .libra-named bare repo"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr
                .lines()
                .any(|line| line.trim() == "Error-Code: LBR-REPO-003"),
            "config-first classifier refuses the disguised bare repo: {stderr}"
        );
    }
    assert!(!disguised.join("worktrees.json").exists());

    // Every git boolean spelling of core.bare=true must classify as bare —
    // `yes`/`on`/`1` are as bare as `true` (fail-open here would let the
    // disguised layout through).
    for spelling in ["yes", "on", "1"] {
        assert_cli_success(
            &run_libra_command(&["config", "core.bare", spelling], &disguised),
            "set core.bare spelling",
        );
        let out = run_libra_command(&["worktree", "list"], &disguised);
        assert!(
            !out.status.success(),
            "core.bare={spelling} must still classify as bare"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr
                .lines()
                .any(|line| line.trim() == "Error-Code: LBR-REPO-003"),
            "core.bare={spelling}: {stderr}"
        );

        // The SHARED classifier must hold beyond the worktree family:
        // `status` refuses a bare repository on the same spellings.
        let status_out = run_libra_command(&["status"], &disguised);
        assert!(
            !status_out.status.success(),
            "status must refuse a bare repo with core.bare={spelling}"
        );
        let status_stderr = String::from_utf8_lossy(&status_out.stderr);
        assert!(
            status_stderr
                .lines()
                .any(|line| line.trim() == "Error-Code: LBR-REPO-003"),
            "status bare refusal for core.bare={spelling}: {status_stderr}"
        );
    }

    // An unparseable core.bare fails CLOSED (refusal, not fall-through).
    assert_cli_success(
        &run_libra_command(&["config", "core.bare", "maybe"], &disguised),
        "set invalid core.bare",
    );
    let out = run_libra_command(&["worktree", "list"], &disguised);
    assert!(
        !out.status.success(),
        "an unparseable core.bare must fail closed"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr
            .lines()
            .any(|line| line.trim() == "Error-Code: LBR-CLI-002"),
        "unparseable core.bare pins LBR-CLI-002: {stderr}"
    );
}
