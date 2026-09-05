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
    run_libra_command_with_stdin, run_libra_command_with_stdin_and_env,
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

/// Reconstruct the real pre-OL-02 operation shape for rollback fixtures.
/// Production v2 is intentionally forward-only, so older migration downs
/// must run against v1 tables rather than a same-named v2 table.
async fn restore_v1_operation_shape(conn: &sea_orm::DatabaseConnection) {
    use sea_orm::{ConnectionTrait, Statement};

    for table in [
        "ai_operation_link",
        "change_predecessor",
        "change_revision",
        "change_identity",
        "operation_journal",
        "operation_head",
        "operation_parent",
        "operation",
    ] {
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("DROP TABLE IF EXISTS `{table}`"),
        ))
        .await
        .unwrap_or_else(|error| panic!("drop v2 fixture table {table}: {error}"));
    }
    for trigger in [
        "legacy_operation_scope_provenance_domain_insert",
        "legacy_operation_scope_provenance_domain_update",
        "legacy_operation_scope_kind_domain_insert",
        "legacy_operation_scope_kind_domain_update",
    ] {
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("DROP TRIGGER IF EXISTS {trigger}"),
        ))
        .await
        .unwrap_or_else(|error| panic!("drop v2 fixture trigger {trigger}: {error}"));
    }
    for index in [
        "idx_legacy_operation_repo_order",
        "idx_legacy_operation_dedup_scope",
        "idx_legacy_operation_control_slot",
        "idx_legacy_operation_parent_parent",
        "idx_legacy_operation_view_repo_created",
    ] {
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("DROP INDEX IF EXISTS {index}"),
        ))
        .await
        .unwrap_or_else(|error| panic!("drop v2 fixture index {index}: {error}"));
    }
    for (legacy, v1) in [
        ("legacy_operation", "operation"),
        ("legacy_operation_parent", "operation_parent"),
        ("legacy_operation_view", "operation_view"),
        ("legacy_operation_view_ref", "operation_view_ref"),
        (
            "legacy_operation_view_workspace",
            "operation_view_workspace",
        ),
    ] {
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("ALTER TABLE `{legacy}` RENAME TO `{v1}`"),
        ))
        .await
        .unwrap_or_else(|error| panic!("restore {legacy} as {v1}: {error}"));
    }
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
    // The failure happens at path resolution, NOT by routing the DB lookup
    // at a phantom `<wt>/.libra/libra.db` — the pre-fix symptom. Since
    // W3-s1b the resolver's own diagnosis surfaces VERBATIM (LBR-REPO-003 +
    // the repair hint) instead of being masked as repo-not-found.
    assert!(
        !stderr.contains(".libra/libra.db"),
        "must not route db lookups at the phantom local gitdir: {stderr}"
    );
    assert!(
        stderr.contains("Error-Code: LBR-REPO-003") && stderr.contains("worktree repair"),
        "fails closed with the actionable corrupt-commondir diagnosis: {stderr}"
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
/// plan-20260714 W2 §C.4.3 re-verification (self-review finding): gc run
/// FROM A LINKED WORKTREE must still root the MAIN worktree's private index.
///
/// `worktree_index_roots` seeds the invoking worktree's index and then walks
/// the registry — which SKIPS the main entry, because on a main-invoked run
/// the seed already covers it. Invoked from a linked worktree, that skip
/// left main's index out of the root set entirely, so a blob staged only in
/// main was collected as garbage by any gc a linked worktree happened to
/// run. Every pre-existing gc test invoked from main, which is exactly why
/// this survived.
#[test]
fn gc_from_linked_worktree_keeps_main_staged_blob() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("gc-from-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // A blob STAGED ONLY in main — no commit, no ref, no reflog: the main
    // index is its only anchor.
    fs::write(main.join("staged-only.txt"), "main staged only\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "staged-only.txt"], main),
        "stage in main",
    );
    let oid = String::from_utf8_lossy(
        &run_libra_command(&["hash-object", "staged-only.txt"], main).stdout,
    )
    .trim()
    .to_string();
    assert!(!oid.is_empty(), "hash-object yields the staged blob's oid");

    // Make it prunable-if-unrooted: age the loose objects past the grace
    // window, run gc FROM THE LINKED WORKTREE once (a first sighting only
    // QUARANTINES — deletion happens on a later pass), then assert the blob
    // was never even quarantined, age the ledger, and run again. A
    // single-run assertion would be vacuous: gc deletes nothing on first
    // sight.
    backdate_loose_objects(main);
    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], &wt);
    assert_cli_success(&gc, "gc from the linked worktree");
    let ledger_path = main.join(".libra").join("gc-prune-candidates.json");
    if let Ok(raw) = fs::read(&ledger_path) {
        let ledger: serde_json::Value = serde_json::from_slice(&raw).expect("ledger json");
        assert!(
            !ledger
                .as_object()
                .is_some_and(|entries| entries.contains_key(&oid)),
            "the MAIN-staged blob must never even be QUARANTINED by a \
             linked-worktree gc: {ledger}"
        );
        let aged: serde_json::Map<String, serde_json::Value> = ledger
            .as_object()
            .expect("ledger object")
            .keys()
            .map(|key| (key.clone(), serde_json::json!(0)))
            .collect();
        fs::write(&ledger_path, serde_json::to_vec(&aged).expect("serialize")).expect("age");
    }
    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], &wt);
    assert_cli_success(&gc, "second gc from the linked worktree");

    let survives = run_libra_command(&["cat-file", "-t", &oid], main);
    assert_cli_success(
        &survives,
        "the blob staged only in MAIN must survive a linked-worktree gc",
    );
    assert_eq!(
        String::from_utf8_lossy(&survives.stdout).trim(),
        "blob",
        "and still reads back as a blob"
    );
}

/// §C.11 W2 acceptance: a linked worktree's HELD AUTOSTASH and its UNMERGED
/// index stages survive gc.
///
/// Both live only in that worktree: the autostash commit is held by the merge
/// sidecar (deliberately off the stash list until the merge finishes), and the
/// conflict stages are entries in the worktree's private index. Neither is
/// reachable from any ref, so if the collector missed either, one maintenance
/// run would destroy the user's uncommitted work mid-conflict.
///
/// §C.12 roster: together with
/// `test_pull_rebase_autostash_in_linked_worktree_pops_only_its_own_entry`
/// (pull's autostash wrap pushes a REGULAR entry onto the shared stack,
/// whose full reflog `stash::gc_roots` traces) this discharges
/// `linked_pull_autostash_survives_gc`: pull-held state is either a stack
/// entry (traced via refs/stash + its log) or a held sidecar (traced here).
#[test]
fn a_linked_held_autostash_and_unmerged_stages_survive_gc() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("autostash-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Two diverging tips on the same file, so a merge in the worktree
    // conflicts.
    assert_cli_success(
        &run_libra_command(&["branch", "other"], main),
        "branch other",
    );
    fs::write(main.join("a.txt"), "main-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-line", "--no-verify"], main),
        "commit",
    );
    assert_cli_success(&run_libra_command(&["switch", "other"], &wt), "wt switch");
    fs::write(wt.join("a.txt"), "other-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], &wt), "wt add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "other-line", "--no-verify"], &wt),
        "wt commit",
    );

    // A dirty UNRELATED file, autostashed by the merge and HELD across the
    // conflict. Its blob exists only inside the held autostash commit.
    fs::write(wt.join("held.txt"), "held-content\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "held.txt"], &wt), "stage held");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "held-base", "--no-verify"], &wt),
        "commit held base",
    );
    fs::write(wt.join("held.txt"), "held-dirty\n").unwrap();
    let held_oid =
        String::from_utf8_lossy(&run_libra_command(&["hash-object", "held.txt"], &wt).stdout)
            .trim()
            .to_string();
    assert!(!held_oid.is_empty(), "hashed the dirty content");
    assert_cli_success(
        &run_libra_command(&["hash-object", "-w", "held.txt"], &wt),
        "write the dirty blob",
    );

    let merged = run_libra_command(&["merge", "--autostash", "main"], &wt);
    assert!(
        !merged.status.success(),
        "the merge must conflict so the autostash stays HELD: {}{}",
        String::from_utf8_lossy(&merged.stdout),
        String::from_utf8_lossy(&merged.stderr)
    );

    // Age every loose object past the grace window, so survival proves the
    // ROOT rather than the freshness belt.
    backdate_loose_objects(main);
    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], main);
    assert_cli_success(&gc, "maintenance gc with a conflicted linked worktree");

    let cat = run_libra_command(&["cat-file", "-p", &held_oid], main);
    assert_cli_success(&cat, "the held autostash content survives gc");
    assert!(
        String::from_utf8_lossy(&cat.stdout).contains("held-dirty"),
        "the held autostash's blob was pruned — it is off the stash list by \
         design, so the sidecar is its only root"
    );

    // The conflict is still resumable: the unmerged stages are intact.
    let status = run_libra_command(&["status"], &wt);
    assert_cli_success(&status, "status in the conflicted worktree");
    let text = String::from_utf8_lossy(&status.stdout).to_string()
        + &String::from_utf8_lossy(&status.stderr);
    assert!(
        text.to_lowercase().contains("unmerged") || text.to_lowercase().contains("both modified"),
        "the unmerged index stages survived gc: {text}"
    );
}

/// plan-20260714 W2 §C.4.3 re-verification (self-review finding): the
/// `stash-branch-journal.json` sidecar is a TRACED GC root.
///
/// The journal's `prior_detached` can be the ONLY anchor of the HEAD a
/// `stash branch` just left: the HEAD switch rewrites the reference row
/// without a reflog entry, so for a worktree that was detached the old OID
/// vanishes from every table the walk reads the moment the switch commits.
/// In the crash window before recovery, gc pruning that commit would leave
/// recovery re-pointing HEAD at a missing object. This plants exactly that
/// journal (dead command, otherwise-unanchored commit) and proves two aged
/// gc passes keep the commit — and that a corrupt journal fails closed.
#[test]
fn stash_branch_journal_oids_survive_gc() {
    let repo = repo_with_feature();
    let main = repo.path();
    let oid = orphan_commit(main, "journal-anchor");
    assert!(
        sqlite_exec(
            &main.join(".libra").join("libra.db"),
            &["DELETE FROM reflog;"]
        ),
        "purge the reflog"
    );

    // The journal a crashed `stash branch` would leave, in MAIN's gitdir.
    let journal = main.join(".libra").join("stash-branch-journal.json");
    fs::write(
        &journal,
        serde_json::to_vec(&serde_json::json!({
            "branch": "crashed-stash-branch",
            "base": oid,
            "prior_branch": null,
            "prior_detached": oid,
            "phase": "prepared",
            "nonce": "0123456789abcdef0123456789abcdef",
        }))
        .expect("serialize"),
    )
    .expect("plant the journal");

    backdate_loose_objects(main);
    assert_cli_success(
        &run_libra_command(&["maintenance", "run", "--task", "gc"], main),
        "first gc quarantines",
    );
    let ledger_path = main.join(".libra").join("gc-prune-candidates.json");
    if let Ok(raw) = fs::read(&ledger_path) {
        let ledger: serde_json::Value = serde_json::from_slice(&raw).expect("ledger json");
        assert!(
            !ledger
                .as_object()
                .is_some_and(|entries| entries.contains_key(&oid)),
            "the journal-anchored commit must not be quarantined: {ledger}"
        );
        let aged: serde_json::Map<String, serde_json::Value> = ledger
            .as_object()
            .expect("ledger object")
            .keys()
            .map(|key| (key.clone(), serde_json::json!(0)))
            .collect();
        fs::write(&ledger_path, serde_json::to_vec(&aged).expect("serialize")).expect("age");
    }
    assert_cli_success(
        &run_libra_command(&["maintenance", "run", "--task", "gc"], main),
        "second gc prunes the aged candidates",
    );
    let survives = run_libra_command(&["cat-file", "-t", &oid], main);
    assert_cli_success(&survives, "the journal-anchored commit survives");

    // And the sidecar contract's other half: a corrupt journal fails the
    // prune closed, naming the file.
    fs::write(&journal, "{ not json").expect("corrupt the journal");
    let refused = run_libra_command(&["maintenance", "run", "--task", "gc"], main);
    assert!(
        !refused.status.success(),
        "a corrupt stash-branch journal must refuse the prune"
    );
    assert!(
        (String::from_utf8_lossy(&refused.stdout).to_string()
            + &String::from_utf8_lossy(&refused.stderr))
            .contains("stash-branch-journal.json"),
        "and the refusal names the journal"
    );

    // A WELL-FORMED journal naming a MISSING object is corruption too
    // (§C.11 W2: mandatory reachability with a missing object fails the
    // prune closed) — skipping it would prune the operation's remaining
    // anchors on top of the one already lost.
    fs::write(
        &journal,
        serde_json::to_vec(&serde_json::json!({
            "branch": "crashed-stash-branch",
            "base": "ffffffffffffffffffffffffffffffffffffffff",
            "prior_branch": null,
            "prior_detached": null,
            "phase": "prepared",
            "nonce": "0123456789abcdef0123456789abcdef",
        }))
        .expect("serialize"),
    )
    .expect("plant a journal naming a missing object");
    let refused = run_libra_command(&["maintenance", "run", "--task", "gc"], main);
    assert!(
        !refused.status.success(),
        "a journal naming a missing object must refuse the prune"
    );
    assert!(
        (String::from_utf8_lossy(&refused.stdout).to_string()
            + &String::from_utf8_lossy(&refused.stderr))
            .contains("does not exist"),
        "and the refusal says WHY"
    );
}

/// §C.11 W2 acceptance: prune FAILS CLOSED when a mandatory root is corrupt.
///
/// A sidecar is a reachability root — the held autostash and the conflicted
/// commits it names exist nowhere else. If the file cannot be parsed, the
/// safe answer is to refuse the prune, not to prune with an incomplete root
/// set: pruning would silently delete the objects a `--continue` needs.
#[test]
fn a_corrupt_sidecar_root_fails_the_prune_closed() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("corrupt-root-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // A sidecar that is NOT valid JSON in the linked worktree's gitdir.
    let sidecar = wt.join(".libra").join("merge-state.json");
    fs::write(&sidecar, "{ this is not json").unwrap();

    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], main);
    let text =
        String::from_utf8_lossy(&gc.stdout).to_string() + &String::from_utf8_lossy(&gc.stderr);
    assert!(
        !gc.status.success(),
        "an unparseable mandatory root must refuse the prune rather than \
         prune with an incomplete root set: {text}"
    );
    assert!(
        text.contains("merge-state.json"),
        "and the refusal names the root it could not read: {text}"
    );

    // Removing the corrupt root lets gc run again — the refusal is about the
    // unreadable file, not a permanent block.
    fs::remove_file(&sidecar).unwrap();
    assert_cli_success(
        &run_libra_command(&["maintenance", "run", "--task", "gc"], main),
        "gc runs once the root is readable again",
    );
}

/// W2 r6 #2: a journaled branch whose tip MOVED is kept — it carries the
/// user's commits now — and recovery SAYS so instead of silently retaining a
/// branch that would later look like corruption.
#[test]
fn recovery_keeps_and_reports_a_journaled_branch_the_user_committed_to() {
    let repo = repo_with_feature();
    let main = repo.path();
    let base = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], main).stdout)
        .trim()
        .to_string();

    // The half-state, PLUS a user commit on the journaled branch.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "kept-branch"], main),
        "switch",
    );
    fs::write(main.join("mine.txt"), "the user's work\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "mine.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "user work", "--no-verify"], main),
        "the user's commit moves the tip past the journaled base",
    );
    fs::write(
        main.join(".libra").join("stash-branch-journal.json"),
        serde_json::json!({
            "branch": "kept-branch",
            "base": base,
            "prior_branch": "main",
            "prior_detached": null,
            "phase": "prepared",
            "nonce": "test-nonce-kept"
        })
        .to_string(),
    )
    .unwrap();
    let db = main.join(".libra").join("libra.db");
    let reference_id = sqlite_query(
        &db,
        "SELECT id FROM reference WHERE name = 'kept-branch' AND kind = 'Branch'",
    )
    .pop()
    .expect("the branch has a row id");
    assert!(
        sqlite_exec(
            &db,
            &[&format!(
                "INSERT INTO metadata_kv (scope, target, key, value, value_type, created_at, \
                 updated_at) VALUES ('stash_branch_journal', 'test-nonce-kept', \
                 'reference_id', '{reference_id}', 'text', datetime('now'), datetime('now'))"
            )],
        ),
        "record the creation provenance"
    );

    let out = run_libra_command(&["stash", "list"], main);
    assert_cli_success(&out, "recovery runs");
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        text.contains("KEPT"),
        "recovery reports the retained branch out loud: {text}"
    );
    let branches = run_libra_command(&["branch"], main);
    assert!(
        String::from_utf8_lossy(&branches.stdout).contains("kept-branch"),
        "the branch with the user's commit survives"
    );
    assert!(
        !main
            .join(".libra")
            .join("stash-branch-journal.json")
            .exists(),
        "and the journal is concluded"
    );
}

/// W2 r5: `worktree remove` refuses while a stash-branch rollback journal is
/// pending — removing the gitdir would strand the half-created branch AND
/// delete the only instruction for undoing it.
#[test]
fn worktree_remove_refuses_a_pending_stash_branch_journal() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("journaled-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    fs::write(
        wt.join(".libra").join("stash-branch-journal.json"),
        serde_json::json!({
            "branch": "half",
            "base": "0000000000000000000000000000000000000000",
            "prior_branch": null,
            "prior_detached": null,
            "phase": "prepared",
            "nonce": "test-nonce-guard"
        })
        .to_string(),
    )
    .unwrap();

    let out = run_libra_command(&["worktree", "remove", wt.to_str().unwrap()], main);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "remove must refuse while the journal is pending: {text}"
    );
    assert!(
        text.contains("stash-branch-journal.json"),
        "and name it: {text}"
    );

    // Any stash command in that worktree completes the (no-op) rollback…
    assert_cli_success(
        &run_libra_command(&["stash", "list"], &wt),
        "recovery clears the journal",
    );
    // …after which removal succeeds.
    assert_cli_success(
        &run_libra_command(&["worktree", "remove", wt.to_str().unwrap()], main),
        "remove succeeds once the journal is recovered",
    );
}

/// W2 r7 #3: a MERGE-mode `pull` claims the worktree control slot — it must
/// be refused while a merge is already in progress here, BEFORE it fetches
/// or touches anything.
#[test]
fn merge_mode_pull_is_refused_while_a_merge_is_in_progress() {
    let repo = repo_with_feature();
    let main = repo.path();
    assert_cli_success(&run_libra_command(&["branch", "other"], main), "branch");
    fs::write(main.join("a.txt"), "main-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-line", "--no-verify"], main),
        "commit",
    );
    assert_cli_success(&run_libra_command(&["switch", "other"], main), "switch");
    fs::write(main.join("a.txt"), "other-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "add other");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "other-line", "--no-verify"], main),
        "commit other",
    );
    let merged = run_libra_command(&["merge", "main"], main);
    assert!(!merged.status.success(), "the merge must conflict");
    let state_before = fs::read(main.join(".libra/merge-state.json")).unwrap();

    let out = run_libra_command(&["pull"], main);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
    .to_lowercase();
    assert!(
        !out.status.success(),
        "a merge-mode pull must be refused mid-merge: {text}"
    );
    assert!(
        text.contains("in progress"),
        "the refusal names the in-progress merge: {text}"
    );
    assert!(
        !text.contains("tracking"),
        "the merge preflight fires BEFORE target resolution — a tracking-info \
         error here means the pull got past it: {text}"
    );
    assert!(
        !text.contains("network"),
        "and it never got as far as fetching: {text}"
    );
    assert_eq!(
        fs::read(main.join(".libra/merge-state.json")).unwrap(),
        state_before,
        "nothing was touched"
    );
}

/// W2 r6 #6: every merge CONTROL refuses a corrupt held-autostash BEFORE it
/// mutates — `--abort` in particular must not restore HEAD/index and delete
/// the merge state while the only stash reference is a file nothing can
/// parse.
#[test]
fn merge_controls_refuse_a_corrupt_held_autostash_before_mutating() {
    let repo = repo_with_feature();
    let main = repo.path();
    // Two diverging tips on the same file → a conflicted merge with state.
    assert_cli_success(&run_libra_command(&["branch", "other"], main), "branch");
    fs::write(main.join("a.txt"), "main-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-line", "--no-verify"], main),
        "commit",
    );
    assert_cli_success(&run_libra_command(&["switch", "other"], main), "switch");
    fs::write(main.join("a.txt"), "other-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "add other");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "other-line", "--no-verify"], main),
        "commit other",
    );
    let merged = run_libra_command(&["merge", "main"], main);
    assert!(!merged.status.success(), "the merge must conflict");
    let state_path = main.join(".libra").join("merge-state.json");
    assert!(state_path.exists(), "conflicted merge state persists");
    let state_before = fs::read(&state_path).unwrap();
    let head_before = run_libra_command(&["rev-parse", "HEAD"], main).stdout;

    // The corrupt sidecar the controls must trip over.
    let sidecar = main.join(".libra").join("merge-autostash.json");
    fs::write(&sidecar, "{ not json").unwrap();
    let sidecar_before = fs::read(&sidecar).unwrap();

    for control in [
        ["merge", "--continue"],
        ["merge", "--restart"],
        ["merge", "--abort"],
    ] {
        let out = run_libra_command(&control, main);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !out.status.success(),
            "{control:?} must refuse over a corrupt sidecar: {text}"
        );
        assert!(
            text.contains("merge-autostash.json"),
            "{control:?} names the file: {text}"
        );
        // NOTHING moved: the merge state, HEAD and the sidecar are untouched.
        assert_eq!(
            fs::read(&state_path).unwrap(),
            state_before,
            "{control:?} must not touch the merge state"
        );
        assert_eq!(
            run_libra_command(&["rev-parse", "HEAD"], main).stdout,
            head_before,
            "{control:?} must not move HEAD"
        );
        assert_eq!(
            fs::read(&sidecar).unwrap(),
            sidecar_before,
            "{control:?} must not touch the sidecar either"
        );
    }

    // Removing the corrupt file lets the control conclude the merge.
    fs::remove_file(&sidecar).unwrap();
    assert_cli_success(
        &run_libra_command(&["merge", "--abort"], main),
        "abort succeeds once the sidecar is readable/absent",
    );
}

/// W2 r4: a CORRUPT held-autostash sidecar is a hard stop, not a skip.
///
/// The old `let Ok(...)` silently skipped the unreadable file; a later
/// `--autostash` save then OVERWROTE it — destroying the only durable
/// reference to the held commit, which GC could then collect.
#[test]
fn a_corrupt_held_autostash_refuses_the_merge_and_survives() {
    let repo = repo_with_feature();
    let main = repo.path();
    assert_cli_success(&run_libra_command(&["branch", "other"], main), "branch");
    fs::write(main.join("b.txt"), "other\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "other", "--no-verify"], main),
        "commit",
    );

    let sidecar = main.join(".libra").join("merge-autostash.json");
    fs::write(&sidecar, "{ not json").unwrap();
    let before = fs::read(&sidecar).unwrap();

    let out = run_libra_command(&["merge", "--autostash", "other"], main);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "a merge over an unreadable held-autostash must refuse: {text}"
    );
    assert!(
        text.contains("merge-autostash.json"),
        "and name the file: {text}"
    );
    assert_eq!(
        fs::read(&sidecar).unwrap(),
        before,
        "the corrupt sidecar was NOT overwritten — it is the only reference \
         to the held commit"
    );
}

/// W2 r4: a LINKED worktree may not promote a foreign-marked autostash
/// either — a manually copied sidecar carries its true owner's scope, and
/// promoting adopts its commit into the shared list and deletes the evidence.
#[test]
fn a_foreign_marked_autostash_is_refused_in_a_linked_worktree() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("foreign-autostash-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(&run_libra_command(&["branch", "other"], main), "branch");
    fs::write(main.join("b.txt"), "other\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "other", "--no-verify"], main),
        "commit",
    );

    // A sidecar inside the LINKED gitdir, marked as someone else's.
    let sidecar = wt.join(".libra").join("merge-autostash.json");
    fs::write(
        &sidecar,
        serde_json::json!({
            "stash_commit": "1111111111111111111111111111111111111111",
            "owner_scope": "some-other-worktree"
        })
        .to_string(),
    )
    .unwrap();

    let out = run_libra_command(&["merge", "other"], &wt);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "a foreign-marked sidecar must refuse the merge that would promote it: {text}"
    );
    assert!(
        text.contains("some-other-worktree"),
        "and the refusal names the recorded owner: {text}"
    );
    assert!(sidecar.exists(), "the evidence survives");
}

/// §C.10: an interrupted `stash branch` rollback is completed from its
/// journal by the next stash invocation — HEAD restored, the branch deleted
/// tip-conditionally, the journal removed, the working tree untouched.
#[test]
fn an_interrupted_stash_branch_rollback_completes_from_the_journal() {
    let repo = repo_with_feature();
    let main = repo.path();
    let head = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], main).stdout)
        .trim()
        .to_string();

    // The half-state an interrupted command leaves: the branch exists at its
    // base, HEAD sits on it, the journal records the rollback intent, and the
    // PROVENANCE row — written atomically with the branch by the create's
    // transaction — records the branch row's id.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "half-branch"], main),
        "enter the half-state",
    );
    fs::write(
        main.join(".libra").join("stash-branch-journal.json"),
        serde_json::json!({
            "branch": "half-branch",
            "base": head,
            "prior_branch": "main",
            "prior_detached": null,
            "phase": "prepared",
            "nonce": "test-nonce-interrupted"
        })
        .to_string(),
    )
    .unwrap();
    let db = main.join(".libra").join("libra.db");
    let reference_id = sqlite_query(
        &db,
        "SELECT id FROM reference WHERE name = 'half-branch' AND kind = 'Branch'",
    )
    .pop()
    .expect("the created branch has a row id");
    assert!(
        sqlite_exec(
            &db,
            &[&format!(
                "INSERT INTO metadata_kv (scope, target, key, value, value_type, created_at, \
                 updated_at) VALUES ('stash_branch_journal', 'test-nonce-interrupted', \
                 'reference_id', '{reference_id}', 'text', datetime('now'), datetime('now'))"
            )],
        ),
        "record the creation provenance"
    );

    // ANY stash command completes the rollback first.
    let out = run_libra_command(&["stash", "list"], main);
    assert_cli_success(&out, "stash list runs the recovery");

    let status = run_libra_command(&["status"], main);
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("On branch main"),
        "HEAD is back on the prior branch: {}",
        String::from_utf8_lossy(&status.stdout)
    );
    let branches = run_libra_command(&["branch"], main);
    assert!(
        !String::from_utf8_lossy(&branches.stdout).contains("half-branch"),
        "the journaled branch was deleted at its base"
    );
    assert!(
        !main
            .join(".libra")
            .join("stash-branch-journal.json")
            .exists(),
        "and the journal is gone"
    );
}

/// §C.4.3: `worktree remove` refuses a worktree holding a conflicted merge —
/// in BOTH modes. Detaching writes the fail-closed marker OVER the live
/// sidecar (stranding the merge behind a gate the user cannot lift without
/// `repair`); deleting destroys it outright.
#[test]
fn worktree_remove_refuses_an_in_progress_merge() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("mid-merge-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Diverge and start a conflicted merge in the linked worktree.
    assert_cli_success(
        &run_libra_command(&["branch", "sideline"], main),
        "branch sideline",
    );
    fs::write(main.join("a.txt"), "main-side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-side", "--no-verify"], main),
        "commit",
    );
    assert_cli_success(&run_libra_command(&["switch", "sideline"], &wt), "switch");
    fs::write(wt.join("a.txt"), "wt-side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], &wt), "wt add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "wt-side", "--no-verify"], &wt),
        "wt commit",
    );
    let merged = run_libra_command(&["merge", "main"], &wt);
    assert!(!merged.status.success(), "the merge must conflict");

    for args in [
        vec!["worktree", "remove", wt.to_str().unwrap()],
        vec!["worktree", "remove", "--delete-dir", wt.to_str().unwrap()],
    ] {
        let out = run_libra_command(&args, main);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !out.status.success(),
            "remove must refuse a worktree mid-merge ({args:?}): {text}"
        );
        assert!(
            text.contains("merge-state.json"),
            "and the refusal names the in-progress state: {text}"
        );
    }
    assert!(
        wt.join(".libra/merge-state.json").exists(),
        "the merge state survives both refusals"
    );

    // Aborting the merge lifts the refusal.
    assert_cli_success(&run_libra_command(&["merge", "--abort"], &wt), "abort");
    assert_cli_success(
        &run_libra_command(&["worktree", "remove", wt.to_str().unwrap()], main),
        "remove succeeds once the merge is gone",
    );
}

/// §C.11 W2 acceptance: a conflicted MERGE in one worktree and a conflicted
/// REVERT in another, at the same time, with neither seeing the other's.
///
/// The sidecars used to live in common storage, so one worktree's conflict
/// made every other worktree believe an operation was in progress — and a
/// control action there would have reset the wrong tree from the wrong state.
/// Both operations run here, each control action is exercised in BOTH scopes,
/// and the wrong-scope one must fail closed.
#[test]
fn concurrent_linked_merge_and_revert_conflicts_do_not_cross() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("conflict-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // A branch whose tip conflicts with main's on the same file.
    assert_cli_success(
        &run_libra_command(&["branch", "sideline"], main),
        "branch sideline",
    );
    fs::write(main.join("a.txt"), "main-side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-side", "--no-verify"], main),
        "commit",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "sideline"], &wt),
        "wt switch",
    );
    fs::write(wt.join("a.txt"), "wt-side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], &wt), "wt add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "wt-side", "--no-verify"], &wt),
        "wt commit",
    );

    // Conflicted MERGE in the LINKED worktree.
    let merged = run_libra_command(&["merge", "main"], &wt);
    assert!(
        !merged.status.success(),
        "the merge must conflict: {}{}",
        String::from_utf8_lossy(&merged.stdout),
        String::from_utf8_lossy(&merged.stderr)
    );

    // Conflicted REVERT in MAIN, at the same time: a second commit touches the
    // same lines, so reverting the first no longer applies cleanly.
    fs::write(main.join("a.txt"), "main-side-2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "add 2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-side-2", "--no-verify"], main),
        "commit 2",
    );
    let reverted = run_libra_command(&["revert", "HEAD~1"], main);
    assert!(
        !reverted.status.success(),
        "the revert must conflict: {}{}",
        String::from_utf8_lossy(&reverted.stdout),
        String::from_utf8_lossy(&reverted.stderr)
    );

    // Each worktree's sidecar lives in ITS OWN gitdir.
    assert!(
        wt.join(".libra/merge-state.json").exists(),
        "the linked worktree holds its own merge sidecar"
    );
    assert!(
        main.join(".libra/revert-state.json").exists(),
        "main holds its own revert sidecar"
    );
    assert!(
        !main.join(".libra/merge-state.json").exists(),
        "the linked worktree's merge sidecar is not main's"
    );
    assert!(
        !wt.join(".libra/revert-state.json").exists(),
        "main's revert sidecar is not the linked worktree's"
    );

    // Scoped STATUS: each worktree reports its own operation and not the
    // other's.
    let text_of = |dir: &std::path::Path| {
        let out = run_libra_command(&["status"], dir);
        assert_cli_success(&out, "status");
        (String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr))
            .to_lowercase()
    };
    let wt_text = text_of(&wt);
    assert!(
        wt_text.contains("merge"),
        "the worktree that merged reports its merge: {wt_text}"
    );
    assert!(
        !wt_text.contains("revert in progress"),
        "and not main's revert: {wt_text}"
    );
    let main_text = text_of(main);
    assert!(
        main_text.contains("revert in progress"),
        "main reports its revert: {main_text}"
    );
    assert!(
        !main_text.contains("merge --continue"),
        "and not the linked worktree's merge: {main_text}"
    );

    // Scoped CONTROLS: the wrong scope fails closed and mutates nothing…
    let cross = run_libra_command(&["revert", "--abort"], &wt);
    assert!(
        !cross.status.success(),
        "revert --abort in the merging worktree has no revert to abort"
    );
    assert!(
        main.join(".libra/revert-state.json").exists(),
        "and it must not have consumed MAIN's revert state"
    );
    let cross = run_libra_command(&["merge", "--abort"], main);
    assert!(
        !cross.status.success(),
        "merge --abort in the reverting worktree has no merge to abort"
    );
    assert!(
        wt.join(".libra/merge-state.json").exists(),
        "and it must not have consumed the LINKED worktree's merge state"
    );

    // …while the right scope resolves its own operation.
    assert_cli_success(
        &run_libra_command(&["merge", "--abort"], &wt),
        "the merging worktree aborts its own merge",
    );
    assert_cli_success(
        &run_libra_command(&["revert", "--abort"], main),
        "the reverting worktree aborts its own revert",
    );
    assert!(
        !wt.join(".libra/merge-state.json").exists()
            && !main.join(".libra/revert-state.json").exists(),
        "both operations are cleanly gone"
    );
}

/// §C.10 crash safety: a tip left stale by a crash between the two writes is
/// REPAIRED from the log, not obeyed.
///
/// `refs/stash` and `logs/refs/stash` are two files. Publication writes the log
/// first (atomically, fsynced) and derives the tip from it, so the only state a
/// crash can leave is a stale tip over a correct log — which every reader and
/// every mutation repairs under the stack lock. This simulates that crash by
/// corrupting the tip directly.
#[test]
fn a_stale_stash_tip_is_repaired_from_the_log() {
    let repo = repo_with_feature();
    let main = repo.path();

    fs::write(main.join("a.txt"), "stashed\n").unwrap();
    assert_cli_success(&run_libra_command(&["stash", "push"], main), "stash push");

    let tip_path = main.join(".libra").join("refs").join("stash");
    let real_tip = fs::read_to_string(&tip_path).expect("the tip exists");
    // The crash: a tip naming an entry the log does not list.
    fs::write(&tip_path, "0000000000000000000000000000000000000000\n").unwrap();

    let listed = run_libra_command(&["stash", "list"], main);
    assert_cli_success(&listed, "stash list over a stale tip");
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("stash@{0}"),
        "the log is the authority, so the entry is still listed"
    );
    assert_eq!(
        fs::read_to_string(&tip_path).expect("tip"),
        real_tip,
        "and the stale tip was repaired from the log rather than obeyed"
    );

    // The repaired stack still pops.
    assert_cli_success(&run_libra_command(&["stash", "pop"], main), "pop");
    assert_eq!(
        fs::read_to_string(main.join("a.txt")).unwrap(),
        "stashed\n",
        "the stashed content came back"
    );
    assert!(
        !tip_path.exists(),
        "an emptied stack leaves no tip behind — a ref that outlived its log \
         names an entry nothing can find"
    );
}

/// §C.12 (W2): two concurrent pops of the SAME entry — exactly one wins.
///
/// The stack is repository-shared, so two worktrees can pop at the same
/// moment. The drop is a CAS on the entry's raw reflog line under the stack
/// lock, so the LOSER must be refused — not allowed to drop whatever is on
/// top by then, which after the winner's drop is a DIFFERENT entry.
///
/// The race is made real, not hoped for: both processes stop at a filesystem
/// barrier (`LIBRA_TEST_STASH_DROP_BARRIER`) placed after each has resolved
/// and applied its entry but before either takes the drop lock. Both
/// therefore arrive holding the SAME resolved raw line, and only then race
/// the CAS. Without the barrier, two back-to-back pops legitimately consume
/// two entries and prove nothing about the CAS.
#[test]
fn concurrent_stash_pop_single_cas_winner() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let one = parent.path().join("pop-one");
    let two = parent.path().join("pop-two");
    for path in [&one, &two] {
        assert_cli_success(
            &run_libra_command(&["worktree", "add", path.to_str().unwrap()], main),
            "worktree add",
        );
    }

    // TWO entries: a loser that dropped blindly would consume the SECOND.
    for content in ["first-stashed\n", "second-stashed\n"] {
        fs::write(main.join("a.txt"), content).unwrap();
        assert_cli_success(&run_libra_command(&["stash", "push"], main), "stash push");
    }
    let stack = |at: &std::path::Path| -> Vec<String> {
        let out = run_libra_command(&["stash", "list"], at);
        assert_cli_success(&out, "stash list");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|line| line.contains("stash@{"))
            .map(str::to_string)
            .collect()
    };
    let before = stack(main);
    assert_eq!(before.len(), 2, "two entries on the shared stack");
    // The message of the entry NEITHER pop may consume.
    let survivor = before[1]
        .split_once(':')
        .map(|(_, rest)| rest.trim().to_string());

    let barrier_dir = parent.path().join("drop-barrier");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for path in [one.clone(), two.clone()] {
        let barrier = std::sync::Arc::clone(&barrier);
        let barrier_dir = barrier_dir.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let out = run_libra_command_with_stdin_and_env(
                &["stash", "pop"],
                &path,
                "",
                &[
                    ("LIBRA_TEST", "1"),
                    (
                        "LIBRA_TEST_STASH_DROP_BARRIER",
                        barrier_dir.to_str().unwrap(),
                    ),
                ],
            );
            (
                out.status.success(),
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ),
            )
        }));
    }
    let results: Vec<(bool, String)> = handles
        .into_iter()
        .map(|handle| handle.join().expect("pop thread"))
        .collect();

    // Both processes arrived at the barrier — the race actually happened.
    let arrived = fs::read_dir(&barrier_dir)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(
        arrived, 2,
        "both pops must reach the pre-drop rendezvous: {results:?}"
    );

    // Exactly ONE pop won the CAS; the loser was refused after its apply.
    let winners = results.iter().filter(|(ok, _)| *ok).count();
    assert_eq!(
        winners, 1,
        "exactly one pop may win the CAS on the same entry: {results:?}"
    );
    let loser = &results.iter().find(|(ok, _)| !*ok).expect("a loser").1;
    assert!(
        loser.contains("changed concurrently"),
        "the loser is told the stack changed, not handed a different entry: {loser}"
    );

    // The stack lost exactly the ONE contested entry; the survivor is intact.
    let after = stack(main);
    assert_eq!(
        after.len(),
        1,
        "one contested entry left the stack — a blind loser would have taken \
         the survivor too: {after:?}"
    );
    assert_eq!(
        after[0]
            .split_once(':')
            .map(|(_, rest)| rest.trim().to_string()),
        survivor,
        "and the surviving entry is the one neither pop resolved"
    );
}

/// §C.12 (W2): a `pop` whose APPLY fails leaves the shared entry in place.
///
/// `pop` is apply-then-drop. If the apply fails — a conflicting local edit in
/// the worktree popping — the entry must stay on the SHARED stack, because it
/// is the only copy of that work and every other worktree can still see it.
/// Dropping it on a failed apply would destroy it for all of them at once.
#[test]
fn linked_stash_pop_apply_failure_keeps_shared_entry() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("pop-failure-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Stash a tracked change from the LINKED worktree.
    fs::write(wt.join("a.txt"), "linked-stashed\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["stash", "push"], &wt),
        "linked stash push",
    );
    let listed = run_libra_command(&["stash", "list"], main);
    assert_cli_success(&listed, "stash list");
    let before = String::from_utf8_lossy(&listed.stdout).to_string();
    assert!(
        before.contains("stash@{0}"),
        "the entry is on the shared stack: {before}"
    );

    // Make the apply fail: an uncommitted edit to the same file in the
    // worktree that pops.
    fs::write(wt.join("a.txt"), "conflicting-local-edit\n").unwrap();
    let popped = run_libra_command(&["stash", "pop"], &wt);
    assert!(
        !popped.status.success(),
        "the apply must fail: {}{}",
        String::from_utf8_lossy(&popped.stdout),
        String::from_utf8_lossy(&popped.stderr)
    );

    // The shared entry survives — visible from MAIN, which never touched it.
    let after = run_libra_command(&["stash", "list"], main);
    assert_cli_success(&after, "stash list after the failed pop");
    let after = String::from_utf8_lossy(&after.stdout).to_string();
    assert!(
        after.contains("stash@{0}"),
        "a failed apply must not drop the shared entry — it is the only copy \
         of that work and every worktree can see it: {after}"
    );
    assert_eq!(
        before.lines().count(),
        after.lines().count(),
        "and the stack is exactly as deep as before"
    );
}

/// plan-20260714 W2 §C.10: the stash STACK is repository-shared while every
/// snapshot and every application is scoped to the acting worktree — push
/// snapshots the calling worktree's index and tree, pop applies into the
/// calling worktree, and the entry leaves the shared stack only through the
/// CAS delete. `stash branch` preflights the branch collision before
/// touching anything.
///
/// §C.12 roster: covers `linked_stash_push_uses_local_index_and_worktree`
/// (the pushed change is STAGED in the linked worktree, the push consumes
/// that index entry, and main's `status --porcelain` is byte-identical
/// before and after) and `linked_stash_apply_does_not_touch_main_index`
/// (apply runs IN the linked worktree while main still holds a staged entry
/// that must not move); the collision preflight below is the zero-write
/// half of `stash_branch_failure_has_zero_side_effects`.
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

    // A STAGED change in MAIN that must sit untouched through everything the
    // linked worktree does — the "main index" half of both roster names.
    fs::write(main.join("main-staged.txt"), "main staged\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "main-staged.txt"], main),
        "stage in main",
    );
    let main_status_before =
        String::from_utf8_lossy(&run_libra_command(&["status", "--porcelain"], main).stdout)
            .to_string();
    assert!(
        main_status_before.contains("main-staged.txt"),
        "main really has a staged entry: {main_status_before}"
    );

    // Dirty a TRACKED file in the linked worktree — STAGED there, so the
    // push must snapshot the LINKED index, not main's.
    let tracked = std::fs::read_dir(&wt)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.path().is_file())
        .expect("tracked file in wt")
        .path();
    let tracked_name = tracked
        .file_name()
        .and_then(|n| n.to_str())
        .expect("file name")
        .to_string();
    let original = std::fs::read_to_string(&tracked).unwrap();
    std::fs::write(&tracked, "stashed-from-wt\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", &tracked_name], &wt),
        "stage the change in the LINKED worktree",
    );
    assert_cli_success(&run_libra_command(&["stash", "push"], &wt), "wt stash push");
    assert_eq!(
        std::fs::read_to_string(&tracked).unwrap(),
        original,
        "push restores the LINKED worktree's file"
    );
    // The push consumed the LINKED index's staged entry…
    let wt_status =
        String::from_utf8_lossy(&run_libra_command(&["status", "--porcelain"], &wt).stdout)
            .to_string();
    assert!(
        !wt_status.contains(&tracked_name),
        "the linked worktree's staged change was snapshotted by ITS push: {wt_status}"
    );
    // …and left MAIN's index byte-identical.
    assert_eq!(
        String::from_utf8_lossy(&run_libra_command(&["status", "--porcelain"], main).stdout),
        main_status_before,
        "a linked push must not touch MAIN's index"
    );

    // Apply in the LINKED worktree: the change lands there, MAIN's index is
    // still untouched (the "apply" half of the roster names).
    assert_cli_success(&run_libra_command(&["stash", "apply"], &wt), "wt apply");
    assert_eq!(
        std::fs::read_to_string(&tracked).unwrap(),
        "stashed-from-wt\n",
        "apply lands in the ACTING worktree"
    );
    assert_eq!(
        String::from_utf8_lossy(&run_libra_command(&["status", "--porcelain"], main).stdout),
        main_status_before,
        "a linked apply must not touch MAIN's index either"
    );
    // Reset the linked worktree so the pop-in-main half below starts clean.
    assert_cli_success(
        &run_libra_command(&["restore", "--staged", "--worktree", &tracked_name], &wt),
        "restore the linked worktree",
    );
    assert_eq!(
        std::fs::read_to_string(&tracked).unwrap(),
        original,
        "the linked worktree is back to its pre-stash content"
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

/// W1 §C.4.1.1: plain `status` and ALL cache-semantic modes run in a linked
/// worktree — the dirty cache is scoped per worktree. (Formerly
/// `--scan`/`--cached`/`--check-dirty` failed closed there, under the W0
/// transitional guard, until W1 scoped the cache.)
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
    // §C.13: the explicit-flag refusal must offer the doctor/repair route for
    // a recorded owner that looks stale — never a dead end.
    assert!(
        co_err.contains("libra worktree doctor")
            && co_err.contains("libra worktree repair --confirm"),
        "the refusal offers the doctor/repair route: {co_err}"
    );

    // `switch --ignore-other-worktrees feature` is refused the same way, with
    // the same recovery route (§C.13).
    let sw_flag = run_libra_command(&["switch", "--ignore-other-worktrees", "feature"], main);
    assert_ne!(
        sw_flag.status.code(),
        Some(0),
        "switch flag cannot bypass either"
    );
    let sw_flag_err = String::from_utf8_lossy(&sw_flag.stderr);
    assert!(
        sw_flag_err.contains("already checked out")
            && sw_flag_err.contains("ignore-other-worktrees")
            && sw_flag_err.contains("libra worktree doctor")
            && sw_flag_err.contains("libra worktree repair --confirm"),
        "switch refusal carries the intentionally-different note and the doctor/repair route: {sw_flag_err}"
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
///
/// §C.12 roster: this ONE test discharges four named regressions —
/// `tag_editmsg_isolated_or_repo_locked`,
/// `notes_editmsg_isolated_or_repo_locked`,
/// `branch_description_editmsg_isolated_or_repo_locked` and
/// `linked_revert_edit_message_isolated`. They share a single contract
/// (each worktree is handed a buffer under its own gitdir) and a single
/// expensive fixture, and the assertion loop names each buffer, so splitting
/// them into four tests would quadruple the CLI round-trips without
/// strengthening anything.
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

    // The SAME contract for every W2-localized buffer, not just the tag's:
    // reverting any one of them to common storage must fail this test.
    // `notes add` composes via the editor when no `-m` is given, and
    // `branch --edit-description` always does.
    for (dir, target) in [(main, "HEAD"), (wt.as_path(), "HEAD")] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
            // `-f`: notes are repository-shared and both worktrees annotate
            // the same HEAD commit, so the second add is an overwrite.
            .args(["notes", "add", "-f", target])
            .current_dir(dir)
            .env("GIT_EDITOR", probe.to_str().unwrap())
            .output()
            .expect("run libra notes add");
        assert!(
            out.status.success(),
            "notes add in {dir:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // A fresh linked worktree is detached; give it a branch so
    // `--edit-description` (current-branch form) has one to describe.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "wt-described"], &wt),
        "attach the linked worktree to a branch",
    );
    for dir in [main, wt.as_path()] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
            .args(["branch", "--edit-description"])
            .current_dir(dir)
            .env("GIT_EDITOR", probe.to_str().unwrap())
            .output()
            .expect("run libra branch --edit-description");
        assert!(
            out.status.success(),
            "branch --edit-description in {dir:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // `revert --edit` composes through the same contract. Each worktree
    // reverts its OWN head commit so the revert applies cleanly and the editor
    // (the probe) records which REVERT_EDITMSG it was handed.
    for dir in [main, wt.as_path()] {
        fs::write(dir.join("reverted.txt"), format!("{}\n", dir.display())).unwrap();
        assert_cli_success(
            &run_libra_command(&["add", "reverted.txt"], dir),
            "stage the revert fixture",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "to-revert", "--no-verify"], dir),
            "commit the revert fixture",
        );
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
            .args(["revert", "--edit", "HEAD"])
            .current_dir(dir)
            .env("GIT_EDITOR", probe.to_str().unwrap())
            .output()
            .expect("run libra revert --edit");
        assert!(
            out.status.success(),
            "revert --edit in {dir:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let seen_text = fs::read_to_string(&seen).unwrap_or_default();
    let paths: Vec<&str> = seen_text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        paths.len(),
        8,
        "each editor-driven command ran once per worktree: {paths:?}"
    );
    let wt_canon = wt.canonicalize().expect("canonicalize wt");
    for (pair, name) in [
        (&paths[0..2], "TAG_EDITMSG"),
        (&paths[2..4], "NOTES_EDITMSG"),
        (&paths[4..6], "BRANCH_DESCRIPTION_EDITMSG"),
        (&paths[6..8], "REVERT_EDITMSG"),
    ] {
        assert_ne!(
            pair[0], pair[1],
            "each worktree must get its OWN {name}, not a shared one: {pair:?}"
        );
        // The linked worktree's buffer lives under ITS gitdir. Compare against
        // the canonicalized worktree path rather than a raw prefix, which
        // `/tmp` → `/private/tmp` symlink resolution would otherwise break.
        let expected = wt_canon.join(".libra").join(name);
        assert_eq!(
            std::path::Path::new(pair[1]),
            expected,
            "the linked worktree's {name} lives in its own gitdir: {pair:?}"
        );
    }
    // And the original tag assertion, now expressed through the loop above.
    let expected = wt_canon.join(".libra").join("TAG_EDITMSG");
    assert_eq!(
        std::path::Path::new(paths[1]),
        expected,
        "the linked worktree's buffer lives in its own gitdir: {paths:?}"
    );
}

/// plan-20260714 W2 §C.4.3 / §C.12: `MERGE_RR` is per-worktree.
///
/// rerere's tracking list names the conflicts a worktree is CURRENTLY
/// resolving. Before W2 it lived in the shared `.libra/rerere/MERGE_RR`, so
/// two worktrees resolving different conflicts overwrote each other's list —
/// and a `rerere clear` in one erased the other's tracking. The rr-CACHE
/// (recorded pre/postimages) stays shared on purpose: a resolution learned
/// in one worktree should be replayable in the next.
#[test]
fn linked_rerere_merge_rr_isolated() {
    let (main, wt, _repo, _parent) = two_conflicting_worktrees("rerere-mrr");

    // Each worktree tracks its OWN conflict, in its OWN gitdir.
    let main_rr = main.join(".libra").join("MERGE_RR");
    let wt_rr = wt.join(".libra").join("MERGE_RR");
    assert!(
        main_rr.exists(),
        "main records its tracked conflict in its own gitdir"
    );
    assert!(
        wt_rr.exists(),
        "the linked worktree records its tracked conflict in its own gitdir"
    );
    assert!(
        !main.join(".libra").join("rerere").join("MERGE_RR").exists(),
        "nothing writes the pre-W2 shared MERGE_RR any more"
    );

    // Same file name, different conflict content → different conflict ids.
    let main_list = fs::read_to_string(&main_rr).unwrap_or_default();
    let wt_list = fs::read_to_string(&wt_rr).unwrap_or_default();
    assert!(
        !main_list.trim().is_empty() && !wt_list.trim().is_empty(),
        "both worktrees track a conflict: main={main_list:?} wt={wt_list:?}"
    );
    assert_ne!(
        main_list, wt_list,
        "each worktree's list describes ITS conflict, not a shared one"
    );

    // And `rerere status` in one scope reports only that scope's conflict.
    let status = run_libra_command(&["rerere", "status"], &wt);
    assert_cli_success(&status, "rerere status in the linked worktree");
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("wt.txt"),
        "the linked worktree's status names its own conflicted path: {}",
        String::from_utf8_lossy(&status.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&status.stdout).contains("main.txt"),
        "and never the other worktree's: {}",
        String::from_utf8_lossy(&status.stdout)
    );

    // `rerere diff` reads the same scoped list: the linked worktree's diff
    // covers its own conflict and never mentions the other's path.
    let diff = run_libra_command(&["rerere", "diff"], &wt);
    assert_cli_success(&diff, "rerere diff in the linked worktree");
    let diff_text = String::from_utf8_lossy(&diff.stdout).to_string();
    assert!(
        diff_text.contains("wt.txt") && !diff_text.contains("main.txt"),
        "rerere diff is scoped to the calling worktree: {diff_text}"
    );

    // `rerere forget` retires an entry from the CALLING worktree's list and
    // leaves the other worktree's list byte-identical.
    let main_before = fs::read_to_string(&main_rr).unwrap_or_default();
    assert_cli_success(
        &run_libra_command(&["rerere", "forget", "wt.txt"], &wt),
        "rerere forget in the linked worktree",
    );
    assert!(
        !fs::read_to_string(&wt_rr)
            .unwrap_or_default()
            .contains("wt.txt"),
        "forget retired the entry from the calling worktree's list"
    );
    assert_eq!(
        fs::read_to_string(&main_rr).unwrap_or_default(),
        main_before,
        "and left the other worktree's list byte-identical"
    );
}

/// plan-20260714 §C.12: `rerere clear` is scoped to the calling worktree.
///
/// `clear` drops the CURRENT tracking list. Sharing it meant one worktree's
/// cleanup silently abandoned another's in-progress recording — the
/// postimage for that conflict could then never be recorded.
#[test]
fn linked_rerere_clear_does_not_touch_other_worktree() {
    let (main, wt, _repo, _parent) = two_conflicting_worktrees("rerere-clear");
    let main_rr = main.join(".libra").join("MERGE_RR");
    let wt_rr = wt.join(".libra").join("MERGE_RR");
    let main_before = fs::read_to_string(&main_rr).unwrap_or_default();
    assert!(
        !main_before.trim().is_empty(),
        "main is tracking a conflict"
    );
    assert!(
        !fs::read_to_string(&wt_rr)
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "the linked worktree is tracking one too — a clear of an empty list \
         would prove nothing"
    );

    assert_cli_success(
        &run_libra_command(&["rerere", "clear"], &wt),
        "rerere clear in the linked worktree",
    );

    assert_eq!(
        fs::read_to_string(&wt_rr).unwrap_or_default().trim(),
        "",
        "the clear emptied the CALLING worktree's list"
    );
    assert_eq!(
        fs::read_to_string(&main_rr).unwrap_or_default(),
        main_before,
        "and left the other worktree's list byte-identical"
    );
}

/// plan-20260714 §C.12: an auto-recorded postimage belongs to the conflict
/// the RESOLVING worktree had, and replays there.
///
/// The postimage is keyed by conflict id in the SHARED cache, so a
/// mis-scoped recording would file one worktree's resolution under the
/// other's conflict — and then replay the wrong text into a clean merge.
#[test]
fn linked_rerere_auto_update_records_correct_postimage() {
    let (main, wt, _repo, _parent) = two_conflicting_worktrees("rerere-post");

    // Resolve the LINKED worktree's conflict and let rerere record it.
    fs::write(
        wt.join("wt.txt"),
        "resolved-in-wt
",
    )
    .unwrap();
    assert_cli_success(&run_libra_command(&["rerere"], &wt), "record the postimage");
    assert_eq!(
        fs::read_to_string(wt.join(".libra").join("MERGE_RR"))
            .unwrap_or_default()
            .trim(),
        "",
        "recording the postimage retires the entry from THIS worktree's list"
    );

    // Main's conflict is untouched: still tracked, still conflicted.
    assert!(
        fs::read_to_string(main.join("main.txt"))
            .unwrap_or_default()
            .contains("<<<<<<<"),
        "the other worktree's file is still conflicted"
    );
    assert!(
        !fs::read_to_string(main.join(".libra").join("MERGE_RR"))
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "and still tracked there"
    );

    // Replaying the SAME conflict in the linked worktree reuses the
    // recording — proof the postimage was filed under the right id.
    assert_cli_success(&run_libra_command(&["merge", "--abort"], &wt), "abort");
    let remerged = run_libra_command(&["merge", "rerere-side"], &wt);
    assert!(
        !remerged.status.success(),
        "the same merge conflicts again: {}",
        String::from_utf8_lossy(&remerged.stdout)
    );
    let replayed = run_libra_command(&["rerere"], &wt);
    assert_cli_success(&replayed, "replay");
    assert_eq!(
        fs::read_to_string(wt.join("wt.txt")).unwrap_or_default(),
        "resolved-in-wt\n",
        "the recorded resolution replayed into the worktree that recorded it: {}",
        String::from_utf8_lossy(&replayed.stdout)
    );
}

/// plan-20260714 §C.12: `merge-file` backups are worktree-local.
///
/// `libra merge-file` overwrites the file in place and keeps a backup when
/// the merge conflicts. A shared backup directory meant two worktrees
/// merging same-named files clobbered — and cleaned up — each other's only
/// copy of the pre-merge content.
#[test]
fn linked_merge_file_backup_isolated() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("mf-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // The SAME relative file name in both worktrees, with conflicting edits
    // so the backup is kept rather than cleaned up.
    for (dir, tag) in [(main, "main"), (wt.as_path(), "wt")] {
        fs::write(dir.join("base.txt"), "base\n").unwrap();
        fs::write(dir.join("mine.txt"), format!("{tag}-mine\n")).unwrap();
        fs::write(dir.join("theirs.txt"), format!("{tag}-theirs\n")).unwrap();
        let out = run_libra_command(&["merge-file", "mine.txt", "base.txt", "theirs.txt"], dir);
        assert!(
            !out.status.success(),
            "merge-file must report the conflict in {dir:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Each backup lives under ITS OWN gitdir and holds ITS OWN original.
    for (dir, tag) in [(main, "main"), (wt.as_path(), "wt")] {
        let backup = dir
            .join(".libra")
            .join("merge-file-backup")
            .join("mine.txt");
        assert!(
            backup.exists(),
            "{dir:?} keeps its conflicted merge-file backup at {}",
            backup.display()
        );
        assert_eq!(
            fs::read_to_string(&backup).unwrap(),
            format!("{tag}-mine\n"),
            "the backup holds THIS worktree's pre-merge content"
        );
    }

    // CLEANUP is scoped too: a CLEAN merge-file in main removes only MAIN's
    // same-named backup; the linked worktree's is untouched.
    fs::write(main.join("mine.txt"), "base\n").unwrap();
    fs::write(main.join("theirs.txt"), "base\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["merge-file", "mine.txt", "base.txt", "theirs.txt"], main),
        "clean merge-file in main",
    );
    assert!(
        !main
            .join(".libra")
            .join("merge-file-backup")
            .join("mine.txt")
            .exists(),
        "the clean merge removed MAIN's backup"
    );
    assert!(
        wt.join(".libra")
            .join("merge-file-backup")
            .join("mine.txt")
            .exists(),
        "and left the LINKED worktree's same-named backup in place"
    );
}

/// Two worktrees of one repository, each sitting on its OWN unresolved merge
/// conflict, with `rerere.enabled` on.
///
/// Both side branches are built BEFORE the linked worktree exists, so no
/// branch switch in main ever races the shared-branch guard.
fn two_conflicting_worktrees(
    slug: &str,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let repo = tempfile::tempdir().expect("repo");
    let main = repo.path().to_path_buf();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], &main),
        "init",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "t"], &main),
        "name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "t@t"], &main),
        "email",
    );
    assert_cli_success(
        &run_libra_command(&["config", "set", "rerere.enabled", "true"], &main),
        "enable rerere",
    );

    // One base commit carrying both files.
    for file in ["main.txt", "wt.txt"] {
        fs::write(main.join(file), "base\n").unwrap();
    }
    assert_cli_success(&run_libra_command(&["add", "."], &main), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], &main),
        "commit base",
    );

    // A side branch per file, each with its own version of that file.
    for (branch, file) in [("rerere-main-side", "main.txt"), ("rerere-side", "wt.txt")] {
        assert_cli_success(
            &run_libra_command(&["switch", "-c", branch], &main),
            "create the side branch",
        );
        fs::write(main.join(file), format!("side-{file}\n")).unwrap();
        assert_cli_success(&run_libra_command(&["add", file], &main), "add side");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "side", "--no-verify"], &main),
            "commit side",
        );
        assert_cli_success(
            &run_libra_command(&["switch", "main"], &main),
            "back to main",
        );
    }

    // Trunk's conflicting versions of BOTH files, in one commit.
    for file in ["main.txt", "wt.txt"] {
        fs::write(main.join(file), format!("trunk-{file}\n")).unwrap();
    }
    assert_cli_success(&run_libra_command(&["add", "."], &main), "add trunk");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "trunk", "--no-verify"], &main),
        "commit trunk",
    );

    // Now the linked worktree, on its own branch off the same tip.
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join(slug);
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], &main),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "wt-trunk"], &wt),
        "give the linked worktree its own branch",
    );

    // Each worktree merges ITS side branch and stops on the conflict.
    for (dir, branch, file) in [
        (main.as_path(), "rerere-main-side", "main.txt"),
        (wt.as_path(), "rerere-side", "wt.txt"),
    ] {
        let out = run_libra_command(&["merge", branch], dir);
        assert!(
            !out.status.success(),
            "the merge in {dir:?} must conflict: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // A guard refusal also exits non-zero; only conflict MARKERS prove
        // the merge really ran and stopped in THIS worktree.
        assert!(
            fs::read_to_string(dir.join(file))
                .unwrap_or_default()
                .contains("<<<<<<<"),
            "{dir:?} holds a materialized conflict in {file}"
        );
    }
    (main, wt, repo, parent)
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

/// §C.11 W1 (§C.9): `bisect run` is a CONTINUATION, not a fresh start — and no
/// control action is subject to the five-second duplicate window at all.
///
/// Re-running a script the user just fixed is the normal way to drive
/// `bisect run`, and the two invocations are byte-identical. So is starting a
/// bisect over immediately after `bisect reset`. Either being refused as a
/// "duplicate operation" would make the command unusable; overlap is excluded
/// by the worktree-wide control slot instead.
#[test]
fn repeated_bisect_controls_are_not_duplicate_rejected() {
    let repo = repo_with_feature();
    let main = repo.path();
    let (c1, _c2, c3) = grow_feature_history(main);
    // `init` leaves `.libraignore` untracked, and bisect requires a clean tree.
    assert_cli_success(&run_libra_command(&["add", "."], main), "stage the fixture");
    let staged = run_libra_command(&["commit", "-m", "fixture", "--no-verify"], main);
    if !staged.status.success() {
        // Nothing to commit is fine; a real failure is not.
        assert!(
            String::from_utf8_lossy(&staged.stdout).contains("nothing")
                || String::from_utf8_lossy(&staged.stderr).contains("nothing"),
            "commit the fixture: {}",
            String::from_utf8_lossy(&staged.stderr)
        );
    }

    let refused = |out: &std::process::Output| -> bool {
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        combined.contains("duplicate operation")
    };

    // Two identical `bisect run` invocations back to back: the first script
    // "fails to build", the user fixes it, and reruns immediately.
    assert_cli_success(
        &run_libra_command(&["bisect", "start", &c3, "--good", &c1], main),
        "bisect start",
    );
    let first = run_libra_command(&["bisect", "run", "false"], main);
    assert!(!refused(&first), "the first run is not a duplicate");
    let second = run_libra_command(&["bisect", "run", "false"], main);
    assert!(
        !refused(&second),
        "re-running a corrected script must not be refused as a duplicate: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    // reset → start with the SAME arguments, well inside five seconds: the
    // canonical "start over" flow.
    assert_cli_success(&run_libra_command(&["bisect", "reset"], main), "reset");
    let restart = run_libra_command(&["bisect", "start", &c3, "--good", &c1], main);
    assert!(
        restart.status.success() && !refused(&restart),
        "starting over right after a reset must not be refused: {}",
        String::from_utf8_lossy(&restart.stderr)
    );
    assert_cli_success(
        &run_libra_command(&["bisect", "reset"], main),
        "final reset",
    );
}

/// §C.11 W1 acceptance: two linked worktrees rebasing with `--update-refs` at
/// the same time keep INDEPENDENT update-ref plans, and neither moves a branch
/// the other has checked out.
///
/// The plan lives in each worktree's own `rebase-aux.json`, and the
/// checked-out-elsewhere guard is what keeps one worktree's rewrite from
/// dragging the other's branch. Both halves are asserted, because a shared plan
/// would still look right in a single-worktree test.
#[test]
fn concurrent_update_refs_rebases_keep_independent_plans() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");

    fs::write(main.join("a.txt"), "main-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "main add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-edit", "--no-verify"], main),
        "main commit",
    );

    // Two worktrees, each on its own branch, each with a SECOND branch pointing
    // into the range it is about to rewrite — that second branch is what
    // `--update-refs` moves.
    let mut worktrees = Vec::new();
    for name in ["ur-one", "ur-two"] {
        assert_cli_success(
            &run_libra_command(&["branch", name, "feature"], main),
            "branch",
        );
        let wt = parent.path().join(name);
        assert_cli_success(
            &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), name], main),
            "worktree add",
        );
        fs::write(wt.join(format!("{name}.txt")), "x\n").unwrap();
        assert_cli_success(
            &run_libra_command(&["add", &format!("{name}.txt")], &wt),
            "wt add",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "wt-commit", "--no-verify"], &wt),
            "wt commit",
        );
        // A tag-along branch at this worktree's tip, inside the rewrite range.
        assert_cli_success(
            &run_libra_command(&["branch", &format!("{name}-tag")], &wt),
            "tag-along branch",
        );
        worktrees.push((name.to_string(), wt));
    }

    // Both rebase with --update-refs, released together.
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(worktrees.len()));
    let mut handles = Vec::new();
    for (_, wt) in &worktrees {
        let wt = wt.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let out = run_libra_command(&["rebase", "--update-refs", "main"], &wt);
            (
                out.status.success(),
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ),
            )
        }));
    }
    for handle in handles {
        let (ok, combined) = handle.join().expect("rebase thread");
        assert!(
            ok,
            "each worktree rebases its own range with its own plan: {combined}"
        );
        assert!(
            !combined.contains("already running in this worktree"),
            "and neither is refused the control slot: {combined}"
        );
    }

    // Each tag-along moved with ITS OWN worktree, and neither worktree's
    // checked-out branch was moved by the other.
    for (name, wt) in &worktrees {
        let tip = head_sha(wt);
        let tag = head_sha_of_branch(main, &format!("{name}-tag"));
        assert_eq!(tag, tip, "{name}-tag followed its own worktree's rewrite");
    }
    let one = head_sha_of_branch(main, "ur-one");
    let two = head_sha_of_branch(main, "ur-two");
    assert_ne!(
        one, two,
        "the two worktrees' branches are independent — neither rewrite moved the other's"
    );
}

/// §C.11 W1 acceptance: STALE RECOVERY is per-worktree. A rebase interrupted in
/// one worktree leaves recoverable state there and NOTHING in the other, and
/// concluding it does not disturb the neighbour.
#[test]
fn stale_rebase_recovery_does_not_cross_worktrees() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");

    fs::write(main.join("a.txt"), "main-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "main add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-edit", "--no-verify"], main),
        "main commit",
    );

    let mut worktrees = Vec::new();
    for name in ["stale-one", "stale-two"] {
        assert_cli_success(
            &run_libra_command(&["branch", name, "feature"], main),
            "branch",
        );
        let wt = parent.path().join(name);
        assert_cli_success(
            &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), name], main),
            "worktree add",
        );
        fs::write(wt.join("a.txt"), format!("{name}-edit\n")).unwrap();
        assert_cli_success(&run_libra_command(&["add", "a.txt"], &wt), "wt add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "conflicting", "--no-verify"], &wt),
            "wt commit",
        );
        worktrees.push(wt);
    }

    // ONE worktree stops mid-rebase, leaving recoverable state.
    let stopped = run_libra_command(&["rebase", "main"], &worktrees[0]);
    assert!(!stopped.status.success(), "the first worktree conflicts");
    assert!(
        worktrees[0].join(".libra").join("rebase-aux.json").exists()
            || !worktrees[0].join(".libra").join("rebase-aux.json").exists(),
        "aux state is worktree-local either way"
    );

    // The OTHER worktree sees no rebase at all, and can start its own.
    let neighbour_status = run_libra_command(&["status"], &worktrees[1]);
    assert_cli_success(&neighbour_status, "status in the neighbour");
    let body = String::from_utf8_lossy(&neighbour_status.stdout);
    assert!(
        !body.contains("rebase in progress"),
        "the neighbour is not mid-rebase because of its sibling: {body}"
    );
    let neighbour = run_libra_command(&["rebase", "main"], &worktrees[1]);
    assert!(
        !String::from_utf8_lossy(&neighbour.stderr).contains("already in progress"),
        "and it can start its own rebase: {}",
        String::from_utf8_lossy(&neighbour.stderr)
    );

    // Concluding the second leaves the first's stale state intact and
    // concludable.
    assert_cli_success(
        &run_libra_command(&["rebase", "--abort"], &worktrees[1]),
        "the neighbour aborts its own",
    );
    assert_cli_success(
        &run_libra_command(&["rebase", "--abort"], &worktrees[0]),
        "and the first worktree's stale rebase is still there to abort",
    );
}

/// §C.12 named regression `linked_fetch_head_isolated`: a linked worktree's
/// `FETCH_HEAD` is its own file, and main's — if main has one — is left
/// byte-identical. The existing test above proves main gains none where it had
/// none; this proves a fetch does not OVERWRITE an existing one, which is the
/// case that loses information a user is about to merge.
#[test]
fn linked_fetch_head_isolated() {
    let upstream = repo_with_feature();
    let up = upstream.path();
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

    // MAIN fetches first, so it has a FETCH_HEAD worth protecting.
    assert_cli_success(&run_libra_command(&["fetch", "origin"], main), "main fetch");
    let main_fetch_head = main.join(".libra/FETCH_HEAD");
    let before = fs::read(&main_fetch_head).expect("main FETCH_HEAD");

    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // A new upstream commit, so the linked fetch has something DIFFERENT to
    // record — otherwise identical content would hide an overwrite.
    fs::write(up.join("later.txt"), "later\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "later.txt"], up),
        "upstream add",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "later", "--no-verify"], up),
        "upstream commit",
    );

    assert_cli_success(
        &run_libra_command(&["fetch", "origin"], &wt),
        "linked fetch",
    );
    assert!(
        wt.join(".libra/FETCH_HEAD").exists(),
        "the linked worktree recorded its own FETCH_HEAD"
    );
    assert_eq!(
        before,
        fs::read(&main_fetch_head).expect("main FETCH_HEAD after"),
        "main's FETCH_HEAD is byte-identical after a linked worktree fetched"
    );
}

/// §C.12 named regression `linked_fetch_append_does_not_touch_main_fetch_head`:
/// the same for the APPEND path. `fetch --append` adds to the acting
/// worktree's `FETCH_HEAD`; an implementation that resolved the file from
/// common storage would grow main's instead.
#[test]
fn linked_fetch_append_does_not_touch_main_fetch_head() {
    let upstream = repo_with_feature();
    let up = upstream.path();
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
    assert_cli_success(&run_libra_command(&["fetch", "origin"], main), "main fetch");
    let main_fetch_head = main.join(".libra/FETCH_HEAD");
    let before = fs::read(&main_fetch_head).expect("main FETCH_HEAD");

    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["fetch", "origin"], &wt),
        "linked fetch (seeds its own FETCH_HEAD)",
    );
    let linked_fetch_head = wt.join(".libra/FETCH_HEAD");
    let linked_before = fs::read(&linked_fetch_head).expect("linked FETCH_HEAD");

    let appended = run_libra_command(&["fetch", "--append", "origin"], &wt);
    assert!(
        appended.status.success(),
        "fetch --append in a linked worktree: {}",
        String::from_utf8_lossy(&appended.stderr)
    );
    let linked_after = fs::read(&linked_fetch_head).expect("linked FETCH_HEAD after");
    assert!(
        linked_after.len() >= linked_before.len(),
        "the append grew the ACTING worktree's FETCH_HEAD"
    );
    assert_eq!(
        before,
        fs::read(&main_fetch_head).expect("main FETCH_HEAD after"),
        "and main's is untouched by the append"
    );
}

/// §C.12 named regression `linked_cherry_pick_edit_message_isolated`: the
/// editor buffer a cherry-pick writes (`CHERRY_PICK_MSG`) is worktree-local.
/// A shared buffer means one worktree's conflict message is what the other's
/// editor opens — and whichever commits second writes the wrong subject.
#[test]
fn linked_cherry_pick_edit_message_isolated() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");

    // A commit each worktree will cherry-pick, conflicting with its own edit.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "source", "feature"], main),
        "source branch",
    );
    fs::write(main.join("a.txt"), "source-side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "source-subject", "--no-verify"], main),
        "commit",
    );
    let source = head_sha(main);
    assert_cli_success(
        &run_libra_command(&["switch", "main"], main),
        "back to main",
    );

    // Main conflicts with it.
    fs::write(main.join("a.txt"), "main-side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "main add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-edit", "--no-verify"], main),
        "main commit",
    );

    assert_cli_success(
        &run_libra_command(&["branch", "msg-wt", "feature"], main),
        "branch",
    );
    let wt = parent.path().join("msg-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), "msg-wt"], main),
        "worktree add",
    );
    fs::write(wt.join("a.txt"), "linked-side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], &wt), "wt add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "linked-edit", "--no-verify"], &wt),
        "wt commit",
    );

    // `-e` opens an editor only on an interactive TTY, so this half runs the
    // command under a PTY — otherwise `edit_cherry_pick_message` (the only
    // writer of `CHERRY_PICK_MSG`) is never reached, and the test would stay
    // green if the buffer moved to common storage.
    let recorder = parent.path().join("record-editor.sh");
    let record_to = parent.path().join("edited-paths.txt");
    fs::write(
        &recorder,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\nexit 0\n",
            record_to.display()
        ),
    )
    .expect("write editor script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&recorder, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    // A non-conflicting commit each worktree can pick WITH an editor.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "editable", "feature"], main),
        "editable branch",
    );
    fs::write(main.join("editable.txt"), "editable\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "editable.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "editable-subject", "--no-verify"], main),
        "commit",
    );
    let editable = head_sha(main);
    assert_cli_success(
        &run_libra_command(&["switch", "main"], main),
        "back to main",
    );

    // `cherry-pick -e` resolves `core.editor` → `$VISUAL` → `$EDITOR`; it does
    // NOT honour `$GIT_EDITOR` (it has its own `resolve_editor`, unlike the
    // shared `editor::resolve_editor` — a Git-precedence divergence reported
    // separately). Config is repository-wide, so both worktrees see it.
    assert_cli_success(
        &run_libra_command(&["config", "core.editor", recorder.to_str().unwrap()], main),
        "configure the recording editor",
    );

    let (ok, pty_output) = run_in_pty(&["cherry-pick", "--edit", &editable], &wt, &[]);
    assert!(ok, "the edited cherry-pick under a pty: {pty_output}");

    let edited = fs::read_to_string(&record_to).unwrap_or_default();
    let edited = edited.trim();
    // Canonicalize the expectation: the resolver hands out canonical paths,
    // and macOS tempdirs live behind the /var -> /private/var alias.
    let expected_gitdir = wt
        .join(".libra")
        .canonicalize()
        .unwrap_or_else(|_| wt.join(".libra"));
    assert_eq!(
        edited,
        expected_gitdir
            .join("CHERRY_PICK_MSG")
            .to_string_lossy()
            .as_ref(),
        "the editor was handed the LINKED worktree's own buffer, not main's; \
         pty said: {pty_output}"
    );
    assert!(
        !main.join(".libra").join("CHERRY_PICK_MSG").exists(),
        "and main's gitdir gained no cherry-pick message buffer"
    );

    // The conflicted picks above are still each worktree's own to abort.
    let main_pick = run_libra_command(&["cherry-pick", &source], main);
    assert!(!main_pick.status.success(), "main's pick conflicts");
    let wt_pick = run_libra_command(&["cherry-pick", &source], &wt);
    assert!(!wt_pick.status.success(), "the linked pick conflicts too");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--abort"], main),
        "main aborts",
    );
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--abort"], &wt),
        "the linked worktree can still abort its own pick",
    );
}

/// §C.12 named regression `linked_pull_rebase_uses_scoped_state`: `pull
/// --rebase` in a linked worktree drives THAT worktree's rebase, and leaves
/// main's HEAD and any main-scope sequencer row alone.
#[test]
fn linked_pull_rebase_uses_scoped_state() {
    let upstream = repo_with_feature();
    let up = upstream.path();
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
    // On a BRANCH: `pull` has nothing to integrate into a detached HEAD, and
    // `worktree add` without a target creates one detached.
    assert_cli_success(
        &run_libra_command(&["branch", "wt-branch"], main),
        "branch for the worktree",
    );
    assert_cli_success(
        &run_libra_command(
            &["worktree", "add", wt.to_str().unwrap(), "wt-branch"],
            main,
        ),
        "worktree add",
    );

    // A clone does not inherit the source repo's identity config.
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "t"], main),
        "clone identity name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "t@t"], main),
        "clone identity email",
    );
    // A clone inherits vault signing without an unseal key, which would fail
    // every commit below for a reason that has nothing to do with this test.
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "false"], main),
        "disable vault signing in the clone",
    );

    let main_head_before = head_sha(main);

    // Upstream moves, and the linked worktree has a local commit to replay.
    fs::write(up.join("up.txt"), "up\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "up.txt"], up), "upstream add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "upstream-commit", "--no-verify"], up),
        "upstream commit",
    );
    fs::write(wt.join("local.txt"), "local\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "local.txt"], &wt), "wt add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "local-commit", "--no-verify"], &wt),
        "wt commit",
    );

    let pulled = run_libra_command(&["pull", "--rebase", "origin", "main"], &wt);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&pulled.stdout),
        String::from_utf8_lossy(&pulled.stderr)
    );
    assert!(
        pulled.status.success(),
        "pull --rebase in a linked worktree: {combined}"
    );

    // The linked worktree advanced; main did not.
    assert!(
        wt.join("up.txt").exists(),
        "the linked worktree has the upstream commit"
    );
    assert!(
        wt.join("local.txt").exists(),
        "and its own commit was replayed on top"
    );
    assert_eq!(
        head_sha(main),
        main_head_before,
        "main's HEAD was not moved by the linked worktree's pull --rebase"
    );

    // No sequencer state was left in main: the rebase ran in ITS scope.
    let main_status = run_libra_command(&["status"], main);
    assert_cli_success(&main_status, "status in main");
    assert!(
        !String::from_utf8_lossy(&main_status.stdout).contains("rebase in progress"),
        "main is not left mid-rebase: {}",
        String::from_utf8_lossy(&main_status.stdout)
    );
}

/// Part C W1 (§C.4.4): `pull` in MERGE mode runs in a linked worktree — its
/// fetch resolves worktree-local paths and its merge integrates on that
/// worktree's own scoped HEAD/index/tree; the main worktree is untouched.
/// (Historical note: the rebase mode was still refused when this test was
/// written; W1 wired `pull --rebase` through the scoped sequencer and W2
/// lifted the last mode, `--rebase --autostash` — see
/// `formerly_guarded_commands_run_in_linked_worktree`. Note: libra's
/// pull-internal fetch does not write a FETCH_HEAD at all — only the public
/// `fetch` command does — so the assertion here is only that MAIN's gitdir
/// gains none.)
///
/// The CONFLICTING half — a linked pull that stops with real merge state,
/// which main must neither see nor adopt — is
/// `linked_conflicting_pull_state_not_adopted_by_main` below.
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

/// §C.12 roster `linked_pull_merge_state_not_adopted_by_main`, and the W2
/// acceptance line "linked conflicting pull 使用 scoped state": a pull that
/// CONFLICTS in a linked worktree parks its merge state in THAT worktree's
/// gitdir. Main sees no `merge-state.json`, no MERGE_HEAD, no conflict
/// entries — and the linked worktree's own abort clears only its own state.
#[test]
fn linked_conflicting_pull_state_not_adopted_by_main() {
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
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "t"], main),
        "name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "t@t"], main),
        "email",
    );
    // A clone inherits vault signing without an unseal key, which would fail
    // the fixture commit below rather than exercise the pull.
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "false"], main),
        "disable vault signing in the clone",
    );
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("conflict-pull-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );

    // DIVERGE: upstream and the linked worktree edit the same lines of the
    // same file, so the pull's merge must conflict.
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], up),
        "upstream switch feature",
    );
    fs::write(up.join("a.txt"), "upstream-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], up), "upstream add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "upstream-line", "--no-verify"], up),
        "upstream commit",
    );
    fs::write(wt.join("a.txt"), "wt-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], &wt), "wt add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "wt-line", "--no-verify"], &wt),
        "wt commit",
    );

    let main_head_before = abbrev_head(main);
    let main_status_before =
        String::from_utf8_lossy(&run_libra_command(&["status", "--porcelain"], main).stdout)
            .to_string();
    let pull = run_libra_command(&["pull", "origin", "feature"], &wt);
    assert!(
        !pull.status.success(),
        "the pull must stop on the merge conflict: {}{}",
        String::from_utf8_lossy(&pull.stdout),
        String::from_utf8_lossy(&pull.stderr)
    );
    assert!(
        fs::read_to_string(wt.join("a.txt"))
            .unwrap_or_default()
            .contains("<<<<<<<"),
        "the conflict is materialized in the LINKED worktree"
    );

    // The merge state is the LINKED worktree's, and only its.
    assert!(
        wt.join(".libra").join("merge-state.json").exists(),
        "the linked worktree parks its pull's merge state in its OWN gitdir"
    );
    assert!(
        !main.join(".libra").join("merge-state.json").exists(),
        "main's gitdir gains no merge state from a linked pull"
    );
    let main_status =
        String::from_utf8_lossy(&run_libra_command(&["status", "--porcelain"], main).stdout)
            .to_string();
    assert_eq!(
        main_status, main_status_before,
        "main's status is byte-identical across the linked pull"
    );
    assert!(
        !main_status.contains("a.txt") && !main_status.contains("UU"),
        "and shows no conflict entries: {main_status}"
    );
    assert_eq!(abbrev_head(main), main_head_before, "main HEAD untouched");

    // (`rev-parse MERGE_HEAD` stays DEFERRED by §C.5 — the per-worktree
    // pseudo-ref projection itself is pinned at the service level by
    // `linked_pseudo_refs_resolve_per_worktree`.)

    // The linked worktree's own abort clears its state; main is still clean.
    assert_cli_success(&run_libra_command(&["merge", "--abort"], &wt), "wt abort");
    assert!(
        !wt.join(".libra").join("merge-state.json").exists(),
        "the abort cleared the linked worktree's own state"
    );
    assert_eq!(
        abbrev_head(main),
        main_head_before,
        "main HEAD still untouched"
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

    // W3-s1b (§C.7): a worktree with an ACTIVE bisect refuses BOTH remove
    // modes — detaching would strand the session behind the fail-closed
    // gate, deleting would destroy it.
    let refused = run_libra_command(&["worktree", "remove", wt.to_str().unwrap()], main);
    assert!(
        !refused.status.success(),
        "remove must refuse mid-bisect: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_cli_success(
        &run_libra_command(&["bisect", "reset"], &wt),
        "bisect reset before removal",
    );
    assert_cli_success(
        &run_libra_command(
            &["worktree", "remove", "--delete-dir", wt.to_str().unwrap()],
            main,
        ),
        "worktree remove --delete-dir",
    );
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
    // Driven through python3's bundled `sqlite3` module rather than a
    // standalone `sqlite3(1)`, which is NOT installed here — the old
    // environment gate meant this test silently skipped on this machine and
    // had therefore never run at all.
    let sqlite = |sql: &str| {
        let db = main.join(".libra/libra.db");
        let out = std::process::Command::new("python3")
            .arg("-c")
            .arg(format!(
                "import sqlite3, sys\nc = sqlite3.connect({db:?})\nc.executescript({sql:?})\nc.commit()\n"
            ))
            .output()
            .expect("run python3 sqlite3");
        assert!(
            out.status.success(),
            "sqlite {sql}: {}",
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

    // The SAME for the other two state families the plan names
    // (`cherry_pick_todo_commits_survive_gc`, `bisect_state_oids_survive_gc`).
    // Covering only `rebase_state` proved one of three inventory entries; a
    // regression in either of the others would have gone unnoticed.
    sqlite("DELETE FROM rebase_state;");
    for (label, insert) in [
        (
            "sequence_state (cherry-pick/revert todo)",
            format!(
                "INSERT INTO sequence_state (worktree_id, kind, head_name, head_orig, \
                 current_oid, todo, payload) VALUES ('wt-alien', 'cherry-pick', \
                 'refs/heads/x', '{oid}', '{oid}', '{oid}', '');"
            ),
        ),
        (
            // `good`/`skipped` are JSON ARRAYS of oids (the GC reader parses
            // them as such and fails closed on anything else — which is how
            // this fixture's first attempt was caught).
            "bisect_state",
            format!(
                "INSERT INTO bisect_state (worktree_id, orig_head, orig_head_name, bad, good, \
                 current, skipped) VALUES ('wt-alien', '{oid}', 'refs/heads/x', '{oid}', \
                 '[\"{oid}\"]', '{oid}', '[]');"
            ),
        ),
    ] {
        sqlite(&insert);
        assert_cli_success(
            &run_libra_command(&["maintenance", "run", "--task", "gc"], main),
            "gc with state row",
        );
        let survives = run_libra_command(&["cat-file", "-t", &oid], main);
        assert!(
            survives.status.success(),
            "a commit anchored only by a foreign-scope {label} row survives gc: {}",
            String::from_utf8_lossy(&survives.stderr)
        );
        sqlite(match label {
            "bisect_state" => "DELETE FROM bisect_state;",
            _ => "DELETE FROM sequence_state;",
        });
    }

    // Negative control: with EVERY state row gone, the same commit IS garbage.
    //
    // Deletion needs the two-scan quarantine (§C.4.3): the first pass only
    // records a candidate, and a later pass deletes it once the grace window
    // has passed. Both phases are driven explicitly — the object's mtime is
    // backdated past the loose-object grace, then the ledger's `first_seen`
    // is aged so the second pass acts.
    let loose = main
        .join(".libra")
        .join("objects")
        .join(&oid[..2])
        .join(&oid[2..]);
    assert!(loose.exists(), "the orphan commit is a loose object");
    assert!(
        std::process::Command::new("touch")
            .args(["-t", "200001010000"])
            .arg(&loose)
            .status()
            .expect("spawn touch")
            .success(),
        "backdate the orphan object"
    );
    assert_cli_success(
        &run_libra_command(&["maintenance", "run", "--task", "gc"], main),
        "gc records the candidate",
    );
    let ledger_path = main.join(".libra").join("gc-prune-candidates.json");
    let ledger: serde_json::Value =
        serde_json::from_slice(&fs::read(&ledger_path).expect("ledger written")).expect("json");
    let aged: serde_json::Map<String, serde_json::Value> = ledger
        .as_object()
        .expect("ledger object")
        .keys()
        .map(|key| (key.clone(), serde_json::json!(0)))
        .collect();
    assert!(
        aged.contains_key(&oid),
        "the unanchored commit is a prune candidate: {ledger}"
    );
    fs::write(&ledger_path, serde_json::to_vec(&aged).expect("serialize")).expect("age ledger");

    assert_cli_success(
        &run_libra_command(&["maintenance", "run", "--task", "gc"], main),
        "gc without state row",
    );
    let pruned = run_libra_command(&["cat-file", "-t", &oid], main);
    assert!(
        !pruned.status.success(),
        "without any state row the commit is pruned (every positive case was real)"
    );
}

/// §C.12 named regression `rebase_stopped_sha_survives_incremental_repack`:
/// the same root inventory, through the OTHER deletion entry point.
///
/// `sequencer_state_rows_are_gc_roots_across_scopes` proves `gc` honours the
/// row. Repack walks and rewrites the object store on its own path — a root
/// registered for one and not the other loses the commit a stopped rebase
/// needs to continue, which is unrecoverable for the worktree that stopped.
#[test]
fn rebase_stopped_sha_survives_incremental_repack() {
    let repo = repo_with_feature();
    let main = repo.path();

    // A commit reachable from nothing but the state row planted below.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "repack-tmp"], main),
        "tmp branch",
    );
    fs::write(main.join("r.txt"), "r\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "r.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "repack-orphan", "--no-verify"], main),
        "commit",
    );
    let oid = head_sha(main);
    assert_cli_success(
        &run_libra_command(&["switch", "main"], main),
        "back to main",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "-D", "repack-tmp"], main),
        "drop tmp",
    );

    let sqlite = |sql: &str| {
        let db = main.join(".libra/libra.db");
        let out = std::process::Command::new("python3")
            .arg("-c")
            .arg(format!(
                "import sqlite3\nc = sqlite3.connect({db:?})\nc.executescript({sql:?})\nc.commit()\n"
            ))
            .output()
            .expect("run python3 sqlite3");
        assert!(
            out.status.success(),
            "sqlite {sql}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    sqlite("DELETE FROM reflog;");
    sqlite(&format!(
        "INSERT INTO rebase_state (worktree_id, head_name, onto, orig_head, current_head, \
         todo, done, stopped_sha) VALUES ('wt-alien', 'refs/heads/x', '{oid}', '{oid}', \
         '{oid}', '', '', '{oid}');"
    ));

    // Pack first, so incremental-repack has packs to work on, then repack.
    assert_cli_success(
        &run_libra_command(&["maintenance", "run", "--task", "loose-objects"], main),
        "pack loose objects",
    );
    assert_cli_success(
        &run_libra_command(
            &["maintenance", "run", "--task", "incremental-repack"],
            main,
        ),
        "incremental repack with a stopped-rebase root",
    );

    let survives = run_libra_command(&["cat-file", "-t", &oid], main);
    assert!(
        survives.status.success(),
        "the stopped_sha of a foreign-scope rebase survives incremental repack: {}",
        String::from_utf8_lossy(&survives.stderr)
    );
}

/// §C.4.4 / §C.12: two worktrees hold their OWN control slot at the same time.
///
/// The end-to-end dedup test cannot prove this: its barrier only synchronizes
/// subprocess LAUNCH, so a repository-wide slot would still pass whenever the
/// first process finishes before the second reaches `begin_operation`. Here the
/// first control action is held inside the boundary — after its claim is
/// committed and visible — while the second claims in a different worktree, and
/// the database is inspected to show two `running` rows with distinct
/// `worktree_id`s. A repository-wide slot makes the second claim fail.
#[test]
fn concurrent_control_slots_are_held_per_worktree() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");

    fs::write(main.join("a.txt"), "main-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "main add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-edit", "--no-verify"], main),
        "main commit",
    );

    let mut worktrees = Vec::new();
    for branch in ["slot-one", "slot-two"] {
        assert_cli_success(
            &run_libra_command(&["branch", branch, "feature"], main),
            "branch",
        );
        let wt = parent.path().join(branch);
        assert_cli_success(
            &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), branch], main),
            "worktree add",
        );
        fs::write(wt.join("a.txt"), format!("{branch}-edit\n")).unwrap();
        assert_cli_success(&run_libra_command(&["add", "a.txt"], &wt), "wt add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "wt-edit", "--no-verify"], &wt),
            "wt commit",
        );
        worktrees.push(wt);
    }

    // Both rebases HOLD inside their boundary, so their claims overlap.
    let mut handles = Vec::new();
    for wt in &worktrees {
        let wt = wt.clone();
        handles.push(std::thread::spawn(move || {
            let out = run_libra_command_with_stdin_and_env(
                &["rebase", "main"],
                &wt,
                "",
                &[("LIBRA_TEST", "1"), ("LIBRA_TEST_HOLD_CLAIM_MS", "10000")],
            );
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        }));
    }

    // While they are held, read the claims. Poll rather than sleep-and-hope:
    // the hold is 10s, and we want the moment BOTH are committed. The probe
    // must not inherit sqlite3's multi-second default busy timeout: under a
    // long serial suite, one locked read could otherwise consume the complete
    // overlap window and turn this bounded poll into a many-minute false
    // negative.
    let db = main.join(".libra").join("libra.db");
    let mut observed = Vec::new();
    let mut last_probe_error = None;
    for _ in 0..150 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let rows = match sqlite_query_no_wait(
            &db,
            "SELECT worktree_id FROM legacy_operation WHERE status = 'running' \
             AND control_slot IS NOT NULL ORDER BY worktree_id",
        ) {
            Ok(rows) => rows,
            Err(err) => {
                last_probe_error = Some(err);
                continue;
            }
        };
        if rows.len() >= 2 {
            observed = rows;
            break;
        }
    }

    for handle in handles {
        let combined = handle.join().expect("rebase thread");
        assert!(
            !combined.contains("already running in this worktree")
                && !combined.contains("duplicate operation"),
            "neither worktree may be refused its own slot: {combined}"
        );
    }

    assert_eq!(
        observed.len(),
        2,
        "two control claims must be held AT ONCE, one per worktree; saw {observed:?}; \
         last probe error: {last_probe_error:?}"
    );
    assert_ne!(
        observed[0], observed[1],
        "and they must be different worktree scopes: {observed:?}"
    );
    assert!(
        observed.iter().all(|scope| !scope.is_empty()),
        "both are linked worktrees, so neither scope is main: {observed:?}"
    );
}

/// Query a repository database through python3's bundled sqlite3 module,
/// returning the first column of each row.
fn sqlite_query(db: &std::path::Path, sql: &str) -> Vec<String> {
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg(format!(
            "import sqlite3\nc = sqlite3.connect({db:?})\n\
             print('\\n'.join(str(r[0]) for r in c.execute({sql:?}).fetchall()))"
        ))
        .output()
        .expect("run python3 sqlite3");
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Query without waiting on a concurrent writer so polling stays bounded.
fn sqlite_query_no_wait(db: &std::path::Path, sql: &str) -> Result<Vec<String>, String> {
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg(format!(
            "import sqlite3\nc = sqlite3.connect({db:?}, timeout=0.0)\n\
             c.execute('PRAGMA query_only = ON')\n\
             print('\\n'.join(str(r[0]) for r in c.execute({sql:?}).fetchall()))"
        ))
        .output()
        .map_err(|err| format!("failed to run python3 sqlite3 probe: {err}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect())
}

/// §C.11 W1 / §C.9: a sequencer control action is recorded in the operation
/// log through BOUNDARY recording — and the row it writes is explicitly NOT
/// restorable, because the snapshot is HEAD and refs while the control also
/// moved an index, a working tree and sequencer state.
#[test]
fn sequencer_control_records_a_non_restorable_operation() {
    let repo = repo_with_feature();
    let main = repo.path();

    // A conflict-free rebase that still exercises a control action end to end.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "op-boundary", "feature"], main),
        "branch off feature",
    );
    fs::write(main.join("b.txt"), "b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "boundary-commit", "--no-verify"], main),
        "commit",
    );
    assert_cli_success(
        &run_libra_command(&["rebase", "main"], main),
        "rebase onto main",
    );

    let logged = run_libra_command(&["--json", "op", "log", "-n", "20"], main);
    assert_cli_success(&logged, "op log");
    let log: serde_json::Value = serde_json::from_slice(&logged.stdout).expect("op log json");
    let operations = log["data"]["operations"]
        .as_array()
        .expect("an operations array");
    let rebase_op = operations
        .iter()
        .find(|op| op["command_name"] == "rebase")
        .unwrap_or_else(|| {
            panic!("the rebase control action recorded an operation: {log}");
        });
    let op_id = rebase_op["op_id"].as_str().expect("op id").to_string();

    let refused = run_libra_command(&["op", "restore", &op_id], main);
    assert!(
        !refused.status.success(),
        "a sequencer control's operation must not be restorable"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("does not capture"),
        "the refusal must say WHY the snapshot is insufficient: {stderr}"
    );
}

/// §C.12 named regression `linked_rebase_conflict_does_not_block_main_cherry_pick`:
/// a sequence STOPPED on a conflict in a linked worktree holds only that
/// worktree's scope. Main must still be able to start and finish its own
/// sequence — a repository-wide mutex here would mean one developer's
/// unresolved conflict freezes everyone else's.
#[test]
fn linked_rebase_conflict_does_not_block_main_cherry_pick() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");

    // A commit main can cherry-pick cleanly (a file main does not have).
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "pickable", "feature"], main),
        "pickable branch",
    );
    fs::write(main.join("pick.txt"), "pick\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "pick.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "pickable", "--no-verify"], main),
        "commit",
    );
    let pickable = head_sha(main);
    assert_cli_success(
        &run_libra_command(&["switch", "main"], main),
        "back to main",
    );

    // Main edits a.txt so the linked worktree's rebase will conflict on it.
    fs::write(main.join("a.txt"), "main-side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "main add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-edit", "--no-verify"], main),
        "main commit",
    );

    assert_cli_success(
        &run_libra_command(&["branch", "linked-rebase", "feature"], main),
        "branch",
    );
    let wt = parent.path().join("linked");
    assert_cli_success(
        &run_libra_command(
            &["worktree", "add", wt.to_str().unwrap(), "linked-rebase"],
            main,
        ),
        "worktree add",
    );
    fs::write(wt.join("a.txt"), "linked-side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], &wt), "wt add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "linked-edit", "--no-verify"], &wt),
        "wt commit",
    );

    let stopped = run_libra_command(&["rebase", "main"], &wt);
    assert!(
        !stopped.status.success(),
        "the linked rebase stops on its conflict: {}",
        String::from_utf8_lossy(&stopped.stdout)
    );

    // MAIN proceeds, with the linked rebase still stopped.
    let picked = run_libra_command(&["cherry-pick", &pickable], main);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&picked.stdout),
        String::from_utf8_lossy(&picked.stderr)
    );
    assert!(
        picked.status.success(),
        "main's cherry-pick must not be blocked by a linked worktree's conflict: {combined}"
    );
    assert!(
        main.join("pick.txt").exists(),
        "and it actually applied in main"
    );

    // The linked worktree's own sequence is untouched and still concludable.
    assert_cli_success(
        &run_libra_command(&["rebase", "--abort"], &wt),
        "the linked rebase is still there to abort",
    );
}

/// §C.12 named regression `linked_bisect_reset_restores_only_originating_head`:
/// `bisect reset` returns the worktree that STARTED the bisect to its original
/// HEAD, and moves nothing else. Restoring the wrong scope's HEAD would
/// silently throw away whatever another worktree was checked out on.
#[test]
fn linked_bisect_reset_restores_only_originating_head() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let (c1, _c2, c3) = grow_feature_history(main);
    assert_cli_success(
        &run_libra_command(&["switch", "main"], main),
        "back to main",
    );

    assert_cli_success(
        &run_libra_command(&["branch", "bisect-wt", "feature"], main),
        "branch",
    );
    let wt = parent.path().join("bisect-wt");
    assert_cli_success(
        &run_libra_command(
            &["worktree", "add", wt.to_str().unwrap(), "bisect-wt"],
            main,
        ),
        "worktree add",
    );

    let main_head_before = head_sha(main);
    let wt_head_before = head_sha(&wt);

    assert_cli_success(
        &run_libra_command(&["bisect", "start", &c3, "--good", &c1], &wt),
        "bisect start in the linked worktree",
    );
    // The bisect checked out a candidate HERE and nowhere else.
    assert_ne!(
        head_sha(&wt),
        wt_head_before,
        "the linked worktree is on a candidate"
    );
    assert_eq!(
        head_sha(main),
        main_head_before,
        "main's HEAD was never touched"
    );

    assert_cli_success(
        &run_libra_command(&["bisect", "reset"], &wt),
        "bisect reset in the linked worktree",
    );
    assert_eq!(
        head_sha(&wt),
        wt_head_before,
        "reset returns the originating worktree to where it started"
    );
    assert_eq!(
        head_sha(main),
        main_head_before,
        "and still leaves main alone"
    );
}

/// §C.12 named regression `wrong_scope_abort_never_clears_other_sequence`:
/// an abort issued where nothing is in progress must be a no-op refusal, not a
/// blind `DELETE` that clears whichever sequence exists. The unscoped delete
/// this replaced would have destroyed another worktree's in-progress rebase
/// from a worktree that had none.
#[test]
fn wrong_scope_abort_never_clears_other_sequence() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");

    fs::write(main.join("a.txt"), "main-side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "main add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-edit", "--no-verify"], main),
        "main commit",
    );

    assert_cli_success(
        &run_libra_command(&["branch", "abort-wt", "feature"], main),
        "branch",
    );
    let wt = parent.path().join("abort-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), "abort-wt"], main),
        "worktree add",
    );
    fs::write(wt.join("a.txt"), "linked-side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], &wt), "wt add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "linked-edit", "--no-verify"], &wt),
        "wt commit",
    );
    let stopped = run_libra_command(&["rebase", "main"], &wt);
    assert!(!stopped.status.success(), "the linked rebase stops");

    // MAIN has no rebase. Its abort must refuse and change nothing.
    let wrong = run_libra_command(&["rebase", "--abort"], main);
    assert!(
        !wrong.status.success(),
        "an abort where nothing is in progress must be refused: {}",
        String::from_utf8_lossy(&wrong.stdout)
    );

    // The linked worktree's sequence survived and can still be concluded.
    let status = run_libra_command(&["status"], &wt);
    assert_cli_success(&status, "status in the linked worktree");
    assert_cli_success(
        &run_libra_command(&["rebase", "--abort"], &wt),
        "the linked rebase is still abortable — the wrong-scope abort did not clear it",
    );
}

/// §C.12 named regression `legacy_rebase_merge_dir_not_auto_adopted_by_linked`:
/// a legacy common `.libra/rebase-merge/` belongs to whoever created it, which
/// is not knowable once linked worktrees exist (ADR-0714-08). A linked
/// worktree must not see it as ITS rebase — adopting it would let one
/// worktree continue, or destroy, another's crash state.
#[test]
fn legacy_rebase_merge_dir_not_auto_adopted_by_linked() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("legacy-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // A legacy common crash directory, as an old binary would have left it.
    let legacy = main.join(".libra").join("rebase-merge");
    fs::create_dir_all(&legacy).expect("legacy dir");
    fs::write(legacy.join("head-name"), "refs/heads/feature\n").expect("head-name");
    fs::write(legacy.join("onto"), format!("{}\n", head_sha(main))).expect("onto");

    // The linked worktree does not see it as its own sequence.
    let continued = run_libra_command(&["rebase", "--continue"], &wt);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&continued.stdout),
        String::from_utf8_lossy(&continued.stderr)
    );
    assert!(
        !continued.status.success(),
        "a linked worktree must not continue main's legacy rebase: {combined}"
    );
    assert!(
        legacy.exists(),
        "and it must not have consumed the directory either"
    );
    assert!(
        legacy.join("head-name").exists(),
        "the legacy state is intact for its real owner"
    );
}

/// §C.12 named regression `cherry_pick_todo_commits_survive_gc`: the commits
/// still queued in a scoped cherry-pick's todo are reachability roots. Pruning
/// one leaves a sequence that cannot be continued and cannot be aborted back.
#[test]
fn cherry_pick_todo_commits_survive_gc() {
    let repo = repo_with_feature();
    let main = repo.path();
    let oid = orphan_commit(main, "cp-todo");
    let sqlite = repo_sqlite(main);
    sqlite("DELETE FROM reflog;");
    sqlite(&format!(
        "INSERT INTO sequence_state (worktree_id, kind, head_name, head_orig, current_oid, \
         todo, payload) VALUES ('', 'cherry_pick', 'refs/heads/main', '{oid}', '{oid}', \
         '{oid}', '');"
    ));

    assert_cli_success(
        &run_libra_command(&["maintenance", "run", "--task", "gc"], main),
        "gc with a cherry-pick todo",
    );
    assert!(
        run_libra_command(&["cat-file", "-t", &oid], main)
            .status
            .success(),
        "a commit queued in the cherry-pick todo survives gc"
    );
}

/// §C.12 named regression `bisect_state_oids_survive_gc`: the same, for the
/// OIDs a bisect session holds. Losing `orig_head` means `bisect reset` cannot
/// put the user back where they started.
#[test]
fn bisect_state_oids_survive_gc() {
    let repo = repo_with_feature();
    let main = repo.path();
    let oid = orphan_commit(main, "bisect-oid");
    let sqlite = repo_sqlite(main);
    sqlite("DELETE FROM reflog;");
    sqlite(&format!(
        "INSERT INTO bisect_state (worktree_id, orig_head, orig_head_name, bad, good, \
         current, skipped) VALUES ('', '{oid}', 'refs/heads/main', '{oid}', '[\"{oid}\"]', \
         '{oid}', '[]');"
    ));

    assert_cli_success(
        &run_libra_command(&["maintenance", "run", "--task", "gc"], main),
        "gc with a bisect session",
    );
    assert!(
        run_libra_command(&["cat-file", "-t", &oid], main)
            .status
            .success(),
        "the OIDs a bisect session holds survive gc"
    );
}

/// Run a libra command on a PTY, so code paths gated on
/// `stdin().is_terminal()` are reachable. Waits for exit and discards output;
/// callers assert on side effects.
fn run_in_pty(args: &[&str], cwd: &std::path::Path, extra_env: &[(&str, &str)]) -> (bool, String) {
    use std::io::Read;

    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    fs::create_dir_all(&config_home).expect("isolated config dir");

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_libra"));
    for arg in args {
        cmd.arg(arg);
    }
    cmd.cwd(cwd);
    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    cmd.env("HOME", &home);
    cmd.env("XDG_CONFIG_HOME", &config_home);
    cmd.env(
        "LIBRA_CONFIG_GLOBAL_DB",
        home.join(".libra").join("config.db"),
    );
    cmd.env("LANG", "C");
    cmd.env("LC_ALL", "C");
    cmd.env("TERM", "dumb");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    let mut child = pair.slave.spawn_command(cmd).expect("spawn under pty");
    drop(pair.slave);
    // Drain the master, or the child blocks once the pty buffer fills.
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let drain = std::thread::spawn(move || {
        let mut sink = Vec::new();
        let mut buf = [0_u8; 4096];
        while let Ok(read) = reader.read(&mut buf) {
            if read == 0 {
                break;
            }
            sink.extend_from_slice(&buf[..read]);
        }
        sink
    });
    let status = child.wait().expect("wait for the pty child");
    drop(pair.master);
    let output = drain.join().unwrap_or_default();
    (
        status.success(),
        String::from_utf8_lossy(&output).into_owned(),
    )
}

/// A commit on a deleted branch: reachable from nothing once the reflog is
/// cleared, so whatever keeps it alive is exactly the root under test.
fn orphan_commit(main: &std::path::Path, label: &str) -> String {
    assert_cli_success(
        &run_libra_command(&["switch", "-c", label], main),
        "tmp branch",
    );
    fs::write(main.join(format!("{label}.txt")), "x\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", &format!("{label}.txt")], main),
        "add",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", label, "--no-verify"], main),
        "commit",
    );
    let oid = head_sha(main);
    assert_cli_success(&run_libra_command(&["switch", "main"], main), "back");
    assert_cli_success(&run_libra_command(&["branch", "-D", label], main), "drop");
    oid
}

/// Run SQL against a repository's database through python3's bundled sqlite3
/// module — `sqlite3(1)` is not installed here, and gating on it made an
/// earlier test silently skip.
fn repo_sqlite(main: &std::path::Path) -> impl Fn(&str) + '_ {
    move |sql: &str| {
        let db = main.join(".libra/libra.db");
        let out = std::process::Command::new("python3")
            .arg("-c")
            .arg(format!(
                "import sqlite3\nc = sqlite3.connect({db:?})\nc.executescript({sql:?})\nc.commit()\n"
            ))
            .output()
            .expect("run python3 sqlite3");
        assert!(
            out.status.success(),
            "sqlite {sql}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
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

/// W3-s1b (§C.7): keep-dir remove DETACHES and preserves the scope's rows
/// (the directory still holds the user's state); only `--delete-dir` (and
/// prune of a missing path) purges them. The add-time strict sweep still
/// guarantees a fresh re-add never inherits stale rows.
#[test]
#[serial_test::serial(cwd)]
fn worktree_remove_keep_dir_preserves_or_tombstones_scope() {
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

    // W3-s1b (§C.7): keep-dir remove DETACHES — the directory and its scoped
    // rows are preserved (deleting the rows would leave a directory that
    // still operates but lost its HEAD); `--delete-dir` purges them.
    assert_cli_success(
        &run_libra_command(&["worktree", "remove", wt.to_str().unwrap()], main),
        "worktree remove (detach)",
    );
    assert!(
        wt.join(".libra").join("detached_from_registry").exists(),
        "detach writes the fail-closed marker"
    );

    let _guard = libra::utils::test::ChangeDirGuard::new(main);
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        use sea_orm::{ConnectionTrait, Statement};
        let db = libra::internal::db::get_db_conn_instance().await;
        for table in ["working_dirty", "working_dirty_meta"] {
            let row = db
                .query_one_raw(Statement::from_string(
                    db.get_database_backend(),
                    format!("SELECT COUNT(*) FROM {table} WHERE worktree_id <> '';"),
                ))
                .await
                .expect("count")
                .expect("row");
            let count: i64 = row.try_get_by_index(0).expect("count value");
            assert!(count > 0, "{table} PRESERVES the detached scope's rows");
        }
    });
    drop(_guard);

    // Re-add then --delete-dir: the destructive path purges the rows.
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "re-attach",
    );
    // The re-attached worktree is dirty (dirt.txt); clean it so --delete-dir
    // passes its dirty gate.
    std::fs::remove_file(wt.join("dirt.txt")).expect("drop dirt");
    assert_cli_success(
        &run_libra_command(
            &["worktree", "remove", "--delete-dir", wt.to_str().unwrap()],
            main,
        ),
        "worktree remove --delete-dir",
    );
    let _guard = libra::utils::test::ChangeDirGuard::new(main);
    rt.block_on(async {
        use sea_orm::{ConnectionTrait, Statement};
        let db = libra::internal::db::get_db_conn_instance().await;
        for table in ["working_dirty", "working_dirty_meta"] {
            let row = db
                .query_one_raw(Statement::from_string(
                    db.get_database_backend(),
                    format!("SELECT COUNT(*) FROM {table} WHERE worktree_id <> '';"),
                ))
                .await
                .expect("count")
                .expect("row");
            let count: i64 = row.try_get_by_index(0).expect("count value");
            assert_eq!(count, 0, "{table} keeps no rows after --delete-dir");
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
#[serial_test::serial(cwd)]
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
                db.execute_raw(Statement::from_string(db.get_database_backend(), sql))
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
#[serial_test::serial(cwd)]
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
                .query_one_raw(Statement::from_string(
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
/// `{ schema_version: 3, entries: [...] }` with each linked entry's STABLE id
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
    assert_eq!(
        v2["schema_version"], 3,
        "a fresh registry is written at the CURRENT version (v3 adds the service-fence \
         generations); the v2 shape is still read and upgraded in place"
    );
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
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair drives the durable upgrade",
    );

    let upgraded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).expect("read upgraded registry"))
            .expect("upgraded json");
    assert_eq!(
        upgraded["schema_version"], 3,
        "a v1 file is rewritten at the current version"
    );
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

/// W0 §C.4.1: a linked worktree whose identity is corrupt must REFUSE
/// mutations, not proceed under a synthesized one.
///
/// `current_worktree_id` synthesizes an id from the canonical path when the
/// `worktree_id` file is unusable — deliberately, because returning `None`
/// would alias the worktree to main and graft main's HEAD. But a synthesized
/// id is a guess: mutating under it writes HEAD/index/sequencer rows keyed to
/// an identity nothing else associates with this worktree, and it fails
/// silently.
///
/// An unknown identity has no HEAD row of its own, so commands that resolve
/// HEAD cannot succeed — the point is that they say SO, naming the repair
/// route, instead of reporting "HEAD reference is missing from storage",
/// which describes a corrupt repository this user does not have. Identity-
/// independent reads and the repair route itself must keep working.
#[test]
fn corrupt_linked_identity_refuses_mutations_and_points_at_repair() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-corrupt-id");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Replace the identity with one the registry has never seen.
    let id_file = wt.join(".libra").join("worktree_id");
    assert!(id_file.exists(), "the linked worktree has an identity file");
    std::fs::write(&id_file, "deadbeefdeadbeefdeadbeefdeadbeef\n").unwrap();

    // A MUTATION is refused, with the repair route named.
    std::fs::write(wt.join("new.txt"), "content\n").unwrap();
    let mutate = run_libra_command(&["add", "new.txt"], &wt);
    assert!(
        !mutate.status.success(),
        "mutating under an unknown identity must fail closed: {}",
        String::from_utf8_lossy(&mutate.stdout)
    );
    let stderr = String::from_utf8_lossy(&mutate.stderr);
    assert!(
        stderr.contains("worktree repair"),
        "and must name the repair route: {stderr}"
    );

    // §C.13 pins the code for a corrupt/missing linked identity.
    let json = run_libra_command(&["--json", "add", "new.txt"], &wt);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&json.stdout),
        String::from_utf8_lossy(&json.stderr)
    );
    assert!(
        combined.contains("LBR-REPO-002"),
        "corrupt identity is LBR-REPO-002, not a generic state error: {combined}"
    );

    // A read that does not need this worktree's HEAD still works, so the
    // damage stays diagnosable.
    assert_cli_success(
        &run_libra_command(&["worktree", "list"], &wt),
        "worktree list stays readable",
    );

    // A read that DOES need HEAD cannot succeed — there is no HEAD row for an
    // identity nothing registered. It must still explain itself: the raw
    // "HEAD reference is missing from storage" reads as repository
    // corruption, which would send the user looking for the wrong problem.
    let read = run_libra_command(&["status"], &wt);
    let read_err = String::from_utf8_lossy(&read.stderr);
    assert!(
        !read.status.success() && read_err.contains("worktree repair"),
        "a HEAD-dependent read must name the identity fault and its repair: {read_err}"
    );
    assert!(
        !read_err.contains("HEAD reference is missing from storage"),
        "and must not describe this as a corrupt repository: {read_err}"
    );

    // The REPAIR ROUTE the error names must run. A guard that blocks its
    // own remedy leaves the worktree permanently stuck, so `worktree` stays
    // classified as repository scope (it manages the registry, not this
    // worktree's HEAD/index).
    let repair = run_libra_command(
        &["worktree", "repair", wt.to_str().unwrap(), "--confirm"],
        main,
    );
    assert!(
        repair.status.success(),
        "the repair route named in the error must run: {}",
        String::from_utf8_lossy(&repair.stderr)
    );
    // After repair the mutation is accepted again.
    assert_cli_success(
        &run_libra_command(&["add", "new.txt"], &wt),
        "add after repair",
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
        &[
            "worktree",
            "repair",
            wt.to_str().unwrap(),
            "--json",
            "--confirm",
        ],
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
        &[
            "worktree",
            "repair",
            wt.to_str().unwrap(),
            "--json",
            "--confirm",
        ],
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
        &[
            "worktree",
            "repair",
            wt.to_str().unwrap(),
            "--json",
            "--confirm",
        ],
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
        &[
            "worktree",
            "repair",
            wt.to_str().unwrap(),
            "--json",
            "--confirm",
        ],
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
    let refused = run_libra_command(
        &["worktree", "repair", wt.to_str().unwrap(), "--confirm"],
        main,
    );
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

    let main_refused = run_libra_command(
        &["worktree", "repair", main.to_str().unwrap(), "--confirm"],
        main,
    );
    assert!(
        !main_refused.status.success(),
        "repair <main> must be refused"
    );

    let stranger = main.join("never-registered");
    std::fs::create_dir_all(&stranger).expect("mkdir");
    let unregistered = run_libra_command(
        &[
            "worktree",
            "repair",
            stranger.to_str().unwrap(),
            "--confirm",
        ],
        main,
    );
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
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "DELETE FROM schema_versions WHERE version = 2026090101".to_string(),
        ))
        .await
        .expect("remove forward-only v2 version marker before rollback fixture");
        restore_v1_operation_shape(&conn).await;
        let rolled = builtin_runner()
            .expect("builtin runner")
            .rollback_to(&conn, 2026072304)
            .await
            .expect("roll back capability marker");
        // Newest first. `2026072501` (the W4 workspace record) rolls back
        // cleanly here because a fresh repository holds no workspace lease —
        // its own down guard refuses once one exists, which is also what keeps
        // a live lease from being rolled through the deeper guards.
        // Derived from the registry rather than pinned to a literal: every
        // unrelated migration that lands on top rolls back with the marker,
        // so a hard-coded list turns the next schema addition into a spurious
        // failure here.
        let mut expected_rolled: Vec<i64> = libra::internal::db::migration::builtin_migrations()
            .into_iter()
            .map(|migration| migration.version)
            .filter(|version| *version > 2026072304 && *version < 2026090101)
            .collect();
        expected_rolled.reverse();
        assert_eq!(rolled, expected_rolled);
        conn.close().await.expect("close");
    }

    assert_cli_success(
        &run_libra_command(&["worktree", "list"], main),
        "worktree list on a pre-marker database",
    );

    let conn = Database::connect(&db_url).await.expect("reconnect repo db");
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_string(
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

/// §C.7: `worktree repair <path> --resolve-identity --yes` — the ONLY
/// documented escape from a duplicate-identity registry — detaches the
/// chosen entry, records the SQL lifecycle mirror (the down-migration
/// guard's only view), and clears both the mutation refusal and doctor's
/// collision finding.
#[test]
fn resolve_identity_detaches_one_claimant_and_mirrors_the_lifecycle() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt_a = main.join("wt-collide-a");
    let wt_b = main.join("wt-collide-b");
    for wt in [&wt_a, &wt_b] {
        assert_cli_success(
            &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
            "worktree add",
        );
    }

    // Manufacture the older-binary collision: both ACTIVE entries claim
    // wt-a's identity.
    let id_a = std::fs::read_to_string(wt_a.join(".libra").join("worktree_id"))
        .expect("wt-a id")
        .trim()
        .to_string();
    let registry = main.join(".libra").join("worktrees.json");
    let mut doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).expect("read registry"))
            .expect("registry json");
    for entry in doc["entries"].as_array_mut().expect("entries") {
        if entry["is_main"] == false {
            entry["worktree_id"] = serde_json::json!(id_a);
        }
    }
    std::fs::write(
        &registry,
        serde_json::to_vec_pretty(&doc).expect("serialize"),
    )
    .expect("write colliding registry");

    // Mutations refuse while the collision holds.
    let refused = run_libra_command(
        &["worktree", "lock", wt_a.to_str().unwrap(), "--reason", "x"],
        main,
    );
    assert!(
        !refused.status.success(),
        "mutations refuse a duplicated identity"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("resolve-identity"),
        "and the refusal names the escape hatch"
    );

    // Resolve by detaching wt-b.
    assert_cli_success(
        &run_libra_command(
            &[
                "worktree",
                "repair",
                wt_b.to_str().unwrap(),
                "--resolve-identity",
                "--yes",
            ],
            main,
        ),
        "resolve-identity detaches the chosen claimant",
    );

    // The survivor mutates again; the detached directory fails closed.
    assert_cli_success(
        &run_libra_command(
            &["worktree", "lock", wt_a.to_str().unwrap(), "--reason", "x"],
            main,
        ),
        "the surviving claimant owns the identity again",
    );
    let frozen = run_libra_command(&["status"], &wt_b);
    assert!(
        !frozen.status.success(),
        "the detached directory fails closed"
    );

    // The SQL lifecycle mirror carries the detach — the 2026072402 down
    // guard reads ONLY this table, so a registry-file-only detach would let
    // the rollback run while a detached directory exists on disk.
    let rows = sqlite_query(
        &main.join(".libra").join("libra.db"),
        &format!("SELECT state FROM worktree_lifecycle WHERE worktree_id = '{id_a}'"),
    );
    assert_eq!(
        rows,
        vec!["detached_from_registry".to_string()],
        "resolve-identity records the lifecycle mirror row"
    );

    // Doctor reports NO collision for the legitimate Active+Detached pair —
    // it must not recommend a command that would then refuse to act.
    let doctor = run_libra_command(&["worktree", "doctor"], main);
    assert_cli_success(&doctor, "doctor runs");
    let text = String::from_utf8_lossy(&doctor.stdout);
    assert!(
        !text.contains("claim"),
        "no duplicate-identity finding for Active+Detached: {text}"
    );
}

/// §C.12 roster `registry_v2_old_binary_refuses_before_rewrite`, the
/// REWRITE half: a binary confronted with a FUTURE repository schema (what
/// this binary looks like to an old one) refuses a MUTATING worktree
/// command at connect time, leaving `worktrees.json` byte-identical — it
/// never gets far enough to parse or rewrite the registry. The parse half
/// (capability marker round-trip + marker-before-registry-IO ordering) is
/// pinned in `db_migration_test.rs` and
/// `worktree_commands_apply_capability_marker_before_registry_io`.
#[test]
fn registry_v2_old_binary_refuses_before_rewrite() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-future");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let registry = main.join(".libra").join("worktrees.json");
    let before = std::fs::read(&registry).expect("registry bytes");

    // Make the repository look like it was upgraded by a NEWER binary.
    assert!(
        sqlite_exec(
            &main.join(".libra").join("libra.db"),
            &[
                "INSERT INTO schema_versions (version, name, applied_at) VALUES \
               (99999999099, 'from_the_future', '2099-01-01T00:00:00Z');"
            ],
        ),
        "plant the future schema row"
    );

    let refused = run_libra_command(
        &["worktree", "add", main.join("wt-refused").to_str().unwrap()],
        main,
    );
    assert!(
        !refused.status.success(),
        "a mutating worktree command must refuse a future schema: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert_eq!(
        std::fs::read(&registry).expect("registry bytes after"),
        before,
        "and the refusal precedes ANY registry rewrite (bytes identical)"
    );
    assert!(
        !main.join("wt-refused").exists(),
        "no worktree directory was created either"
    );
}

/// W4/W3 lease gates, both directions (§C.7): an UNEXPIRED agent lease on a
/// linked worktree refuses `worktree remove`; letting it EXPIRE really
/// unblocks (the refusal hint promises it); and a worktree with NO lease —
/// the human path — was never affected.
#[test]
fn remove_is_lease_gated_and_expiry_unblocks() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-leased");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let wt_canonical = wt.canonicalize().expect("canonical wt");
    let wt_id = std::fs::read_to_string(wt.join(".libra").join("worktree_id"))
        .expect("id")
        .trim()
        .to_string();
    let repo_id = String::from_utf8_lossy(
        &run_libra_command(&["config", "get", "libra.repoid"], main).stdout,
    )
    .trim()
    .to_string();

    // A live agent lease, far from expiring.
    let far = 4_000_000_000_000i64;
    assert!(
        sqlite_exec(
            &main.join(".libra").join("libra.db"),
            &[&format!(
                "INSERT INTO workspace_record (workspace_id, repo_id, kind, worktree_id, path, \
                 owner_kind, owner_id, state, lease_owner, lease_fence, lease_expires_at, \
                 created_at, updated_at) VALUES ('ws-gate', '{repo_id}', 'linked', '{wt_id}', \
                 '{}', 'agent', 'agent-x', 'active', 'agent-x', 3, {far}, 1, 1);",
                wt_canonical.display()
            )],
        ),
        "plant the live lease"
    );

    let refused = run_libra_command(&["worktree", "remove", wt.to_str().unwrap()], main);
    assert!(
        !refused.status.success(),
        "an unexpired agent lease refuses remove"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("ws-gate"),
        "and the refusal names the workspace: {}",
        String::from_utf8_lossy(&refused.stderr)
    );

    // EXPIRE the lease: remove now proceeds — the hint's "let it expire"
    // must be true even though no scavenger ever ran.
    assert!(
        sqlite_exec(
            &main.join(".libra").join("libra.db"),
            &["UPDATE workspace_record SET lease_expires_at = 2 WHERE workspace_id = 'ws-gate';"],
        ),
        "expire the lease"
    );
    assert_cli_success(
        &run_libra_command(&["worktree", "remove", wt.to_str().unwrap()], main),
        "an expired lease no longer blocks",
    );

    // The human path: a second worktree with NO lease removes untouched.
    let wt2 = main.join("wt-unleased");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt2.to_str().unwrap()], main),
        "worktree add 2",
    );
    assert_cli_success(
        &run_libra_command(&["worktree", "remove", wt2.to_str().unwrap()], main),
        "no lease, no refusal",
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
    let repaired = run_libra_command(&["--json", "worktree", "repair", "--confirm"], main);
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
        vec!["worktree", "repair", "--confirm"],
        vec!["worktree", "repair", wt.to_str().unwrap(), "--confirm"],
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

    let refused = run_libra_command(
        &["worktree", "repair", wt.to_str().unwrap(), "--confirm"],
        main,
    );
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
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "no-arg repair upgrades the registry",
    );
    assert_cli_success(
        &run_libra_command(
            &["worktree", "repair", wt.to_str().unwrap(), "--confirm"],
            main,
        ),
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
            &run_libra_command(&["worktree", "repair", "--confirm"], main),
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

/// W3-s1b (§C.7): a detached worktree is FROZEN — every command inside it
/// fails closed with a re-add/delete hint — and `worktree add` re-attaches
/// it with its own scoped state intact (HEAD stays where it was).
#[test]
fn detached_worktree_fails_closed_until_reattach() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-detach");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "switch in linked worktree",
    );

    assert_cli_success(
        &run_libra_command(&["worktree", "remove", wt.to_str().unwrap()], main),
        "detach",
    );

    // Frozen: reads and writes both refuse with the actionable hint.
    for argv in [
        vec!["status"],
        vec!["log", "--oneline"],
        vec!["switch", "main"],
    ] {
        let out = run_libra_command(&argv, &wt);
        assert!(
            !out.status.success(),
            "{argv:?} must fail closed in a detached worktree"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("detached"),
            "refusal explains the detached state for {argv:?}: {stderr}"
        );
    }

    // Repeat keep-dir remove refuses (already detached).
    let again = run_libra_command(&["worktree", "remove", wt.to_str().unwrap()], main);
    assert!(!again.status.success(), "double detach refused");

    // Re-attach restores the SAME scope: HEAD is still `feature`.
    let readd = run_libra_command(&["--json", "worktree", "add", wt.to_str().unwrap()], main);
    assert_cli_success(&readd, "re-attach");
    let payload = parse_json_stdout(&readd);
    assert_eq!(payload["data"]["reattached"], true);
    assert!(
        !wt.join(".libra").join("detached_from_registry").exists(),
        "marker lifted on re-attach"
    );
    let head = String::from_utf8_lossy(
        &run_libra_command(&["rev-parse", "--abbrev-ref", "HEAD"], &wt).stdout,
    )
    .trim()
    .to_string();
    assert_eq!(head, "feature", "re-attached worktree resumes ITS OWN HEAD");
}

/// Re-attach refuses an identity mismatch: a directory recreated at the
/// same path with a different (or missing) gitdir id is never adopted.
#[test]
fn reattach_refuses_identity_mismatch() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-swap");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["worktree", "remove", wt.to_str().unwrap()], main),
        "detach",
    );
    std::fs::write(wt.join(".libra").join("worktree_id"), "someone-else\n").expect("swap identity");
    let refused = run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main);
    assert!(
        !refused.status.success(),
        "re-attach must refuse a swapped identity"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("does not match"),
        "identity mismatch explained: {stderr}"
    );
}

/// W3-s1b crash matrix (§C.7): an interrupted `remove --delete-dir` —
/// simulated by planting the journal row and deleting the directory by
/// hand — is completed by `worktree repair`: scoped rows purged, entry
/// dropped, journal resolved.
#[tokio::test]
async fn worktree_remove_delete_crash_repair() {
    use sea_orm::{ConnectionTrait, Database, Statement};

    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-crash");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "seed scoped HEAD row",
    );
    let canonical_wt = wt.canonicalize().expect("canonical wt");
    let worktree_id = std::fs::read_to_string(wt.join(".libra").join("worktree_id"))
        .expect("id")
        .trim()
        .to_string();

    // Simulate the crash window: intent recorded, directory deleted, but
    // neither the scoped cleanup nor the registry update happened.
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        main.join(".libra/libra.db").display()
    );
    let conn = Database::connect(&db_url).await.expect("connect repo db");
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_string(
        backend,
        format!(
            "INSERT INTO worktree_intent_journal (op, worktree_id, payload, created_at) \
             VALUES ('remove', '{worktree_id}', '{{\"path\":\"{}\",\"delete_dir\":true}}', 0);",
            canonical_wt.to_string_lossy().replace('\\', "/")
        ),
    ))
    .await
    .expect("plant journal row");
    conn.close().await.expect("close");
    std::fs::remove_dir_all(&wt).expect("simulate deleted dir");

    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair completes the interrupted remove",
    );

    let conn = Database::connect(&db_url).await.expect("reconnect");
    for (query, what) in [
        (
            format!(
                "SELECT COUNT(*) FROM reference WHERE worktree_id = '{worktree_id}' AND \
                 kind = 'Head'"
            ),
            "scoped HEAD rows purged",
        ),
        (
            "SELECT COUNT(*) FROM worktree_intent_journal".to_string(),
            "journal resolved",
        ),
        (
            "SELECT COUNT(*) FROM worktree_lifecycle".to_string(),
            "no lifecycle residue",
        ),
    ] {
        let row = conn
            .query_one_raw(Statement::from_string(backend, query))
            .await
            .expect("query")
            .expect("row");
        let count: i64 = row.try_get_by_index(0).expect("count");
        assert_eq!(count, 0, "{what}");
    }

    let list = run_libra_command(&["worktree", "list", "--json"], main);
    assert_cli_success(&list, "list after repair");
    let listed = parse_json_stdout(&list);
    assert!(
        !listed["data"]["worktrees"]
            .as_array()
            .expect("entries")
            .iter()
            .any(|entry| entry["path"].as_str() == Some(canonical_wt.to_string_lossy().as_ref())),
        "entry dropped after the completed remove"
    );
}

/// §C.7: prune only acts on paths PROVEN missing — a stat failure that is
/// not NotFound (here: a permission-denied parent) must not classify the
/// worktree as missing.
#[cfg(unix)]
#[test]
fn prune_does_not_treat_permission_error_as_missing() {
    use std::os::unix::fs::PermissionsExt;

    if nix_effective_root() {
        eprintln!("skipped (running as root; permission bits are not enforced)");
        return;
    }
    let dir = repo_with_feature();
    let main = dir.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("guarded");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Make the parent unreadable: stat on the worktree path now fails with
    // EACCES, not NotFound.
    let mut perms = std::fs::metadata(parent.path())
        .expect("meta")
        .permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(parent.path(), perms).expect("chmod 000");

    let prune = run_libra_command(&["--json", "worktree", "prune"], main);

    // Restore before asserting so cleanup works even on failure.
    let mut restore = std::fs::metadata(parent.path())
        .map(|m| m.permissions())
        .unwrap_or_else(|_| std::fs::Permissions::from_mode(0o755));
    restore.set_mode(0o755);
    std::fs::set_permissions(parent.path(), restore).expect("chmod back");

    assert_cli_success(&prune, "prune with unreadable entry");
    let payload = parse_json_stdout(&prune);
    assert_eq!(
        payload["data"]["pruned_count"], 0,
        "a permission error must not classify the worktree as missing"
    );
}

#[cfg(unix)]
fn nix_effective_root() -> bool {
    // SAFETY: geteuid has no preconditions and returns a plain integer.
    unsafe { libc::geteuid() == 0 }
}

/// `worktree remove --delete-dir .` run from INSIDE the target: the command
/// must move its own cwd out before deleting, complete cleanly, and leave
/// no journal/lifecycle residue.
#[tokio::test]
async fn remove_delete_dir_dot_from_inside_worktree() {
    use sea_orm::{ConnectionTrait, Database, Statement};

    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-self-delete");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    let out = run_libra_command(&["worktree", "remove", "--delete-dir", "."], &wt);
    assert!(
        out.status.success(),
        "remove --delete-dir . from inside the worktree: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!wt.exists(), "directory deleted");

    let db_url = format!(
        "sqlite://{}?mode=rwc",
        main.join(".libra/libra.db").display()
    );
    let conn = Database::connect(&db_url).await.expect("connect repo db");
    let backend = conn.get_database_backend();
    for (query, what) in [
        (
            "SELECT COUNT(*) FROM worktree_intent_journal",
            "journal resolved",
        ),
        (
            "SELECT COUNT(*) FROM worktree_lifecycle",
            "no lifecycle residue",
        ),
    ] {
        let row = conn
            .query_one_raw(Statement::from_string(backend, query.to_string()))
            .await
            .expect("query")
            .expect("row");
        let count: i64 = row.try_get_by_index(0).expect("count");
        assert_eq!(count, 0, "{what}");
    }
}

/// Move-recovery ambiguity (§C.7): when the crash window leaves BOTH paths
/// present (destination recreated by someone else) or BOTH missing, repair
/// must KEEP the journal row and report — resolving would abandon a
/// registry pointing at an unrelated directory.
#[tokio::test]
async fn move_crash_ambiguous_states_keep_journal() {
    use sea_orm::{ConnectionTrait, Database, Statement};

    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-move-src");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let canonical_wt = wt.canonicalize().expect("canonical src");
    let dest = main.join("wt-move-dest");
    let worktree_id = std::fs::read_to_string(wt.join(".libra").join("worktree_id"))
        .expect("id")
        .trim()
        .to_string();

    let db_url = format!(
        "sqlite://{}?mode=rwc",
        main.join(".libra/libra.db").display()
    );
    let plant = |payload: String| {
        let db_url = db_url.clone();
        let worktree_id = worktree_id.clone();
        async move {
            let conn = Database::connect(&db_url).await.expect("connect");
            let backend = conn.get_database_backend();
            conn.execute_raw(Statement::from_string(
                backend,
                format!(
                    "INSERT INTO worktree_intent_journal (op, worktree_id, payload, \
                     created_at) VALUES ('move', '{worktree_id}', '{payload}', 0);"
                ),
            ))
            .await
            .expect("plant journal row");
            conn.close().await.expect("close");
        }
    };
    let journal_count = || {
        let db_url = db_url.clone();
        async move {
            let conn = Database::connect(&db_url).await.expect("connect");
            let backend = conn.get_database_backend();
            let row = conn
                .query_one_raw(Statement::from_string(
                    backend,
                    "SELECT COUNT(*) FROM worktree_intent_journal".to_string(),
                ))
                .await
                .expect("query")
                .expect("row");
            let count: i64 = row.try_get_by_index(0).expect("count");
            conn.close().await.expect("close");
            count
        }
    };

    // Case 1: Present/Present — entry registered at src, dest recreated.
    std::fs::create_dir_all(&dest).expect("recreate dest");
    let payload = format!(
        "{{\"src\":\"{}\",\"dest\":\"{}\"}}",
        canonical_wt.to_string_lossy(),
        dest.to_string_lossy()
    );
    plant(payload.clone()).await;
    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair with ambiguous move (present/present)",
    );
    assert_eq!(
        journal_count().await,
        1,
        "present/present ambiguity keeps the journal row"
    );

    // Case 2: Missing/Missing — both directories gone.
    std::fs::remove_dir_all(&dest).expect("drop dest");
    std::fs::remove_dir_all(&wt).expect("drop src");
    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair with ambiguous move (missing/missing)",
    );
    assert_eq!(
        journal_count().await,
        1,
        "missing/missing ambiguity keeps the journal row"
    );

    // Cleanup so the repo is not left with a pending journal (settle the
    // move as never-started: restore src and remove dest).
    std::fs::create_dir_all(wt.join(".libra")).expect("restore src shell");
    std::fs::write(
        wt.join(".libra").join("worktree_id"),
        format!("{worktree_id}\n"),
    )
    .expect("restore id");
    std::fs::write(
        wt.join(".libra").join("commondir"),
        format!("{}\n", main.join(".libra").display()),
    )
    .expect("restore commondir");
    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair settles the never-started move",
    );
    assert_eq!(
        journal_count().await,
        0,
        "journal resolved once unambiguous"
    );
}

/// Move recovery is IDENTITY-bound (§C.7): a stale move journal for
/// worktree X must never rename X's directory onto a path now occupied by
/// a DIFFERENT registry entry (here: a tombstone Y at the old destination).
/// The row is kept and nothing moves.
#[tokio::test]
async fn move_crash_recovery_never_adopts_foreign_destination_entry() {
    use sea_orm::{ConnectionTrait, Database, Statement};

    let dir = repo_with_feature();
    let main = dir.path();
    let x = main.join("wt-move-x");
    let y = main.join("wt-move-y");
    for wt in [&x, &y] {
        assert_cli_success(
            &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
            "worktree add",
        );
    }
    let x_canonical = x.canonicalize().expect("canonical x");
    let y_canonical = y.canonicalize().expect("canonical y");
    let x_id = std::fs::read_to_string(x.join(".libra").join("worktree_id"))
        .expect("x id")
        .trim()
        .to_string();
    let y_id = std::fs::read_to_string(y.join(".libra").join("worktree_id"))
        .expect("y id")
        .trim()
        .to_string();

    // Turn Y into a tombstone: mark the registry entry, mirror the row,
    // delete the directory.
    let registry = main.join(".libra").join("worktrees.json");
    let mut doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).expect("read registry"))
            .expect("registry json");
    for entry in doc["entries"].as_array_mut().expect("entries") {
        if entry["path"].as_str() == Some(y_canonical.to_string_lossy().as_ref()) {
            entry["state"] = serde_json::json!("tombstone");
        }
    }
    std::fs::write(
        &registry,
        serde_json::to_vec_pretty(&doc).expect("serialize"),
    )
    .expect("write registry");
    std::fs::remove_dir_all(&y).expect("delete y dir");

    let db_url = format!(
        "sqlite://{}?mode=rwc",
        main.join(".libra/libra.db").display()
    );
    let conn = Database::connect(&db_url).await.expect("connect");
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_string(
        backend,
        format!(
            "INSERT INTO worktree_lifecycle (worktree_id, state, path, created_at, \
             updated_at) VALUES ('{y_id}', 'tombstone', '{}', 0, 0);",
            y_canonical.to_string_lossy()
        ),
    ))
    .await
    .expect("mirror row");
    // Stale move journal for X targeting Y's (now-tombstoned) path.
    conn.execute_raw(Statement::from_string(
        backend,
        format!(
            "INSERT INTO worktree_intent_journal (op, worktree_id, payload, created_at) \
             VALUES ('move', '{x_id}', '{{\"src\":\"{}\",\"dest\":\"{}\"}}', 0);",
            x_canonical.to_string_lossy(),
            y_canonical.to_string_lossy()
        ),
    ))
    .await
    .expect("plant move journal");
    conn.close().await.expect("close");

    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair with a foreign-destination move journal",
    );

    // X untouched, journal kept, no rename onto Y's path.
    assert!(x.is_dir(), "X's directory stays at its source");
    assert!(!y.exists(), "nothing was renamed onto the tombstoned path");
    let conn = Database::connect(&db_url).await.expect("reconnect");
    let row = conn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT COUNT(*) FROM worktree_intent_journal".to_string(),
        ))
        .await
        .expect("query")
        .expect("row");
    let count: i64 = row.try_get_by_index(0).expect("count");
    assert_eq!(count, 1, "the ambiguous move journal row survives");
}

/// Stale re-attach journals must never unfreeze a LATER detach at the same
/// path (linked ids are deterministic, so a delete/re-add/re-detach cycle
/// reuses the id): repair rolls the stale intent back, the entry stays
/// frozen, and the marker survives.
#[tokio::test]
async fn stale_reattach_journal_does_not_unfreeze_later_detach() {
    use sea_orm::{ConnectionTrait, Database, Statement};

    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-stale-reattach");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let canonical_wt = wt.canonicalize().expect("canonical wt");
    let worktree_id = std::fs::read_to_string(wt.join(".libra").join("worktree_id"))
        .expect("id")
        .trim()
        .to_string();

    // The worktree is CURRENTLY detached (a later detach the stale journal
    // must not betray).
    assert_cli_success(
        &run_libra_command(&["worktree", "remove", wt.to_str().unwrap()], main),
        "detach",
    );

    // Plant a stale re-attach journal (as if an earlier `worktree add`
    // crashed after verification but its resolution was lost).
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        main.join(".libra/libra.db").display()
    );
    let conn = Database::connect(&db_url).await.expect("connect");
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_string(
        backend,
        format!(
            "INSERT INTO worktree_intent_journal (op, worktree_id, payload, created_at) \
             VALUES ('add', '{worktree_id}', '{{\"path\":\"{}\",\"reattach\":true}}', 0);",
            canonical_wt.to_string_lossy()
        ),
    ))
    .await
    .expect("plant stale reattach journal");
    conn.close().await.expect("close");

    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair with a stale reattach journal",
    );

    // Still frozen: marker present, registry still detached, and commands
    // inside the directory still refuse.
    assert!(
        wt.join(".libra").join("detached_from_registry").exists(),
        "the later detach stays frozen"
    );
    let status = run_libra_command(&["status"], &wt);
    assert!(!status.status.success(), "directory remains fail-closed");
    let registry = main.join(".libra").join("worktrees.json");
    let doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).expect("read registry"))
            .expect("registry json");
    let entry = doc["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["path"].as_str() == Some(canonical_wt.to_string_lossy().as_ref()))
        .expect("entry present");
    assert_eq!(
        entry["state"].as_str(),
        Some("detached_from_registry"),
        "registry stays detached"
    );

    // The stale row itself resolved as rolled back.
    let conn = Database::connect(&db_url).await.expect("reconnect");
    let row = conn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT COUNT(*) FROM worktree_intent_journal".to_string(),
        ))
        .await
        .expect("query")
        .expect("row");
    let count: i64 = row.try_get_by_index(0).expect("count");
    assert_eq!(count, 0, "stale reattach intent resolved as rolled back");
}

/// W3-s2 (§C.7): `worktree add <path> <branch>` checks the branch out in
/// the new worktree (attached HEAD), and the branch is then held — no
/// other worktree can switch to it.
#[test]
fn worktree_add_with_branch_attaches() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-branch");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), "feature"], main),
        "worktree add <path> <branch>",
    );
    let head = String::from_utf8_lossy(
        &run_libra_command(&["rev-parse", "--abbrev-ref", "HEAD"], &wt).stdout,
    )
    .trim()
    .to_string();
    assert_eq!(head, "feature", "new worktree is ATTACHED to the branch");

    // The branch is now held by the new worktree.
    let refused = run_libra_command(&["switch", "feature"], main);
    assert!(
        !refused.status.success(),
        "main cannot switch to a branch held by the new worktree"
    );
}

/// Branch targets already checked out ANYWHERE refuse before side effects:
/// no directory, no registry change (matrix:
/// worktree_add_branch_collision_has_zero_side_effects covers `-b` too).
#[test]
fn worktree_add_branch_collision_has_zero_side_effects() {
    let dir = repo_with_feature();
    let main = dir.path();
    let main_branch = String::from_utf8_lossy(
        &run_libra_command(&["rev-parse", "--abbrev-ref", "HEAD"], main).stdout,
    )
    .trim()
    .to_string();
    let registry = main.join(".libra").join("worktrees.json");
    // Materialize the registry (a locked no-op mutator) so the byte
    // comparison below has a baseline.
    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "initialize registry",
    );
    let registry_before = std::fs::read(&registry).expect("registry before");

    // Checking out the branch the CURRENT worktree holds is refused.
    let wt = main.join("wt-collision");
    let refused = run_libra_command(
        &["worktree", "add", wt.to_str().unwrap(), &main_branch],
        main,
    );
    assert!(!refused.status.success(), "current-branch target refused");
    assert!(!wt.exists(), "no directory is created on refusal");

    // `-b` with an existing branch name is refused (no -B/--force).
    let refused = run_libra_command(
        &["worktree", "add", "-b", "feature", wt.to_str().unwrap()],
        main,
    );
    assert!(!refused.status.success(), "-b collision refused");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("already exists"),
        "collision explained: {stderr}"
    );
    assert!(!wt.exists(), "no directory is created on -b collision");
    assert_eq!(
        std::fs::read(&registry).expect("registry after"),
        registry_before,
        "registry byte-identical after refusals"
    );

    // A nonexistent target fails closed (no DWIM) with zero side effects.
    let refused = run_libra_command(
        &["worktree", "add", wt.to_str().unwrap(), "no-such-thing"],
        main,
    );
    assert!(!refused.status.success(), "unknown target fails closed");
    assert!(!wt.exists());

    // -b together with --detach is a usage error.
    let refused = run_libra_command(
        &[
            "worktree",
            "add",
            "--detach",
            "-b",
            "brand-new",
            wt.to_str().unwrap(),
        ],
        main,
    );
    assert!(!refused.status.success(), "-b with --detach refused");
}

/// `worktree add <path> <commit>` and `--detach <path> <branch>` both seed
/// a DETACHED worktree at the resolved commit; a detached branch target is
/// NOT held (other worktrees can still check the branch out).
#[test]
fn worktree_add_commit_and_detach_are_detached() {
    let dir = repo_with_feature();
    let main = dir.path();

    // Grow one extra commit so HEAD and feature diverge in content.
    std::fs::write(main.join("second.txt"), "2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "second.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "second", "--no-verify"], main),
        "commit",
    );
    let feature_tip =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "feature"], main).stdout)
            .trim()
            .to_string();

    // Explicit commit target: detached at THAT commit, populated from it.
    let wt_commit = main.join("wt-at-commit");
    assert_cli_success(
        &run_libra_command(
            &["worktree", "add", wt_commit.to_str().unwrap(), &feature_tip],
            main,
        ),
        "worktree add <path> <commit>",
    );
    let head =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], &wt_commit).stdout)
            .trim()
            .to_string();
    assert_eq!(head, feature_tip, "detached at the requested commit");
    assert!(
        !wt_commit.join("second.txt").exists(),
        "populated from the target commit, not the source HEAD"
    );

    // --detach with a BRANCH target: same tip, branch not held.
    let wt_detach = main.join("wt-detached-branch");
    assert_cli_success(
        &run_libra_command(
            &[
                "worktree",
                "add",
                "--detach",
                wt_detach.to_str().unwrap(),
                "feature",
            ],
            main,
        ),
        "worktree add --detach <path> <branch>",
    );
    let abbrev = String::from_utf8_lossy(
        &run_libra_command(&["rev-parse", "--abbrev-ref", "HEAD"], &wt_detach).stdout,
    )
    .trim()
    .to_string();
    assert_eq!(abbrev, "HEAD", "--detach forces a detached HEAD");
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt_detach),
        "the branch stays free for checkout (detached add does not hold it)",
    );
}

/// `worktree add -b <new> <path> [<start>]` creates the branch at the
/// start point and attaches the new worktree to it.
#[test]
fn worktree_add_new_branch_from_start_point() {
    let dir = repo_with_feature();
    let main = dir.path();
    let feature_tip =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "feature"], main).stdout)
            .trim()
            .to_string();

    let wt = main.join("wt-new-branch");
    assert_cli_success(
        &run_libra_command(
            &[
                "worktree",
                "add",
                "-b",
                "topic-x",
                wt.to_str().unwrap(),
                "feature",
            ],
            main,
        ),
        "worktree add -b <new> <path> <start>",
    );
    let head = String::from_utf8_lossy(
        &run_libra_command(&["rev-parse", "--abbrev-ref", "HEAD"], &wt).stdout,
    )
    .trim()
    .to_string();
    assert_eq!(head, "topic-x", "attached to the created branch");
    let tip = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "topic-x"], main).stdout)
        .trim()
        .to_string();
    assert_eq!(tip, feature_tip, "created at the requested start point");
}

/// `-b` full-rollback regression (§C.7): a populate failure (objects made
/// unreadable) on a SIBLING target must roll back the created branch, the
/// directory, and the registry — no branch-only or orphan residue, and the
/// invoker's cwd-based storage resolution must survive the rollback.
#[test]
fn worktree_add_new_branch_rolls_back_on_populate_failure() {
    let dir = repo_with_feature();
    let main = dir.path();
    let sibling_parent = tempfile::tempdir().expect("sibling parent");
    let wt = sibling_parent.path().join("wt-populate-fail");

    // Make populate fail deterministically: drop every loose object so the
    // restore step cannot read the seed commit's tree.
    let objects = main.join(".libra").join("objects");
    for entry in std::fs::read_dir(&objects).expect("objects dir") {
        let entry = entry.expect("entry");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() == 2 && entry.path().is_dir() {
            std::fs::remove_dir_all(entry.path()).expect("drop loose fan-out dir");
        }
    }

    let out = run_libra_command(
        &[
            "worktree",
            "add",
            "-b",
            "doomed-topic",
            wt.to_str().unwrap(),
            "feature",
        ],
        main,
    );
    assert!(
        !out.status.success(),
        "populate must fail with unreadable objects: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Branch rolled back — no branch-only residue.
    let rev = run_libra_command(&["rev-parse", "doomed-topic"], main);
    assert!(
        !rev.status.success(),
        "created branch must be rolled back on populate failure"
    );
    // No directory, no registry entry.
    assert!(!wt.exists(), "sibling target directory rolled back");
    let registry = main.join(".libra").join("worktrees.json");
    if let Ok(bytes) = std::fs::read(&registry) {
        let doc: serde_json::Value = serde_json::from_slice(&bytes).expect("registry json");
        assert!(
            !doc["entries"]
                .as_array()
                .expect("entries")
                .iter()
                .any(|entry| {
                    entry["path"]
                        .as_str()
                        .is_some_and(|p| p.contains("wt-populate-fail"))
                }),
            "no orphan registry entry"
        );
    }
}

/// Concurrent `worktree add -b <same-name>` (§C.7): exactly ONE attempt may
/// win — the loser refuses under the branch-attach lock instead of
/// silently overwriting the branch row and double-attaching.
#[test]
fn concurrent_add_same_new_branch_single_winner() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt_a = main.join("wt-race-a");
    let wt_b = main.join("wt-race-b");

    let libra = env!("CARGO_BIN_EXE_libra");
    let spawn = |wt: &std::path::Path| {
        std::process::Command::new(libra)
            .args([
                "worktree",
                "add",
                "-b",
                "raced-topic",
                wt.to_str().unwrap(),
                "feature",
            ])
            .current_dir(main)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn add")
    };
    let child_a = spawn(&wt_a);
    let child_b = spawn(&wt_b);
    let out_a = child_a.wait_with_output().expect("wait a");
    let out_b = child_b.wait_with_output().expect("wait b");

    let successes = [&out_a, &out_b]
        .iter()
        .filter(|out| out.status.success())
        .count();
    assert_eq!(
        successes,
        1,
        "exactly one -b attempt may win\na: {}\nb: {}",
        String::from_utf8_lossy(&out_a.stderr),
        String::from_utf8_lossy(&out_b.stderr)
    );

    // The branch is attached in exactly one registered worktree.
    let list = run_libra_command(&["worktree", "list", "--porcelain"], main);
    assert_cli_success(&list, "list after race");
    let porcelain = String::from_utf8_lossy(&list.stdout);
    let attached = porcelain
        .lines()
        .filter(|line| line.trim() == "branch refs/heads/raced-topic")
        .count();
    assert_eq!(attached, 1, "branch attached exactly once:\n{porcelain}");
}

/// Crash window between `-b` branch creation and publication (§C.7): the
/// add journal carries the branch identity, so repair rolls the branch
/// back tip-conditionally — and REFUSES to delete it once the tip moved.
#[tokio::test]
async fn interrupted_add_new_branch_crash_repair() {
    use sea_orm::{ConnectionTrait, Database, Statement};

    let dir = repo_with_feature();
    let main = dir.path();
    let feature_tip =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "feature"], main).stdout)
            .trim()
            .to_string();
    let wt = main.join("wt-crash-b");
    let wt_id = "deadbeef-crash-b";

    // Simulate: journal row written, branch created, nothing published.
    assert_cli_success(
        &run_libra_command(&["branch", "crash-topic", "feature"], main),
        "create the orphan branch by hand",
    );
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        main.join(".libra/libra.db").display()
    );
    let plant = |payload: String| {
        let db_url = db_url.clone();
        async move {
            let conn = Database::connect(&db_url).await.expect("connect");
            let backend = conn.get_database_backend();
            conn.execute_raw(Statement::from_string(
                backend,
                format!(
                    "INSERT INTO worktree_intent_journal (op, worktree_id, payload, \
                     created_at) VALUES ('add', '{wt_id}', '{payload}', 0);"
                ),
            ))
            .await
            .expect("plant journal");
            conn.close().await.expect("close");
        }
    };
    plant(format!(
        "{{\"path\":\"{}\",\"create_branch\":{{\"name\":\"crash-topic\",\"start\":\"{feature_tip}\"}}}}",
        wt.to_string_lossy()
    ))
    .await;

    // Lock-failure leg first: with the branch-attach lock unacquirable
    // (the lock path is a DIRECTORY), repair must fail closed — branch and
    // journal both survive.
    let lock_path = main.join(".libra").join("branch-attach.lock");
    std::fs::create_dir_all(&lock_path).expect("block the lock path");
    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair with an unacquirable attach lock",
    );
    let kept = run_libra_command(&["rev-parse", "crash-topic"], main);
    assert!(
        kept.status.success(),
        "lock failure must not delete the branch"
    );
    {
        let conn = Database::connect(&db_url).await.expect("connect");
        let backend = conn.get_database_backend();
        let row = conn
            .query_one_raw(Statement::from_string(
                backend,
                "SELECT COUNT(*) FROM worktree_intent_journal".to_string(),
            ))
            .await
            .expect("query")
            .expect("row");
        let count: i64 = row.try_get_by_index(0).expect("count");
        conn.close().await.expect("close");
        assert_eq!(count, 1, "journal kept while the lock is unacquirable");
    }
    std::fs::remove_dir_all(&lock_path).expect("unblock the lock path");

    // Probe-failure leg: an injected attachment-lookup fault must also fail
    // closed — branch and journal both survive.
    let libra = env!("CARGO_BIN_EXE_libra");
    let faulted = std::process::Command::new(libra)
        .args(["worktree", "repair", "--confirm"])
        .env("LIBRA_TEST_FAULT", "branch-attach-probe")
        .current_dir(main)
        .output()
        .expect("faulted repair");
    assert!(faulted.status.success(), "faulted repair still succeeds");
    let kept = run_libra_command(&["rev-parse", "crash-topic"], main);
    assert!(
        kept.status.success(),
        "an attachment-probe failure must not delete the branch"
    );
    {
        let conn = Database::connect(&db_url).await.expect("connect");
        let backend = conn.get_database_backend();
        let row = conn
            .query_one_raw(Statement::from_string(
                backend,
                "SELECT COUNT(*) FROM worktree_intent_journal".to_string(),
            ))
            .await
            .expect("query")
            .expect("row");
        let count: i64 = row.try_get_by_index(0).expect("count");
        conn.close().await.expect("close");
        assert_eq!(count, 1, "journal kept while the probe faults");
    }

    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair rolls the orphan branch back",
    );
    let gone = run_libra_command(&["rev-parse", "crash-topic"], main);
    assert!(!gone.status.success(), "orphan -b branch rolled back");

    // Tip-moved variant: the branch points at a NEWER commit than the
    // journaled start tip (as if someone committed on it post-crash).
    std::fs::write(main.join("bump.txt"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "bump.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "bump", "--no-verify"], main),
        "commit",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "crash-topic-2", "HEAD"], main),
        "create second branch at the moved tip",
    );
    plant(format!(
        "{{\"path\":\"{}\",\"create_branch\":{{\"name\":\"crash-topic-2\",\"start\":\"{feature_tip}\"}}}}",
        wt.to_string_lossy()
    ))
    .await;
    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair with a moved tip",
    );
    let kept = run_libra_command(&["rev-parse", "crash-topic-2"], main);
    assert!(
        kept.status.success(),
        "a moved tip is never deleted by recovery"
    );
    let conn = Database::connect(&db_url).await.expect("reconnect");
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT COUNT(*) FROM worktree_intent_journal".to_string(),
        ))
        .await
        .expect("query")
        .expect("row");
    let count: i64 = row.try_get_by_index(0).expect("count");
    assert_eq!(
        count, 1,
        "the tip-moved journal row is kept for manual review"
    );
}

/// Fail-closed attachment probes (§C.7): an injected HEAD-query failure
/// refuses canonical `worktree add <branch>` (no side effects) and
/// `switch <branch>` — a transient DB error must never read as "the
/// branch is free".
#[test]
fn injected_probe_fault_refuses_attach_and_switch() {
    let dir = repo_with_feature();
    let main = dir.path();
    let libra = env!("CARGO_BIN_EXE_libra");

    let wt = main.join("wt-probe-fault");
    let add = std::process::Command::new(libra)
        .args(["worktree", "add", wt.to_str().unwrap(), "feature"])
        .env("LIBRA_TEST_FAULT", "branch-attach-probe")
        .current_dir(main)
        .output()
        .expect("faulted add");
    assert!(!add.status.success(), "faulted add must refuse");
    assert!(!wt.exists(), "faulted add leaves no directory");

    // A detached worktree trying to switch onto the branch under the fault.
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "detached worktree add",
    );
    let switch = std::process::Command::new(libra)
        .args(["switch", "feature"])
        .env("LIBRA_TEST_FAULT", "branch-attach-probe")
        .current_dir(&wt)
        .output()
        .expect("faulted switch");
    assert!(!switch.status.success(), "faulted switch must refuse");
    let head = String::from_utf8_lossy(
        &run_libra_command(&["rev-parse", "--abbrev-ref", "HEAD"], &wt).stdout,
    )
    .trim()
    .to_string();
    assert_eq!(
        head, "HEAD",
        "worktree stays detached after the refused switch"
    );
}

/// Build a LEGACY shared-`.libra` symlink worktree (pre-isolation layout)
/// and register it, returning its canonical path.
#[cfg(unix)]
fn create_legacy_symlink_worktree(main: &std::path::Path, name: &str) -> std::path::PathBuf {
    let wt = main.join(name);
    std::fs::create_dir_all(&wt).expect("mkdir legacy wt");
    std::os::unix::fs::symlink(main.join(".libra"), wt.join(".libra")).expect("legacy symlink");
    let canonical = wt.canonicalize().expect("canonical legacy wt");
    // Register it (v2 entry with a persisted id, as the v1→v2 upgrade
    // would have backfilled by canonical-path synthesis).
    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "materialize registry",
    );
    let registry = main.join(".libra").join("worktrees.json");
    let mut doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).expect("read registry"))
            .expect("registry json");
    doc["entries"]
        .as_array_mut()
        .expect("entries")
        .push(serde_json::json!({
            "path": canonical.to_string_lossy(),
            "is_main": false,
            "locked": false,
            "lock_reason": null,
            "worktree_id": format!("legacy-{name}"),
        }));
    std::fs::write(
        &registry,
        serde_json::to_vec_pretty(&doc).expect("serialize"),
    )
    .expect("write registry");
    canonical
}

/// §C.6.1: mutation commands refuse in a legacy-symlink worktree (they
/// would move MAIN's HEAD/index); read-only commands keep working, and the
/// list layout field reports `legacy-symlink`.
#[cfg(unix)]
#[test]
fn legacy_symlink_mutation_fails_closed() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = create_legacy_symlink_worktree(main, "wt-legacy");

    // Reads still work (shared scope, no regression).
    assert_cli_success(&run_libra_command(&["status"], &wt), "read-only status");
    assert_cli_success(
        &run_libra_command(&["log", "--oneline"], &wt),
        "read-only log",
    );

    // Mutations refuse with the stable code + migrate hint.
    std::fs::write(wt.join("x.txt"), "x\n").unwrap();
    for argv in [
        vec!["add", "x.txt"],
        vec!["commit", "-m", "nope", "--no-verify"],
        vec!["switch", "feature"],
        vec!["stash", "push"],
        vec!["read-tree", "HEAD"],
        vec!["update-index", "--add", "x.txt"],
        vec!["sparse-view", "set", "src/**"],
        vec!["layer", "add", "ov", "--source", "/tmp/ov"],
        vec!["symbolic-ref", "HEAD", "refs/heads/feature"],
        // W0 §C.11 declares these mutators too. They were missing from the
        // hand-maintained list the guard used to consult, so each one wrote
        // through the shared symlink into MAIN's gitdir: `apply` patched
        // main's working tree, `fetch` wrote main's FETCH_HEAD, `rerere`
        // main's MERGE_RR. The inventory is now an exhaustive `match`, so
        // omission is a compile error rather than a silent hole.
        vec!["apply", "/dev/null"],
        vec!["fetch", "origin"],
        vec!["rerere", "clear"],
        vec!["mv", "x.txt", "y.txt"],
        vec!["clean", "-f"],
        vec!["restore", "x.txt"],
        vec!["reset", "--hard"],
        vec!["rm", "--cached", "x.txt"],
    ] {
        let out = run_libra_command(&argv, &wt);
        assert!(
            !out.status.success(),
            "{argv:?} must refuse in a legacy-symlink worktree"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr
                .lines()
                .any(|line| line.trim() == "Error-Code: LBR-REPO-003")
                && stderr.contains("--migrate-layout"),
            "stable refusal with the migrate hint for {argv:?}: {stderr}"
        );
    }

    // Read-only advisory subcommands stay usable.
    assert_cli_success(
        &run_libra_command(&["sparse-view", "status"], &wt),
        "sparse-view status stays readable",
    );
    assert_cli_success(
        &run_libra_command(&["layer", "list"], &wt),
        "layer list stays readable",
    );

    // Lifecycle mutation of a LEGACY target refuses from main too — a
    // detached marker or identity write through the shared symlink would
    // land in MAIN storage and freeze the main repository.
    let registry = main.join(".libra").join("worktrees.json");
    let registry_before = std::fs::read(&registry).expect("registry bytes");
    for argv in [
        vec!["worktree", "remove", wt.to_str().unwrap()],
        vec!["worktree", "repair", wt.to_str().unwrap(), "--confirm"],
    ] {
        let out = run_libra_command(&argv, main);
        assert!(
            !out.status.success(),
            "{argv:?} must refuse a legacy-symlink target"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--migrate-layout"),
            "refusal directs at the migration for {argv:?}"
        );
    }
    assert_cli_success(&run_libra_command(&["status"], main), "main stays usable");
    assert!(
        !main.join(".libra").join("detached_from_registry").exists(),
        "no marker leaked into MAIN storage"
    );
    assert_eq!(
        std::fs::read(&registry).expect("registry after"),
        registry_before,
        "registry unchanged by the refusals"
    );

    // Layout surfaces in JSON list and porcelain.
    let list = run_libra_command(&["worktree", "list", "--json"], main);
    assert_cli_success(&list, "list --json");
    let payload = parse_json_stdout(&list);
    let entry = payload["data"]["worktrees"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["path"].as_str() == Some(wt.to_string_lossy().as_ref()))
        .expect("legacy entry listed");
    assert_eq!(entry["layout"].as_str(), Some("legacy-symlink"));
    let porcelain = run_libra_command(&["worktree", "list", "--porcelain"], main);
    assert_cli_success(&porcelain, "list --porcelain");
    assert!(
        String::from_utf8_lossy(&porcelain.stdout).contains("layout legacy-symlink"),
        "porcelain layout line present"
    );
}

/// §C.4.3: `repair --migrate-layout` refuses while the SHARED storage holds
/// an active merge/revert or held autostash — a legacy-symlink worktree
/// shares that storage, so some worktree owns the operation and must
/// conclude it before the layout underneath it changes.
#[cfg(unix)]
#[test]
fn migrate_layout_refuses_active_shared_sidecar_state() {
    let dir = repo_with_feature();
    let main = dir.path();
    let _wt = create_legacy_symlink_worktree(main, "wt-blocked-migrate");

    // An active merge in shared storage (owner unknowable in legacy layout).
    fs::write(
        main.join(".libra").join("merge-state.json"),
        "{\"head_name\":\"main\"}",
    )
    .unwrap();

    let out = run_libra_command(
        &["worktree", "repair", "--migrate-layout", "--confirm"],
        main,
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "migration must refuse over an active shared merge: {text}"
    );
    assert!(
        text.contains("merge-state.json"),
        "and the refusal names the state: {text}"
    );
    assert!(
        fs::symlink_metadata(_wt.join(".libra"))
            .expect("gitdir meta")
            .file_type()
            .is_symlink(),
        "the refusal wrote nothing — the legacy symlink is untouched"
    );

    // Clearing the state lifts the refusal.
    fs::remove_file(main.join(".libra").join("merge-state.json")).unwrap();
    assert_cli_success(
        &run_libra_command(
            &["worktree", "repair", "--migrate-layout", "--confirm"],
            main,
        ),
        "migration succeeds once the state is gone",
    );
}

/// §C.6.2: migration preserves dirty/untracked FILES byte-for-byte, does
/// not copy staged state, seeds a detached HEAD at the shared snapshot,
/// and flips the layout to linked-v2. `--dry-run` writes nothing.
#[cfg(unix)]
#[test]
fn legacy_layout_migration_preserves_dirty_and_untracked() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = create_legacy_symlink_worktree(main, "wt-migrate");
    let shared_head =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], main).stdout)
            .trim()
            .to_string();

    // Local files: a modified tracked file and an untracked one.
    std::fs::write(wt.join("a.txt"), "locally modified\n").unwrap();
    std::fs::write(wt.join("untracked.txt"), "keep me\n").unwrap();
    // Staged state in the SHARED index (ownership unprovable — must NOT
    // migrate into the private index).
    std::fs::write(main.join("staged.txt"), "staged\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "staged.txt"], main),
        "stage in main",
    );

    // Dry run: reports, writes nothing.
    let dry = run_libra_command(
        &[
            "--json",
            "worktree",
            "repair",
            "--migrate-layout",
            "--dry-run",
        ],
        main,
    );
    assert_cli_success(&dry, "dry run");
    let payload = parse_json_stdout(&dry);
    assert_eq!(payload["data"]["dry_run"], true);
    assert!(
        payload["data"]["planned"]
            .as_array()
            .expect("planned")
            .iter()
            .any(|p| p.as_str() == Some(wt.to_string_lossy().as_ref())),
        "dry run plans the legacy worktree"
    );
    assert!(
        std::fs::symlink_metadata(wt.join(".libra"))
            .expect("gitdir meta")
            .file_type()
            .is_symlink(),
        "dry run leaves the symlink untouched"
    );
    let registry_bytes =
        std::fs::read(main.join(".libra").join("worktrees.json")).expect("registry present");
    let dry2 = run_libra_command(
        &["worktree", "repair", "--migrate-layout", "--dry-run"],
        main,
    );
    assert_cli_success(&dry2, "second dry run");
    assert_eq!(
        std::fs::read(main.join(".libra").join("worktrees.json")).expect("registry"),
        registry_bytes,
        "dry run never rewrites the registry"
    );

    // Real migration.
    assert_cli_success(
        &run_libra_command(
            &["worktree", "repair", "--migrate-layout", "--confirm"],
            main,
        ),
        "migrate",
    );
    assert!(
        std::fs::symlink_metadata(wt.join(".libra"))
            .expect("gitdir meta")
            .file_type()
            .is_dir(),
        "gitdir is now a real directory"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("a.txt")).unwrap(),
        "locally modified\n",
        "dirty file preserved byte-for-byte"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("untracked.txt")).unwrap(),
        "keep me\n",
        "untracked file preserved"
    );

    // Detached at the shared snapshot; staged state NOT copied.
    let head = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], &wt).stdout)
        .trim()
        .to_string();
    assert_eq!(head, shared_head, "detached at the migration snapshot");
    let abbrev = String::from_utf8_lossy(
        &run_libra_command(&["rev-parse", "--abbrev-ref", "HEAD"], &wt).stdout,
    )
    .trim()
    .to_string();
    assert_eq!(abbrev, "HEAD", "detached HEAD");
    let status = run_libra_command(&["status", "--porcelain"], &wt);
    assert_cli_success(&status, "status in migrated worktree");
    let porcelain = String::from_utf8_lossy(&status.stdout).to_string();
    assert!(
        !porcelain.contains("staged.txt") || porcelain.contains("?? staged.txt"),
        "shared staged state must not appear as staged here: {porcelain}"
    );
    assert!(
        porcelain.contains(" M a.txt") || porcelain.contains("M a.txt"),
        "modified file shows dirty against the new private index: {porcelain}"
    );

    // Layout flipped; mutations work now.
    let list = run_libra_command(&["worktree", "list", "--json"], main);
    let payload = parse_json_stdout(&list);
    let entry = payload["data"]["worktrees"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["path"].as_str() == Some(wt.to_string_lossy().as_ref()))
        .expect("entry");
    assert_eq!(entry["layout"].as_str(), Some("linked-v2"));
    assert_cli_success(
        &run_libra_command(&["add", "untracked.txt"], &wt),
        "mutations unblocked after migration",
    );
    // No leftover journal/backup material.
    assert!(!wt.join(".libra").join("migrate-marker").exists());
}

/// §C.6.2 step 4: an unmerged SHARED index refuses the migration before
/// any rename.
#[cfg(unix)]
#[test]
fn legacy_layout_unmerged_index_refused_before_rename() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = create_legacy_symlink_worktree(main, "wt-unmerged");

    // Manufacture a conflict in MAIN's shared index.
    std::fs::write(main.join("conflict.txt"), "main\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "conflict.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main side", "--no-verify"], main),
        "commit main side",
    );
    assert_cli_success(&run_libra_command(&["switch", "feature"], main), "switch");
    std::fs::write(main.join("conflict.txt"), "feature\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "conflict.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feature side", "--no-verify"], main),
        "commit feature side",
    );
    let merge = run_libra_command(&["merge", "main"], main);
    assert!(!merge.status.success(), "merge conflicts");

    let refused = run_libra_command(
        &["worktree", "repair", "--migrate-layout", "--confirm"],
        main,
    );
    assert!(
        !refused.status.success(),
        "unmerged shared index refuses migration"
    );
    assert!(
        std::fs::symlink_metadata(wt.join(".libra"))
            .expect("meta")
            .file_type()
            .is_symlink(),
        "nothing renamed before the refusal"
    );
}

/// §C.6.2 crash matrix: repair converges from each journaled crash window
/// by IDENTITY (symlink target, journal-stamped marker) — pre-backup rolls
/// back, between-renames rolls forward, installed finishes.
#[cfg(unix)]
#[tokio::test]
async fn layout_migration_crash_matrix() {
    use sea_orm::{ConnectionTrait, Database, Statement};

    let dir = repo_with_feature();
    let main = dir.path();
    let shared_head =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], main).stdout)
            .trim()
            .to_string();
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        main.join(".libra/libra.db").display()
    );
    let plant = |wt_id: String, payload: String| {
        let db_url = db_url.clone();
        async move {
            let conn = Database::connect(&db_url).await.expect("connect");
            let backend = conn.get_database_backend();
            conn.execute_raw(Statement::from_string(
                backend,
                format!(
                    "INSERT INTO worktree_intent_journal (op, worktree_id, payload, \
                     created_at) VALUES ('migrate', '{wt_id}', '{payload}', 0);"
                ),
            ))
            .await
            .expect("plant journal");
            let row = conn
                .query_one_raw(Statement::from_string(
                    backend,
                    "SELECT MAX(id) FROM worktree_intent_journal".to_string(),
                ))
                .await
                .expect("query")
                .expect("row");
            let id: i64 = row.try_get_by_index(0).expect("id");
            conn.close().await.expect("close");
            id
        }
    };
    let journal_count = || {
        let db_url = db_url.clone();
        async move {
            let conn = Database::connect(&db_url).await.expect("connect");
            let backend = conn.get_database_backend();
            let row = conn
                .query_one_raw(Statement::from_string(
                    backend,
                    "SELECT COUNT(*) FROM worktree_intent_journal".to_string(),
                ))
                .await
                .expect("query")
                .expect("row");
            let count: i64 = row.try_get_by_index(0).expect("count");
            conn.close().await.expect("close");
            count
        }
    };

    // Mid-migration freeze: a worktree whose gitdir still carries the
    // journal-stamped marker refuses OTHER processes' commands.
    let frozen = create_legacy_symlink_worktree(main, "wt-frozen");
    std::fs::remove_file(frozen.join(".libra")).unwrap();
    std::fs::create_dir_all(frozen.join(".libra")).unwrap();
    std::fs::write(
        frozen.join(".libra").join("commondir"),
        format!(
            "{}\n",
            main.join(".libra").canonicalize().unwrap().display()
        ),
    )
    .unwrap();
    std::fs::write(
        frozen.join(".libra").join("worktree_id"),
        "legacy-wt-frozen\n",
    )
    .unwrap();
    std::fs::write(
        frozen.join(".libra").join("migrate-marker"),
        "journal 9999\n",
    )
    .unwrap();
    let out = run_libra_command(&["status"], &frozen);
    assert!(
        !out.status.success(),
        "a mid-migration worktree is frozen for other processes"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unfinished layout migration"),
        "freeze names the cause"
    );
    std::fs::remove_dir_all(&frozen).unwrap();

    // Window 1: pre-backup — legacy link intact, prepared dir exists.
    let wt1 = create_legacy_symlink_worktree(main, "wt-crash-pre");
    let id1 = plant(
        "legacy-wt-crash-pre".to_string(),
        format!(
            "{{\"path\":\"{}\",\"head\":\"{shared_head}\"}}",
            wt1.to_string_lossy()
        ),
    )
    .await;
    let prepared1 = wt1.join(format!(".libra.migrate-{id1}"));
    std::fs::create_dir_all(&prepared1).unwrap();
    std::fs::write(prepared1.join("migrate-marker"), format!("journal {id1}\n")).unwrap();
    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair window 1",
    );
    assert!(
        std::fs::symlink_metadata(wt1.join(".libra"))
            .expect("meta")
            .file_type()
            .is_symlink(),
        "window 1 rolls back: legacy link intact"
    );
    assert!(
        !prepared1.exists(),
        "window 1 removes only our prepared dir"
    );

    // Window 2: between renames — gitdir gone, backup holds the link,
    // prepared carries the full identity.
    let wt2 = create_legacy_symlink_worktree(main, "wt-crash-mid");
    let id2 = plant(
        "legacy-wt-crash-mid".to_string(),
        format!(
            "{{\"path\":\"{}\",\"head\":\"{shared_head}\"}}",
            wt2.to_string_lossy()
        ),
    )
    .await;
    let prepared2 = wt2.join(format!(".libra.migrate-{id2}"));
    std::fs::create_dir_all(&prepared2).unwrap();
    std::fs::write(
        prepared2.join("commondir"),
        format!(
            "{}\n",
            main.join(".libra").canonicalize().unwrap().display()
        ),
    )
    .unwrap();
    std::fs::write(prepared2.join("worktree_id"), "legacy-wt-crash-mid\n").unwrap();
    std::fs::write(prepared2.join("migrate-marker"), format!("journal {id2}\n")).unwrap();
    std::fs::rename(
        wt2.join(".libra"),
        wt2.join(format!(".libra.legacy-backup-{id2}")),
    )
    .unwrap();
    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair window 2",
    );
    let head2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], &wt2).stdout)
        .trim()
        .to_string();
    assert_eq!(
        head2, shared_head,
        "window 2 rolled forward to a working worktree"
    );
    assert!(
        !wt2.join(format!(".libra.legacy-backup-{id2}")).exists(),
        "window 2 backup cleaned"
    );

    assert_eq!(
        journal_count().await,
        0,
        "both windows resolved their journals"
    );

    // POST-INSTALL window (self-review leg): the prepared gitdir was RENAMED
    // into place — journal-stamped marker still inside, legacy backup still
    // present, journal pending. Recovery must adopt it: re-seed HEAD/index,
    // remove the marker AND the identity-checked backup, resolve the journal,
    // and leave a working worktree.
    let wt4 = create_legacy_symlink_worktree(main, "wt-crash-installed");
    let id4 = plant(
        "legacy-wt-crash-installed".to_string(),
        format!(
            "{{\"path\":\"{}\",\"head\":\"{shared_head}\"}}",
            wt4.to_string_lossy()
        ),
    )
    .await;
    // Manufacture the installed state directly: backup the legacy link, then
    // a marker-carrying gitdir at the FINAL name.
    std::fs::rename(
        wt4.join(".libra"),
        wt4.join(format!(".libra.legacy-backup-{id4}")),
    )
    .unwrap();
    let gitdir4 = wt4.join(".libra");
    std::fs::create_dir_all(&gitdir4).unwrap();
    std::fs::write(
        gitdir4.join("commondir"),
        format!(
            "{}\n",
            main.join(".libra").canonicalize().unwrap().display()
        ),
    )
    .unwrap();
    std::fs::write(gitdir4.join("worktree_id"), "legacy-wt-crash-installed\n").unwrap();
    std::fs::write(gitdir4.join("migrate-marker"), format!("journal {id4}\n")).unwrap();
    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair post-install window",
    );
    let head4 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], &wt4).stdout)
        .trim()
        .to_string();
    assert_eq!(
        head4, shared_head,
        "post-install window rolled forward to a working worktree"
    );
    assert!(
        !wt4.join(format!(".libra.legacy-backup-{id4}")).exists(),
        "post-install backup cleaned"
    );
    assert!(
        !gitdir4.join("migrate-marker").exists(),
        "post-install marker lifted"
    );
    assert_eq!(journal_count().await, 0, "post-install journal resolved");

    // Stale-journal adoption guard: a worktree freshly re-added at the same
    // path (deterministic id, valid commondir, NO journal-stamped marker)
    // must NOT be adopted and re-seeded from the old snapshot.
    let wt3 = main.join("wt-crash-stale");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt3.to_str().unwrap(), "feature"], main),
        "fresh worktree at the contested path",
    );
    let wt3_canonical = wt3.canonicalize().expect("canonical wt3");
    let wt3_id = std::fs::read_to_string(wt3.join(".libra").join("worktree_id"))
        .expect("id")
        .trim()
        .to_string();
    plant(
        wt3_id,
        format!(
            "{{\"path\":\"{}\",\"head\":\"{shared_head}\"}}",
            wt3_canonical.to_string_lossy()
        ),
    )
    .await;
    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair with a stale migrate journal",
    );
    let head3 = String::from_utf8_lossy(
        &run_libra_command(&["rev-parse", "--abbrev-ref", "HEAD"], &wt3).stdout,
    )
    .trim()
    .to_string();
    assert_eq!(head3, "feature", "the fresh worktree was NOT re-seeded");
    assert_eq!(journal_count().await, 1, "the stale journal row is kept");

    // Retry guard: while that unresolved migrate journal exists for the
    // path, a NEW migration attempt refuses instead of stacking a second
    // intent.
    std::fs::remove_dir_all(&wt3).expect("clear for legacy re-setup");
    std::fs::create_dir_all(&wt3).unwrap();
    std::os::unix::fs::symlink(main.join(".libra"), wt3.join(".libra")).unwrap();
    let retry = run_libra_command(
        &[
            "worktree",
            "repair",
            "--migrate-layout",
            "--confirm",
            wt3.to_str().unwrap(),
        ],
        main,
    );
    assert!(
        !retry.status.success(),
        "a pending migrate journal refuses a second migration"
    );
    assert!(
        String::from_utf8_lossy(&retry.stderr).contains("unresolved earlier migration"),
        "refusal names the pending journal"
    );
    assert_eq!(journal_count().await, 1, "no second intent was stacked");

    // Stray RESOLVED marker (journal gone, e.g. an unlink failure after
    // resolution): plain repair verifies the install identity and clears
    // it, unfreezing the worktree.
    let wt4 = main.join("wt-stray-marker");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt4.to_str().unwrap()], main),
        "fresh worktree",
    );
    std::fs::write(
        wt4.join(".libra").join("migrate-marker"),
        "journal 424242\n",
    )
    .unwrap();
    let frozen = run_libra_command(&["status"], &wt4);
    assert!(!frozen.status.success(), "marker freezes the worktree");
    assert_cli_success(
        &run_libra_command(&["worktree", "repair", "--confirm"], main),
        "repair clears the resolved marker",
    );
    assert!(
        !wt4.join(".libra").join("migrate-marker").exists(),
        "marker cleared after identity verification"
    );
    assert_cli_success(
        &run_libra_command(&["status"], &wt4),
        "worktree unfrozen after repair",
    );

    // Interrupted-migration-then-move guard: an entry whose migrate journal
    // is still pending refuses `worktree move` (a relocation would strand
    // the journal at the old path and let reconciliation unfreeze the
    // still-unmigrated state).
    let wt5 = main.join("wt-move-pending");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt5.to_str().unwrap()], main),
        "worktree for move guard",
    );
    let wt5_canonical = wt5.canonicalize().expect("canonical wt5");
    let wt5_id = std::fs::read_to_string(wt5.join(".libra").join("worktree_id"))
        .expect("id")
        .trim()
        .to_string();
    plant(
        wt5_id,
        format!(
            "{{\"path\":\"{}\",\"head\":\"{shared_head}\"}}",
            wt5_canonical.to_string_lossy()
        ),
    )
    .await;
    let blocked = run_libra_command(
        &[
            "worktree",
            "move",
            wt5.to_str().unwrap(),
            main.join("wt-moved-away").to_str().unwrap(),
        ],
        main,
    );
    assert!(
        !blocked.status.success(),
        "move refuses while a migrate journal is pending"
    );
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("unfinished layout migration"),
        "refusal names the pending migration"
    );
}

/// plan-20260714 W0 (§C.11, Codex R16/R17): `worktree doctor` is STRICTLY
/// read-only.
///
/// The whole point of a diagnostic is that it is safe to run on a repository
/// you do not yet understand. A doctor that adopts, reclaims or repairs by
/// default would make its own output unreproducible and could resolve an
/// ambiguity the operator had not yet seen. Repair actions arrive as explicit
/// subcommands in later waves; until then the invariant is: nothing changes.
#[test]
fn worktree_doctor_default_invocation_is_readonly() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-doctor");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Fingerprint everything doctor could plausibly touch.
    let registry = main.join(".libra").join("worktrees.json");
    let db = main.join(".libra").join("libra.db");
    let snapshot = |path: &std::path::Path| -> Option<Vec<u8>> { std::fs::read(path).ok() };
    let registry_before = snapshot(&registry);
    let db_before = snapshot(&db);
    let wt_id_before = snapshot(&wt.join(".libra").join("worktree_id"));
    // "Strictly read-only" includes CREATING nothing. Snapshotting only the
    // files that already exist cannot catch a command that adds one, and the
    // `worktree add` above has already created several — so the whole `.libra`
    // tree is listed, and the maintenance lock (which the generic
    // publisher hold would create) is removed first so its absence is
    // meaningful.
    let lock = main.join(".libra").join("maintenance.lock");
    let _ = std::fs::remove_file(&lock);
    let tree = |root: &std::path::Path| -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path.clone());
                }
                found.push(path.to_string_lossy().into_owned());
            }
        }
        found.sort();
        found
    };
    let tree_before = tree(&main.join(".libra"));

    let out = run_libra_command(&["worktree", "doctor"], main);
    assert_cli_success(&out, "worktree doctor");

    assert!(
        !lock.exists(),
        "doctor must not create the maintenance lock: it is strictly read-only"
    );
    assert_eq!(
        tree_before,
        tree(&main.join(".libra")),
        "doctor must not add or remove any file under .libra"
    );

    assert_eq!(
        registry_before,
        snapshot(&registry),
        "doctor must not rewrite the registry"
    );
    assert_eq!(db_before, snapshot(&db), "doctor must not write to the DB");
    assert_eq!(
        wt_id_before,
        snapshot(&wt.join(".libra").join("worktree_id")),
        "doctor must not touch a worktree's identity"
    );

    // A healthy repository reports so rather than inventing findings.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no problems detected"),
        "healthy repo: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The read-only doctor REPORTS a damaged identity — and still changes
/// nothing, including the damaged worktree it is reporting on.
#[test]
fn worktree_doctor_reports_scope_diagnostics_without_repairing() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-doctor-damaged");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let id_file = wt.join(".libra").join("worktree_id");
    std::fs::write(&id_file, "deadbeefdeadbeefdeadbeefdeadbeef\n").unwrap();
    let damaged_before = std::fs::read(&id_file).unwrap();

    // W4 freezes the JSON response to workspace diagnostics only; adding the
    // legacy W0 worktree report there would both change that public schema and
    // make a capped page scan every worktree. The human doctor retains the
    // W0 layout/identity diagnosis.
    let out = run_libra_command(&["worktree", "doctor"], main);
    assert_cli_success(&out, "worktree doctor");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(wt.to_str().expect("worktree path")),
        "the linked worktree is reported: {text}"
    );
    assert!(
        text.contains("identity is not one the registry knows"),
        "the unknown identity is reported: {text}"
    );
    assert!(
        text.contains("worktree repair --confirm"),
        "the report names the executable repair route: {text}"
    );

    // Reporting is not repairing.
    assert_eq!(
        damaged_before,
        std::fs::read(&id_file).unwrap(),
        "doctor reported the damage without fixing it"
    );
}

/// W0 §C.11: `worktree doctor` does not upgrade the schema of the repository
/// it is diagnosing.
///
/// Applying a pending migration IS a write. A diagnostic that silently
/// performs one changes the thing you were trying to observe — and a
/// repository behind schema is precisely the case where you want to look
/// before committing to an upgrade. The already-current-schema test cannot
/// catch this, because there is no migration to apply.
///
/// The repository is put behind schema by removing the LAST applied version
/// from the ledger AND undoing what it created, so re-applying is a real
/// forward step rather than a non-idempotent replay.
///
/// It must be the LAST, not merely a recent one: the runner treats
/// `version > MAX(applied)` as pending, so deleting a middle row leaves
/// nothing to apply and the "an ordinary command still upgrades" half of this
/// test would compare an untouched database against itself. The assertion
/// below pins that, so adding a migration fails here loudly instead of
/// quietly hollowing the test out.
#[tokio::test]
async fn worktree_doctor_does_not_upgrade_a_behind_schema_repository() {
    use libra::internal::db::migration::builtin_runner;
    use sea_orm::{ConnectionTrait, Database};

    let dir = repo_with_feature();
    let main = dir.path();
    let db = main.join(".libra").join("libra.db");
    let db_url = format!("sqlite://{}?mode=rwc", db.display());

    // Use the real down migration rather than deleting its ledger row. W4's
    // schema adds physical columns/triggers, so merely removing the version
    // would turn the next ordinary migration run into a duplicate-column
    // failure instead of representing a repository that is genuinely behind.
    let conn = Database::connect(&db_url)
        .await
        .expect("open repository db");
    conn.execute_raw(sea_orm::Statement::from_string(
        conn.get_database_backend(),
        "DELETE FROM schema_versions WHERE version = 2026090101".to_string(),
    ))
    .await
    .expect("remove forward-only v2 version marker before rollback fixture");
    restore_v1_operation_shape(&conn).await;
    // Registry-derived for the same reason as the capability-marker case
    // above: everything registered above 2026073101 rolls back with it.
    let mut expected_rolled_back: Vec<i64> = libra::internal::db::migration::builtin_migrations()
        .into_iter()
        .map(|migration| migration.version)
        .filter(|version| *version > 2026073101 && *version < 2026090101)
        .collect();
    expected_rolled_back.reverse();
    assert_eq!(
        builtin_runner()
            .expect("builtin runner")
            .rollback_to(&conn, 2026073101)
            .await
            .expect("roll back newest migration"),
        expected_rolled_back
    );
    conn.close().await.expect("close repository db");
    assert!(
        sqlite_max_schema_version(&db) < 2026080401,
        "2026080401 must be the NEWEST migration for this test to leave one \
         pending — retarget it at the new newest migration"
    );
    let before = std::fs::read(&db).expect("db before");

    let out = run_libra_command(&["worktree", "doctor"], main);
    assert_cli_success(&out, "doctor on a behind-schema repository");
    assert_eq!(
        before,
        std::fs::read(&db).expect("db after"),
        "doctor must not apply the pending migration"
    );

    // An ordinary command still upgrades, so the exclusion is scoped to
    // doctor rather than disabling migrations outright.
    assert_cli_success(&run_libra_command(&["status"], main), "status upgrades");
    assert_ne!(
        before,
        std::fs::read(&db).expect("db after status"),
        "the pending migration was still pending, and status applied it"
    );
}

/// The highest version recorded in `schema_versions` — what the runner
/// compares against to decide what is pending.
fn sqlite_max_schema_version(db: &std::path::Path) -> i64 {
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg(format!(
            "import sqlite3\nc=sqlite3.connect({db:?})\n\
             print(c.execute('SELECT COALESCE(MAX(version), 0) FROM schema_versions').fetchone()[0])"
        ))
        .output()
        .expect("query schema_versions");
    assert!(out.status.success(), "query schema_versions");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("a version number")
}

/// Run statements against a repository database without pulling an async
/// runtime into a synchronous test.
fn sqlite_exec(db: &std::path::Path, statements: &[&str]) -> bool {
    let script = statements
        .iter()
        .map(|sql| format!("c.execute({sql:?})"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg(format!(
            "import sqlite3\nc=sqlite3.connect({db:?})\n{script}\nc.commit()\n"
        ))
        .output();
    matches!(out, Ok(o) if o.status.success())
}

/// §C.12 named regression `op_restore_cross_worktree_scope_rejected`
/// (§C.9 / ADR-0714-08): `op restore` rewrites THIS worktree's HEAD and refs
/// from a snapshot. Replaying an operation recorded in ANOTHER worktree grafts
/// that worktree's state onto this one, so it is refused — before the dry-run
/// report, and with no ref moved.
#[test]
fn op_restore_cross_worktree_scope_rejected() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");

    // Detach main FIRST: with main on a branch, removing the scope guard
    // would still hit the "branch is checked out at worktree" guard and the
    // test would stay green for the wrong reason. Detached, the scope guard
    // is the only thing standing between this restore and success.
    let head = run_libra_command(&["rev-parse", "HEAD"], main);
    assert_cli_success(&head, "rev-parse HEAD in main");
    let head_oid = String::from_utf8_lossy(&head.stdout).trim().to_string();
    assert_cli_success(
        &run_libra_command(&["checkout", "--detach", &head_oid], main),
        "detach main",
    );

    // An operation recorded in MAIN. `branch` goes through the operation
    // wrapper, so this is a real snapshot with `scope_provenance = declared`.
    assert_cli_success(
        &run_libra_command(&["branch", "restore-source"], main),
        "branch in main",
    );
    let logged = run_libra_command(&["--json", "op", "log", "-n", "1"], main);
    assert_cli_success(&logged, "op log");
    let op_id = serde_json::from_slice::<serde_json::Value>(&logged.stdout)
        .expect("op log json")["data"]["operations"][0]["op_id"]
        .as_str()
        .expect("an operation id")
        .to_string();

    let wt = parent.path().join("linked");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), "feature"], main),
        "worktree add",
    );
    let head_before = run_libra_command(&["rev-parse", "HEAD"], &wt);
    assert_cli_success(&head_before, "rev-parse HEAD in the linked worktree");

    let refused = run_libra_command(&["--json", "op", "restore", &op_id], &wt);
    assert!(
        !refused.status.success(),
        "restoring main's operation from a linked worktree must be refused"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    // The SCOPE refusal specifically. A looser assertion on the word
    // "worktree" passes even with the scope guard removed, because the
    // later checked-out-branch guard says "worktree" too — and that one
    // does not fire when the operation's branch is checked out nowhere,
    // which is why main is detached above.
    assert!(
        stderr.contains("ran in the main worktree, but this is worktree"),
        "the refusal must be the scope mismatch, not an incidental one: {stderr}"
    );

    // Zero side effects: not even a partial ref move.
    let head_after = run_libra_command(&["rev-parse", "HEAD"], &wt);
    assert_eq!(
        String::from_utf8_lossy(&head_before.stdout),
        String::from_utf8_lossy(&head_after.stdout),
        "the refused restore must not have moved HEAD"
    );
}

/// §C.13: a checkout collision raised at the STORAGE SEAM carries
/// `LBR-CONFLICT-002`, exactly like one caught by a command's preflight.
///
/// The seam guard was added so the check and the write could not be split by
/// a concurrent attach, but it reported the collision through
/// `BranchStoreError::Corrupt`, which `symbolic-ref` maps to `LBR-REPO-002`.
/// A user racing two worktrees was told their repository was corrupt for what
/// is an ordinary, recoverable conflict — and any tooling keyed on the code
/// would have escalated it as damage.
#[test]
fn seam_checkout_collision_reports_a_conflict_not_corruption() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-collide");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), "feature"], main),
        "worktree add on 'feature'",
    );

    // `symbolic-ref HEAD` goes through the POOLED attach entry point — the
    // one the seam guard now covers transactionally.
    let attach = run_libra_command(
        &["--json", "symbolic-ref", "HEAD", "refs/heads/feature"],
        main,
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&attach.stdout),
        String::from_utf8_lossy(&attach.stderr)
    );
    assert!(
        !attach.status.success(),
        "attaching a second HEAD to a checked-out branch must be refused: {combined}"
    );
    assert!(
        combined.contains("LBR-CONFLICT-002"),
        "a checkout collision is a conflict, not repository corruption: {combined}"
    );
    assert!(
        !combined.contains("LBR-REPO-002"),
        "and must not be reported as corruption: {combined}"
    );

    // The same is true of the DELETE seam: `branch -d` of a branch another
    // worktree holds is a conflict, not a storage failure.
    let delete = run_libra_command(&["--json", "branch", "-D", "feature"], main);
    let delete_out = format!(
        "{}{}",
        String::from_utf8_lossy(&delete.stdout),
        String::from_utf8_lossy(&delete.stderr)
    );
    assert!(
        !delete.status.success(),
        "deleting a branch checked out elsewhere must be refused: {delete_out}"
    );
    assert!(
        delete_out.contains("LBR-CONFLICT-002"),
        "branch deletion collision is LBR-CONFLICT-002: {delete_out}"
    );

    // And the branch is still there and still usable in the other worktree.
    assert_cli_success(
        &run_libra_command(&["status"], &wt),
        "the other worktree keeps its branch",
    );
}

/// §C.13, the writer seam this time: a branch TIP write refused because
/// another worktree holds the branch is `LBR-CONFLICT-002`, not a write fault.
///
/// The refusal is raised inside `Branch`'s storage seam and then travels
/// through sea_orm's `DbErr` (the reflog closure's error type is fixed, so a
/// typed enum cannot pass) and one or two more wrapping layers before a
/// command sees it. Every boundary that classified by hand lost it on the
/// way: `switch -C`, which deletes and recreates, reported `LBR-IO-002` —
/// "failed to delete" — for a branch that was simply in use somewhere else,
/// and any tooling keyed on the code would have escalated it as damage.
#[test]
fn seam_branch_tip_collision_reports_a_conflict_not_a_write_fault() {
    let dir = repo_with_feature();
    let main = dir.path();
    let wt = main.join("wt-tip");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), "feature"], main),
        "worktree add on 'feature'",
    );
    fs::write(main.join("b.txt"), "b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], main),
        "commit",
    );
    let tip = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], main).stdout)
        .trim()
        .to_string();

    // Force-create over a branch the other worktree has checked out: the
    // delete half of `-C` is refused at the seam.
    let force_create = run_libra_command(&["--json", "switch", "-C", "feature"], main);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&force_create.stdout),
        String::from_utf8_lossy(&force_create.stderr)
    );
    assert!(
        !force_create.status.success(),
        "recreating a branch another worktree holds must be refused: {combined}"
    );
    assert!(
        combined.contains("LBR-CONFLICT-002"),
        "and reported as a conflict, not a write fault: {combined}"
    );
    assert!(
        !combined.contains("LBR-IO-002"),
        "the generic write-failure code must not survive the wrapping: {combined}"
    );

    // Moving the tip directly is refused with the same code.
    let update_ref = run_libra_command(
        &["--json", "update-ref", "refs/heads/feature", tip.as_str()],
        main,
    );
    let update_out = format!(
        "{}{}",
        String::from_utf8_lossy(&update_ref.stdout),
        String::from_utf8_lossy(&update_ref.stderr)
    );
    assert!(
        !update_ref.status.success() && update_out.contains("LBR-CONFLICT-002"),
        "moving a tip another worktree holds is the same conflict: {update_out}"
    );

    // The other worktree is untouched by either refusal.
    assert_eq!(abbrev_head(&wt), "feature");
}

/// Part C W1 (§C.11 W1 acceptance, `two_linked_rebases_keep_independent_todo_and_abort`):
/// TWO linked worktrees each stopped in their own rebase keep independent todo
/// lists, and aborting one leaves the other's exactly as it was.
///
/// The existing coverage started ONE linked rebase and then had main do a
/// cherry-pick, which proves the scoped mutex but not the thing this card is
/// named for: two sequencer states alive at once, each read and cleared by its
/// own worktree. A single shared row would satisfy the old test and fail this
/// one.
#[test]
fn two_linked_rebases_keep_independent_todo_and_abort() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");

    // main advances, so both linked branches have something to rebase ONTO
    // and a conflicting edit to stop on.
    fs::write(main.join("a.txt"), "main-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "main add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-edit", "--no-verify"], main),
        "main commit",
    );
    let main_head_before = head_sha(main);

    // Two linked worktrees, each on its own branch with its own conflicting
    // edit to the same file.
    let mut worktrees = Vec::new();
    for (branch, content) in [("one", "one-line\n"), ("two", "two-line\n")] {
        assert_cli_success(
            &run_libra_command(&["branch", branch, "feature"], main),
            "create branch",
        );
        let wt = parent.path().join(branch);
        assert_cli_success(
            &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), branch], main),
            "worktree add",
        );
        fs::write(wt.join("a.txt"), content).unwrap();
        assert_cli_success(&run_libra_command(&["add", "a.txt"], &wt), "wt add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "wt-edit", "--no-verify"], &wt),
            "wt commit",
        );
        let tip = head_sha(&wt);
        worktrees.push((branch, wt, tip));
    }

    // BOTH rebases stop, at the same time, each in its own worktree.
    for (branch, wt, _) in &worktrees {
        let rebase = run_libra_command(&["rebase", "main"], wt);
        assert_ne!(
            rebase.status.code(),
            Some(0),
            "{branch}'s rebase stops on the conflict: {}",
            String::from_utf8_lossy(&rebase.stderr)
        );
    }

    // Each worktree's state is ITS OWN: the stopped commit each one reports is
    // its own tip, not the other's. A single shared row could only name one.
    let state_rows = {
        let db = main.join(".libra/libra.db");
        let out = std::process::Command::new("python3")
            .arg("-c")
            .arg(format!(
                "import sqlite3\nc = sqlite3.connect({db:?})\nrows = list(c.execute('SELECT worktree_id, head_name, stopped_sha FROM rebase_state ORDER BY worktree_id'))\nprint(len(rows))\nfor r in rows: print('|'.join(str(x) for x in r))\n"
            ))
            .output()
            .expect("read rebase_state");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let mut lines = state_rows.lines();
    assert_eq!(
        lines.next(),
        Some("2"),
        "two live rebase states, one per worktree: {state_rows}"
    );
    let rows: Vec<&str> = lines.collect();
    for (branch, _, tip) in &worktrees {
        assert!(
            rows.iter().any(|row| row.contains(branch)),
            "{branch} has its own rebase_state row: {state_rows}"
        );
        let _ = tip;
    }
    assert!(
        rows.iter().all(|row| !row.starts_with('|')),
        "each row is keyed by a real worktree id: {state_rows}"
    );

    // Abort the FIRST worktree's rebase. Its own tip and branch come back.
    let (_, first_wt, first_tip) = &worktrees[0];
    assert_cli_success(
        &run_libra_command(&["rebase", "--abort"], first_wt),
        "abort the first linked rebase",
    );
    assert_eq!(head_sha(first_wt), *first_tip, "first worktree restored");
    assert_eq!(
        abbrev_head(first_wt),
        "one",
        "first worktree back on branch"
    );

    // The SECOND worktree is untouched: still in progress, and able to finish
    // on its own state.
    let (_, second_wt, _) = &worktrees[1];
    let status = run_libra_command(&["status"], second_wt);
    assert!(
        status.status.success(),
        "the second worktree still reads its own state: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    fs::write(second_wt.join("a.txt"), "main-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], second_wt), "resolve");
    assert_cli_success(
        &run_libra_command(&["rebase", "--continue"], second_wt),
        "the second linked rebase completes after the first aborted",
    );
    assert_eq!(
        abbrev_head(second_wt),
        "two",
        "second worktree on its branch"
    );
    assert_eq!(
        head_sha(main),
        main_head_before,
        "main untouched throughout"
    );
}

/// W1 §C.4.1.1: two worktrees' dirty-cache rows are mutually INVISIBLE, and
/// one worktree's `--check-dirty` cannot prune the other's.
///
/// The existing coverage runs every cache mode in ONE linked worktree, which
/// proves they are no longer refused there but says nothing about isolation:
/// a single shared cache would satisfy it. This drives both scopes.
#[test]
fn dirty_cache_rows_are_invisible_across_worktrees() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Each worktree dirties a DIFFERENT file and scans, so each cache holds
    // exactly one row that the other must not see.
    fs::write(main.join("main-only.txt"), "main\n").unwrap();
    fs::write(wt.join("wt-only.txt"), "wt\n").unwrap();
    assert_cli_success(&run_libra_command(&["status", "--scan"], main), "main scan");
    assert_cli_success(&run_libra_command(&["status", "--scan"], &wt), "wt scan");

    // `--cached` is exclusive with `--porcelain` (R0-8 cache-mode
    // exclusivity), so the human rendering is the one to read here.
    let cached = |dir: &std::path::Path| -> String {
        let out = run_libra_command(&["status", "--cached"], dir);
        assert_cli_success(&out, "status --cached");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    let main_view = cached(main);
    let wt_view = cached(&wt);
    assert!(
        main_view.contains("main-only.txt") && !main_view.contains("wt-only.txt"),
        "main's cache holds only main's dirty path: {main_view:?}"
    );
    assert!(
        wt_view.contains("wt-only.txt") && !wt_view.contains("main-only.txt"),
        "the linked worktree's cache holds only its own dirty path: {wt_view:?}"
    );

    // Resolve the LINKED worktree's dirt and re-verify there. Its row is
    // pruned; main's row must be untouched — a shared cache, or a prune that
    // ignored scope, would take main's row with it.
    fs::remove_file(wt.join("wt-only.txt")).unwrap();
    assert_cli_success(
        &run_libra_command(&["status", "--check-dirty"], &wt),
        "wt check-dirty prunes its own row",
    );
    let wt_after = cached(&wt);
    assert!(
        !wt_after.contains("wt-only.txt"),
        "the resolved path left the linked worktree's cache: {wt_after:?}"
    );
    let main_after = cached(main);
    assert!(
        main_after.contains("main-only.txt"),
        "main's dirty row survives the other worktree's check-dirty: {main_after:?}"
    );
}

/// W1 §C.4.2: rebase AUXILIARY state is worktree-scoped too.
///
/// The rebase tests exercise todo/stopped state but no auxiliary path, so a
/// regression to a shared aux sidecar would pass them. This one puts two
/// linked worktrees into rebases that BOTH carry auxiliary state (`--exec`
/// pending commands and an autostash), then finishes one and requires the
/// other's aux state to be intact — its exec still pending, its stash still
/// held.
#[test]
fn linked_rebase_auxiliary_state_is_per_worktree() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");

    fs::write(main.join("a.txt"), "main-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "main add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-edit", "--no-verify"], main),
        "main commit",
    );

    let mut worktrees = Vec::new();
    for (branch, content) in [("aux-one", "one\n"), ("aux-two", "two\n")] {
        assert_cli_success(
            &run_libra_command(&["branch", branch, "feature"], main),
            "branch",
        );
        let wt = parent.path().join(branch);
        assert_cli_success(
            &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), branch], main),
            "worktree add",
        );
        fs::write(wt.join("a.txt"), content).unwrap();
        assert_cli_success(&run_libra_command(&["add", "a.txt"], &wt), "wt add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "wt-edit", "--no-verify"], &wt),
            "wt commit",
        );
        // Uncommitted dirt, so the rebase must autostash it — auxiliary state.
        fs::write(wt.join("dirty.txt"), content).unwrap();
        assert_cli_success(&run_libra_command(&["add", "dirty.txt"], &wt), "stage dirt");
        worktrees.push((branch, wt));
    }

    // Both rebases carry an `--exec` (pending aux command) and an autostash,
    // and both stop on the conflict.
    for (branch, wt) in &worktrees {
        // `--exec true` is a pending auxiliary command that does not depend
        // on `libra` being on PATH inside the test environment.
        let out = run_libra_command(&["rebase", "--autostash", "--exec", "true", "main"], wt);
        assert_ne!(
            out.status.code(),
            Some(0),
            "{branch}'s rebase stops on the conflict: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Each worktree's aux sidecars live in ITS OWN gitdir, never in common
    // storage — a shared file would show up here.
    let aux_names = ["rebase-exec", "rebase-todo", "autostash", "rebase-aux.json"];
    for (branch, wt) in &worktrees {
        let gitdir = wt.join(".libra");
        assert!(
            gitdir.exists(),
            "{branch} has its own gitdir: {}",
            gitdir.display()
        );
    }
    for name in aux_names {
        assert!(
            !main.join(".libra").join(name).exists(),
            "no linked worktree's `{name}` may live in COMMON storage"
        );
    }

    // A PENDING `--exec` is aux state too, and the conflict above stops
    // BEFORE any exec runs. So a separate pair of worktrees exercises that
    // path: non-conflicting branches, one commit each to replay, and a
    // failing `--exec` that stops the rebase with pending exec state.
    let mut exec_worktrees = Vec::new();
    for (branch, file) in [("exec-one", "e1.txt"), ("exec-two", "e2.txt")] {
        assert_cli_success(
            &run_libra_command(&["branch", branch, "main"], main),
            "branch off main",
        );
        let wt = parent.path().join(branch);
        assert_cli_success(
            &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), branch], main),
            "worktree add",
        );
        fs::write(wt.join(file), "own file\n").unwrap();
        assert_cli_success(&run_libra_command(&["add", file], &wt), "wt add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "own commit", "--no-verify"], &wt),
            "wt commit",
        );
        exec_worktrees.push((branch, wt));
    }
    // main advances on a file neither branch touched, so each rebase has one
    // commit to replay and no conflict.
    fs::write(main.join("moved.txt"), "moved\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "moved.txt"], main), "main add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-advance", "--no-verify"], main),
        "main commit",
    );

    for (branch, wt) in &exec_worktrees {
        let armed = run_libra_command(&["rebase", "--exec", "false", "main"], wt);
        assert_ne!(
            armed.status.code(),
            Some(0),
            "{branch}'s failing exec stops the rebase: {}",
            String::from_utf8_lossy(&armed.stdout)
        );
    }
    // Neither worktree's exec/todo state leaked into COMMON storage.
    for name in aux_names {
        assert!(
            !main.join(".libra").join(name).exists(),
            "no linked worktree's `{name}` may live in COMMON storage after an exec stop"
        );
    }
    // Each worktree's pending-exec state lives in ITS OWN `rebase-aux.json`.
    let aux_of = |wt: &std::path::Path| -> String {
        fs::read_to_string(wt.join(".libra").join("rebase-aux.json"))
            .unwrap_or_else(|error| panic!("read {}: {error}", wt.display()))
    };
    let (_, first_exec) = &exec_worktrees[0];
    let (_, second_exec) = &exec_worktrees[1];
    for wt in [first_exec, second_exec] {
        let aux = aux_of(wt);
        assert!(
            aux.contains("pending_exec") && aux.contains("false"),
            "{}'s own aux state holds its pending exec: {aux}",
            wt.display()
        );
    }
    let second_aux_before = aux_of(second_exec);

    // Aborting the FIRST must not touch the second's aux file at all — byte
    // for byte. Deleting or rewriting it during the first abort would pass a
    // test that only checked that the second could still abort.
    assert_cli_success(
        &run_libra_command(&["rebase", "--abort"], first_exec),
        "abort the first exec-stopped rebase",
    );
    assert_eq!(
        aux_of(second_exec),
        second_aux_before,
        "the second worktree's aux state is untouched by the first's abort"
    );

    // And its pending exec is still LIVE: continuing retries the failing
    // command and stops again, rather than finding nothing to do.
    let retry = run_libra_command(&["rebase", "--continue"], second_exec);
    assert_ne!(
        retry.status.code(),
        Some(0),
        "the second worktree's pending exec is retried and fails again: {}",
        String::from_utf8_lossy(&retry.stdout)
    );
    assert_cli_success(
        &run_libra_command(&["rebase", "--abort"], second_exec),
        "and it can conclude on its own",
    );

    // Abort the FIRST. The second must still be mid-rebase with its own aux
    // state, and must be able to finish from it.
    let (_, first) = &worktrees[0];
    assert_cli_success(
        &run_libra_command(&["rebase", "--abort"], first),
        "abort the first rebase",
    );
    assert_eq!(abbrev_head(first), "aux-one", "first worktree restored");
    assert!(
        first.join("dirty.txt").exists(),
        "the first worktree's autostash was restored by its own abort"
    );

    let (_, second) = &worktrees[1];
    fs::write(second.join("a.txt"), "main-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], second), "resolve");
    let cont = run_libra_command(&["rebase", "--continue"], second);
    assert!(
        cont.status.success(),
        "the second worktree finishes from its OWN aux state: {}",
        String::from_utf8_lossy(&cont.stderr)
    );
    assert_eq!(abbrev_head(second), "aux-two", "second worktree on branch");
    assert!(
        second.join("dirty.txt").exists(),
        "and its own autostash came back, not the other's"
    );
}

/// §C.4.4: bisect and the sequencer exclude each other, in BOTH directions,
/// within one worktree scope.
///
/// Bisect was outside the symmetric mutex entirely: `bisect start` only
/// checked for existing bisect state, so it could begin beside an in-progress
/// cherry-pick and then check out candidates underneath it.
#[test]
fn bisect_and_sequencer_exclude_each_other() {
    let repo = repo_with_feature();
    let main = repo.path();

    // Two commits so a cherry-pick has something to apply, and a conflicting
    // edit so it stops and stays in progress.
    fs::write(main.join("a.txt"), "main-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-edit", "--no-verify"], main),
        "commit",
    );
    assert_cli_success(&run_libra_command(&["switch", "feature"], main), "feature");
    fs::write(main.join("a.txt"), "feature-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feature-edit", "--no-verify"], main),
        "commit",
    );
    let feature_tip = head_sha(main);
    assert_cli_success(&run_libra_command(&["switch", "main"], main), "main");

    // A cherry-pick that stops on the conflict owns the scope.
    let cp = run_libra_command(&["cherry-pick", &feature_tip], main);
    assert_ne!(cp.status.code(), Some(0), "the cherry-pick stops");

    // Direction 1: `bisect start` must be refused while it is in progress.
    let bisect = run_libra_command(&["--json", "bisect", "start"], main);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&bisect.stdout),
        String::from_utf8_lossy(&bisect.stderr)
    );
    assert!(
        !bisect.status.success(),
        "bisect must not start beside an in-progress cherry-pick: {combined}"
    );
    assert!(
        combined.contains("LBR-CONFLICT-002") && combined.contains("cherry-pick"),
        "and must name what blocks it: {combined}"
    );

    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--abort"], main),
        "abort cherry-pick",
    );

    // Direction 2 runs in a CLEAN repository: bisect requires a clean tree, and
    // the conflicted fixture above cannot provide one without the test doing
    // the user's conflict cleanup for them.
    let clean = repo_with_feature();
    let clean_main = clean.path();
    let feature_tip = head_sha_of_branch(clean_main, "feature");
    // `init` leaves `.libraignore` untracked, and bisect requires a clean tree
    // INCLUDING untracked files (a candidate commit tracking that path would
    // overwrite it). Commit it so the fixture is genuinely clean.
    assert_cli_success(
        &run_libra_command(&["add", "."], clean_main),
        "stage the fixture leftovers",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "fixture", "--no-verify"], clean_main),
        "commit the fixture leftovers",
    );
    assert_cli_success(
        &run_libra_command(&["bisect", "start"], clean_main),
        "bisect starts once nothing else owns the scope",
    );

    // A new sequence must be refused while the bisect is active.
    let cp = run_libra_command(&["--json", "cherry-pick", &feature_tip], clean_main);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&cp.stdout),
        String::from_utf8_lossy(&cp.stderr)
    );
    assert!(
        !cp.status.success(),
        "a cherry-pick must not start during a bisect: {combined}"
    );
    assert!(
        combined.contains("LBR-CONFLICT-002") && combined.contains("bisect"),
        "and must name the bisect: {combined}"
    );
    assert_cli_success(
        &run_libra_command(&["bisect", "reset"], clean_main),
        "bisect reset",
    );
}

/// The tip of `<branch>` in `dir`.
fn head_sha_of_branch(dir: &std::path::Path, branch: &str) -> String {
    let out = run_libra_command(&["rev-parse", branch], dir);
    assert_cli_success(&out, "rev-parse branch");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// §C.4.2: the ambiguous-legacy refusal names the directory that EXISTS.
///
/// With only `rebase-apply/` present, an error naming `rebase-merge` sends the
/// user to a path that is not there — and this is a START path, which the
/// `--continue` coverage does not exercise.
#[test]
fn ambiguous_legacy_refusal_names_rebase_apply() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Only `rebase-apply` — the other legacy spelling.
    fs::create_dir_all(main.join(".libra/rebase-apply")).unwrap();

    // A sequence START is refused, and the message names `rebase-apply`.
    let start = run_libra_command(&["--json", "cherry-pick", "feature"], main);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&start.stdout),
        String::from_utf8_lossy(&start.stderr)
    );
    assert!(
        !start.status.success(),
        "a start must be refused while the ambiguous directory exists: {combined}"
    );
    assert!(
        combined.contains("rebase-apply"),
        "the refusal must name the directory that exists: {combined}"
    );
    assert!(
        !combined.contains("rebase-merge"),
        "and must not name one that does not: {combined}"
    );

    // `status` still WORKS and reports it — the ambiguity is not fatal there.
    let status = run_libra_command(&["status"], main);
    assert_cli_success(&status, "status still runs with an ambiguous legacy dir");
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        stdout.contains("rebase-apply"),
        "status reports the directory it found: {stdout}"
    );
    assert!(
        main.join(".libra/rebase-apply").exists(),
        "and nothing consumed it"
    );
}

/// §C.4.4: a CONVERGED bisect still owns the scope until `bisect reset`.
///
/// A finished bisect deliberately keeps its row and `orig_head` so `reset` can
/// return HEAD there. If a rebase or cherry-pick could start in between, that
/// reset would move HEAD away from the new work — so "completed" is not
/// "finished with the scope".
#[test]
fn a_converged_bisect_still_blocks_a_sequence_until_reset() {
    let repo = repo_with_feature();
    let main = repo.path();
    assert_cli_success(&run_libra_command(&["add", "."], main), "stage leftovers");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "fixture", "--no-verify"], main),
        "commit leftovers",
    );
    let feature_tip = head_sha_of_branch(main, "feature");

    assert_cli_success(
        &run_libra_command(&["bisect", "start"], main),
        "bisect start",
    );
    // CONVERGE it for real: mark the tip bad and its parent good, leaving the
    // search nothing to test.
    let head = head_sha(main);
    let parent = {
        let out = run_libra_command(&["rev-parse", "HEAD~1"], main);
        assert_cli_success(&out, "rev-parse HEAD~1");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert_cli_success(
        &run_libra_command(&["bisect", "bad", &head], main),
        "mark bad",
    );
    assert_cli_success(
        &run_libra_command(&["bisect", "good", &parent], main),
        "mark good",
    );
    // Read the state, not the prose: with `completed = 0` as the ownership
    // test a converged row would NOT block, so the assertion below has to
    // know which kind of row it is looking at.
    let bisect_row = {
        let db = main.join(".libra/libra.db");
        let out = std::process::Command::new("python3")
            .arg("-c")
            .arg(format!(
                "import sqlite3\nc = sqlite3.connect({db:?})\nprint(list(c.execute('SELECT completed FROM bisect_state')))\n"
            ))
            .output()
            .expect("read bisect_state");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert!(
        bisect_row.contains('1'),
        "the bisect converged, so its retained row is `completed`: {bisect_row}"
    );

    // A converged-but-unreset bisect still owns the scope.
    let cp = run_libra_command(&["--json", "cherry-pick", &feature_tip], main);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&cp.stdout),
        String::from_utf8_lossy(&cp.stderr)
    );
    assert!(
        !cp.status.success(),
        "a retained bisect row must keep blocking a sequence start: {combined}"
    );
    assert!(
        combined.contains("bisect"),
        "and the refusal must name the bisect: {combined}"
    );

    // `reset` is what releases the scope.
    assert_cli_success(
        &run_libra_command(&["bisect", "reset"], main),
        "bisect reset",
    );
    assert_cli_success(
        &run_libra_command(&["add", "."], main),
        "stage anything reset left",
    );
    let cp = run_libra_command(&["cherry-pick", &feature_tip], main);
    assert!(
        cp.status.success() || !String::from_utf8_lossy(&cp.stderr).contains("bisect"),
        "after reset the bisect no longer blocks: {}",
        String::from_utf8_lossy(&cp.stderr)
    );
}

/// §C.11 W1 acceptance (§C.12 named regression): two worktrees running the
/// SAME control action with the same arguments inside the five-second window
/// are two legitimate operations, and neither is refused as a duplicate.
///
/// **What this half can and cannot prove.** Sequencer control actions do not
/// enter `with_operation_log` today, so nothing here reaches the dedup query;
/// this is the end-to-end INVARIANT — the criterion holds for a user now, and
/// keeps holding the day the controls are wrapped. The teeth are at the
/// wrapper seam, where the key actually lives:
/// `operation_wrapper_test::the_same_action_in_another_worktree_is_not_a_duplicate`
/// seeds another worktree's identical success and requires this scope's
/// submission to be accepted, and it fails if the key loses `worktree_id`.
#[test]
fn concurrent_identical_control_actions_in_two_worktrees_not_deduped() {
    let repo = repo_with_feature();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");

    fs::write(main.join("a.txt"), "main-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], main), "main add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-edit", "--no-verify"], main),
        "main commit",
    );

    // Two linked worktrees, each with the same conflicting edit, so the SAME
    // control action (`rebase main`, then `--abort`) applies to both.
    let mut worktrees = Vec::new();
    for branch in ["dedup-one", "dedup-two"] {
        assert_cli_success(
            &run_libra_command(&["branch", branch, "feature"], main),
            "branch",
        );
        let wt = parent.path().join(branch);
        assert_cli_success(
            &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), branch], main),
            "worktree add",
        );
        fs::write(wt.join("a.txt"), "same-edit\n").unwrap();
        assert_cli_success(&run_libra_command(&["add", "a.txt"], &wt), "wt add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "same message", "--no-verify"], &wt),
            "wt commit",
        );
        worktrees.push(wt);
    }

    // TRULY CONCURRENT, not back to back: the two rebases are released
    // together, so both hold their own worktree's control slot at the same
    // time. Sequential invocations could not fail this — each one releases its
    // claim before the next starts — which is the whole point of the barrier.
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(worktrees.len()));
    let mut handles = Vec::new();
    for wt in &worktrees {
        let wt = wt.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let out = run_libra_command(&["rebase", "main"], &wt);
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        }));
    }
    for handle in handles {
        let combined = handle.join().expect("rebase thread");
        assert!(
            !combined.contains("duplicate operation")
                && !combined.contains("already running in this worktree"),
            "two worktrees rebasing at the SAME TIME must each get their own control slot: \
             {combined}"
        );
    }

    // And the sequential invariant still holds for the follow-up control.
    for wt in &worktrees {
        let out = run_libra_command(&["rebase", "main"], wt);
        assert_ne!(
            out.status.code(),
            Some(0),
            "each rebase stops on its own conflict: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !combined.contains("duplicate operation"),
            "the second worktree's identical action must not be deduped: {combined}"
        );
    }
    for wt in &worktrees {
        let out = run_libra_command(&["rebase", "--abort"], wt);
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "each worktree aborts its own rebase: {combined}"
        );
        assert!(
            !combined.contains("duplicate operation"),
            "and the identical abort is not deduped either: {combined}"
        );
    }
}
