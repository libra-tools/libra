//! `review --fix` executes only through an active, authorized Code runtime; its
//! no-runtime paths stay fail-closed with stable `LBR-AGENT-010`. This
//! command-layer guard pins their human and structured-JSON error surfaces.

use libra::internal::ai::investigate::InvestigateRunStore;

use super::{
    create_committed_repo_via_cli, parse_cli_error_stderr, run_libra_command,
    run_libra_command_with_stdin_and_env,
};

#[test]
fn review_investigate_fix_json_errors() {
    let repo = create_committed_repo_via_cli();
    InvestigateRunStore::new(repo.path().join(".libra").join("sessions"))
        .create_run(
            "some-run-id",
            "untrusted seed",
            &["codex".to_string()],
            4,
            1,
            "sha-seed",
        )
        .expect("seed investigate run");

    // review --fix — human surface: fatal (128) + LBR-AGENT-010.
    let out = run_libra_command(&["review", "--fix"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(128),
        "review --fix must be a fatal refusal: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-AGENT-010"),
        "review --fix human surface must carry LBR-AGENT-010: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // review --fix — structured JSON error surface.
    let out = run_libra_command_with_stdin_and_env(
        &["review", "--fix"],
        repo.path(),
        "",
        &[("LIBRA_ERROR_JSON", "1")],
    );
    assert_eq!(
        out.status.code(),
        Some(128),
        "review --fix JSON surface must exit 128"
    );
    let (_human, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-AGENT-010");
    assert_eq!(report.exit_code, 128);

    // investigate fix <run_id> — human no-runtime surface.
    let out = run_libra_command(&["investigate", "fix", "some-run-id"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(128),
        "investigate fix must be a fatal refusal: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-AGENT-010"),
        "investigate fix human surface must carry LBR-AGENT-010: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // investigate fix — structured JSON error surface.
    let out = run_libra_command_with_stdin_and_env(
        &["investigate", "fix", "some-run-id"],
        repo.path(),
        "",
        &[("LIBRA_ERROR_JSON", "1")],
    );
    assert_eq!(
        out.status.code(),
        Some(128),
        "investigate fix JSON surface must exit 128"
    );
    let (_human, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-AGENT-010");
    assert_eq!(report.exit_code, 128);
}
