use super::*;

fn dirty_context_repo(fixture: &Fixture, name: &str) -> PathBuf {
    let repo = fixture.path(name);
    fixture.init_repo(&repo);
    let body = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n";
    fixture.commit_file(&repo, "ctx.txt", body, "base");
    fs::write(repo.join("ctx.txt"), body.replace("l5", "L5")).expect("modify context file");
    repo
}

fn staged_rename_repo(fixture: &Fixture, name: &str) -> PathBuf {
    let repo = fixture.path(name);
    fixture.init_repo(&repo);
    let body = "alpha\nbeta\ngamma\ndelta\nepsilon\n";
    fixture.commit_file(&repo, "old-name.txt", body, "base");
    fs::rename(repo.join("old-name.txt"), repo.join("new-name.txt")).expect("rename file");
    fixture.success(&repo, &["add", "-A"]);
    repo
}

fn assert_no_progress(output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("Scanning working tree"), "{stderr}");
}

#[test]
fn diff_context_config_cascades_accepts_git_integers_and_cli_wins() {
    let fixture = Fixture::new();
    let repo = dirty_context_repo(&fixture, "diff-context-cascade");

    fixture.success(&repo, &["config", "--system", "diff.Context", "1k"]);
    let system = stdout_trim(&fixture.success(&repo, &["diff"]));
    assert!(system.contains(" l1"), "system 1k context: {system}");

    fixture.success(&repo, &["config", "--global", "diff.context", "1"]);
    let global = stdout_trim(&fixture.success(&repo, &["diff"]));
    assert!(global.contains(" l4"), "global context 1: {global}");
    assert!(
        !global.contains(" l3"),
        "global must override system: {global}"
    );

    fixture.success(&repo, &["config", "diff.context", "0"]);
    let local_zero = stdout_trim(&fixture.success(&repo, &["diff"]));
    assert!(!local_zero.contains(" l4"), "zero context: {local_zero}");
    fixture.success(&repo, &["config", "diff.context", "2"]);
    let local = stdout_trim(&fixture.success(&repo, &["diff"]));
    assert!(local.contains(" l3"), "local context 2: {local}");
    assert!(
        !local.contains(" l2"),
        "local must override global: {local}"
    );

    fixture.success(&repo, &["config", "diff.context", "2147483647"]);
    let int_max = stdout_trim(&fixture.success(&repo, &["diff"]));
    assert!(int_max.contains(" l1"), "Git int max context: {int_max}");
    fixture.success(&repo, &["config", "diff.context", "2097151k"]);
    let suffixed_int_max = stdout_trim(&fixture.success(&repo, &["diff"]));
    assert!(
        suffixed_int_max.contains(" l1"),
        "largest in-range k suffix: {suffixed_int_max}"
    );
    fixture.success(&repo, &["config", "diff.context", "2"]);

    let cli = stdout_trim(&fixture.success(&repo, &["diff", "-U3"]));
    assert!(cli.contains(" l2"), "-U3 must override config: {cli}");
}

#[test]
fn diff_context_errors_fail_before_progress_with_stable_codes() {
    let fixture = Fixture::new();
    let repo = dirty_context_repo(&fixture, "diff-context-errors");

    for invalid in [
        "-1",
        "abc",
        "2147483648",
        "2097152k",
        "9223372036854775807g",
    ] {
        fixture.success(&repo, &["config", "--", "diff.context", invalid]);
        let rejected = fixture.run(&repo, &["diff"]);
        assert_eq!(rejected.status.code(), Some(129), "value: {invalid}");
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
        assert!(stderr.contains("diff.context"), "{stderr}");
        assert!(rejected.stdout.is_empty());
        assert_no_progress(&rejected);
        fixture.success(&repo, &["config", "--unset", "diff.context"]);
    }

    fs::create_dir_all(fixture.home.join(".libra").join("config.db"))
        .expect("create unreadable global config path");
    let unreadable = fixture.run(&repo, &["diff"]);
    assert_eq!(unreadable.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&unreadable.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("diff.context"), "{stderr}");
    assert!(unreadable.stdout.is_empty());
    assert_no_progress(&unreadable);
}

#[test]
fn diff_renames_defaults_to_git_and_config_cascades_with_cli_precedence() {
    let fixture = Fixture::new();
    let repo = staged_rename_repo(&fixture, "diff-renames-cascade");

    let default = stdout_trim(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(
        default.contains("rename from old-name.txt"),
        "Git default: {default}"
    );

    fixture.success(&repo, &["config", "--system", "diff.ReNames", "false"]);
    let system = stdout_trim(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(!system.contains("rename from"), "system false: {system}");

    fixture.success(&repo, &["config", "--global", "diff.renames", "true"]);
    let global = stdout_trim(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(
        global.contains("rename from old-name.txt"),
        "global true: {global}"
    );

    fixture.success(&repo, &["config", "diff.renames", "false"]);
    let local = stdout_trim(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(!local.contains("rename from"), "local false: {local}");
    let cli_on = stdout_trim(&fixture.success(&repo, &["diff", "--staged", "-M"]));
    assert!(
        cli_on.contains("rename from old-name.txt"),
        "-M wins: {cli_on}"
    );

    fixture.success(&repo, &["config", "diff.renames", "copies"]);
    let copies = stdout_trim(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(
        copies.contains("rename from old-name.txt"),
        "copies degrades to rename detection: {copies}"
    );
    fixture.success(&repo, &["config", "diff.renames", "copy"]);
    let copy = stdout_trim(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(
        copy.contains("rename from old-name.txt"),
        "copy degrades to rename detection: {copy}"
    );
    let cli_off = stdout_trim(&fixture.success(&repo, &["diff", "--staged", "--no-renames"]));
    assert!(
        !cli_off.contains("rename from"),
        "--no-renames wins: {cli_off}"
    );
}

#[test]
fn diff_renames_errors_fail_before_progress_with_stable_codes() {
    let fixture = Fixture::new();
    let repo = staged_rename_repo(&fixture, "diff-renames-errors");

    for invalid in ["", "sideways"] {
        fixture.success(&repo, &["config", "diff.renames", invalid]);
        let rejected = fixture.run(&repo, &["diff"]);
        assert_eq!(rejected.status.code(), Some(129), "value: {invalid:?}");
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
        assert!(stderr.contains("diff.renames"), "{stderr}");
        assert!(rejected.stdout.is_empty());
        assert_no_progress(&rejected);
        fixture.success(&repo, &["config", "--unset", "diff.renames"]);
    }

    fixture.success(&repo, &["config", "diff.context", "3"]);
    fs::create_dir_all(fixture.home.join(".libra").join("config.db"))
        .expect("create unreadable global config path");
    let unreadable = fixture.run(&repo, &["diff"]);
    assert_eq!(unreadable.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&unreadable.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("diff.renames"), "{stderr}");
    assert!(unreadable.stdout.is_empty());
    assert_no_progress(&unreadable);
}

/// plan-20260714 R0-1: `diff.renameLimit` cascades (status>diff comes later
/// with `status.renameLimit`); `0` uncaps; invalid values fail closed before
/// progress.
#[test]
fn diff_rename_limit_config() {
    let fixture = Fixture::new();
    let repo = fixture.path("diff-rename-limit");
    fixture.init_repo(&repo);
    // Three renamed files with *distinct* content and different basenames so
    // only the inexact pass can pair them.
    let bodies = [
        "alpha\nbravo\ncharlie\ndelta\necho\nfoxtrot\n",
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\n",
        "red\ngreen\nblue\ncyan\nmagenta\nyellow\n",
    ];
    for (i, body) in bodies.iter().enumerate() {
        fixture.commit_file(&repo, &format!("src{i}.txt"), body, &format!("base{i}"));
    }
    for (i, body) in bodies.iter().enumerate() {
        fs::remove_file(repo.join(format!("src{i}.txt"))).expect("remove source");
        let tweaked = body.replacen('\n', "!\n", 1);
        fs::write(repo.join(format!("dst{i}.log")), tweaked).expect("write dest");
    }
    fixture.success(&repo, &["add", "-A"]);

    // limit=2 < 3 sources: inexact skipped per side, no rename rows.
    fixture.success(&repo, &["config", "diff.renameLimit", "2"]);
    let capped = fixture.success(&repo, &["diff", "--staged", "-M40%"]);
    let capped_out = stdout_trim(&capped);
    assert!(
        !capped_out.contains("rename from"),
        "limit=2 must skip inexact: {capped_out}"
    );
    let stderr = String::from_utf8_lossy(&capped.stderr);
    assert!(
        stderr.contains("diff.renameLimit"),
        "limit skip warns with config name: {stderr}"
    );

    // limit=0 uncaps: all three pairs found.
    fixture.success(&repo, &["config", "diff.renameLimit", "0"]);
    let uncapped = stdout_trim(&fixture.success(&repo, &["diff", "--staged", "-M40%"]));
    for i in 0..3 {
        assert!(
            uncapped.contains(&format!("rename from src{i}.txt")),
            "limit=0 must pair src{i}: {uncapped}"
        );
    }

    // Invalid value fails closed with a stable code before any diff output.
    fixture.success(&repo, &["config", "--", "diff.renameLimit", "-1"]);
    let rejected = fixture.run(&repo, &["diff", "--staged", "-M40%"]);
    assert_eq!(rejected.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("diff.renameLimit"), "{stderr}");
    assert!(rejected.stdout.is_empty());
    fixture.success(&repo, &["config", "--unset", "diff.renameLimit"]);
}

/// plan-20260714 R0-1: `diff` and `status` must apply the SAME inexact
/// eligibility rule — non-empty regular files only. `diff` built every
/// `BlobRef` with `size: None`, so the shared engine's empty-file gate
/// (`size != Some(0)`) never fired on this side: an empty blob reached
/// content scoring, spent a comparison, and under a small
/// `diff.renameComparisonBudget` could exhaust it before a real inexact
/// rename was ever scored. Same repository, two different answers.
#[test]
fn diff_empty_blob_does_not_consume_the_comparison_budget() {
    let fixture = Fixture::new();
    let repo = fixture.path("diff-empty-blob-budget");
    fixture.init_repo(&repo);
    // `init` leaves `.libraignore` untracked; commit it so the candidate set
    // below is exactly what this test builds and the comparison count is
    // deterministic.
    fixture.success(&repo, &["add", ".libraignore"]);
    fixture.success(&repo, &["commit", "-m", "track the ignore file"]);
    // One empty file and one genuine inexact candidate.
    fixture.commit_file(&repo, "empty.txt", "", "empty base");
    fixture.commit_file(
        &repo,
        "near.txt",
        "alpha1\nalpha2\nalpha3\nalpha4\nalpha5\n",
        "near base",
    );
    // The empty file is REPLACED by non-empty content at a new path, so it
    // can only ever pair inexactly — an exact OID match is impossible.
    fs::remove_file(repo.join("empty.txt")).expect("rm empty");
    fs::write(repo.join("was-empty.log"), "something entirely new\n").expect("write dest");
    fs::remove_file(repo.join("near.txt")).expect("rm near");
    fs::write(
        repo.join("moved-near.log"),
        "alpha1\nalpha2\nalpha3\nalpha4\nCHANGED\n",
    )
    .expect("write near dest");
    fixture.success(&repo, &["add", "-A"]);

    // Two deleted (empty.txt, near.txt) and two added (was-empty.log,
    // moved-near.log). With the empty source correctly ineligible that is
    // 1 x 2 = 2 comparisons; with it eligible it is 2 x 2 = 4. A budget of 2
    // is therefore exactly enough for the correct behaviour and one short of
    // the broken one, which discards every scored inexact edge.
    fixture.success(&repo, &["config", "diff.renameComparisonBudget", "2"]);
    let out = stdout_trim(&fixture.success(&repo, &["diff", "--staged", "-M40%"]));
    assert!(
        out.contains("rename from near.txt"),
        "the real inexact rename keeps the budget it deserves: {out}"
    );
    assert!(
        !out.contains("rename from empty.txt"),
        "and an empty source never pairs inexactly, matching status: {out}"
    );
}

/// plan-20260714 R0-1: `diff.renameComparisonBudget` parses (0 → unlimited,
/// invalid fail-closed) and exhaustion discards inexact candidates with a
/// warning while exact renames survive.
#[test]
fn diff_rename_comparison_budget_config() {
    let fixture = Fixture::new();
    let repo = fixture.path("diff-rename-budget");
    fixture.init_repo(&repo);
    fixture.commit_file(
        &repo,
        "exact.txt",
        "exact content stays identical\n",
        "exact base",
    );
    // Two inexact candidates so budget=1 deterministically exhausts on the
    // second comparison regardless of iteration order, and the discard rule
    // (drop ALL scored inexact edges, keep exact) is observable.
    fixture.commit_file(
        &repo,
        "near-a.txt",
        "alpha1\nalpha2\nalpha3\nalpha4\nalpha5\n",
        "near-a base",
    );
    fixture.commit_file(
        &repo,
        "near-b.txt",
        "beta1\nbeta2\nbeta3\nbeta4\nbeta5\n",
        "near-b base",
    );
    fs::rename(repo.join("exact.txt"), repo.join("exact-moved.txt")).expect("mv exact");
    for (src, dst, body) in [
        (
            "near-a.txt",
            "moved-a.log",
            "alpha1\nalpha2\nalpha3\nalpha4\nCHANGED\n",
        ),
        (
            "near-b.txt",
            "moved-b.log",
            "beta1\nbeta2\nbeta3\nbeta4\nCHANGED\n",
        ),
    ] {
        fs::remove_file(repo.join(src)).expect("rm near source");
        fs::write(repo.join(dst), body).expect("write near dest");
    }
    fixture.success(&repo, &["add", "-A"]);

    // budget=1: at most one inexact comparison is allowed; the second one
    // exhausts the budget, discarding every scored inexact candidate. Only
    // the exact rename survives.
    fixture.success(&repo, &["config", "diff.renameComparisonBudget", "1"]);
    let budgeted = fixture.success(&repo, &["diff", "--staged", "-M40%"]);
    let budgeted_out = stdout_trim(&budgeted);
    assert!(
        budgeted_out.contains("rename from exact.txt"),
        "exact rename must survive budget exhaustion: {budgeted_out}"
    );
    assert!(
        !budgeted_out.contains("rename from near-a.txt")
            && !budgeted_out.contains("rename from near-b.txt"),
        "all inexact candidates must be discarded on budget exhaustion: {budgeted_out}"
    );
    let stderr = String::from_utf8_lossy(&budgeted.stderr);
    assert!(
        stderr.contains("diff.renameComparisonBudget"),
        "budget discard warns: {stderr}"
    );

    // budget=0 → unlimited (same as unset).
    fixture.success(&repo, &["config", "diff.renameComparisonBudget", "0"]);
    let unlimited = stdout_trim(&fixture.success(&repo, &["diff", "--staged", "-M40%"]));
    assert!(
        unlimited.contains("rename from near-a.txt")
            && unlimited.contains("rename from near-b.txt"),
        "budget=0 keeps inexact detection: {unlimited}"
    );

    // Invalid value fails closed.
    fixture.success(
        &repo,
        &["config", "--", "diff.renameComparisonBudget", "abc"],
    );
    let rejected = fixture.run(&repo, &["diff", "--staged", "-M40%"]);
    assert_eq!(rejected.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("diff.renameComparisonBudget"), "{stderr}");
    fixture.success(&repo, &["config", "--unset", "diff.renameComparisonBudget"]);
}

/// plan-20260714 §B.7: without configuration, diff has NO comparison budget —
/// a candidate set larger than status's 500k comparison ceiling must still
/// pair. 710 × 710 = 504,100 inexact comparisons: if a default budget of
/// `Some(500_000)` (or anything smaller) were ever wired into diff, the
/// discard rule would strip these renames and this test would fail. Stays
/// under the default `diff.renameLimit` (1000 per side) so only the budget
/// semantics are exercised.
#[test]
fn diff_no_comparison_budget_regression() {
    const N: usize = 710;
    let fixture = Fixture::new();
    let repo = fixture.path("diff-no-budget");
    fixture.init_repo(&repo);
    for i in 0..N {
        fs::write(
            repo.join(format!("f{i:03}.txt")),
            format!("file {i}\nshared line\nunique {i} {i}\n"),
        )
        .expect("write source");
    }
    fixture.success(&repo, &["add", "-A"]);
    fixture.success(&repo, &["commit", "-m", "base", "--no-verify"]);
    for i in 0..N {
        fs::remove_file(repo.join(format!("f{i:03}.txt"))).expect("remove source");
        // Change one line so the exact (identical-OID) pass cannot pair the
        // files: every pairing below MUST come from inexact scoring.
        fs::write(
            repo.join(format!("g{i:03}.txt")),
            format!("file {i} CHANGED\nshared line\nunique {i} {i}\n"),
        )
        .expect("write inexact dest");
    }
    fixture.success(&repo, &["add", "-A"]);

    let output = fixture.success(&repo, &["diff", "--staged", "-M40%"]);
    let stdout = stdout_trim(&output);
    assert!(
        stdout.contains("rename from f000.txt")
            && stdout.contains(&format!("rename from f{:03}.txt", N - 1)),
        "all inexact renames must pair without a default budget"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("renameComparisonBudget"),
        "no budget warning without configuration: {stderr}"
    );
}

/// plan-20260714 R0-1 §B.4.2.3: a tripped `diff.renameLimit` skips ONLY
/// the exhaustive inexact stage — exact pairs AND unique-basename pairs
/// still report as renames instead of degrading to delete + add.
#[test]
fn rename_limit_degradation_keeps_unique_basename_pairs() {
    let fixture = Fixture::new();
    let repo = fixture.path("diff-rename-limit-basename");
    fixture.init_repo(&repo);
    // `src/keep.txt` → `dst/keep.txt` shares a BASENAME but its content
    // changed enough that only the basename pass can pair it; the noise
    // files push both sides past renameLimit=1 so the exhaustive stage
    // is skipped.
    fs::create_dir_all(repo.join("src")).expect("create src");
    fixture.commit_file(
        &repo,
        "src/keep.txt",
        "alpha\nbravo\ncharlie\ndelta\n",
        "base keep",
    );
    for index in 0..3 {
        fixture.commit_file(
            &repo,
            &format!("src/noise{index}.txt"),
            &format!("noise {index}\nmore {index}\n"),
            &format!("base noise{index}"),
        );
    }
    fs::remove_file(repo.join("src/keep.txt")).expect("remove keep");
    fs::create_dir_all(repo.join("dst")).expect("create dst");
    fs::write(repo.join("dst/keep.txt"), "alpha\nbravo\ncharlie\nomega\n")
        .expect("write keep dest");
    for index in 0..3 {
        fs::remove_file(repo.join(format!("src/noise{index}.txt"))).expect("remove noise");
        fs::write(
            repo.join(format!("dst/other{index}.txt")),
            format!("entirely unrelated payload {index}\n"),
        )
        .expect("write noise dest");
    }
    fixture.success(&repo, &["add", "-A"]);

    fixture.success(&repo, &["config", "diff.renameLimit", "1"]);
    let out = fixture.success(&repo, &["diff", "--staged", "-M40%"]);
    let text = stdout_trim(&out);
    assert!(
        text.contains("rename from src/keep.txt") && text.contains("rename to dst/keep.txt"),
        "the basename-proven rename survives a tripped renameLimit: {text}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("diff.renameLimit"),
        "the degradation still warns: {stderr}"
    );
    // The warning must describe what ACTUALLY survived. Saying "exact
    // renames were still detected" while a unique-basename pair is sitting
    // in the output above sends readers hunting for a bug that is not
    // there — and contradicts the docs, COMPATIBILITY and the CHANGELOG.
    assert!(
        stderr.contains("unique-basename"),
        "the warning must name unique-basename pairs as surviving, since one \
         is present in this very diff: {stderr}"
    );
    fixture.success(&repo, &["config", "--unset", "diff.renameLimit"]);

    // Same claim for the OTHER degradation: the comparison budget discards
    // the exhaustive pass, and the basename pair survives that too.
    fixture.success(&repo, &["config", "diff.renameComparisonBudget", "1"]);
    let out = fixture.success(&repo, &["diff", "--staged", "-M40%"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("diff.renameComparisonBudget") {
        assert!(
            stderr.contains("unique-basename"),
            "the budget warning must name unique-basename pairs as surviving: {stderr}"
        );
        assert!(
            stdout_trim(&out).contains("rename to dst/keep.txt"),
            "and the pair really is still reported: {}",
            stdout_trim(&out)
        );
    }
    fixture.success(&repo, &["config", "--unset", "diff.renameComparisonBudget"]);
}
