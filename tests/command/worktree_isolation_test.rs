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

use super::{assert_cli_success, run_libra_command};

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
/// tables, and the composite `fetch`/`pull` (shared `FETCH_HEAD`) — must fail
/// closed in a linked worktree until W1/W2 make them worktree-scoped. The
/// guard fires before any side effect, so no remote/network is needed.
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
        &["fetch"],
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
    for op in ["merge", "rebase", "cherry-pick", "revert"] {
        let out = run_libra_command(&[op, "feature"], &wt);
        assert_ne!(
            out.status.code(),
            Some(0),
            "{op} refused in linked worktree"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("linked worktree"),
            "{op}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // The same op works in the main worktree.
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], main),
        "merge in main",
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
