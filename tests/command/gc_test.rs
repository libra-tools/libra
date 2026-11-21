//! Integration tests for `libra gc` (ADR-MX-07) — the M-GC matrix.
//!
//! **Layer:** L1 — deterministic, no external dependencies.
//!
//! The command reuses the maintenance reachability inventory and the
//! alternates borrow protection verbatim; these tests pin the public
//! `gc` surface on top of them (G1–G6) and never write a traversal of
//! their own.

use std::{fs, path::Path};

use super::*;

/// Every loose object id currently on disk under `.libra/objects/`
/// (skipping `pack`, `info`, and empty shard directories).
fn loose_object_ids(repo: &Path) -> Vec<String> {
    let objects = repo.join(".libra").join("objects");
    let mut ids = Vec::new();
    let Ok(dirs) = fs::read_dir(&objects) else {
        return ids;
    };
    for dir in dirs.flatten() {
        let name = dir.file_name().to_string_lossy().into_owned();
        // Object shards are the 2-hex-char directories.
        if name.len() != 2 || !name.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let Ok(files) = fs::read_dir(dir.path()) else {
            continue;
        };
        for file in files.flatten() {
            ids.push(format!("{name}{}", file.file_name().to_string_lossy()));
        }
    }
    ids
}

/// Pack files (`*.pack`) currently in the pack directory.
fn pack_files(repo: &Path) -> Vec<std::path::PathBuf> {
    let pack_dir = repo.join(".libra").join("objects").join("pack");
    let Ok(entries) = fs::read_dir(&pack_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "pack"))
        .collect()
}

/// Build a repo with a few commits so several loose objects exist.
fn repo_with_history() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    for i in 0..3 {
        fs::write(
            repo.path().join(format!("file{i}.txt")),
            format!("contents number {i}\n"),
        )
        .expect("write file");
        let output = run_libra_command(&["add", &format!("file{i}.txt")], repo.path());
        assert_cli_success(&output, "add file");
        let output = run_libra_command(
            &["commit", "-m", &format!("commit {i}"), "--no-verify"],
            repo.path(),
        );
        assert_cli_success(&output, "commit file");
    }
    repo
}

/// Write a loose object nothing references, returning its full OID.
fn write_unreachable_object(repo: &Path) -> String {
    let orphan = repo.join("orphan.txt");
    fs::write(&orphan, "orphan payload\n").expect("write orphan file");
    let output = run_libra_command(&["hash-object", "-w", "orphan.txt"], repo);
    assert_cli_success(&output, "hash-object -w");
    fs::remove_file(&orphan).expect("remove orphan file");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Whether the loose file for `oid` still exists on disk.
fn loose_object_exists(repo: &Path, oid: &str) -> bool {
    repo.join(".libra")
        .join("objects")
        .join(&oid[..2])
        .join(&oid[2..])
        .exists()
}

/// M-GC G1: a freshly initialized repository collects garbage successfully
/// (t6500:18 analog — `git gc` in a new repo exits 0).
#[test]
fn test_gc_matrix_g1_fresh_repo() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());

    let out = run_libra_command(&["gc"], repo.path());
    assert_cli_success(&out, "gc in a fresh repository");

    // Every step renders in the human summary, in Git's order.
    let stdout = String::from_utf8_lossy(&out.stdout);
    for task in [
        "pack-refs",
        "reflog-expire",
        "repack",
        "prune",
        "worktree-prune",
    ] {
        assert!(
            stdout.contains(&format!("gc {task}:")),
            "gc summary must report the {task} step, got: {stdout}"
        );
    }

    // JSON works on the empty repository too.
    let json = run_libra_command(&["--json", "gc"], repo.path());
    assert_cli_success(&json, "gc --json in a fresh repository");
    let envelope: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("gc --json emits a JSON envelope");
    assert_eq!(envelope["command"], "gc");
    assert_eq!(
        envelope["data"]["steps"].as_array().map(|s| s.len()),
        Some(5)
    );
}

/// M-GC G2: reachable objects end up packed into a fresh pack (loose copies
/// dropped), and unreachable loose objects past `gc.pruneExpire` are deleted,
/// matching Git's post-`gc` object state.
#[test]
fn test_gc_matrix_g2_packs_reachable_and_prunes_expired() {
    let repo = repo_with_history();
    let head = libra_stdout(&["rev-parse", "HEAD"], repo.path());
    let orphan = write_unreachable_object(repo.path());

    // The default policy (2.weeks.ago) keeps a fresh orphan; `0` (0 days)
    // makes every unreachable object past the expiry, deterministically.
    let set = run_libra_command(&["config", "set", "gc.pruneExpire", "0"], repo.path());
    assert_cli_success(&set, "config set gc.pruneExpire 0");

    let out = run_libra_command(&["gc"], repo.path());
    assert_cli_success(&out, "gc");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("removed 1 unreachable loose objects"),
        "the orphan past the expiry must be pruned, got: {stdout}"
    );

    // Reachable objects survive (readable through the new pack), and every
    // reachable loose copy was dropped by the repack step.
    let head_after = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_cli_success(&head_after, "rev-parse HEAD after gc");
    assert_eq!(
        String::from_utf8_lossy(&head_after.stdout).trim(),
        head.trim(),
        "gc must not move HEAD"
    );
    let subject = run_libra_command(&["show", "-s", "--format=%s", "HEAD"], repo.path());
    assert_cli_success(&subject, "show HEAD after gc");
    assert_eq!(
        String::from_utf8_lossy(&subject.stdout).trim(),
        "commit 2",
        "reachable commits must remain readable after gc"
    );
    assert!(
        !loose_object_exists(repo.path(), &orphan),
        "the expired unreachable object must be deleted"
    );
    assert!(
        loose_object_ids(repo.path()).is_empty(),
        "reachable objects must live in the pack, not as loose copies: {:?}",
        loose_object_ids(repo.path())
    );
    assert!(
        !pack_files(repo.path()).is_empty(),
        "gc must have written at least one pack"
    );

    // A fresh orphan (within the default 2.weeks.ago grace) survives a plain gc.
    let fresh = write_unreachable_object(repo.path());
    let out = run_libra_command(&["gc", "--no-prune"], repo.path());
    assert_cli_success(&out, "gc --no-prune");
    assert!(
        loose_object_exists(repo.path(), &fresh),
        "--no-prune must never delete unreachable loose objects"
    );
}

/// M-GC G3: `--prune=now` deletes unreachable objects immediately (Git
/// parity, including objects created within the current second) and
/// `--no-prune` keeps every one of them.
#[test]
fn test_gc_matrix_g3_prune_now_and_no_prune() {
    // `--prune=now` removes the orphan even though it was just written.
    let repo = repo_with_history();
    let orphan = write_unreachable_object(repo.path());
    let out = run_libra_command(&["gc", "--prune=now"], repo.path());
    assert_cli_success(&out, "gc --prune=now");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("removed 1 unreachable loose objects"),
        "--prune=now must delete the fresh orphan immediately: {}",
        format_cli_failure(&out, "gc --prune=now")
    );
    assert!(
        !loose_object_exists(repo.path(), &orphan),
        "--prune=now must delete the fresh orphan"
    );
    let head = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_cli_success(&head, "HEAD survives --prune=now");

    // `--no-prune` keeps it.
    let repo = repo_with_history();
    let orphan = write_unreachable_object(repo.path());
    let out = run_libra_command(&["gc", "--no-prune"], repo.path());
    assert_cli_success(&out, "gc --no-prune");
    assert!(
        loose_object_exists(repo.path(), &orphan),
        "--no-prune must keep unreachable loose objects"
    );
    let kind = run_libra_command(&["cat-file", "-t", &orphan], repo.path());
    assert_cli_success(&kind, "orphan remains readable after --no-prune");
    assert_eq!(String::from_utf8_lossy(&kind.stdout).trim(), "blob");
}

/// M-GC G4: `--auto` below `gc.auto` (default 6700) exits 0 with ZERO writes;
/// at/above the threshold it runs the full collection. `gc.auto <= 0`
/// disables auto-gc, matching Git's `too_many_loose_objects`.
#[test]
fn test_gc_matrix_g4_auto_threshold_zero_writes() {
    let repo = repo_with_history();
    let loose_before = loose_object_ids(repo.path());
    assert!(!loose_before.is_empty(), "fixture has loose objects");

    let out = run_libra_command(&["gc", "--auto"], repo.path());
    assert_cli_success(&out, "gc --auto below the threshold");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Auto gc skipped"),
        "below gc.auto the command reports the skip: {}",
        format_cli_failure(&out, "gc --auto")
    );
    assert!(
        pack_files(repo.path()).is_empty(),
        "--auto below the threshold must not write a pack"
    );
    assert!(
        !repo.path().join(".libra/packed-refs").exists(),
        "--auto below the threshold must not write packed-refs"
    );
    assert_eq!(
        loose_object_ids(repo.path()),
        loose_before,
        "--auto below the threshold must not touch loose objects"
    );

    // At/above the threshold the full collection runs (gc.auto=1 with a
    // non-empty object store).
    let set = run_libra_command(&["config", "set", "gc.auto", "1"], repo.path());
    assert_cli_success(&set, "config set gc.auto 1");
    let out = run_libra_command(&["gc", "--auto"], repo.path());
    assert_cli_success(&out, "gc --auto at the threshold");
    assert!(
        !pack_files(repo.path()).is_empty(),
        "--auto at/above the threshold must collect"
    );

    // gc.auto <= 0 disables auto-gc entirely (Git parity: a non-positive
    // threshold makes `too_many_loose_objects` always false).
    let set = run_libra_command(&["config", "set", "gc.auto", "0"], repo.path());
    assert_cli_success(&set, "config set gc.auto 0");
    let out = run_libra_command(&["gc", "--auto"], repo.path());
    assert_cli_success(&out, "gc --auto with gc.auto=0");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Auto gc skipped"),
        "gc.auto=0 disables auto-gc: {}",
        format_cli_failure(&out, "gc --auto gc.auto=0")
    );
}

/// M-GC G5: `--quiet` suppresses the human summary, `--aggressive` is
/// accepted and reported, and `--json` summarizes every step.
#[test]
fn test_gc_matrix_g5_quiet_aggressive_json() {
    let repo = repo_with_history();

    let out = run_libra_command(&["gc", "--quiet"], repo.path());
    assert_cli_success(&out, "gc --quiet");
    assert!(
        out.stdout.is_empty(),
        "--quiet must suppress the human summary, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let out = run_libra_command(&["gc", "--aggressive"], repo.path());
    assert_cli_success(&out, "gc --aggressive");

    let json = run_libra_command(&["--json", "gc", "--aggressive"], repo.path());
    assert_cli_success(&json, "gc --json --aggressive");
    let envelope: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("gc --json emits a JSON envelope");
    assert_eq!(envelope["command"], "gc");
    assert_eq!(envelope["ok"], true);
    let data = &envelope["data"];
    assert_eq!(data["aggressive"], true);
    assert_eq!(data["auto"], false);
    assert_eq!(data["auto_skipped"], false);
    assert_eq!(data["prune"], "2.weeks.ago");
    let steps = data["steps"].as_array().expect("steps array");
    assert_eq!(steps.len(), 5, "gc runs exactly five steps");
    for (i, expected_task) in [
        "pack-refs",
        "reflog-expire",
        "repack",
        "prune",
        "worktree-prune",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            steps[i]["task"],
            expected_task.to_string(),
            "step order matches Git's gc"
        );
        assert_eq!(steps[i]["success"], true, "step {expected_task} succeeded");
    }

    // The skipped-auto envelope is also machine-readable.
    let json = run_libra_command(&["--json", "gc", "--auto"], repo.path());
    assert_cli_success(&json, "gc --json --auto");
    let envelope: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("gc --json --auto emits a JSON envelope");
    assert_eq!(envelope["data"]["auto"], true);
    assert_eq!(envelope["data"]["auto_skipped"], true);
    assert_eq!(
        envelope["data"]["steps"].as_array().map(|s| s.len()),
        Some(0),
        "a skipped auto run performs no steps"
    );
}

/// M-GC G6: while another repository borrows this object store via
/// alternates, the deletion steps refuse (the shared maintenance guard —
/// regression), reachable AND unreachable loose objects stay on disk, and
/// the borrower keeps reading.
#[test]
fn test_gc_matrix_g6_alternates_borrow_protection() {
    let base = repo_with_history();
    let orphan = write_unreachable_object(base.path());

    let dest = tempdir().expect("borrower parent");
    let borrower_path = dest.path().join("borrower");
    let clone = run_libra_command(
        &[
            "clone",
            "--shared",
            base.path().to_str().expect("base path"),
            borrower_path.to_str().expect("borrower path"),
        ],
        dest.path(),
    );
    assert_cli_success(&clone, "clone --shared");

    // The base is now a protected shared store: gc must refuse the deletion
    // steps (LBR-CONFLICT-002) instead of deleting borrowed objects.
    let gc = run_libra_command(&["gc", "--prune=now"], base.path());
    assert!(
        !gc.status.success(),
        "gc must refuse deletion while a live borrower exists: {}",
        format_cli_failure(&gc, "gc with borrower")
    );
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&gc.stdout),
        String::from_utf8_lossy(&gc.stderr)
    );
    assert!(
        diagnostic.contains("shared (other repositories borrow"),
        "the refusal must name the borrow, got: {diagnostic}"
    );
    assert!(
        diagnostic.contains("LBR-CONFLICT-002"),
        "the refusal carries the stable conflict code, got: {diagnostic}"
    );
    assert!(
        loose_object_exists(base.path(), &orphan),
        "a borrowed store must never delete unreachable loose objects"
    );
    let head = run_libra_command(&["rev-parse", "HEAD"], base.path());
    assert_cli_success(&head, "base HEAD survives the refused gc");

    // The borrower still reads every object through the alternate.
    let kind = run_libra_command(&["cat-file", "-t", "HEAD"], &borrower_path);
    assert_cli_success(&kind, "borrower reads HEAD");
    assert_eq!(String::from_utf8_lossy(&kind.stdout).trim(), "commit");
    let kind = run_libra_command(&["cat-file", "-t", &orphan], &borrower_path);
    assert_cli_success(&kind, "borrower reads the orphan");
    assert_eq!(String::from_utf8_lossy(&kind.stdout).trim(), "blob");
}

/// `--prune` and `--no-prune` are mutually exclusive, and an invalid prune
/// expiry fails before any step writes.
#[test]
fn test_gc_matrix_prune_argument_validation() {
    let repo = repo_with_history();
    let orphan = write_unreachable_object(repo.path());

    let out = run_libra_command(&["gc", "--prune=now", "--no-prune"], repo.path());
    assert!(
        !out.status.success(),
        "--prune conflicts with --no-prune: {}",
        format_cli_failure(&out, "conflicting prune flags")
    );

    let out = run_libra_command(&["gc", "--prune=not-a-date"], repo.path());
    assert!(
        !out.status.success(),
        "an invalid prune expiry must fail: {}",
        format_cli_failure(&out, "invalid prune expiry")
    );
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        diagnostic.contains("invalid prune expiry"),
        "the failure must name the bad expiry, got: {diagnostic}"
    );
    assert!(
        loose_object_exists(repo.path(), &orphan),
        "an invalid expiry must fail before any deletion"
    );
}

/// Git's dotted approxidate forms parse as prune cutoffs (`2.weeks.ago`,
/// `3.days.ago`), and `never` disables pruning.
#[test]
fn test_gc_matrix_prune_expiry_forms() {
    let repo = repo_with_history();
    let orphan = write_unreachable_object(repo.path());

    // `never` keeps everything (like `--no-prune`).
    let out = run_libra_command(&["gc", "--prune=never"], repo.path());
    assert_cli_success(&out, "gc --prune=never");
    assert!(
        loose_object_exists(repo.path(), &orphan),
        "--prune=never must keep unreachable objects"
    );

    // `3.days.ago` keeps a fresh orphan (it is younger than the cutoff).
    let out = run_libra_command(&["gc", "--prune=3.days.ago"], repo.path());
    assert_cli_success(&out, "gc --prune=3.days.ago");
    assert!(
        loose_object_exists(repo.path(), &orphan),
        "a fresh orphan must survive a 3-day prune cutoff"
    );

    // `0.days.ago` expires everything, like Git.
    let out = run_libra_command(&["gc", "--prune=0.days.ago"], repo.path());
    assert_cli_success(&out, "gc --prune=0.days.ago");
    assert!(
        !loose_object_exists(repo.path(), &orphan),
        "--prune=0.days.ago must delete the fresh orphan"
    );
}

/// A recursive snapshot of a repository's `.libra` directory: relative path →
/// file bytes. A no-write `gc --auto` must leave it byte-identical.
fn libra_dir_snapshot(repo: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                // The SQLite database is touched by any command that opens it
                // (WAL checkpointing), so the no-write assertion covers the
                // object/ref files gc actually writes.
                if rel.starts_with("libra.db") || rel.starts_with("object-index-repair-locks/") {
                    continue;
                }
                if let Ok(bytes) = fs::read(&path) {
                    out.insert(rel, bytes);
                }
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(&repo.join(".libra"), &repo.join(".libra"), &mut out);
    out
}

/// Compare two snapshots, reporting only the paths that changed.
fn assert_snapshot_unchanged(
    before: &std::collections::BTreeMap<String, Vec<u8>>,
    after: &std::collections::BTreeMap<String, Vec<u8>>,
    label: &str,
) {
    let mut changed = Vec::new();
    for (path, bytes) in before {
        match after.get(path) {
            Some(new) if new == bytes => {}
            Some(_) => changed.push(format!("{path} (modified)")),
            None => changed.push(format!("{path} (removed)")),
        }
    }
    for path in after.keys() {
        if !before.contains_key(path) {
            changed.push(format!("{path} (added)"));
        }
    }
    assert!(changed.is_empty(), "{label}: changed files: {changed:?}");
}

/// Build a repository holding two packs (a gc pack plus a fetched pack).
fn repo_with_two_packs(root: &Path) -> std::path::PathBuf {
    let g = root.join("g");
    fs::create_dir_all(&g).expect("mkdir g");
    assert_cli_success(&run_libra_command(&["init"], &g), "init g");
    run_libra_command(&["config", "set", "user.name", "t"], &g);
    run_libra_command(&["config", "set", "user.email", "t@t"], &g);
    fs::write(g.join("f.txt"), "a\n").expect("write f");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], &g), "stage A");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "A", "--no-verify"], &g),
        "commit A",
    );
    assert_cli_success(&run_libra_command(&["gc"], &g), "pack the first set");

    let other = root.join("other");
    fs::create_dir_all(&other).expect("mkdir other");
    assert_cli_success(&run_libra_command(&["init"], &other), "init other");
    run_libra_command(&["config", "set", "user.name", "t"], &other);
    run_libra_command(&["config", "set", "user.email", "t@t"], &other);
    fs::write(other.join("g.txt"), "b\n").expect("write g");
    assert_cli_success(&run_libra_command(&["add", "g.txt"], &other), "stage B");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "B", "--no-verify"], &other),
        "commit B",
    );
    assert_cli_success(
        &run_libra_command(
            &["remote", "add", "other", other.to_str().expect("other")],
            &g,
        ),
        "add remote",
    );
    assert_cli_success(
        &run_libra_command(&["fetch", "other"], &g),
        "fetch the second pack",
    );
    g
}

fn packs_of(repo: &Path) -> usize {
    fs::read_dir(repo.join(".libra/objects/pack"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("pack"))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn test_gc_auto_pack_limit_decision() {
    // AC1/AC2: two packs and `gc.autoPackLimit=1` trigger a full gc.
    let work = tempfile::tempdir().expect("work dir");
    let g = repo_with_two_packs(work.path());
    assert_eq!(packs_of(&g), 2, "fixture has two packs");
    run_libra_command(&["config", "set", "gc.autoPackLimit", "1"], &g);
    let out = run_libra_command(&["gc", "--auto"], &g);
    assert_cli_success(&out, "AC1: gc --auto runs");
    assert_eq!(packs_of(&g), 1, "AC1: the packs were consolidated");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .any(|line| line == "Auto packing the repository for optimum performance."),
        "AC2: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // AC3: `gc.auto=0` disables auto-gc even when the pack limit is exceeded.
    let work3 = tempfile::tempdir().expect("work dir");
    let g3 = repo_with_two_packs(work3.path());
    run_libra_command(&["config", "set", "gc.autoPackLimit", "1"], &g3);
    run_libra_command(&["config", "set", "gc.auto", "0"], &g3);
    let before = libra_dir_snapshot(&g3);
    assert_cli_success(&run_libra_command(&["gc", "--auto"], &g3), "AC3: gc --auto");
    assert_snapshot_unchanged(&before, &libra_dir_snapshot(&g3), "AC3");

    // AC4: a non-positive pack limit never triggers by pack count.
    let work4 = tempfile::tempdir().expect("work dir");
    let g4 = repo_with_two_packs(work4.path());
    run_libra_command(&["config", "set", "gc.autoPackLimit", "0"], &g4);
    let before = libra_dir_snapshot(&g4);
    assert_cli_success(&run_libra_command(&["gc", "--auto"], &g4), "AC4: gc --auto");
    assert_snapshot_unchanged(&before, &libra_dir_snapshot(&g4), "AC4");

    // AC5: a `.keep`-pinned pack does not count towards the limit.
    let work5 = tempfile::tempdir().expect("work dir");
    let g5 = repo_with_two_packs(work5.path());
    run_libra_command(&["config", "set", "gc.autoPackLimit", "1"], &g5);
    let pack = fs::read_dir(g5.join(".libra/objects/pack"))
        .expect("read pack dir")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|e| e.to_str()) == Some("pack"))
        .expect("a pack file");
    fs::write(pack.with_extension("keep"), b"").expect("write .keep");
    let before = libra_dir_snapshot(&g5);
    assert_cli_success(&run_libra_command(&["gc", "--auto"], &g5), "AC5: gc --auto");
    assert_snapshot_unchanged(&before, &libra_dir_snapshot(&g5), "AC5");

    // AC6: exactly 50 non-keep packs do not exceed the default limit of 50.
    let work6 = tempfile::tempdir().expect("work dir");
    let g6 = work6.path().join("g");
    fs::create_dir_all(&g6).expect("mkdir g6");
    assert_cli_success(&run_libra_command(&["init"], &g6), "init g6");
    let pack_dir = g6.join(".libra/objects/pack");
    fs::create_dir_all(&pack_dir).expect("mkdir pack dir");
    for index in 0..50 {
        fs::write(pack_dir.join(format!("pack-{index:02}.pack")), b"stub").expect("write pack");
    }
    let before = libra_dir_snapshot(&g6);
    assert_cli_success(&run_libra_command(&["gc", "--auto"], &g6), "AC6: gc --auto");
    assert_snapshot_unchanged(&before, &libra_dir_snapshot(&g6), "AC6");
}
