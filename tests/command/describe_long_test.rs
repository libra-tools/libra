use super::*;

#[test]
fn test_describe_long_for_exact_tag_forces_zero_distance_suffix() {
    let repo = create_committed_repo_via_cli();

    let tag_output = run_libra_command(&["tag", "-m", "Release v1.0", "v1.0"], repo.path());
    assert_cli_success(&tag_output, "failed to create tag for describe --long");

    let output = run_libra_command(&["describe", "--long", "--json"], repo.path());
    assert_cli_success(&output, "describe --long --json should succeed");

    let json = parse_json_stdout(&output);
    let result = json["data"]["result"]
        .as_str()
        .expect("result should be a string");
    let abbreviated_commit = json["data"]["abbreviated_commit"]
        .as_str()
        .expect("--long exact match should include an abbreviated commit");

    assert!(
        result.starts_with("v1.0-0-g"),
        "--long exact match should force Git's tag-0-gHASH form, got {result}"
    );
    assert_eq!(result, format!("v1.0-0-g{abbreviated_commit}"));
    assert_eq!(abbreviated_commit.len(), 7);
    assert_eq!(json["data"]["distance"], 0);
    assert_eq!(json["data"]["exact_match"], true);
    assert_eq!(json["data"]["long_format"], true);
}

#[test]
fn test_describe_long_for_reachable_tag_keeps_long_result() {
    let repo = create_committed_repo_via_cli();

    let tag_output = run_libra_command(&["tag", "-m", "Release v1.0", "v1.0"], repo.path());
    assert_cli_success(&tag_output, "failed to create tag for describe --long");
    std::fs::write(repo.path().join("tracked.txt"), "tracked\nnext\n")
        .expect("failed to update tracked file");
    let add_output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&add_output, "failed to stage updated tracked file");
    let commit_output = run_libra_command(&["commit", "-m", "next", "--no-verify"], repo.path());
    assert_cli_success(&commit_output, "failed to create second commit");

    let output = run_libra_command(&["describe", "--long", "--json"], repo.path());
    assert_cli_success(&output, "describe --long --json should succeed");

    let json = parse_json_stdout(&output);
    let result = json["data"]["result"]
        .as_str()
        .expect("result should be a string");

    assert!(
        result.starts_with("v1.0-1-g"),
        "--long reachable tag should keep tag-N-gHASH form, got {result}"
    );
    assert_eq!(json["data"]["distance"], 1);
    assert_eq!(json["data"]["exact_match"], false);
    assert_eq!(json["data"]["long_format"], true);
}

#[test]
fn test_describe_long_rejects_abbrev_zero_like_git() {
    let repo = create_committed_repo_via_cli();

    let tag_output = run_libra_command(&["tag", "-m", "Release v1.0", "v1.0"], repo.path());
    assert_cli_success(&tag_output, "failed to create tag for describe --long");

    let output = run_libra_command(&["describe", "--long", "--abbrev=0"], repo.path());
    let (human, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        human.contains("--long") && human.contains("--abbrev=0"),
        "invalid argument error should name both conflicting options: {human}"
    );
}

/// DESC-04 AC1–AC5: `--no-long` and last-one-wins with `--long`.
#[test]
fn test_describe_no_long_last_wins() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["tag", "-m", "v1.0", "v1.0"], p), "tag");
    let head = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    let ab7 = &head[..7];

    let out = run_libra_command(&["describe", "--no-long"], p);
    assert_cli_success(&out, "AC1");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "v1.0", "AC1");

    let out = run_libra_command(&["describe", "--long", "--no-long"], p);
    assert_cli_success(&out, "AC2");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "v1.0", "AC2");

    let out = run_libra_command(&["describe", "--long", "--no-long", "--json"], p);
    assert_cli_success(&out, "AC3");
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["long_format"], false, "AC3");

    let out = run_libra_command(&["describe", "--no-long", "--long"], p);
    assert_cli_success(&out, "AC4");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("v1.0-0-g{ab7}"),
        "AC4"
    );

    let out = run_libra_command(&["describe", "--long", "--no-long", "--abbrev=0"], p);
    assert_cli_success(&out, "AC5");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "v1.0", "AC5");
}
