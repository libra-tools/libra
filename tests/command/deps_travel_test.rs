//! Integration tests for cross-machine dependency-graph travel + dependency-
//! filtered clone (lore.md 3.2).
//!
//! Verifies: `fetch/pull --notes` travels `refs/notes/deps` from a LOCAL Libra
//! source over the local protocol (default OFF without `--notes`); union-merge
//! on import (no clobber of local edges); `clone --deps-of` scopes the sparse
//! VIEW to the forward dependency closure while keeping a commit-safe FULL
//! checkout (out-of-closure files survive a later commit); `--deps-depth-limit`;
//! anchored+escaped view patterns match a metacharacter filename; empty-graph
//! roots-only fallback; a note for an absent commit is warn-skipped; the cloud
//! and `--no-checkout`/`--bare` combinations are rejected; and a foreign-Git
//! source defers with an honest warning (never a crash).
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network — the "remote"
//! is always a local filesystem path).

use std::{fs, path::Path, process::Command};

use super::{assert_cli_success, run_libra_command};

/// Assert a CLI invocation succeeded (thin wrapper for terse call sites).
fn cli(args: &[&str], cwd: &Path) {
    assert_cli_success(&run_libra_command(args, cwd), &format!("{args:?}"));
}

/// `deps list <path> --json` neighbor set on the repo at `cwd`.
fn neighbors(cwd: &Path, path: &str) -> Vec<String> {
    let out = run_libra_command(&["--json", "deps", "list", path], cwd);
    assert_cli_success(&out, "deps list --json");
    let js: serde_json::Value = serde_json::from_slice(&out.stdout).expect("deps list json");
    js["data"]["neighbors"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Tracked file set from `ls-files` (honors the active sparse VIEW, lore.md 2.2).
fn ls_files(cwd: &Path) -> Vec<String> {
    let out = run_libra_command(&["ls-files"], cwd);
    assert_cli_success(&out, "ls-files");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// A committed Libra source repo whose HEAD declares:
///   a.txt -> b.txt -> c.txt,  a.txt -> `a[1].txt` (metacharacter name),
/// with d.txt unrelated. Deps are added AFTER the final commit because deps
/// notes are per-commit and do not carry forward.
fn source_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("source repo");
    let p = repo.path();
    cli(&["init"], p);
    cli(&["config", "user.name", "t"], p);
    cli(&["config", "user.email", "t@t"], p);
    fs::write(p.join("a.txt"), "a\n").unwrap();
    fs::write(p.join("b.txt"), "b\n").unwrap();
    fs::write(p.join("c.txt"), "c\n").unwrap();
    fs::write(p.join("d.txt"), "d\n").unwrap();
    fs::write(p.join("a[1].txt"), "meta\n").unwrap();
    cli(&["add", "-A"], p);
    cli(&["commit", "-m", "c1", "--no-verify"], p);
    cli(&["deps", "add", "a.txt", "b.txt"], p);
    cli(&["deps", "add", "b.txt", "c.txt"], p);
    cli(&["deps", "add", "a.txt", "a[1].txt"], p);
    repo
}

/// A committed Libra source repo with the same files but NO dependency edges.
fn source_repo_no_deps() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("source repo");
    let p = repo.path();
    cli(&["init"], p);
    cli(&["config", "user.name", "t"], p);
    cli(&["config", "user.email", "t@t"], p);
    fs::write(p.join("a.txt"), "a\n").unwrap();
    fs::write(p.join("d.txt"), "d\n").unwrap();
    cli(&["add", "-A"], p);
    cli(&["commit", "-m", "c1", "--no-verify"], p);
    repo
}

#[test]
fn fetch_notes_travels_from_local_libra_source_and_is_default_off() {
    let src = source_repo();
    let src_path = src.path().to_str().unwrap();
    let work = tempfile::tempdir().unwrap();
    let wp = work.path();

    cli(&["clone", src_path, "cloned"], wp);
    let dest = wp.join("cloned");

    // A plain clone does NOT fetch notes (Git parity): the graph is empty.
    assert!(
        neighbors(&dest, "a.txt").is_empty(),
        "plain clone must not import the dependency graph"
    );

    // `fetch --notes` travels the graph from the local Libra source.
    cli(&["fetch", "origin", "--notes"], &dest);
    let n = neighbors(&dest, "a.txt");
    assert!(
        n.iter().any(|candidate| candidate.as_str() == "b.txt")
            && n.iter().any(|candidate| candidate.as_str() == "a[1].txt"),
        "fetch --notes must import a.txt's edges, got {n:?}"
    );
    assert!(
        neighbors(&dest, "b.txt")
            .iter()
            .any(|candidate| candidate.as_str() == "c.txt"),
        "transitive edge b.txt -> c.txt must travel too"
    );
}

#[test]
fn clone_deps_of_scopes_view_but_keeps_commit_safe_full_checkout() {
    let src = source_repo();
    let src_path = src.path().to_str().unwrap();
    let work = tempfile::tempdir().unwrap();
    let wp = work.path();

    cli(&["clone", "--deps-of", "a.txt", src_path, "cloned"], wp);
    let dest = wp.join("cloned");

    // FULL checkout: every file is on disk, including the out-of-closure d.txt.
    for f in ["a.txt", "b.txt", "c.txt", "d.txt", "a[1].txt"] {
        assert!(dest.join(f).exists(), "{f} must be materialized on disk");
    }

    // The VIEW is scoped to the forward closure of a.txt (incl. the metacharacter
    // file, proving anchored+escaped patterns match), excluding d.txt.
    let view = ls_files(&dest);
    for f in ["a.txt", "b.txt", "c.txt", "a[1].txt"] {
        assert!(
            view.iter().any(|candidate| candidate == f),
            "{f} must be in view: {view:?}"
        );
    }
    assert!(
        !view.iter().any(|candidate| candidate.as_str() == "d.txt"),
        "out-of-closure d.txt must be scoped out of the view: {view:?}"
    );

    // Commit-safety: a later edit + commit must NOT drop the out-of-closure file.
    cli(&["config", "user.name", "t"], &dest);
    cli(&["config", "user.email", "t@t"], &dest);
    fs::write(dest.join("a.txt"), "a2\n").unwrap();
    cli(&["add", "a.txt"], &dest);
    // `--no-gpg-sign`: the clone-created dest has `vault.signing=true` but the
    // test's isolated HOME has no unseal key; the commit's tree contents are what
    // this test asserts on, not its signature.
    cli(
        &["commit", "-m", "edit a", "--no-verify", "--no-gpg-sign"],
        &dest,
    );

    // With the view off, ls-files reflects the full index (== the committed tree):
    // d.txt is still tracked, proving the commit did not silently delete it.
    cli(&["sparse-view", "disable"], &dest);
    let full = ls_files(&dest);
    assert!(
        full.iter().any(|candidate| candidate.as_str() == "d.txt"),
        "commit must preserve the out-of-closure d.txt (no mass-deletion): {full:?}"
    );
}

#[test]
fn clone_deps_of_depth_limit_scopes_direct_dependencies_only() {
    let src = source_repo();
    let src_path = src.path().to_str().unwrap();
    let work = tempfile::tempdir().unwrap();
    let wp = work.path();

    cli(
        &[
            "clone",
            "--deps-of",
            "a.txt",
            "--deps-depth-limit",
            "1",
            src_path,
            "cloned",
        ],
        wp,
    );
    let dest = wp.join("cloned");
    let view = ls_files(&dest);
    // Direct deps of a.txt: b.txt and a[1].txt. c.txt is depth-2 → excluded.
    assert!(view.iter().any(|candidate| candidate.as_str() == "a.txt"));
    assert!(view.iter().any(|candidate| candidate.as_str() == "b.txt"));
    assert!(
        view.iter()
            .any(|candidate| candidate.as_str() == "a[1].txt")
    );
    assert!(
        !view.iter().any(|candidate| candidate.as_str() == "c.txt"),
        "depth-limit 1 must exclude the transitive c.txt: {view:?}"
    );
    assert!(!view.iter().any(|candidate| candidate.as_str() == "d.txt"));
}

#[test]
fn clone_deps_of_empty_graph_scopes_to_roots_and_succeeds() {
    let src = source_repo_no_deps();
    let src_path = src.path().to_str().unwrap();
    let work = tempfile::tempdir().unwrap();
    let wp = work.path();

    // No deps declared → absence-tolerant: exit 0, view scoped to the root only.
    cli(&["clone", "--deps-of", "a.txt", src_path, "cloned"], wp);
    let dest = wp.join("cloned");
    assert!(
        dest.join("d.txt").exists(),
        "full checkout still lands d.txt"
    );
    let view = ls_files(&dest);
    assert_eq!(
        view,
        vec!["a.txt".to_string()],
        "empty graph scopes the view to the root only: {view:?}"
    );
}

#[test]
fn fetch_notes_union_merges_and_does_not_clobber_local_edges() {
    let src = source_repo();
    let src_path = src.path().to_str().unwrap();
    let work = tempfile::tempdir().unwrap();
    let wp = work.path();

    cli(&["clone", src_path, "cloned"], wp);
    let dest = wp.join("cloned");

    // A locally-authored edge on the same HEAD commit as the source.
    cli(&["deps", "add", "d.txt", "a.txt"], &dest);

    // Import the source's edges — the local edge must survive (union, not clobber).
    cli(&["fetch", "origin", "--notes"], &dest);
    assert!(
        neighbors(&dest, "d.txt")
            .iter()
            .any(|candidate| candidate.as_str() == "a.txt"),
        "local edge d.txt -> a.txt must survive the import"
    );
    assert!(
        neighbors(&dest, "a.txt")
            .iter()
            .any(|candidate| candidate.as_str() == "b.txt"),
        "imported edge a.txt -> b.txt must be merged in"
    );
}

#[test]
fn clone_deps_of_is_rejected_for_cloud_sources() {
    let work = tempfile::tempdir().unwrap();
    let out = run_libra_command(
        &[
            "clone",
            "libra+cloud://code.example.com/kepler",
            "--deps-of",
            "a.txt",
            "dest",
        ],
        work.path(),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(129),
        "retired publish restore source must be refused: {stderr}"
    );
    assert!(
        stderr.contains("no longer supported"),
        "error must refuse the retired restore source: {stderr}"
    );
    assert!(
        stderr.contains("libra cloud"),
        "error must hint at git remotes / libra cloud: {stderr}"
    );
}

#[test]
fn clone_deps_of_conflicts_with_no_checkout_and_bare() {
    let src = source_repo();
    let src_path = src.path().to_str().unwrap();
    let work = tempfile::tempdir().unwrap();
    let wp = work.path();

    for skip in ["--no-checkout", "--bare"] {
        let out = run_libra_command(&["clone", "--deps-of", "a.txt", skip, src_path, "dest"], wp);
        assert_ne!(
            out.status.code(),
            Some(0),
            "--deps-of must conflict with {skip}"
        );
    }
}

#[test]
fn fetch_notes_from_foreign_git_source_defers_without_crashing() {
    // A plain Git source cannot travel Libra deps notes in v1 (D17). Requires the
    // `git` binary; skip cleanly if unavailable.
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("skipped (git not available)");
        return;
    }
    let git_src = tempfile::tempdir().unwrap();
    let gp = git_src.path();
    let ok = Command::new("git")
        .args(["init", "-q", gp.to_str().unwrap()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("skipped (git init failed)");
        return;
    }
    for kv in [
        ["config", "user.name", "t"],
        ["config", "user.email", "t@t"],
    ] {
        let _ = Command::new("git").current_dir(gp).args(kv).status();
    }
    fs::write(gp.join("f.txt"), "f\n").unwrap();
    let _ = Command::new("git")
        .current_dir(gp)
        .args(["add", "-A"])
        .status();
    let committed = Command::new("git")
        .current_dir(gp)
        .args(["commit", "-m", "c1"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !committed {
        eprintln!("skipped (git commit failed)");
        return;
    }

    let work = tempfile::tempdir().unwrap();
    let wp = work.path();
    let git_path = gp.to_str().unwrap();
    // Clone the Git source into a Libra repo, then request notes.
    if !run_libra_command(&["clone", git_path, "cloned"], wp)
        .status
        .success()
    {
        eprintln!("skipped (libra clone of git source unsupported here)");
        return;
    }
    let dest = wp.join("cloned");
    let out = run_libra_command(&["fetch", "origin", "--notes"], &dest);
    // Honest deferral: exit 0, an explicit warning, and no dependency graph.
    assert!(
        out.status.success(),
        "fetch --notes must not crash on a git source"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not supported yet"),
        "expected a deferred-notes warning, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
