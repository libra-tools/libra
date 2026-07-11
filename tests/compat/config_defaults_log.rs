use super::*;

fn log_repo(fixture: &Fixture, name: &str) -> PathBuf {
    let repo = fixture.path(name);
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "tracked.txt", "one\n", "first subject");
    fixture.commit_file(&repo, "tracked.txt", "two\n", "second subject");
    repo
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn subject_lines(output: &Output) -> Vec<String> {
    stdout_text(output)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[test]
fn format_pretty_config_is_the_default_and_cli_formats_win() {
    let fixture = Fixture::new();
    let repo = log_repo(&fixture, "log-format-pretty");
    fixture.success(&repo, &["config", "--system", "format.pretty", "format:%s"]);

    let configured = fixture.success(&repo, &["log", "-1"]);
    assert_eq!(subject_lines(&configured), ["second subject"]);

    let pretty = stdout_text(&fixture.success(&repo, &["log", "-1", "--pretty=full"]));
    assert!(pretty.contains("Author: Config Test"), "{pretty}");
    assert!(pretty.contains("Commit: Config Test"), "{pretty}");

    let format = subject_lines(&fixture.success(&repo, &["log", "-1", "--format=%H"]));
    assert_eq!(format.len(), 1, "{format:?}");
    assert_eq!(format[0].len(), 40, "full object id: {format:?}");

    let oneline = stdout_text(&fixture.success(&repo, &["log", "-1", "--oneline"]));
    assert!(oneline.contains("second subject"), "{oneline}");
    assert_ne!(oneline.trim(), "second subject", "{oneline}");

    fixture.success(&repo, &["config", "format.pretty", " oneline "]);
    let padded_preset = stdout_text(&fixture.success(&repo, &["log", "-1"]));
    assert!(padded_preset.contains("second subject"), "{padded_preset}");
    assert_ne!(padded_preset.trim(), "oneline", "{padded_preset}");

    fixture.success(&repo, &["config", "format.pretty", " format:%s "]);
    let padded_template = subject_lines(&fixture.success(&repo, &["log", "-1"]));
    assert_eq!(padded_template, ["second subject"]);
}

#[test]
fn log_date_config_matches_date_option_and_cli_wins() {
    let fixture = Fixture::new();
    let repo = log_repo(&fixture, "log-date-default");
    fixture.success(&repo, &["config", "format.pretty", "format:%ad"]);
    fixture.success(&repo, &["config", "--global", "log.date", "unix"]);

    let configured = subject_lines(&fixture.success(&repo, &["log", "-1"]));
    let explicit_unix = subject_lines(
        &fixture.success(&repo, &["log", "-1", "--pretty=format:%ad", "--date=unix"]),
    );
    assert_eq!(configured, explicit_unix);
    assert!(
        configured[0].bytes().all(|byte| byte.is_ascii_digit()),
        "unix date: {configured:?}"
    );

    let explicit_short = subject_lines(
        &fixture.success(&repo, &["log", "-1", "--pretty=format:%ad", "--date=short"]),
    );
    assert_ne!(explicit_short, configured);
    assert_eq!(
        explicit_short[0].len(),
        10,
        "short date: {explicit_short:?}"
    );
}

#[test]
fn format_pretty_and_log_date_defaults_apply_to_show_and_cli_wins() {
    let fixture = Fixture::new();
    let repo = log_repo(&fixture, "show-format-defaults");
    fixture.success(&repo, &["config", "format.pretty", "format:%ad|%s"]);
    fixture.success(&repo, &["config", "log.date", "unix"]);

    let configured = subject_lines(&fixture.success(&repo, &["show", "--no-patch", "HEAD"]));
    assert_eq!(configured.len(), 1, "{configured:?}");
    let (date, subject) = configured[0]
        .split_once('|')
        .expect("configured show renders date and subject");
    assert!(date.bytes().all(|byte| byte.is_ascii_digit()), "{date}");
    assert_eq!(subject, "second subject");

    fixture.success(&repo, &["config", "format.pretty", "not-a-pretty-format"]);
    fixture.success(&repo, &["config", "log.date", "not-a-date-mode"]);
    let explicit = subject_lines(&fixture.success(
        &repo,
        &[
            "show",
            "--no-patch",
            "--pretty=format:%ad|%s",
            "--date=short",
            "HEAD",
        ],
    ));
    assert_eq!(explicit.len(), 1, "{explicit:?}");
    let (date, _) = explicit[0]
        .split_once('|')
        .expect("explicit show renders date and subject");
    assert_eq!(date.len(), 10, "{explicit:?}");
    assert!(explicit[0].ends_with("|second subject"), "{explicit:?}");
}

#[test]
fn explicit_log_options_bypass_invalid_matching_defaults() {
    let fixture = Fixture::new();
    let repo = log_repo(&fixture, "log-config-cli-precedence");
    fixture.success(&repo, &["config", "format.pretty", ""]);
    fixture.success(&repo, &["config", "log.date", "not-a-date-mode"]);
    fixture.success(&repo, &["config", "log.follow", "sometimes"]);

    let output = fixture.success(
        &repo,
        &[
            "log",
            "-1",
            "--pretty=format:%ad %s",
            "--date=short",
            "--no-follow",
        ],
    );
    let text = stdout_text(&output);
    assert!(text.contains("second subject"), "{text}");
}

#[test]
fn format_pretty_medium_preserves_default_full_hash_for_log_and_show() {
    let fixture = Fixture::new();
    let repo = log_repo(&fixture, "pretty-medium-default");
    let head = stdout_trim(&fixture.success(&repo, &["rev-parse", "HEAD"]));
    fixture.success(&repo, &["config", "format.pretty", "medium"]);

    for args in [&["log", "-1"][..], &["show", "--no-patch", "HEAD"][..]] {
        let output = stdout_text(&fixture.success(&repo, args));
        assert!(
            output.contains(&format!("commit {head}")),
            "{args:?}: {output}"
        );
    }
}

#[test]
fn show_non_commit_targets_ignore_invalid_commit_display_defaults() {
    let fixture = Fixture::new();
    let repo = log_repo(&fixture, "show-non-commit-config-immunity");
    let commit = stdout_text(&fixture.success(&repo, &["cat-file", "-p", "HEAD"]));
    let tree = commit
        .lines()
        .find_map(|line| line.strip_prefix("tree "))
        .expect("HEAD tree id");
    let listing = stdout_text(&fixture.success(&repo, &["ls-tree", "HEAD", "tracked.txt"]));
    let blob = listing.split_whitespace().nth(2).expect("tracked blob id");

    fixture.success(&repo, &["config", "format.pretty", "not-a-pretty-format"]);
    fixture.success(&repo, &["config", "log.date", "not-a-date-mode"]);

    let tree_output = stdout_text(&fixture.success(&repo, &["show", tree]));
    assert!(tree_output.contains("tracked.txt"), "{tree_output}");
    let blob_output = stdout_text(&fixture.success(&repo, &["show", blob]));
    assert_eq!(blob_output, "two\n");
    let path_output = stdout_text(&fixture.success(&repo, &["show", "HEAD:tracked.txt"]));
    assert_eq!(path_output, "two\n");
    let quiet = fixture.success(&repo, &["--quiet", "show", "HEAD"]);
    assert!(quiet.stdout.is_empty(), "{quiet:?}");
    let json = fixture.success(&repo, &["--json", "show", "HEAD"]);
    let payload: Value = serde_json::from_slice(&json.stdout).expect("parse show JSON");
    assert_eq!(payload["data"]["type"], "commit", "{payload}");
}

#[test]
fn only_trailers_overrides_format_pretty_config() {
    let fixture = Fixture::new();
    let repo = fixture.path("pretty-only-trailers");
    fixture.init_repo(&repo);
    fixture.commit_file(
        &repo,
        "reviewed.txt",
        "reviewed\n",
        "reviewed subject\n\nReviewed-by: Alice <alice@example.com>",
    );
    fixture.success(&repo, &["config", "format.pretty", "oneline"]);
    fixture.success(&repo, &["config", "log.date", "not-a-date-mode"]);

    let output = stdout_text(&fixture.success(&repo, &["log", "-1", "--only-trailers"]));
    assert!(
        output.contains("Reviewed-by: Alice <alice@example.com>"),
        "{output}"
    );
    assert!(!output.contains("reviewed subject"), "{output}");
}
