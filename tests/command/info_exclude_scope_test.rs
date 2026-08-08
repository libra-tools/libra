//! W0 §C.4.1.1 (plan-20260714 line 2262): `info/exclude` and
//! `info/attributes` are part of the WORKTREE VIEW — each worktree reads its
//! OWN local gitdir's `info/*`, never another scope's via `commondir`.
//!
//! Before W0 the `.libra` layout never read these files at all (only a
//! literal `.git` marker was recognized), so the `.libra/info/exclude` that
//! `libra init` itself creates was dead code — these tests are the liveness
//! proof as much as the scoping proof.
//!
//! Layer: L1 (deterministic; tempdir, no network).

use std::fs;

use super::{assert_cli_success, run_libra_command};

fn committed_repo() -> tempfile::TempDir {
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
    repo
}

fn status_short(dir: &std::path::Path) -> String {
    let out = run_libra_command(&["status", "--porcelain"], dir);
    assert_cli_success(&out, "status --porcelain");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `.libra/info/exclude` in the MAIN worktree hides matching untracked files
/// from status, and `check-ignore` reports them ignored.
#[test]
fn main_dot_libra_info_exclude_is_honored() {
    let repo = committed_repo();
    let p = repo.path();

    fs::write(p.join("hidden.tmp"), b"x").unwrap();
    assert!(
        status_short(p).contains("hidden.tmp"),
        "before the exclude entry the file must be visible"
    );

    let info_dir = p.join(".libra").join("info");
    fs::create_dir_all(&info_dir).unwrap();
    fs::write(info_dir.join("exclude"), b"hidden.tmp\n").unwrap();

    assert!(
        !status_short(p).contains("hidden.tmp"),
        ".libra/info/exclude must hide the file from status"
    );
    let check = run_libra_command(&["check-ignore", "hidden.tmp"], p);
    assert!(
        check.status.success(),
        "check-ignore must report the info/exclude match (exit 0): {}",
        String::from_utf8_lossy(&check.stderr)
    );
}

/// Each worktree reads its OWN `info/exclude`: main's entries do not leak
/// into a linked worktree and vice versa (plan line 2262 — worktree view,
/// intentionally different from Git's commondir sharing).
#[test]
fn info_exclude_is_scoped_per_worktree() {
    let repo = committed_repo();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Main-only exclude entry.
    let main_info = main.join(".libra").join("info");
    fs::create_dir_all(&main_info).unwrap();
    fs::write(main_info.join("exclude"), b"mainonly.tmp\n").unwrap();
    fs::write(main.join("mainonly.tmp"), b"x").unwrap();
    fs::write(wt.join("mainonly.tmp"), b"x").unwrap();

    assert!(
        !status_short(main).contains("mainonly.tmp"),
        "main hides its own info/exclude match"
    );
    assert!(
        status_short(&wt).contains("mainonly.tmp"),
        "main's info/exclude must NOT leak into the linked worktree"
    );

    // Linked-only exclude entry, in the linked worktree's own local gitdir.
    let wt_info = wt.join(".libra").join("info");
    fs::create_dir_all(&wt_info).unwrap();
    fs::write(wt_info.join("exclude"), b"wtonly.tmp\n").unwrap();
    fs::write(wt.join("wtonly.tmp"), b"x").unwrap();
    fs::write(main.join("wtonly.tmp"), b"x").unwrap();

    assert!(
        !status_short(&wt).contains("wtonly.tmp"),
        "the linked worktree hides its own info/exclude match"
    );
    assert!(
        status_short(main).contains("wtonly.tmp"),
        "the linked worktree's info/exclude must NOT leak into main"
    );
}

/// `.libra/info/attributes` resolves the same way: alive in the `.libra`
/// layout and per-worktree.
#[test]
fn info_attributes_resolve_from_the_local_gitdir() {
    let repo = committed_repo();
    let main = repo.path();

    let info_dir = main.join(".libra").join("info");
    fs::create_dir_all(&info_dir).unwrap();
    fs::write(info_dir.join("attributes"), b"*.dat filter=lfs\n").unwrap();
    fs::write(main.join("f.dat"), b"x").unwrap();

    let out = run_libra_command(&["check-attr", "filter", "f.dat"], main);
    assert_cli_success(&out, "check-attr");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("lfs"),
        ".libra/info/attributes must be consulted: {stdout}"
    );

    // A linked worktree does not inherit main's info/attributes.
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    fs::write(wt.join("f.dat"), b"x").unwrap();
    let wt_out = run_libra_command(&["check-attr", "filter", "f.dat"], &wt);
    assert_cli_success(&wt_out, "check-attr in wt");
    let wt_stdout = String::from_utf8_lossy(&wt_out.stdout);
    assert!(
        wt_stdout.contains("unspecified"),
        "main's info/attributes must not leak into the linked worktree: {wt_stdout}"
    );
}

/// W0 §C.4.1.1 origin diagnostics: with linked worktrees present, `worktree
/// doctor` reports that common info files apply only to main and names the
/// explicit adopt/clear actions.
#[test]
fn doctor_reports_common_info_scope_and_actions() {
    let repo = committed_repo();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let info_dir = main.join(".libra").join("info");
    fs::create_dir_all(&info_dir).unwrap();
    fs::write(info_dir.join("exclude"), b"legacy.tmp\n").unwrap();

    let doctor = run_libra_command(&["worktree", "doctor"], main);
    assert_cli_success(&doctor, "worktree doctor");
    let stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(
        stdout.contains("apply ONLY to this main worktree"),
        "doctor must state the info files' scope: {stdout}"
    );
    assert!(
        stdout.contains("--adopt-info-to") && stdout.contains("--clear-common-info"),
        "doctor must name both explicit actions: {stdout}"
    );
    // §C.4.1.1 origin inventory: the per-worktree info-source listing names
    // the file the main worktree's engines actually read.
    assert!(
        stdout.contains("info sources") && stdout.contains(".libra/info/exclude"),
        "doctor must list each worktree's local info sources: {stdout}"
    );

    // Dual layout: a `.git/info/exclude` beside `.libra` is a live source
    // for the engines, so the inventory must list it too.
    let dual_git_info = main.join(".git").join("info");
    fs::create_dir_all(&dual_git_info).unwrap();
    fs::write(dual_git_info.join("exclude"), b"dual.tmp\n").unwrap();
    let dual = run_libra_command(&["worktree", "doctor"], main);
    assert_cli_success(&dual, "worktree doctor (dual layout)");
    let dual_stdout = String::from_utf8_lossy(&dual.stdout);
    assert!(
        dual_stdout.contains(".libra/info/exclude") && dual_stdout.contains(".git/info/exclude"),
        "a dual-layout tree reports BOTH live info sources: {dual_stdout}"
    );
}

/// The adopt action is confirmed, copies into ONE worktree's local gitdir,
/// and never overwrites the destination's own view.
#[test]
fn doctor_adopt_info_requires_confirm_and_copies_once() {
    let repo = committed_repo();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let info_dir = main.join(".libra").join("info");
    fs::create_dir_all(&info_dir).unwrap();
    fs::write(info_dir.join("exclude"), b"legacy.tmp\n").unwrap();
    let wt_str = wt.to_str().unwrap();

    let unconfirmed = run_libra_command(&["worktree", "doctor", "--adopt-info-to", wt_str], main);
    assert!(
        !unconfirmed.status.success(),
        "adopt without --confirm must refuse"
    );
    assert!(
        !wt.join(".libra/info/exclude").exists(),
        "the refusal must precede every side effect"
    );

    let adopt = run_libra_command(
        &["worktree", "doctor", "--adopt-info-to", wt_str, "--confirm"],
        main,
    );
    assert_cli_success(&adopt, "confirmed adopt");
    assert_eq!(
        fs::read(wt.join(".libra/info/exclude")).expect("adopted file"),
        b"legacy.tmp\n",
        "the common file is copied into the linked worktree's local gitdir"
    );

    // A second adopt must keep the (possibly edited) local view.
    fs::write(wt.join(".libra/info/exclude"), b"mine.tmp\n").unwrap();
    let again = run_libra_command(
        &["worktree", "doctor", "--adopt-info-to", wt_str, "--confirm"],
        main,
    );
    assert_cli_success(&again, "re-adopt");
    assert_eq!(
        fs::read(wt.join(".libra/info/exclude")).expect("kept file"),
        b"mine.tmp\n",
        "an existing destination is never overwritten"
    );
}

/// A comments-only info file (what `libra init` writes) matches nothing, so
/// it is neither a reported source nor grounds for adoption advice — the
/// plan's origin-inventory contract is about files with EFFECTIVE rules.
#[test]
fn doctor_ignores_comment_only_info_files() {
    let repo = committed_repo();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let info_dir = main.join(".libra").join("info");
    fs::create_dir_all(&info_dir).unwrap();
    fs::write(
        info_dir.join("exclude"),
        b"# only comments\n\n#  and blank lines\n",
    )
    .unwrap();

    let doctor = run_libra_command(&["worktree", "doctor"], main);
    assert_cli_success(&doctor, "worktree doctor");
    let stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(
        !stdout.contains("info sources"),
        "a comments-only file is not a live info source: {stdout}"
    );
    assert!(
        !stdout.contains("--adopt-info-to"),
        "a comments-only file must not trigger adoption advice: {stdout}"
    );

    // One effective rule flips both.
    fs::write(info_dir.join("exclude"), b"# comment\nreal.tmp\n").unwrap();
    let again = run_libra_command(&["worktree", "doctor"], main);
    assert_cli_success(&again, "worktree doctor (effective rule)");
    let again_stdout = String::from_utf8_lossy(&again.stdout);
    assert!(
        again_stdout.contains("info sources") && again_stdout.contains("--adopt-info-to"),
        "an effective rule IS reported and DOES trigger advice: {again_stdout}"
    );
}

/// "Effective" is decided by the ENGINES' parsers, not by line shape. Two
/// cases a heuristic gets wrong in OPPOSITE directions:
/// - an INDENTED `#…` line is a real gitignore pattern (a "looks like a
///   comment" test would hide a live source);
/// - an attributes line with a pattern but NO assignment parses to nothing
///   (a "non-comment line" test would report a source the engine ignores).
#[test]
fn doctor_effective_rule_detection_matches_engine_parsers() {
    let repo = committed_repo();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let info_dir = main.join(".libra").join("info");
    fs::create_dir_all(&info_dir).unwrap();

    // False-NEGATIVE guard: an indented `#…` IS a pattern.
    fs::write(info_dir.join("exclude"), b"  #indented.tmp\n").unwrap();
    fs::write(info_dir.join("attributes"), b"*.dat\n").unwrap();
    let out = run_libra_command(&["worktree", "doctor"], main);
    assert_cli_success(&out, "worktree doctor");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("info/exclude"),
        "an indented '#' line is a real gitignore pattern, so the file IS a \
         live source: {stdout}"
    );
    // False-POSITIVE guard: `*.dat` with no assignment is not a rule.
    assert!(
        !stdout.contains("info/attributes"),
        "an attributes line with no assignment yields no rule, so the file is \
         NOT a live source: {stdout}"
    );

    // Give the attributes file a real assignment: now it is a source.
    fs::write(info_dir.join("attributes"), b"*.dat filter=lfs\n").unwrap();
    let with_rule = run_libra_command(&["worktree", "doctor"], main);
    assert_cli_success(&with_rule, "worktree doctor (attributes rule)");
    assert!(
        String::from_utf8_lossy(&with_rule.stdout).contains("info/attributes"),
        "a real attribute assignment IS a live source"
    );
}

/// The two mutations carry their own machine envelopes
/// (`worktree.doctor.adopt_info` / `worktree.doctor.clear_common_info`),
/// leaving the frozen read-only `worktree.doctor` page schema untouched.
#[test]
fn doctor_info_mutations_emit_json_envelopes() {
    let repo = committed_repo();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let info_dir = main.join(".libra").join("info");
    fs::create_dir_all(&info_dir).unwrap();
    fs::write(info_dir.join("exclude"), b"legacy.tmp\n").unwrap();
    let wt_str = wt.to_str().unwrap();

    let adopt = run_libra_command(
        &[
            "--json",
            "worktree",
            "doctor",
            "--adopt-info-to",
            wt_str,
            "--confirm",
        ],
        main,
    );
    assert_cli_success(&adopt, "json adopt");
    let adopt_doc: serde_json::Value =
        serde_json::from_slice(&adopt.stdout).expect("adopt output parses as JSON");
    assert_eq!(adopt_doc["command"], "worktree.doctor.adopt_info");
    assert_eq!(adopt_doc["data"]["target"], wt_str);
    assert!(
        adopt_doc["data"]["report"]
            .as_str()
            .is_some_and(|report| report.contains("adopted")),
        "the adopt envelope reports what happened: {adopt_doc}"
    );

    let clear = run_libra_command(
        &[
            "--json",
            "worktree",
            "doctor",
            "--clear-common-info",
            "--confirm",
        ],
        main,
    );
    assert_cli_success(&clear, "json clear");
    let clear_doc: serde_json::Value =
        serde_json::from_slice(&clear.stdout).expect("clear output parses as JSON");
    assert_eq!(clear_doc["command"], "worktree.doctor.clear_common_info");
    assert!(
        clear_doc["data"]["report"]
            .as_str()
            .is_some_and(|report| report.contains("cleared")),
        "the clear envelope reports what happened: {clear_doc}"
    );
}

/// The clear action is confirmed and removes the common info files.
#[test]
fn doctor_clear_common_info_requires_confirm_and_deletes() {
    let repo = committed_repo();
    let main = repo.path();
    let info_dir = main.join(".libra").join("info");
    fs::create_dir_all(&info_dir).unwrap();
    fs::write(info_dir.join("exclude"), b"legacy.tmp\n").unwrap();
    fs::write(info_dir.join("attributes"), b"*.dat filter=lfs\n").unwrap();

    let unconfirmed = run_libra_command(&["worktree", "doctor", "--clear-common-info"], main);
    assert!(
        !unconfirmed.status.success(),
        "clear without --confirm must refuse"
    );
    assert!(info_dir.join("exclude").exists(), "nothing removed yet");

    let clear = run_libra_command(
        &["worktree", "doctor", "--clear-common-info", "--confirm"],
        main,
    );
    assert_cli_success(&clear, "confirmed clear");
    assert!(!info_dir.join("exclude").exists(), "exclude removed");
    assert!(!info_dir.join("attributes").exists(), "attributes removed");
}

/// W0 §C.4.1.1 (plan line 2258, 2026-08-06 revision): `worktree add` probes
/// the TARGET filesystem's case behavior and warns when it disagrees with
/// the repository's persisted `core.ignorecase`. Exactly one of the two
/// opposite explicit settings must mismatch this machine's filesystem.
#[test]
fn worktree_add_warns_on_case_probe_mismatch() {
    let repo = committed_repo();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");

    let mut warnings = 0;
    for (value, name) in [("false", "wt-false"), ("true", "wt-true")] {
        assert_cli_success(
            &run_libra_command(&["config", "core.ignorecase", value], main),
            "set ignorecase",
        );
        let wt = parent.path().join(name);
        let add = run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main);
        assert_cli_success(&add, "worktree add");
        if String::from_utf8_lossy(&add.stderr).contains("case-collision guards") {
            warnings += 1;
        }
    }
    assert_eq!(
        warnings, 1,
        "exactly one of the two opposite persisted values must mismatch the \
         target filesystem's probe (0 = warning never fires, 2 = it fires \
         unconditionally)"
    );
}

/// A failed adopt is failure-atomic: no partial file at the final name and
/// no temp residue — a retry then succeeds instead of mis-classifying a
/// partial file as "already exists".
#[cfg(unix)]
#[test]
fn doctor_adopt_info_failure_leaves_no_partial_destination() {
    use std::os::unix::fs::PermissionsExt;

    let repo = committed_repo();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let info_dir = main.join(".libra").join("info");
    fs::create_dir_all(&info_dir).unwrap();
    fs::write(info_dir.join("exclude"), b"legacy.tmp\n").unwrap();
    let wt_str = wt.to_str().unwrap();

    // Make the destination info dir unwritable so the copy fails.
    let wt_info = wt.join(".libra").join("info");
    fs::create_dir_all(&wt_info).unwrap();
    let writable = fs::metadata(&wt_info).unwrap().permissions();
    fs::set_permissions(&wt_info, fs::Permissions::from_mode(0o555)).unwrap();
    // Root is immune to permission bits — the failure cannot be staged.
    if fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(wt_info.join(".probe"))
        .is_ok()
    {
        fs::set_permissions(&wt_info, writable).unwrap();
        let _ = fs::remove_file(wt_info.join(".probe"));
        eprintln!("skipped (running as root: permission bits cannot stage the write failure)");
        return;
    }

    let failed = run_libra_command(
        &["worktree", "doctor", "--adopt-info-to", wt_str, "--confirm"],
        main,
    );
    fs::set_permissions(&wt_info, writable).unwrap();
    assert!(
        !failed.status.success(),
        "an unwritable destination must fail the adopt"
    );
    assert!(
        !wt_info.join("exclude").exists(),
        "no partial file may sit at the final name after a failure"
    );
    let residue: Vec<_> = fs::read_dir(&wt_info)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        residue.is_empty(),
        "no temp residue may survive a failed adopt: {residue:?}"
    );

    // The retry now succeeds — the failure did not poison the destination.
    let retry = run_libra_command(
        &["worktree", "doctor", "--adopt-info-to", wt_str, "--confirm"],
        main,
    );
    assert_cli_success(&retry, "retry after failure");
    assert_eq!(
        fs::read(wt_info.join("exclude")).expect("adopted on retry"),
        b"legacy.tmp\n"
    );
}
