//! Integration tests for object alternates (lore.md 2.3) — borrowing objects
//! from a shared store, and the airtight deletion-safety of a shared base.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::fs;

use super::{assert_cli_success, parse_cli_error_stderr, run_libra_command};

/// Build a committed repo with `<file>` and return (dir, its blob oid).
fn committed_repo(name_hint: &str) -> (tempfile::TempDir, String) {
    let repo = tempfile::tempdir().expect("repo dir");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init"], p), "init");
    assert_cli_success(&run_libra_command(&["config", "user.name", "t"], p), "name");
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "t@t"], p),
        "email",
    );
    let fname = format!("{name_hint}.txt");
    fs::write(p.join(&fname), format!("{name_hint} shared content\n")).unwrap();
    assert_cli_success(&run_libra_command(&["add", &fname], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );
    let ls = run_libra_command(&["ls-tree", "HEAD"], p);
    let oid = String::from_utf8_lossy(&ls.stdout)
        .lines()
        .find(|l| l.contains(&fname))
        .and_then(|l| {
            l.split_whitespace()
                .find(|w| w.len() == 40 || w.len() == 64)
        })
        .expect("blob oid")
        .to_string();
    (repo, oid)
}

fn objects_dir(repo: &std::path::Path) -> String {
    repo.join(".libra/objects").to_string_lossy().into_owned()
}

#[test]
fn borrower_reads_base_objects_without_a_copy() {
    let (base, oid) = committed_repo("base");
    let borrower = tempfile::tempdir().expect("borrower");
    let bp = borrower.path();
    assert_cli_success(&run_libra_command(&["init"], bp), "init borrower");

    // Before borrowing, the borrower cannot read the base's object.
    let miss = run_libra_command(&["cat-file", "-p", &oid], bp);
    assert_ne!(miss.status.code(), Some(0), "not borrowable yet");

    // Register the alternate; now the borrower reads the base's object.
    assert_cli_success(
        &run_libra_command(&["alternates", "add", &objects_dir(base.path())], bp),
        "add alternate",
    );
    let hit = run_libra_command(&["cat-file", "-p", &oid], bp);
    assert_cli_success(&hit, "borrowed read");
    assert!(String::from_utf8_lossy(&hit.stdout).contains("base shared content"));

    // The borrower's own objects dir does NOT contain the borrowed loose object
    // (read-only borrow, no copy).
    let loose = bp.join(".libra/objects").join(&oid[..2]).join(&oid[2..]);
    assert!(
        !loose.exists(),
        "borrowed object is NOT copied into the borrower"
    );

    // `alternates list` shows it (JSON).
    let list = run_libra_command(&["--json", "alternates", "list"], bp);
    let js: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(
        js["data"]["alternates"].as_array().map(|a| a.len()),
        Some(1)
    );
}

#[test]
fn shared_base_gc_refuses_to_prune_then_allows_after_remove() {
    let (base, _oid) = committed_repo("base");
    let borrower = tempfile::tempdir().expect("borrower");
    let bp = borrower.path();
    assert_cli_success(&run_libra_command(&["init"], bp), "init");
    assert_cli_success(
        &run_libra_command(&["alternates", "add", &objects_dir(base.path())], bp),
        "add alternate",
    );

    // The base now has a live borrower → gc refuses to prune loose objects.
    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], base.path());
    assert_cli_success(&gc, "gc runs");
    assert!(
        String::from_utf8_lossy(&gc.stdout).contains("shared"),
        "gc skips prune on a shared base: {}",
        String::from_utf8_lossy(&gc.stdout)
    );

    // After the borrower removes the alternate, the base prunes normally.
    assert_cli_success(
        &run_libra_command(&["alternates", "remove", &objects_dir(base.path())], bp),
        "remove alternate",
    );
    let gc2 = run_libra_command(&["maintenance", "run", "--task", "gc"], base.path());
    assert_cli_success(&gc2, "gc after remove");
    assert!(
        !String::from_utf8_lossy(&gc2.stdout).contains("shared"),
        "gc no longer skips: {}",
        String::from_utf8_lossy(&gc2.stdout)
    );
}

#[test]
fn add_refuses_self_reference() {
    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init"], p), "init");
    let out = run_libra_command(&["alternates", "add", &objects_dir(p)], p);
    assert_ne!(out.status.code(), Some(0), "self-borrow refused");
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
}

#[test]
fn fsck_reports_dangling_alternate() {
    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init"], p), "init");
    // Register a base, then delete it → dangling alternate.
    let (base, _oid) = committed_repo("base");
    assert_cli_success(
        &run_libra_command(&["alternates", "add", &objects_dir(base.path())], p),
        "add",
    );
    drop(base); // the base repo (and its objects dir) is removed
    let fsck = run_libra_command(&["fsck"], p);
    assert!(
        String::from_utf8_lossy(&fsck.stderr).contains("dangling object alternate"),
        "fsck flags the dangling alternate: {}",
        String::from_utf8_lossy(&fsck.stderr)
    );
    // Codex P1: a dangling alternate must FAIL fsck (non-zero exit).
    assert_ne!(fsck.status.code(), Some(0), "dangling alternate fails fsck");
}

#[test]
fn shared_base_refuses_obliterate() {
    let (base, oid) = committed_repo("base");
    let borrower = tempfile::tempdir().expect("borrower");
    let bp = borrower.path();
    assert_cli_success(&run_libra_command(&["init"], bp), "init");
    assert_cli_success(
        &run_libra_command(&["alternates", "add", &objects_dir(base.path())], bp),
        "add",
    );
    // The base is now shared → `file obliterate` on its object is refused
    // (a borrower may need it) — Codex P1.
    let out = run_libra_command(&["file", "obliterate", &oid, "--yes"], base.path());
    assert_ne!(
        out.status.code(),
        Some(0),
        "obliterate on a shared base refused"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("shared"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// W0 §C.11 release gate: obliteration RECOVERY is a deletion entry point.
///
/// `run_obliterate` used to complete any interrupted obliteration — unlinking
/// payloads — BEFORE asking whether a borrower still needed them, and
/// `--recover` never asked at all. Both routes must now refuse.
#[test]
fn shared_base_refuses_obliterate_recovery() {
    let (base, _oid) = committed_repo("base");
    let borrower = tempfile::tempdir().expect("borrower");
    let bp = borrower.path();
    assert_cli_success(&run_libra_command(&["init"], bp), "init");
    assert_cli_success(
        &run_libra_command(&["alternates", "add", &objects_dir(base.path())], bp),
        "add",
    );

    let out = run_libra_command(&["file", "obliterate", "--recover"], base.path());
    assert_ne!(
        out.status.code(),
        Some(0),
        "obliterate --recover on a shared base is refused: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("shared"),
        "and says why the store is off limits: {stderr}"
    );
    // §C.13: the refusal is a CONFLICT. It used to carry
    // `LBR-OBLITERATE-003`, whose documented meaning is "re-run with --yes" —
    // advice the user may already have taken, about a condition that has
    // nothing to do with confirmation.
    let json = run_libra_command(&["--json", "file", "obliterate", "--recover"], base.path());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&json.stdout),
        String::from_utf8_lossy(&json.stderr)
    );
    assert!(
        combined.contains("LBR-CONFLICT-002") && !combined.contains("LBR-OBLITERATE-003"),
        "a borrowed store is a conflict, not a missing confirmation: {combined}"
    );

    // Once nobody borrows, recovery runs again (there is nothing to recover
    // here, which is exactly the point — the refusal was about the borrower,
    // not about the absence of work).
    assert_cli_success(
        &run_libra_command(&["alternates", "remove", &objects_dir(base.path())], bp),
        "remove alternate",
    );
    assert_cli_success(
        &run_libra_command(&["file", "obliterate", "--recover"], base.path()),
        "recover after remove",
    );
}

/// W0 §C.11 release gate: BOTH cache-eviction routes pass the same gate.
///
/// `libra cache evict` had the borrower check inlined; `libra maintenance run
/// --task cache-evict` called the eviction engine directly and so skipped it
/// entirely. A safety contract with two entry points and one implementation is
/// a contract with a hole in it — this pins that both routes refuse.
#[test]
fn shared_base_refuses_cache_eviction_on_both_routes() {
    let (base, _oid) = committed_repo("base");
    let borrower = tempfile::tempdir().expect("borrower");
    let bp = borrower.path();
    assert_cli_success(&run_libra_command(&["init"], bp), "init");
    assert_cli_success(
        &run_libra_command(&["alternates", "add", &objects_dir(base.path())], bp),
        "add alternate",
    );

    let direct = run_libra_command(&["cache", "evict"], base.path());
    assert_ne!(
        direct.status.code(),
        Some(0),
        "`cache evict` refuses on a shared base: {}",
        String::from_utf8_lossy(&direct.stdout)
    );
    assert!(
        String::from_utf8_lossy(&direct.stderr).contains("shared"),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );

    // The scheduled route reaches the same engine and must refuse too. It is
    // reported as a failed task rather than a process-level error, so assert
    // on the message rather than only on the exit code.
    let scheduled = run_libra_command(
        &["maintenance", "run", "--task", "cache-evict"],
        base.path(),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&scheduled.stdout),
        String::from_utf8_lossy(&scheduled.stderr)
    );
    assert!(
        combined.contains("shared"),
        "scheduled cache-evict refuses on a shared base too: {combined}"
    );
}

// ── lore.md 2.11: default shared-store (clone --shared / clone.shared) ──────

#[test]
fn clone_shared_registers_alternate_for_local_libra_source() {
    let (base, _oid) = committed_repo("base");
    let dest = tempfile::tempdir().expect("dest parent");
    let clone_path = dest.path().join("clone");

    // `clone --shared <local libra src>` registers the source as an alternate
    // (v1 still copies, but the borrow link + base protection are established).
    let out = run_libra_command(
        &[
            "clone",
            "--shared",
            base.path().to_str().unwrap(),
            clone_path.to_str().unwrap(),
        ],
        dest.path(),
    );
    assert_cli_success(&out, "clone --shared");
    let alts = run_libra_command(&["alternates", "list"], &clone_path);
    assert!(
        String::from_utf8_lossy(&alts.stdout).contains(".libra/objects"),
        "alternate registered: {}",
        String::from_utf8_lossy(&alts.stdout)
    );
    // The base is now a protected shared store.
    let borrowers = base.path().join(".libra/objects/info/borrowers");
    assert!(borrowers.exists(), "base has a borrowers file");
}

#[test]
fn plain_clone_registers_no_alternate_by_default() {
    let (base, _oid) = committed_repo("base");
    let dest = tempfile::tempdir().expect("dest parent");
    let clone_path = dest.path().join("clone");
    // Default OFF: a plain clone (no --shared, no config) registers nothing.
    let out = run_libra_command(
        &[
            "clone",
            base.path().to_str().unwrap(),
            clone_path.to_str().unwrap(),
        ],
        dest.path(),
    );
    assert_cli_success(&out, "plain clone");
    let alts = run_libra_command(&["alternates", "list"], &clone_path);
    assert!(
        String::from_utf8_lossy(&alts.stdout).contains("no alternates"),
        "no alternate by default: {}",
        String::from_utf8_lossy(&alts.stdout)
    );
}

#[test]
fn clone_no_shared_overrides_shared() {
    let (base, _oid) = committed_repo("base");
    let dest = tempfile::tempdir().expect("dest parent");
    let clone_path = dest.path().join("clone");
    // --no-shared wins over --shared.
    let out = run_libra_command(
        &[
            "clone",
            "--shared",
            "--no-shared",
            base.path().to_str().unwrap(),
            clone_path.to_str().unwrap(),
        ],
        dest.path(),
    );
    assert_cli_success(&out, "clone --shared --no-shared");
    let alts = run_libra_command(&["alternates", "list"], &clone_path);
    assert!(
        String::from_utf8_lossy(&alts.stdout).contains("no alternates"),
        "--no-shared overrides: {}",
        String::from_utf8_lossy(&alts.stdout)
    );
}

/// plan-20260714 W0 deletion hard gate: EVERY deletion entry point must
/// refuse while a live borrower exists. `repack -d` drops loose objects,
/// so it is one of them (the base's reachability does not include the
/// borrower's refs).
#[test]
fn repack_delete_refuses_while_a_borrower_exists() {
    let (base, oid) = committed_repo("repack-base");
    let borrower = tempfile::tempdir().expect("borrower");
    let bp = borrower.path();
    assert_cli_success(&run_libra_command(&["init"], bp), "init borrower");
    assert_cli_success(
        &run_libra_command(&["alternates", "add", &objects_dir(base.path())], bp),
        "add alternate",
    );

    // With the borrower registered, `repack -d` must refuse in the BASE.
    let refused = run_libra_command(&["repack", "-a", "-d"], base.path());
    assert!(
        !refused.status.success(),
        "repack -d must refuse while borrowed: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("borrow"),
        "the refusal names the borrower relationship: {stderr}"
    );
    let loose = base
        .path()
        .join(".libra/objects")
        .join(&oid[..2])
        .join(&oid[2..]);
    assert!(
        loose.exists(),
        "no loose object was deleted before refusing"
    );

    // After the borrower detaches, the same command is allowed again.
    assert_cli_success(
        &run_libra_command(&["alternates", "remove", &objects_dir(base.path())], bp),
        "remove alternate",
    );
    let allowed = run_libra_command(&["repack", "-a", "-d"], base.path());
    assert!(
        allowed.status.success(),
        "repack -d proceeds once nothing borrows: {}",
        String::from_utf8_lossy(&allowed.stderr)
    );
}

/// W0 deletion hard gate: `agent clean` unlinks object payloads (checkpoint
/// prune, findings-blob reclamation), so a live borrower must stop it — and a
/// preview must remain non-mutating either way.
///
/// Without this the gate could be deleted and the whole `agent clean` suite
/// would stay green: no other test combines the two subsystems.
#[test]
fn shared_base_refuses_agent_clean() {
    let (base, _oid) = committed_repo("base");
    let borrower = tempfile::tempdir().expect("borrower");
    let bp = borrower.path();
    assert_cli_success(&run_libra_command(&["init"], bp), "init");
    assert_cli_success(
        &run_libra_command(&["alternates", "add", &objects_dir(base.path())], bp),
        "add",
    );

    let out = run_libra_command(&["--json", "agent", "clean", "--gc"], base.path());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "agent clean on a shared base must be refused: {combined}"
    );
    assert!(
        combined.contains("LBR-CONFLICT-002"),
        "and reported as a conflict: {combined}"
    );

    // A preview deletes nothing, so it is allowed to run.
    let dry = run_libra_command(&["agent", "clean", "--gc", "--dry-run"], base.path());
    assert_cli_success(&dry, "agent clean --dry-run on a shared base");

    // Once nobody borrows, the real clean runs.
    assert_cli_success(
        &run_libra_command(&["alternates", "remove", &objects_dir(base.path())], bp),
        "remove alternate",
    );
    assert_cli_success(
        &run_libra_command(&["agent", "clean", "--gc"], base.path()),
        "agent clean after the borrower is gone",
    );
}

/// A borrower registration whose repository is GONE is retired only by an
/// explicit `libra alternates prune` — never automatically.
///
/// Absence is not proof: an automounted or temporarily unavailable borrower
/// answers ENOENT for a path that exists again the moment it is mounted, and
/// pruning on that would let the next `gc` delete objects it still needs. So
/// the gate keeps refusing until a human asserts the borrower is gone, and
/// this test pins both halves — the refusal that persists, and the command
/// that ends it.
#[test]
fn a_missing_borrower_is_retired_only_by_an_explicit_prune() {
    let (base, _oid) = committed_repo("base");
    let borrower = tempfile::tempdir().expect("borrower");
    let bp = borrower.path();
    assert_cli_success(&run_libra_command(&["init"], bp), "init");
    assert_cli_success(
        &run_libra_command(&["alternates", "add", &objects_dir(base.path())], bp),
        "add",
    );

    let borrowers = base
        .path()
        .join(".libra")
        .join("objects")
        .join("info")
        .join("borrowers");
    assert!(
        std::fs::read_to_string(&borrowers)
            .expect("borrowers file")
            .contains("objects"),
        "the borrower is registered"
    );

    // The borrowing repository goes away without unregistering (rm -rf).
    drop(borrower);

    // The base still refuses to delete: absence is not proof.
    let gc = run_libra_command(
        &["--json", "maintenance", "run", "--task", "gc"],
        base.path(),
    );
    let gc_out = String::from_utf8_lossy(&gc.stdout);
    assert!(
        gc_out.contains("skipped loose-object prune"),
        "a registration this store cannot disprove keeps protecting the objects: {gc_out}"
    );
    assert!(
        !std::fs::read_to_string(&borrowers)
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "and nothing removed it automatically"
    );

    // A preview reports it and changes nothing.
    let dry = run_libra_command(&["alternates", "prune", "--dry-run"], base.path());
    assert_cli_success(&dry, "alternates prune --dry-run");
    assert!(
        String::from_utf8_lossy(&dry.stdout).contains("would retire"),
        "the preview names what it would retire: {}",
        String::from_utf8_lossy(&dry.stdout)
    );
    assert!(
        !std::fs::read_to_string(&borrowers)
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "a preview must not change the registration"
    );

    // The explicit prune retires it, and deletion is unblocked.
    let prune = run_libra_command(&["alternates", "prune"], base.path());
    assert_cli_success(&prune, "alternates prune");
    assert!(
        std::fs::read_to_string(&borrowers)
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "the explicit prune retires the registration"
    );
    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], base.path());
    assert_cli_success(&gc, "gc after the prune");
    assert!(
        !String::from_utf8_lossy(&gc.stdout).contains("skipped loose-object prune"),
        "and the base is no longer pinned"
    );
}

/// `alternates prune <path>` matches the registration VERBATIM.
///
/// Canonicalizing the user's argument first would let `prune /alias`, where
/// `/alias` is a symlink at a live borrower, retire that borrower's
/// registration — and the base would then be free to delete objects it is
/// still lending. The alias must simply not match.
#[cfg(unix)]
#[test]
fn a_positional_prune_does_not_retire_a_borrower_through_an_alias() {
    let (base, _oid) = committed_repo("base");
    let borrower = tempfile::tempdir().expect("borrower");
    let bp = borrower.path();
    assert_cli_success(&run_libra_command(&["init"], bp), "init");
    assert_cli_success(
        &run_libra_command(&["alternates", "add", &objects_dir(base.path())], bp),
        "add",
    );

    // A DIFFERENT path that resolves to the registered object directory.
    let alias_root = tempfile::tempdir().expect("alias root");
    let alias = alias_root.path().join("alias-objects");
    std::os::unix::fs::symlink(bp.join(".libra").join("objects"), &alias).expect("symlink");

    let borrowers = base
        .path()
        .join(".libra")
        .join("objects")
        .join("info")
        .join("borrowers");
    let before = std::fs::read_to_string(&borrowers).expect("borrowers");
    assert!(!before.trim().is_empty(), "the borrower is registered");

    let prune = run_libra_command(
        &["alternates", "prune", alias.to_str().expect("utf-8")],
        base.path(),
    );
    assert_cli_success(&prune, "alternates prune <alias>");
    assert_eq!(
        std::fs::read_to_string(&borrowers).expect("borrowers"),
        before,
        "an alias must not retire a live borrower's registration"
    );

    // And the base is still protected.
    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], base.path());
    assert_cli_success(&gc, "gc still runs");
    assert!(
        String::from_utf8_lossy(&gc.stdout).contains("skipped loose-object prune"),
        "the live borrower still blocks deletion: {}",
        String::from_utf8_lossy(&gc.stdout)
    );
}
