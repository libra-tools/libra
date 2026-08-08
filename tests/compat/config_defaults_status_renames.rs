//! `status.renames` / `status.renameLimit` cascade guards
//! (plan-20260714 R0-7, §B.5).

use super::*;

/// A committed file moved and lightly edited so detection must run the
/// inexact stage (a pure exact pair would mask threshold/limit semantics).
fn staged_inexact_rename_repo(fixture: &Fixture, name: &str) -> PathBuf {
    let repo = fixture.path(name);
    fixture.init_repo(&repo);
    let body: String = (0..40).map(|i| format!("line {i}\n")).collect();
    fixture.commit_file(&repo, "old-name.txt", &body, "base");
    fs::rename(repo.join("old-name.txt"), repo.join("new-name.txt")).expect("rename file");
    fs::write(
        repo.join("new-name.txt"),
        body.replace("line 5\n", "line five\n"),
    )
    .expect("edit renamed file");
    fixture.success(&repo, &["add", "-A"]);
    repo
}

fn status_reports_rename(fixture: &Fixture, repo: &Path) -> bool {
    let out = fixture.success(repo, &["status"]);
    String::from_utf8_lossy(&out.stdout).contains("renamed:")
}

/// `status.renames` cascades local→global→system with local winning, and a
/// `false` at the winning scope splits the rename into delete + add.
#[test]
fn status_renames_cascade_local_beats_global_beats_system() {
    let fixture = Fixture::new();

    // System scope alone disables detection.
    fixture.success(
        Path::new("/"),
        &["config", "--system", "status.renames", "false"],
    );
    let repo = staged_inexact_rename_repo(&fixture, "renames-system");
    assert!(
        !status_reports_rename(&fixture, &repo),
        "system-scope false must disable detection"
    );

    // Global true beats system false.
    fixture.success(
        Path::new("/"),
        &["config", "--global", "status.renames", "true"],
    );
    assert!(
        status_reports_rename(&fixture, &repo),
        "global true beats system false"
    );

    // Local false beats global true.
    fixture.success(&repo, &["config", "status.renames", "false"]);
    assert!(
        !status_reports_rename(&fixture, &repo),
        "local false beats global true"
    );

    // The CLI flag beats every scope.
    let out = fixture.success(&repo, &["status", "--renames"]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("renamed:"),
        "--renames overrides the config cascade"
    );
}

/// `status.renames` falls back to `diff.renames` when unset.
#[test]
fn status_renames_falls_back_to_diff_renames() {
    let fixture = Fixture::new();
    let repo = staged_inexact_rename_repo(&fixture, "renames-fallback");
    fixture.success(&repo, &["config", "diff.renames", "false"]);
    assert!(
        !status_reports_rename(&fixture, &repo),
        "diff.renames=false is inherited"
    );
    fixture.success(&repo, &["config", "status.renames", "true"]);
    assert!(
        status_reports_rename(&fixture, &repo),
        "status.renames beats the diff fallback"
    );
}

/// The FALLBACK key has its own three-scope cascade: with `status.renames`
/// unset everywhere, `diff.renames` resolves local → global → system just
/// like the primary key, and an unsupported/invalid value at the winning
/// scope fails closed. Testing only a local `diff.renames` would miss a
/// fallback that consults the wrong scope.
#[test]
fn diff_renames_fallback_cascades_across_all_three_scopes() {
    let fixture = Fixture::new();

    // System scope alone drives the fallback.
    fixture.success(
        Path::new("/"),
        &["config", "--system", "diff.renames", "false"],
    );
    let repo = staged_inexact_rename_repo(&fixture, "diff-renames-cascade");
    assert!(
        !status_reports_rename(&fixture, &repo),
        "system-scope diff.renames=false is inherited by status"
    );

    // Global beats system.
    fixture.success(
        Path::new("/"),
        &["config", "--global", "diff.renames", "true"],
    );
    assert!(
        status_reports_rename(&fixture, &repo),
        "global diff.renames=true beats system false"
    );

    // Local beats global.
    fixture.success(&repo, &["config", "diff.renames", "false"]);
    assert!(
        !status_reports_rename(&fixture, &repo),
        "local diff.renames=false beats global true"
    );

    // A `status.renames` at ANY scope outranks every `diff.renames` scope.
    fixture.success(
        Path::new("/"),
        &["config", "--global", "status.renames", "true"],
    );
    assert!(
        status_reports_rename(&fixture, &repo),
        "global status.renames beats local diff.renames"
    );
    fixture.success(
        Path::new("/"),
        &["config", "--global", "--unset", "status.renames"],
    );

    // Unsupported/invalid values fail closed through the fallback too, at
    // whichever scope currently wins.
    for value in ["copy", "sideways"] {
        fixture.success(&repo, &["config", "diff.renames", value]);
        let out = fixture.run(&repo, &["status"]);
        assert!(
            !out.status.success(),
            "diff.renames={value} must fail closed through the fallback"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("diff.renames"),
            "the failure names the fallback key, not the primary one"
        );
    }
}

/// `copy`/`copies` values fail closed (R0 has no copy detection) and invalid
/// booleans fail closed too — never a silent downgrade.
#[test]
fn status_renames_copy_and_invalid_fail_closed() {
    let fixture = Fixture::new();
    let repo = staged_inexact_rename_repo(&fixture, "renames-copy");

    fixture.success(&repo, &["config", "status.renames", "copy"]);
    let out = fixture.run(&repo, &["status"]);
    assert!(!out.status.success(), "copy must fail closed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("copy detection is not supported"),
        "copy failure is actionable: {stderr}"
    );

    fixture.success(&repo, &["config", "status.renames", "sideways"]);
    let out = fixture.run(&repo, &["status"]);
    assert!(!out.status.success(), "invalid boolean must fail closed");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("status.renames"),
        "failure names the key"
    );
}

/// `status.renameUntracked` and `core.quotePath` are documented as strict
/// local → global → system cascades, but the wave-0 regressions only reach
/// global and local (they have no isolated system store). This pins the
/// SYSTEM scope for both: a system-only value takes effect, a nearer scope
/// overrides it, and an invalid value at the winning scope fails closed
/// before any output — an empty stdout, not a partial status.
#[test]
fn rename_untracked_and_quote_path_honor_the_system_scope() {
    let fixture = Fixture::new();
    let repo = fixture.path("system-scope-keys");
    fixture.init_repo(&repo);
    let body = "system scope content\nsecond line\n";
    fixture.commit_file(&repo, "a.txt", body, "base");
    fs::rename(repo.join("a.txt"), repo.join("moved.txt")).expect("worktree move");

    let porcelain = |fixture: &Fixture| -> String {
        String::from_utf8_lossy(&fixture.success(&repo, &["status", "--porcelain"]).stdout)
            .into_owned()
    };

    // Default (unset everywhere): a tracked→untracked move stays D + ??.
    assert!(
        porcelain(&fixture).lines().any(|l| l == "?? moved.txt"),
        "the extension is off by default"
    );

    // System scope alone turns it on.
    fixture.success(
        Path::new("/"),
        &["config", "--system", "status.renameUntracked", "true"],
    );
    assert!(
        porcelain(&fixture)
            .lines()
            .any(|l| l == " R a.txt -> moved.txt"),
        "a system-scope value takes effect"
    );

    // A nearer scope overrides it.
    fixture.success(&repo, &["config", "status.renameUntracked", "false"]);
    assert!(
        porcelain(&fixture).lines().any(|l| l == "?? moved.txt"),
        "local false beats system true"
    );
    fixture.success(&repo, &["config", "--unset", "status.renameUntracked"]);

    // Invalid at the winning (system) scope fails closed with NO output.
    fixture.success(
        Path::new("/"),
        &["config", "--system", "status.renameUntracked", "sideways"],
    );
    let out = fixture.run(&repo, &["status", "--porcelain"]);
    assert!(
        !out.status.success(),
        "an invalid system value fails closed"
    );
    assert!(
        out.stdout.is_empty(),
        "fail-closed means no partial status was printed: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    fixture.success(
        Path::new("/"),
        &["config", "--system", "--unset", "status.renameUntracked"],
    );

    // Same three checks for core.quotePath, whose effect is visible in the
    // escaping of a non-ASCII path.
    fs::write(repo.join("née.txt"), "accented\n").expect("write non-ascii path");
    fixture.success(
        Path::new("/"),
        &["config", "--system", "core.quotePath", "false"],
    );
    assert!(
        porcelain(&fixture).contains("née.txt"),
        "system-scope quotePath=false leaves non-ASCII unescaped"
    );
    fixture.success(&repo, &["config", "core.quotePath", "true"]);
    assert!(
        porcelain(&fixture).contains("\\303"),
        "local true beats system false and octal-escapes the name"
    );
    fixture.success(&repo, &["config", "--unset", "core.quotePath"]);

    fixture.success(
        Path::new("/"),
        &["config", "--system", "core.quotePath", "sideways"],
    );
    let out = fixture.run(&repo, &["status", "--porcelain"]);
    assert!(
        !out.status.success(),
        "an invalid system core.quotePath fails closed"
    );
    assert!(
        out.stdout.is_empty(),
        "fail-closed means no partial status was printed: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Precedence applies to the VALUE, not to validation. A CLI rename flag
/// beats a *valid* config value, but it does not suppress the fail-closed
/// parse of an unsupported/invalid one — same as Git, which parses the
/// config before applying the command line. Documented in
/// `docs/commands/status.md`; this pins it so the two cannot drift.
#[test]
fn status_renames_cli_flag_does_not_mask_an_invalid_config() {
    let fixture = Fixture::new();
    let repo = staged_inexact_rename_repo(&fixture, "renames-cli-vs-invalid");

    // Valid value + CLI flag: the flag wins, both directions.
    fixture.success(&repo, &["config", "status.renames", "false"]);
    assert!(
        String::from_utf8_lossy(&fixture.success(&repo, &["status", "--renames"]).stdout)
            .contains("renamed:"),
        "--renames overrides a valid status.renames=false"
    );
    fixture.success(&repo, &["config", "status.renames", "true"]);
    assert!(
        !String::from_utf8_lossy(&fixture.success(&repo, &["status", "--no-renames"]).stdout)
            .contains("renamed:"),
        "--no-renames overrides a valid status.renames=true"
    );

    // Unsupported/invalid values still fail closed under every rename
    // spelling — the flag never makes a broken config invisible.
    for value in ["copy", "sideways"] {
        fixture.success(&repo, &["config", "status.renames", value]);
        for flag in ["--no-renames", "--renames", "--find-renames=80"] {
            let out = fixture.run(&repo, &["status", flag]);
            assert!(
                !out.status.success(),
                "status.renames={value} with {flag} must still fail closed"
            );
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains("status.renames"),
                "status.renames={value} with {flag} must name the key: {stderr}"
            );
        }
    }

    // The same holds for the diff.renames fallback.
    fixture.success(&repo, &["config", "--unset", "status.renames"]);
    fixture.success(&repo, &["config", "diff.renames", "copy"]);
    let out = fixture.run(&repo, &["status", "--no-renames"]);
    assert!(
        !out.status.success(),
        "an invalid diff.renames fallback fails closed under --no-renames too"
    );
}

/// `status.renameLimit` inherits `diff.renameLimit` across scopes and rejects
/// invalid values before any output.
#[test]
fn status_rename_limit_cascade_and_invalid_fail_closed() {
    let fixture = Fixture::new();
    let repo = staged_inexact_rename_repo(&fixture, "rename-limit");

    // A GLOBAL diff.renameLimit is inherited when the status key is unset.
    // The staged new side holds TWO candidates (.libraignore + the renamed
    // file), so limit=1 gates the exhaustive stage with the warning…
    fixture.success(
        Path::new("/"),
        &["config", "--global", "diff.renameLimit", "1"],
    );
    let out = fixture.success(&repo, &["status"]);
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("renamed:"),
        "inherited limit=1 gates the 2-candidate exhaustive stage"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("renameLimit"),
        "the degradation warning names the limit"
    );

    // …while a local status.renameLimit=2 wins over the fallback and lets
    // the pair through again.
    fixture.success(&repo, &["config", "status.renameLimit", "2"]);
    assert!(
        status_reports_rename(&fixture, &repo),
        "status.renameLimit beats the diff fallback"
    );

    // …and an invalid status.renameLimit still fails closed first.
    fixture.success(&repo, &["config", "status.renameLimit", "many"]);
    let out = fixture.run(&repo, &["status"]);
    assert!(
        !out.status.success(),
        "invalid renameLimit must fail closed"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("status.renameLimit"),
        "failure names the key"
    );
}
